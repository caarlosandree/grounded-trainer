#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Testes do binário `gt` contra o Grounded 2 aberto. Só leem; nenhum caminho escreve no jogo.
//!
//! Rodam com `cargo test -p gt-cli --test game -- --ignored`, com o jogo aberto. O `gt` acha o
//! jogo sozinho (sem `--pid`).

use std::process::{Command, Output};

/// Chave de versão da build em que os offsets de `RESEARCH.md` foram medidos.
const KNOWN_BUILD: &str = "c935e799-0b27c000";

/// Roda `gt --json <args...>` sem `--pid`.
fn gt_json(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_gt"))
        .arg("--json")
        .args(args)
        .output()
        .unwrap()
}

/// Roda o `gt` e devolve o JSON da saída; falha com a mensagem do `gt` se ele terminar com erro.
fn run_json(args: &[&str]) -> serde_json::Value {
    let out = gt_json(args);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "`gt --json {}` falhou (o jogo está aberto?): {stderr}",
        args.join(" ")
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|err| {
        panic!(
            "saída de `gt --json {}` não é JSON ({err}): {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

/// `^[0-9a-f]{8}-[0-9a-f]{8}$`.
fn is_version_key(key: &str) -> bool {
    let is_hex8 = |part: &str| {
        part.len() == 8
            && part
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    key.split_once('-')
        .is_some_and(|(a, b)| is_hex8(a) && is_hex8(b))
}

/// Chave de versão do jogo aberto, via `gt inspect`.
fn game_version_key() -> String {
    run_json(&["inspect"])["version_key"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
#[ignore = "exige o jogo aberto"]
fn inspect_finds_game() {
    let v = run_json(&["inspect"]);
    assert!(
        v["module"]["base"].as_str().is_some_and(|b| !b.is_empty()),
        "{v}"
    );
    let sections = v["sections"].as_array().unwrap();
    assert!(sections.len() >= 6, "seções: {}", sections.len());
    let key = v["version_key"].as_str().unwrap();
    assert!(is_version_key(key), "chave de versão inesperada: {key}");
}

#[test]
#[ignore = "exige o jogo aberto"]
fn known_build_bytes() {
    let key = game_version_key();
    if key != KNOWN_BUILD {
        eprintln!("pulado: build diferente ({key})");
        return;
    }
    let v = run_json(&["disasm", "+56FC11C", "-n", "1"]);
    let insn = &v["instructions"][0];
    let text = insn["text"].as_str().unwrap();
    assert!(text.starts_with("call"), "instrução: {text}");
    assert_eq!(
        insn["target_label"], "Grounded2Steam-Win64-Shipping.exe+39e6d10",
        "{insn}"
    );
}

#[test]
#[ignore = "exige o jogo aberto"]
fn diff_runs_on_text() {
    let out = gt_json(&["diff"]);
    assert!(
        out.status.success(),
        "`gt diff` falhou: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["chunks"].is_array(), "{v}");
}

#[test]
#[ignore = "exige o jogo aberto"]
fn fling_lists_symbols() {
    let key = game_version_key();
    let v = run_json(&["fling"]);
    let symbols = v["symbols"].as_array().unwrap();
    assert!(!symbols.is_empty(), "nenhum símbolo listado: {v}");
    if key == KNOWN_BUILD {
        assert_eq!(symbols.len(), 11, "{v}");
    } else {
        eprintln!("build diferente ({key}): só verifiquei que a lista não está vazia");
    }
}
