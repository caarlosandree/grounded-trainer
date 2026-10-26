#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]
//! `GameBackend` contra o Grounded 2 aberto (spec 003, 5).
//!
//! Rodam com `cargo test -p gt-tui --test game -- --ignored`, com o jogo aberto na build
//! `c935e799-0b27c000` (nas outras, pulam com aviso). Escrevem no jogo e desfazem; o diário vai
//! para um diretório temporário.

use std::sync::Arc;

use gt_core::journal::JournalStore;
use gt_core::mem::MemReader;
use gt_tui::backend::{Backend, Find, OpOutcome, Recovered, Session};
use gt_tui::game::GameBackend;
use gt_tui::model::Avail;

/// Chave de versão da build em que o patch `teste-codificacao` foi medido.
const KNOWN_BUILD: &str = "c935e799-0b27c000";
const TEST_PATCH: &str = "teste-codificacao";
const ORIGINAL: [u8; 3] = [0x48, 0x0b, 0xc2];
const PATCHED: [u8; 3] = [0x48, 0x09, 0xd0];

#[test]
#[ignore = "exige o jogo aberto"]
fn game_backend_toggles_test_patch() {
    let dir = tempfile::tempdir().unwrap();
    let store = JournalStore::new(dir.path().to_path_buf());
    let lock = Arc::new(store.lock().unwrap());
    let mut backend = GameBackend::new(None, store, lock);

    let Find::Found(pid) = backend.find() else {
        panic!("o jogo está aberto?");
    };
    let search = (backend.search_fn())(pid, None, None).unwrap();
    if search.version_key != KNOWN_BUILD {
        eprintln!("pulado: build diferente ({})", search.version_key);
        return;
    }
    let cheat = search
        .cheats
        .iter()
        .find(|c| c.id == TEST_PATCH)
        .expect("patch de teste no catálogo");
    assert_eq!(cheat.avail, Avail::Available { verified: true });
    let addr = search
        .payload
        .1
        .entries
        .iter()
        .find(|e| e.def.id == TEST_PATCH)
        .and_then(|e| e.state.apply_addr())
        .expect("endereço resolvido");

    assert!(matches!(backend.recover(), Recovered::Clean { .. }));
    let mut session = backend.open(search, true).unwrap();
    assert!(session.has_engine());

    let read = || {
        let mut buf = [0u8; 3];
        MemReader::new(pid).read_exact(addr, &mut buf).unwrap();
        buf
    };
    assert_eq!(read(), ORIGINAL);
    let on = session.apply(&[(TEST_PATCH.to_string(), 0)]);
    assert!(matches!(on, OpOutcome::Done(_)), "{on:?}");
    assert_eq!(read(), PATCHED);
    let off = session.undo(TEST_PATCH);
    assert!(matches!(off, OpOutcome::Done(_)), "{off:?}");
    assert_eq!(read(), ORIGINAL);
    assert!(session.pending_ids().is_empty());
    assert!(!session.exited());
}
