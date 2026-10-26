#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]
//! `GameBackend` não vaza descritores de arquivo ao reabrir o alvo (spec 003, 6.2).
//!
//! Roda com `cargo test -p gt-tui --test game_fd -- --ignored`, com o jogo aberto. Não escreve
//! no jogo; o diário vai para um diretório temporário.

use std::sync::Arc;

use gt_core::journal::JournalStore;
use gt_tui::backend::{Backend, Find};
use gt_tui::game::GameBackend;

/// Quantos descritores o processo tem agora.
fn fd_count() -> usize {
    std::fs::read_dir("/proc/self/fd").unwrap().count()
}

#[test]
#[ignore = "exige o jogo aberto"]
fn game_backend_reopen_50_times_no_fd_leak() {
    let dir = tempfile::tempdir().unwrap();
    let store = JournalStore::new(dir.path().to_path_buf());
    let lock = Arc::new(store.lock().unwrap());
    let mut backend = GameBackend::new(None, store, lock);
    let Find::Found(pid) = backend.find() else {
        panic!("o jogo está aberto?");
    };
    let search_fn = backend.search_fn();

    // A contagem inicial já inclui a trava do diário e o diretório temporário.
    let before = fd_count();
    for _ in 0..50 {
        let search = search_fn(pid, None, None).unwrap();
        let session = backend.open(search, true).unwrap();
        drop(session);
    }
    let after = fd_count();
    assert!(
        after <= before + 2,
        "vazamento de descritores: {before} antes, {after} depois"
    );
}
