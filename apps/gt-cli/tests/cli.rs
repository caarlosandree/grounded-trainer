#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Integração do binário `gt` contra o processo-alvo de teste (nunca contra o jogo).

mod support;

use std::process::{Command, Output};

use support::{TestTarget, marker_bytes};

/// Roda `gt --pid <alvo> --module <module> <args...>`.
fn gt_with(pid: i32, module: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_gt"))
        .args(["--pid", &pid.to_string(), "--module", module])
        .args(args)
        .output()
        .unwrap()
}

/// Roda `gt` contra o alvo, com `--module gt-testtarget`.
fn gt(t: &TestTarget, args: &[&str]) -> Output {
    gt_with(t.pid, "gt-testtarget", args)
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn marker_hex() -> String {
    marker_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn read_shows_marker() {
    let t = TestTarget::spawn();
    let out = gt(&t, &["--json", "read", &format!("{:#x}", t.marker), "32"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(v["bytes_hex"], marker_hex());
    assert_eq!(v["address"], format!("{:#x}", t.marker));
    assert_eq!(v["size"], 32);
    assert!(v["label"].is_null());
    assert_eq!(v["holes"], serde_json::json!([]));
}

#[test]
fn read_text_is_hexdump() {
    let t = TestTarget::spawn();
    let out = gt(&t, &["read", &format!("{:x}", t.marker), "32"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    let first = text.lines().next().unwrap();
    assert!(
        first.starts_with(&format!("0x{:016x}", t.marker)),
        "{first}"
    );
    let hex_part = first.split_once("  ").unwrap().1;
    let hex_part = hex_part.split('|').next().unwrap();
    let bytes: Vec<&str> = hex_part.split_whitespace().collect();
    assert_eq!(bytes.len(), 16, "{first}");
    assert_eq!(&bytes[..4], ["a5", "a4", "a7", "a6"]);
    assert_eq!(text.lines().count(), 2, "{text}");
}

#[test]
fn inspect_on_elf_says_not_pe() {
    let t = TestTarget::spawn();
    let out = gt(&t, &["inspect"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("não é uma imagem PE"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn unknown_module_with_shortcut_errors() {
    let t = TestTarget::spawn();
    let out = gt_with(t.pid, "nada.exe", &["read", "+10"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("não encontrado"), "{}", stderr(&out));
}

#[test]
fn absolute_read_ignores_missing_module() {
    let t = TestTarget::spawn();
    let out = gt_with(
        t.pid,
        "nada.exe",
        &["read", &format!("{:#x}", t.marker), "4"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn read_on_exec_page_is_blocked() {
    let t = TestTarget::spawn();
    let out = gt(&t, &["read", &format!("{:#x}", t.exec_page), "16"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("bloqueado pela regra do projeto"),
        "{}",
        stderr(&out)
    );
    assert!(
        !stdout(&out).contains("cc cc"),
        "nenhum byte da página pode vazar"
    );
}

#[test]
fn read_hole_shows_question_marks() {
    let t = TestTarget::spawn();
    let out = gt(&t, &["read", &format!("{:#x}", t.region_start), "12288"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("??"), "{}", stdout(&out));
}

#[test]
fn read_hole_is_listed_in_json() {
    let t = TestTarget::spawn();
    let out = gt(
        &t,
        &["--json", "read", &format!("{:#x}", t.region_start), "12288"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(
        v["holes"],
        serde_json::json!([{
            "start": format!("{:#x}", t.region_start + 4096),
            "end": format!("{:#x}", t.region_start + 8192),
        }])
    );
}

#[test]
fn read_rejects_size_over_limit() {
    let t = TestTarget::spawn();
    let out = gt(&t, &["read", &format!("{:#x}", t.marker), "0xFFFFFFFF"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("1 MiB"), "{}", stderr(&out));
}

#[test]
fn read_rejects_address_overflow() {
    let t = TestTarget::spawn();
    let out = gt(&t, &["read", "FFFFFFFFFFFFFFF0", "32"]);
    assert!(!out.status.success());
    assert!(!stderr(&out).contains("panicked"), "{}", stderr(&out));
}

#[test]
fn read_after_target_exits_reports_gone() {
    let mut t = TestTarget::spawn();
    let pid = t.pid;
    t.child.kill().unwrap();
    t.child.wait().unwrap();
    let out = gt_with(pid, "gt-testtarget", &["read", &format!("{:#x}", t.marker)]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("terminou"), "{}", stderr(&out));
}

#[test]
fn no_game_running_message() {
    let procs = gt_core::process::list_processes().unwrap();
    if procs
        .iter()
        .any(|(_, args)| gt_grounded2::target::is_game(args))
    {
        eprintln!("aviso: o jogo está aberto; teste ignorado");
        return;
    }
    let out = Command::new(env!("CARGO_BIN_EXE_gt"))
        .args(["read", "0x1000"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("abra o Grounded 2"),
        "{}",
        stderr(&out)
    );
}
