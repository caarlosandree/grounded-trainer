//! Saída dos comandos: texto, JSON e hexdump.

use std::io::{self, Write};
use std::ops::Range;

use anyhow::Context;
use serde::Serialize;

/// Bytes por linha do hexdump.
const LINE: usize = 16;

/// Escreve o resultado de um comando em stdout: um único objeto JSON com `--json`, ou o texto.
///
/// `text` só é chamado no modo texto; `value` só é serializado no modo JSON.
pub fn emit(
    json: bool,
    text: impl FnOnce(&mut dyn Write) -> io::Result<()>,
    value: &impl Serialize,
) -> anyhow::Result<()> {
    let mut out = io::stdout().lock();
    if json {
        serde_json::to_writer(&mut out, value).context("falha ao gerar o JSON")?;
        writeln!(out)?;
    } else {
        text(&mut out)?;
    }
    out.flush()?;
    Ok(())
}

/// Hexdump de `bytes`, que começam em `start`: 16 bytes por linha, em hexadecimal minúsculo, com
/// a coluna ASCII entre barras (`.` para bytes não imprimíveis). Os bytes dentro de `holes`
/// (intervalos absolutos ilegíveis) aparecem como `??`.
pub fn hexdump(start: u64, bytes: &[u8], holes: &[Range<u64>]) -> String {
    let mut out = String::new();
    for (index, chunk) in bytes.chunks(LINE).enumerate() {
        let line_start = start.wrapping_add((index * LINE) as u64);
        let mut hex: Vec<String> = Vec::with_capacity(LINE);
        let mut ascii = String::with_capacity(LINE);
        for (i, byte) in chunk.iter().enumerate() {
            let addr = line_start.wrapping_add(i as u64);
            if holes.iter().any(|hole| hole.contains(&addr)) {
                hex.push("??".to_string());
                ascii.push('.');
            } else {
                hex.push(format!("{byte:02x}"));
                ascii.push(if byte.is_ascii_graphic() || *byte == b' ' {
                    char::from(*byte)
                } else {
                    '.'
                });
            }
        }
        // Linha curta: completa a coluna hexadecimal para alinhar a coluna ASCII.
        let hex = format!("{:<width$}", hex.join(" "), width = LINE * 3 - 1);
        out.push_str(&format!("{line_start:#018x}  {hex} |{ascii}|\n"));
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn two_lines_for_twenty_bytes() {
        let dump = hexdump(0x1000, &[0x41; 20], &[]);
        let lines: Vec<&str> = dump.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            format!(
                "0x0000000000001000  {} |AAAAAAAAAAAAAAAA|",
                ["41"; 16].join(" ")
            )
        );
        assert!(lines[1].starts_with("0x0000000000001010  41 41 41 41 "));
        assert!(lines[1].ends_with("|AAAA|"));
    }

    #[test]
    fn short_last_line_keeps_the_ascii_column_aligned() {
        let dump = hexdump(0x0, &[0x41; 17], &[]);
        let lines: Vec<&str> = dump.lines().collect();
        assert_eq!(lines[0].find('|'), lines[1].find('|'));
    }

    #[test]
    fn non_printable_bytes_become_dots() {
        let dump = hexdump(0x0, &[0x00, 0x7f, 0x20, 0x7e, 0xff], &[]);
        assert!(dump.trim_end().ends_with("|.. ~.|"), "{dump}");
    }

    #[test]
    fn holes_show_question_marks() {
        let mut bytes = vec![0x41u8; 20];
        bytes[4..8].fill(0);
        #[allow(clippy::single_range_in_vec_init)]
        let holes = [0x1004u64..0x1008];
        let dump = hexdump(0x1000, &bytes, &holes);
        let first = dump.lines().next().unwrap();
        assert!(first.contains("41 41 41 41 ?? ?? ?? ?? 41"), "{first}");
        assert!(first.ends_with("|AAAA....AAAAAAAA|"), "{first}");
    }

    #[test]
    fn empty_input_is_empty() {
        assert_eq!(hexdump(0x1000, &[], &[]), "");
    }
}
