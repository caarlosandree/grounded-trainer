//! Implementação da interface de alvo para o Grounded 2 (spec 003, 5, 6.1, 6.2 e 6.5).
//!
//! Escolha de projeto (brief da Tarefa 10): o `Target` do `gt-grounded2` é `Send`, então o
//! `Payload` da busca é `(Target, CatalogScan)` e a busca devolve o alvo já aberto. A sessão
//! guarda o `Target` da busca que a abriu só para abrir o motor (pid e identidade); o `open`
//! não reabre o alvo na thread principal.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gt_core::engine::{BatchReport, Engine};
use gt_core::journal::{JournalLock, JournalStore};
use gt_core::mem::Pidfd;
use gt_core::modules::Module;
use gt_core::patch::{PatchSpec, Validated, validate};
use gt_core::pe::PeHeaders;
use gt_core::process::ptrace_scope;
use gt_core::procinfo::{Identity, check_alive, identity};
use gt_core::recovery::recover;
use gt_core::suspend::{PauseConfig, open_checked};
use gt_grounded2::catalog::{CatalogScan, Entry, scan, scan_subset};
use gt_grounded2::fields::TickReport;
use gt_grounded2::patching;
use gt_grounded2::resolve::{Target, find_pid, permission_hint};
use gt_grounded2::signatures::{FieldDef, PatchDef, Signatures};
use gt_grounded2::state::PatchState;
use gt_grounded2::ue::UeScan;

use crate::backend::{
    Backend, CheatInfo, Find, JobKind, OpOutcome, Recovered, ReflectionJob, ReflectionNews,
    ReflectionOutcome, Search, SearchError, SearchFn, Session, TuiError, outcome_of,
    recovery_outcome, restore_outcome,
};
use crate::model::Avail;
use crate::values::{ValueCheats, field_infos, globals_of, search_avail};

/// Nome do programa nas dicas de permissão.
const PROGRAM: &str = "gt-tui";

/// Dados que a busca leva de volta para a sessão: o alvo aberto, o catálogo e as definições dos
/// cheats de valor.
pub type GamePayload = (Target, CatalogScan, Vec<FieldDef>);

/// Disponibilidade mostrada para o estado de um patch: só `Verified` e `Unverified` podem ser
/// ligados; todos os outros (inclusive `Active` e `Pending`, que a busca sem diário não
/// produz) ficam indisponíveis com o rótulo do estado.
pub fn avail_of(state: &PatchState) -> Avail {
    match state {
        PatchState::Verified { .. } => Avail::Available { verified: true },
        PatchState::Unverified { .. } => Avail::Available { verified: false },
        other => Avail::Unavailable(other.label().to_string()),
    }
}

/// Junta `new` em `old`: cada entrada de `new` cujo `id` está em `ids` (com `None`, todas)
/// substitui a de mesmo `id` (ou entra no fim, se não havia). As entradas de `old` fora de
/// `ids` não mudam.
pub fn merge_entries(old: &mut Vec<Entry>, new: Vec<Entry>, ids: Option<&[String]>) {
    for entry in new {
        if ids.is_some_and(|list| !list.contains(&entry.def.id)) {
            continue;
        }
        match old.iter_mut().find(|e| e.def.id == entry.def.id) {
            Some(slot) => *slot = entry,
            None => old.push(entry),
        }
    }
}

/// `Ok(())` se o cabeçalho do disco foi lido; senão a causa, tirada do primeiro
/// `DiskUnreadable` das entradas (`"disco ilegível"` se não houver entradas).
fn disk_status(disk_read: bool, entries: &[Entry]) -> Result<(), String> {
    if disk_read {
        return Ok(());
    }
    let cause = entries.iter().find_map(|e| match &e.state {
        PatchState::DiskUnreadable(cause) => Some(cause.clone()),
        _ => None,
    });
    Err(cause.unwrap_or_else(|| "disco ilegível".to_string()))
}

/// Traduz o erro do `gt-grounded2` para o da busca.
fn search_error(e: gt_grounded2::Error) -> SearchError {
    match e {
        gt_grounded2::Error::Core(gt_core::Error::ProcessGone { .. }) => SearchError::Gone,
        gt_grounded2::Error::Core(gt_core::Error::PermissionDenied {
            ptrace_scope: scope,
            ..
        }) => SearchError::NoAccess(permission_hint(PROGRAM, ptrace_scope().or(scope))),
        e => SearchError::Other(e.to_string()),
    }
}

fn core_search_error(e: gt_core::Error) -> SearchError {
    search_error(gt_grounded2::Error::Core(e))
}

/// Os cheats que a busca viu: os patches na ordem do catálogo e, depois, os cheats de valor
/// (a tela os mostra depois dos patches da mesma categoria). Os de valor só entram quando a
/// busca trouxe o `[ue]` (busca completa ou com `@ue`).
fn cheats_of(
    entries: &[Entry],
    fields: &[FieldDef],
    ue: Option<&UeScan>,
    disk: &Result<(), String>,
) -> Vec<CheatInfo> {
    let mut out: Vec<CheatInfo> = entries
        .iter()
        .map(|e| CheatInfo {
            id: e.def.id.clone(),
            name: e.def.name.clone(),
            category: e.def.category,
            value: e.def.value.clone(),
            avail: avail_of(&e.state),
            field: false,
        })
        .collect();
    if ue.is_some() {
        let avail = search_avail(&globals_of(ue, disk));
        out.extend(field_infos(fields, &avail));
    }
    out
}

/// A busca: abre o alvo, confere a identidade (no começo e no fim) e varre o catálogo.
fn game_search(
    pid: i32,
    ids: Option<Vec<String>>,
    expected: Option<Identity>,
) -> Result<Search<GamePayload>, SearchError> {
    let started = Instant::now();
    let target = Target::open(Some(pid)).map_err(search_error)?;
    if expected.as_ref().is_some_and(|e| *e != target.identity) {
        return Err(SearchError::Gone);
    }
    let sigs = Signatures::embedded().map_err(search_error)?;
    let scanned = match &ids {
        None => scan(&target, &sigs, None, false),
        Some(list) => scan_subset(&target, &sigs, list),
    };
    let catalog = match scanned {
        Ok(catalog) => catalog,
        Err(e) => {
            // Um processo que fechou no meio da varredura falha de jeitos variados.
            if matches!(
                check_alive(&target.identity),
                Err(gt_core::Error::ProcessGone { .. })
            ) {
                return Err(SearchError::Gone);
            }
            return Err(search_error(e));
        }
    };
    // O PID pode ter sido reaproveitado durante a varredura: o resultado não vale.
    let now = identity(pid).map_err(core_search_error)?;
    if now != target.identity {
        return Err(SearchError::Gone);
    }
    let took = started.elapsed();
    tracing::info!(
        duracao_ms = u64::try_from(took.as_millis()).unwrap_or(u64::MAX),
        "busca dos padrões"
    );
    let disk = disk_status(catalog.disk.is_some(), &catalog.entries);
    let fields = sigs.fields().to_vec();
    Ok(Search {
        pid,
        identity: target.identity.clone(),
        version_key: catalog.version_key.clone(),
        has_version_file: catalog.has_version_file,
        cheats: cheats_of(&catalog.entries, &fields, catalog.ue.as_ref(), &disk),
        disk,
        ids,
        took,
        payload: (target, catalog, fields),
    })
}

/// O jogo: acha o processo, recupera o diário e abre as sessões.
pub struct GameBackend {
    pid: Option<i32>,
    store: JournalStore,
    lock: Arc<JournalLock>,
}

impl GameBackend {
    /// `pid` é o `--pid`, se houver; `lock` é a trava do diário que o chamador já pegou.
    pub fn new(pid: Option<i32>, store: JournalStore, lock: Arc<JournalLock>) -> GameBackend {
        GameBackend { pid, store, lock }
    }
}

impl Backend for GameBackend {
    type Payload = GamePayload;
    type Session = GameSession;

    fn find(&mut self) -> Find {
        match find_pid(self.pid) {
            Ok((pid, _)) => Find::Found(pid),
            Err(gt_grounded2::Error::GameNotFound) => Find::NotRunning,
            Err(gt_grounded2::Error::ManyCandidates { pids }) => Find::Many(pids),
            Err(gt_grounded2::Error::Core(gt_core::Error::ProcessGone { .. }))
                if self.pid.is_some() =>
            {
                Find::PidGone
            }
            Err(e) => {
                tracing::warn!(erro = %e, "falha ao procurar o jogo");
                Find::Error(e.to_string())
            }
        }
    }

    fn search_fn(&self) -> SearchFn<GamePayload> {
        Arc::new(game_search)
    }

    fn recover(&mut self) -> Recovered {
        recovery_outcome(recover(&self.store, &self.lock, &PauseConfig::default()))
    }

    fn open(&mut self, search: Search<GamePayload>, engine: bool) -> Result<GameSession, TuiError> {
        let (pidfd, id) = open_checked(search.pid).map_err(|e| TuiError::Other(e.to_string()))?;
        if id != search.identity {
            return Err(TuiError::Other("o processo terminou".to_string()));
        }
        let (target, catalog, fields) = search.payload;
        let values = ValueCheats::new(
            target.reader,
            fields,
            globals_of(catalog.ue.as_ref(), &search.disk),
        );
        let mut session = GameSession {
            engine: None,
            pidfd,
            identity: search.identity,
            store: self.store.clone(),
            disk: search.disk,
            target,
            module: catalog.module,
            headers: catalog.disk,
            entries: catalog.entries,
            values,
            lock: self.lock.clone(),
        };
        if engine && session.disk.is_ok() {
            session.open_engine()?;
        }
        Ok(session)
    }
}

/// Sessão com o jogo conectado.
///
/// O motor é o primeiro campo: cai antes da trava (`lock`, o último), que só é solta quando a
/// última cópia do `Arc` cai.
pub struct GameSession {
    engine: Option<Engine>,
    pidfd: Pidfd,
    identity: Identity,
    store: JournalStore,
    disk: Result<(), String>,
    target: Target,
    module: Module,
    headers: Option<PeHeaders>,
    /// Por `id`: a definição e o estado (de onde sai o endereço resolvido) da última busca.
    entries: Vec<Entry>,
    /// Cheats de valor (spec 005).
    values: ValueCheats,
    lock: Arc<JournalLock>,
}

impl GameSession {
    fn entry(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.def.id == id)
    }

    /// Bytes que o cheat escreve no ponto `k` da grade (sem valor, sempre `replacement`).
    fn bytes_at(def: &PatchDef, k: u64) -> Vec<u8> {
        match &def.value {
            Some(v) => v.bytes(&def.replacement, k),
            None => def.replacement.clone(),
        }
    }

    /// Roda uma operação do motor e a classifica; sem motor, recusa.
    fn run(
        &mut self,
        id: &str,
        op: impl FnOnce(&mut Engine) -> gt_core::Result<BatchReport>,
    ) -> OpOutcome {
        let Some(engine) = self.engine.as_mut() else {
            return OpOutcome::Refused("sem motor".to_string());
        };
        let res = op(engine);
        let pending = engine.pending_ids();
        outcome_of(res, id, &pending)
    }
}

impl Session<GamePayload> for GameSession {
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
        let Some(headers) = self.headers.clone() else {
            return Err(TuiError::Other("disco ilegível".to_string()));
        };
        let engine = patching::open_engine(
            &self.target,
            self.store.clone(),
            &self.lock,
            &self.module,
            headers,
        )
        .map_err(|e| TuiError::Other(e.to_string()))?;
        self.engine = Some(engine);
        Ok(())
    }

    fn merge_search(&mut self, s: Search<GamePayload>) -> bool {
        if s.identity != self.identity {
            return false;
        }
        self.disk = s.disk;
        let (target, catalog, _) = s.payload;
        if catalog.ue.is_some() {
            self.values
                .update_globals(globals_of(catalog.ue.as_ref(), &self.disk));
        }
        self.target = target;
        self.module = catalog.module;
        self.headers = catalog.disk;
        let active: Vec<String> = self
            .engine
            .as_ref()
            .map(|e| e.active().iter().map(|p| p.id.clone()).collect())
            .unwrap_or_default();
        // Ligados não se tocam. Um cheat com endereço resolvido que a nova busca não acha
        // (a memória pode ter outros patches aplicados) mantém o estado anterior (spec 6.2).
        let fresh: Vec<Entry> = catalog
            .entries
            .into_iter()
            .filter(|new| !active.contains(&new.def.id))
            .filter(|new| {
                let lost = matches!(new.state, PatchState::NotFound | PatchState::Inconclusive);
                let had_addr = self
                    .entry(&new.def.id)
                    .is_some_and(|old| old.state.apply_addr().is_some());
                !(lost && had_addr)
            })
            .collect();
        merge_entries(&mut self.entries, fresh, s.ids.as_deref());
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
            let Some(entry) = self.entry(id) else {
                return OpOutcome::Refused(format!("cheat desconhecido: {id}"));
            };
            let Some(addr) = entry.state.apply_addr() else {
                return OpOutcome::Refused(format!("{id} indisponível: {}", entry.state.label()));
            };
            let spec = PatchSpec {
                id: id.clone(),
                addr,
                expected: entry.def.expected.clone(),
                replacement: Self::bytes_at(&entry.def, *k),
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
        let bytes = match self.entry(id) {
            Some(entry) if entry.def.value.is_some() => Self::bytes_at(&entry.def, k),
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
                // O resultado final da saída é decidido relendo o diário (spec 6.8). O jogo já
                // fechado é esperado: não é erro.
                if matches!(e, gt_core::Error::ProcessGone { .. }) {
                    tracing::warn!(erro = %e, "undo_all: o jogo já tinha fechado");
                } else {
                    tracing::error!(erro = %e, "undo_all falhou");
                }
                (Vec::new(), Vec::new())
            }
        }
    }

    fn reflection_job(&self, kind: JobKind) -> Option<ReflectionJob> {
        self.values.job(kind)
    }

    fn merge_reflection(&mut self, out: ReflectionOutcome) -> ReflectionNews {
        self.values.merge(out)
    }

    fn field_avail(&self) -> Vec<(String, Avail)> {
        self.values.avail()
    }

    fn needs_controller(&self, now: Instant) -> bool {
        self.values.needs_controller(now)
    }

    fn field_on(&mut self, id: &str) -> OpOutcome {
        self.values.on(self.engine.as_mut(), id)
    }

    fn field_off(&mut self, id: &str) -> OpOutcome {
        self.values.off(self.engine.as_mut(), id)
    }

    fn field_tick(&mut self, on: &[String], now: Instant) -> TickReport {
        self.values.tick(self.engine.as_mut(), on, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gt_core::scan::Pattern;
    use gt_grounded2::signatures::Category;
    use gt_grounded2::ue::GlobalState;

    fn def(id: &str, name: &str) -> PatchDef {
        PatchDef {
            id: id.into(),
            name: name.into(),
            category: Category::Itens,
            description: "d".into(),
            module: gt_grounded2::GAME_EXE.into(),
            pattern: Pattern::parse("48 8B ?? 90").unwrap(),
            offset: 0,
            expected: vec![0x48, 0x8B],
            replacement: vec![0x48, 0x89],
            rva: None,
            value: None,
        }
    }

    fn entry(id: &str, name: &str, state: PatchState) -> Entry {
        Entry {
            def: def(id, name),
            state,
        }
    }

    #[test]
    fn avail_mapping() {
        assert_eq!(
            avail_of(&PatchState::Verified { addr: 1 }),
            Avail::Available { verified: true }
        );
        assert_eq!(
            avail_of(&PatchState::Unverified { addr: 1 }),
            Avail::Available { verified: false }
        );
        let cases = [
            (PatchState::Active { addr: 1 }, "ativo"),
            (PatchState::Pending { addr: 1 }, "pendente"),
            (
                PatchState::BytesDiffer {
                    addr: 1,
                    found: vec![0],
                },
                "bytes diferentes",
            ),
            (PatchState::Misplaced { found_rva: 1 }, "fora do lugar"),
            (PatchState::NotFound, "não encontrado"),
            (PatchState::Ambiguous(2), "ambíguo"),
            (PatchState::Inconclusive, "inconclusivo"),
            (PatchState::DiskUnreadable("x".into()), "disco ilegível"),
        ];
        for (state, label) in cases {
            assert_eq!(avail_of(&state), Avail::Unavailable(label.to_string()));
        }
    }

    #[test]
    fn merge_entries_only_listed() {
        let ver = |addr| PatchState::Verified { addr };
        let mut old = vec![
            entry("a", "A velho", ver(1)),
            entry("b", "B velho", ver(2)),
            entry("c", "C velho", ver(3)),
        ];
        let new = vec![
            entry("a", "A novo", PatchState::NotFound),
            entry("b", "B novo", ver(20)),
            entry("d", "D novo", ver(40)),
        ];
        let ids = vec!["b".to_string(), "d".to_string()];
        merge_entries(&mut old, new, Some(&ids));
        let view: Vec<(&str, &str)> = old
            .iter()
            .map(|e| (e.def.id.as_str(), e.def.name.as_str()))
            .collect();
        // `a` não estava na lista: fica como era; `b` troca; `d` entra; `c` não vem em `new`.
        assert_eq!(
            view,
            vec![
                ("a", "A velho"),
                ("b", "B novo"),
                ("c", "C velho"),
                ("d", "D novo")
            ]
        );
        assert_eq!(old[1].state, ver(20));
        assert_eq!(old[0].state, ver(1));

        // Sem lista, tudo o que veio entra.
        let mut all = vec![entry("a", "A velho", ver(1))];
        merge_entries(&mut all, vec![entry("a", "A novo", ver(5))], None);
        assert_eq!(all[0].def.name, "A novo");
        assert_eq!(all[0].state, ver(5));
    }

    #[test]
    fn disk_status_cases() {
        assert_eq!(disk_status(true, &[]), Ok(()));
        assert_eq!(disk_status(false, &[]), Err("disco ilegível".to_string()));
        let entries = vec![
            entry("a", "A", PatchState::NotFound),
            entry("b", "B", PatchState::DiskUnreadable("causa b".into())),
            entry("c", "C", PatchState::DiskUnreadable("causa c".into())),
        ];
        assert_eq!(disk_status(false, &entries), Err("causa b".to_string()));
        // Com o disco lido, as entradas não importam.
        assert_eq!(disk_status(true, &entries), Ok(()));
    }

    #[test]
    fn search_error_mapping() {
        use gt_core::Error as Core;
        assert!(matches!(
            search_error(gt_grounded2::Error::Core(Core::ProcessGone { pid: 1 })),
            SearchError::Gone
        ));
        match search_error(gt_grounded2::Error::Core(Core::PermissionDenied {
            pid: 1,
            ptrace_scope: Some(1),
        })) {
            SearchError::NoAccess(hint) => assert!(hint.contains("ptrace_scope"), "{hint}"),
            other => panic!("esperava NoAccess: {other:?}"),
        }
        assert!(matches!(
            search_error(gt_grounded2::Error::GameNotFound),
            SearchError::Other(_)
        ));
    }

    #[test]
    fn cheats_of_follows_entries() {
        let entries = vec![
            entry("a", "A", PatchState::Verified { addr: 1 }),
            entry("b", "B", PatchState::NotFound),
        ];
        let cheats = cheats_of(&entries, &[], None, &Ok(()));
        assert_eq!(cheats.len(), 2);
        assert_eq!(cheats[0].id, "a");
        assert_eq!(cheats[0].avail, Avail::Available { verified: true });
        assert_eq!(
            cheats[1].avail,
            Avail::Unavailable("não encontrado".to_string())
        );
    }

    /// `[ue]` das assinaturas embutidas, com as globais e a conferência barata dadas.
    fn ue_scan(gobjects: GlobalState, names: GlobalState, cheap: Result<(), String>) -> UeScan {
        let sigs = Signatures::embedded().unwrap();
        UeScan {
            sigs: sigs.ue_for_version("c935e799-0b27c000").unwrap(),
            gobjects,
            names,
            cheap,
        }
    }

    fn fields() -> Vec<FieldDef> {
        Signatures::embedded().unwrap().fields().to_vec()
    }

    fn sobrevivencia(id: &str) -> Entry {
        let mut e = entry(id, id, PatchState::Verified { addr: 1 });
        e.def.category = Category::Sobrevivencia;
        e
    }

    #[test]
    fn field_cheats_after_patches_in_category() {
        let ue = ue_scan(GlobalState::Verified(1), GlobalState::Verified(2), Ok(()));
        let entries = vec![sobrevivencia("p1"), entry("p2", "P2", PatchState::NotFound)];
        let cheats = cheats_of(&entries, &fields(), Some(&ue), &Ok(()));
        let ids: Vec<&str> = cheats.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(&ids[..3], ["p1", "p2", "vida-infinita"]);
        assert_eq!(cheats.len(), 2 + 8);
        assert!(
            cheats[2..]
                .iter()
                .all(|c| c.field && c.avail == Avail::Checking)
        );
        assert!(cheats[..2].iter().all(|c| !c.field));
        // Uma busca sem o `[ue]` (nova busca só de patches) não traz os cheats de valor.
        assert_eq!(cheats_of(&entries, &fields(), None, &Ok(())).len(), 2);
    }

    #[test]
    fn globals_unavailable_reason_on_fields() {
        for (gobjects, names, reason) in [
            (
                GlobalState::Unavailable("não encontrado"),
                GlobalState::Verified(2),
                "não encontrado",
            ),
            (
                GlobalState::Verified(1),
                GlobalState::Unavailable("ambíguo"),
                "ambíguo",
            ),
            (
                GlobalState::Unavailable("fora do lugar"),
                GlobalState::Unavailable("inconclusivo"),
                "fora do lugar",
            ),
        ] {
            let ue = ue_scan(gobjects, names, Err("globais indisponíveis".into()));
            let cheats = cheats_of(&[], &fields(), Some(&ue), &Ok(()));
            assert!(
                cheats
                    .iter()
                    .all(|c| c.avail == Avail::Unavailable(reason.to_string())),
                "{reason}"
            );
        }
    }

    #[test]
    fn cheap_check_failure_reason() {
        let ue = ue_scan(
            GlobalState::Verified(1),
            GlobalState::Unverified(2),
            Err("nome 0 não é None".into()),
        );
        let cheats = cheats_of(&[], &fields(), Some(&ue), &Ok(()));
        assert!(
            cheats
                .iter()
                .all(|c| c.avail == Avail::Unavailable("reflexão não reconhecida".into()))
        );
    }

    #[test]
    fn disk_unreadable_makes_fields_unavailable() {
        let ue = ue_scan(GlobalState::Verified(1), GlobalState::Verified(2), Ok(()));
        let cheats = cheats_of(&[], &fields(), Some(&ue), &Err("causa".into()));
        assert!(
            cheats
                .iter()
                .all(|c| c.avail == Avail::Unavailable("disco ilegível".into()))
        );
    }
}
