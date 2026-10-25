//! Processo-alvo de teste: expõe um marcador, uma página executável, uma região
//! com página ilegível e uma região `rw` grande, e espera o `stdin` fechar. Nunca é o jogo.

use std::io::{Read, Write};

use gt_core::mem::test_support::{allow_any_tracer, anon_rw, exec_page, guarded_pages, page_size};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    allow_any_tracer()?;

    // Montado em tempo de execução para o literal não existir em `.rodata`.
    let marker: Box<[u8]> = (0..32u8).map(|i| 0xA5 ^ i).collect::<Vec<u8>>().into();
    let exec = exec_page(0xCC)?;
    let region = guarded_pages()?;
    let page = page_size();
    // 2 MiB + 1 página: passa de 1 MiB e termina fora de um limite de bloco.
    let big_len = (2usize << 20) + page as usize;
    let big_region = anon_rw(big_len)?;

    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "{{\"pid\":{},\"marker\":\"{:#x}\",\"marker_len\":{},\"exec_page\":\"{:#x}\",\"region_start\":\"{:#x}\",\"big_region\":\"{:#x}\",\"big_len\":{},\"page\":{}}}",
        std::process::id(),
        marker.as_ptr() as u64,
        marker.len(),
        exec,
        region,
        big_region,
        big_len,
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
