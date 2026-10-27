//! Processo-alvo de teste: expõe um marcador, páginas executáveis, uma região
//! com página ilegível, uma região `rw` grande e uma página compartilhada somente
//! leitura, e espera o `stdin` fechar. Nunca é o jogo.
//!
//! Argumento opcional `--spin`: cria uma thread que gira para sempre em `spin_page`.
//! Argumento opcional `--spin-value`: cria uma thread no laço de valor em `value_page`,
//! que escreve o imediato do `mov eax, imm32` em `value_word`.
//! Argumento opcional `--ue`: monta a reflexão sintética (`gt_testkit::ue`) numa arena `rw` e
//! informa os endereços na chave `ue`.
//! Argumento opcional `--module <dir>`: grava `gt-synthetic.exe` em `<dir>` e o mapeia como uma
//! imagem do jogo (`gt_testkit::module`); a base, o arquivo e os símbolos saem na chave `module`.
//!
//! Depois da linha de partida, lê comandos de uma linha no `stdin` e responde uma linha no
//! `stdout` (`ok ...` ou `err ...`), até o `stdin` fechar:
//! - `call <função> [args...]`: chama a função (símbolo do módulo ou endereço) no formato x64 do
//!   Windows com até 4 argumentos inteiros e responde `ok <rax em hex>`;
//! - `loop <função> [args...]`: thread que chama a função em laço até `stop`; responde `ok`;
//! - `stop`: para todos os laços e responde `ok <chamadas> <resultados distintos em hex, vírgula>`;
//! - `park <endereço>`: thread que salta para o endereço (o código ali precisa ser um laço) e
//!   responde `ok <tid>`.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gt_core::mem::test_support::{
    allow_any_tracer, anon_rw, call_win64, exec_code, exec_page, guarded_pages, page_size,
    pipe_never_written, readonly_shared_page, spawn_thread_at, value_code, write_local,
};
use gt_core::procinfo::{ThreadPos, list_tids, read_task_stat, read_thread_pos};

/// Prazo para as threads auxiliares chegarem ao estado esperado.
const READY_TIMEOUT: Duration = Duration::from_secs(2);

/// `mov rdi, rcx; jmp -5`: laço de 5 bytes que não toca a pilha.
const SPIN_CODE: [u8; 5] = [0x48, 0x8B, 0xF9, 0xEB, 0xFB];

/// Deslocamento do `pc` de uma thread parada no `syscall` do `syscall_page`.
const SYSCALL_PC: u64 = 22;

/// Código que lê 1 byte de `fd` para sempre (a `syscall` fica no deslocamento 20).
fn syscall_code(fd: i32) -> Vec<u8> {
    let mut code = vec![0xB8, 0, 0, 0, 0, 0xBF];
    code.extend_from_slice(&(fd as u32).to_le_bytes());
    code.extend_from_slice(&[0x48, 0x8D, 0x74, 0x24, 0xF0, 0xBA, 1, 0, 0, 0, 0x0F, 0x05]);
    code.extend_from_slice(&[0x90, 0x90, 0x90, 0x90, 0xEB, 0xE4]);
    code
}

/// Cria uma thread em `addr`, espera `ready` aceitar o TID novo (poll a cada 1 ms) e o devolve.
fn spawn_and_wait(
    addr: u64,
    ready: impl Fn(i32) -> Result<bool, Box<dyn std::error::Error>>,
) -> Result<i32, Box<dyn std::error::Error>> {
    let pid = std::process::id() as i32;
    let before = list_tids(pid)?;
    spawn_thread_at(addr)?;
    let start = Instant::now();
    loop {
        for tid in list_tids(pid)? {
            if !before.contains(&tid) && ready(tid)? {
                return Ok(tid);
            }
        }
        if start.elapsed() > READY_TIMEOUT {
            return Err("thread auxiliar não chegou ao estado esperado".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Resultados distintos que cada laço guarda no máximo.
const MAX_DISTINCT: usize = 64;

/// Thread de `loop`: chamadas feitas e resultados distintos, devolvidos pelo `join`.
struct LoopThread {
    handle: JoinHandle<(u64, Vec<u64>)>,
}

/// Estado dos comandos do `stdin`.
struct Commands {
    symbols: HashMap<String, u64>,
    loops: Vec<LoopThread>,
    /// Pede às threads de `loop` que terminem.
    stop: Arc<AtomicBool>,
}

/// Número decimal, hexadecimal (`0x`) ou negativo (complemento de 2).
fn parse_num(text: &str) -> Result<u64, String> {
    let parsed = match text.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => text
            .parse::<u64>()
            .ok()
            .or_else(|| text.parse::<i64>().ok().map(|v| v as u64)),
    };
    parsed.ok_or_else(|| format!("número inválido: {text}"))
}

impl Commands {
    /// Endereço de uma função: símbolo do módulo ou número.
    fn resolve(&self, name: &str) -> Result<u64, String> {
        match self.symbols.get(name) {
            Some(addr) => Ok(*addr),
            None => parse_num(name).map_err(|_| format!("símbolo desconhecido: {name}")),
        }
    }

    /// Endereço e até 4 argumentos de `call` e `loop`.
    fn target_and_args<'a>(
        &self,
        parts: &mut impl Iterator<Item = &'a str>,
    ) -> Result<(u64, [u64; 4]), String> {
        let name = parts.next().ok_or("falta a função")?;
        let addr = self.resolve(name)?;
        let mut args = [0u64; 4];
        for (i, text) in parts.enumerate() {
            *args.get_mut(i).ok_or("no máximo 4 argumentos")? = parse_num(text)?;
        }
        Ok((addr, args))
    }

    /// Executa um comando e devolve o resto da resposta `ok` (com espaço na frente, ou vazio).
    fn handle(&mut self, line: &str) -> Result<String, String> {
        let mut parts = line.split_whitespace();
        match parts.next() {
            Some("call") => {
                let (addr, args) = self.target_and_args(&mut parts)?;
                Ok(format!(" {:#x}", call_win64(addr, args)))
            }
            Some("loop") => {
                let (addr, args) = self.target_and_args(&mut parts)?;
                let flag = Arc::clone(&self.stop);
                let handle = std::thread::spawn(move || {
                    let mut calls = 0u64;
                    let mut results: Vec<u64> = Vec::new();
                    // Pelo menos uma chamada, mesmo que o `stop` chegue antes de a thread rodar.
                    loop {
                        let ret = call_win64(addr, args);
                        calls += 1;
                        if results.len() < MAX_DISTINCT && !results.contains(&ret) {
                            results.push(ret);
                        }
                        if flag.load(Ordering::Relaxed) {
                            break;
                        }
                    }
                    (calls, results)
                });
                self.loops.push(LoopThread { handle });
                Ok(String::new())
            }
            Some("stop") => {
                self.stop.store(true, Ordering::Relaxed);
                let mut calls = 0u64;
                let mut results: Vec<u64> = Vec::new();
                for thread in self.loops.drain(..) {
                    let (n, found) = thread.handle.join().map_err(|_| "laço entrou em pânico")?;
                    calls += n;
                    for r in found {
                        if !results.contains(&r) {
                            results.push(r);
                        }
                    }
                }
                self.stop.store(false, Ordering::Relaxed);
                results.sort_unstable();
                let csv: Vec<String> = results.iter().map(|r| format!("{r:#x}")).collect();
                Ok(format!(" {calls} {}", csv.join(",")))
            }
            Some("park") => {
                let addr = self.resolve(parts.next().ok_or("falta o endereço")?)?;
                let pid = std::process::id() as i32;
                let tid = spawn_and_wait(addr, |tid| {
                    Ok(read_task_stat(pid, tid)?.is_some_and(|s| s.state == 'R'))
                })
                .map_err(|e| e.to_string())?;
                Ok(format!(" {tid}"))
            }
            Some(other) => Err(format!("comando desconhecido: {other}")),
            None => Err("comando vazio".to_string()),
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let spin = std::env::args().skip(1).any(|a| a == "--spin");
    let spin_value = std::env::args().skip(1).any(|a| a == "--spin-value");
    let with_ue = std::env::args().skip(1).any(|a| a == "--ue");
    let module_dir = std::env::args()
        .skip(1)
        .skip_while(|a| a != "--module")
        .nth(1);
    allow_any_tracer()?;

    // Montado em tempo de execução para o literal não existir em `.rodata`.
    let marker: Box<[u8]> = (0..32u8).map(|i| 0xA5 ^ i).collect::<Vec<u8>>().into();
    let exec = exec_page(0xCC)?;
    let exec2 = exec_code(&[], 2)?;
    let region = guarded_pages()?;
    let page = page_size();
    // 2 MiB + 1 página: passa de 1 MiB e termina fora de um limite de bloco.
    let big_len = (2usize << 20) + page as usize;
    let big_region = anon_rw(big_len)?;
    let ro_shared = readonly_shared_page(0x90)?;

    // Thread bloqueada para sempre num `read` de um pipe que ninguém escreve.
    let syscall_page = exec_code(&syscall_code(pipe_never_written()?), 1)?;
    let pid = std::process::id() as i32;
    spawn_and_wait(syscall_page, |tid| {
        Ok(matches!(
            read_thread_pos(pid, tid)?,
            Some(ThreadPos::Stopped { syscall: Some(0), pc }) if pc == syscall_page + SYSCALL_PC
        ))
    })?;

    // Thread que gira (só com `--spin`, para não saturar a CPU).
    let spin_page = exec_code(&SPIN_CODE, 1)?;
    if spin {
        spawn_and_wait(spin_page, |tid| {
            Ok(read_task_stat(pid, tid)?.is_some_and(|s| s.state == 'R'))
        })?;
    }

    // Laço de valor: a palavra fica numa página `rw` própria e a thread só existe com
    // `--spin-value`, como a do `--spin`.
    let value_word = anon_rw(page as usize)?;
    let value_page = exec_code(&value_code(value_word), 1)?;
    if spin_value {
        spawn_and_wait(value_page, |tid| {
            Ok(read_task_stat(pid, tid)?.is_some_and(|s| s.state == 'R'))
        })?;
    }

    // Reflexão sintética: as escritas da imagem aplicadas na arena, em ordem.
    let ue = if with_ue {
        let arena = anon_rw(gt_testkit::ue::ARENA_LEN)?;
        let (image, a) = gt_testkit::ue::build(arena, gt_testkit::ue::ARENA_LEN);
        for (addr, bytes) in &image.writes {
            write_local(*addr, bytes)?;
        }
        format!(
            ",\"ue\":{{\"gobjects\":\"{:#x}\",\"names\":\"{:#x}\",\"controller\":\"{:#x}\",\"pawn\":\"{:#x}\",\"creature\":\"{:#x}\",\"pawn_field_addr\":\"{:#x}\",\"health\":\"{:#x}\",\"alt_pawn\":\"{:#x}\",\"hauling\":\"{:#x}\",\"movement\":\"{:#x}\",\"creature_hauling\":\"{:#x}\",\"creature_movement\":\"{:#x}\",\"level\":\"{:#x}\",\"world\":\"{:#x}\",\"game_state\":\"{:#x}\",\"party\":\"{:#x}\",\"mode_manager\":\"{:#x}\",\"settings\":\"{:#x}\",\"cdo_settings\":\"{:#x}\"}}",
            a.gobjects,
            a.names,
            a.controller,
            a.pawn,
            a.creature,
            a.pawn_field_addr,
            a.health,
            a.alt_pawn,
            a.hauling,
            a.movement,
            a.creature_hauling,
            a.creature_movement,
            a.level,
            a.world,
            a.game_state,
            a.party,
            a.mode_manager,
            a.settings,
            a.cdo_settings
        )
    } else {
        String::new()
    };

    // Módulo sintético: arquivo no diretório dado, imagem mapeada como a do jogo.
    let mut symbols: HashMap<String, u64> = HashMap::new();
    let module_json = match module_dir {
        Some(dir) => {
            let installed = gt_testkit::module::install(Path::new(&dir))?;
            symbols.extend(installed.symbols.iter().cloned());
            let map: serde_json::Map<String, serde_json::Value> = installed
                .symbols
                .iter()
                .map(|(name, addr)| (name.clone(), format!("{addr:#x}").into()))
                .collect();
            let json = serde_json::json!({
                "base": format!("{:#x}", installed.base),
                "path": installed.path.to_string_lossy(),
                "symbols": map,
            });
            format!(",\"module\":{json}")
        }
        None => String::new(),
    };

    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "{{\"pid\":{},\"marker\":\"{:#x}\",\"marker_len\":{},\"exec_page\":\"{:#x}\",\"exec2\":\"{:#x}\",\"spin_page\":\"{:#x}\",\"syscall_page\":\"{:#x}\",\"value_page\":\"{:#x}\",\"value_word\":\"{:#x}\",\"region_start\":\"{:#x}\",\"big_region\":\"{:#x}\",\"big_len\":{},\"ro_shared\":\"{:#x}\",\"page\":{}{}{}}}",
        std::process::id(),
        marker.as_ptr() as u64,
        marker.len(),
        exec,
        exec2,
        spin_page,
        syscall_page,
        value_page,
        value_word,
        region,
        big_region,
        big_len,
        ro_shared,
        page,
        ue,
        module_json,
    )?;
    out.flush()?;
    drop(out);

    // Atende os comandos até o pai fechar o stdin.
    let mut state = Commands {
        symbols,
        loops: Vec::new(),
        stop: Arc::new(AtomicBool::new(false)),
    };
    for line in std::io::stdin().lock().lines() {
        let reply = match state.handle(&line?) {
            Ok(reply) => format!("ok{reply}"),
            Err(err) => format!("err {err}"),
        };
        let mut out = std::io::stdout().lock();
        if writeln!(out, "{reply}").and_then(|()| out.flush()).is_err() {
            break;
        }
    }

    // Mantém o marcador vivo até aqui.
    std::hint::black_box(&marker);
    Ok(())
}
