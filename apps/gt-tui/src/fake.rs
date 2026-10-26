//! Alvo falso em memória para os testes do controlador (spec 003, 8).
//!
//! `FakeBackend` e `FakeSession` seguem a interface de alvo sem processo nenhum. O teste guarda
//! um [`FakeCtl`] (compartilhado com o backend, as sessões e a thread de busca) para programar
//! as respostas, ler o registro de chamadas e ver o último `Model` desenhado.
//!
//! Reusado pelas tarefas seguintes do controlador e pela tela: nem tudo é usado desde já.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use gt_core::procinfo::Identity;
use gt_grounded2::signatures::Category;

use crate::backend::{
    Backend, CheatInfo, Find, OpOutcome, Recovered, Search, SearchError, SearchFn, Session,
    TuiError,
};
use crate::model::{Avail, Model};

/// Uma chamada ao alvo falso. As que podem demorar levam o `busy` do último `Model`
/// desenhado, para o teste conferir que a tela mostrou o que estava sendo feito.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Call {
    Find,
    Recover {
        busy: Option<String>,
    },
    Search {
        pid: i32,
        ids: Option<Vec<String>>,
        expected: Option<Identity>,
    },
    Open {
        engine: bool,
    },
    OpenEngine,
    MergeSearch {
        ids: Option<Vec<String>>,
    },
    Apply {
        items: Vec<(String, u64)>,
        busy: Option<String>,
    },
    Undo {
        id: String,
        busy: Option<String>,
    },
    SetValue {
        id: String,
        k: u64,
        busy: Option<String>,
    },
    RestorePending {
        busy: Option<String>,
    },
    RetryResume {
        busy: Option<String>,
    },
    DiscardGone,
    UndoAll {
        busy: Option<String>,
    },
    SessionDropped,
}

/// O que uma busca devolve (a parte que o teste programa).
#[derive(Debug, Clone)]
pub(crate) struct SearchSpec {
    pub cheats: Vec<CheatInfo>,
    pub took: Duration,
    pub disk: Result<(), String>,
    pub version_key: String,
    pub has_version_file: bool,
}

impl SearchSpec {
    pub fn new(cheats: Vec<CheatInfo>) -> SearchSpec {
        SearchSpec {
            cheats,
            took: Duration::from_millis(10),
            disk: Ok(()),
            version_key: "v1".to_string(),
            has_version_file: true,
        }
    }
}

/// Cheat de teste: categoria `Itens`, sem valor, nome igual ao `id`.
pub(crate) fn cheat(id: &str, avail: Avail) -> CheatInfo {
    CheatInfo {
        id: id.to_string(),
        name: id.to_string(),
        category: Category::Itens,
        value: None,
        avail,
    }
}

/// Disponível e verificado.
pub(crate) fn available() -> Avail {
    Avail::Available { verified: true }
}

/// Indisponível com o motivo.
pub(crate) fn unavailable(reason: &str) -> Avail {
    Avail::Unavailable(reason.to_string())
}

/// Prazo máximo de uma busca presa por [`FakeCtl::hold_search`], para um teste que esquece de
/// soltar não travar a suíte.
const MAX_HOLD: Duration = Duration::from_secs(20);

pub(crate) struct FakeState {
    pub find: Find,
    pub recover: VecDeque<Recovered>,
    pub calls: Vec<Call>,
    pub drawn: Option<Model>,
    /// Respostas programadas das próximas buscas; vazia, vale `default_search`.
    pub search_queue: VecDeque<Result<SearchSpec, SearchError>>,
    pub default_search: SearchSpec,
    pub search_delay: Duration,
    pub search_hold: bool,
    pub search_panics: bool,
    /// Início do processo falso: muda para simular um processo novo no mesmo PID.
    pub start_time: u64,
    pub open_error: Option<String>,
    pub open_engine_error: Option<String>,
    pub merge_result: bool,
    pub exited: bool,
    pub stuck: bool,
    pub pending: Vec<String>,
    /// Desfechos programados; cada um leva, se houver, os `id` pendentes que a operação deixa.
    pub outcomes: VecDeque<(OpOutcome, Option<Vec<String>>)>,
    pub undo_all: (Vec<String>, Vec<String>),
}

fn lock(state: &Mutex<FakeState>) -> MutexGuard<'_, FakeState> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Controle compartilhado do alvo falso.
#[derive(Clone)]
pub(crate) struct FakeCtl(Arc<Mutex<FakeState>>);

impl FakeCtl {
    fn state(&self) -> MutexGuard<'_, FakeState> {
        lock(&self.0)
    }

    /// Função para o `draw` do controlador: guarda uma cópia do `Model` desenhado.
    pub fn drawer(&self) -> Box<dyn FnMut(&Model)> {
        let ctl = self.clone();
        Box::new(move |m| ctl.state().drawn = Some(m.clone()))
    }

    /// O último `Model` desenhado.
    pub fn drawn(&self) -> Option<Model> {
        self.state().drawn.clone()
    }

    fn drawn_busy(state: &FakeState) -> Option<String> {
        state.drawn.as_ref().and_then(|m| m.busy.clone())
    }

    pub fn set_find(&self, find: Find) {
        self.state().find = find;
    }

    pub fn push_recover(&self, r: Recovered) {
        self.state().recover.push_back(r);
    }

    /// Catálogo devolvido quando não há resposta programada.
    pub fn set_catalog(&self, cheats: Vec<CheatInfo>) {
        self.state().default_search.cheats = cheats;
    }

    pub fn set_default_search(&self, spec: SearchSpec) {
        self.state().default_search = spec;
    }

    /// Programa a próxima busca (as respostas saem na ordem).
    pub fn push_search(&self, res: Result<SearchSpec, SearchError>) {
        self.state().search_queue.push_back(res);
    }

    pub fn set_search_delay(&self, delay: Duration) {
        self.state().search_delay = delay;
    }

    pub fn set_search_panics(&self, panics: bool) {
        self.state().search_panics = panics;
    }

    /// Prende as buscas que começarem até [`FakeCtl::release_search`].
    pub fn hold_search(&self) {
        self.state().search_hold = true;
    }

    pub fn release_search(&self) {
        self.state().search_hold = false;
    }

    pub fn set_start_time(&self, start_time: u64) {
        self.state().start_time = start_time;
    }

    pub fn set_open_error(&self, e: Option<&str>) {
        self.state().open_error = e.map(str::to_string);
    }

    pub fn set_open_engine_error(&self, e: Option<&str>) {
        self.state().open_engine_error = e.map(str::to_string);
    }

    pub fn set_merge_result(&self, ok: bool) {
        self.state().merge_result = ok;
    }

    pub fn set_exited(&self, exited: bool) {
        self.state().exited = exited;
    }

    pub fn set_stuck(&self, stuck: bool) {
        self.state().stuck = stuck;
    }

    pub fn set_pending(&self, ids: &[&str]) {
        self.state().pending = ids.iter().map(|s| s.to_string()).collect();
    }

    /// Programa o desfecho da próxima operação da sessão (vazio, vale `Done(1 ms)`).
    pub fn push_outcome(&self, o: OpOutcome) {
        self.state().outcomes.push_back((o, None));
    }

    /// Programa um desfecho `Residue` que deixa `ids` pendentes no motor (como o real).
    pub fn push_residue(&self, cause: &str, ids: &[&str]) {
        let pending = ids.iter().map(|s| s.to_string()).collect();
        self.state()
            .outcomes
            .push_back((OpOutcome::Residue(cause.to_string()), Some(pending)));
    }

    pub fn set_undo_all(&self, restored: Vec<String>, foreign: Vec<String>) {
        self.state().undo_all = (restored, foreign);
    }

    /// Todas as chamadas feitas até agora.
    pub fn calls(&self) -> Vec<Call> {
        self.state().calls.clone()
    }

    /// Quantas chamadas satisfazem `f`.
    pub fn count(&self, f: impl Fn(&Call) -> bool) -> usize {
        self.state().calls.iter().filter(|c| f(c)).count()
    }

    pub fn find_calls(&self) -> usize {
        self.count(|c| matches!(c, Call::Find))
    }

    pub fn search_calls(&self) -> usize {
        self.count(|c| matches!(c, Call::Search { .. }))
    }

    pub fn recover_calls(&self) -> usize {
        self.count(|c| matches!(c, Call::Recover { .. }))
    }

    /// `ids` de cada chamada de busca, na ordem.
    pub fn search_ids(&self) -> Vec<Option<Vec<String>>> {
        self.state()
            .calls
            .iter()
            .filter_map(|c| match c {
                Call::Search { ids, .. } => Some(ids.clone()),
                _ => None,
            })
            .collect()
    }

    /// `ids` de cada `merge_search` da sessão, na ordem.
    pub fn merge_ids(&self) -> Vec<Option<Vec<String>>> {
        self.state()
            .calls
            .iter()
            .filter_map(|c| match c {
                Call::MergeSearch { ids } => Some(ids.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn clear_calls(&self) {
        self.state().calls.clear();
    }
}

/// Alvo falso.
pub(crate) struct FakeBackend {
    ctl: FakeCtl,
}

impl FakeBackend {
    pub fn new() -> FakeBackend {
        let state = FakeState {
            find: Find::NotRunning,
            recover: VecDeque::new(),
            calls: Vec::new(),
            drawn: None,
            search_queue: VecDeque::new(),
            default_search: SearchSpec::new(Vec::new()),
            search_delay: Duration::ZERO,
            search_hold: false,
            search_panics: false,
            start_time: 1,
            open_error: None,
            open_engine_error: None,
            merge_result: true,
            exited: false,
            stuck: false,
            pending: Vec::new(),
            outcomes: VecDeque::new(),
            undo_all: (Vec::new(), Vec::new()),
        };
        FakeBackend {
            ctl: FakeCtl(Arc::new(Mutex::new(state))),
        }
    }

    pub fn ctl(&self) -> FakeCtl {
        self.ctl.clone()
    }
}

fn identity_of(pid: i32, start_time: u64) -> Identity {
    Identity {
        pid,
        start_time,
        boot_id: "boot".to_string(),
    }
}

/// A busca falsa, que roda na thread de busca.
fn fake_search(
    ctl: &FakeCtl,
    pid: i32,
    ids: Option<Vec<String>>,
    expected: Option<Identity>,
) -> Result<Search<()>, SearchError> {
    let (delay, panics) = {
        let mut s = ctl.state();
        s.calls.push(Call::Search {
            pid,
            ids: ids.clone(),
            expected: expected.clone(),
        });
        (s.search_delay, s.search_panics)
    };
    let end = std::time::Instant::now() + MAX_HOLD;
    while ctl.state().search_hold && std::time::Instant::now() < end {
        std::thread::sleep(Duration::from_millis(2));
    }
    if !delay.is_zero() {
        std::thread::sleep(delay);
    }
    if panics {
        panic!("pânico de teste na busca");
    }
    let (spec, start_time) = {
        let mut s = ctl.state();
        let spec = match s.search_queue.pop_front() {
            Some(res) => res?,
            None => s.default_search.clone(),
        };
        (spec, s.start_time)
    };
    let identity = identity_of(pid, start_time);
    if expected.as_ref().is_some_and(|e| *e != identity) {
        return Err(SearchError::Gone);
    }
    let cheats = spec
        .cheats
        .into_iter()
        .filter(|c| ids.as_ref().is_none_or(|list| list.contains(&c.id)))
        .collect();
    Ok(Search {
        pid,
        identity,
        version_key: spec.version_key,
        has_version_file: spec.has_version_file,
        disk: spec.disk,
        cheats,
        ids,
        took: spec.took,
        payload: (),
    })
}

impl Backend for FakeBackend {
    type Payload = ();
    type Session = FakeSession;

    fn find(&mut self) -> Find {
        let mut s = self.ctl.state();
        s.calls.push(Call::Find);
        s.find.clone()
    }

    fn search_fn(&self) -> SearchFn<()> {
        let ctl = self.ctl.clone();
        Arc::new(move |pid, ids, expected| fake_search(&ctl, pid, ids, expected))
    }

    fn recover(&mut self) -> Recovered {
        let mut s = self.ctl.state();
        let busy = FakeCtl::drawn_busy(&s);
        s.calls.push(Call::Recover { busy });
        s.recover.pop_front().unwrap_or(Recovered::Clean {
            lines: Vec::new(),
            corrupt: None,
        })
    }

    fn open(&mut self, search: Search<()>, engine: bool) -> Result<FakeSession, TuiError> {
        let mut s = self.ctl.state();
        s.calls.push(Call::Open { engine });
        if let Some(e) = &s.open_error {
            return Err(TuiError::Other(e.clone()));
        }
        Ok(FakeSession {
            ctl: self.ctl.clone(),
            engine: engine && search.disk.is_ok(),
            disk: search.disk,
        })
    }
}

/// Sessão falsa.
pub(crate) struct FakeSession {
    ctl: FakeCtl,
    engine: bool,
    disk: Result<(), String>,
}

impl FakeSession {
    /// Registra a chamada (com o `busy` desenhado) e devolve o desfecho programado.
    fn op(&self, make: impl FnOnce(Option<String>) -> Call) -> OpOutcome {
        let mut s = self.ctl.state();
        let busy = FakeCtl::drawn_busy(&s);
        s.calls.push(make(busy));
        let (outcome, pending) = s
            .outcomes
            .pop_front()
            .unwrap_or((OpOutcome::Done(Duration::from_millis(1)), None));
        if let Some(pending) = pending {
            s.pending = pending;
        }
        outcome
    }
}

impl Drop for FakeSession {
    fn drop(&mut self) {
        self.ctl.state().calls.push(Call::SessionDropped);
    }
}

impl Session<()> for FakeSession {
    fn has_engine(&self) -> bool {
        self.engine
    }

    fn open_engine(&mut self) -> Result<(), TuiError> {
        let mut s = self.ctl.state();
        s.calls.push(Call::OpenEngine);
        if self.engine {
            return Ok(());
        }
        if let Err(cause) = &self.disk {
            return Err(TuiError::Other(cause.clone()));
        }
        if let Some(e) = &s.open_engine_error {
            return Err(TuiError::Other(e.clone()));
        }
        self.engine = true;
        Ok(())
    }

    fn merge_search(&mut self, search: Search<()>) -> bool {
        let mut s = self.ctl.state();
        s.calls.push(Call::MergeSearch {
            ids: search.ids.clone(),
        });
        if !s.merge_result {
            return false;
        }
        self.disk = search.disk;
        true
    }

    fn exited(&self) -> bool {
        self.ctl.state().exited
    }

    fn apply(&mut self, items: &[(String, u64)]) -> OpOutcome {
        self.op(|busy| Call::Apply {
            items: items.to_vec(),
            busy,
        })
    }

    fn undo(&mut self, id: &str) -> OpOutcome {
        self.op(|busy| Call::Undo {
            id: id.to_string(),
            busy,
        })
    }

    fn set_value(&mut self, id: &str, k: u64) -> OpOutcome {
        self.op(|busy| Call::SetValue {
            id: id.to_string(),
            k,
            busy,
        })
    }

    fn restore_pending(&mut self) -> OpOutcome {
        let out = self.op(|busy| Call::RestorePending { busy });
        if matches!(out, OpOutcome::Done(_)) {
            self.ctl.state().pending.clear();
        }
        out
    }

    fn retry_resume(&mut self) -> OpOutcome {
        let out = self.op(|busy| Call::RetryResume { busy });
        if matches!(out, OpOutcome::Done(_)) {
            self.ctl.state().stuck = false;
        }
        out
    }

    fn is_stuck(&self) -> bool {
        self.ctl.state().stuck
    }

    fn pending_ids(&self) -> Vec<String> {
        self.ctl.state().pending.clone()
    }

    fn discard_gone(&mut self) {
        self.ctl.state().calls.push(Call::DiscardGone);
    }

    fn undo_all(&mut self) -> (Vec<String>, Vec<String>) {
        let mut s = self.ctl.state();
        let busy = FakeCtl::drawn_busy(&s);
        s.calls.push(Call::UndoAll { busy });
        s.undo_all.clone()
    }
}
