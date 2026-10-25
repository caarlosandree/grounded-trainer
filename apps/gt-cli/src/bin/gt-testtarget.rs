//! Processo-alvo de teste: expõe um marcador, uma página executável e uma região
//! com página ilegível, e espera o `stdin` fechar. Nunca é o jogo.

use std::io::{Read, Write};

use gt_core::mem::test_support::{allow_any_tracer, exec_page, guarded_pages};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    allow_any_tracer()?;

    // Montado em tempo de execução para o literal não existir em `.rodata`.
    let marker: Box<[u8]> = (0..32u8).map(|i| 0xA5 ^ i).collect::<Vec<u8>>().into();
    let exec = exec_page(0xCC)?;
    let region = guarded_pages()?;

    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "{{\"pid\":{},\"marker\":\"{:#x}\",\"marker_len\":{},\"exec_page\":\"{:#x}\",\"region_start\":\"{:#x}\"}}",
        std::process::id(),
        marker.as_ptr() as u64,
        marker.len(),
        exec,
        region,
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
