//! Execução dos cheats de valor (spec 005, 6.4 a 6.6; feature `write`).
//!
//! A cada ciclo (~10 Hz), [`FieldRuntime::tick`] revalida o caminho até o personagem
//! ([`read_player`]), planeja as escritas ([`plan_tick`]) e as executa no motor: travas com
//! [`Engine::write_data`] e bits com [`Engine::set_bits`]. Antes de tocar os bits de um objeto
//! novo (outro pawn), a entrada do antigo é resolvida com [`Engine::restore_bits`], então o
//! diário nunca guarda duas entradas vivas do mesmo cheat.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use gt_core::Error as CoreError;
use gt_core::engine::{BatchReport, Engine};
use gt_core::journal::GuardBytes;
use gt_core::mem::ReadMem;
use gt_ue::{ObjectArray, ReflectionIndex};

use crate::player::{Absent, FieldAction, FieldCatalog, ResolvedRule, plan_tick, read_player};

/// Onde está o personagem neste ciclo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerState {
    Found,
    Absent(Absent),
}

/// Marca de um cheat ligado que não escreveu neste ciclo (spec 6.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Note {
    /// `aguardando o personagem`.
    Waiting,
    /// `alvo inválido`.
    InvalidTarget,
}

/// Resultado de um ciclo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TickReport {
    pub player: PlayerState,
    /// Por `id` ligado que não escreveu.
    pub notes: HashMap<String, Note>,
    /// Linhas para o registro, só nas transições.
    pub events: Vec<String>,
    /// Desde quando o `Pawn` está nulo (para a nova procura do controller, spec 6.3).
    pub pawn_null_since: Option<Instant>,
}

/// Estado dos cheats de valor de uma conexão.
pub struct FieldRuntime {
    catalog: FieldCatalog,
    /// Pawn do último ciclo com o personagem achado.
    last_pawn: Option<u64>,
    /// O personagem estava achado no último ciclo (`None` antes do primeiro).
    found: Option<bool>,
    /// `id` com o alvo inválido no último ciclo.
    invalid: HashSet<String>,
    pawn_null_since: Option<Instant>,
}

/// Objeto dono de uma guarda (o primeiro trecho é a vtable, em `+0`).
fn owner(guard: &[GuardBytes]) -> Option<u64> {
    guard.first().map(|g| g.addr)
}

impl FieldRuntime {
    pub fn new(catalog: FieldCatalog) -> Self {
        Self {
            catalog,
            last_pawn: None,
            found: None,
            invalid: HashSet::new(),
            pawn_null_since: None,
        }
    }

    pub fn catalog(&self) -> &FieldCatalog {
        &self.catalog
    }

    /// Um ciclo: lê o personagem pelo `controller` e escreve o que os cheats `on` pedem. Um
    /// `id` com resto pendente no motor é pulado, sem nota nova (spec 6.6).
    pub fn tick<M: ReadMem + ?Sized>(
        &mut self,
        engine: &mut Engine,
        mem: &M,
        index: &ReflectionIndex,
        objects: &ObjectArray,
        controller: u64,
        on: &[String],
    ) -> TickReport {
        let pending = engine.pending_ids();
        let on: Vec<String> = on
            .iter()
            .filter(|id| !pending.contains(id))
            .cloned()
            .collect();
        let mut report = TickReport {
            player: PlayerState::Found,
            notes: HashMap::new(),
            events: Vec::new(),
            pawn_null_since: None,
        };

        let snapshot = match read_player(mem, index, &self.catalog, objects, controller) {
            Ok(s) => s,
            Err(absent) => {
                if absent == Absent::PawnNull {
                    self.pawn_null_since.get_or_insert_with(Instant::now);
                } else {
                    self.pawn_null_since = None;
                }
                if self.found != Some(false) {
                    if self.found == Some(true) {
                        report.events.push("personagem perdido".to_string());
                    }
                    tracing::info!("personagem indisponível: {absent}");
                }
                self.found = Some(false);
                for id in &on {
                    report.notes.insert(id.clone(), Note::Waiting);
                }
                report.player = PlayerState::Absent(absent);
                report.pawn_null_since = self.pawn_null_since;
                return report;
            }
        };
        self.pawn_null_since = None;
        if self.found != Some(true) {
            report.events.push("personagem encontrado".to_string());
        }
        self.found = Some(true);
        if self.last_pawn != Some(snapshot.pawn) {
            // Objeto novo: o `maps` é relido antes de escrever nele.
            if let Err(e) = engine.refresh_data_maps() {
                tracing::warn!("falha ao reler o maps: {e}");
            }
            self.last_pawn = Some(snapshot.pawn);
        }

        let mut invalid_now = HashSet::new();
        for action in plan_tick(&snapshot, &self.catalog, &on) {
            match action {
                FieldAction::WriteF32 {
                    id,
                    addr,
                    value,
                    guard,
                } => {
                    if let Err(e) = engine.write_data(addr, &value.to_le_bytes(), &guard) {
                        tracing::debug!(id = %id, "trava não escrita: {e}");
                        report.notes.insert(id, Note::Waiting);
                    }
                }
                FieldAction::ClearBits {
                    id,
                    addr,
                    mask,
                    guard,
                } => {
                    if let Err(e) = self.clear_bits(engine, &id, addr, mask, &guard) {
                        tracing::debug!(id = %id, "bits não limpos: {e}");
                        report.notes.insert(id, Note::Waiting);
                    }
                }
                FieldAction::InvalidTarget { id, target } => {
                    if !self.invalid.contains(&id) {
                        let name = self.catalog.name(&id).unwrap_or(&id).to_string();
                        tracing::warn!(id = %id, target, "alvo fora da faixa; nada escrito");
                        report.events.push(format!("{name}: alvo inválido"));
                    }
                    report.notes.insert(id.clone(), Note::InvalidTarget);
                    invalid_now.insert(id);
                }
            }
        }
        self.invalid = invalid_now;
        report
    }

    /// Limpa os bits; se a entrada viva do `id` é de outro objeto, ela é resolvida antes.
    fn clear_bits(
        &mut self,
        engine: &mut Engine,
        id: &str,
        addr: u64,
        mask: u8,
        guard: &[GuardBytes],
    ) -> gt_core::Result<()> {
        let obj = owner(guard);
        let stale = engine
            .data_entries()
            .iter()
            .any(|e| e.id == id && owner(&e.guard) != obj);
        if stale {
            let r = engine.restore_bits(id)?;
            tracing::info!(
                id,
                restaurados = r.restored.len(),
                descartados = r.foreign.len(),
                "objeto novo: entrada do anterior resolvida"
            );
        }
        engine.set_bits(id, addr, mask, guard)
    }

    /// Desliga o cheat `id`: uma trava só para; os bits voltam ao original (spec 6.5).
    pub fn turn_off(&mut self, engine: &mut Engine, id: &str) -> gt_core::Result<BatchReport> {
        self.invalid.remove(id);
        match self.catalog.get(id) {
            Some(Ok(resolved)) if matches!(resolved.rule, ResolvedRule::ClearBits { .. }) => {
                engine.restore_bits(id)
            }
            Some(_) => Ok(BatchReport {
                restored: vec![id.to_string()],
                ..BatchReport::default()
            }),
            None => Err(CoreError::NotActive { id: id.to_string() }),
        }
    }
}
