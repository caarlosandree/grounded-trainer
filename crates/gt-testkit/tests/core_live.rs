//! Testes de API viva do `gt-core` contra o processo-alvo de teste (nunca contra o jogo).

use gt_core::mem::{MemReader, MemWriter};
use gt_testkit::{TestTarget, hex_with_spaces, marker_bytes};

#[test]
fn reads_marker() {
    let t = TestTarget::spawn();
    let mut buf = [0u8; 32];
    MemReader::new(t.pid)
        .read_exact(t.marker, &mut buf)
        .unwrap();
    assert_eq!(buf, marker_bytes());
}
#[test]
fn scan_finds_marker_once_in_its_region() {
    let t = TestTarget::spawn();
    let maps = gt_core::maps::read_maps(t.pid).unwrap();
    let region = maps
        .iter()
        .find(|r| r.start <= t.marker && t.marker < r.end)
        .unwrap();
    let pat = gt_core::scan::Pattern::parse(&hex_with_spaces(&marker_bytes())).unwrap();
    let reader = MemReader::new(t.pid);
    let hits = gt_core::scan::scan_chunked(region.end - region.start, 1 << 20, &pat, |off, buf| {
        reader.read_exact(region.start + off, buf)
    })
    .unwrap();
    assert_eq!(hits, vec![t.marker - region.start]);
}
#[test]
fn raw_writer_changes_marker() {
    let t = TestTarget::spawn();
    MemWriter::open(t.pid)
        .unwrap()
        .write(t.marker, &[0x11; 4])
        .unwrap();
    let mut buf = [0u8; 4];
    MemReader::new(t.pid)
        .read_exact(t.marker, &mut buf)
        .unwrap();
    assert_eq!(buf, [0x11; 4]);
}
#[test]
fn lossy_read_over_hole_marks_only_the_hole() {
    let t = TestTarget::spawn();
    let r = MemReader::new(t.pid)
        .read_lossy(t.region_start, 3 * t.page as usize)
        .unwrap();
    assert_eq!(
        r.holes,
        vec![t.region_start + t.page..t.region_start + 2 * t.page]
    );
    assert_eq!(r.bytes.len(), 3 * t.page as usize);
}
#[test]
fn fully_unmapped_is_error() {
    let t = TestTarget::spawn();
    assert!(matches!(
        MemReader::new(t.pid).read_lossy(t.region_start + t.page, t.page as usize),
        Err(gt_core::Error::Unmapped { .. })
    ));
}
#[test]
fn read_past_u64_max_is_unmapped() {
    let t = TestTarget::spawn();
    assert!(matches!(
        MemReader::new(t.pid).read_lossy(u64::MAX - 15, 32),
        Err(gt_core::Error::Unmapped { .. })
    ));
}
#[test]
fn dead_process_is_gone_for_lossy_too() {
    let mut t = TestTarget::spawn();
    let pid = t.pid;
    t.child.kill().unwrap();
    t.child.wait().unwrap();
    assert!(matches!(
        MemReader::new(pid).read_lossy(t.marker, 32),
        Err(gt_core::Error::ProcessGone { .. })
    ));
}
#[test]
fn dead_process_is_gone() {
    let mut t = TestTarget::spawn();
    let pid = t.pid;
    t.child.kill().unwrap();
    t.child.wait().unwrap();
    let mut buf = [0u8; 1];
    assert!(matches!(
        MemReader::new(pid).read_exact(0x1000, &mut buf),
        Err(gt_core::Error::ProcessGone { .. })
    ));
}
#[test]
fn lossy_read_on_last_page_is_unmapped_without_panic() {
    let t = TestTarget::spawn();
    assert!(matches!(
        MemReader::new(t.pid).read_lossy(0xFFFF_FFFF_FFFF_F000, 16),
        Err(gt_core::Error::Unmapped { .. })
    ));
}
#[test]
fn writer_handles_more_than_one_mib() {
    let t = TestTarget::spawn();
    let data: Vec<u8> = (0..t.big_len).map(|i| (i % 251) as u8).collect();
    MemWriter::open(t.pid)
        .unwrap()
        .write(t.big_region, &data)
        .unwrap();
    let mut back = vec![0u8; t.big_len];
    MemReader::new(t.pid)
        .read_exact(t.big_region, &mut back)
        .unwrap();
    assert_eq!(back, data);
}

#[test]
fn writer_on_prot_none_page() {
    // Kernel de referência (Fedora 44, kernel 7.2, proc_mem.force_override = always): a escrita
    // forçada em página PROT_NONE privada é aceita (verificado em 2026-10-25).
    let t = TestTarget::spawn();
    let result = MemWriter::open(t.pid)
        .unwrap()
        .write(t.region_start + t.page, &[1, 2, 3]);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn writer_to_dead_process_is_gone() {
    let mut t = TestTarget::spawn();
    let pid = t.pid;
    let writer = MemWriter::open(pid).unwrap();
    t.child.kill().unwrap();
    t.child.wait().unwrap();
    assert!(matches!(
        writer.write(t.marker, &[1]),
        Err(gt_core::Error::ProcessGone { .. })
    ));
}

#[test]
fn scan_memory_finds_marker_once() {
    use gt_core::memscan::{Verdict, scan_memory};
    let t = TestTarget::spawn();
    let maps = gt_core::maps::read_maps(t.pid).unwrap();
    let region = maps
        .iter()
        .find(|r| r.start <= t.marker && t.marker < r.end)
        .unwrap();
    let pat = gt_core::scan::Pattern::parse(&hex_with_spaces(&marker_bytes())).unwrap();
    let range = region.start..region.end;
    let scans = scan_memory(&MemReader::new(t.pid), std::slice::from_ref(&range), &[pat]).unwrap();
    assert_eq!(scans.len(), 1);
    assert_eq!(scans[0].hits, vec![t.marker]);
    assert_eq!(scans[0].verdict, Verdict::Unique);
}

#[test]
fn scan_memory_skips_hole() {
    use gt_core::memscan::scan_memory;
    let t = TestTarget::spawn();
    // As páginas 0 e 2 ficam sem zeros; só a lacuna (página 1, PROT_NONE) "tem" zeros.
    let writer = MemWriter::open(t.pid).unwrap();
    let fill = vec![0xAAu8; t.page as usize];
    writer.write(t.region_start, &fill).unwrap();
    writer.write(t.region_start + 2 * t.page, &fill).unwrap();
    let pat = gt_core::scan::Pattern::parse("00 00 00 00").unwrap();
    let range = t.region_start..t.region_start + 3 * t.page;
    let scans = scan_memory(&MemReader::new(t.pid), std::slice::from_ref(&range), &[pat]).unwrap();
    assert!(scans[0].hits.is_empty(), "{:?}", scans[0].hits);
    assert_eq!(scans[0].unreadable_bytes, t.page);
}

#[test]
fn identity_and_pidfd_on_live_target() {
    use gt_core::Error;
    use gt_core::mem::Pidfd;
    use gt_core::procinfo::{self, Identity};
    use nix::sys::signal::Signal;
    use std::time::{Duration, Instant};

    // Espera (até 500 ms) todas as threads chegarem ao estado pedido.
    fn all_threads(pid: i32, want_stopped: bool) -> bool {
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            let tids = procinfo::list_tids(pid).unwrap();
            let ok = !tids.is_empty()
                && tids.iter().all(|&tid| {
                    matches!(
                        procinfo::read_task_stat(pid, tid).unwrap(),
                        Some(s) if procinfo::is_stopped(s.state) == want_stopped
                    )
                });
            if ok {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    let mut t = TestTarget::spawn();
    let id = procinfo::identity(t.pid).unwrap();
    assert_eq!(id.pid, t.pid);
    assert_eq!(id.boot_id, procinfo::boot_id().unwrap());
    procinfo::check_alive(&id).unwrap();
    let reused = Identity {
        start_time: id.start_time + 1,
        ..id.clone()
    };
    assert!(matches!(
        procinfo::check_alive(&reused),
        Err(Error::ProcessGone { .. })
    ));

    let pidfd = Pidfd::open(t.pid).unwrap();
    assert!(!pidfd.exited(Duration::ZERO).unwrap());
    pidfd.send(Signal::SIGSTOP).unwrap();
    assert!(all_threads(t.pid, true), "threads não pararam em 500 ms");
    pidfd.send(Signal::SIGCONT).unwrap();
    assert!(all_threads(t.pid, false), "threads não voltaram em 500 ms");

    t.child.kill().unwrap();
    t.child.wait().unwrap();
    assert!(matches!(
        procinfo::check_alive(&id),
        Err(Error::ProcessGone { .. })
    ));
    assert!(pidfd.exited(Duration::ZERO).unwrap());
    assert!(matches!(
        pidfd.send(Signal::SIGCONT),
        Err(Error::ProcessGone { .. })
    ));
}

#[test]
fn ignore_job_control_sets_sig_ign() {
    gt_core::mem::ignore_job_control().unwrap();
    // SigIgn em /proc/self/status: máscara com o bit (sinal - 1) ligado.
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let mask = status
        .lines()
        .find_map(|l| l.strip_prefix("SigIgn:"))
        .map(|h| u64::from_str_radix(h.trim(), 16).unwrap())
        .unwrap();
    // Números no Linux: SIGTSTP 20, SIGTTIN 21, SIGTTOU 22.
    for sig in [20u32, 21, 22] {
        assert!(mask & (1 << (sig - 1)) != 0, "sinal {sig} não ignorado");
    }
}
