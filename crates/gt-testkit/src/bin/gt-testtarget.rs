//! Processo-alvo de teste: expõe um marcador, páginas executáveis, uma região
//! com página ilegível, uma região `rw` grande e uma página compartilhada somente
//! leitura, e espera o `stdin` fechar. Nunca é o jogo.
//!
//! Argumento opcional `--spin`: cria uma thread que gira para sempre em `spin_page`.
//! Argumento opcional `--spin-value`: cria uma thread no laço de valor em `value_page`,
//! que escreve o imediato do `mov eax, imm32` em `value_word`.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use gt_core::mem::test_support::{
    allow_any_tracer, anon_rw, exec_code, exec_page, guarded_pages, page_size, pipe_never_written,
    readonly_shared_page, spawn_thread_at, value_code,
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

/// Cria uma thread em `addr` e espera `ready` aceitar o TID novo (poll a cada 1 ms).
fn spawn_and_wait(
    addr: u64,
    ready: impl Fn(i32) -> Result<bool, Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let pid = std::process::id() as i32;
    let before = list_tids(pid)?;
    spawn_thread_at(addr)?;
    let start = Instant::now();
    loop {
        for tid in list_tids(pid)? {
            if !before.contains(&tid) && ready(tid)? {
                return Ok(());
            }
        }
        if start.elapsed() > READY_TIMEOUT {
            return Err("thread auxiliar não chegou ao estado esperado".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let spin = std::env::args().skip(1).any(|a| a == "--spin");
    let spin_value = std::env::args().skip(1).any(|a| a == "--spin-value");
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

    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "{{\"pid\":{},\"marker\":\"{:#x}\",\"marker_len\":{},\"exec_page\":\"{:#x}\",\"exec2\":\"{:#x}\",\"spin_page\":\"{:#x}\",\"syscall_page\":\"{:#x}\",\"value_page\":\"{:#x}\",\"value_word\":\"{:#x}\",\"region_start\":\"{:#x}\",\"big_region\":\"{:#x}\",\"big_len\":{},\"ro_shared\":\"{:#x}\",\"page\":{}}}",
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
    )?;
    out.flush()?;
    drop(out);

    // Espera o pai fechar o stdin.
    let mut stdin = std::io::stdin().lock();
    let mut byte = [0u8; 1];
    while stdin.read(&mut byte)? > 0 {}

    // Mantém o marcador vivo até aqui.
    std::hint::black_box(&marker);
    Ok(())
}
