#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Apoio aos testes de integração: inicia e encerra o `gt-testtarget`.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

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
}

/// Opções de [`TestTarget::spawn_with`].
#[derive(Debug, Clone, Copy, Default)]
pub struct TargetOpts {
    /// Cria uma thread que gira em `spin_page` (consome uma CPU).
    pub spin: bool,
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
        TestTarget {
            pid: v["pid"].as_i64().unwrap() as i32,
            marker: parse_hex(&v["marker"]),
            exec_page: parse_hex(&v["exec_page"]),
            exec2: parse_hex(&v["exec2"]),
            spin_page: parse_hex(&v["spin_page"]),
            syscall_page: parse_hex(&v["syscall_page"]),
            region_start: parse_hex(&v["region_start"]),
            big_region: parse_hex(&v["big_region"]),
            big_len: v["big_len"].as_u64().unwrap() as usize,
            ro_shared: parse_hex(&v["ro_shared"]),
            page: v["page"].as_u64().unwrap(),
            child,
        }
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
