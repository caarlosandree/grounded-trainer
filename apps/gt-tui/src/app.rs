//! O laço da TUI e a saída (spec 003, 6.1, 6.8 e 6.10), separados do `main` para serem testados
//! com um terminal falso.

use std::cell::Cell;
use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::KeyEvent;
use gt_core::journal::JournalStore;
use gt_core::session::Shutdown;

use crate::backend::Backend;
use crate::controller::{Controller, LOOP_INTERVAL};
use crate::exit::{Summary, summarize};
use crate::keys::{Action, action};
use crate::model::Model;
use crate::term::{RestoreOnce, Term};
use crate::ui::too_small;

/// Como o laço terminou.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunEnd {
    /// Patches restaurados pelo `undo_all` da saída.
    pub restored: Vec<String>,
    /// Patches que outro processo alterou (não restaurados).
    pub foreign: Vec<String>,
    /// Pânico no laço, em outra thread ou na busca: a saída é pelo código 101.
    pub panicked: bool,
}

/// O `draw` que o controlador recebe: desenha pelo terminal, a não ser que ele já tenha sido
/// restaurado ou tenha sumido. A primeira falha de desenho marca `gone` (o laço sai em silêncio).
fn drawer<'a, T: Term>(
    term: &'a mut T,
    restore: &'a RestoreOnce,
    gone: &'a Cell<bool>,
) -> impl FnMut(&Model) + 'a {
    move |model| {
        if gone.get() || restore.done() {
            return;
        }
        if term.draw(model).is_err() {
            gone.set(true);
        }
    }
}

fn actions_of(keys: &[KeyEvent]) -> Vec<Action> {
    keys.iter().filter_map(action).collect()
}

/// Atende as ações em ordem. Depois de uma que chamou a sessão (`handle` devolveu `true`), as
/// teclas que sobraram do lote e as que chegaram durante a chamada passam por `after_op_keys`.
/// Devolve `false` se a leitura do terminal falhou.
fn handle_actions<T: Term, B: Backend>(
    term: &mut T,
    controller: &mut Controller<B>,
    restore: &RestoreOnce,
    gone: &Cell<bool>,
    first: Vec<Action>,
) -> bool {
    let mut queue: VecDeque<Action> = first.into();
    while let Some(act) = queue.pop_front() {
        let called = controller.handle(act, Instant::now(), &mut drawer(&mut *term, restore, gone));
        if controller.quit_requested() || gone.get() {
            return true;
        }
        if called {
            let mut rest: Vec<Action> = queue.drain(..).collect();
            match term.read_keys(Duration::ZERO) {
                Ok(keys) => rest.extend(actions_of(&keys)),
                Err(_) => return false,
            }
            queue.extend(controller.after_op_keys(rest));
        }
    }
    true
}

/// Se o laço deve parar.
fn should_stop<B: Backend>(
    controller: &Controller<B>,
    shutdown: &Shutdown,
    elsewhere: &AtomicBool,
    gone: &Cell<bool>,
) -> bool {
    controller.quit_requested()
        || shutdown.requested()
        || elsewhere.load(Ordering::SeqCst)
        || controller.search_panicked()
        || gone.get()
}

/// As voltas do laço, até uma condição de saída (ou um erro de leitura do terminal).
fn event_loop<T: Term, B: Backend>(
    term: &mut T,
    controller: &mut Controller<B>,
    shutdown: &Shutdown,
    restore: &RestoreOnce,
    elsewhere: &AtomicBool,
    gone: &Cell<bool>,
) {
    while !should_stop(controller, shutdown, elsewhere, gone) {
        match term.size() {
            Ok((width, height)) => controller.model.too_small = too_small(width, height),
            Err(_) => return,
        }
        let keys = match term.read_keys(LOOP_INTERVAL) {
            Ok(keys) => keys,
            Err(_) => return,
        };
        if !handle_actions(term, controller, restore, gone, actions_of(&keys)) {
            return;
        }
        if should_stop(controller, shutdown, elsewhere, gone) {
            return;
        }
        let called = controller.tick(Instant::now(), &mut drawer(&mut *term, restore, gone));
        if called {
            // Teclas que chegaram durante a chamada da sessão: só as que a 6.6 deixa passar.
            let keys = match term.read_keys(Duration::ZERO) {
                Ok(keys) => keys,
                Err(_) => return,
            };
            let kept = controller.after_op_keys(actions_of(&keys));
            if !handle_actions(term, controller, restore, gone, kept) {
                return;
            }
        }
        drawer(&mut *term, restore, gone)(&controller.model);
    }
}

/// O laço: lê teclas, atende, dá a volta do controlador e desenha, até `Quit`, sinal, pânico em
/// outra thread, pânico na busca, ou terminal perdido. Depois desfaz tudo (`shutdown`), mesmo
/// depois de um pânico no laço (só o laço fica sob `catch_unwind`; o controlador é de quem chama).
pub fn run<T: Term, B: Backend>(
    term: &mut T,
    controller: &mut Controller<B>,
    shutdown: &Shutdown,
    restore: &RestoreOnce,
    elsewhere: &AtomicBool,
) -> RunEnd {
    let gone = Cell::new(false);
    let looped = catch_unwind(AssertUnwindSafe(|| {
        event_loop(term, controller, shutdown, restore, elsewhere, &gone);
    }));
    let mut panicked = looped.is_err();
    let undone = catch_unwind(AssertUnwindSafe(|| {
        controller.shutdown(&mut drawer(&mut *term, restore, &gone))
    }));
    let (restored, foreign) = undone.unwrap_or_else(|_| {
        panicked = true;
        (Vec::new(), Vec::new())
    });
    panicked |= elsewhere.load(Ordering::SeqCst) || controller.search_panicked();
    RunEnd {
        restored,
        foreign,
        panicked,
    }
}

/// Fim: solta o controlador (o `Drop` do motor restaura o que sobrou), restaura o terminal
/// (uma vez só, erro ignorado) e monta o resumo pelo diário.
pub fn finish<T: Term, B: Backend>(
    term: &mut T,
    controller: Controller<B>,
    restore: &RestoreOnce,
    store: &JournalStore,
    end: RunEnd,
    logged: bool,
) -> Summary {
    drop(controller);
    let _ = restore.run(|| term.restore());
    summarize(store, &end.restored, &end.foreign, end.panicked, logged)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::VecDeque;
    use std::io;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use gt_core::journal::{Journal, JournalEntry};

    use super::*;
    use crate::backend::Find;
    use crate::fake::{Call, FakeBackend, FakeCtl, available, cheat};
    use crate::model::Model;

    type Script = Box<dyn FnMut(usize, Duration, Option<&Model>) -> io::Result<Vec<KeyEvent>>>;

    #[derive(Default)]
    struct TermLog {
        events: Vec<String>,
        drawn: Option<Model>,
        timeouts: Vec<Duration>,
    }

    struct FakeTerm {
        log: Arc<Mutex<TermLog>>,
        ctl: FakeCtl,
        size: (u16, u16),
        script: Script,
        reads: usize,
        draw_err: Option<i32>,
        restore_err: bool,
    }

    impl FakeTerm {
        fn new(ctl: &FakeCtl, script: Script) -> (FakeTerm, Arc<Mutex<TermLog>>) {
            let log = Arc::new(Mutex::new(TermLog::default()));
            let term = FakeTerm {
                log: Arc::clone(&log),
                ctl: ctl.clone(),
                size: (80, 24),
                script,
                reads: 0,
                draw_err: None,
                restore_err: false,
            };
            (term, log)
        }

        /// Terminal que não digita nada.
        fn idle(ctl: &FakeCtl) -> (FakeTerm, Arc<Mutex<TermLog>>) {
            FakeTerm::new(ctl, Box::new(|_, _, _| Ok(Vec::new())))
        }
    }

    impl Term for FakeTerm {
        fn size(&self) -> io::Result<(u16, u16)> {
            Ok(self.size)
        }

        fn draw(&mut self, model: &Model) -> io::Result<()> {
            let mut log = self.log.lock().unwrap();
            log.events
                .push(format!("draw:{}", model.busy.clone().unwrap_or_default()));
            if let Some(code) = self.draw_err {
                return Err(io::Error::from_raw_os_error(code));
            }
            log.drawn = Some(model.clone());
            Ok(())
        }

        fn read_keys(&mut self, timeout: Duration) -> io::Result<Vec<KeyEvent>> {
            self.reads += 1;
            // Rede de segurança: um roteiro que não termina vira erro de leitura.
            if self.reads > 3000 {
                return Err(io::Error::other("roteiro esgotado"));
            }
            std::thread::sleep(Duration::from_millis(1));
            let drawn = {
                let mut log = self.log.lock().unwrap();
                log.timeouts.push(timeout);
                log.drawn.clone()
            };
            (self.script)(self.reads, timeout, drawn.as_ref())
        }

        fn restore(&mut self) -> io::Result<()> {
            let dropped = self.ctl.count(|c| matches!(c, Call::SessionDropped));
            self.log
                .lock()
                .unwrap()
                .events
                .push(format!("restore:dropped={dropped}"));
            if self.restore_err {
                return Err(io::Error::from_raw_os_error(5));
            }
            Ok(())
        }
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn space() -> KeyEvent {
        key(' ')
    }

    fn events(log: &Arc<Mutex<TermLog>>) -> Vec<String> {
        log.lock().unwrap().events.clone()
    }

    /// Controlador já conectado a um jogo falso com o cheat `a` disponível.
    fn connected() -> (Controller<FakeBackend>, FakeCtl) {
        let backend = FakeBackend::new();
        let ctl = backend.ctl();
        ctl.set_catalog(vec![cheat("a", available())]);
        ctl.set_find(Find::Found(100));
        let mut c = Controller::new(backend, Box::new(|| "12:00".to_string()), false, false);
        let mut draw = |_: &Model| {};
        c.start(&mut draw);
        let now = Instant::now();
        c.tick(now, &mut draw);
        assert!(c.wait_search(now, Duration::from_secs(10)));
        assert!(
            matches!(c.model.conn, crate::model::Conn::Connected { .. }),
            "{:?}",
            c.model.conn
        );
        (c, ctl)
    }

    fn temp_store() -> (tempfile::TempDir, JournalStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = JournalStore::new(dir.path().to_path_buf());
        (dir, store)
    }

    fn run_with<T: Term>(
        term: &mut T,
        c: &mut Controller<FakeBackend>,
        shutdown: &Shutdown,
    ) -> RunEnd {
        let restore = RestoreOnce::new();
        let elsewhere = AtomicBool::new(false);
        run(term, c, shutdown, &restore, &elsewhere)
    }

    #[test]
    fn exit_restores_terminal_before_summary() {
        let (mut c, ctl) = connected();
        ctl.set_undo_all(vec!["a".to_string()], Vec::new());
        let (mut term, log) = FakeTerm::new(
            &ctl,
            Box::new(|n, _, _| Ok(if n == 2 { vec![key('q')] } else { Vec::new() })),
        );
        let restore = RestoreOnce::new();
        let elsewhere = AtomicBool::new(false);
        let (_dir, store) = temp_store();

        let end = run(&mut term, &mut c, &Shutdown::manual(), &restore, &elsewhere);
        assert_eq!(
            end,
            RunEnd {
                restored: vec!["a".to_string()],
                foreign: Vec::new(),
                panicked: false
            }
        );
        // O terminal só é restaurado dentro do `finish`.
        assert!(!events(&log).iter().any(|e| e.starts_with("restore")));
        // A saída mostrou `restaurando…` antes de desfazer.
        assert!(events(&log).contains(&"draw:restaurando…".to_string()));

        let summary = finish(&mut term, c, &restore, &store, end, true);
        let ev = events(&log);
        // Uma restauração só, depois do `drop` do controlador (a sessão já tinha caído), e
        // nenhum desenho depois dela.
        assert_eq!(ev.last().unwrap(), "restore:dropped=1", "{ev:?}");
        assert_eq!(ev.iter().filter(|e| e.starts_with("restore")).count(), 1);
        assert_eq!(summary.code, 0);
        assert_eq!(summary.lines, ["restaurado: a"]);
        assert_eq!(ctl.count(|c| matches!(c, Call::UndoAll { .. })), 1);
    }

    #[test]
    fn search_panic_gives_101() {
        let backend = FakeBackend::new();
        let ctl = backend.ctl();
        ctl.set_search_panics(true);
        ctl.set_find(Find::Found(100));
        let mut c = Controller::new(backend, Box::new(|| "12:00".to_string()), false, false);
        let (mut term, log) = FakeTerm::idle(&ctl);
        let restore = RestoreOnce::new();
        let elsewhere = AtomicBool::new(false);
        let (_dir, store) = temp_store();

        let end = run(&mut term, &mut c, &Shutdown::manual(), &restore, &elsewhere);
        assert!(end.panicked);
        let summary = finish(&mut term, c, &restore, &store, end, true);
        assert_eq!(summary.code, 101);
        assert_eq!(events(&log).last().unwrap(), "restore:dropped=0");
    }

    #[test]
    fn panic_in_loop_gives_101() {
        let (mut c, ctl) = connected();
        ctl.set_apply_panics(true);
        ctl.set_undo_all(vec!["x".to_string()], Vec::new());
        let (mut term, log) = FakeTerm::new(
            &ctl,
            Box::new(|n, _, _| Ok(if n == 1 { vec![space()] } else { Vec::new() })),
        );
        let restore = RestoreOnce::new();
        let elsewhere = AtomicBool::new(false);
        let (_dir, store) = temp_store();

        let end = run(&mut term, &mut c, &Shutdown::manual(), &restore, &elsewhere);
        assert!(end.panicked);
        // O `shutdown` rodou depois do pânico.
        assert_eq!(end.restored, ["x"]);
        assert_eq!(ctl.count(|c| matches!(c, Call::UndoAll { .. })), 1);
        let summary = finish(&mut term, c, &restore, &store, end, true);
        assert_eq!(summary.code, 101);
        assert_eq!(events(&log).last().unwrap(), "restore:dropped=1");
    }

    #[test]
    fn draw_eio_exits_quietly() {
        let (mut c, ctl) = connected();
        let (mut term, log) = FakeTerm::idle(&ctl);
        term.draw_err = Some(5);
        let restore = RestoreOnce::new();
        let elsewhere = AtomicBool::new(false);
        let (_dir, store) = temp_store();

        let end = run(&mut term, &mut c, &Shutdown::manual(), &restore, &elsewhere);
        assert!(!end.panicked);
        assert_eq!(ctl.count(|c| matches!(c, Call::UndoAll { .. })), 1);
        // Depois da primeira falha ninguém tenta desenhar de novo (nem o `restaurando…`).
        let draws = events(&log)
            .iter()
            .filter(|e| e.starts_with("draw"))
            .count();
        assert_eq!(draws, 1, "{:?}", events(&log));
        let summary = finish(&mut term, c, &restore, &store, end, true);
        assert_eq!(summary.code, 0);
    }

    #[test]
    fn restore_eio_keeps_code() {
        // Diário limpo: 0.
        let (c, ctl) = connected();
        let (mut term, _log) = FakeTerm::idle(&ctl);
        term.restore_err = true;
        let restore = RestoreOnce::new();
        let (_dir, store) = temp_store();
        let end = RunEnd {
            restored: Vec::new(),
            foreign: Vec::new(),
            panicked: false,
        };
        assert_eq!(finish(&mut term, c, &restore, &store, end, true).code, 0);

        // Com uma entrada no diário: 1.
        let (c, ctl) = connected();
        let (mut term, _log) = FakeTerm::idle(&ctl);
        term.restore_err = true;
        let restore = RestoreOnce::new();
        let (_dir, store) = temp_store();
        store
            .save(&Journal {
                boot_id: "b".into(),
                pid: 1,
                start_time: 1,
                version_key: "v".into(),
                paused_by_us: false,
                entries: vec![JournalEntry {
                    id: "god".into(),
                    addr: 0x10,
                    original: vec![1],
                    replacement: vec![2],
                    next: None,
                    kind: gt_core::patch::PatchKind::Code,
                }],
                data: Vec::new(),
                vars: Vec::new(),
            })
            .unwrap();
        let end = RunEnd {
            restored: Vec::new(),
            foreign: Vec::new(),
            panicked: false,
        };
        assert_eq!(finish(&mut term, c, &restore, &store, end, true).code, 1);
    }

    #[test]
    fn quit_and_shutdown_signal_exit() {
        // `Quit`.
        let (mut c, ctl) = connected();
        let (mut term, _log) = FakeTerm::new(&ctl, Box::new(|_, _, _| Ok(vec![key('q')])));
        let end = run_with(&mut term, &mut c, &Shutdown::manual());
        assert!(!end.panicked);
        assert!(c.quit_requested());
        assert_eq!(ctl.count(|c| matches!(c, Call::UndoAll { .. })), 1);

        // Sinal: sai sem esperar teclas.
        let (mut c, ctl) = connected();
        let (mut term, log) = FakeTerm::idle(&ctl);
        let shutdown = Shutdown::manual();
        shutdown.request();
        let end = run_with(&mut term, &mut c, &shutdown);
        assert!(!end.panicked);
        assert!(!c.quit_requested());
        assert_eq!(ctl.count(|c| matches!(c, Call::UndoAll { .. })), 1);
        assert_eq!(term.reads, 0);
        assert_eq!(events(&log), ["draw:restaurando…"]);
    }

    #[test]
    fn elsewhere_panic_exits_with_panicked() {
        let (mut c, ctl) = connected();
        let (mut term, _log) = FakeTerm::idle(&ctl);
        let restore = RestoreOnce::new();
        let elsewhere = AtomicBool::new(false);
        elsewhere.store(true, Ordering::SeqCst);
        let end = run(&mut term, &mut c, &Shutdown::manual(), &restore, &elsewhere);
        assert!(end.panicked);
        assert_eq!(ctl.count(|c| matches!(c, Call::UndoAll { .. })), 1);
    }

    #[test]
    fn read_error_exits_through_normal_path() {
        let (mut c, ctl) = connected();
        let (mut term, _log) = FakeTerm::new(
            &ctl,
            Box::new(|_, _, _| Err(io::Error::from_raw_os_error(5))),
        );
        let end = run_with(&mut term, &mut c, &Shutdown::manual());
        assert!(!end.panicked);
        assert_eq!(ctl.count(|c| matches!(c, Call::UndoAll { .. })), 1);
    }

    #[test]
    fn keys_after_op_filtered() {
        let (mut c, ctl) = connected();
        let queue: VecDeque<Vec<KeyEvent>> = VecDeque::from([
            // Lote: liga (chama a sessão); o segundo espaço do lote é descartado e o `q` vale.
            vec![space(), space(), key('q')],
        ]);
        let mut queue = queue;
        let (mut term, log) = FakeTerm::new(
            &ctl,
            Box::new(move |_, _, _| Ok(queue.pop_front().unwrap_or_default())),
        );
        run_with(&mut term, &mut c, &Shutdown::manual());
        assert_eq!(ctl.count(|c| matches!(c, Call::Apply { .. })), 1);
        assert_eq!(ctl.count(|c| matches!(c, Call::Undo { .. })), 0);
        assert!(c.quit_requested());
        let _ = log;
    }

    #[test]
    fn keys_read_after_op_are_filtered_too() {
        let (mut c, ctl) = connected();
        // 1ª leitura: liga. A leitura sem espera logo depois traz um espaço e o `q`.
        let (mut term, log) = FakeTerm::new(
            &ctl,
            Box::new(|n, timeout, _| {
                Ok(match n {
                    1 => vec![space()],
                    2 => {
                        assert_eq!(timeout, Duration::ZERO);
                        vec![space(), key('q')]
                    }
                    _ => Vec::new(),
                })
            }),
        );
        run_with(&mut term, &mut c, &Shutdown::manual());
        assert_eq!(ctl.count(|c| matches!(c, Call::Apply { .. })), 1);
        assert_eq!(ctl.count(|c| matches!(c, Call::Undo { .. })), 0);
        assert!(c.quit_requested());
        assert_eq!(
            log.lock().unwrap().timeouts[..2],
            [crate::controller::LOOP_INTERVAL, Duration::ZERO]
        );
    }

    #[test]
    fn small_terminal_sets_model_flag() {
        let (mut c, ctl) = connected();
        let (mut term, _log) = FakeTerm::new(
            &ctl,
            Box::new(|n, _, _| Ok(if n == 2 { vec![key('q')] } else { Vec::new() })),
        );
        term.size = (40, 10);
        run_with(&mut term, &mut c, &Shutdown::manual());
        assert!(c.model.too_small);
    }
}
