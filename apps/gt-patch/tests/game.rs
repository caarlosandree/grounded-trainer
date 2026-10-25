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

#[test]
#[ignore = "exige o jogo aberto"]
fn single_toggle_round_trip() {
    if !known_build() {
        return;
    }
    let state = tempfile::tempdir().unwrap();
    let list = gt_patch(state.path(), &["list"]);
    let text = String::from_utf8_lossy(&list.stdout).into_owned();
    // `<id>  <estado>  <módulo>+<offset>`
    let line = text
        .lines()
        .find(|l| l.starts_with("teste-codificacao"))
        .unwrap_or_else(|| panic!("patch ausente em: {text}"));
    let offset = parse_hex(line.rsplit_once('+').unwrap().1.trim());
    let base = parse_hex(gt_json(&["inspect"])["module"]["base"].as_str().unwrap());
    let patch_addr = base + offset;

    let out = gt_patch(
        state.path(),
        &["toggle", "teste-codificacao", "--times", "1"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Depois do ciclo, a memória volta ao disco: nenhuma diferença cobre o endereço do patch.
    let diff = gt_json(&["diff"]);
    for chunk in diff["chunks"].as_array().unwrap() {
        let start = parse_hex(chunk["address"].as_str().unwrap());
        let len = chunk["len"].as_u64().unwrap();
        assert!(
            !(start..start + len).contains(&patch_addr),
            "o patch deixou diferença em {patch_addr:#x}: {chunk}"
        );
    }
}
