#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]
//! Testes do `gt-patch` contra o Grounded 2 aberto.
//!
//! Rodam com `cargo test -p gt-patch --test game -- --ignored`, com o jogo aberto na build
//! `c935e799-0b27c000` (nas outras, pulam com aviso). Escrevem no jogo e desfazem; o diário vai
//! para um diretório temporário.

use std::process::{Command, Output};

use gt_testkit::bin_path;

/// Chave de versão da build em que o patch `teste-codificacao` foi medido.
const KNOWN_BUILD: &str = "c935e799-0b27c000";

/// Roda `gt-patch <args...>` com o diário num diretório temporário.
fn gt_patch(state: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_gt-patch"))
        .env("XDG_STATE_HOME", state)
        .args(args)
        .output()
        .unwrap()
}

/// Roda `gt --json <args...>` e devolve o JSON; falha com a mensagem do `gt` em caso de erro.
fn gt_json(args: &[&str]) -> serde_json::Value {
    let out = Command::new(bin_path("gt"))
        .arg("--json")
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "`gt --json {}` falhou (o jogo está aberto?): {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// `true` se o jogo aberto é a build conhecida; senão avisa.
fn known_build() -> bool {
    let key = gt_json(&["inspect"])["version_key"]
        .as_str()
        .unwrap()
        .to_string();
    if key != KNOWN_BUILD {
        eprintln!("pulado: build diferente ({key})");
    }
    key == KNOWN_BUILD
}

fn parse_hex(text: &str) -> u64 {
    u64::from_str_radix(text.trim_start_matches("0x"), 16).unwrap()
}

/// `true` se o trecho do `gt diff` em `[start, start + len)` encosta no patch em
/// `[addr, addr + patch_len)`.
fn touches_patch(start: u64, len: u64, addr: u64, patch_len: u64) -> bool {
    start < addr + patch_len && addr < start + len
}

#[test]
fn touches_patch_sees_difference_after_first_byte() {
    // `recarga-instantanea` troca `0f 28 c7` por `0f 57 c0`: o primeiro byte não muda, então uma
    // sobra do patch aparece no diff a partir do segundo byte.
    assert!(touches_patch(0x1001, 2, 0x1000, 3));
    assert!(touches_patch(0x0ff0, 0x20, 0x1000, 3));
    assert!(!touches_patch(0x1003, 4, 0x1000, 3));
    assert!(!touches_patch(0x0ff0, 0x10, 0x1000, 3));
}

#[test]
#[ignore = "exige o jogo aberto"]
fn list_shows_test_patch_verified() {
    if !known_build() {
        return;
    }
    let state = tempfile::tempdir().unwrap();
    let out = gt_patch(state.path(), &["list"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("teste-codificacao"), "{text}");
    assert!(text.contains("disponível (verificado)"), "{text}");
}

/// Liga e desliga `id` (que troca `patch_len` bytes) uma vez e confere que a memória volta ao disco
/// em todos os bytes do patch.
fn round_trip(id: &str, patch_len: u64) {
    let state = tempfile::tempdir().unwrap();
    let list = gt_patch(state.path(), &["list"]);
    let text = String::from_utf8_lossy(&list.stdout).into_owned();
    // `<id>  <estado>  <módulo>+<offset>`
    let line = text
        .lines()
        .find(|l| l.split_whitespace().next() == Some(id))
        .unwrap_or_else(|| panic!("patch {id} ausente em: {text}"));
    assert!(line.contains("disponível (verificado)"), "{line}");
    let offset = parse_hex(line.rsplit_once('+').unwrap().1.trim());
    let base = parse_hex(gt_json(&["inspect"])["module"]["base"].as_str().unwrap());
    let patch_addr = base + offset;

    let out = gt_patch(state.path(), &["toggle", id, "--times", "1"]);
    assert!(
        out.status.success(),
        "{id}: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Depois do ciclo, a memória volta ao disco: nenhuma diferença encosta nos bytes do patch.
    let diff = gt_json(&["diff"]);
    for chunk in diff["chunks"].as_array().unwrap() {
        let start = parse_hex(chunk["address"].as_str().unwrap());
        let len = chunk["len"].as_u64().unwrap();
        assert!(
            !touches_patch(start, len, patch_addr, patch_len),
            "{id} deixou diferença em {patch_addr:#x}: {chunk}"
        );
    }
}

#[test]
#[ignore = "exige o jogo aberto"]
fn single_toggle_round_trip() {
    if !known_build() {
        return;
    }
    round_trip("teste-codificacao", 3);
}

/// Cheats da Fase 4 (spec 004): cada um liga e desliga uma vez no endereço verificado.
#[test]
#[ignore = "exige o jogo aberto"]
fn phase4_cheats_round_trip() {
    if !known_build() {
        return;
    }
    for (id, patch_len) in [
        ("durabilidade-infinita", 5),
        ("energia-infinita", 4),
        ("recarga-instantanea", 3),
    ] {
        round_trip(id, patch_len);
    }
}
