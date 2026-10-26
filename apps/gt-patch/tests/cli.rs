#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]
//! Integração do binário `gt-patch` sem o jogo: diário, recuperação e mensagens de erro.
//!
//! Cada teste aponta `XDG_STATE_HOME` para um diretório temporário.

use std::path::Path;
use std::process::{Command, Output};

use gt_core::journal::{Journal, JournalEntry, JournalStore};

/// Diretório do diário dentro de `state` (`$XDG_STATE_HOME/grounded-trainer`).
fn journal_dir(state: &Path) -> std::path::PathBuf {
    state.join("grounded-trainer")
}

/// Roda `gt-patch <args...>` com o diário em `state`.
fn gt_patch(state: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_gt-patch"))
        .env("XDG_STATE_HOME", state)
        .args(args)
        .output()
        .unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// O jogo está aberto? Os testes que dependem da ausência dele pulam com aviso.
fn game_is_open() -> bool {
    let procs = gt_core::process::list_processes().unwrap();
    let open = procs
        .iter()
        .any(|(_, args)| gt_grounded2::target::is_game(args));
    if open {
        eprintln!("aviso: o jogo está aberto; teste ignorado");
    }
    open
}

#[test]
fn list_without_game_says_so() {
    if game_is_open() {
        return;
    }
    let state = tempfile::tempdir().unwrap();
    let out = gt_patch(state.path(), &["list"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("jogo não encontrado"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn restore_without_journal() {
    let state = tempfile::tempdir().unwrap();
    let out = gt_patch(state.path(), &["restore"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("nada a restaurar"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn restore_discards_dead_process_journal() {
    let state = tempfile::tempdir().unwrap();
    // Um processo já terminado e colhido: o PID não existe mais.
    let mut child = Command::new("true").spawn().unwrap();
    let pid = i32::try_from(child.id()).unwrap();
    child.wait().unwrap();
    let store = JournalStore::new(journal_dir(state.path()));
    store
        .save(&Journal {
            boot_id: gt_core::procinfo::boot_id().unwrap(),
            pid,
            start_time: 1,
            version_key: "teste".into(),
            paused_by_us: false,
            entries: vec![JournalEntry {
                id: "teste-codificacao".into(),
                addr: 0x1_4000_1000,
                original: vec![0x48, 0x8B, 0xF9],
                replacement: vec![0x48, 0x89, 0xCF],
                next: None,
            }],
        })
        .unwrap();
    let out = gt_patch(state.path(), &["restore"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("diário descartado"),
        "{}",
        stdout(&out)
    );
    assert!(!journal_dir(state.path()).join("journal.json").exists());
}

#[test]
fn restore_moves_corrupt_journal() {
    let state = tempfile::tempdir().unwrap();
    let dir = journal_dir(state.path());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("journal.json"), "{").unwrap();
    let out = gt_patch(state.path(), &["restore"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(dir.join("journal.json.bad").exists());
    assert!(!dir.join("journal.json").exists());
    assert!(stdout(&out).contains("corrompido"), "{}", stdout(&out));
}

#[test]
fn toggle_and_profile_without_game_say_so() {
    if game_is_open() {
        return;
    }
    for args in [
        &["toggle", "x", "--times", "1"][..],
        &["profile", "--samples", "1"][..],
    ] {
        let state = tempfile::tempdir().unwrap();
        let out = gt_patch(state.path(), args);
        assert!(!out.status.success(), "{args:?}");
        assert!(
            stderr(&out).contains("jogo não encontrado"),
            "{args:?}: {}",
            stderr(&out)
        );
        // O `journal.lock` pode existir; o diário não.
        assert!(!journal_dir(state.path()).join("journal.json").exists());
    }
}

#[test]
fn restore_refused_while_locked() {
    let state = tempfile::tempdir().unwrap();
    let store = JournalStore::new(journal_dir(state.path()));
    let _lock = store.lock().unwrap();
    let out = gt_patch(state.path(), &["restore"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("outro gt-patch já está em execução"),
        "{}",
        stderr(&out)
    );
}
