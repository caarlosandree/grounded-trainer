//! Desvios com cave de uma sessão (spec 008, 6.11): a varredura, a conferência pelo índice da
//! reflexão e a execução no motor, para o Grounded 2.
//!
//! O [`HookCheats`] guarda as ocorrências que a varredura achou e o [`HookRuntime`] do
//! `gt-grounded2`, que só nasce quando há índice (precisa do `ClassPrivate` e dos campos). Como os
//! cheats de valor, os desvios ficam `verificando…` até o índice chegar; o motivo de um desvio
//! indisponível vem **sem** o `indisponível (…)` em volta (a tela é que o põe).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gt_core::engine::Engine;
use gt_core::maps::MapRegion;
use gt_core::mem::ReadMem;
use gt_core::modules::Module;
use gt_core::pe::DiskImage;
use gt_grounded2::Error;
use gt_grounded2::catalog::{HookEntry, HookHit, HookState};
use gt_grounded2::hooks::{ArmOutcome, HookAvail, HookEnv, HookEvent, HookRuntime, PawnView};
use gt_grounded2::signatures::{CaveBody, HookDef};
use gt_ue::ReflectionIndex;

use crate::backend::{ArmReport, OpOutcome};
use crate::model::Avail;
use crate::values::{Globals, ReflectionGate};

/// Motivo de uma varredura que não achou o padrão, que a nova busca tenta resolver.
const NOT_FOUND: &str = "não encontrado";

/// Desvios de uma sessão.
pub struct HookCheats {
    module: Module,
    maps: Vec<MapRegion>,
    /// A imagem do módulo no disco (para conferir os bytes do desvio) ou o motivo de não haver.
    disk: Result<Arc<DiskImage>, String>,
    /// O estado de cada `[[hook]]` na última varredura, na ordem do arquivo.
    entries: Vec<HookEntry>,
    runtime: Option<HookRuntime>,
    /// O que o `resolve` do runtime disse de cada desvio.
    resolved: HashMap<String, HookAvail>,
    /// As globais da última conferência estavam conferidas com o `rva` da versão.
    verified: bool,
}

impl HookCheats {
    pub fn new(
        module: Module,
        maps: Vec<MapRegion>,
        disk: Result<Arc<DiskImage>, String>,
        entries: Vec<HookEntry>,
    ) -> Self {
        Self {
            module,
            maps,
            disk,
            entries,
            runtime: None,
            resolved: HashMap::new(),
            verified: false,
        }
    }

    fn defs(&self) -> Vec<HookDef> {
        self.entries.iter().map(|e| e.def.clone()).collect()
    }

    /// Se `id` é um desvio desta sessão.
    pub fn is_hook(&self, id: &str) -> bool {
        self.entries.iter().any(|e| e.def.id == id)
    }

    /// O desvio `id` está instalado.
    pub fn is_hooked(&self, id: &str) -> bool {
        self.runtime.as_ref().is_some_and(|r| r.is_hooked(id))
    }

    /// Junta uma nova varredura: cada entrada de `new` cujo `id` está em `ids` (com `None`,
    /// todas) substitui a de mesmo `id`. Um desvio instalado não se toca (o padrão dele não está
    /// mais na memória) e um desvio já achado que a nova varredura não acha mantém o que tinha
    /// (spec 003, 6.2).
    pub fn merge_scan(&mut self, new: Vec<HookEntry>, ids: Option<&[String]>) {
        for entry in new {
            let id = entry.def.id.clone();
            if ids.is_some_and(|list| !list.contains(&id)) || self.is_hooked(&id) {
                continue;
            }
            let Some(slot) = self.entries.iter_mut().find(|e| e.def.id == id) else {
                self.entries.push(entry);
                continue;
            };
            let lost = matches!(&entry.state, HookState::Unavailable(r) if r == NOT_FOUND);
            if lost && matches!(slot.state, HookState::Found { .. }) {
                continue;
            }
            *slot = entry;
        }
    }

    /// As ocorrências achadas, com os bytes que as caves leem.
    fn hits(&self) -> Vec<HookHit> {
        self.entries
            .iter()
            .filter_map(|e| match &e.state {
                HookState::Found { addr, captures } => Some(HookHit {
                    def: e.def.clone(),
                    addr: *addr,
                    captures: captures.clone(),
                }),
                HookState::Unavailable(_) => None,
            })
            .collect()
    }

    /// Confere as ocorrências com o índice da reflexão (e reserva as caves que o motor já tem,
    /// num runtime novo sobre um motor que já escreveu). Idempotente: chamar de novo (índice
    /// melhor, nova varredura) mantém o que já foi escrito.
    pub fn resolve<M: ReadMem + ?Sized>(
        &mut self,
        mem: &M,
        engine: Option<&Engine>,
        index: &ReflectionIndex,
        globals: &Globals,
    ) {
        let Ok(disk) = &self.disk else {
            return;
        };
        let defs = self.defs();
        let hits = self.hits();
        let runtime = self.runtime.get_or_insert_with(|| {
            HookRuntime::new(HookEnv::new(
                self.module.clone(),
                disk.clone(),
                &self.maps,
                globals.sigs.layout,
                globals.sigs.player.clone(),
            ))
        });
        if let Some(engine) = engine {
            runtime.recognize_installed(engine, &defs);
        }
        self.resolved = runtime.resolve(&hits, mem, index).into_iter().collect();
        self.verified = globals.verified;
    }

    /// Disponibilidade de cada desvio. Um desvio instalado fica disponível mesmo sem o padrão na
    /// memória; um que a varredura não achou mostra o motivo dela; os achados esperam o índice.
    pub fn avail(&self, gate: &ReflectionGate) -> Vec<(String, Avail)> {
        self.entries
            .iter()
            .map(|e| {
                let id = e.def.id.clone();
                let avail = if self.is_hooked(&id) {
                    Avail::Available {
                        verified: self.verified,
                    }
                } else {
                    self.avail_of(e, gate)
                };
                (id, avail)
            })
            .collect()
    }

    fn avail_of(&self, e: &HookEntry, gate: &ReflectionGate) -> Avail {
        if let HookState::Unavailable(reason) = &e.state {
            return Avail::Unavailable(reason.clone());
        }
        if let Err(reason) = &self.disk {
            return Avail::Unavailable(reason.clone());
        }
        match gate {
            ReflectionGate::Unavailable(reason) => Avail::Unavailable(reason.clone()),
            ReflectionGate::Checking => Avail::Checking,
            ReflectionGate::Ready { .. } => match self.resolved.get(&e.def.id) {
                Some(HookAvail::Available) => Avail::Available {
                    verified: self.verified,
                },
                Some(HookAvail::Unavailable(reason)) => Avail::Unavailable(reason.clone()),
                // O índice chegou, mas a conferência ainda não rodou.
                None => Avail::Checking,
            },
        }
    }

    /// O `id` do desvio a que a entrada `engine_id` do motor (`<id>-cave` ou `<id>-hook`) pertence.
    pub fn row_of_engine_id(&self, engine_id: &str) -> Option<String> {
        let base = engine_id
            .strip_suffix("-hook")
            .or_else(|| engine_id.strip_suffix("-cave"))?;
        self.is_hook(base).then(|| base.to_string())
    }

    /// Se algum desvio instalado depende do personagem (a coleta).
    pub fn wants_pawn(&self) -> bool {
        self.entries
            .iter()
            .any(|e| e.def.body == CaveBody::Collect && self.is_hooked(&e.def.id))
    }

    pub fn enable(&mut self, engine: &mut Engine, id: &str, k: u64) -> OpOutcome {
        let started = Instant::now();
        let Some(runtime) = self.runtime.as_mut() else {
            return OpOutcome::Refused(crate::model::CHECKING.to_string());
        };
        let res = runtime.enable(engine, id, Some(k));
        outcome_of(res, id, engine, started.elapsed())
    }

    pub fn disable(&mut self, engine: &mut Engine, id: &str) -> OpOutcome {
        let started = Instant::now();
        // Sem runtime nada foi instalado por esta sessão.
        let Some(runtime) = self.runtime.as_mut() else {
            return OpOutcome::Done(Duration::ZERO);
        };
        let res = runtime.disable(engine, id);
        outcome_of(res, id, engine, started.elapsed())
    }

    pub fn set_value(&mut self, engine: &mut Engine, id: &str, k: u64) -> OpOutcome {
        let started = Instant::now();
        let Some(runtime) = self.runtime.as_mut() else {
            return OpOutcome::Refused(crate::model::CHECKING.to_string());
        };
        let res = runtime.set_value(engine, id, k);
        outcome_of(res, id, engine, started.elapsed())
    }

    /// Arma o arrastar com o ponto `k` (ou o desarma, se já está armado).
    pub fn arm(&mut self, engine: &mut Engine, k: u64) -> ArmReport {
        let Some(runtime) = self.runtime.as_mut() else {
            return ArmReport::Refused(crate::model::CHECKING.to_string());
        };
        // O desfecho não traz a quantidade: lê-se o que estava armado antes.
        let before = runtime.armed().map_or(0, |(n, _)| n);
        match runtime.arm(engine, k) {
            Ok(ArmOutcome::Armed) => ArmReport::Armed(runtime.armed().map_or(0, |(n, _)| n)),
            Ok(ArmOutcome::Disarmed) => ArmReport::Disarmed(before),
            Ok(ArmOutcome::Applied) => ArmReport::Applied(before),
            Err(e) => match outcome_of(Err(e), drag_id(&self.entries), engine, Duration::ZERO) {
                OpOutcome::Gone => ArmReport::Gone,
                OpOutcome::Residue(cause) => ArmReport::Residue(cause),
                OpOutcome::Refused(reason) => ArmReport::Refused(reason),
                // `outcome_of` não produz os outros dois; recusa por garantia.
                OpOutcome::Done(_) | OpOutcome::Foreign => {
                    ArmReport::Refused("falha ao armar".to_string())
                }
            },
        }
    }

    /// Um ciclo de manutenção dos desvios.
    pub fn tick<M: ReadMem + ?Sized>(
        &mut self,
        mem: &M,
        engine: &mut Engine,
        pawn: Option<u64>,
        now: Instant,
    ) -> Vec<HookEvent> {
        let Some(runtime) = self.runtime.as_mut() else {
            return Vec::new();
        };
        runtime.tick(engine, mem, pawn.map(PawnView::from), now)
    }
}

/// O `id` do desvio do arrastar (para o texto de um erro; sem ele, vazio).
fn drag_id(entries: &[HookEntry]) -> &str {
    entries
        .iter()
        .find(|e| e.def.body == CaveBody::Drag)
        .map_or("", |e| e.def.id.as_str())
}

/// Classifica o resultado de uma operação do runtime sobre o desvio `id`. Uma falha que deixa
/// entradas do desvio pendentes no motor é resto (só a restauração resolve).
fn outcome_of(
    res: gt_grounded2::Result<()>,
    id: &str,
    engine: &Engine,
    took: Duration,
) -> OpOutcome {
    match res {
        Ok(()) => OpOutcome::Done(took),
        Err(Error::Core(gt_core::Error::ProcessGone { .. })) => OpOutcome::Gone,
        Err(e) => {
            let ours = [format!("{id}-hook"), format!("{id}-cave")];
            if engine.pending_ids().iter().any(|p| ours.contains(p)) {
                OpOutcome::Residue(e.to_string())
            } else {
                OpOutcome::Refused(e.to_string())
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use gt_grounded2::signatures::Signatures;

    fn found(def: &HookDef) -> HookEntry {
        HookEntry {
            def: def.clone(),
            state: HookState::Found {
                addr: 0x1000,
                captures: vec![0x90; 64],
            },
        }
    }

    fn unavailable(def: &HookDef, reason: &str) -> HookEntry {
        HookEntry {
            def: def.clone(),
            state: HookState::Unavailable(reason.to_string()),
        }
    }

    fn defs() -> Vec<HookDef> {
        Signatures::embedded().unwrap().hooks().to_vec()
    }

    fn cheats(entries: Vec<HookEntry>) -> HookCheats {
        HookCheats::new(
            Module::default(),
            Vec::new(),
            Err("disco ilegível".to_string()),
            entries,
        )
    }

    fn avail_of(c: &HookCheats, gate: &ReflectionGate, id: &str) -> Avail {
        c.avail(gate)
            .into_iter()
            .find(|(i, _)| i == id)
            .map(|(_, a)| a)
            .unwrap()
    }

    #[test]
    fn avail_shows_the_scan_reason_before_anything_else() {
        let defs = defs();
        let c = cheats(vec![
            unavailable(&defs[0], "ambíguo"),
            found(&defs[1]),
            unavailable(&defs[2], "não encontrado"),
        ]);
        let gate = ReflectionGate::Checking;
        assert_eq!(
            avail_of(&c, &gate, &defs[0].id),
            Avail::Unavailable("ambíguo".to_string())
        );
        // Achado, mas sem a imagem do disco: o motivo dela, mesmo com o índice pronto.
        let ready = ReflectionGate::Ready { verified: true };
        assert_eq!(
            avail_of(&c, &ready, &defs[1].id),
            Avail::Unavailable("disco ilegível".to_string())
        );
        assert_eq!(
            avail_of(&c, &ready, &defs[2].id),
            Avail::Unavailable("não encontrado".to_string())
        );
    }

    #[test]
    fn merge_scan_keeps_found_when_the_new_scan_loses_it() {
        let defs = defs();
        let mut c = cheats(vec![
            found(&defs[0]),
            unavailable(&defs[1], "não encontrado"),
        ]);
        let ids = vec![defs[0].id.clone(), defs[1].id.clone()];
        c.merge_scan(
            vec![unavailable(&defs[0], "não encontrado"), found(&defs[1])],
            Some(&ids),
        );
        // O achado que a busca nova não acha fica como estava; o que faltava entra.
        assert!(matches!(c.entries[0].state, HookState::Found { .. }));
        assert!(matches!(c.entries[1].state, HookState::Found { .. }));
        // Um motivo que não é "não encontrado" substitui.
        c.merge_scan(vec![unavailable(&defs[0], "ambíguo")], None);
        assert_eq!(
            c.entries[0].state,
            HookState::Unavailable("ambíguo".to_string())
        );
        // Só os `id` da lista valem.
        c.merge_scan(vec![found(&defs[0])], Some(&[defs[1].id.clone()]));
        assert!(matches!(c.entries[0].state, HookState::Unavailable(_)));
    }

    #[test]
    fn engine_ids_map_back_to_the_row() {
        let defs = defs();
        let c = cheats(defs.iter().map(found).collect());
        let id = &defs[0].id;
        assert_eq!(c.row_of_engine_id(&format!("{id}-hook")), Some(id.clone()));
        assert_eq!(c.row_of_engine_id(&format!("{id}-cave")), Some(id.clone()));
        assert_eq!(c.row_of_engine_id("outro-hook"), None);
        assert_eq!(c.row_of_engine_id(id), None);
    }

    #[test]
    fn not_resolved_runtime_refuses_with_checking() {
        let defs = defs();
        let c = cheats(defs.iter().map(found).collect());
        assert!(!c.is_hooked(&defs[0].id) && !c.wants_pawn());
        assert!(c.is_hook(&defs[0].id) && !c.is_hook("outro"));
    }
}
