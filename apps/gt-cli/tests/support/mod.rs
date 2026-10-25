#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]
//! Apoio aos testes de integração: inicia e encerra o `gt-testtarget`.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

/// Processo-alvo de teste em execução.
pub struct TestTarget {
    pub child: Child,
    pub pid: i32,
    pub marker: u64,
    pub exec_page: u64,
    /// Início de 3 páginas; a do meio (`region_start + 4096`) é ilegível.
    pub region_start: u64,
}

fn parse_hex(v: &serde_json::Value) -> u64 {
    let s = v.as_str().unwrap();
    u64::from_str_radix(s.trim_start_matches("0x"), 16).unwrap()
}

impl TestTarget {
    pub fn spawn() -> TestTarget {
        let mut child = Command::new(env!("CARGO_BIN_EXE_gt-testtarget"))
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
            region_start: parse_hex(&v["region_start"]),
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
