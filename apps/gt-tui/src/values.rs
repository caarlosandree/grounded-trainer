//! Cheats de valor de uma sessão (spec 005, 6.3, 6.6 e 6.7), comuns ao jogo e ao alvo de teste:
//! as globais da busca, o índice da reflexão, o controller achado e a execução no motor.
//!
//! A sessão só delega: o [`ValueCheats`] monta os trabalhos da tarefa de reflexão (que rodam
//! noutra thread, com cópias do que precisam), junta os resultados e faz a manutenção de cada
//! ciclo.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gt_core::engine::Engine;
use gt_core::mem::MemReader;
use gt_grounded2::fields::{ActionOutcome, FieldRuntime, PlayerState, TickReport};
use gt_grounded2::player::{
    Absent, ControllerFind, FieldCatalog, controller_needs_search, find_controller,
};
use gt_grounded2::signatures::{FieldDef, FieldRule, UeSigs};
use gt_grounded2::ue::{DISK_UNREADABLE, GlobalState, UeScan};
use gt_ue::{Names, ObjectArray, ReflectionIndex, ScanBudget, UeError};

use crate::backend::{
    CheatInfo, JobKind, OpOutcome, ReflectionJob, ReflectionNews, ReflectionOutcome, idle_tick,
    outcome_of,
};
use crate::model::Avail;

/// Motivo de uma conferência barata que falhou (spec 6.7).
pub const REFLECTION_UNRECOGNIZED: &str = "reflexão não reconhecida";
/// Motivo de um índice cuja conferência profunda falhou, ou ilegível.
const REFLECTION_INCOHERENT: &str = "reflexão incoerente";
/// Motivo de um índice que estourou o teto de tempo.
const REFLECTION_INCONCLUSIVE: &str = "reflexão inconclusiva";

/// As globais resolvidas, prontas para ler a reflexão.
#[derive(Debug, Clone)]
pub struct Globals {
    pub sigs: UeSigs,
    pub gobjects: u64,
    pub names: u64,
    /// As duas globais conferidas com o `rva` da versão (sem `?` nos cheats de valor).
    pub verified: bool,
}

/// Globais de uma busca: prontas, ou o motivo de os cheats de valor ficarem indisponíveis
/// (disco, globais e conferência barata, com os textos dos patches).
pub fn globals_of(ue: Option<&UeScan>, disk: &Result<(), String>) -> Result<Globals, String> {
    if disk.is_err() {
        return Err(DISK_UNREADABLE.to_string());
    }
    let Some(ue) = ue else {
        return Err(REFLECTION_UNRECOGNIZED.to_string());
    };
    let addr = |g: &GlobalState| match g {
        GlobalState::Verified(a) | GlobalState::Unverified(a) => Ok(*a),
        GlobalState::Unavailable(reason) => Err((*reason).to_string()),
    };
    let gobjects = addr(&ue.gobjects)?;
    let names = addr(&ue.names)?;
    if let Err(detail) = &ue.cheap {
        tracing::warn!("conferência barata da reflexão falhou: {detail}");
        return Err(REFLECTION_UNRECOGNIZED.to_string());
    }
    Ok(Globals {
        sigs: ue.sigs.clone(),
        gobjects,
        names,
        verified: matches!(ue.gobjects, GlobalState::Verified(_))
            && matches!(ue.names, GlobalState::Verified(_)),
    })
}

/// Disponibilidade dos cheats de valor logo depois da busca: `Checking` com as globais
/// prontas, ou o motivo.
pub fn search_avail(globals: &Result<Globals, String>) -> Avail {
    match globals {
        Ok(_) => Avail::Checking,
        Err(reason) => Avail::Unavailable(reason.clone()),
    }
}

/// As linhas dos cheats de valor para a busca, na ordem das definições.
pub fn field_infos(defs: &[FieldDef], avail: &Avail) -> Vec<CheatInfo> {
    defs.iter()
        .map(|d| CheatInfo {
            id: d.id.clone(),
            name: d.name.clone(),
            category: d.category,
            value: d.value.clone(),
            avail: avail.clone(),
            field: true,
            action: matches!(d.rule, FieldRule::Add { .. }),
        })
        .collect()
}

/// Índice e catálogo de uma conexão, e a execução dos cheats.
struct Reflection {
    index: Arc<ReflectionIndex>,
    catalog: Arc<FieldCatalog>,
    runtime: FieldRuntime,
}

/// Quantos cheats de valor o catálogo resolveu.
fn resolved_count(catalog: &FieldCatalog, defs: &[FieldDef]) -> usize {
    defs.iter().filter(|d| catalog.avail(&d.id).is_ok()).count()
}

impl Reflection {
    fn resolved(&self, defs: &[FieldDef]) -> usize {
        resolved_count(&self.catalog, defs)
    }
}

/// Resultado do trabalho `Index`.
type IndexData = Result<(ReflectionIndex, FieldCatalog), String>;
/// Resultado do trabalho `Controller`.
type ControllerData = Result<ControllerFind, String>;

fn index_reason(e: &UeError) -> String {
    match e {
        UeError::Deadline => REFLECTION_INCONCLUSIVE.to_string(),
        _ => REFLECTION_INCOHERENT.to_string(),
    }
}

/// Cheats de valor de uma sessão.
pub struct ValueCheats {
    reader: MemReader,
    defs: Vec<FieldDef>,
    globals: Result<Globals, String>,
    reflection: Option<Reflection>,
    /// Último motivo de um índice que falhou (enquanto não há índice bom).
    index_failure: Option<String>,
    controller: Option<u64>,
    pawn_null_since: Option<Instant>,
    /// Uma ação pediu a procura do controller e ela ainda não voltou.
    action_search: bool,
    /// Resultado da procura pedida pela ação, até o controlador buscar.
    controller_news: Option<bool>,
}

/// Motivo de a ação recusar com o motor parado (o SIGCONT que falhou).
pub const STUCK_REFUSAL: &str = "o jogo pode ter ficado parado; aperte r";

impl ValueCheats {
    pub fn new(reader: MemReader, defs: Vec<FieldDef>, globals: Result<Globals, String>) -> Self {
        Self {
            reader,
            defs,
            globals,
            reflection: None,
            index_failure: None,
            controller: None,
            pawn_null_since: None,
            action_search: false,
            controller_news: None,
        }
    }

    /// A definição do cheat de valor `id`.
    fn def(&self, id: &str) -> Option<&FieldDef> {
        self.defs.iter().find(|d| d.id == id)
    }

    /// Se `id` é um cheat de valor desta sessão.
    pub fn is_field(&self, id: &str) -> bool {
        self.def(id).is_some()
    }

    /// Passa o ponto `k` da grade de `id` para o valor desejado do motor de execução.
    fn desire(&mut self, id: &str, k: u64) {
        let Some(grid) = self.def(id).and_then(|d| d.value.clone()) else {
            return;
        };
        if let Some(r) = self.reflection.as_mut() {
            r.runtime.set_desired(id, grid.value(k));
        }
    }

    /// Troca o valor de um cheat de valor com grade: só muda o desejado, e a escrita vem no
    /// `tick` do próximo ciclo (spec 006, 6.5).
    pub fn set_value(&mut self, id: &str, k: u64) -> OpOutcome {
        match self.def(id) {
            Some(d) if d.value.is_some() && !matches!(d.rule, FieldRule::Add { .. }) => {}
            Some(_) => return OpOutcome::Refused(format!("{id} não tem valor")),
            None => return OpOutcome::Refused(format!("cheat desconhecido: {id}")),
        }
        if self.reflection.is_none() {
            return OpOutcome::Refused(crate::model::CHECKING.to_string());
        }
        self.desire(id, k);
        OpOutcome::Done(Duration::ZERO)
    }

    /// Executa a ação `id` (spec 006, 6.6, passos 1 a 5; a janela de 1 s e o registro são do
    /// controlador).
    pub fn action(&mut self, engine: Option<&mut Engine>, id: &str, k: u64) -> ActionOutcome {
        let Some(engine) = engine else {
            return ActionOutcome::Refused("sem motor".to_string());
        };
        if engine.is_stuck() {
            return ActionOutcome::Refused(STUCK_REFUSAL.to_string());
        }
        let amount = match self.def(id) {
            Some(d) if matches!(d.rule, FieldRule::Add { .. }) => {
                match d.value.as_ref().map(|g| g.value(k)) {
                    Some(v) if v.is_finite() && v >= 0.0 && v <= f64::from(i32::MAX) => v as i32,
                    _ => return ActionOutcome::Refused("quantidade inválida".to_string()),
                }
            }
            _ => return ActionOutcome::Refused(format!("{id} não é uma ação")),
        };
        let Ok(globals) = &self.globals else {
            return ActionOutcome::Refused(crate::model::CHECKING.to_string());
        };
        let Some(r) = self.reflection.as_mut() else {
            return ActionOutcome::Refused(crate::model::CHECKING.to_string());
        };
        if let Err(reason) = r.catalog.avail(id) {
            return ActionOutcome::Refused(reason);
        }
        let objects = match ObjectArray::read(&self.reader, &globals.sigs.layout, globals.gobjects)
        {
            Ok(o) => o,
            Err(e) => {
                tracing::debug!("tabela de objetos ilegível: {e}");
                return ActionOutcome::Refused("estado do jogo ausente".to_string());
            }
        };
        let out = r.runtime.run_action(
            engine,
            &self.reader,
            &r.index,
            &objects,
            self.controller,
            id,
            amount,
        );
        if out == ActionOutcome::NeedsController {
            self.controller = None;
            self.action_search = true;
        }
        out
    }

    /// Uma ação pediu a procura do controller e ela ainda não terminou.
    pub fn action_wants_controller(&self) -> bool {
        self.action_search
    }

    /// Resultado da procura pedida pela ação, entregue uma vez.
    pub fn take_controller_news(&mut self) -> Option<bool> {
        self.controller_news.take()
    }

    /// Globais de uma nova busca. Com um índice bom, nada muda: um índice já bom não é trocado
    /// por um pior (spec 6.7).
    pub fn update_globals(&mut self, globals: Result<Globals, String>) {
        if self.reflection.is_none() {
            self.globals = globals;
        }
    }

    pub fn job(&self, kind: JobKind) -> Option<ReflectionJob> {
        let globals = self.globals.as_ref().ok()?.clone();
        let reader = self.reader;
        match kind {
            JobKind::Index => {
                let defs = self.defs.clone();
                Some(ReflectionJob {
                    kind,
                    run: Box::new(move || {
                        let started = Instant::now();
                        let data: IndexData = build_index(reader, &globals, &defs);
                        ReflectionOutcome {
                            kind,
                            took: started.elapsed(),
                            data: Box::new(data),
                        }
                    }),
                })
            }
            JobKind::Controller => {
                let r = self.reflection.as_ref()?;
                let (index, catalog) = (r.index.clone(), r.catalog.clone());
                Some(ReflectionJob {
                    kind,
                    run: Box::new(move || {
                        let started = Instant::now();
                        let layout = globals.sigs.layout;
                        let data: ControllerData =
                            ObjectArray::read(&reader, &layout, globals.gobjects)
                                .and_then(|objects| {
                                    find_controller(
                                        &index,
                                        &reader,
                                        &objects,
                                        &catalog,
                                        &ScanBudget::standard(),
                                    )
                                })
                                .map_err(|e| e.to_string());
                        ReflectionOutcome {
                            kind,
                            took: started.elapsed(),
                            data: Box::new(data),
                        }
                    }),
                })
            }
        }
    }

    pub fn merge(&mut self, out: ReflectionOutcome) -> ReflectionNews {
        let mut news = ReflectionNews::default();
        match out.kind {
            JobKind::Index => {
                let Ok(data) = out.data.downcast::<IndexData>() else {
                    tracing::error!("resultado do índice de outro tipo");
                    return news;
                };
                match *data {
                    Ok((index, catalog)) => {
                        let resolved = resolved_count(&catalog, &self.defs);
                        let better = self
                            .reflection
                            .as_ref()
                            .is_none_or(|old| resolved > old.resolved(&self.defs));
                        if better {
                            let catalog = Arc::new(catalog);
                            // O estado do runtime velho (desejados, "o jogo não deixa", valor
                            // inválido, aprendizado) continua valendo; só o catálogo troca.
                            let runtime = match self.reflection.take() {
                                Some(old) => old.runtime.with_catalog(catalog.clone()),
                                None => FieldRuntime::new(catalog.clone()),
                            };
                            self.reflection = Some(Reflection {
                                index: Arc::new(index),
                                runtime,
                                catalog,
                            });
                            self.index_failure = None;
                            self.controller = None;
                            tracing::info!(took = ?out.took, resolved, "índice da reflexão montado");
                        }
                    }
                    Err(reason) => {
                        tracing::warn!(took = ?out.took, "índice da reflexão: {reason}");
                        if self.reflection.is_none() {
                            self.index_failure = Some(reason);
                        }
                    }
                }
                news.index_ok = self.reflection.is_some();
                news.retry = self
                    .reflection
                    .as_ref()
                    .is_none_or(|r| r.resolved(&self.defs) < self.defs.len());
            }
            JobKind::Controller => {
                let Ok(data) = out.data.downcast::<ControllerData>() else {
                    tracing::error!("resultado do controller de outro tipo");
                    return news;
                };
                news.index_ok = self.reflection.is_some();
                let state = match *data {
                    Ok(ControllerFind::Found(c)) => {
                        tracing::info!(took = ?out.took, controller = format_args!("{c:#x}"), "controller achado");
                        self.controller = Some(c);
                        self.pawn_null_since = None;
                        PlayerState::Found
                    }
                    Ok(ControllerFind::Absent) => {
                        self.controller = None;
                        PlayerState::Absent(Absent::NoController)
                    }
                    Ok(ControllerFind::Ambiguous) => {
                        self.controller = None;
                        PlayerState::Absent(Absent::ControllerAmbiguous)
                    }
                    Err(e) => {
                        tracing::warn!("procura do controller: {e}");
                        self.controller = None;
                        PlayerState::Absent(Absent::Unreadable)
                    }
                };
                if std::mem::take(&mut self.action_search) {
                    self.controller_news = Some(state == PlayerState::Found);
                }
                news.retry = state != PlayerState::Found;
                news.player_changed = Some(state);
            }
        }
        news
    }

    pub fn avail(&self) -> Vec<(String, Avail)> {
        self.defs
            .iter()
            .map(|d| {
                let avail = match (&self.globals, &self.reflection) {
                    (Err(reason), _) => Avail::Unavailable(reason.clone()),
                    (Ok(g), Some(r)) => match r.catalog.avail(&d.id) {
                        Ok(()) => Avail::Available {
                            verified: g.verified,
                        },
                        Err(reason) => Avail::Unavailable(reason),
                    },
                    (Ok(_), None) => match &self.index_failure {
                        Some(reason) => Avail::Unavailable(reason.clone()),
                        None => Avail::Checking,
                    },
                };
                (d.id.clone(), avail)
            })
            .collect()
    }

    pub fn needs_controller(&self, now: Instant) -> bool {
        self.reflection.is_some()
            && (self.controller.is_none()
                || controller_needs_search(true, self.pawn_null_since, now))
    }

    pub fn on(&mut self, engine: Option<&mut Engine>, id: &str, k: u64) -> OpOutcome {
        if engine.is_none() {
            return OpOutcome::Refused("sem motor".to_string());
        }
        if self
            .def(id)
            .is_some_and(|d| matches!(d.rule, FieldRule::Add { .. }))
        {
            return OpOutcome::Refused(format!("{id} é uma ação"));
        }
        let avail = match &self.reflection {
            None => return OpOutcome::Refused(crate::model::CHECKING.to_string()),
            Some(r) => r.catalog.avail(id),
        };
        match avail {
            // A escrita vem na manutenção do próximo ciclo (100 ms).
            Ok(()) => {
                self.desire(id, k);
                OpOutcome::Done(Duration::ZERO)
            }
            Err(reason) => OpOutcome::Refused(reason),
        }
    }

    pub fn off(&mut self, engine: Option<&mut Engine>, id: &str) -> OpOutcome {
        let Some(engine) = engine else {
            return OpOutcome::Refused("sem motor".to_string());
        };
        let Some(r) = self.reflection.as_mut() else {
            // Sem índice nada foi escrito por esta sessão.
            return OpOutcome::Done(Duration::ZERO);
        };
        let res = r.runtime.turn_off(engine, id);
        let pending = engine.pending_ids();
        outcome_of(res, id, &pending)
    }

    pub fn tick(&mut self, engine: Option<&mut Engine>, on: &[String], now: Instant) -> TickReport {
        let Some(engine) = engine else {
            return idle_tick();
        };
        if engine.is_stuck() || on.is_empty() {
            return idle_tick();
        }
        let (Ok(globals), Some(r)) = (&self.globals, self.reflection.as_mut()) else {
            return idle_tick();
        };
        let waiting = |absent: Absent| {
            let mut report = idle_tick();
            report.player = PlayerState::Absent(absent);
            report.notes = on
                .iter()
                .map(|id| (id.clone(), gt_grounded2::fields::Note::Waiting))
                .collect();
            report
        };
        let Some(controller) = self.controller else {
            return waiting(Absent::NoController);
        };
        let objects = match ObjectArray::read(&self.reader, &globals.sigs.layout, globals.gobjects)
        {
            Ok(o) => o,
            Err(e) => {
                tracing::debug!("tabela de objetos ilegível: {e}");
                return waiting(Absent::Unreadable);
            }
        };
        let report = r.runtime.tick(
            engine,
            &self.reader,
            &r.index,
            &objects,
            controller,
            on,
            now,
        );
        match report.player {
            PlayerState::Absent(Absent::ControllerInvalid) => self.controller = None,
            PlayerState::Absent(Absent::PawnNull) => {
                self.pawn_null_since = report.pawn_null_since;
                if controller_needs_search(true, self.pawn_null_since, now) {
                    // Controller antigo depois de carregar outro save: procura de novo.
                    self.controller = None;
                }
            }
            _ => self.pawn_null_since = None,
        }
        report
    }
}

/// Monta o índice e resolve o catálogo (roda na thread da tarefa de reflexão).
fn build_index(reader: MemReader, g: &Globals, defs: &[FieldDef]) -> IndexData {
    let layout = g.sigs.layout;
    let names = Names::new(&reader, g.names).map_err(|e| index_reason(&e))?;
    let objects = ObjectArray::read(&reader, &layout, g.gobjects).map_err(|e| index_reason(&e))?;
    let index = ReflectionIndex::build(&reader, &layout, &objects, &names, &ScanBudget::standard())
        .map_err(|e| {
            tracing::warn!("índice da reflexão: {e}");
            index_reason(&e)
        })?;
    if let Err(detail) = index.deep_check() {
        tracing::warn!("conferência profunda da reflexão: {detail}");
        return Err(REFLECTION_INCOHERENT.to_string());
    }
    let catalog = FieldCatalog::resolve(&index, &reader, &g.sigs.player, defs);
    Ok((index, catalog))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use gt_grounded2::signatures::FieldRule;
    use gt_testkit::synth_signatures;
    use gt_ue::synth::SparseMem;

    /// Índice e catálogo da cena sintética do alvo de teste, resolvido com `defs`.
    fn index_data(defs: &[FieldDef]) -> IndexData {
        let (image, _) = gt_testkit::ue::build(0x1000_0000, gt_testkit::ue::ARENA_LEN);
        let mem = SparseMem::from_image(&image);
        let (sigs, _) = synth_signatures();
        let names = Names::new(&mem, image.names).unwrap();
        let objects = ObjectArray::read(&mem, &sigs.layout, image.gobjects).unwrap();
        let index = ReflectionIndex::build(
            &mem,
            &sigs.layout,
            &objects,
            &names,
            &ScanBudget::standard(),
        )
        .unwrap();
        let catalog = FieldCatalog::resolve(&index, &mem, &sigs.player, defs);
        Ok((index, catalog))
    }

    fn outcome(data: IndexData) -> ReflectionOutcome {
        ReflectionOutcome {
            kind: JobKind::Index,
            took: Duration::from_millis(5),
            data: Box::new(data),
        }
    }

    fn cheats() -> ValueCheats {
        let (sigs, defs) = synth_signatures();
        let globals = Ok(Globals {
            sigs,
            gobjects: 1,
            names: 2,
            verified: false,
        });
        ValueCheats::new(MemReader::new(1), defs, globals)
    }

    fn available_count(v: &ValueCheats) -> usize {
        v.avail()
            .iter()
            .filter(|(_, a)| matches!(a, Avail::Available { .. }))
            .count()
    }

    #[test]
    fn checking_until_index_then_available_unverified() {
        let mut v = cheats();
        assert!(v.avail().iter().all(|(_, a)| *a == Avail::Checking));
        let (_, defs) = synth_signatures();
        let news = v.merge(outcome(index_data(&defs)));
        assert!(news.index_ok && !news.retry);
        // Globais sem `rva` conferido: disponível com `?`.
        assert!(
            v.avail()
                .iter()
                .all(|(_, a)| *a == Avail::Available { verified: false })
        );
    }

    #[test]
    fn good_index_not_replaced_by_worse() {
        let mut v = cheats();
        let (_, defs) = synth_signatures();
        v.merge(outcome(index_data(&defs)));
        assert_eq!(available_count(&v), defs.len());
        // Um índice pior (um campo que falta) e uma falha não trocam o bom.
        let mut worse = defs.clone();
        if let FieldRule::Lock { field, .. } = &mut worse[0].rule {
            *field = "NaoExiste".into();
        }
        let news = v.merge(outcome(index_data(&worse)));
        assert!(news.index_ok);
        assert_eq!(available_count(&v), defs.len());
        let news = v.merge(outcome(Err(REFLECTION_INCOHERENT.to_string())));
        assert!(news.index_ok && !news.retry);
        assert_eq!(available_count(&v), defs.len());
    }

    #[test]
    fn better_index_keeps_desired_values() {
        let mut v = cheats();
        let (_, defs) = synth_signatures();
        // Primeiro um índice parcial (um `Lock` quebrado); `carga` resolve.
        let mut partial = defs.clone();
        let broken = partial
            .iter_mut()
            .find_map(|d| match &mut d.rule {
                FieldRule::Lock { field, .. } => Some(field),
                _ => None,
            })
            .unwrap();
        *broken = "NaoExiste".into();
        v.merge(outcome(index_data(&partial)));
        let grid = defs
            .iter()
            .find(|d| d.id == "carga")
            .and_then(|d| d.value.clone())
            .unwrap();
        let k = grid.default_k.saturating_add(1).min(grid.steps());
        assert!(matches!(v.set_value("carga", k), OpOutcome::Done(_)));
        let wanted = grid.value(k);
        assert_ne!(wanted, grid.value(grid.default_k));
        // Chega o índice completo (melhor): o valor desejado continua o mostrado.
        v.merge(outcome(index_data(&defs)));
        assert_eq!(available_count(&v), defs.len());
        let kept = v
            .reflection
            .as_ref()
            .and_then(|r| r.runtime.desired_values().get("carga").copied());
        assert_eq!(kept, Some(wanted));
    }

    #[test]
    fn index_failure_reason_until_a_good_one() {
        let mut v = cheats();
        let news = v.merge(outcome(Err(REFLECTION_INCONCLUSIVE.to_string())));
        assert!(!news.index_ok && news.retry);
        assert!(
            v.avail()
                .iter()
                .all(|(_, a)| *a == Avail::Unavailable(REFLECTION_INCONCLUSIVE.to_string()))
        );
        // Índice parcial: os que faltam mostram o motivo, e a tarefa tenta de novo.
        let (_, mut defs) = synth_signatures();
        if let FieldRule::Lock { field, .. } = &mut defs[1].rule {
            *field = "CurrentFoo".into();
        }
        let news = v.merge(outcome(index_data(&defs)));
        assert!(news.index_ok && news.retry);
        let avail = v.avail();
        assert_eq!(
            avail[1].1,
            Avail::Unavailable("campo CurrentFoo não encontrado".to_string())
        );
    }

    #[test]
    fn field_infos_carry_value_and_action() {
        let (_, defs) = synth_signatures();
        let infos = field_infos(&defs, &Avail::Checking);
        let find = |id: &str| infos.iter().find(|i| i.id == id).unwrap();
        assert!(find("somar-ciencia").action && find("somar-ciencia").value.is_some());
        assert!(!find("carga").action && find("carga").value.is_some());
        assert!(!find("vida-infinita").action && find("vida-infinita").value.is_none());
    }

    #[test]
    fn globals_reasons() {
        let disk_err: Result<(), String> = Err("x".into());
        assert_eq!(
            globals_of(None, &disk_err).unwrap_err(),
            DISK_UNREADABLE.to_string()
        );
        assert_eq!(
            globals_of(None, &Ok(())).unwrap_err(),
            REFLECTION_UNRECOGNIZED.to_string()
        );
    }
}
