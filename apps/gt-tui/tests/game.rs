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

// ---- cheats de valor (spec 005, plano 005 tarefa 19) ----

use std::time::{Duration, Instant};

use gt_core::journal::Loaded;
use gt_tui::backend::{JobKind, ReflectionJob};

const FIELDS: [&str; 21] = [
    "vida-infinita",
    "stamina-infinita",
    "comida-infinita",
    "agua-infinita",
    "oxigenio-infinito",
    "temperatura-estavel",
    "sem-dano-queda",
    "nao-morrer",
    "pet-vida-infinita",
    "pet-nao-morre",
    "pet-stamina-infinita",
    "carga",
    "dano",
    "somar-ciencia",
    "velocidade",
    "pulos-infinitos",
    "gravidade",
    "construir-sem-ingredientes",
    "dano-recebido",
    "pular-hora",
    "congelar-hora",
];

/// Roda um trabalho da tarefa de reflexão nesta thread e junta o resultado na sessão.
fn run_job<S: Session<P>, P>(session: &mut S, kind: JobKind) -> Duration {
    let job: ReflectionJob = session.reflection_job(kind).expect("trabalho disponível");
    let out = (job.run)();
    let took = out.took;
    session.merge_reflection(out);
    took
}

#[test]
#[ignore = "exige o jogo aberto"]
fn game_globals_layout_and_fields_available() {
    let dir = tempfile::tempdir().unwrap();
    let store = JournalStore::new(dir.path().to_path_buf());
    let lock = Arc::new(store.lock().unwrap());
    let mut backend = GameBackend::new(None, store, lock);
    let Find::Found(pid) = backend.find() else {
        panic!("o jogo está aberto?");
    };
    let started = Instant::now();
    let search = (backend.search_fn())(pid, None, None).unwrap();
    eprintln!("busca (patches e globais): {:?}", started.elapsed());
    let ids: Vec<&str> = search
        .cheats
        .iter()
        .filter(|c| c.field)
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(ids, FIELDS);
    assert!(
        search
            .cheats
            .iter()
            .filter(|c| c.field)
            .all(|c| c.avail == Avail::Checking)
    );
    // Só leitura: sem motor.
    let mut session = backend.open(search, false).unwrap();
    let took = run_job(&mut session, JobKind::Index);
    eprintln!("índice da reflexão: {took:?}");
    for (id, avail) in session.field_avail() {
        assert_eq!(avail, Avail::Available { verified: true }, "{id}");
    }
    assert!(session.needs_controller(Instant::now()));
    let took = run_job(&mut session, JobKind::Controller);
    eprintln!("procura do controller: {took:?}");
    assert!(!session.needs_controller(Instant::now()));
}

#[test]
#[ignore = "exige o jogo aberto e escreve nele (faça o backup do save antes)"]
fn game_bits_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let store = JournalStore::new(dir.path().to_path_buf());
    let lock = Arc::new(store.lock().unwrap());
    let mut backend = GameBackend::new(None, store.clone(), lock);
    let Find::Found(pid) = backend.find() else {
        panic!("o jogo está aberto?");
    };
    let search = (backend.search_fn())(pid, None, None).unwrap();
    if search.version_key != KNOWN_BUILD {
        eprintln!("pulado: build diferente ({})", search.version_key);
        return;
    }
    assert!(matches!(backend.recover(), Recovered::Clean { .. }));
    let mut session = backend.open(search, true).unwrap();
    assert!(session.has_engine());
    run_job(&mut session, JobKind::Index);
    run_job(&mut session, JobKind::Controller);

    let read = |addr: u64| {
        let mut b = [0u8; 1];
        MemReader::new(pid).read_exact(addr, &mut b).unwrap();
        b[0]
    };
    for id in ["sem-dano-queda", "nao-morrer"] {
        let on = session.field_on(id, 0);
        assert!(matches!(on, OpOutcome::Done(_)), "{id}: {on:?}");
        let report = session.field_tick(&[id.to_string()], Instant::now());
        assert!(report.notes.is_empty(), "{id}: {:?}", report.notes);
        let Loaded::Ok(journal) = store.peek().unwrap() else {
            panic!("{id}: o diário devia ter a entrada dos bits");
        };
        let entry = journal
            .data
            .iter()
            .find(|e| e.id == id)
            .unwrap_or_else(|| panic!("{id}: sem entrada (os bits já estavam limpos?)"));
        let (addr, original, mask) = (entry.addr, entry.original[0], entry.mask);
        eprintln!("{id}: byte {addr:#x} original {original:#04x} máscara {mask:#04x}");
        assert_eq!(read(addr) & mask, 0, "{id}: bits limpos");
        let off = session.field_off(id);
        assert!(matches!(off, OpOutcome::Done(_)), "{id}: {off:?}");
        assert_eq!(read(addr) & mask, original & mask, "{id}: bits de volta");
    }
    assert!(session.pending_ids().is_empty());
    assert_eq!(store.peek().unwrap(), Loaded::Missing);
}

/// Ajustes `set` da spec 006: liga cada um com o valor padrão da grade (ou o `target`), confere o
/// valor no jogo pelo diário e, ao desligar, que o original voltou (spec 006, 6.4 e 6.8).
#[test]
#[ignore = "exige o jogo aberto e escreve nele (faça o backup do save antes)"]
fn game_set_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let store = JournalStore::new(dir.path().to_path_buf());
    let lock = Arc::new(store.lock().unwrap());
    let mut backend = GameBackend::new(None, store.clone(), lock);
    let Find::Found(pid) = backend.find() else {
        panic!("o jogo está aberto?");
    };
    let search = (backend.search_fn())(pid, None, None).unwrap();
    if search.version_key != KNOWN_BUILD {
        eprintln!("pulado: build diferente ({})", search.version_key);
        return;
    }
    let defaults: Vec<(String, u64)> = search
        .cheats
        .iter()
        .map(|c| (c.id.clone(), c.value.as_ref().map_or(0, |g| g.default_k)))
        .collect();
    assert!(matches!(backend.recover(), Recovered::Clean { .. }));
    let mut session = backend.open(search, true).unwrap();
    assert!(session.has_engine());
    run_job(&mut session, JobKind::Index);
    run_job(&mut session, JobKind::Controller);

    let read = |addr: u64, len: usize| {
        let mut b = vec![0u8; len];
        MemReader::new(pid).read_exact(addr, &mut b).unwrap();
        b
    };
    for id in [
        "carga",
        "dano",
        "velocidade",
        "pulos-infinitos",
        "gravidade",
    ] {
        let k = defaults
            .iter()
            .find(|(d, _)| d == id)
            .map_or(0, |(_, k)| *k);
        let on = session.field_on(id, k);
        assert!(matches!(on, OpOutcome::Done(_)), "{id}: {on:?}");
        let report = session.field_tick(&[id.to_string()], Instant::now());
        assert!(report.notes.is_empty(), "{id}: {:?}", report.notes);
        let Loaded::Ok(journal) = store.peek().unwrap() else {
            panic!("{id}: o diário devia ter a entrada do ajuste");
        };
        let entries: Vec<_> = journal
            .data
            .iter()
            .filter(|e| e.id == id)
            .cloned()
            .collect();
        assert!(!entries.is_empty(), "{id}: sem entrada no diário");
        for e in &entries {
            eprintln!(
                "{id}: {:#x} original {:02x?} escrito {:02x?}",
                e.addr, e.original, e.written
            );
            assert_eq!(
                read(e.addr, e.written.len()),
                e.written,
                "{id}: valor no jogo"
            );
        }
        let off = session.field_off(id);
        assert!(matches!(off, OpOutcome::Done(_)), "{id}: {off:?}");
        for e in &entries {
            assert_eq!(
                read(e.addr, e.original.len()),
                e.original,
                "{id}: original de volta"
            );
        }
    }
    assert!(session.pending_ids().is_empty());
    assert_eq!(store.peek().unwrap(), Loaded::Missing);
}

/// Par (original do jogo, valor escrito pelo cheat) de um campo, em bytes.
type Pair = (Vec<u8>, Vec<u8>);

/// Cheats do mundo da spec 007 (`construir-sem-ingredientes` e `dano-recebido`): liga e desliga cada
/// um com o jogo aberto, confere o valor escrito nas configurações do modo e, no fim, que os
/// originais voltaram: `bRecipesRequireIngredients = true`, `bAutoCompleteBuildings = false` e
/// `EnemyDamageMultiplier = 1.0` (spec 007, 9).
#[test]
#[ignore = "exige o jogo aberto e escreve nele (faça o backup do save antes)"]
fn world_cheats_toggle_and_restore() {
    let dir = tempfile::tempdir().unwrap();
    let store = JournalStore::new(dir.path().to_path_buf());
    let lock = Arc::new(store.lock().unwrap());
    let mut backend = GameBackend::new(None, store.clone(), lock);
    let Find::Found(pid) = backend.find() else {
        panic!("o jogo está aberto?");
    };
    let search = (backend.search_fn())(pid, None, None).unwrap();
    if search.version_key != KNOWN_BUILD {
        eprintln!("pulado: build diferente ({})", search.version_key);
        return;
    }
    let defaults: Vec<(String, u64)> = search
        .cheats
        .iter()
        .map(|c| (c.id.clone(), c.value.as_ref().map_or(0, |g| g.default_k)))
        .collect();
    assert!(matches!(backend.recover(), Recovered::Clean { .. }));
    let mut session = backend.open(search, true).unwrap();
    assert!(session.has_engine());
    run_job(&mut session, JobKind::Index);
    run_job(&mut session, JobKind::Controller);

    let read = |addr: u64, len: usize| {
        let mut b = vec![0u8; len];
        MemReader::new(pid).read_exact(addr, &mut b).unwrap();
        b
    };
    // Pares (original do jogo, valor do cheat) esperados de cada cheat (spec 007, 6.1):
    // bRecipesRequireIngredients (true -> false), bAutoCompleteBuildings (false -> true) e
    // EnemyDamageMultiplier (1.0 -> 0.25). O diário não guarda o nome do campo, então a
    // conferência é por multiconjunto de pares, ordenado: trocar ou repetir valores falha.
    let cases: [(&str, Vec<Pair>); 2] = [
        (
            "construir-sem-ingredientes",
            vec![(vec![0], vec![1]), (vec![1], vec![0])],
        ),
        (
            "dano-recebido",
            vec![(
                1.0f32.to_le_bytes().to_vec(),
                0.25f32.to_le_bytes().to_vec(),
            )],
        ),
    ];
    for (id, mut expected) in cases {
        expected.sort();
        let k = defaults
            .iter()
            .find(|(d, _)| d == id)
            .map_or(0, |(_, k)| *k);
        let on = session.field_on(id, k);
        assert!(matches!(on, OpOutcome::Done(_)), "{id}: {on:?}");
        let report = session.field_tick(&[id.to_string()], Instant::now());
        assert!(report.notes.is_empty(), "{id}: {:?}", report.notes);
        let Loaded::Ok(journal) = store.peek().unwrap() else {
            panic!("{id}: o diário devia ter as entradas do ajuste");
        };
        let entries: Vec<_> = journal
            .data
            .iter()
            .filter(|e| e.id == id)
            .cloned()
            .collect();
        let mut pairs: Vec<Pair> = entries
            .iter()
            .map(|e| (e.original.clone(), e.written.clone()))
            .collect();
        pairs.sort();
        assert_eq!(pairs, expected, "{id}: pares (original, escrito) do diário");
        for e in &entries {
            eprintln!(
                "{id}: {:#x} original {:02x?} escrito {:02x?}",
                e.addr, e.original, e.written
            );
            assert_eq!(
                read(e.addr, e.written.len()),
                e.written,
                "{id}: valor no jogo"
            );
        }
        let off = session.field_off(id);
        assert!(matches!(off, OpOutcome::Done(_)), "{id}: {off:?}");
        for e in &entries {
            assert_eq!(
                read(e.addr, e.original.len()),
                e.original,
                "{id}: original de volta"
            );
        }
    }
    assert!(session.pending_ids().is_empty());
    assert_eq!(store.peek().unwrap(), Loaded::Missing);
}
