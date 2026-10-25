#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Testes de API viva do motor de patches contra o processo-alvo de teste (nunca contra o jogo).

use std::cell::Cell;
use std::time::Duration;

use gt_core::procinfo::{ThreadPos, list_tids, read_task_stat};
use gt_core::suspend::{PauseConfig, is_stopped_now, open_checked, pause, sample_pcs, wait_until};
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
