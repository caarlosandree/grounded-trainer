#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Portão da pausa no jogo real (spec 6.5, 6.12 e 7.1). Todos os testes são `#[ignore]`: exigem o
//! Grounded 2 aberto numa partida.
//!
//! ```text
//! cargo test -p gt-grounded2 --test game_pause -- --ignored --nocapture
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use gt_core::Error;
use gt_core::procinfo::{ThreadPos, is_stopped, list_tids, read_task_stat};
use gt_core::suspend::{PauseConfig, open_checked, pause, wait_until};
use gt_grounded2::resolve::Target;

/// Pausas do portão.
const PAUSES: u32 = 100;
/// Espera entre pausas.
const GAP: Duration = Duration::from_millis(300);
/// Novas tentativas por prazo permitidas no total (spec 6.5).
const MAX_RETRIES: u32 = 2;
/// Prazo para o jogo sair do estado parado depois do `SIGCONT`.
const RESUME_TIMEOUT: Duration = Duration::from_millis(500);
/// Quantos `pc` mais frequentes imprimir.
const TOP_PCS: usize = 20;

/// Estados (`/proc/<pid>/task/<tid>/stat`) das threads vivas, por tid.
fn thread_states(pid: i32) -> Vec<(i32, char)> {
    list_tids(pid)
        .expect("listar as threads do jogo")
        .into_iter()
        .filter_map(|tid| {
            read_task_stat(pid, tid)
                .expect("ler o stat da thread")
                .map(|stat| (tid, stat.state))
        })
        .collect()
}

/// `true` se nenhuma thread do jogo está parada (`T`/`t`).
fn none_stopped(pid: i32) -> gt_core::Result<bool> {
    let mut any = false;
    for tid in list_tids(pid)? {
        if let Some(stat) = read_task_stat(pid, tid)?
            && is_stopped(stat.state)
        {
            any = true;
        }
    }
    Ok(!any)
}

/// `módulo+offset` do endereço, ou só o endereço absoluto.
fn describe_pc(target: &Target, pc: u64) -> String {
    target
        .module_label(pc)
        .map_or_else(|| format!("{pc:#x}"), |label| format!("{pc:#x} ({label})"))
}

#[test]
#[ignore = "exige o Grounded 2 aberto"]
fn pause_gate() {
    let target = Target::open(None).expect("abrir o jogo (ele está aberto?)");
    let pid = target.pid;
    let (pidfd, _identity) = open_checked(pid).expect("abrir o pidfd do jogo");
    let cfg = PauseConfig::default();

    let mut retries = 0u32;
    let mut stop_times: Vec<Duration> = Vec::new();
    let mut d_tids: BTreeMap<i32, u32> = BTreeMap::new();
    let mut pc_counts: BTreeMap<u64, u32> = BTreeMap::new();
    let mut thread_total = 0usize;

    for round in 0..PAUSES {
        if round > 0 {
            std::thread::sleep(GAP);
        }

        // Threads em `D` (espera ininterruptível) antes de pausar: são as que atrasam o SIGSTOP.
        for (tid, state) in thread_states(pid) {
            if state == 'D' {
                *d_tids.entry(tid).or_default() += 1;
            }
        }

        let started = Instant::now();
        let guard = loop {
            match pause(&pidfd, pid, &cfg, true) {
                Ok(guard) => break guard,
                Err(Error::PauseTimeout { .. }) => {
                    retries += 1;
                    assert!(
                        retries <= MAX_RETRIES,
                        "pausa {round}: mais de {MAX_RETRIES} novas tentativas por prazo"
                    );
                    eprintln!("pausa {round}: prazo estourado, nova tentativa ({retries})");
                    std::thread::sleep(cfg.retry_wait);
                }
                Err(e) => panic!("pausa {round}: {e}"),
            }
        };
        stop_times.push(started.elapsed());

        assert!(
            !guard.threads().is_empty(),
            "pausa {round}: nenhuma thread viva"
        );
        for thread in guard.threads() {
            match thread.pos {
                Some(ThreadPos::Stopped { pc, .. }) => {
                    *pc_counts.entry(pc).or_default() += 1;
                }
                other => panic!(
                    "pausa {round}: posição ilegível na thread {}: {other:?}",
                    thread.tid
                ),
            }
        }
        thread_total += guard.threads().len();
        guard.resume().expect("retomar o jogo");

        let running = wait_until(
            || none_stopped(pid),
            RESUME_TIMEOUT,
            Duration::from_millis(5),
        )
        .expect("conferir a retomada");
        assert!(
            running,
            "pausa {round}: ainda há thread parada {RESUME_TIMEOUT:?} depois do SIGCONT"
        );
    }

    let min = stop_times.iter().min().copied().unwrap_or_default();
    let max = stop_times.iter().max().copied().unwrap_or_default();
    let mean = stop_times.iter().sum::<Duration>() / u32::try_from(stop_times.len()).unwrap_or(1);
    eprintln!("pausas: {PAUSES}, novas tentativas por prazo: {retries}");
    eprintln!(
        "threads por pausa (média): {}",
        thread_total / PAUSES as usize
    );
    eprintln!("tempo até parar: mín {min:?}, médio {mean:?}, máx {max:?}");

    let d_seen: BTreeSet<i32> = d_tids.keys().copied().collect();
    eprintln!("tids vistos em D antes da pausa: {d_seen:?}");
    for (tid, times) in &d_tids {
        eprintln!("  tid {tid}: {times} vez(es)");
    }

    let mut ranked: Vec<(u64, u32)> = pc_counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    eprintln!("{TOP_PCS} pc mais frequentes:");
    for (pc, count) in ranked.into_iter().take(TOP_PCS) {
        eprintln!("  {count:4}x  {}", describe_pc(&target, pc));
    }
}

#[test]
#[ignore = "exige o Grounded 2 aberto"]
fn size_of_image_matches_disk() {
    let target = Target::open(None).expect("abrir o jogo (ele está aberto?)");

    let mut trusted = 0usize;
    let mut mismatched = 0usize;
    for module in target.modules.iter().filter(|m| m.trusted) {
        trusted += 1;
        if module.disk_size != Some(module.memory_size) {
            mismatched += 1;
            eprintln!(
                "{}: memória {:#x}, disco {:?} ({})",
                module.name,
                module.memory_size,
                module.disk_size.map(|n| format!("{n:#x}")),
                module.path.display()
            );
        }
    }
    eprintln!("módulos confiáveis: {trusted}, com SizeOfImage diferente do disco: {mismatched}");
    assert_eq!(mismatched, 0);
}
