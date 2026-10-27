#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Apoio aos testes de integração: inicia e encerra o `gt-testtarget`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};

pub mod module;
pub mod rig;
pub mod ue;

pub use ue::{UeAddrs, synth_signatures};

/// Caminho de um binário deste crate (ex. `gt-testtarget`) em `target/<perfil>/`.
///
/// Sobe a partir do executável de teste até o diretório que contém `deps`. Entra em
/// pânico se o binário não existir.
pub fn bin_path(name: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let found = exe
        .ancestors()
        .skip(1)
        .find(|dir| dir.join("deps").is_dir())
        .map(|dir| dir.join(name))
        .filter(|path| path.is_file());
    match found {
        Some(path) => path,
        None => panic!("binário {name} não encontrado; rode `cargo build --workspace` antes"),
    }
}

/// Processo-alvo de teste em execução.
pub struct TestTarget {
    pub child: Child,
    pub pid: i32,
    pub marker: u64,
    pub exec_page: u64,
    /// Início de 2 páginas `r-x` preenchidas com `0x90`, sem thread.
    pub exec2: u64,
    /// Página com `48 8B F9 EB FB` (laço de 5 bytes); só há thread nela com `spin`.
    pub spin_page: u64,
    /// Página de uma thread bloqueada num `read`; o `pc` parado é `syscall_page + 22`.
    pub syscall_page: u64,
    /// Página com o laço de valor (`gt_core::mem::test_support::value_code`): `B8 11 11 11 11`
    /// no início (imediato no deslocamento 1); só há thread nela com `spin_value`.
    pub value_page: u64,
    /// Palavra de 32 bits em página `rw` onde o laço de valor escreve o imediato.
    pub value_word: u64,
    /// Início de 3 páginas; a do meio (`region_start + page`) é ilegível.
    pub region_start: u64,
    /// Início de uma região `rw` de `big_len` bytes (2 MiB + 1 página).
    pub big_region: u64,
    pub big_len: usize,
    /// Página `MAP_SHARED` somente leitura (de arquivo) preenchida com `0x90`: legível,
    /// mas a escrita por `/proc/<pid>/mem` é recusada.
    pub ro_shared: u64,
    /// Tamanho da página do sistema, informado pelo alvo.
    pub page: u64,
    /// Reflexão sintética (só com `ue`).
    pub ue: Option<UeAddrs>,
    /// Módulo sintético (só com `module`).
    pub module: Option<ModuleAddrs>,
    /// Diretório do arquivo do módulo sintético; apagado junto com o alvo.
    module_dir: Option<tempfile::TempDir>,
    /// Saída do alvo, para as respostas dos comandos.
    reader: BufReader<ChildStdout>,
}

/// O módulo sintético (`gt-synthetic.exe`) do alvo.
#[derive(Debug, Clone)]
pub struct ModuleAddrs {
    /// Onde a imagem foi mapeada.
    pub base: u64,
    /// Arquivo da imagem (o "disco" do `gt diff`).
    pub path: PathBuf,
    /// Endereço absoluto de cada símbolo ([`module::build`] lista os nomes).
    pub symbols: BTreeMap<String, u64>,
}

/// O que as threads de `loop` fizeram até o `stop`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopStats {
    /// Chamadas feitas, somadas de todas as threads.
    pub calls: u64,
    /// Resultados distintos (`rax`), em ordem crescente; no máximo 64.
    pub results: Vec<u64>,
}

/// Opções de [`TestTarget::spawn_with`].
#[derive(Debug, Clone, Copy, Default)]
pub struct TargetOpts {
    /// Cria uma thread que gira em `spin_page` (consome uma CPU).
    pub spin: bool,
    /// Cria uma thread no laço de valor em `value_page` (consome uma CPU).
    pub spin_value: bool,
    /// Monta a reflexão sintética de [`ue::build`] numa arena da própria memória.
    pub ue: bool,
    /// Grava e mapeia o módulo sintético `gt-synthetic.exe` ([`module`]) e aceita os comandos
    /// `call`, `loop`, `stop` e `park`.
    pub module: bool,
}

fn parse_hex(v: &serde_json::Value) -> u64 {
    let s = v.as_str().unwrap();
    u64::from_str_radix(s.trim_start_matches("0x"), 16).unwrap()
}

impl TestTarget {
    /// Inicia o alvo sem a thread que gira.
    pub fn spawn() -> TestTarget {
        TestTarget::spawn_with(TargetOpts::default())
    }

    /// Inicia o alvo com as opções dadas e espera ele ficar pronto.
    pub fn spawn_with(opts: TargetOpts) -> TestTarget {
        let mut cmd = Command::new(bin_path("gt-testtarget"));
        if opts.spin {
            cmd.arg("--spin");
        }
        if opts.spin_value {
            cmd.arg("--spin-value");
        }
        if opts.ue {
            cmd.arg("--ue");
        }
        let module_dir = opts.module.then(|| tempfile::tempdir().unwrap());
        if let Some(dir) = &module_dir {
            cmd.arg("--module").arg(dir.path());
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        reader.read_line(&mut line).unwrap();
        let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        // O `stdin` do filho continua aberto dentro de `child`: ele só termina
        // quando o `Drop` mata o processo.
        let module = v.get("module").map(|m| ModuleAddrs {
            base: parse_hex(&m["base"]),
            path: PathBuf::from(m["path"].as_str().unwrap()),
            symbols: m["symbols"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(name, addr)| (name.clone(), parse_hex(addr)))
                .collect(),
        });
        TestTarget {
            pid: v["pid"].as_i64().unwrap() as i32,
            marker: parse_hex(&v["marker"]),
            exec_page: parse_hex(&v["exec_page"]),
            exec2: parse_hex(&v["exec2"]),
            spin_page: parse_hex(&v["spin_page"]),
            syscall_page: parse_hex(&v["syscall_page"]),
            value_page: parse_hex(&v["value_page"]),
            value_word: parse_hex(&v["value_word"]),
            region_start: parse_hex(&v["region_start"]),
            big_region: parse_hex(&v["big_region"]),
            big_len: v["big_len"].as_u64().unwrap() as usize,
            ro_shared: parse_hex(&v["ro_shared"]),
            page: v["page"].as_u64().unwrap(),
            ue: v.get("ue").map(|u| UeAddrs {
                gobjects: parse_hex(&u["gobjects"]),
                names: parse_hex(&u["names"]),
                controller: parse_hex(&u["controller"]),
                pawn: parse_hex(&u["pawn"]),
                creature: parse_hex(&u["creature"]),
                pawn_field_addr: parse_hex(&u["pawn_field_addr"]),
                health: parse_hex(&u["health"]),
                alt_pawn: parse_hex(&u["alt_pawn"]),
                hauling: parse_hex(&u["hauling"]),
                movement: parse_hex(&u["movement"]),
                creature_hauling: parse_hex(&u["creature_hauling"]),
                creature_movement: parse_hex(&u["creature_movement"]),
                level: parse_hex(&u["level"]),
                world: parse_hex(&u["world"]),
                game_state: parse_hex(&u["game_state"]),
                party: parse_hex(&u["party"]),
                mode_manager: parse_hex(&u["mode_manager"]),
                settings: parse_hex(&u["settings"]),
                cdo_settings: parse_hex(&u["cdo_settings"]),
            }),
            module,
            module_dir,
            reader,
            child,
        }
    }

    /// Base do módulo sintético. Entra em pânico sem `module`.
    pub fn module_base(&self) -> u64 {
        self.module.as_ref().unwrap().base
    }

    /// Diretório onde está o arquivo do módulo sintético (o que a política de confiança lista).
    pub fn module_dir(&self) -> &Path {
        self.module_dir.as_ref().unwrap().path()
    }

    /// Arquivo do módulo sintético. Entra em pânico sem `module`.
    pub fn module_path(&self) -> &Path {
        &self.module.as_ref().unwrap().path
    }

    /// Endereço absoluto de um símbolo do módulo sintético (`sum`, `level_gain`, `cc300`, ...).
    /// Entra em pânico se o nome não existir.
    pub fn symbol(&self, name: &str) -> u64 {
        match self.module.as_ref().unwrap().symbols.get(name) {
            Some(addr) => *addr,
            None => panic!("símbolo {name} não existe no módulo sintético"),
        }
    }

    /// O alvo como o `Target::open` o montaria para o jogo, mas com o diretório do módulo sintético
    /// confiável: `maps`, `modules` e a identidade lidos agora. Entra em pânico sem `module`.
    pub fn module_target(&self) -> gt_grounded2::resolve::Target {
        let maps = gt_core::maps::read_maps(self.pid).unwrap();
        let reader = gt_core::mem::MemReader::new(self.pid);
        let policy = gt_core::modules::TrustPolicy::new(vec![self.module_dir().to_path_buf()]);
        let root = PathBuf::from(format!("/proc/{}/root", self.pid));
        let modules = gt_core::modules::discover(&maps, &reader, &policy, &root).unwrap();
        gt_grounded2::resolve::Target {
            pid: self.pid,
            identity: gt_core::procinfo::identity(self.pid).unwrap(),
            discarded: Vec::new(),
            maps,
            reader,
            modules,
            non_pe: std::collections::HashSet::new(),
        }
    }

    /// Envia uma linha de comando ao alvo e devolve a linha de resposta, sem o fim de linha.
    pub fn command(&mut self, line: &str) -> String {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
        let mut reply = String::new();
        let n = self.reader.read_line(&mut reply).unwrap();
        assert!(n > 0, "o alvo fechou a saída ao responder `{line}`");
        reply.trim_end().to_string()
    }

    /// Resposta `ok ...` de um comando; pânico se o alvo respondeu erro.
    fn command_ok(&mut self, line: &str) -> Vec<String> {
        let reply = self.command(line);
        let mut parts = reply.split_whitespace().map(str::to_string);
        assert_eq!(parts.next().as_deref(), Some("ok"), "`{line}`: {reply}");
        parts.collect()
    }

    /// Chama uma função do módulo (símbolo ou endereço) com até 4 argumentos inteiros e devolve
    /// `rax`.
    pub fn call(&mut self, function: &str, args: &[u64]) -> u64 {
        let line = format!("call {function}{}", fmt_args(args));
        let reply = self.command_ok(&line);
        parse_u64(&reply[0])
    }

    /// Como [`TestTarget::call`], mas com o símbolo ou endereço já formatado como número.
    pub fn call_addr(&mut self, addr: u64, args: &[u64]) -> u64 {
        self.call(&format!("{addr:#x}"), args)
    }

    /// Inicia uma thread que chama a função em laço até [`TestTarget::stop_loops`].
    pub fn start_loop(&mut self, function: &str, args: &[u64]) {
        let line = format!("loop {function}{}", fmt_args(args));
        self.command_ok(&line);
    }

    /// Para todas as threads de [`TestTarget::start_loop`] e devolve o que elas fizeram.
    pub fn stop_loops(&mut self) -> LoopStats {
        let reply = self.command_ok("stop");
        let results = reply
            .get(1)
            .map(|csv| {
                csv.split(',')
                    .filter(|s| !s.is_empty())
                    .map(parse_u64)
                    .collect()
            })
            .unwrap_or_default();
        LoopStats {
            calls: parse_u64(&reply[0]),
            results,
        }
    }

    /// Cria uma thread que salta para `addr`; o código ali precisa ser um laço (ex.: o símbolo
    /// `spin`, ou `EB FE` escrito pelo teste). Devolve o TID da thread.
    pub fn park(&mut self, addr: u64) -> i32 {
        let reply = self.command_ok(&format!("park {addr:#x}"));
        reply[0].parse().unwrap()
    }
}

fn fmt_args(args: &[u64]) -> String {
    args.iter().map(|a| format!(" {a:#x}")).collect()
}

fn parse_u64(s: &str) -> u64 {
    match s.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).unwrap(),
        None => s.parse().unwrap(),
    }
}

impl Drop for TestTarget {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Os 32 bytes do marcador: `0xA5 ^ i`.
pub fn marker_bytes() -> [u8; 32] {
    std::array::from_fn(|i| 0xA5 ^ i as u8)
}

/// Bytes em hexadecimal maiúsculo separados por espaço (ex. `"A5 A4"`).
pub fn hex_with_spaces(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}
