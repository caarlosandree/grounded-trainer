#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Integração do binário `gt` contra o processo-alvo de teste (nunca contra o jogo).

use std::process::{Command, Output};

use gt_testkit::{TestTarget, marker_bytes};

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

#[test]
fn disasm_on_exec_page_is_blocked() {
    let t = TestTarget::spawn();
    let out = gt(&t, &["disasm", &format!("{:#x}", t.exec_page), "-n", "3"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("bloqueado pela regra do projeto"),
        "{}",
        stderr(&out)
    );
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
}

#[test]
fn disasm_on_data_is_allowed() {
    let t = TestTarget::spawn();
    let out = gt(&t, &["disasm", &format!("{:#x}", t.marker), "-n", "3"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out).lines().count(), 3, "{}", stdout(&out));
}

#[test]
fn disasm_json_has_instruction_fields() {
    let t = TestTarget::spawn();
    let out = gt(
        &t,
        &["--json", "disasm", &format!("{:#x}", t.marker), "-n", "2"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    let insns = v["instructions"].as_array().unwrap();
    assert_eq!(insns.len(), 2);
    assert_eq!(insns[0]["address"], format!("{:#x}", t.marker));
    assert!(insns[0]["bytes_hex"].as_str().unwrap().starts_with("a5"));
    assert!(insns[0]["text"].is_string());
    assert!(insns[0]["label"].is_null());
}

#[test]
fn disasm_rejects_huge_count() {
    let t = TestTarget::spawn();
    let out = gt(
        &t,
        &["disasm", &format!("{:#x}", t.marker), "-n", "4294967295"],
    );
    assert!(!out.status.success());
    assert!(stderr(&out).contains("10000"), "{}", stderr(&out));
}

#[test]
fn scan_on_elf_says_not_pe() {
    let t = TestTarget::spawn();
    let out = gt(&t, &["scan", "A5 A4"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("não é uma imagem PE"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn diff_on_elf_says_not_pe() {
    let t = TestTarget::spawn();
    let out = gt(&t, &["diff"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("não é uma imagem PE"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn fling_on_elf_says_not_pe() {
    let t = TestTarget::spawn();
    let out = gt(&t, &["fling"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("não é uma imagem PE"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn dump_on_exec_page_is_blocked() {
    let t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("x.bin");
    let out = gt(
        &t,
        &[
            "dump",
            &format!("{:#x}", t.exec_page),
            "4096",
            "-o",
            bin.to_str().unwrap(),
        ],
    );
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("bloqueado pela regra do projeto"),
        "{}",
        stderr(&out)
    );
    assert!(!bin.exists());
    assert!(!dir.path().join("x.json").exists());
}

#[test]
fn dump_range_writes_bin_and_json() {
    let t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("sub").join("m.bin");
    let out = gt(
        &t,
        &[
            "dump",
            &format!("{:#x}", t.marker),
            "32",
            "-o",
            bin.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(std::fs::read(&bin).unwrap(), marker_bytes());
    let json = std::fs::read_to_string(dir.path().join("sub").join("m.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["size"], 32);
    assert_eq!(v["start"], format!("{:#x}", t.marker));
    assert_eq!(v["holes"], serde_json::json!([]));
    assert!(v["version_key"].is_null());
}

#[test]
fn dump_refuses_overwrite() {
    let t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("m.bin");
    let args = [
        "dump",
        &format!("{:#x}", t.marker),
        "32",
        "-o",
        bin.to_str().unwrap(),
    ];
    assert!(gt(&t, &args).status.success());
    let second = gt(&t, &args);
    assert!(!second.status.success());
    assert!(stderr(&second).contains("já existe"), "{}", stderr(&second));
    let mut forced = args.to_vec();
    forced.push("--force");
    let third = gt(&t, &forced);
    assert!(third.status.success(), "{}", stderr(&third));
}

#[test]
fn dump_refuses_overwrite_when_only_json_exists() {
    let t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("m.json"), "{}").unwrap();
    let bin = dir.path().join("m.bin");
    let out = gt(
        &t,
        &[
            "dump",
            &format!("{:#x}", t.marker),
            "32",
            "-o",
            bin.to_str().unwrap(),
        ],
    );
    assert!(!out.status.success());
    assert!(!bin.exists());
}

#[test]
fn dump_default_path_uses_pid_dir() {
    let t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_gt"))
        .current_dir(dir.path())
        .args(["--pid", &t.pid.to_string(), "--module", "gt-testtarget"])
        .args(["dump", &format!("{:#x}", t.marker), "32"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let base = dir
        .path()
        .join("scratch/dumps")
        .join(format!("pid{}", t.pid));
    assert!(base.join(format!("{:x}.bin", t.marker)).exists());
    assert!(base.join(format!("{:x}.json", t.marker)).exists());
}

#[test]
fn dump_rejects_size_over_limit() {
    let t = TestTarget::spawn();
    let out = gt(&t, &["dump", &format!("{:#x}", t.marker), "0xFFFFFFFF"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("1 GiB"), "{}", stderr(&out));
}

#[test]
fn dump_hole_writes_zeros_and_lists_hole() {
    let t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("h.bin");
    let out = gt(
        &t,
        &[
            "dump",
            &format!("{:#x}", t.region_start),
            "12288",
            "-o",
            bin.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let bytes = std::fs::read(&bin).unwrap();
    assert_eq!(bytes.len(), 12288);
    assert!(bytes[4096..8192].iter().all(|b| *b == 0));
    let json = std::fs::read_to_string(dir.path().join("h.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        v["holes"],
        serde_json::json!([{
            "start": format!("{:#x}", t.region_start + 4096),
            "end": format!("{:#x}", t.region_start + 8192),
        }])
    );
}

#[test]
fn dump_section_on_elf_says_not_pe() {
    let t = TestTarget::spawn();
    let out = gt(&t, &["dump", ".text"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("não é uma imagem PE"),
        "{}",
        stderr(&out)
    );
}
