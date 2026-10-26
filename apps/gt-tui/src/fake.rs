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
use std::time::{Duration, Instant};

use gt_core::procinfo::Identity;
use gt_grounded2::fields::{ActionOutcome, TickReport};
use gt_grounded2::signatures::Category;

use crate::backend::{
    Backend, CheatInfo, Find, JobKind, OpOutcome, Recovered, ReflectionJob, ReflectionNews,
    ReflectionOutcome, Search, SearchError, SearchFn, Session, TuiError, idle_tick,
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
    /// Um trabalho da tarefa de reflexão começou a rodar (na thread dele).
    JobRun {
        kind: JobKind,
        /// Número da sessão que criou o trabalho.
        session: u64,
    },
    MergeReflection {
        kind: JobKind,
        session: u64,
    },
    FieldOn {
        id: String,
        k: u64,
        busy: Option<String>,
    },
    FieldAction {
        id: String,
        k: u64,
    },
    FieldOff {
        id: String,
        busy: Option<String>,
    },
    FieldTick {
        on: Vec<String>,
    },
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

/// Cheat de valor de teste: categoria `Sobrevivencia`, nome igual ao `id`.
pub(crate) fn field(id: &str, avail: Avail) -> CheatInfo {
    CheatInfo {
        id: id.to_string(),
        name: id.to_string(),
        category: Category::Sobrevivencia,
        value: None,
        avail,
        field: true,
        action: false,
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
        field: false,
        action: false,
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
    /// `apply` entra em pânico (a sessão falsa de um teste de pânico no laço).
    pub apply_panics: bool,
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
    /// Sessões abertas até agora (cada sessão guarda o seu número).
    pub sessions: u64,
    /// Tipos de trabalho que a sessão oferece.
    pub jobs: Vec<JobKind>,
    /// Os trabalhos esperam até [`FakeCtl::release_jobs`].
    pub job_hold: bool,
    /// Quanto cada trabalho diz ter levado.
    pub job_took: Duration,
    /// Notícias das próximas junções (vazia: índice bom, sem nova tentativa).
    pub news: VecDeque<ReflectionNews>,
    pub field_avail: Vec<(String, Avail)>,
    pub needs_controller: bool,
    /// Resultados programados das próximas ações (vazia: soma `k` a partir de zero).
    pub action_outcomes: VecDeque<ActionOutcome>,
    /// Uma ação pediu a procura do controller.
    pub action_wants: bool,
    /// Resultado da procura pedida pela ação, entregue uma vez.
    pub controller_news: Option<bool>,
    /// Relatórios dos próximos ciclos (vazia: ciclo sem manutenção).
    pub tick_reports: VecDeque<TickReport>,
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

    pub fn set_apply_panics(&self, panics: bool) {
        self.state().apply_panics = panics;
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

    /// A sessão passa a oferecer estes trabalhos da tarefa de reflexão.
    pub fn set_jobs(&self, kinds: &[JobKind]) {
        self.state().jobs = kinds.to_vec();
    }

    pub fn hold_jobs(&self) {
        self.state().job_hold = true;
    }

    pub fn release_jobs(&self) {
        self.state().job_hold = false;
    }

    pub fn set_job_took(&self, took: Duration) {
        self.state().job_took = took;
    }

    /// Programa a notícia da próxima junção de um resultado.
    pub fn push_news(&self, news: ReflectionNews) {
        self.state().news.push_back(news);
    }

    pub fn set_field_avail(&self, avail: &[(&str, Avail)]) {
        self.state().field_avail = avail
            .iter()
            .map(|(id, a)| (id.to_string(), a.clone()))
            .collect();
    }

    pub fn set_needs_controller(&self, needs: bool) {
        self.state().needs_controller = needs;
    }

    /// Programa o resultado da próxima ação.
    pub fn push_action(&self, outcome: ActionOutcome) {
        self.state().action_outcomes.push_back(outcome);
    }

    /// Programa a notícia da procura pedida pela ação (`true` achou, `false` não achou).
    pub fn set_controller_news(&self, news: Option<bool>) {
        self.state().controller_news = news;
    }

    pub fn action_wants(&self) -> bool {
        self.state().action_wants
    }

    pub fn actions(&self) -> usize {
        self.count(|c| matches!(c, Call::FieldAction { .. }))
    }

    /// Programa o relatório do próximo ciclo de manutenção.
    pub fn push_tick(&self, report: TickReport) {
        self.state().tick_reports.push_back(report);
    }

    pub fn job_runs(&self, kind: JobKind) -> usize {
        self.count(|c| matches!(c, Call::JobRun { kind: k, .. } if *k == kind))
    }

    pub fn merges(&self, kind: JobKind) -> usize {
        self.count(|c| matches!(c, Call::MergeReflection { kind: k, .. } if *k == kind))
    }

    pub fn field_ticks(&self) -> usize {
        self.count(|c| matches!(c, Call::FieldTick { .. }))
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
            apply_panics: false,
            start_time: 1,
            open_error: None,
            open_engine_error: None,
            merge_result: true,
            exited: false,
            stuck: false,
            pending: Vec::new(),
            outcomes: VecDeque::new(),
            undo_all: (Vec::new(), Vec::new()),
            sessions: 0,
            jobs: Vec::new(),
            job_hold: false,
            job_took: Duration::from_millis(10),
            news: VecDeque::new(),
            field_avail: Vec::new(),
            needs_controller: false,
            action_outcomes: VecDeque::new(),
            action_wants: false,
            controller_news: None,
            tick_reports: VecDeque::new(),
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
        s.sessions += 1;
        Ok(FakeSession {
            ctl: self.ctl.clone(),
            engine: engine && search.disk.is_ok(),
            disk: search.disk,
            number: s.sessions,
        })
    }
}

/// Sessão falsa.
pub(crate) struct FakeSession {
    ctl: FakeCtl,
    engine: bool,
    disk: Result<(), String>,
    /// Número desta sessão (o `Call` dos trabalhos diz de qual sessão eles vieram).
    number: u64,
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
        if self.ctl.state().apply_panics {
            panic!("pânico de teste no apply");
        }
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
        if matches!(out, OpOutcome::Done(_) | OpOutcome::Foreign) {
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

    fn reflection_job(&self, kind: JobKind) -> Option<ReflectionJob> {
        if !self.ctl.state().jobs.contains(&kind) {
            return None;
        }
        let ctl = self.ctl.clone();
        let session = self.number;
        Some(ReflectionJob {
            kind,
            run: Box::new(move || {
                ctl.state().calls.push(Call::JobRun { kind, session });
                let end = Instant::now() + MAX_HOLD;
                while ctl.state().job_hold && Instant::now() < end {
                    std::thread::sleep(Duration::from_millis(2));
                }
                let took = ctl.state().job_took;
                ReflectionOutcome {
                    kind,
                    took,
                    data: Box::new(session),
                }
            }),
        })
    }

    fn merge_reflection(&mut self, out: ReflectionOutcome) -> ReflectionNews {
        let mut s = self.ctl.state();
        let session = out.data.downcast_ref::<u64>().copied().unwrap_or(0);
        s.calls.push(Call::MergeReflection {
            kind: out.kind,
            session,
        });
        // Como a sessão real: toda procura terminada limpa o pedido da ação, com ou sem notícia.
        if out.kind == JobKind::Controller {
            s.action_wants = false;
        }
        s.news.pop_front().unwrap_or(ReflectionNews {
            index_ok: true,
            retry: false,
            player_changed: None,
        })
    }

    fn field_avail(&self) -> Vec<(String, Avail)> {
        self.ctl.state().field_avail.clone()
    }

    fn needs_controller(&self, _now: Instant) -> bool {
        self.ctl.state().needs_controller
    }

    fn field_on(&mut self, id: &str, k: u64) -> OpOutcome {
        self.op(|busy| Call::FieldOn {
            id: id.to_string(),
            k,
            busy,
        })
    }

    fn field_action(&mut self, id: &str, k: u64) -> ActionOutcome {
        let mut s = self.ctl.state();
        s.calls.push(Call::FieldAction {
            id: id.to_string(),
            k,
        });
        let out = s
            .action_outcomes
            .pop_front()
            .unwrap_or(ActionOutcome::Done {
                before: 0,
                after: i32::try_from(k).unwrap_or(i32::MAX),
            });
        if out == ActionOutcome::NeedsController {
            s.action_wants = true;
        }
        out
    }

    fn action_wants_controller(&self) -> bool {
        self.ctl.state().action_wants
    }

    fn take_controller_news(&mut self) -> Option<bool> {
        let mut s = self.ctl.state();
        let news = s.controller_news.take();
        if news.is_some() {
            s.action_wants = false;
        }
        news
    }

    fn field_off(&mut self, id: &str) -> OpOutcome {
        self.op(|busy| Call::FieldOff {
            id: id.to_string(),
            busy,
        })
    }

    fn field_tick(&mut self, on: &[String], _now: Instant) -> TickReport {
        let mut s = self.ctl.state();
        s.calls.push(Call::FieldTick { on: on.to_vec() });
        s.tick_reports.pop_front().unwrap_or_else(idle_tick)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> (FakeCtl, FakeSession) {
        let backend = FakeBackend::new();
        let ctl = backend.ctl.clone();
        let session = FakeSession {
            ctl: ctl.clone(),
            engine: true,
            disk: Ok(()),
            number: 1,
        };
        (ctl, session)
    }

    #[test]
    fn action_needs_controller_marks_search() {
        let (ctl, mut s) = session();
        assert!(!s.action_wants_controller());
        ctl.push_action(ActionOutcome::NeedsController);
        assert_eq!(s.field_action("a", 3), ActionOutcome::NeedsController);
        assert!(s.action_wants_controller());
        // Sem notícia, o pedido segue em aberto.
        assert_eq!(s.take_controller_news(), None);
        assert!(s.action_wants_controller());
        ctl.set_controller_news(Some(true));
        assert_eq!(s.take_controller_news(), Some(true));
        assert!(!s.action_wants_controller());
        assert_eq!(s.take_controller_news(), None);
        assert_eq!(ctl.actions(), 1);
    }

    #[test]
    fn field_on_records_k() {
        let (ctl, mut s) = session();
        assert!(matches!(s.field_on("f", 7), OpOutcome::Done(_)));
        assert!(ctl.calls().iter().any(|c| matches!(
            c,
            Call::FieldOn { id, k: 7, .. } if id == "f"
        )));
    }
}
