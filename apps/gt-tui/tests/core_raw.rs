#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]
//! `RawBackend` (spec 003, 5 e 8): a interface de alvo contra o `gt-testtarget`, com patches
//! crus. Os testes que criam alvos com `spin_value` se serializam: cada alvo gasta uma CPU, e
//! disputa de CPU estoura o prazo da pausa.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use gt_core::mem::{MemReader, MemWriter};
use gt_core::procinfo::identity;
use gt_core::value::{ValueSpec, ValueType};
use gt_grounded2::signatures::Category;
use gt_testkit::{TargetOpts, TestTarget};
use gt_tui::backend::{Backend, Find, OpOutcome, Recovered, Session};
use gt_tui::controller::Controller;
use gt_tui::model::{Avail, Conn, Model};
use gt_tui::raw::{RawBackend, RawControl, RawDef};

static SERIAL: Mutex<()> = Mutex::new(());

const TOGGLE: &str = "toggle";
const VALUE: &str = "value";

fn grid() -> ValueSpec {
    ValueSpec {
        ty: ValueType::U32,
        slot: 1,
        min: 0.0,
        max: f64::from(0x3333_3333_u32),
        step: f64::from(0x1111_1111_u32),
        default_k: 2,
        unit: None,
        decimals: 0,
    }
}

/// Os dois cheats do alvo: um de alternância (`spin_page`) e um de valor (`value_page`).
fn defs_of(t: &TestTarget) -> Vec<RawDef> {
    vec![
        RawDef {
            id: TOGGLE.into(),
            name: "Alternar".into(),
            category: Category::Itens,
            addr: t.spin_page,
            expected: vec![0x48, 0x8B, 0xF9],
            replacement: vec![0x48, 0x89, 0xCF],
            value: None,
            verified: true,
        },
        RawDef {
            id: VALUE.into(),
            name: "Valor".into(),
            category: Category::Movimento,
            addr: t.value_page,
            expected: vec![0xB8, 0x11, 0x11, 0x11, 0x11],
            replacement: vec![0xB8, 0x22, 0x22, 0x22, 0x22],
            value: Some(grid()),
            verified: true,
        },
    ]
}

struct Env {
    target: TestTarget,
    control: Arc<Mutex<RawControl>>,
    backend: RawBackend,
    _dir: tempfile::TempDir,
    // Último campo: solta depois do alvo e do backend.
    _guard: MutexGuard<'static, ()>,
}

fn env() -> Env {
    let guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let target = TestTarget::spawn_with(TargetOpts {
        spin_value: true,
        ..Default::default()
    });
    let control = Arc::new(Mutex::new(RawControl {
        pid: Some(target.pid),
        defs: defs_of(&target),
        version_key: "raw-1".into(),
        search_delay: Duration::ZERO,
        search_panics: false,
    }));
    let dir = tempfile::tempdir().unwrap();
    let backend = RawBackend::new(control.clone(), dir.path().to_path_buf()).unwrap();
    Env {
        target,
        control,
        backend,
        _dir: dir,
        _guard: guard,
    }
}

fn read(pid: i32, addr: u64, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    MemReader::new(pid).read_exact(addr, &mut buf).unwrap();
    buf
}

fn word(t: &TestTarget) -> u32 {
    u32::from_le_bytes(read(t.pid, t.value_word, 4).try_into().unwrap())
}

/// Espera o laço do alvo gravar `want` na palavra de valor.
fn wait_word(t: &TestTarget, want: u32) {
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        if word(t) == want {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        word(t),
        want,
        "o laço do alvo não passou a gravar o valor novo"
    );
}

fn is_done(o: &OpOutcome) -> bool {
    matches!(o, OpOutcome::Done(_))
}

#[test]
fn raw_search_and_toggle() {
    let mut e = env();
    let t = &e.target;
    assert_eq!(e.backend.find(), Find::Found(t.pid));
    assert!(matches!(
        e.backend.recover(),
        Recovered::Clean { lines, corrupt: None } if lines.is_empty()
    ));

    let search = (e.backend.search_fn())(t.pid, None, None).unwrap();
    assert_eq!(search.pid, t.pid);
    assert_eq!(search.identity, identity(t.pid).unwrap());
    assert_eq!(search.version_key, "raw-1");
    assert!(search.has_version_file);
    assert_eq!(search.disk, Ok(()));
    assert_eq!(search.ids, None);
    let ids: Vec<&str> = search.cheats.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, [TOGGLE, VALUE]);
    for c in &search.cheats {
        assert_eq!(c.avail, Avail::Available { verified: true });
    }
    assert_eq!(search.cheats[1].value, Some(grid()));

    let mut s = e.backend.open(search, true).unwrap();
    assert!(s.has_engine());
    assert!(!s.exited());

    assert!(is_done(&s.apply(&[(TOGGLE.into(), 0)])));
    assert_eq!(read(t.pid, t.spin_page, 3), [0x48, 0x89, 0xCF]);
    assert!(is_done(&s.undo(TOGGLE)));
    assert_eq!(read(t.pid, t.spin_page, 3), [0x48, 0x8B, 0xF9]);
    assert!(!s.is_stuck());
    assert!(s.pending_ids().is_empty());
}

#[test]
fn raw_set_value() {
    let mut e = env();
    let t = &e.target;
    let search = (e.backend.search_fn())(t.pid, None, None).unwrap();
    let mut s = e.backend.open(search, true).unwrap();

    // `k = 2` é o valor padrão: ligar muda os bytes.
    assert!(is_done(&s.apply(&[(VALUE.into(), 2)])));
    wait_word(t, 0x2222_2222);
    assert!(is_done(&s.set_value(VALUE, 3)));
    wait_word(t, 0x3333_3333);
    assert!(is_done(&s.undo(VALUE)));
    wait_word(t, 0x1111_1111);
}

#[test]
fn raw_exited_after_kill() {
    let mut e = env();
    let search = (e.backend.search_fn())(e.target.pid, None, None).unwrap();
    let mut s = e.backend.open(search, true).unwrap();
    assert!(is_done(&s.apply(&[(TOGGLE.into(), 0)])));
    assert!(!s.exited());

    e.target.child.kill().unwrap();
    e.target.child.wait().unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    while !s.exited() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(s.exited());
    // Sem escrita: o motor descarta o registro.
    s.discard_gone();
    assert_eq!(s.apply(&[(VALUE.into(), 2)]), OpOutcome::Gone);
    assert_eq!(e.backend.find(), Find::NotRunning);
}

#[test]
fn raw_merge_search_updates_only_listed() {
    let mut e = env();
    let t = &e.target;
    // O `toggle` começa com uma substituição diferente (`mov rcx, rdi`).
    e.control.lock().unwrap().defs[0].replacement = vec![0x48, 0x89, 0xF9];
    let first = (e.backend.search_fn())(t.pid, None, None).unwrap();
    let identity = first.identity.clone();
    let mut s = e.backend.open(first, true).unwrap();

    // A nova busca vê as duas definições mudadas, mas só o `toggle` é pedido.
    {
        let mut c = e.control.lock().unwrap();
        c.defs[0].replacement = vec![0x48, 0x89, 0xCF];
        // O `ValueSpec` sobrescreve o slot (bytes 1..5); o primeiro byte ele não mascara.
        c.defs[1].replacement = vec![0xB9, 0x44, 0x44, 0x44, 0x44];
    }
    let listed =
        (e.backend.search_fn())(t.pid, Some(vec![TOGGLE.into()]), Some(identity.clone())).unwrap();
    assert_eq!(listed.ids, Some(vec![TOGGLE.to_string()]));
    assert_eq!(listed.cheats.len(), 1);
    assert_eq!(listed.cheats[0].id, TOGGLE);
    // O `merge_search` precisa respeitar `ids` por conta própria: este resultado leva as duas
    // definições novas no `payload`, mas lista só o `toggle` em `ids`.
    let mut again = (e.backend.search_fn())(t.pid, None, Some(identity)).unwrap();
    assert_eq!(again.payload.len(), 2);
    again.ids = listed.ids;
    assert!(s.merge_search(again));

    assert!(is_done(&s.apply(&[(TOGGLE.into(), 0)])));
    assert_eq!(read(t.pid, t.spin_page, 3), [0x48, 0x89, 0xCF]);
    assert!(is_done(&s.apply(&[(VALUE.into(), 2)])));
    // O `value` não foi listado: continua com a definição da primeira busca (`B8`, não `B9`).
    assert_eq!(read(t.pid, t.value_page, 5), [0xB8, 0x22, 0x22, 0x22, 0x22]);
}

#[test]
fn raw_merge_search_keeps_active_definition() {
    let mut e = env();
    let t = &e.target;
    let first = (e.backend.search_fn())(t.pid, None, None).unwrap();
    let identity = first.identity.clone();
    let mut s = e.backend.open(first, true).unwrap();
    assert!(is_done(&s.apply(&[(TOGGLE.into(), 0)])));
    assert_eq!(read(t.pid, t.spin_page, 3), [0x48, 0x89, 0xCF]);

    // Uma busca lista o `toggle`, que está ativo, com uma substituição nova.
    e.control.lock().unwrap().defs[0].replacement = vec![0x48, 0x89, 0xF9];
    let again = (e.backend.search_fn())(t.pid, Some(vec![TOGGLE.into()]), Some(identity)).unwrap();
    assert!(s.merge_search(again));

    // O ativo não é tocado: o `undo` restaura os bytes certos...
    assert!(is_done(&s.undo(TOGGLE)));
    assert_eq!(read(t.pid, t.spin_page, 3), [0x48, 0x8B, 0xF9]);
    // ...e a definição guardada continua a antiga (ligar de novo escreve `48 89 CF`).
    assert!(is_done(&s.apply(&[(TOGGLE.into(), 0)])));
    assert_eq!(read(t.pid, t.spin_page, 3), [0x48, 0x89, 0xCF]);
}

#[test]
fn raw_outcomes_foreign_and_refused() {
    let mut e = env();
    let t = &e.target;
    let search = (e.backend.search_fn())(t.pid, None, None).unwrap();
    let mut s = e.backend.open(search, true).unwrap();

    // Trocar o valor de quem não está ligado, ou de um cheat sem valor, é recusado.
    assert!(matches!(s.set_value(VALUE, 1), OpOutcome::Refused(_)));
    assert!(matches!(s.set_value(TOGGLE, 1), OpOutcome::Refused(_)));
    assert!(matches!(
        s.set_value("nao-existe", 1),
        OpOutcome::Refused(_)
    ));
    assert!(matches!(
        s.apply(&[("nao-existe".into(), 0)]),
        OpOutcome::Refused(_)
    ));

    // Bytes trocados por fora antes do `undo`: não são nossos.
    assert!(is_done(&s.apply(&[(TOGGLE.into(), 0)])));
    MemWriter::open(t.pid)
        .unwrap()
        .write(t.spin_page, &[0x90, 0x90, 0x90])
        .unwrap();
    assert_eq!(s.undo(TOGGLE), OpOutcome::Foreign);
    assert_eq!(read(t.pid, t.spin_page, 3), [0x90, 0x90, 0x90]);
    assert!(s.pending_ids().is_empty());
}

#[test]
fn raw_research_other_identity_is_gone() {
    let mut e = env();
    let t = &e.target;
    let search = (e.backend.search_fn())(t.pid, None, None).unwrap();
    let old_identity = search.identity.clone();
    let mut s = e.backend.open(search, false).unwrap();
    assert!(!s.has_engine());

    // Um alvo novo, com defs próprias (os endereços mudam a cada alvo, por ASLR).
    let other = TestTarget::spawn();
    {
        let mut c = e.control.lock().unwrap();
        c.pid = Some(other.pid);
        c.defs = defs_of(&other);
    }
    let search_fn = e.backend.search_fn();
    // A identidade do alvo anterior não vale para o processo novo.
    assert!(matches!(
        search_fn(
            other.pid,
            Some(vec![TOGGLE.into()]),
            Some(old_identity.clone())
        ),
        Err(gt_tui::backend::SearchError::Gone)
    ));
    // A mesma conta com o alvo anterior já morto.
    let dead_pid = t.pid;
    e.target.child.kill().unwrap();
    e.target.child.wait().unwrap();
    assert!(matches!(
        search_fn(dead_pid, Some(vec![]), Some(old_identity)),
        Err(gt_tui::backend::SearchError::Gone)
    ));

    // Uma busca de outro processo não entra na sessão.
    let foreign = search_fn(other.pid, None, None).unwrap();
    assert!(!s.merge_search(foreign));
}

#[test]
fn raw_search_checks_bytes_and_honours_control() {
    let mut e = env();
    let t = &e.target;
    let search_fn = e.backend.search_fn();

    // Bytes diferentes dos esperados e de qualquer ponto da grade.
    e.control.lock().unwrap().defs[0].expected = vec![0x90, 0x90, 0x90];
    let s = search_fn(t.pid, None, None).unwrap();
    assert_eq!(
        s.cheats[0].avail,
        Avail::Unavailable("bytes diferentes".into())
    );
    assert_eq!(s.cheats[1].avail, Avail::Available { verified: true });

    // Cheat de valor já ligado em outro ponto da grade continua disponível.
    let mut sess = e.backend.open(s, true).unwrap();
    assert!(is_done(&sess.apply(&[(VALUE.into(), 1)])));
    let s = search_fn(t.pid, Some(vec![VALUE.into()]), None).unwrap();
    assert_eq!(s.cheats[0].avail, Avail::Available { verified: true });

    // `search_delay` e `search_panics`.
    e.control.lock().unwrap().search_delay = Duration::from_millis(150);
    let t0 = Instant::now();
    search_fn(t.pid, None, None).unwrap();
    assert!(t0.elapsed() >= Duration::from_millis(150));
    e.control.lock().unwrap().search_panics = true;
    let f = search_fn.clone();
    let pid = t.pid;
    assert!(
        std::thread::spawn(move || f(pid, None, None))
            .join()
            .is_err()
    );
}

#[test]
fn raw_backend_takes_the_journal_lock() {
    let e = env();
    // Uma segunda instância no mesmo diretório recusa.
    let second = RawBackend::new(e.control.clone(), e._dir.path().to_path_buf());
    assert!(matches!(second, Err(gt_core::Error::AlreadyRunning)));
}

#[test]
fn raw_drop_order_keeps_the_lock_until_the_session_falls() {
    let mut e = env();
    let t = &e.target;
    let search = (e.backend.search_fn())(t.pid, None, None).unwrap();
    let mut s = e.backend.open(search, true).unwrap();
    assert!(is_done(&s.apply(&[(TOGGLE.into(), 0)])));
    assert_eq!(read(t.pid, t.spin_page, 3), [0x48, 0x89, 0xCF]);

    // Sem o backend, a sessão ainda segura a trava (o `Arc`).
    drop(e.backend);
    let second = RawBackend::new(e.control.clone(), e._dir.path().to_path_buf());
    assert!(matches!(second, Err(gt_core::Error::AlreadyRunning)));

    // A sessão cai: o motor restaura o patch e só depois a trava é solta.
    drop(s);
    assert_eq!(
        read(e.target.pid, e.target.spin_page, 3),
        [0x48, 0x8B, 0xF9]
    );
    assert!(RawBackend::new(e.control.clone(), e._dir.path().to_path_buf()).is_ok());
}

#[test]
#[ignore = "Tarefa 12"]
fn raw_slow_search_responsive() {
    let e = env();
    e.control.lock().unwrap().search_delay = Duration::from_secs(3);
    let mut c = Controller::new(e.backend, Box::new(|| "00:00".to_string()), false, false);
    let mut draw = |_: &Model| {};
    let t0 = Instant::now();
    c.start(&mut draw);

    // A busca dorme 3 s na thread; o laço continua respondendo (limite da spec: 250 ms, aqui
    // o dobro por folga em máquina carregada).
    c.tick(t0, &mut draw);
    assert_eq!(c.model.conn, Conn::Connecting);
    for i in 1..=5 {
        let started = Instant::now();
        c.tick(t0 + Duration::from_millis(100 * i), &mut draw);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "tick demorou {:?} durante a busca",
            started.elapsed()
        );
        assert_eq!(c.model.conn, Conn::Connecting);
        // Tarefa 12: o mesmo para `c.handle(Action::Quit, ..)`.
        std::thread::sleep(Duration::from_millis(100));
    }

    assert!(c.wait_search(t0 + Duration::from_secs(4), Duration::from_secs(10)));
    assert!(matches!(c.model.conn, Conn::Connected { engine: true, .. }));
}

#[test]
fn raw_controller_connects_and_detects_close() {
    let e = env();
    let t_pid = e.target.pid;
    let mut c = Controller::new(e.backend, Box::new(|| "00:00".to_string()), false, false);
    let mut draw = |_: &Model| {};
    let t0 = Instant::now();
    c.start(&mut draw);

    c.tick(t0, &mut draw);
    assert!(c.wait_search(t0, Duration::from_secs(10)));
    let Conn::Connected { pid, engine, .. } = &c.model.conn else {
        panic!("esperava conectado: {:?}", c.model.conn);
    };
    assert_eq!((*pid, *engine), (t_pid, true));
    let ids: Vec<&str> = c.model.cheats.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, [TOGGLE, VALUE]);
    assert!(
        c.model
            .cheats
            .iter()
            .all(|r| r.avail == Avail::Available { verified: true })
    );

    // O alvo morre: a TUI percebe sem nenhuma operação em andamento.
    let mut e_target = e.target;
    e_target.child.kill().unwrap();
    e_target.child.wait().unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    let mut now = t0;
    while !matches!(c.model.conn, Conn::Waiting) && Instant::now() < end {
        now += Duration::from_millis(100);
        c.tick(now, &mut draw);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(c.model.conn, Conn::Waiting);
    assert!(
        c.model
            .events
            .back()
            .is_some_and(|l| l.ends_with("o jogo fechou"))
    );
}
