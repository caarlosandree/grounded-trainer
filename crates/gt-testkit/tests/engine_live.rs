#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Testes de API viva do motor de patches contra o processo-alvo de teste (nunca contra o jogo).

use std::cell::Cell;
use std::ops::Range;
use std::os::unix::fs::PermissionsExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::time::Duration;

use gt_core::Error;
use gt_core::engine::{Engine, EngineOptions, PolicyFn};
use gt_core::journal::{JournalEntry, JournalLock, JournalStore, Loaded};
use gt_core::mem::{MemReader, MemWriter};
use gt_core::patch::{PatchSpec, Validated, WritePolicy, validate};
use gt_core::procinfo::{Identity, ThreadPos, identity, list_tids, read_stat, read_task_stat};
use gt_core::suspend::{
    HookPoint, PauseConfig, is_stopped_now, open_checked, pause, sample_pcs, wait_until,
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
    let t = TestTarget::spawn_with(TargetOpts { spin: true });
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
    let policy: PolicyFn = Box::new(move |_maps| WritePolicy::new(ranges.clone()));
    let opts = EngineOptions {
        version_key: "teste".into(),
        pause: PauseConfig {
            hook,
            ..PauseConfig::default()
        },
    };
    Engine::open(pid, None, env.store.clone(), &env.lock, policy, opts).unwrap()
}

fn patch(id: &str, addr: u64, expected: &[u8], replacement: &[u8]) -> Validated {
    validate(PatchSpec {
        id: id.into(),
        addr,
        expected: expected.to_vec(),
        replacement: replacement.to_vec(),
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
    };
    let report = engine.restore_entries(vec![entry]).unwrap();
    assert_eq!(report.restored, vec!["meio".to_string()]);
    assert!(report.dropped.is_empty() && report.foreign.is_empty());
    assert_eq!(bytes_at(t.pid, t.exec2, 4), [0x90; 4]);
    assert!(missing(&env));
}

#[test]
fn stress_1000_toggles_on_spinning_loop() {
    let t = TestTarget::spawn_with(TargetOpts { spin: true });
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
    let report = toggle(&mut engine, p.clone(), 20, Duration::ZERO, &shutdown).unwrap();
    assert_eq!(report.cycles, 20);
    assert!(report.pause_max >= report.pause_avg, "{report:?}");
    assert!(report.pause_max > Duration::ZERO, "{report:?}");
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
    assert!(engine.active().is_empty());
    assert!(missing(&env));

    shutdown.request();
    let report = toggle(&mut engine, p, 20, Duration::ZERO, &shutdown).unwrap();
    assert_eq!(report.cycles, 0);
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
    assert!(missing(&env));
}
