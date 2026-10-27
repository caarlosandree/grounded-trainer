#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Testes de API viva do motor de patches contra o processo-alvo de teste (nunca contra o jogo).

use std::cell::Cell;
use std::ops::Range;
use std::os::unix::fs::PermissionsExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gt_core::Error;
use gt_core::engine::{DataFault, Engine, EngineOptions, PolicyFn, SetOutcome};
use gt_core::journal::{JournalEntry, JournalLock, JournalStore, Loaded};
use gt_core::mem::{MemReader, MemWriter};
use gt_core::patch::{PatchSpec, Validated, WritePolicy, validate};
use gt_core::procinfo::{Identity, ThreadPos, identity, list_tids, read_stat, read_task_stat};
use gt_core::recovery::recover;
use gt_core::suspend::{
    Fault, HookPoint, PauseConfig, is_stopped_now, open_checked, pause, sample_pcs, wait_until,
};
use gt_testkit::{TargetOpts, TestTarget};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

/// Estados das threads vivas do alvo.
fn states(pid: i32) -> Vec<char> {
    list_tids(pid)
        .unwrap()
        .into_iter()
        .filter_map(|tid| read_task_stat(pid, tid).unwrap())
        .map(|s| s.state)
        .collect()
}

/// Espera (até 2 s) nenhuma thread do alvo ficar em `T`.
fn wait_all_running(pid: i32) -> bool {
    wait_until(
        || Ok(!states(pid).contains(&'T')),
        Duration::from_secs(2),
        Duration::from_millis(1),
    )
    .unwrap()
}

#[test]
fn pause_stops_and_guard_resumes() {
    let t = TestTarget::spawn();
    let (pidfd, _id) = open_checked(t.pid).unwrap();
    let guard = pause(&pidfd, t.pid, &PauseConfig::default(), true).unwrap();
    assert!(guard.sent_stop());
    let st = states(t.pid);
    assert!(st.len() >= 2, "{st:?}");
    assert!(st.iter().all(|s| *s == 'T'), "{st:?}");
    let want = ThreadPos::Stopped {
        syscall: Some(0),
        pc: t.syscall_page + 22,
    };
    assert!(
        guard.threads().iter().any(|th| th.pos == Some(want)),
        "{:?}",
        guard.threads()
    );
    assert!(guard.still_stopped().unwrap());
    guard.resume().unwrap();
    assert!(wait_all_running(t.pid), "{:?}", states(t.pid));
}

#[test]
fn guard_drop_resumes() {
    let t = TestTarget::spawn();
    let (pidfd, _id) = open_checked(t.pid).unwrap();
    let guard = pause(&pidfd, t.pid, &PauseConfig::default(), false).unwrap();
    assert!(guard.threads().iter().all(|th| th.pos.is_none()));
    assert!(states(t.pid).iter().all(|s| *s == 'T'));
    drop(guard);
    assert!(wait_all_running(t.pid), "{:?}", states(t.pid));
}

#[test]
fn already_stopped_target_is_left_stopped() {
    let t = TestTarget::spawn();
    let (pidfd, _id) = open_checked(t.pid).unwrap();
    kill(Pid::from_raw(t.pid), Signal::SIGSTOP).unwrap();
    assert!(
        wait_until(
            || is_stopped_now(t.pid),
            Duration::from_secs(2),
            Duration::from_millis(1)
        )
        .unwrap()
    );
    let guard = pause(&pidfd, t.pid, &PauseConfig::default(), true).unwrap();
    assert!(!guard.sent_stop());
    guard.resume().unwrap();
    // Sem SIGCONT nosso, o alvo continua parado.
    std::thread::sleep(Duration::from_millis(50));
    assert!(is_stopped_now(t.pid).unwrap());
    assert!(states(t.pid).iter().all(|s| *s == 'T'));
}

#[test]
fn sample_pcs_stops_early() {
    let t = TestTarget::spawn();
    let (pidfd, _id) = open_checked(t.pid).unwrap();
    let threads = list_tids(t.pid).unwrap().len();
    let (asked, before, after) = (Cell::new(0), Cell::new(0), Cell::new(0));
    let pcs = sample_pcs(
        &pidfd,
        t.pid,
        &PauseConfig::default(),
        10,
        Duration::from_millis(1),
        |will_stop| {
            assert!(will_stop);
            before.set(before.get() + 1);
            Ok(())
        },
        || {
            after.set(after.get() + 1);
            Ok(())
        },
        || {
            asked.set(asked.get() + 1);
            asked.get() >= 3
        },
    )
    .unwrap();
    // Duas amostras (pausas), cada uma com o `pc` de todas as threads.
    assert_eq!((before.get(), after.get()), (2, 2));
    assert_eq!(pcs.len(), 2 * threads);
    assert!(wait_all_running(t.pid), "{:?}", states(t.pid));
}

#[test]
fn sample_pcs_sees_spin_loop() {
    let t = TestTarget::spawn_with(TargetOpts {
        spin: true,
        ..Default::default()
    });
    let (pidfd, _id) = open_checked(t.pid).unwrap();
    let pcs = sample_pcs(
        &pidfd,
        t.pid,
        &PauseConfig::default(),
        50,
        Duration::from_millis(2),
        |_| Ok(()),
        || Ok(()),
        || false,
    )
    .unwrap();
    let hits = pcs
        .iter()
        .filter(|pc| (t.spin_page..t.spin_page + 5).contains(pc))
        .count();
    assert!(hits >= 1, "nenhum pc em spin_page: {pcs:x?}");
}

// ---------------------------------------------------------------------------
// Motor (Tarefa 11)
// ---------------------------------------------------------------------------

/// Diário num diretório temporário, com a trava segura pelo teste.
struct Env {
    dir: tempfile::TempDir,
    store: JournalStore,
    lock: JournalLock,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let store = JournalStore::new(dir.path().join("state"));
    let lock = store.lock().unwrap();
    Env { dir, store, lock }
}

fn open_engine(
    pid: i32,
    env: &Env,
    ranges: Vec<Range<u64>>,
    hook: Option<fn(HookPoint)>,
) -> Engine {
    open_engine_with(
        pid,
        env,
        ranges,
        PauseConfig {
            hook,
            ..PauseConfig::default()
        },
    )
}

/// Como `open_engine`, mas com o `PauseConfig` completo (falhas injetadas, contador).
fn open_engine_with(pid: i32, env: &Env, ranges: Vec<Range<u64>>, cfg: PauseConfig) -> Engine {
    let policy: PolicyFn = Box::new(move |_maps| WritePolicy::new(ranges.clone()));
    let opts = EngineOptions {
        version_key: "teste".into(),
        pause: cfg,
    };
    Engine::open(pid, None, env.store.clone(), &env.lock, policy, opts).unwrap()
}

fn patch(id: &str, addr: u64, expected: &[u8], replacement: &[u8]) -> Validated {
    validate(PatchSpec {
        id: id.into(),
        addr,
        expected: expected.to_vec(),
        replacement: replacement.to_vec(),
        kind: gt_core::patch::PatchKind::Code,
    })
    .unwrap()
}

fn bytes_at(pid: i32, addr: u64, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    MemReader::new(pid).read_exact(addr, &mut buf).unwrap();
    buf
}

fn exec2_range(t: &TestTarget) -> Range<u64> {
    t.exec2..t.exec2 + 2 * t.page
}

fn page_range(t: &TestTarget, start: u64) -> Range<u64> {
    start..start + t.page
}

fn missing(env: &Env) -> bool {
    env.store.peek().unwrap() == Loaded::Missing
}

const NOP3: [u8; 3] = [0x90, 0x90, 0x90];
const NOP2_NOP: [u8; 3] = [0x66, 0x90, 0x90];

fn open_expecting(t: &TestTarget, env: &Env, expected: &Identity) -> gt_core::Result<Engine> {
    let range = exec2_range(t);
    let policy: PolicyFn = Box::new(move |_maps| WritePolicy::new(vec![range.clone()]));
    let opts = EngineOptions {
        version_key: "teste".into(),
        pause: PauseConfig::default(),
    };
    Engine::open(
        t.pid,
        Some(expected),
        env.store.clone(),
        &env.lock,
        policy,
        opts,
    )
}

#[test]
fn open_refuses_other_identity() {
    let t = TestTarget::spawn();
    let env = env();
    let real = identity(t.pid).unwrap();
    for wrong in [
        Identity {
            start_time: real.start_time + 1,
            ..real.clone()
        },
        Identity {
            boot_id: "outro-boot".into(),
            ..real.clone()
        },
    ] {
        let r = open_expecting(&t, &env, &wrong);
        assert!(matches!(r, Err(Error::ProcessGone { .. })), "{wrong:?}");
    }
    assert!(missing(&env));
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
    // Com a identidade certa, abre e escreve normalmente.
    let mut engine = open_expecting(&t, &env, &real).unwrap();
    engine
        .apply(vec![patch("p", t.exec2, &NOP3, &NOP2_NOP)])
        .unwrap();
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP2_NOP);
}

#[test]
fn apply_and_undo_on_exec_page() {
    let t = TestTarget::spawn();
    let env = env();
    let mut engine = open_engine(t.pid, &env, vec![exec2_range(&t)], None);
    let report = engine
        .apply(vec![patch("p", t.exec2, &NOP3, &NOP2_NOP)])
        .unwrap();
    assert_eq!(report.restored, vec!["p".to_string()]);
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP2_NOP);
    assert_eq!(engine.active().len(), 1);
    match env.store.peek().unwrap() {
        Loaded::Ok(j) => {
            assert!(!j.paused_by_us);
            assert_eq!(j.pid, t.pid);
            assert_eq!(j.version_key, "teste");
            assert_eq!(
                j.entries,
                vec![JournalEntry {
                    id: "p".into(),
                    addr: t.exec2,
                    original: NOP3.to_vec(),
                    replacement: NOP2_NOP.to_vec(),
                    next: None,
                    kind: gt_core::patch::PatchKind::Code,
                }]
            );
        }
        other => panic!("diário inesperado: {other:?}"),
    }
    let report = engine.undo_all().unwrap();
    assert_eq!(report.restored, vec!["p".to_string()]);
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
    assert!(engine.active().is_empty());
    assert!(missing(&env));
    assert!(wait_all_running(t.pid), "{:?}", states(t.pid));
}

/// Cave de 16 bytes sobre o preenchimento `0xCC` da página executável do alvo.
fn cave(id: &str, addr: u64) -> Validated {
    let mut code = vec![0x48, 0xFF, 0xC0, 0xC3]; // inc rax; ret
    code.resize(16, 0x90);
    validate(PatchSpec {
        id: id.into(),
        addr,
        expected: vec![0xCC; 16],
        replacement: code,
        kind: gt_core::patch::PatchKind::Cave,
    })
    .unwrap()
}

#[test]
fn undo_all_undoes_code_then_cave() {
    let t = TestTarget::spawn();
    let env = env();
    let cave_addr = t.exec_page + 0x100;
    let mut engine = open_engine(
        t.pid,
        &env,
        vec![exec2_range(&t), page_range(&t, t.exec_page)],
        None,
    );
    let batch = vec![patch("p", t.exec2, &NOP3, &NOP2_NOP), cave("c", cave_addr)];
    engine.apply(batch).unwrap();
    assert_eq!(bytes_at(t.pid, cave_addr, 4), [0x48, 0xFF, 0xC0, 0xC3]);
    match env.store.peek().unwrap() {
        Loaded::Ok(j) => {
            let kinds: Vec<_> = j.entries.iter().map(|e| (e.id.as_str(), e.kind)).collect();
            assert!(
                kinds.contains(&("c", gt_core::patch::PatchKind::Cave)),
                "{kinds:?}"
            );
        }
        other => panic!("diário inesperado: {other:?}"),
    }
    let report = engine.undo_all().unwrap();
    let mut restored = report.restored.clone();
    restored.sort();
    assert_eq!(restored, vec!["c".to_string(), "p".to_string()]);
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
    assert_eq!(bytes_at(t.pid, cave_addr, 16), vec![0xCC; 16]);
    assert!(engine.active().is_empty());
    assert!(engine.pending_ids().is_empty());
    assert!(missing(&env));
    assert!(wait_all_running(t.pid), "{:?}", states(t.pid));
}

#[test]
fn restore_keeps_cave_while_code_unresolved() {
    let t = TestTarget::spawn();
    let env = env();
    let cave_addr = t.exec_page + 0x100;
    // A política só cobre a página da cave: o desvio (em `exec2`) fica sem resolver.
    let mut engine = open_engine(t.pid, &env, vec![page_range(&t, t.exec_page)], None);
    engine.apply(vec![cave("c", cave_addr)]).unwrap();
    let code = JournalEntry {
        id: "p".into(),
        addr: t.exec2,
        original: NOP3.to_vec(),
        replacement: NOP2_NOP.to_vec(),
        next: None,
        kind: gt_core::patch::PatchKind::Code,
    };
    let cave_entry = JournalEntry {
        id: "c".into(),
        addr: cave_addr,
        original: vec![0xCC; 16],
        replacement: bytes_at(t.pid, cave_addr, 16),
        next: None,
        kind: gt_core::patch::PatchKind::Cave,
    };
    let report = engine
        .restore_entries(vec![code, cave_entry.clone()])
        .unwrap();
    assert_eq!(report.outside_policy, vec!["p".to_string()]);
    assert_eq!(report.deferred, vec!["c".to_string()]);
    assert!(report.restored.is_empty(), "{report:?}");
    // A cave continua escrita e as duas entradas continuam no diário.
    assert_eq!(bytes_at(t.pid, cave_addr, 4), [0x48, 0xFF, 0xC0, 0xC3]);
    match env.store.peek().unwrap() {
        Loaded::Ok(j) => {
            let ids: Vec<_> = j.entries.iter().map(|e| e.id.as_str()).collect();
            assert!(ids.contains(&"p") && ids.contains(&"c"), "{ids:?}");
        }
        other => panic!("diário inesperado: {other:?}"),
    }
    // O desvio continua pendente no registro: a cave segue adiada.
    let report = engine.restore_entries(vec![cave_entry]).unwrap();
    assert_eq!(report.deferred, vec!["c".to_string()], "{report:?}");
    assert_eq!(bytes_at(t.pid, cave_addr, 4), [0x48, 0xFF, 0xC0, 0xC3]);
}

#[test]
fn mismatch_refuses_without_pausing() {
    let t = TestTarget::spawn();
    let env = env();
    let mut engine = open_engine(t.pid, &env, vec![exec2_range(&t)], None);
    let err = engine
        .apply(vec![patch("p", t.exec2, &[0x66, 0x90], &[0x90, 0x90])])
        .unwrap_err();
    match err {
        Error::BytesMismatch {
            id,
            expected,
            found,
        } => {
            assert_eq!(id, "p");
            assert_eq!(expected, "66 90");
            assert_eq!(found, "90 90");
        }
        other => panic!("esperava BytesMismatch, veio {other:?}"),
    }
    assert!(!states(t.pid).contains(&'T'), "{:?}", states(t.pid));
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
    assert!(missing(&env));
}

#[test]
fn outside_policy_refuses() {
    let t = TestTarget::spawn();
    let env = env();
    let mut engine = open_engine(t.pid, &env, vec![page_range(&t, t.exec2)], None);
    // Começa na última página de `exec2`, que fica fora da política.
    let addr = t.exec2 + t.page;
    let err = engine
        .apply(vec![patch("fora", addr, &NOP3, &NOP2_NOP)])
        .unwrap_err();
    assert!(
        matches!(&err, Error::OutsidePolicy { id, addr: a } if id == "fora" && *a == addr),
        "{err:?}"
    );
    assert_eq!(bytes_at(t.pid, addr, 3), NOP3);
    assert!(missing(&env));
}

#[test]
fn overlap_refuses() {
    let t = TestTarget::spawn();
    let env = env();
    let mut engine = open_engine(t.pid, &env, vec![exec2_range(&t)], None);
    engine
        .apply(vec![patch("a", t.exec2, &NOP3, &NOP2_NOP)])
        .unwrap();
    let err = engine
        .apply(vec![patch("b", t.exec2 + 2, &NOP3, &NOP2_NOP)])
        .unwrap_err();
    assert!(
        matches!(&err, Error::Overlap { id } if id == "b"),
        "{err:?}"
    );
    // Dentro do mesmo lote também.
    let err = engine
        .apply(vec![
            patch("c", t.exec2 + 16, &NOP3, &NOP2_NOP),
            patch("d", t.exec2 + 17, &NOP3, &NOP2_NOP),
        ])
        .unwrap_err();
    assert!(
        matches!(&err, Error::Overlap { id } if id == "d"),
        "{err:?}"
    );
    assert_eq!(bytes_at(t.pid, t.exec2 + 16, 4), [0x90; 4]);
    assert_eq!(engine.active().len(), 1);
    engine.undo_all().unwrap();
    assert!(missing(&env));
}

#[test]
fn drop_restores() {
    let t = TestTarget::spawn();
    let env = env();
    let mut engine = open_engine(t.pid, &env, vec![exec2_range(&t)], None);
    engine
        .apply(vec![patch("p", t.exec2, &NOP3, &NOP2_NOP)])
        .unwrap();
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP2_NOP);
    drop(engine);
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
    assert!(missing(&env));
    assert!(wait_all_running(t.pid), "{:?}", states(t.pid));
}

static DENY_POLICY: AtomicBool = AtomicBool::new(false);

#[test]
fn drop_keeps_journal_when_policy_refuses_undo() {
    let t = TestTarget::spawn();
    let env = env();
    let range = exec2_range(&t);
    let policy: PolicyFn = Box::new(move |_maps| {
        if DENY_POLICY.load(Ordering::SeqCst) {
            WritePolicy::new(Vec::new())
        } else {
            WritePolicy::new(vec![range.clone()])
        }
    });
    let opts = EngineOptions {
        version_key: "teste".into(),
        pause: PauseConfig::default(),
    };
    let mut engine = Engine::open(t.pid, None, env.store.clone(), &env.lock, policy, opts).unwrap();
    engine
        .apply(vec![patch("p", t.exec2, &NOP3, &NOP2_NOP)])
        .unwrap();
    DENY_POLICY.store(true, Ordering::SeqCst);
    drop(engine);
    // Nada foi escrito e a entrada continua no diário para a próxima execução.
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP2_NOP);
    match env.store.peek().unwrap() {
        Loaded::Ok(j) => {
            assert!(!j.paused_by_us);
            assert_eq!(j.entries.len(), 1);
            assert_eq!(j.entries[0].id, "p");
            assert_eq!(j.entries[0].replacement, NOP2_NOP.to_vec());
        }
        other => panic!("diário inesperado: {other:?}"),
    }
    assert!(wait_all_running(t.pid), "{:?}", states(t.pid));
}

#[test]
fn dead_target_discards_without_writing() {
    let mut t = TestTarget::spawn();
    let env = env();
    let mut engine = open_engine(t.pid, &env, vec![exec2_range(&t)], None);
    engine
        .apply(vec![patch("p", t.exec2, &NOP3, &NOP2_NOP)])
        .unwrap();
    t.child.kill().unwrap();
    t.child.wait().unwrap();
    let err = engine.undo_all().unwrap_err();
    assert!(matches!(err, Error::ProcessGone { .. }), "{err:?}");
    assert!(engine.active().is_empty());
    assert!(missing(&env));
}

#[test]
fn zombie_target_is_closed() {
    let mut t = TestTarget::spawn();
    let env = env();
    let mut engine = open_engine(t.pid, &env, vec![exec2_range(&t)], None);
    engine
        .apply(vec![patch("p", t.exec2, &NOP3, &NOP2_NOP)])
        .unwrap();
    t.child.kill().unwrap();
    let pid = t.pid;
    assert!(
        wait_until(
            || Ok(read_stat(pid)?.state == 'Z'),
            Duration::from_millis(500),
            Duration::from_millis(1),
        )
        .unwrap()
    );
    let err = engine.undo_all().unwrap_err();
    assert!(matches!(err, Error::ProcessGone { .. }), "{err:?}");
    assert!(engine.active().is_empty());
    assert!(missing(&env));
}

static KILL_PID: AtomicI32 = AtomicI32::new(0);

/// Mata o alvo dentro da pausa e espera ele virar zumbi.
fn kill_in_pause(point: HookPoint) {
    if point != HookPoint::InPause {
        return;
    }
    let pid = KILL_PID.load(Ordering::SeqCst);
    kill(Pid::from_raw(pid), Signal::SIGKILL).unwrap();
    let dead = wait_until(
        || Ok(read_stat(pid).map(|s| s.state == 'Z').unwrap_or(true)),
        Duration::from_secs(2),
        Duration::from_millis(1),
    )
    .unwrap();
    assert!(dead);
}

#[test]
fn apply_when_target_dies_during_pause() {
    let t = TestTarget::spawn();
    let env = env();
    KILL_PID.store(t.pid, Ordering::SeqCst);
    let mut engine = open_engine(t.pid, &env, vec![exec2_range(&t)], Some(kill_in_pause));
    let err = engine
        .apply(vec![patch("p", t.exec2, &NOP3, &NOP2_NOP)])
        .unwrap_err();
    assert!(matches!(err, Error::ProcessGone { .. }), "{err:?}");
    assert!(engine.active().is_empty());
    assert!(missing(&env));
}

#[test]
fn apply_refuses_when_journal_unwritable() {
    let t = TestTarget::spawn();
    let env = env();
    let dir = env.dir.path().join("state");
    let mut engine = open_engine(t.pid, &env, vec![exec2_range(&t)], None);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = engine.apply(vec![patch("p", t.exec2, &NOP3, &NOP2_NOP)]);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let err = result.unwrap_err();
    match &err {
        Error::Journal(msg) => assert!(msg.contains("journal.json"), "{msg}"),
        other => panic!("esperava Journal, veio {other:?}"),
    }
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
    assert!(!states(t.pid).contains(&'T'), "{:?}", states(t.pid));
    assert!(engine.active().is_empty());
    assert!(missing(&env));
}

#[test]
fn patch_across_page_boundary() {
    let t = TestTarget::spawn();
    let env = env();
    let mut engine = open_engine(t.pid, &env, vec![exec2_range(&t)], None);
    let addr = t.exec2 + t.page - 2;
    let new = [0x66, 0x90, 0x66, 0x90];
    engine
        .apply(vec![patch("borda", addr, &[0x90; 4], &new)])
        .unwrap();
    assert_eq!(bytes_at(t.pid, addr, 4), new);
    engine.undo_all().unwrap();
    assert_eq!(bytes_at(t.pid, addr, 4), [0x90; 4]);
    assert!(missing(&env));
}

#[test]
fn already_stopped_target_gets_written_and_stays_stopped() {
    let t = TestTarget::spawn();
    let env = env();
    kill(Pid::from_raw(t.pid), Signal::SIGSTOP).unwrap();
    assert!(
        wait_until(
            || is_stopped_now(t.pid),
            Duration::from_secs(2),
            Duration::from_millis(1)
        )
        .unwrap()
    );
    let mut engine = open_engine(t.pid, &env, vec![exec2_range(&t)], None);
    engine
        .apply(vec![patch("p", t.exec2, &NOP3, &NOP2_NOP)])
        .unwrap();
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP2_NOP);
    match env.store.peek().unwrap() {
        Loaded::Ok(j) => assert!(!j.paused_by_us),
        other => panic!("diário inesperado: {other:?}"),
    }
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        states(t.pid).iter().all(|s| *s == 'T'),
        "{:?}",
        states(t.pid)
    );
    engine.undo_all().unwrap();
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
    assert!(
        states(t.pid).iter().all(|s| *s == 'T'),
        "{:?}",
        states(t.pid)
    );
    kill(Pid::from_raw(t.pid), Signal::SIGCONT).unwrap();
}

/// Patch com fronteira perdida no `syscall_page` deve ser recusado depois das 10 tentativas.
fn assert_threads_in_patch(addr_offset: u64, expected: &[u8], replacement: &[u8]) {
    let t = TestTarget::spawn();
    let env = env();
    let mut engine = open_engine(t.pid, &env, vec![page_range(&t, t.syscall_page)], None);
    let addr = t.syscall_page + addr_offset;
    let err = engine
        .apply(vec![patch("sys", addr, expected, replacement)])
        .unwrap_err();
    assert!(
        matches!(&err, Error::ThreadsInPatch { id, attempts: 10 } if id == "sys"),
        "{err:?}"
    );
    assert_eq!(bytes_at(t.pid, addr, expected.len()), expected);
    assert!(wait_all_running(t.pid), "{:?}", states(t.pid));
    assert!(engine.active().is_empty());
    assert!(missing(&env));
}

#[test]
fn lost_boundary_at_syscall_pc_is_refused() {
    assert_threads_in_patch(
        20,
        &[0x0F, 0x05, 0x90, 0x90, 0x90, 0x90],
        &[0x66, 0x66, 0x66, 0x66, 0x66, 0x90],
    );
}

#[test]
fn lost_boundary_at_syscall_pc_minus_2_is_refused() {
    assert_threads_in_patch(
        15,
        &[0xBA, 0x01, 0x00, 0x00, 0x00, 0x0F, 0x05],
        &[0x0F, 0x1F, 0x80, 0x00, 0x00, 0x00, 0x00],
    );
}

#[test]
fn patch_keeping_syscall_boundaries_is_accepted() {
    let t = TestTarget::spawn();
    let env = env();
    let mut engine = open_engine(t.pid, &env, vec![page_range(&t, t.syscall_page)], None);
    let addr = t.syscall_page + 22;
    let new = [0x66, 0x90, 0x66, 0x90];
    let report = engine
        .apply(vec![patch("sys", addr, &[0x90; 4], &new)])
        .unwrap();
    assert_eq!(report.danger_retries, 0);
    assert_eq!(bytes_at(t.pid, addr, 4), new);
    engine.undo_all().unwrap();
    assert_eq!(bytes_at(t.pid, addr, 4), [0x90; 4]);
    assert!(missing(&env));
    assert!(wait_all_running(t.pid), "{:?}", states(t.pid));
}

#[test]
fn failed_write_mid_batch_rolls_back() {
    let t = TestTarget::spawn();
    let env = env();
    let mut engine = open_engine(
        t.pid,
        &env,
        vec![exec2_range(&t), page_range(&t, t.ro_shared)],
        None,
    );
    let err = engine
        .apply(vec![
            patch("a", t.exec2, &NOP3, &NOP2_NOP),
            patch("ro", t.ro_shared, &NOP3, &NOP2_NOP),
        ])
        .unwrap_err();
    assert!(
        matches!(err, Error::WriteRefused { addr } if addr == t.ro_shared),
        "{err:?}"
    );
    assert!(err.to_string().contains("proc_mem.force_override"), "{err}");
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
    assert_eq!(bytes_at(t.pid, t.ro_shared, 3), NOP3);
    assert!(engine.active().is_empty());
    assert!(missing(&env));
    assert!(wait_all_running(t.pid), "{:?}", states(t.pid));
}

#[test]
fn undo_foreign_bytes_does_not_write() {
    let t = TestTarget::spawn();
    let env = env();
    let mut engine = open_engine(t.pid, &env, vec![exec2_range(&t)], None);
    engine
        .apply(vec![patch("p", t.exec2, &NOP3, &NOP2_NOP)])
        .unwrap();
    let other = [0xCC, 0xCC, 0xCC];
    MemWriter::open(t.pid)
        .unwrap()
        .write(t.exec2, &other)
        .unwrap();
    let report = engine.undo_all().unwrap();
    assert_eq!(report.foreign, vec!["p".to_string()]);
    assert!(report.restored.is_empty());
    assert_eq!(bytes_at(t.pid, t.exec2, 3), other);
    assert!(engine.active().is_empty());
    assert!(missing(&env));
}

static CHANGE_PID: AtomicI32 = AtomicI32::new(0);
static CHANGE_ADDR: AtomicU64 = AtomicU64::new(0);

/// Escreve outros bytes no local do patch dentro da pausa.
fn change_in_pause(point: HookPoint) {
    if point != HookPoint::InPause {
        return;
    }
    let pid = CHANGE_PID.load(Ordering::SeqCst);
    let addr = CHANGE_ADDR.load(Ordering::SeqCst);
    MemWriter::open(pid)
        .unwrap()
        .write(addr, &[0xCC, 0xCC, 0xCC])
        .unwrap();
}

#[test]
fn bytes_changed_during_pause_refuses() {
    let t = TestTarget::spawn();
    let env = env();
    CHANGE_PID.store(t.pid, Ordering::SeqCst);
    CHANGE_ADDR.store(t.exec2, Ordering::SeqCst);
    let mut engine = open_engine(t.pid, &env, vec![exec2_range(&t)], Some(change_in_pause));
    let err = engine
        .apply(vec![patch("p", t.exec2, &NOP3, &NOP2_NOP)])
        .unwrap_err();
    assert!(
        matches!(&err, Error::BytesMismatch { id, found, .. } if id == "p" && found == "CC CC CC"),
        "{err:?}"
    );
    assert_eq!(bytes_at(t.pid, t.exec2, 3), [0xCC, 0xCC, 0xCC]);
    assert!(engine.active().is_empty());
    assert!(missing(&env));
    assert!(wait_all_running(t.pid), "{:?}", states(t.pid));
}

/// Entra em pânico dentro da pausa.
fn panic_in_pause(point: HookPoint) {
    if point == HookPoint::InPause {
        panic!("pânico de teste dentro da pausa");
    }
}

#[test]
fn panic_in_pause_hook_resumes_and_leaves_bytes() {
    let t = TestTarget::spawn();
    let env = env();
    let mut engine = open_engine(t.pid, &env, vec![exec2_range(&t)], Some(panic_in_pause));
    let result = catch_unwind(AssertUnwindSafe(|| {
        engine.apply(vec![patch("p", t.exec2, &NOP3, &NOP2_NOP)])
    }));
    assert!(result.is_err());
    assert!(wait_all_running(t.pid), "{:?}", states(t.pid));
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
    // O `Drop` resolve a intenção que ficou no diário (sem disparar o gancho de novo).
    drop(engine);
    assert!(missing(&env));
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
    assert!(wait_all_running(t.pid), "{:?}", states(t.pid));
}

#[test]
fn restore_entries_handles_mixed_bytes() {
    let t = TestTarget::spawn();
    let env = env();
    let mut engine = open_engine(t.pid, &env, vec![exec2_range(&t)], None);
    let new = [0x66, 0x90, 0x66, 0x90];
    MemWriter::open(t.pid)
        .unwrap()
        .write(t.exec2, &new[..2])
        .unwrap();
    assert_eq!(bytes_at(t.pid, t.exec2, 4), [0x66, 0x90, 0x90, 0x90]);
    let entry = JournalEntry {
        id: "meio".into(),
        addr: t.exec2,
        original: vec![0x90; 4],
        replacement: new.to_vec(),
        next: None,
        kind: gt_core::patch::PatchKind::Code,
    };
    let report = engine.restore_entries(vec![entry]).unwrap();
    assert_eq!(report.restored, vec!["meio".to_string()]);
    assert!(report.dropped.is_empty() && report.foreign.is_empty());
    assert_eq!(bytes_at(t.pid, t.exec2, 4), [0x90; 4]);
    assert!(missing(&env));
}

#[test]
fn stress_1000_toggles_on_spinning_loop() {
    let t = TestTarget::spawn_with(TargetOpts {
        spin: true,
        ..Default::default()
    });
    let env = env();
    let mut engine = open_engine(t.pid, &env, vec![page_range(&t, t.spin_page)], None);
    let p = patch(
        "spin",
        t.spin_page,
        &[0x48, 0x8B, 0xF9],
        &[0x48, 0x89, 0xCF],
    );
    for _ in 0..1000 {
        engine.apply(vec![p.clone()]).unwrap();
        engine.undo_all().unwrap();
    }
    assert_eq!(bytes_at(t.pid, t.spin_page, 3), [0x48, 0x8B, 0xF9]);
    assert!(missing(&env));
    assert_ne!(read_stat(t.pid).unwrap().state, 'Z');
    let pcs = sample_pcs(
        engine.pidfd(),
        t.pid,
        &PauseConfig::default(),
        20,
        Duration::from_millis(2),
        |_| Ok(()),
        || Ok(()),
        || false,
    )
    .unwrap();
    assert!(
        pcs.iter()
            .any(|pc| (t.spin_page..t.spin_page + 5).contains(pc)),
        "nenhum pc em spin_page: {pcs:x?}"
    );
}

#[test]
fn toggle_cycles_and_ends_off() {
    use gt_core::session::{Shutdown, toggle};

    let t = TestTarget::spawn();
    let env = env();
    let mut engine = open_engine(t.pid, &env, vec![exec2_range(&t)], None);
    let p = patch("t", t.exec2, &NOP3, &NOP2_NOP);
    let shutdown = Shutdown::manual();
    // O aviso roda uma vez só, logo depois do primeiro `apply` (bytes já trocados).
    let seen = Cell::new(Vec::new());
    let report = toggle(
        &mut engine,
        p.clone(),
        20,
        Duration::ZERO,
        &shutdown,
        || seen.set(vec![bytes_at(t.pid, t.exec2, 3)]),
    )
    .unwrap();
    assert_eq!(seen.take(), vec![NOP2_NOP.to_vec()]);
    assert_eq!(report.cycles, 20);
    assert!(report.pause_max >= report.pause_avg, "{report:?}");
    assert!(report.pause_max > Duration::ZERO, "{report:?}");
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
    assert!(engine.active().is_empty());
    assert!(missing(&env));

    // `apply` recusado (bytes esperados errados): nada aplicado, nenhum aviso.
    let wrong = patch("t", t.exec2, &NOP2_NOP, &NOP3);
    let called = Cell::new(false);
    assert!(
        toggle(&mut engine, wrong, 20, Duration::ZERO, &shutdown, || called
            .set(true))
        .is_err()
    );
    assert!(!called.get());

    shutdown.request();
    let report = toggle(&mut engine, p, 20, Duration::ZERO, &shutdown, || {
        called.set(true)
    })
    .unwrap();
    assert!(!called.get());
    assert_eq!(report.cycles, 0);
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
    assert!(missing(&env));
}

// ---------------------------------------------------------------------------
// SIGCONT que falha (plano 003, tarefa 1)
// ---------------------------------------------------------------------------

/// Diário atual (falha se não existir ou estiver ilegível).
fn journal_now(env: &Env) -> gt_core::journal::Journal {
    match env.store.peek().unwrap() {
        Loaded::Ok(j) => j,
        other => panic!("diário inesperado: {other:?}"),
    }
}

fn fault_cfg(fault: Fault) -> PauseConfig {
    PauseConfig {
        fault: Some(fault),
        ..PauseConfig::default()
    }
}

#[test]
fn cont_fails_once_then_next_op_resumes() {
    let t = TestTarget::spawn();
    let env = env();
    let mut engine = open_engine_with(
        t.pid,
        &env,
        vec![exec2_range(&t)],
        fault_cfg(Fault::ContFailsOnce),
    );
    assert!(!engine.is_stuck());
    let err = engine
        .apply(vec![patch("p", t.exec2, &NOP3, &NOP2_NOP)])
        .unwrap_err();
    assert!(matches!(err, Error::Io(_)), "{err:?}");
    assert!(engine.is_stuck());
    assert!(is_stopped_now(t.pid).unwrap());
    let j = journal_now(&env);
    assert!(j.paused_by_us);
    // A escrita aconteceu: a entrada fica pendente no diário.
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP2_NOP);
    assert_eq!(j.entries.len(), 1);
    assert_eq!(j.entries[0].id, "p");

    // A operação seguinte tenta o SIGCONT antes de tudo e depois restaura.
    let report = engine.restore_entries(j.entries).unwrap();
    assert_eq!(report.restored, vec!["p".to_string()]);
    assert!(!engine.is_stuck());
    assert!(wait_all_running(t.pid), "{:?}", states(t.pid));
    assert!(!is_stopped_now(t.pid).unwrap());
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
    assert!(missing(&env));
}

#[test]
fn cont_fails_always_keeps_marker_after_undo_and_drop() {
    let t = TestTarget::spawn();
    let env = env();
    let mut engine = open_engine_with(
        t.pid,
        &env,
        vec![exec2_range(&t)],
        fault_cfg(Fault::ContFailsAlways),
    );
    engine
        .apply(vec![patch("p", t.exec2, &NOP3, &NOP2_NOP)])
        .unwrap_err();
    assert!(engine.is_stuck());
    assert!(journal_now(&env).paused_by_us);

    // O desfazer é recusado com o erro do SIGCONT, sem escrever nada.
    let err = engine.undo_all().unwrap_err();
    assert!(matches!(err, Error::Io(_)), "{err:?}");
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP2_NOP);
    assert!(journal_now(&env).paused_by_us);

    drop(engine);
    assert!(journal_now(&env).paused_by_us);
    assert!(is_stopped_now(t.pid).unwrap());

    let report = recover(&env.store, &env.lock, &PauseConfig::default()).unwrap();
    assert!(report.resumed, "{report:?}");
    assert!(wait_all_running(t.pid), "{:?}", states(t.pid));
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
    assert!(missing(&env));
}

#[test]
fn drop_with_stuck_and_nothing_active_keeps_marker() {
    let t = TestTarget::spawn();
    let env = env();
    let counter = Arc::new(AtomicU32::new(0));
    let mut engine = open_engine_with(
        t.pid,
        &env,
        vec![exec2_range(&t)],
        PauseConfig {
            fault: Some(Fault::ContFailsAlways),
            resume_counter: Some(counter.clone()),
            ..PauseConfig::default()
        },
    );
    engine.force_stuck_for_test();
    assert!(engine.is_stuck());
    assert!(journal_now(&env).paused_by_us);
    drop(engine);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    let j = journal_now(&env);
    assert!(j.paused_by_us);
    assert!(j.entries.is_empty());
}

#[test]
fn retry_resume_without_stuck_sends_nothing() {
    let t = TestTarget::spawn();
    let env = env();
    kill(Pid::from_raw(t.pid), Signal::SIGSTOP).unwrap();
    assert!(
        wait_until(
            || is_stopped_now(t.pid),
            Duration::from_secs(2),
            Duration::from_millis(1)
        )
        .unwrap()
    );
    let mut engine = open_engine(t.pid, &env, vec![exec2_range(&t)], None);
    engine.retry_resume().unwrap();
    std::thread::sleep(Duration::from_millis(50));
    assert!(is_stopped_now(t.pid).unwrap());
    drop(engine);
    kill(Pid::from_raw(t.pid), Signal::SIGCONT).unwrap();
}

#[test]
fn stuck_with_dead_target_clears_registry_and_journal() {
    let mut t = TestTarget::spawn();
    let env = env();
    let mut engine = open_engine(t.pid, &env, vec![exec2_range(&t)], None);
    engine.force_stuck_for_test();
    assert!(journal_now(&env).paused_by_us);
    t.child.kill().unwrap();
    t.child.wait().unwrap();
    let err = engine.retry_resume().unwrap_err();
    assert!(matches!(err, Error::ProcessGone { .. }), "{err:?}");
    assert!(!engine.is_stuck());
    assert!(missing(&env));
}

#[test]
fn drop_with_stuck_and_dead_target_clears_journal() {
    let mut t = TestTarget::spawn();
    let env = env();
    let mut engine = open_engine(t.pid, &env, vec![exec2_range(&t)], None);
    engine.force_stuck_for_test();
    t.child.kill().unwrap();
    t.child.wait().unwrap();
    drop(engine);
    assert!(missing(&env));
}

// ---------------------------------------------------------------------------
// Restos de uma falha e descarte quando o jogo fecha (plano 003, tarefa 2)
// ---------------------------------------------------------------------------

#[test]
fn dirty_failure_leaves_pending_and_blocks_range() {
    let t = TestTarget::spawn();
    let env = env();
    let mut engine = open_engine_with(
        t.pid,
        &env,
        vec![exec2_range(&t)],
        fault_cfg(Fault::WriteAndRollbackFail),
    );
    let err = engine
        .apply(vec![patch("A", t.exec2, &NOP3, &NOP2_NOP)])
        .unwrap_err();
    assert!(matches!(err, Error::Io(_)), "{err:?}");
    assert!(!engine.is_stuck());
    assert_eq!(engine.pending_ids(), vec!["A".to_string()]);
    assert!(engine.active().is_empty());
    // Os bytes novos ficaram no jogo.
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP2_NOP);
    let j = journal_now(&env);
    assert_eq!(j.entries.len(), 1);
    assert_eq!(j.entries[0].id, "A");
    assert!(!j.paused_by_us);

    // O trecho fica bloqueado para novos patches.
    let err = engine
        .apply(vec![patch("A", t.exec2, &NOP3, &NOP2_NOP)])
        .unwrap_err();
    assert!(
        matches!(&err, Error::Overlap { id } if id == "A"),
        "{err:?}"
    );

    // Um patch em outro endereço aplica normalmente (a falha só dispara uma vez).
    engine
        .apply(vec![patch("B", t.exec2 + 64, &NOP3, &NOP2_NOP)])
        .unwrap();
    assert_eq!(engine.active().len(), 1);
    assert_eq!(engine.pending_ids(), vec!["A".to_string()]);
    assert_eq!(bytes_at(t.pid, t.exec2 + 64, 3), NOP2_NOP);
    let ids: Vec<String> = journal_now(&env)
        .entries
        .into_iter()
        .map(|e| e.id)
        .collect();
    assert!(ids.contains(&"A".to_string()), "{ids:?}");
}

#[test]
fn restore_pending_returns_originals() {
    let t = TestTarget::spawn();
    let env = env();
    let mut engine = open_engine_with(
        t.pid,
        &env,
        vec![exec2_range(&t)],
        fault_cfg(Fault::WriteAndRollbackFail),
    );
    engine
        .apply(vec![patch("A", t.exec2, &NOP3, &NOP2_NOP)])
        .unwrap_err();
    assert_eq!(engine.pending_ids(), vec!["A".to_string()]);

    let report = engine.restore_pending().unwrap();
    assert_eq!(report.restored, vec!["A".to_string()]);
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
    assert!(engine.pending_ids().is_empty());
    assert!(missing(&env));

    // Sem restos, não faz nada.
    let report = engine.restore_pending().unwrap();
    assert_eq!(report, gt_core::engine::RestoreReport::default());
}

#[test]
fn restore_pending_retries_stuck_resume_first() {
    let t = TestTarget::spawn();
    let env = env();
    let counter = Arc::new(AtomicU32::new(0));
    let mut engine = open_engine_with(
        t.pid,
        &env,
        vec![exec2_range(&t)],
        PauseConfig {
            fault: Some(Fault::ContFailsAlways),
            resume_counter: Some(counter.clone()),
            ..PauseConfig::default()
        },
    );
    engine.force_stuck_for_test();
    // Sem restos, ainda assim tenta o SIGCONT antes de qualquer retorno antecipado.
    let err = engine.restore_pending().unwrap_err();
    assert!(matches!(err, Error::Io(_)), "{err:?}");
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    assert!(engine.is_stuck());
}

#[test]
fn discard_gone_after_target_exit() {
    let mut t = TestTarget::spawn();
    let env = env();
    let mut engine = open_engine(t.pid, &env, vec![exec2_range(&t)], None);
    engine
        .apply(vec![patch("p", t.exec2, &NOP3, &NOP2_NOP)])
        .unwrap();
    assert!(!missing(&env));
    // Com o alvo vivo, não faz nada.
    engine.discard_gone();
    assert_eq!(engine.active().len(), 1);
    assert!(!missing(&env));

    t.child.kill().unwrap();
    t.child.wait().unwrap();
    engine.discard_gone();
    assert!(engine.active().is_empty());
    assert!(engine.pending_ids().is_empty());
    assert!(!engine.is_stuck());
    assert!(missing(&env));
}

#[test]
fn discard_gone_clears_stuck_and_pending() {
    let mut t = TestTarget::spawn();
    let env = env();
    let mut engine = open_engine_with(
        t.pid,
        &env,
        vec![exec2_range(&t)],
        fault_cfg(Fault::WriteAndRollbackFail),
    );
    engine
        .apply(vec![patch("A", t.exec2, &NOP3, &NOP2_NOP)])
        .unwrap_err();
    engine.force_stuck_for_test();
    assert_eq!(engine.pending_ids().len(), 1);
    t.child.kill().unwrap();
    t.child.wait().unwrap();
    engine.discard_gone();
    assert!(engine.pending_ids().is_empty());
    assert!(!engine.is_stuck());
    assert!(missing(&env));
}

// ---------------------------------------------------------------------------
// Troca dos bytes de um patch ativo (plano 003, tarefa 3)
// ---------------------------------------------------------------------------

/// `mov eax, imm32` com o imediato do laço de valor do alvo.
const VALUE_11: [u8; 5] = [0xB8, 0x11, 0x11, 0x11, 0x11];
const VALUE_22: [u8; 5] = [0xB8, 0x22, 0x22, 0x22, 0x22];
const VALUE_33: [u8; 5] = [0xB8, 0x33, 0x33, 0x33, 0x33];

/// Os testes com a thread do laço de valor se serializam: a pausa tem prazo de 500 ms por
/// tentativa e não pode disputar CPU com outros alvos girando.
static SPIN_VALUE_LOCK: Mutex<()> = Mutex::new(());

fn spin_value_lock() -> std::sync::MutexGuard<'static, ()> {
    SPIN_VALUE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn spin_value_target() -> TestTarget {
    TestTarget::spawn_with(TargetOpts {
        spin_value: true,
        ..Default::default()
    })
}

/// Patch de valor: `B8 11 11 11 11` → `B8 22 22 22 22` em `value_page`.
fn value_patch(t: &TestTarget) -> Validated {
    patch("val", t.value_page, &VALUE_11, &VALUE_22)
}

fn value_engine(t: &TestTarget, env: &Env, cfg: PauseConfig) -> Engine {
    open_engine_with(t.pid, env, vec![page_range(t, t.value_page)], cfg)
}

fn read_word(t: &TestTarget) -> u32 {
    u32::from_le_bytes(bytes_at(t.pid, t.value_word, 4).try_into().unwrap())
}

/// Espera (até 2 s) a palavra do laço de valor chegar a `want`.
fn wait_word(t: &TestTarget, want: u32) -> bool {
    wait_until(
        || Ok(read_word(t) == want),
        Duration::from_secs(2),
        Duration::from_millis(1),
    )
    .unwrap()
}

/// Diário num diretório de memória (`/dev/shm`, se existir): milhares de gravações com
/// `fsync` ficam rápidas.
fn env_fast() -> Env {
    let dir = if std::path::Path::new("/dev/shm").is_dir() {
        tempfile::tempdir_in("/dev/shm").unwrap()
    } else {
        tempfile::tempdir().unwrap()
    };
    let store = JournalStore::new(dir.path().join("state"));
    let lock = store.lock().unwrap();
    Env { dir, store, lock }
}

#[test]
fn set_replacement_changes_executed_value() {
    let _serial = spin_value_lock();
    let t = spin_value_target();
    let env = env();
    let mut engine = value_engine(&t, &env, PauseConfig::default());
    // O laço escreve o imediato inicial até o patch entrar.
    assert!(wait_word(&t, 0x1111_1111));
    engine.apply(vec![value_patch(&t)]).unwrap();
    assert!(wait_word(&t, 0x2222_2222), "{:#x}", read_word(&t));

    let report = engine.set_replacement("val", VALUE_33.to_vec()).unwrap();
    assert_eq!(report.restored, vec!["val".to_string()]);
    assert!(report.foreign.is_empty());
    assert_eq!(report.danger_retries, 0);
    assert!(wait_word(&t, 0x3333_3333), "{:#x}", read_word(&t));
    assert_eq!(bytes_at(t.pid, t.value_page, 5), VALUE_33);
    assert_eq!(engine.active().len(), 1);
    assert_eq!(engine.active()[0].replacement, VALUE_33);
    assert!(engine.pending_ids().is_empty());

    // Diário: uma única entrada, com o valor novo e sem `next`.
    let j = journal_now(&env);
    assert!(!j.paused_by_us);
    assert_eq!(
        j.entries,
        vec![JournalEntry {
            id: "val".into(),
            addr: t.value_page,
            original: VALUE_11.to_vec(),
            replacement: VALUE_33.to_vec(),
            next: None,
            kind: gt_core::patch::PatchKind::Code,
        }]
    );

    // Desfazer devolve os originais e o laço volta ao valor inicial.
    engine.undo_all().unwrap();
    assert_eq!(bytes_at(t.pid, t.value_page, 5), VALUE_11);
    assert!(wait_word(&t, 0x1111_1111), "{:#x}", read_word(&t));
    assert!(missing(&env));
    assert!(wait_all_running(t.pid), "{:?}", states(t.pid));
}

#[test]
fn set_replacement_foreign_drops_patch() {
    let t = TestTarget::spawn();
    let env = env();
    let mut engine = value_engine(&t, &env, PauseConfig::default());
    engine.apply(vec![value_patch(&t)]).unwrap();
    // Alguém troca os bytes por fora.
    let other = [0xB8, 0x44, 0x44, 0x44, 0x44];
    MemWriter::open(t.pid)
        .unwrap()
        .write(t.value_page, &other)
        .unwrap();

    let report = engine.set_replacement("val", VALUE_33.to_vec()).unwrap();
    assert_eq!(report.foreign, vec!["val".to_string()]);
    assert!(report.restored.is_empty());
    assert!(engine.active().is_empty());
    assert!(engine.pending_ids().is_empty());
    // Nada foi escrito: o alvo continua como o outro deixou.
    assert_eq!(bytes_at(t.pid, t.value_page, 5), other);
    assert!(missing(&env));
    assert!(wait_all_running(t.pid), "{:?}", states(t.pid));
}

#[test]
fn set_replacement_rejects_new_boundaries() {
    let t = TestTarget::spawn();
    let env = env();
    let mut engine = value_engine(&t, &env, PauseConfig::default());
    engine.apply(vec![value_patch(&t)]).unwrap();
    // Cinco `nop` decodificam em outras instruções que o `mov eax, imm32`.
    let err = engine.set_replacement("val", vec![0x90; 5]).unwrap_err();
    assert!(
        matches!(&err, Error::BoundariesChanged { id } if id == "val"),
        "{err:?}"
    );
    assert_eq!(bytes_at(t.pid, t.value_page, 5), VALUE_22);
    assert_eq!(engine.active().len(), 1);
    assert!(engine.pending_ids().is_empty());
    let j = journal_now(&env);
    assert_eq!(j.entries[0].replacement, VALUE_22);
    assert_eq!(j.entries[0].next, None);
}

#[test]
fn set_replacement_1000_times_under_load() {
    let _serial = spin_value_lock();
    let t = spin_value_target();
    let env = env_fast();
    let mut engine = value_engine(&t, &env, PauseConfig::default());
    engine.apply(vec![value_patch(&t)]).unwrap();
    let mut danger = 0;
    let mut last = VALUE_22;
    for i in 0..1000 {
        last = if i % 2 == 0 { VALUE_33 } else { VALUE_22 };
        let report = engine.set_replacement("val", last.to_vec()).unwrap();
        assert_eq!(report.restored, vec!["val".to_string()]);
        danger += report.danger_retries;
    }
    assert_eq!(danger, 0);
    assert_ne!(read_stat(t.pid).unwrap().state, 'Z');
    let want = u32::from_le_bytes([last[1], last[2], last[3], last[4]]);
    assert!(wait_word(&t, want), "{:#x}", read_word(&t));
    // O laço continua escrevendo: uma troca a mais ainda chega à palavra.
    engine.set_replacement("val", VALUE_11.to_vec()).unwrap();
    assert!(wait_word(&t, 0x1111_1111), "{:#x}", read_word(&t));
    engine.undo_all().unwrap();
    assert_eq!(bytes_at(t.pid, t.value_page, 5), VALUE_11);
    assert!(missing(&env));
}

#[test]
fn set_replacement_unknown_id() {
    let t = TestTarget::spawn();
    let env = env();
    let mut engine = value_engine(&t, &env, PauseConfig::default());
    let err = engine
        .set_replacement("nada", VALUE_33.to_vec())
        .unwrap_err();
    assert!(
        matches!(&err, Error::NotActive { id } if id == "nada"),
        "{err:?}"
    );
    assert!(missing(&env));
    // Com outro patch ativo, o `id` errado continua sendo recusado sem tocar nele.
    engine.apply(vec![value_patch(&t)]).unwrap();
    let err = engine
        .set_replacement("nada", VALUE_33.to_vec())
        .unwrap_err();
    assert!(matches!(err, Error::NotActive { .. }), "{err:?}");
    assert_eq!(bytes_at(t.pid, t.value_page, 5), VALUE_22);
    assert_eq!(engine.active().len(), 1);
}

#[test]
fn set_replacement_retries_stuck_resume_first() {
    let t = TestTarget::spawn();
    let env = env();
    let counter = Arc::new(AtomicU32::new(0));
    let mut engine = value_engine(
        &t,
        &env,
        PauseConfig {
            fault: Some(Fault::ContFailsAlways),
            resume_counter: Some(counter.clone()),
            ..PauseConfig::default()
        },
    );
    engine.force_stuck_for_test();
    // Mesmo com `id` desconhecido, o SIGCONT que falhou é tentado antes de tudo.
    let err = engine
        .set_replacement("nada", VALUE_33.to_vec())
        .unwrap_err();
    assert!(matches!(err, Error::Io(_)), "{err:?}");
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    assert!(engine.is_stuck());
}

#[test]
fn set_replacement_clean_refusal_restores_journal() {
    let t = TestTarget::spawn();
    let env = env();
    let dir = env.dir.path().join("state");
    let mut engine = value_engine(&t, &env, PauseConfig::default());
    engine.apply(vec![value_patch(&t)]).unwrap();

    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = engine.set_replacement("val", VALUE_33.to_vec());
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let err = result.unwrap_err();
    match &err {
        Error::Journal(msg) => assert!(msg.contains("journal.json"), "{msg}"),
        other => panic!("esperava Journal, veio {other:?}"),
    }
    // Falhou antes da pausa: o ativo e os bytes no alvo continuam os antigos.
    assert!(!states(t.pid).contains(&'T'), "{:?}", states(t.pid));
    assert_eq!(bytes_at(t.pid, t.value_page, 5), VALUE_22);
    assert_eq!(engine.active().len(), 1);
    assert_eq!(engine.active()[0].replacement, VALUE_22);
    assert!(engine.pending_ids().is_empty());
    let j = journal_now(&env);
    assert_eq!(j.entries.len(), 1);
    assert_eq!(j.entries[0].replacement, VALUE_22);
    assert_eq!(j.entries[0].next, None);

    // Com a permissão de volta, o motor segue normal e o `undo_all` restaura.
    let report = engine.undo_all().unwrap();
    assert_eq!(report.restored, vec!["val".to_string()]);
    assert_eq!(bytes_at(t.pid, t.value_page, 5), VALUE_11);
    assert!(missing(&env));
}

#[test]
fn set_replacement_dirty_leaves_pending_with_next() {
    let t = TestTarget::spawn();
    let env = env();
    // A falha de escrita vale só para a segunda pausa (a da troca).
    let mut engine = value_engine(
        &t,
        &env,
        PauseConfig {
            fault: Some(Fault::WriteAndRollbackFail),
            hook_on_pause: 2,
            ..PauseConfig::default()
        },
    );
    engine.apply(vec![value_patch(&t)]).unwrap();
    assert_eq!(engine.active().len(), 1);

    let err = engine
        .set_replacement("val", VALUE_33.to_vec())
        .unwrap_err();
    assert!(matches!(err, Error::Io(_)), "{err:?}");
    assert!(!engine.is_stuck());
    assert!(engine.active().is_empty());
    assert_eq!(engine.pending_ids(), vec!["val".to_string()]);
    // Os bytes novos ficaram no jogo e o diário guarda os três conjuntos de bytes.
    assert_eq!(bytes_at(t.pid, t.value_page, 5), VALUE_33);
    let j = journal_now(&env);
    assert!(!j.paused_by_us);
    assert_eq!(
        j.entries,
        vec![JournalEntry {
            id: "val".into(),
            addr: t.value_page,
            original: VALUE_11.to_vec(),
            replacement: VALUE_22.to_vec(),
            next: Some(VALUE_33.to_vec()),
            kind: gt_core::patch::PatchKind::Code,
        }]
    );

    let report = engine.restore_pending().unwrap();
    assert_eq!(report.restored, vec!["val".to_string()]);
    assert_eq!(bytes_at(t.pid, t.value_page, 5), VALUE_11);
    assert!(engine.pending_ids().is_empty());
    assert!(missing(&env));
}

static KILL_PID_SWAP: AtomicI32 = AtomicI32::new(0);

/// Mata o alvo dentro da pausa da troca e espera ele virar zumbi.
fn kill_in_swap(point: HookPoint) {
    if point != HookPoint::InPause {
        return;
    }
    let pid = KILL_PID_SWAP.load(Ordering::SeqCst);
    kill(Pid::from_raw(pid), Signal::SIGKILL).unwrap();
    let dead = wait_until(
        || Ok(read_stat(pid).map(|s| s.state == 'Z').unwrap_or(true)),
        Duration::from_secs(2),
        Duration::from_millis(1),
    )
    .unwrap();
    assert!(dead);
}

#[test]
fn set_replacement_target_exit_is_process_gone() {
    let t = TestTarget::spawn();
    let env = env();
    KILL_PID_SWAP.store(t.pid, Ordering::SeqCst);
    let mut engine = value_engine(
        &t,
        &env,
        PauseConfig {
            hook: Some(kill_in_swap),
            hook_on_pause: 2,
            ..PauseConfig::default()
        },
    );
    engine.apply(vec![value_patch(&t)]).unwrap();
    let err = engine
        .set_replacement("val", VALUE_33.to_vec())
        .unwrap_err();
    assert!(matches!(err, Error::ProcessGone { .. }), "{err:?}");
    assert!(engine.active().is_empty());
    assert!(engine.pending_ids().is_empty());
    assert!(!engine.is_stuck());
    assert!(missing(&env));
}

/// Caminho do diário e o que o gancho viu dentro da pausa da troca.
static JOURNAL_PATH_SWAP: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);
static JOURNAL_SEEN_SWAP: Mutex<Option<gt_core::journal::Journal>> = Mutex::new(None);

/// Lê o diário dentro da pausa e guarda o que viu.
fn read_journal_in_swap(point: HookPoint) {
    if point != HookPoint::InPause {
        return;
    }
    let dir = JOURNAL_PATH_SWAP.lock().unwrap().clone().unwrap();
    if let Loaded::Ok(j) = JournalStore::new(dir).peek().unwrap() {
        *JOURNAL_SEEN_SWAP.lock().unwrap() = Some(j);
    }
}

#[test]
fn set_replacement_rewrite_keeps_next() {
    let t = TestTarget::spawn();
    let env = env();
    *JOURNAL_PATH_SWAP.lock().unwrap() = Some(env.dir.path().join("state"));
    *JOURNAL_SEEN_SWAP.lock().unwrap() = None;
    let mut engine = value_engine(
        &t,
        &env,
        PauseConfig {
            hook: Some(read_journal_in_swap),
            hook_on_pause: 2,
            ..PauseConfig::default()
        },
    );
    engine.apply(vec![value_patch(&t)]).unwrap();
    // O gancho só dispara na segunda pausa: depois do `apply` ainda não viu nada.
    assert!(JOURNAL_SEEN_SWAP.lock().unwrap().is_none());
    engine.set_replacement("val", VALUE_33.to_vec()).unwrap();
    let seen = JOURNAL_SEEN_SWAP.lock().unwrap().take().unwrap();
    assert!(seen.paused_by_us);
    assert_eq!(seen.entries.len(), 1);
    assert_eq!(seen.entries[0].replacement, VALUE_22);
    assert_eq!(seen.entries[0].next, Some(VALUE_33.to_vec()));
    // Depois da troca, o diário final não tem `next`.
    let j = journal_now(&env);
    assert_eq!(j.entries[0].replacement, VALUE_33);
    assert_eq!(j.entries[0].next, None);
}

// ---------------------------------------------------------------------------
// Escrita em dados (plano 005, tarefa 12)
// ---------------------------------------------------------------------------

/// "Objeto" em `big_region`: 16 bytes de guarda no início e o byte dos bits em `+0x100`.
struct DataObj {
    guard: Vec<gt_core::journal::GuardBytes>,
    byte: u64,
    word: u64,
    region: Range<u64>,
}

fn data_obj(t: &TestTarget) -> DataObj {
    let w = MemWriter::open(t.pid).unwrap();
    let marker: Vec<u8> = (0..16u8).map(|i| 0xC0 | i).collect();
    w.write(t.big_region, &marker).unwrap();
    w.write(t.big_region + 0x100, &[0x07]).unwrap();
    DataObj {
        guard: vec![gt_core::journal::GuardBytes {
            addr: t.big_region,
            bytes: marker,
        }],
        byte: t.big_region + 0x100,
        word: t.big_region + 0x200,
        region: t.big_region..t.big_region + t.big_len as u64,
    }
}

/// Motor sem política de patches e com a política de dados = a região do objeto.
fn data_engine(t: &TestTarget, env: &Env, obj: &DataObj) -> Engine {
    let mut engine = open_engine(t.pid, env, Vec::new(), None);
    let region = obj.region.clone();
    engine.set_data_policy(Box::new(move |_maps| {
        WritePolicy::new(vec![region.clone()])
    }));
    engine
}

fn byte_at(t: &TestTarget, addr: u64) -> u8 {
    let mut b = [0u8; 1];
    MemReader::new(t.pid).read_exact(addr, &mut b).unwrap();
    b[0]
}

fn poke(t: &TestTarget, addr: u64, bytes: &[u8]) {
    MemWriter::open(t.pid).unwrap().write(addr, bytes).unwrap();
}

#[test]
fn write_data_writes_and_rereads() {
    let t = TestTarget::spawn();
    let env = env();
    let obj = data_obj(&t);
    let mut engine = data_engine(&t, &env, &obj);
    engine
        .write_data(obj.word, &1.5f32.to_le_bytes(), &obj.guard)
        .unwrap();
    let mut back = [0u8; 4];
    MemReader::new(t.pid)
        .read_exact(obj.word, &mut back)
        .unwrap();
    assert_eq!(f32::from_le_bytes(back), 1.5);
    // Barras não vão ao diário.
    assert_eq!(env.store.peek().unwrap(), Loaded::Missing);
    // Sem política de dados, nenhuma escrita.
    let mut bare = open_engine(t.pid, &env, Vec::new(), None);
    assert!(matches!(
        bare.write_data(obj.word, &[0], &obj.guard),
        Err(Error::NoDataPolicy)
    ));
}

#[test]
fn write_data_outside_policy_refused() {
    let t = TestTarget::spawn();
    let env = env();
    let obj = data_obj(&t);
    let mut engine = data_engine(&t, &env, &obj);
    let err = engine.write_data(t.exec_page, &[0x90], &[]).unwrap_err();
    assert!(
        matches!(err, Error::DataOutsidePolicy { addr } if addr == t.exec_page),
        "{err:?}"
    );
    // A escrita que cruza o fim da região também é recusada.
    let edge = obj.region.end - 2;
    assert!(matches!(
        engine.write_data(edge, &[0; 4], &[]),
        Err(Error::DataOutsidePolicy { .. })
    ));
}

#[test]
fn write_data_guard_mismatch_skips() {
    let t = TestTarget::spawn();
    let env = env();
    let obj = data_obj(&t);
    let mut engine = data_engine(&t, &env, &obj);
    poke(&t, obj.word, &7f32.to_le_bytes());
    // O objeto muda entre a leitura e a escrita (outro pawn no mesmo endereço).
    poke(&t, t.big_region + 3, &[0x00]);
    let err = engine
        .write_data(obj.word, &1f32.to_le_bytes(), &obj.guard)
        .unwrap_err();
    assert!(matches!(err, Error::GuardMismatch { .. }), "{err:?}");
    let mut back = [0u8; 4];
    MemReader::new(t.pid)
        .read_exact(obj.word, &mut back)
        .unwrap();
    assert_eq!(f32::from_le_bytes(back), 7.0);
}

#[test]
fn set_bits_journals_before_write() {
    let t = TestTarget::spawn();
    let env = env();
    let obj = data_obj(&t);
    let mut engine = data_engine(&t, &env, &obj);
    engine
        .set_bits("nao-morrer", obj.byte, 0x06, &obj.guard)
        .unwrap();
    assert_eq!(byte_at(&t, obj.byte), 0x01);
    let j = journal_now(&env);
    assert!(j.entries.is_empty());
    let [e] = j.data.as_slice() else {
        panic!("esperada uma entrada de dados: {:?}", j.data);
    };
    assert_eq!(
        (
            e.id.as_str(),
            e.addr,
            e.original.as_slice(),
            e.written.as_slice(),
            e.mask
        ),
        (
            "nao-morrer",
            obj.byte,
            [0x07].as_slice(),
            [0x01].as_slice(),
            0x06
        )
    );
    assert_eq!(e.guard, obj.guard);
    assert_eq!(engine.data_entries().len(), 1);
}

#[test]
fn set_bits_maintenance_reclears() {
    let t = TestTarget::spawn();
    let env = env();
    let obj = data_obj(&t);
    let mut engine = data_engine(&t, &env, &obj);
    engine
        .set_bits("queda", obj.byte, 0x06, &obj.guard)
        .unwrap();
    // O jogo religa um dos bits: a manutenção limpa de novo e o original não muda.
    poke(&t, obj.byte, &[0x05]);
    engine
        .set_bits("queda", obj.byte, 0x06, &obj.guard)
        .unwrap();
    assert_eq!(byte_at(&t, obj.byte), 0x01);
    let j = journal_now(&env);
    assert_eq!(j.data.len(), 1);
    assert_eq!(j.data[0].original, vec![0x07]);
    // Bits já limpos: nada muda.
    engine
        .set_bits("queda", obj.byte, 0x06, &obj.guard)
        .unwrap();
    assert_eq!(byte_at(&t, obj.byte), 0x01);
}

#[test]
fn restore_keeps_foreign_bits() {
    let t = TestTarget::spawn();
    let env = env();
    let obj = data_obj(&t);
    let mut engine = data_engine(&t, &env, &obj);
    engine
        .set_bits("nao-morrer", obj.byte, 0x06, &obj.guard)
        .unwrap();
    // O jogo liga outro bit do mesmo byte enquanto os nossos estão limpos.
    poke(&t, obj.byte, &[0x81]);
    let report = engine.restore_field("nao-morrer").unwrap();
    assert_eq!(report.restored, vec!["nao-morrer"]);
    assert_eq!(byte_at(&t, obj.byte), 0x87);
    assert_eq!(env.store.peek().unwrap(), Loaded::Missing);
    assert!(engine.data_entries().is_empty());
}

#[test]
fn restore_guard_mismatch_discards() {
    let t = TestTarget::spawn();
    let env = env();
    let obj = data_obj(&t);
    let mut engine = data_engine(&t, &env, &obj);
    engine
        .set_bits("nao-morrer", obj.byte, 0x06, &obj.guard)
        .unwrap();
    // O objeto não é mais o mesmo: a entrada sai sem escrever nada.
    poke(&t, t.big_region, &[0x00]);
    let report = engine.restore_field("nao-morrer").unwrap();
    assert_eq!(report.foreign, vec!["nao-morrer"]);
    assert!(report.restored.is_empty());
    assert_eq!(byte_at(&t, obj.byte), 0x01);
    assert_eq!(env.store.peek().unwrap(), Loaded::Missing);
}

#[test]
fn undo_all_restores_bits() {
    let t = TestTarget::spawn();
    let env = env();
    let obj = data_obj(&t);
    {
        let mut engine = data_engine(&t, &env, &obj);
        engine.set_bits("a", obj.byte, 0x02, &obj.guard).unwrap();
        engine.set_bits("b", obj.byte, 0x04, &obj.guard).unwrap();
        assert_eq!(byte_at(&t, obj.byte), 0x01);
        let report = engine.undo_all().unwrap();
        let mut restored = report.restored.clone();
        restored.sort();
        assert_eq!(restored, vec!["a", "b"]);
        assert_eq!(byte_at(&t, obj.byte), 0x07);
        assert_eq!(env.store.peek().unwrap(), Loaded::Missing);
        // O `Drop` também restaura.
        engine.set_bits("a", obj.byte, 0x02, &obj.guard).unwrap();
        assert_eq!(byte_at(&t, obj.byte), 0x05);
    }
    assert_eq!(byte_at(&t, obj.byte), 0x07);
    assert_eq!(env.store.peek().unwrap(), Loaded::Missing);
}

#[test]
fn set_bits_refused_with_pending() {
    let t = TestTarget::spawn();
    let env = env();
    let obj = data_obj(&t);
    let mut engine = data_engine(&t, &env, &obj);
    engine.force_data_pending_for_test(gt_core::journal::DataEntry::bits(
        "nao-morrer",
        obj.byte,
        0x07,
        0x01,
        0x06,
        obj.guard.clone(),
    ));
    assert_eq!(engine.pending_ids(), vec!["nao-morrer"]);
    let err = engine
        .set_bits("nao-morrer", obj.byte, 0x06, &obj.guard)
        .unwrap_err();
    assert!(
        matches!(&err, Error::DataPending { id } if id == "nao-morrer"),
        "{err:?}"
    );
    assert_eq!(byte_at(&t, obj.byte), 0x07);
    // Outro `id` (em outro byte) segue livre, e a recuperação (`r`) resolve o resto.
    poke(&t, obj.byte + 1, &[0x10]);
    engine
        .set_bits("outro", obj.byte + 1, 0x10, &obj.guard)
        .unwrap();
    assert_eq!(byte_at(&t, obj.byte + 1), 0x00);
    let report = engine.restore_pending().unwrap();
    assert_eq!(report.dropped, vec!["nao-morrer"]);
    assert!(engine.pending_ids().is_empty());
}

// ---------------------------------------------------------------------------
// Escrita `set` de valor inteiro (plano 006, tarefa 3)
// ---------------------------------------------------------------------------

const OLD: [u8; 4] = [1, 2, 3, 4];
const OURS: [u8; 4] = [9, 9, 9, 9];
const OTHER: [u8; 4] = [7, 7, 7, 7];

fn word_bytes(t: &TestTarget, addr: u64) -> [u8; 4] {
    let mut b = [0u8; 4];
    MemReader::new(t.pid).read_exact(addr, &mut b).unwrap();
    b
}

fn accept_all(_: &[u8]) -> bool {
    true
}

fn set(engine: &mut Engine, obj: &DataObj, value: &[u8]) -> gt_core::Result<SetOutcome> {
    engine.set_word("carga", obj.word, value, &obj.guard, &accept_all)
}

#[test]
fn set_word_creates_entry_before_write() {
    let t = TestTarget::spawn();
    let env = env();
    let obj = data_obj(&t);
    let mut engine = data_engine(&t, &env, &obj);
    poke(&t, obj.word, &OLD);
    assert_eq!(set(&mut engine, &obj, &OURS).unwrap(), SetOutcome::Written);
    assert_eq!(word_bytes(&t, obj.word), OURS);
    let j = journal_now(&env);
    let [e] = j.data.as_slice() else {
        panic!("esperada uma entrada: {:?}", j.data);
    };
    assert_eq!(e.mode, gt_core::journal::DataMode::Whole);
    assert_eq!(
        (
            e.id.as_str(),
            e.addr,
            e.original.as_slice(),
            e.written.as_slice()
        ),
        ("carga", obj.word, OLD.as_slice(), OURS.as_slice())
    );
    assert_eq!(e.next, None);
    assert_eq!(e.guard, obj.guard);
}

#[test]
fn set_word_equal_value_creates_entry() {
    let t = TestTarget::spawn();
    let env = env();
    let obj = data_obj(&t);
    let mut engine = data_engine(&t, &env, &obj);
    poke(&t, obj.word, &OURS);
    assert_eq!(set(&mut engine, &obj, &OURS).unwrap(), SetOutcome::Written);
    let j = journal_now(&env);
    assert_eq!(j.data.len(), 1);
    assert_eq!(j.data[0].original, OURS.to_vec());
    // Ligar de novo com o mesmo valor: nada a fazer.
    assert_eq!(
        set(&mut engine, &obj, &OURS).unwrap(),
        SetOutcome::Unchanged
    );
}

#[test]
fn set_word_rejects_insane_current() {
    let t = TestTarget::spawn();
    let env = env();
    let obj = data_obj(&t);
    let mut engine = data_engine(&t, &env, &obj);
    poke(&t, obj.word, &OLD);
    let out = engine
        .set_word("carga", obj.word, &OURS, &obj.guard, &|_| false)
        .unwrap();
    assert_eq!(out, SetOutcome::Rejected(OLD.to_vec()));
    assert_eq!(word_bytes(&t, obj.word), OLD);
    assert!(engine.data_entries().is_empty());
    assert_eq!(env.store.peek().unwrap(), Loaded::Missing);
}

#[test]
fn set_word_game_change_new_original() {
    let t = TestTarget::spawn();
    let env = env();
    let obj = data_obj(&t);
    let mut engine = data_engine(&t, &env, &obj);
    poke(&t, obj.word, &OLD);
    set(&mut engine, &obj, &OURS).unwrap();
    // O jogo muda o campo; um valor insano não vira original.
    poke(&t, obj.word, &OTHER);
    let out = engine
        .set_word("carga", obj.word, &OURS, &obj.guard, &|_| false)
        .unwrap();
    assert_eq!(out, SetOutcome::Rejected(OTHER.to_vec()));
    assert_eq!(word_bytes(&t, obj.word), OTHER);
    assert_eq!(journal_now(&env).data[0].original, OLD.to_vec());
    // Valor são: vira o novo original, com o diário atualizado, uma entrada só.
    let out = set(&mut engine, &obj, &OURS).unwrap();
    assert_eq!(out, SetOutcome::NewOriginal(OTHER.to_vec()));
    assert_eq!(word_bytes(&t, obj.word), OURS);
    let j = journal_now(&env);
    assert_eq!(j.data.len(), 1);
    assert_eq!(j.data[0].original, OTHER.to_vec());
    assert_eq!(engine.data_entries().len(), 1);
    let report = engine.restore_field("carga").unwrap();
    assert_eq!(report.restored, vec!["carga"]);
    assert_eq!(word_bytes(&t, obj.word), OTHER);
    assert_eq!(env.store.peek().unwrap(), Loaded::Missing);
}

#[test]
fn set_word_swap_uses_next() {
    let t = TestTarget::spawn();
    let env = env();
    let obj = data_obj(&t);
    let mut engine = data_engine(&t, &env, &obj);
    poke(&t, obj.word, &OLD);
    set(&mut engine, &obj, &OURS).unwrap();
    assert_eq!(set(&mut engine, &obj, &OTHER).unwrap(), SetOutcome::Written);
    assert_eq!(word_bytes(&t, obj.word), OTHER);
    let j = journal_now(&env);
    assert_eq!(j.data.len(), 1);
    assert_eq!(j.data[0].original, OLD.to_vec());
    assert_eq!(j.data[0].written, OTHER.to_vec());
    assert_eq!(j.data[0].next, None);
    assert_eq!(engine.data_entries()[0].next, None);
    engine.restore_field("carga").unwrap();
    assert_eq!(word_bytes(&t, obj.word), OLD);
}

#[test]
fn set_word_next_equal_current_finalizes() {
    let t = TestTarget::spawn();
    let env = env();
    let obj = data_obj(&t);
    let mut engine = data_engine(&t, &env, &obj);
    // Troca interrompida depois da escrita: o campo já tem o `next`.
    let mut e = gt_core::journal::DataEntry::whole(
        "carga",
        obj.word,
        OLD.to_vec(),
        OURS.to_vec(),
        obj.guard.clone(),
    );
    e.next = Some(OTHER.to_vec());
    engine.force_data_for_test(e);
    poke(&t, obj.word, &OTHER);
    assert_eq!(
        set(&mut engine, &obj, &OTHER).unwrap(),
        SetOutcome::Finalized
    );
    let j = journal_now(&env);
    assert_eq!(j.data[0].written, OTHER.to_vec());
    assert_eq!(j.data[0].next, None);
    assert_eq!(
        set(&mut engine, &obj, &OTHER).unwrap(),
        SetOutcome::Unchanged
    );
}

#[test]
fn set_word_next_equal_written_drops_next_and_swaps() {
    let t = TestTarget::spawn();
    let env = env();
    let obj = data_obj(&t);
    let mut engine = data_engine(&t, &env, &obj);
    // Troca interrompida antes da escrita: o campo ainda tem o `written`.
    let mut e = gt_core::journal::DataEntry::whole(
        "carga",
        obj.word,
        OLD.to_vec(),
        OURS.to_vec(),
        obj.guard.clone(),
    );
    e.next = Some(OTHER.to_vec());
    engine.force_data_for_test(e);
    poke(&t, obj.word, &OURS);
    // Mesmo valor do `written`: retira o `next` e nada mais.
    assert_eq!(
        set(&mut engine, &obj, &OURS).unwrap(),
        SetOutcome::Unchanged
    );
    assert_eq!(journal_now(&env).data[0].next, None);
    assert_eq!(word_bytes(&t, obj.word), OURS);
}

#[test]
fn set_same_address_new_guard() {
    let t = TestTarget::spawn();
    let env = env();
    let obj = data_obj(&t);
    let mut engine = data_engine(&t, &env, &obj);
    poke(&t, obj.word, &OLD);
    set(&mut engine, &obj, &OURS).unwrap();
    // Outro objeto no mesmo endereço: guarda nova e valor próprio.
    let marker2: Vec<u8> = (0..16u8).map(|i| 0xD0 | i).collect();
    poke(&t, t.big_region, &marker2);
    poke(&t, obj.word, &OTHER);
    let guard2 = vec![gt_core::journal::GuardBytes {
        addr: t.big_region,
        bytes: marker2,
    }];
    let err = engine
        .set_word("carga", obj.word, &OURS, &guard2, &accept_all)
        .unwrap_err();
    assert!(
        matches!(&err, Error::DataEntryOtherObject { id } if id == "carga"),
        "{err:?}"
    );
    assert_eq!(word_bytes(&t, obj.word), OTHER);
    // Quem chama resolve a entrada antiga: a guarda dela não bate, nada é escrito.
    let report = engine.restore_field("carga").unwrap();
    assert_eq!(report.foreign, vec!["carga"]);
    assert_eq!(word_bytes(&t, obj.word), OTHER);
    assert!(engine.data_entries().is_empty());
    // A entrada nova tem o original lido de novo.
    let out = engine
        .set_word("carga", obj.word, &OURS, &guard2, &accept_all)
        .unwrap();
    assert_eq!(out, SetOutcome::Written);
    let j = journal_now(&env);
    assert_eq!(j.data.len(), 1);
    assert_eq!(j.data[0].original, OTHER.to_vec());
    assert_eq!(j.data[0].guard, guard2);
}

#[test]
fn restore_field_whole_ours_restores() {
    let t = TestTarget::spawn();
    let env = env();
    let obj = data_obj(&t);
    let mut engine = data_engine(&t, &env, &obj);
    poke(&t, obj.word, &OLD);
    set(&mut engine, &obj, &OURS).unwrap();
    let report = engine.restore_field("carga").unwrap();
    assert_eq!(report.restored, vec!["carga"]);
    assert_eq!(word_bytes(&t, obj.word), OLD);
    assert_eq!(env.store.peek().unwrap(), Loaded::Missing);
    assert!(engine.data_entries().is_empty());
}

#[test]
fn restore_field_whole_game_value_forgets() {
    let t = TestTarget::spawn();
    let env = env();
    let obj = data_obj(&t);
    let mut engine = data_engine(&t, &env, &obj);
    poke(&t, obj.word, &OLD);
    set(&mut engine, &obj, &OURS).unwrap();
    // Valor do jogo, inclusive parecido com o nosso em parte dos bytes: nada é escrito.
    poke(&t, obj.word, &[9, 9, 9, 5]);
    let report = engine.restore_field("carga").unwrap();
    assert_eq!(report.restored, vec!["carga"]);
    assert_eq!(word_bytes(&t, obj.word), [9, 9, 9, 5]);
    assert_eq!(env.store.peek().unwrap(), Loaded::Missing);
}

#[test]
fn entry_update_then_failed_restore_no_stale_copy() {
    let t = TestTarget::spawn();
    let env = env();
    let obj = data_obj(&t);
    let mut engine = data_engine(&t, &env, &obj);
    poke(&t, obj.word, &OLD);
    set(&mut engine, &obj, &OURS).unwrap();
    // O jogo muda o campo: o original é atualizado (mesmo par, outra entrada).
    poke(&t, obj.word, &OTHER);
    assert_eq!(
        set(&mut engine, &obj, &OURS).unwrap(),
        SetOutcome::NewOriginal(OTHER.to_vec())
    );
    // A restauração falha: sobra uma entrada só por par, com o original atualizado.
    engine.inject_data_fault_for_test(Some(DataFault::RestoreFails));
    assert!(engine.restore_field("carga").is_err());
    assert_eq!(engine.pending_ids(), vec!["carga"]);
    assert!(engine.data_entries().is_empty());
    let j = journal_now(&env);
    assert_eq!(j.data.len(), 1);
    assert_eq!(j.data[0].original, OTHER.to_vec());
    assert_eq!(word_bytes(&t, obj.word), OURS);
    // Sem a falha, a recuperação devolve o original novo, não o velho.
    engine.inject_data_fault_for_test(None);
    let report = engine.restore_pending().unwrap();
    assert_eq!(report.restored, vec!["carga"]);
    assert_eq!(word_bytes(&t, obj.word), OTHER);
    assert!(engine.pending_ids().is_empty());
    assert_eq!(env.store.peek().unwrap(), Loaded::Missing);
}

#[test]
fn set_word_reread_third_value_keeps_entry_alive() {
    let t = TestTarget::spawn();
    let env = env();
    let obj = data_obj(&t);
    let mut engine = data_engine(&t, &env, &obj);
    poke(&t, obj.word, &OLD);
    // O jogo escreve entre a nossa escrita e a releitura: terceiro valor.
    engine.inject_data_fault_for_test(Some(DataFault::ThirdValue(OTHER.to_vec())));
    let err = set(&mut engine, &obj, &OURS).unwrap_err();
    assert!(matches!(err, Error::BytesMismatch { .. }), "{err:?}");
    assert!(engine.pending_ids().is_empty());
    assert_eq!(engine.data_entries().len(), 1);
    assert_eq!(word_bytes(&t, obj.word), OTHER);
    // O próximo `set_word` cai em "o jogo mudou o campo", sem `DataPending`.
    assert_eq!(
        set(&mut engine, &obj, &OURS).unwrap(),
        SetOutcome::NewOriginal(OTHER.to_vec())
    );
    assert_eq!(word_bytes(&t, obj.word), OURS);
    let j = journal_now(&env);
    assert_eq!(j.data.len(), 1);
    assert_eq!(j.data[0].original, OTHER.to_vec());
}

#[test]
fn set_word_refused_with_pending_and_bad_len() {
    let t = TestTarget::spawn();
    let env = env();
    let obj = data_obj(&t);
    let mut engine = data_engine(&t, &env, &obj);
    assert!(matches!(
        engine.set_word("carga", obj.word, &[], &obj.guard, &accept_all),
        Err(Error::BadPatch { .. })
    ));
    assert!(matches!(
        engine.set_word("carga", obj.word, &[0; 9], &obj.guard, &accept_all),
        Err(Error::BadPatch { .. })
    ));
    engine.force_data_pending_for_test(gt_core::journal::DataEntry::whole(
        "carga",
        obj.word,
        OLD.to_vec(),
        OURS.to_vec(),
        obj.guard.clone(),
    ));
    let err = set(&mut engine, &obj, &OURS).unwrap_err();
    assert!(
        matches!(&err, Error::DataPending { id } if id == "carga"),
        "{err:?}"
    );
}

#[test]
fn undo_all_and_drop_restore_whole() {
    let t = TestTarget::spawn();
    let env = env();
    let obj = data_obj(&t);
    poke(&t, obj.word, &OLD);
    {
        let mut engine = data_engine(&t, &env, &obj);
        set(&mut engine, &obj, &OURS).unwrap();
        engine.undo_all().unwrap();
        assert_eq!(word_bytes(&t, obj.word), OLD);
        set(&mut engine, &obj, &OURS).unwrap();
        assert_eq!(word_bytes(&t, obj.word), OURS);
    }
    assert_eq!(word_bytes(&t, obj.word), OLD);
    assert_eq!(env.store.peek().unwrap(), Loaded::Missing);
}

// ---------------------------------------------------------------------------
// Variáveis das caves na folga da `.data` (plano 008, tarefa 3)
// ---------------------------------------------------------------------------

/// Região de variáveis de teste: 280 bytes (o total do orçamento) em `big_region`, que é
/// `rw-` e zerada.
fn vars_region(t: &TestTarget) -> Range<u64> {
    let start = t.big_region + 0x800;
    start..start + 280
}

fn vars_in_journal(env: &Env) -> Vec<gt_core::journal::VarRegion> {
    match env.store.peek().unwrap() {
        Loaded::Ok(j) => j.vars,
        Loaded::Missing => Vec::new(),
        other => panic!("diário inesperado: {other:?}"),
    }
}

#[test]
fn claim_vars_requires_zero() {
    let t = TestTarget::spawn();
    let env = env();
    let region = vars_region(&t);
    let mut engine = open_engine(t.pid, &env, Vec::new(), None);
    // Um byte diferente de zero no meio: recusa, sem diário e sem região reclamada.
    poke(&t, region.start + 100, &[0x01]);
    let err = engine.claim_vars(region.clone()).unwrap_err();
    assert!(
        matches!(err, Error::VarsNotZero { addr } if addr == region.start + 100),
        "{err:?}"
    );
    assert!(engine.vars().is_empty());
    assert_eq!(env.store.peek().unwrap(), Loaded::Missing);
    // Zerado: reclama, e a região vai ao diário (só endereço e tamanho).
    poke(&t, region.start + 100, &[0x00]);
    engine.claim_vars(region.clone()).unwrap();
    assert_eq!(
        vars_in_journal(&env),
        vec![gt_core::journal::VarRegion {
            addr: region.start,
            len: 280
        }]
    );
    // A mesma região de novo é um no-op; uma que a corta em parte é recusada.
    engine.claim_vars(region.clone()).unwrap();
    assert_eq!(engine.vars().len(), 1);
    let err = engine
        .claim_vars(region.start + 8..region.end + 8)
        .unwrap_err();
    assert!(matches!(err, Error::BadPatch { .. }), "{err:?}");
    assert_eq!(engine.vars().len(), 1);
}

#[test]
fn claim_vars_refuses_non_rw_and_bad_shapes() {
    let t = TestTarget::spawn();
    let env = env();
    let mut engine = open_engine(t.pid, &env, Vec::new(), None);
    // Página executável (`r-x`) e página ilegível (`---`): não são `rw-`.
    for start in [t.exec2, t.region_start + t.page] {
        let err = engine.claim_vars(start..start + 64).unwrap_err();
        assert!(matches!(err, Error::VarsNotWritable { .. }), "{err:?}");
    }
    // Trecho que invade a página vizinha não `rw-`.
    let err = engine
        .claim_vars(t.region_start + t.page - 8..t.region_start + t.page + 8)
        .unwrap_err();
    assert!(matches!(err, Error::VarsNotWritable { .. }), "{err:?}");
    // Vazia, desalinhada e grande demais.
    let start = t.big_region + 0x800;
    for bad in [start..start, start + 4..start + 32, start..start + 4104] {
        let err = engine.claim_vars(bad).unwrap_err();
        assert!(matches!(err, Error::BadPatch { .. }), "{err:?}");
    }
    assert!(engine.vars().is_empty());
    assert_eq!(env.store.peek().unwrap(), Loaded::Missing);
}

#[test]
fn write_var_outside_region_refused() {
    let t = TestTarget::spawn();
    let env = env();
    let region = vars_region(&t);
    let mut engine = open_engine(t.pid, &env, Vec::new(), None);
    // Antes de reclamar, nada é variável.
    let err = engine.write_var(region.start, &[1]).unwrap_err();
    assert!(matches!(err, Error::VarOutsideRegion { .. }), "{err:?}");
    engine.claim_vars(region.clone()).unwrap();
    // Dentro: escreve e lê de volta.
    engine
        .write_var(region.start + 8, &7i32.to_le_bytes())
        .unwrap();
    assert_eq!(
        engine.read_var(region.start + 8, 4).unwrap(),
        7i32.to_le_bytes()
    );
    engine
        .write_var(region.end - 8, &9u64.to_le_bytes())
        .unwrap();
    // Fora (antes, cruzando o fim, depois) e vazia: recusa e não escreve nada.
    for (addr, len) in [
        (region.start - 1, 1),
        (region.end - 4, 8),
        (region.end, 1),
        (region.start, 0),
    ] {
        let bytes = vec![0xEEu8; len];
        let err = engine.write_var(addr, &bytes).unwrap_err();
        assert!(matches!(err, Error::VarOutsideRegion { .. }), "{err:?}");
        let err = engine.read_var(addr, len).unwrap_err();
        assert!(matches!(err, Error::VarOutsideRegion { .. }), "{err:?}");
    }
    assert_eq!(bytes_at(t.pid, region.start - 1, 1), [0]);
    assert_eq!(bytes_at(t.pid, region.end - 4, 4), [0, 0, 0, 0]);
    assert_eq!(bytes_at(t.pid, region.end, 1), [0]);
}

#[test]
fn undo_all_zeros_vars_after_caves() {
    let t = TestTarget::spawn();
    let env = env();
    let region = vars_region(&t);
    let cave_addr = t.exec_page + 0x100;
    let mut engine = open_engine(
        t.pid,
        &env,
        vec![exec2_range(&t), page_range(&t, t.exec_page)],
        None,
    );
    engine.claim_vars(region.clone()).unwrap();
    engine.write_var(region.start, &5i32.to_le_bytes()).unwrap();
    engine.write_var(region.end - 8, &[0xAA; 8]).unwrap();
    engine
        .apply(vec![
            patch("p", t.exec2, &NOP3, &NOP2_NOP),
            cave("c", cave_addr),
        ])
        .unwrap();
    engine.undo_all().unwrap();
    assert_eq!(bytes_at(t.pid, region.start, 280), vec![0u8; 280]);
    assert_eq!(bytes_at(t.pid, cave_addr, 16), vec![0xCC; 16]);
    assert!(engine.vars().is_empty());
    assert!(missing(&env));
    // Já zerada, a região pode ser reclamada de novo.
    engine.claim_vars(region).unwrap();
}

#[test]
fn drop_zeros_vars() {
    let t = TestTarget::spawn();
    let env = env();
    let region = vars_region(&t);
    {
        let mut engine = open_engine(t.pid, &env, Vec::new(), None);
        engine.claim_vars(region.clone()).unwrap();
        engine.write_var(region.start + 40, &[1; 8]).unwrap();
        assert_eq!(vars_in_journal(&env).len(), 1);
    }
    assert_eq!(bytes_at(t.pid, region.start, 280), vec![0u8; 280]);
    assert!(missing(&env));
}

#[test]
fn vars_not_zeroed_while_code_active() {
    let t = TestTarget::spawn();
    let env = env();
    let region = vars_region(&t);
    let mut engine = open_engine(t.pid, &env, vec![exec2_range(&t)], None);
    engine.claim_vars(region.clone()).unwrap();
    engine.write_var(region.start, &5i32.to_le_bytes()).unwrap();
    engine
        .apply(vec![patch("p", t.exec2, &NOP3, &NOP2_NOP)])
        .unwrap();
    // Com o desvio ativo, limpar não escreve nada e a região continua no diário.
    engine.clear_vars().unwrap();
    assert_eq!(bytes_at(t.pid, region.start, 4), 5i32.to_le_bytes());
    assert_eq!(engine.vars().len(), 1);
    assert_eq!(vars_in_journal(&env).len(), 1);
    // Sem o desvio, zera.
    engine.undo(&["p"]).unwrap();
    engine.clear_vars().unwrap();
    assert_eq!(bytes_at(t.pid, region.start, 4), [0; 4]);
    assert!(engine.vars().is_empty());
    assert!(missing(&env));
}

static DENY_POLICY_VARS: AtomicBool = AtomicBool::new(false);

#[test]
fn undo_all_and_drop_keep_vars_when_code_cannot_be_undone() {
    let t = TestTarget::spawn();
    let env = env();
    let region = vars_region(&t);
    let range = exec2_range(&t);
    let policy: PolicyFn = Box::new(move |_maps| {
        if DENY_POLICY_VARS.load(Ordering::SeqCst) {
            WritePolicy::new(Vec::new())
        } else {
            WritePolicy::new(vec![range.clone()])
        }
    });
    let opts = EngineOptions {
        version_key: "teste".into(),
        pause: PauseConfig::default(),
    };
    let mut engine = Engine::open(t.pid, None, env.store.clone(), &env.lock, policy, opts).unwrap();
    engine.claim_vars(region.clone()).unwrap();
    engine.write_var(region.start, &5i32.to_le_bytes()).unwrap();
    engine
        .apply(vec![patch("p", t.exec2, &NOP3, &NOP2_NOP)])
        .unwrap();
    DENY_POLICY_VARS.store(true, Ordering::SeqCst);
    engine.undo_all().unwrap_err();
    assert_eq!(bytes_at(t.pid, region.start, 4), 5i32.to_le_bytes());
    drop(engine);
    // O desvio ficou (a política recusou): a variável também, e as duas no diário.
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP2_NOP);
    assert_eq!(bytes_at(t.pid, region.start, 4), 5i32.to_le_bytes());
    match env.store.peek().unwrap() {
        Loaded::Ok(j) => {
            assert_eq!(j.entries.len(), 1);
            assert_eq!(j.vars.len(), 1);
        }
        other => panic!("diário inesperado: {other:?}"),
    }
    // A próxima execução (política normal) desfaz o desvio e zera as variáveis.
    DENY_POLICY_VARS.store(false, Ordering::SeqCst);
    let report = recover(&env.store, &env.lock, &PauseConfig::default()).unwrap();
    assert_eq!(report.restored, vec!["p".to_string()], "{report:?}");
    assert!(report.kept.is_empty(), "{report:?}");
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
    assert_eq!(bytes_at(t.pid, region.start, 4), [0; 4]);
    assert!(missing(&env));
}

#[test]
fn recovery_zeros_vars_after_crash() {
    let t = TestTarget::spawn();
    let env = env();
    let region = vars_region(&t);
    let cave_addr = t.exec_page + 0x100;
    let mut engine = open_engine(
        t.pid,
        &env,
        vec![exec2_range(&t), page_range(&t, t.exec_page)],
        None,
    );
    engine.claim_vars(region.clone()).unwrap();
    engine.write_var(region.start + 24, &[3; 8]).unwrap();
    engine
        .apply(vec![
            patch("p", t.exec2, &NOP3, &NOP2_NOP),
            cave("c", cave_addr),
        ])
        .unwrap();
    // Simula o SIGKILL: o motor some sem desfazer nada, o diário fica.
    std::mem::forget(engine);
    assert_eq!(vars_in_journal(&env).len(), 1);
    let report = recover(&env.store, &env.lock, &PauseConfig::default()).unwrap();
    let mut restored = report.restored.clone();
    restored.sort();
    assert_eq!(
        restored,
        vec!["c".to_string(), "p".to_string()],
        "{report:?}"
    );
    assert!(report.kept.is_empty(), "{report:?}");
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
    assert_eq!(bytes_at(t.pid, cave_addr, 16), vec![0xCC; 16]);
    assert_eq!(bytes_at(t.pid, region.start, 280), vec![0u8; 280]);
    assert!(missing(&env));
}

#[test]
fn recovery_keeps_vars_when_region_is_not_rw_anymore() {
    let t = TestTarget::spawn();
    let env = env();
    // Diário de um processo igual, mas a região já não é `rw-` (página executável).
    let id = identity(t.pid).unwrap();
    env.store
        .save(&gt_core::journal::Journal {
            boot_id: id.boot_id,
            pid: id.pid,
            start_time: id.start_time,
            version_key: "teste".into(),
            paused_by_us: false,
            entries: Vec::new(),
            data: Vec::new(),
            vars: vec![gt_core::journal::VarRegion {
                addr: t.exec2,
                len: 64,
            }],
        })
        .unwrap();
    let report = recover(&env.store, &env.lock, &PauseConfig::default()).unwrap();
    assert_eq!(report.kept, vec!["variáveis".to_string()], "{report:?}");
    assert_eq!(bytes_at(t.pid, t.exec2, 4), [0x90; 4]);
    assert_eq!(
        vars_in_journal(&env),
        vec![gt_core::journal::VarRegion {
            addr: t.exec2,
            len: 64
        }]
    );
}

#[test]
fn clear_vars_keeps_region_while_code_pending() {
    let t = TestTarget::spawn();
    let env = env();
    let region = vars_region(&t);
    // A política só cobre a página da cave: o desvio fica fora e continua pendente.
    let mut engine = open_engine(t.pid, &env, vec![page_range(&t, t.exec_page)], None);
    engine.claim_vars(region.clone()).unwrap();
    engine.write_var(region.start, &5i32.to_le_bytes()).unwrap();
    let code = JournalEntry {
        id: "p".into(),
        addr: t.exec2,
        original: NOP3.to_vec(),
        replacement: NOP2_NOP.to_vec(),
        next: None,
        kind: gt_core::patch::PatchKind::Code,
    };
    let report = engine.restore_entries(vec![code]).unwrap();
    assert_eq!(report.outside_policy, vec!["p".to_string()]);
    engine.clear_vars().unwrap();
    assert_eq!(bytes_at(t.pid, region.start, 4), 5i32.to_le_bytes());
    assert_eq!(engine.vars().len(), 1);
    assert_eq!(vars_in_journal(&env).len(), 1);
}
