//! Implementação crua da interface de alvo, só para testes (spec 003, 5): patches crus
//! contra um processo qualquer (o `gt-testtarget`), sem o jogo e sem assinaturas.
//!
//! Depende só do `gt-core`. Os testes de integração trazem o processo-alvo e controlam esta
//! implementação por um [`RawControl`] compartilhado (qual PID existe, quais definições valem).

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use gt_core::engine::{Engine, EngineOptions, PolicyFn};
use gt_core::journal::{JournalLock, JournalStore};
use gt_core::mem::{MemReader, Pidfd};
use gt_core::patch::{PatchSpec, Validated, WritePolicy, validate};
use gt_core::procinfo::{Identity, check_alive, identity, is_dead, read_stat};
use gt_core::recovery::recover;
use gt_core::suspend::{PauseConfig, open_checked};
use gt_core::value::ValueSpec;
use gt_grounded2::signatures::Category;

use crate::backend::{
    Backend, CheatInfo, Find, OpOutcome, Recovered, Search, SearchError, SearchFn, Session,
    TuiError, outcome_of, recovery_outcome, restore_outcome,
};
use crate::model::Avail;

/// Um cheat cru: onde está, que bytes espera e que bytes escreve.
#[derive(Debug, Clone, PartialEq)]
pub struct RawDef {
    /// Identificador estável.
    pub id: String,
    /// Nome mostrado.
    pub name: String,
    /// Categoria.
    pub category: Category,
    /// Endereço absoluto no alvo.
    pub addr: u64,
    /// Bytes esperados antes de ligar.
    pub expected: Vec<u8>,
    /// Bytes escritos ao ligar (no valor padrão, se houver `value`).
    pub replacement: Vec<u8>,
    /// Definição do valor ajustável.
    pub value: Option<ValueSpec>,
    /// Se o cheat é "verificado" (senão aparece como não verificado).
    pub verified: bool,
}

impl RawDef {
    /// Bytes a escrever no ponto `k` da grade (sem valor, sempre `replacement`).
    fn bytes_at(&self, k: u64) -> Vec<u8> {
        match &self.value {
            Some(v) => v.bytes(&self.replacement, k),
            None => self.replacement.clone(),
        }
    }
}

/// O que o teste controla na implementação crua. Os endereços mudam a cada alvo novo (ASLR):
/// o teste troca `pid` e `defs` juntos.
#[derive(Debug, Clone)]
pub struct RawControl {
    /// PID que [`Backend::find`] devolve; `None` = o jogo não está aberto.
    pub pid: Option<i32>,
    /// Definições dos cheats.
    pub defs: Vec<RawDef>,
    /// Chave de versão devolvida pela busca.
    pub version_key: String,
    /// Quanto a busca dorme antes de ler o alvo.
    pub search_delay: Duration,
    /// A busca entra em pânico (testa o desfazer da saída por pânico na thread de busca).
    pub search_panics: bool,
}

fn lock_control(control: &Mutex<RawControl>) -> MutexGuard<'_, RawControl> {
    control.lock().unwrap_or_else(PoisonError::into_inner)
}

fn search_error(e: gt_core::Error) -> SearchError {
    match e {
        gt_core::Error::ProcessGone { .. } => SearchError::Gone,
        e @ gt_core::Error::PermissionDenied { .. } => SearchError::NoAccess(e.to_string()),
        e => SearchError::Other(e.to_string()),
    }
}

/// Disponibilidade de uma definição segundo os bytes que estão no alvo.
fn avail_of(def: &RawDef, pid: i32) -> Result<Avail, SearchError> {
    let mut buf = vec![0u8; def.expected.len()];
    match MemReader::new(pid).read_exact(def.addr, &mut buf) {
        Ok(()) => {}
        Err(gt_core::Error::Unmapped { .. }) => {
            return Ok(Avail::Unavailable("não encontrado".to_string()));
        }
        Err(e) => return Err(search_error(e)),
    }
    // Esperados, ou (cheat de valor ligado) os bytes de algum ponto da grade.
    let on_grid = def
        .value
        .as_ref()
        .is_some_and(|v| (0..=v.steps()).any(|k| v.bytes(&def.replacement, k) == buf));
    if buf == def.expected || on_grid {
        Ok(Avail::Available {
            verified: def.verified,
        })
    } else {
        Ok(Avail::Unavailable("bytes diferentes".to_string()))
    }
}

/// A busca crua: lê as definições do `control`, confere os bytes no alvo.
fn raw_search(
    control: &Mutex<RawControl>,
    pid: i32,
    ids: Option<Vec<String>>,
    expected: Option<Identity>,
) -> Result<Search<Vec<RawDef>>, SearchError> {
    let started = Instant::now();
    let (defs, version_key, delay, panics) = {
        let c = lock_control(control);
        (
            c.defs.clone(),
            c.version_key.clone(),
            c.search_delay,
            c.search_panics,
        )
    };
    if !delay.is_zero() {
        std::thread::sleep(delay);
    }
    if panics {
        panic!("pânico de teste na busca");
    }
    let now = identity(pid).map_err(search_error)?;
    if expected.as_ref().is_some_and(|e| *e != now) {
        return Err(SearchError::Gone);
    }
    let defs: Vec<RawDef> = defs
        .into_iter()
        .filter(|d| ids.as_ref().is_none_or(|list| list.contains(&d.id)))
        .collect();
    let mut cheats = Vec::with_capacity(defs.len());
    for def in &defs {
        cheats.push(CheatInfo {
            id: def.id.clone(),
            name: def.name.clone(),
            category: def.category,
            value: def.value.clone(),
            avail: avail_of(def, pid)?,
        });
    }
    Ok(Search {
        pid,
        identity: now,
        version_key,
        has_version_file: true,
        disk: Ok(()),
        cheats,
        ids,
        took: started.elapsed(),
        payload: defs,
    })
}

/// A política de escrita da implementação crua: toda página executável do alvo.
fn exec_policy() -> PolicyFn {
    Box::new(|maps| {
        WritePolicy::new(
            maps.iter()
                .filter(|r| r.perms.exec)
                .map(|r| r.start..r.end)
                .collect(),
        )
    })
}

fn start_engine(
    pid: i32,
    identity: &Identity,
    store: &JournalStore,
    lock: &JournalLock,
    version_key: &str,
) -> Result<Engine, TuiError> {
    let opts = EngineOptions {
        version_key: version_key.to_string(),
        pause: PauseConfig::default(),
    };
    Engine::open(
        pid,
        Some(identity),
        store.clone(),
        lock,
        exec_policy(),
        opts,
    )
    .map_err(|e| TuiError::Other(e.to_string()))
}

/// Implementação crua da interface de alvo.
pub struct RawBackend {
    control: Arc<Mutex<RawControl>>,
    store: JournalStore,
    lock: Arc<JournalLock>,
}

impl RawBackend {
    /// Cria o backend e pega a trava do diário em `journal_dir`
    /// ([`gt_core::Error::AlreadyRunning`] se já estiver ocupada).
    pub fn new(
        control: Arc<Mutex<RawControl>>,
        journal_dir: PathBuf,
    ) -> gt_core::Result<RawBackend> {
        let store = JournalStore::new(journal_dir);
        let lock = Arc::new(store.lock()?);
        Ok(RawBackend {
            control,
            store,
            lock,
        })
    }
}

impl Backend for RawBackend {
    type Payload = Vec<RawDef>;
    type Session = RawSession;

    fn find(&mut self) -> Find {
        let Some(pid) = lock_control(&self.control).pid else {
            return Find::NotRunning;
        };
        match read_stat(pid) {
            Ok(stat) if !is_dead(stat.state) => Find::Found(pid),
            _ => Find::NotRunning,
        }
    }

    fn search_fn(&self) -> SearchFn<Vec<RawDef>> {
        let control = self.control.clone();
        Arc::new(move |pid, ids, expected| raw_search(&control, pid, ids, expected))
    }

    fn recover(&mut self) -> Recovered {
        recovery_outcome(recover(&self.store, &self.lock, &PauseConfig::default()))
    }

    fn open(&mut self, search: Search<Vec<RawDef>>, engine: bool) -> Result<RawSession, TuiError> {
        let (pidfd, id) = open_checked(search.pid).map_err(|e| TuiError::Other(e.to_string()))?;
        if id != search.identity {
            return Err(TuiError::Other("o processo terminou".to_string()));
        }
        let mut session = RawSession {
            engine: None,
            pidfd,
            identity: search.identity,
            store: self.store.clone(),
            version_key: search.version_key,
            disk: search.disk,
            defs: search.payload,
            lock: self.lock.clone(),
        };
        if engine && session.disk.is_ok() {
            session.open_engine()?;
        }
        Ok(session)
    }
}

/// Sessão crua com um alvo conectado.
///
/// O motor é o primeiro campo: cai antes da trava (`lock`, o último), que só é solta quando a
/// última cópia do `Arc` cai.
pub struct RawSession {
    engine: Option<Engine>,
    pidfd: Pidfd,
    identity: Identity,
    store: JournalStore,
    version_key: String,
    disk: Result<(), String>,
    defs: Vec<RawDef>,
    lock: Arc<JournalLock>,
}

impl RawSession {
    fn def(&self, id: &str) -> Option<&RawDef> {
        self.defs.iter().find(|d| d.id == id)
    }

    /// Roda uma operação do motor e a classifica; sem motor, recusa.
    fn run(
        &mut self,
        id: &str,
        op: impl FnOnce(&mut Engine) -> gt_core::Result<gt_core::engine::BatchReport>,
    ) -> OpOutcome {
        let Some(engine) = self.engine.as_mut() else {
            return OpOutcome::Refused("sem motor".to_string());
        };
        let res = op(engine);
        let pending = engine.pending_ids();
        outcome_of(res, id, &pending)
    }
}

impl Session<Vec<RawDef>> for RawSession {
    fn has_engine(&self) -> bool {
        self.engine.is_some()
    }

    fn open_engine(&mut self) -> Result<(), TuiError> {
        if self.engine.is_some() {
            return Ok(());
        }
        if let Err(cause) = &self.disk {
            return Err(TuiError::Other(cause.clone()));
        }
        let engine = start_engine(
            self.identity.pid,
            &self.identity,
            &self.store,
            &self.lock,
            &self.version_key,
        )?;
        self.engine = Some(engine);
        Ok(())
    }

    fn merge_search(&mut self, s: Search<Vec<RawDef>>) -> bool {
        if s.identity != self.identity {
            return false;
        }
        self.disk = s.disk;
        self.version_key = s.version_key;
        let active: Vec<String> = self
            .engine
            .as_ref()
            .map(|e| e.active().iter().map(|p| p.id.clone()).collect())
            .unwrap_or_default();
        for def in s.payload {
            let listed = s.ids.as_ref().is_none_or(|ids| ids.contains(&def.id));
            if !listed || active.contains(&def.id) {
                continue;
            }
            match self.defs.iter_mut().find(|d| d.id == def.id) {
                Some(slot) => *slot = def,
                None => self.defs.push(def),
            }
        }
        true
    }

    fn exited(&self) -> bool {
        self.pidfd.exited(Duration::ZERO).unwrap_or(false)
            || matches!(
                check_alive(&self.identity),
                Err(gt_core::Error::ProcessGone { .. })
            )
    }

    fn apply(&mut self, items: &[(String, u64)]) -> OpOutcome {
        if self.engine.is_none() {
            return OpOutcome::Refused("sem motor".to_string());
        }
        let mut batch: Vec<Validated> = Vec::with_capacity(items.len());
        for (id, k) in items {
            let Some(def) = self.def(id) else {
                return OpOutcome::Refused(format!("cheat desconhecido: {id}"));
            };
            let spec = PatchSpec {
                id: id.clone(),
                addr: def.addr,
                expected: def.expected.clone(),
                replacement: def.bytes_at(*k),
            };
            match validate(spec) {
                Ok(v) => batch.push(v),
                Err(e) => return OpOutcome::Refused(e.to_string()),
            }
        }
        let Some(first) = items.first().map(|(id, _)| id.clone()) else {
            return OpOutcome::Refused("nenhum cheat para ligar".to_string());
        };
        let Some(engine) = self.engine.as_mut() else {
            return OpOutcome::Refused("sem motor".to_string());
        };
        let res = engine.apply(batch);
        let pending = engine.pending_ids();
        // Um lote que falha deixa todos pendentes: o resto vale para o primeiro `id` pendente.
        let id = items
            .iter()
            .map(|(id, _)| id)
            .find(|id| pending.contains(id))
            .unwrap_or(&first);
        outcome_of(res, id, &pending)
    }

    fn undo(&mut self, id: &str) -> OpOutcome {
        self.run(id, |e| e.undo(&[id]))
    }

    fn set_value(&mut self, id: &str, k: u64) -> OpOutcome {
        let bytes = match self.def(id) {
            Some(def) if def.value.is_some() => def.bytes_at(k),
            Some(_) => return OpOutcome::Refused(format!("{id} não tem valor")),
            None => return OpOutcome::Refused(format!("cheat desconhecido: {id}")),
        };
        self.run(id, |e| e.set_replacement(id, bytes))
    }

    fn restore_pending(&mut self) -> OpOutcome {
        let Some(engine) = self.engine.as_mut() else {
            return OpOutcome::Refused("sem motor".to_string());
        };
        let res = engine.restore_pending();
        let pending = engine.pending_ids();
        restore_outcome(res, &pending)
    }

    fn retry_resume(&mut self) -> OpOutcome {
        let Some(engine) = self.engine.as_mut() else {
            return OpOutcome::Refused("sem motor".to_string());
        };
        match engine.retry_resume() {
            Ok(()) => OpOutcome::Done(Duration::ZERO),
            Err(gt_core::Error::ProcessGone { .. }) => OpOutcome::Gone,
            Err(e) => OpOutcome::Refused(e.to_string()),
        }
    }

    fn is_stuck(&self) -> bool {
        self.engine.as_ref().is_some_and(Engine::is_stuck)
    }

    fn pending_ids(&self) -> Vec<String> {
        self.engine
            .as_ref()
            .map(Engine::pending_ids)
            .unwrap_or_default()
    }

    fn discard_gone(&mut self) {
        if let Some(engine) = self.engine.as_mut() {
            engine.discard_gone();
        }
    }

    fn undo_all(&mut self) -> (Vec<String>, Vec<String>) {
        let Some(engine) = self.engine.as_mut() else {
            return (Vec::new(), Vec::new());
        };
        match engine.undo_all() {
            Ok(report) => (report.restored, report.foreign),
            Err(e) => {
                tracing::warn!(erro = %e, "undo_all falhou");
                (Vec::new(), Vec::new())
            }
        }
    }
}
