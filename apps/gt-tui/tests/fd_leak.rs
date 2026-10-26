#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]
//! O controlador não vaza descritores de arquivo ao reconectar (spec 003, 6.2): 50 ciclos de
//! conectar, matar o alvo e conectar a um alvo novo, com a implementação crua.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gt_grounded2::signatures::Category;
use gt_testkit::TestTarget;
use gt_tui::controller::Controller;
use gt_tui::model::{Conn, Model};
use gt_tui::raw::{RawBackend, RawControl, RawDef};

/// Quantos descritores o processo tem agora.
fn fd_count() -> usize {
    std::fs::read_dir("/proc/self/fd").unwrap().count()
}

/// O cheat de alternância do alvo (os endereços mudam a cada alvo, por ASLR).
fn defs_of(t: &TestTarget) -> Vec<RawDef> {
    vec![RawDef {
        id: "toggle".into(),
        name: "Alternar".into(),
        category: Category::Itens,
        addr: t.spin_page,
        expected: vec![0x48, 0x8B, 0xF9],
        replacement: vec![0x48, 0x89, 0xCF],
        value: None,
        verified: true,
    }]
}

#[test]
fn raw_50_reconnects_no_fd_leak() {
    let control = Arc::new(Mutex::new(RawControl {
        pid: None,
        defs: Vec::new(),
        version_key: "raw-1".into(),
        search_delay: Duration::ZERO,
        search_panics: false,
    }));
    let dir = tempfile::tempdir().unwrap();
    let backend = RawBackend::new(control.clone(), dir.path().to_path_buf()).unwrap();
    let mut c = Controller::new(backend, Box::new(|| "00:00".to_string()), false, false);
    let mut draw = |_: &Model| {};
    c.start(&mut draw);

    // A contagem inicial já inclui a trava do diário e o diretório temporário.
    let before = fd_count();
    let mut now = Instant::now();
    for cycle in 0..50 {
        let mut target = TestTarget::spawn();
        {
            let mut ctl = control.lock().unwrap();
            ctl.pid = Some(target.pid);
            ctl.defs = defs_of(&target);
        }
        // Passa dos 2 s da procura entre um ciclo e o outro.
        now += Duration::from_secs(3);
        c.tick(now, &mut draw);
        assert!(
            c.wait_search(now, Duration::from_secs(10)),
            "ciclo {cycle}: a busca não terminou"
        );
        assert!(
            matches!(c.model.conn, Conn::Connected { engine: true, .. }),
            "ciclo {cycle}: {:?}",
            c.model.conn
        );

        target.child.kill().unwrap();
        target.child.wait().unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        while !matches!(c.model.conn, Conn::Waiting) && Instant::now() < end {
            now += Duration::from_millis(100);
            c.tick(now, &mut draw);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            c.model.conn,
            Conn::Waiting,
            "ciclo {cycle}: não percebeu o fim"
        );
        control.lock().unwrap().pid = None;
    }
    let after = fd_count();
    assert!(
        after <= before + 2,
        "vazamento de descritores: {before} antes, {after} depois"
    );
}
