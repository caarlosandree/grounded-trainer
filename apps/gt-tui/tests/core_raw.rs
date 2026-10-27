#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]
//! `RawBackend` (spec 003, 5 e 8): a interface de alvo contra o `gt-testtarget`, com patches
//! crus. Os testes que criam alvos com `spin_value` se serializam: cada alvo gasta uma CPU, e
//! disputa de CPU estoura o prazo da pausa.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use gt_core::journal::{JournalStore, Loaded};
use gt_core::mem::{MemReader, MemWriter};
use gt_core::procinfo::identity;
use gt_core::value::{ValueGrid, ValueSpec, ValueType};
use gt_grounded2::signatures::Category;
use gt_testkit::{TargetOpts, TestTarget};
use gt_tui::backend::{Backend, Find, OpOutcome, Recovered, Session};
use gt_tui::controller::Controller;
use gt_tui::keys::Action;
use gt_tui::model::{Avail, Conn, Model};
use gt_tui::raw::{RawBackend, RawControl, RawDef};

static SERIAL: Mutex<()> = Mutex::new(());

const TOGGLE: &str = "toggle";
const VALUE: &str = "value";

fn grid() -> ValueGrid {
    ValueGrid {
        ty: ValueType::U32,
        min: 0.0,
        max: f64::from(0x3333_3333_u32),
        step: f64::from(0x1111_1111_u32),
        default_k: 2,
        unit: None,
        decimals: 0,
    }
}

/// A grade com o `slot` do imediato (bytes 1..5 do `value_page`).
fn spec() -> ValueSpec {
    ValueSpec {
        grid: grid(),
        slot: 1,
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
            value: Some(spec()),
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
        ue: None,
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
    assert_eq!(again.payload.defs().len(), 2);
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
        // O mesmo para as teclas: `q` durante a busca é atendido na hora.
        let started = Instant::now();
        let called = c.handle(Action::Quit, t0 + Duration::from_millis(100 * i), &mut draw);
        assert!(!called, "sair não chama a sessão");
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "handle(Quit) demorou {:?} durante a busca",
            started.elapsed()
        );
        assert!(c.quit_requested());
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

/// Posição do cheat `id` na lista mostrada.
fn select(c: &mut Controller<RawBackend>, id: &str) {
    c.model.selected = c.model.visible().iter().position(|r| r.id == id).unwrap();
}

#[test]
fn raw_connect_toggle_reconnect() {
    let e = env();
    let control = e.control.clone();
    let mut old = e.target;
    let old_pid = old.pid;
    let old_spin = old.spin_page;
    let mut c = Controller::new(e.backend, Box::new(|| "00:00".to_string()), false, false);
    let mut draw = |_: &Model| {};
    let t0 = Instant::now();
    c.start(&mut draw);
    c.tick(t0, &mut draw);
    assert!(c.wait_search(t0, Duration::from_secs(10)));
    assert!(matches!(c.model.conn, Conn::Connected { engine: true, .. }));

    // Liga o cheat de alternância: os bytes mudam no alvo e ele vira desejado.
    select(&mut c, TOGGLE);
    assert!(c.handle(Action::Toggle, t0, &mut draw));
    assert_eq!(read(old_pid, old_spin, 3), [0x48, 0x89, 0xCF]);
    assert!(c.model.cheats.iter().any(|r| r.id == TOGGLE && r.on));
    assert!(c.desired().contains_key(TOGGLE));

    // O alvo morre: "o jogo fechou", sem erro de escrita, e o cheat continua desejado.
    old.child.kill().unwrap();
    old.child.wait().unwrap();
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
    assert!(c.desired().contains_key(TOGGLE));
    assert!(c.model.cheats.iter().all(|r| !r.on));

    // Um alvo novo (outros endereços): o teste troca o PID e as definições.
    let new = TestTarget::spawn_with(TargetOpts {
        spin_value: true,
        ..Default::default()
    });
    {
        let mut ctl = control.lock().unwrap();
        ctl.pid = Some(new.pid);
        ctl.defs = defs_of(&new);
    }
    now += Duration::from_secs(3);
    c.tick(now, &mut draw);
    assert!(c.wait_search(now, Duration::from_secs(10)));
    assert!(matches!(c.model.conn, Conn::Connected { engine: true, .. }));
    // A religação vem no `tick` seguinte (é ele que recebe o `draw` do `religando…`).
    now += Duration::from_millis(100);
    c.tick(now, &mut draw);
    assert_eq!(read(new.pid, new.spin_page, 3), [0x48, 0x89, 0xCF]);
    assert!(c.model.cheats.iter().any(|r| r.id == TOGGLE && r.on));
    assert!(
        c.model
            .events
            .iter()
            .any(|l| l.contains("Alternar religado (pausa"))
    );

    // Sair restaura o alvo novo.
    drop(c);
    assert_eq!(read(new.pid, new.spin_page, 3), [0x48, 0x8B, 0xF9]);
}

#[test]
fn raw_panic_in_main_loop_restores() {
    let e = env();
    let pid = e.target.pid;
    let (spin_page, value_page) = (e.target.spin_page, e.target.value_page);
    let dir = e._dir.path().to_path_buf();
    let backend = e.backend;

    // Um pânico no laço principal, com dois cheats ligados: o `Drop` do controlador (e da
    // sessão, e do motor) desfaz tudo no desenrolar.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let mut c = Controller::new(backend, Box::new(|| "00:00".to_string()), false, false);
        let mut draw = |_: &Model| {};
        let t0 = Instant::now();
        c.start(&mut draw);
        c.tick(t0, &mut draw);
        assert!(c.wait_search(t0, Duration::from_secs(10)));
        for id in [TOGGLE, VALUE] {
            select(&mut c, id);
            assert!(c.handle(Action::Toggle, t0, &mut draw));
        }
        assert!(c.model.cheats.iter().all(|r| r.on));
        panic!("pânico de teste no laço principal");
    }));
    assert!(result.is_err());

    assert_eq!(read(pid, spin_page, 3), [0x48, 0x8B, 0xF9]);
    assert_eq!(read(pid, value_page, 5), [0xB8, 0x11, 0x11, 0x11, 0x11]);
    // Diário vazio: nada ficou registrado.
    match JournalStore::new(dir).peek().unwrap() {
        Loaded::Missing => {}
        Loaded::Ok(j) => assert!(j.entries.is_empty(), "restou: {:?}", j.entries),
        other => panic!("diário inesperado: {other:?}"),
    }
}

/// Conecta o controlador ao alvo do `env` (já com a busca terminada).
fn connect(c: &mut Controller<RawBackend>, now: Instant) {
    let mut draw = |_: &Model| {};
    c.start(&mut draw);
    c.tick(now, &mut draw);
    assert!(c.wait_search(now, Duration::from_secs(10)));
    assert!(matches!(c.model.conn, Conn::Connected { engine: true, .. }));
}

#[test]
fn raw_value_reapplied_after_reconnect() {
    let e = env();
    let control = e.control.clone();
    let mut old = e.target;
    let mut c = Controller::new(e.backend, Box::new(|| "00:00".to_string()), false, false);
    let mut draw = |_: &Model| {};
    let t0 = Instant::now();
    connect(&mut c, t0);

    // Liga com o valor padrão (`k = 2`) e troca para o fim da grade (`k = 3`).
    select(&mut c, VALUE);
    assert!(c.handle(Action::Toggle, t0, &mut draw));
    wait_word(&old, 0x2222_2222);
    assert!(c.handle(Action::Inc, t0, &mut draw));
    wait_word(&old, 0x3333_3333);
    let row = c.model.cheats.iter().find(|r| r.id == VALUE).unwrap();
    assert_eq!((row.k, row.applied_k), (3, Some(3)));
    let line = format!("Valor: {} → {}", grid().format(2), grid().format(3));
    assert!(c.model.events.iter().any(|l| l.ends_with(&line)));

    // O alvo morre e um novo abre: a religação leva o valor guardado.
    old.child.kill().unwrap();
    old.child.wait().unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    let mut now = t0;
    while !matches!(c.model.conn, Conn::Waiting) && Instant::now() < end {
        now += Duration::from_millis(100);
        c.tick(now, &mut draw);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(c.model.conn, Conn::Waiting);
    assert_eq!(c.desired().get(VALUE).map(|d| d.k), Some(3));

    let new = TestTarget::spawn_with(TargetOpts {
        spin_value: true,
        ..Default::default()
    });
    {
        let mut ctl = control.lock().unwrap();
        ctl.pid = Some(new.pid);
        ctl.defs = defs_of(&new);
    }
    now += Duration::from_secs(3);
    c.tick(now, &mut draw);
    assert!(c.wait_search(now, Duration::from_secs(10)));
    now += Duration::from_millis(100);
    assert!(c.tick(now, &mut draw), "a religação chamou a sessão");
    wait_word(&new, 0x3333_3333);
    drop(c);
    wait_word(&new, 0x1111_1111);
}

#[test]
fn raw_shutdown_restores() {
    let e = env();
    let pid = e.target.pid;
    let (spin_page, value_page) = (e.target.spin_page, e.target.value_page);
    let dir = e._dir.path().to_path_buf();
    let mut c = Controller::new(e.backend, Box::new(|| "00:00".to_string()), false, false);
    let mut draw = |_: &Model| {};
    let t0 = Instant::now();
    connect(&mut c, t0);
    for id in [TOGGLE, VALUE] {
        select(&mut c, id);
        assert!(c.handle(Action::Toggle, t0, &mut draw));
    }
    assert!(c.model.cheats.iter().all(|r| r.on));
    assert_eq!(read(pid, spin_page, 3), [0x48, 0x89, 0xCF]);

    // A saída desenha `restaurando…`, desfaz tudo e devolve o que restaurou.
    let mut seen_busy = Vec::new();
    let (mut restored, foreign) = c.shutdown(&mut |m: &Model| seen_busy.push(m.busy.clone()));
    restored.sort();
    assert_eq!(restored, [TOGGLE, VALUE]);
    assert!(foreign.is_empty());
    assert_eq!(seen_busy, [Some("restaurando…".to_string())]);
    assert_eq!(read(pid, spin_page, 3), [0x48, 0x8B, 0xF9]);

    drop(c);
    assert_eq!(read(pid, spin_page, 3), [0x48, 0x8B, 0xF9]);
    assert_eq!(read(pid, value_page, 5), [0xB8, 0x11, 0x11, 0x11, 0x11]);
    match JournalStore::new(dir).peek().unwrap() {
        Loaded::Missing => {}
        Loaded::Ok(j) => assert!(j.entries.is_empty(), "restou: {:?}", j.entries),
        other => panic!("diário inesperado: {other:?}"),
    }
}

#[test]
fn raw_search_panic_restores() {
    let e = env();
    let pid = e.target.pid;
    let spin_page = e.target.spin_page;
    let control = e.control.clone();
    let dir = e._dir.path().to_path_buf();
    let mut c = Controller::new(e.backend, Box::new(|| "00:00".to_string()), false, false);
    let mut draw = |_: &Model| {};
    let t0 = Instant::now();
    connect(&mut c, t0);
    select(&mut c, TOGGLE);
    assert!(c.handle(Action::Toggle, t0, &mut draw));
    assert_eq!(read(pid, spin_page, 3), [0x48, 0x89, 0xCF]);

    // O `r` dispara a nova busca, que entra em pânico na thread dela.
    control.lock().unwrap().search_panics = true;
    assert!(!c.handle(Action::Retry, t0, &mut draw));
    assert!(c.wait_search(t0, Duration::from_secs(10)));
    assert!(c.search_panicked());

    // A thread principal sai pelo caminho normal: desfaz e solta.
    let (restored, foreign) = c.shutdown(&mut draw);
    assert_eq!(restored, [TOGGLE]);
    assert!(foreign.is_empty());
    assert_eq!(read(pid, spin_page, 3), [0x48, 0x8B, 0xF9]);
    drop(c);
    assert_eq!(read(pid, spin_page, 3), [0x48, 0x8B, 0xF9]);
    match JournalStore::new(dir).peek().unwrap() {
        Loaded::Missing => {}
        Loaded::Ok(j) => assert!(j.entries.is_empty(), "restou: {:?}", j.entries),
        other => panic!("diário inesperado: {other:?}"),
    }
}

// ---- cheats de valor (spec 005, plano 005 tarefa 17) ----

use gt_testkit::ue::{DAMAGE_OFF, DIE_BYTE};
use gt_testkit::{UeAddrs, synth_signatures};
use gt_tui::model::PlayerLine;
use gt_tui::raw::RawUe;

/// Alvo com a reflexão sintética e o controlador ligado a ele.
struct UeEnv {
    target: TestTarget,
    ue: UeAddrs,
    c: Controller<RawBackend>,
    _dir: tempfile::TempDir,
    _guard: MutexGuard<'static, ()>,
}

fn ue_env() -> UeEnv {
    let guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let target = TestTarget::spawn_with(TargetOpts {
        ue: true,
        ..Default::default()
    });
    let ue = target.ue.unwrap();
    let (sigs, fields) = synth_signatures();
    let control = Arc::new(Mutex::new(RawControl {
        pid: Some(target.pid),
        defs: defs_of(&target),
        version_key: "raw-1".into(),
        search_delay: Duration::ZERO,
        search_panics: false,
        ue: Some(RawUe {
            gobjects: ue.gobjects,
            names: ue.names,
            sigs,
            fields,
        }),
    }));
    let dir = tempfile::tempdir().unwrap();
    let backend = RawBackend::new(control, dir.path().to_path_buf()).unwrap();
    let c = Controller::new(backend, Box::new(|| "00:00".to_string()), false, false);
    UeEnv {
        target,
        ue,
        c,
        _dir: dir,
        _guard: guard,
    }
}

impl UeEnv {
    /// Uma volta do laço em `now`, esperando o trabalho da tarefa de reflexão que ela iniciar.
    fn step(&mut self, now: Instant) {
        let mut draw = |_: &Model| {};
        self.c.tick(now, &mut draw);
        self.c.wait_job(now, Duration::from_secs(10));
        self.c.tick(now, &mut draw);
    }

    fn connect(&mut self, now: Instant) {
        let mut draw = |_: &Model| {};
        self.c.start(&mut draw);
        self.c.tick(now, &mut draw);
        assert!(self.c.wait_search(now, Duration::from_secs(10)));
        assert!(matches!(
            self.c.model.conn,
            Conn::Connected { engine: true, .. }
        ));
    }

    fn row(&self, id: &str) -> &gt_tui::model::CheatRow {
        self.c.model.cheats.iter().find(|r| r.id == id).unwrap()
    }

    fn toggle(&mut self, id: &str, now: Instant) {
        let index = self
            .c
            .model
            .visible()
            .iter()
            .position(|r| r.id == id)
            .unwrap();
        self.c.model.selected = index;
        let mut draw = |_: &Model| {};
        assert!(self.c.handle(Action::Toggle, now, &mut draw));
    }

    fn health(&self) -> u64 {
        let raw = read(
            self.target.pid,
            self.ue.pawn + u64::from(gt_testkit::ue::HEALTH_OFF),
            8,
        );
        u64::from_le_bytes(raw.try_into().unwrap())
    }

    fn damage(&self) -> f32 {
        let raw = read(self.target.pid, self.health() + u64::from(DAMAGE_OFF), 4);
        f32::from_le_bytes(raw.try_into().unwrap())
    }

    fn poke(&self, addr: u64, bytes: &[u8]) {
        MemWriter::open(self.target.pid)
            .unwrap()
            .write(addr, bytes)
            .unwrap();
    }
}

#[test]
fn raw_fields_checking_then_available() {
    let mut e = ue_env();
    let t0 = Instant::now();
    e.connect(t0);
    assert!(e.row("vida-infinita").field);
    assert_eq!(e.row("vida-infinita").avail, Avail::Checking);
    // O patch não espera a reflexão.
    assert_eq!(e.row(TOGGLE).avail, Avail::Available { verified: true });
    e.step(t0);
    for id in ["vida-infinita", "nao-morrer", "comida-infinita"] {
        assert_eq!(e.row(id).avail, Avail::Available { verified: true }, "{id}");
    }
}

#[test]
fn raw_lock_holds_value() {
    let mut e = ue_env();
    let t0 = Instant::now();
    e.connect(t0);
    e.step(t0);
    let damage = e.health() + u64::from(DAMAGE_OFF);
    e.poke(damage, &30f32.to_le_bytes());
    e.toggle("vida-infinita", t0);
    assert!(e.row("vida-infinita").on);
    // A procura do controller e, no ciclo seguinte, a trava.
    for i in 1..=3 {
        e.step(t0 + Duration::from_millis(100 * i));
    }
    assert_eq!(e.damage(), 0.0);
    assert_eq!(e.c.model.player, PlayerLine::Found);
    // O jogo volta a ferir: a trava zera de novo.
    e.poke(damage, &12f32.to_le_bytes());
    e.step(t0 + Duration::from_millis(500));
    assert_eq!(e.damage(), 0.0);
}

#[test]
fn raw_bits_restored_on_shutdown() {
    let mut e = ue_env();
    let t0 = Instant::now();
    e.connect(t0);
    e.step(t0);
    let die = e.health() + u64::from(DIE_BYTE);
    assert_eq!(read(e.target.pid, die, 1), [0x03]);
    e.toggle("nao-morrer", t0);
    for i in 1..=3 {
        e.step(t0 + Duration::from_millis(100 * i));
    }
    assert_eq!(read(e.target.pid, die, 1), [0x00]);
    let (restored, foreign) = e.c.shutdown(&mut |_: &Model| {});
    assert!(restored.contains(&"nao-morrer".to_string()), "{restored:?}");
    assert!(foreign.is_empty());
    assert_eq!(read(e.target.pid, die, 1), [0x03]);
}

#[test]
fn raw_absent_then_found_resumes() {
    let mut e = ue_env();
    let t0 = Instant::now();
    e.connect(t0);
    e.step(t0);
    e.toggle("vida-infinita", t0);
    for i in 1..=3 {
        e.step(t0 + Duration::from_millis(100 * i));
    }
    assert_eq!(e.c.model.player, PlayerLine::Found);
    let damage = e.health() + u64::from(DAMAGE_OFF);
    // Menu ou carregamento: `Pawn` nulo.
    e.poke(e.ue.pawn_field_addr, &0u64.to_le_bytes());
    e.poke(damage, &40f32.to_le_bytes());
    e.step(t0 + Duration::from_millis(400));
    assert_eq!(e.c.model.player, PlayerLine::NotFound);
    assert_eq!(
        e.row("vida-infinita").note,
        Some(gt_grounded2::fields::Note::Waiting)
    );
    assert_eq!(e.damage(), 40.0);
    // O personagem volta: retoma sozinho.
    e.poke(e.ue.pawn_field_addr, &e.ue.pawn.to_le_bytes());
    e.step(t0 + Duration::from_millis(500));
    assert_eq!(e.c.model.player, PlayerLine::Found);
    assert_eq!(e.row("vida-infinita").note, None);
    assert_eq!(e.damage(), 0.0);
    let events: Vec<&str> = e.c.model.events.iter().map(String::as_str).collect();
    assert_eq!(
        events
            .iter()
            .filter(|l| l.ends_with("personagem perdido"))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|l| l.ends_with("personagem encontrado"))
            .count(),
        2
    );
}

// ---- valor nos cheats de valor e a ação (spec 006, 6.5 e 6.6; plano 006 tarefa 9) ----

use gt_grounded2::fields::ActionOutcome;
use gt_testkit::ue::{CAPACITY_OFF, SCIENCE_OFF};
use gt_tui::backend::JobKind;

/// Sessão crua contra o alvo com a reflexão sintética, com o índice montado.
struct UeSession {
    target: TestTarget,
    ue: UeAddrs,
    s: <RawBackend as Backend>::Session,
    _backend: RawBackend,
    _dir: tempfile::TempDir,
    _guard: MutexGuard<'static, ()>,
}

fn ue_session(engine: bool) -> UeSession {
    let guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let target = TestTarget::spawn_with(TargetOpts {
        ue: true,
        ..Default::default()
    });
    let ue = target.ue.unwrap();
    let (sigs, fields) = synth_signatures();
    let control = Arc::new(Mutex::new(RawControl {
        pid: Some(target.pid),
        defs: defs_of(&target),
        version_key: "raw-1".into(),
        search_delay: Duration::ZERO,
        search_panics: false,
        ue: Some(RawUe {
            gobjects: ue.gobjects,
            names: ue.names,
            sigs,
            fields,
        }),
    }));
    let dir = tempfile::tempdir().unwrap();
    let mut backend = RawBackend::new(control, dir.path().to_path_buf()).unwrap();
    assert!(matches!(backend.recover(), Recovered::Clean { .. }));
    let search = (backend.search_fn())(target.pid, None, None).unwrap();
    let mut s = backend.open(search, engine).unwrap();
    run_job(&mut s, JobKind::Index);
    UeSession {
        target,
        ue,
        s,
        _backend: backend,
        _dir: dir,
        _guard: guard,
    }
}

/// Roda um trabalho da tarefa de reflexão nesta thread e junta o resultado na sessão.
fn run_job<S: Session<P>, P>(session: &mut S, kind: JobKind) {
    let job = session.reflection_job(kind).expect("trabalho disponível");
    let out = (job.run)();
    session.merge_reflection(out);
}

fn read_i32(pid: i32, addr: u64) -> i32 {
    i32::from_le_bytes(read(pid, addr, 4).try_into().unwrap())
}

#[test]
fn field_on_sets_desired_k() {
    let mut u = ue_session(true);
    run_job(&mut u.s, JobKind::Controller);
    let capacity = u.ue.hauling + u64::from(CAPACITY_OFF);
    // `k = 49` na grade 1..100 de passo 1 é o valor 50.
    assert!(is_done(&u.s.field_on("carga", 49)));
    u.s.field_tick(&["carga".to_string()], Instant::now());
    assert_eq!(read_i32(u.target.pid, capacity), 50);
    // Um `k` fora da grade fica no máximo dela.
    assert!(is_done(&u.s.field_on("carga", 5000)));
    u.s.field_tick(
        &["carga".to_string()],
        Instant::now() + Duration::from_secs(1),
    );
    assert_eq!(read_i32(u.target.pid, capacity), 100);
}

#[test]
fn set_value_field_updates_desired() {
    let mut u = ue_session(true);
    run_job(&mut u.s, JobKind::Controller);
    let capacity = u.ue.hauling + u64::from(CAPACITY_OFF);
    assert!(is_done(&u.s.field_on("carga", 49)));
    let t0 = Instant::now();
    u.s.field_tick(&["carga".to_string()], t0);
    assert_eq!(read_i32(u.target.pid, capacity), 50);
    // A troca só muda o desejado; a escrita vem no ciclo seguinte.
    assert!(is_done(&u.s.set_value("carga", 9)));
    assert_eq!(read_i32(u.target.pid, capacity), 50);
    u.s.field_tick(&["carga".to_string()], t0 + Duration::from_secs(1));
    assert_eq!(read_i32(u.target.pid, capacity), 10);
    // Cheat de valor sem grade e desconhecido continuam recusados.
    assert!(matches!(
        u.s.set_value("pulos-infinitos", 1),
        OpOutcome::Refused(_)
    ));
    assert!(matches!(
        u.s.set_value("nao-existe", 1),
        OpOutcome::Refused(_)
    ));
}

#[test]
fn action_refused_without_engine_or_stuck() {
    {
        let mut u = ue_session(false);
        assert!(!u.s.has_engine());
        assert_eq!(
            u.s.field_action("somar-ciencia", 9),
            ActionOutcome::Refused("sem motor".to_string())
        );
        assert!(!u.s.action_wants_controller());
    }
    // Uma linha que não é ação também é recusada.
    let mut u = ue_session(true);
    assert!(matches!(
        u.s.field_action("carga", 0),
        ActionOutcome::Refused(_)
    ));
    // Ligar uma ação não faz sentido.
    assert!(matches!(
        u.s.field_on("somar-ciencia", 0),
        OpOutcome::Refused(_)
    ));
}

#[test]
fn action_needs_controller_marks_search() {
    let mut u = ue_session(true);
    let science = u.ue.party + u64::from(SCIENCE_OFF);
    let start = read_i32(u.target.pid, science);
    // Sem controller: pede a procura e não escreve.
    assert_eq!(
        u.s.field_action("somar-ciencia", 9),
        ActionOutcome::NeedsController
    );
    assert!(u.s.action_wants_controller());
    assert_eq!(read_i32(u.target.pid, science), start);
    assert_eq!(u.s.take_controller_news(), None);
    // A procura termina e avisa uma única vez.
    run_job(&mut u.s, JobKind::Controller);
    assert_eq!(u.s.take_controller_news(), Some(true));
    assert_eq!(u.s.take_controller_news(), None);
    assert!(!u.s.action_wants_controller());
    // Agora a ação soma `100 × (9 + 1)`.
    assert_eq!(
        u.s.field_action("somar-ciencia", 9),
        ActionOutcome::Done {
            before: start,
            after: start + 1000
        }
    );
    assert_eq!(read_i32(u.target.pid, science), start + 1000);
}

/// Passa os ciclos de manutenção em `base + secs` e devolve o último relatório.
fn tick_at(
    u: &mut UeSession,
    ids: &[&str],
    base: Instant,
    secs: u64,
) -> gt_grounded2::fields::TickReport {
    let ids: Vec<String> = ids.iter().map(ToString::to_string).collect();
    u.s.field_tick(&ids, base + Duration::from_secs(secs))
}

fn poke_at(u: &UeSession, addr: u64, bytes: &[u8]) {
    MemWriter::open(u.target.pid)
        .unwrap()
        .write(addr, bytes)
        .unwrap();
}

fn note_of(report: &gt_grounded2::fields::TickReport, id: &str) -> Option<Note> {
    report
        .notes
        .iter()
        .find(|(i, _)| i.as_str() == id)
        .map(|(_, n)| *n)
}

use gt_grounded2::fields::Note;

#[test]
fn raw_pawn_null_over_30s_keeps_controller() {
    let mut u = ue_session(true);
    run_job(&mut u.s, JobKind::Controller);
    let t0 = Instant::now();
    let damage_at = |u: &UeSession| {
        let health = u64::from_le_bytes(
            read(
                u.target.pid,
                u.ue.pawn + u64::from(gt_testkit::ue::HEALTH_OFF),
                8,
            )
            .try_into()
            .unwrap(),
        );
        health + u64::from(DAMAGE_OFF)
    };
    let damage = damage_at(&u);
    assert!(is_done(&u.s.field_on("vida-infinita", 0)));
    let ids = ["vida-infinita"];
    let r = tick_at(&mut u, &ids, t0, 0);
    assert_eq!(note_of(&r, "vida-infinita"), None);
    // Menu ou carregamento: `Pawn` nulo; o ciclo marca o início da espera.
    poke_at(&u, u.ue.pawn_field_addr, &0u64.to_le_bytes());
    tick_at(&mut u, &ids, t0, 1);
    let r = tick_at(&mut u, &ids, t0, 31);
    assert_eq!(note_of(&r, "vida-infinita"), Some(Note::Waiting));
    tick_at(&mut u, &ids, t0, 32);
    // Passou de 30 s: a procura é pedida, mas o controller guardado não é descartado.
    assert!(u.s.needs_controller(t0 + Duration::from_secs(32)));
    // O personagem volta sem nenhum trabalho de controller: retoma no ciclo seguinte.
    poke_at(&u, u.ue.pawn_field_addr, &u.ue.pawn.to_le_bytes());
    poke_at(&u, damage, &25f32.to_le_bytes());
    let r = tick_at(&mut u, &ids, t0, 33);
    assert_eq!(note_of(&r, "vida-infinita"), None);
    assert_eq!(
        f32::from_le_bytes(read(u.target.pid, damage, 4).try_into().unwrap()),
        0.0
    );
}

#[test]
fn raw_pawn_null_over_30s_world_cheats_keep_writing() {
    use gt_testkit::ue::{AUTO_COMPLETE_OFF, ENEMY_DAMAGE_OFF, RECIPES_OFF};
    let mut u = ue_session(true);
    run_job(&mut u.s, JobKind::Controller);
    let t0 = Instant::now();
    assert!(is_done(&u.s.field_on("construir-sem-ingredientes", 0)));
    // `k = 5` na grade 0..1 de passo 0,05 é 0,25.
    assert!(is_done(&u.s.field_on("dano-recebido", 5)));
    assert!(is_done(&u.s.field_on("vida-infinita", 0)));
    let ids = [
        "construir-sem-ingredientes",
        "dano-recebido",
        "vida-infinita",
    ];
    tick_at(&mut u, &ids, t0, 0);
    poke_at(&u, u.ue.pawn_field_addr, &0u64.to_le_bytes());
    tick_at(&mut u, &ids, t0, 1);
    let settings = u.ue.settings;
    tick_at(&mut u, &ids, t0, 31);
    tick_at(&mut u, &ids, t0, 32);
    assert!(u.s.needs_controller(t0 + Duration::from_secs(32)));
    // Depois de passar dos 30 s, o jogo devolve as configurações: o controller guardado segue em
    // uso e elas voltam a ser forçadas no ciclo seguinte (o pawn continua nulo).
    poke_at(&u, settings + u64::from(RECIPES_OFF), &[1]);
    poke_at(
        &u,
        settings + u64::from(ENEMY_DAMAGE_OFF),
        &1f32.to_le_bytes(),
    );
    let r = tick_at(&mut u, &ids, t0, 33);
    assert_eq!(note_of(&r, "vida-infinita"), Some(Note::Waiting));
    assert_eq!(note_of(&r, "construir-sem-ingredientes"), None);
    assert_eq!(note_of(&r, "dano-recebido"), None);
    assert_eq!(
        read(u.target.pid, settings + u64::from(RECIPES_OFF), 1),
        [0]
    );
    assert_eq!(
        read(u.target.pid, settings + u64::from(AUTO_COMPLETE_OFF), 1),
        [1]
    );
    assert_eq!(
        f32::from_le_bytes(
            read(u.target.pid, settings + u64::from(ENEMY_DAMAGE_OFF), 4)
                .try_into()
                .unwrap()
        ),
        0.25
    );
}
