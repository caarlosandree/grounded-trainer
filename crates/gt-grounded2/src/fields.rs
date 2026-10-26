//! Execução dos cheats de valor (spec 005, 6.4 a 6.6; feature `write`).
//!
//! A cada ciclo (~10 Hz), [`FieldRuntime::tick`] revalida o caminho até o personagem
//! ([`read_player`]), planeja as escritas ([`plan_tick`]) e as executa no motor: travas com
//! [`Engine::write_data`] e bits com [`Engine::set_bits`]. Antes de tocar os bits de um objeto
//! novo (outro pawn), a entrada do antigo é resolvida com [`Engine::restore_bits`], então o
//! diário nunca guarda duas entradas vivas do mesmo cheat.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gt_core::Error as CoreError;
use gt_core::engine::{BatchReport, Engine, SetOutcome};
use gt_core::journal::GuardBytes;
use gt_core::mem::ReadMem;
use gt_core::value::ValueGrid;
use gt_ue::read::read_u32;
use gt_ue::{ObjectArray, ReflectionIndex};

use crate::player::{
    Absent, AddRefusal, FieldAction, FieldCatalog, FieldValues, GameStateAbsent, MissingKind,
    PlayerSnapshot, ResolvedField, ResolvedRule, encode_value, missing_kind, plan_add, plan_tick,
    read_game_state, read_player, value_is_sane,
};
use crate::signatures::{FieldKind, LockMode};

/// Originais novos tolerados na janela antes de o jogo contar como "brigando" (spec 006, 6.4).
const FIGHT_LIMIT: usize = 5;
/// Janela deslizante da briga com o jogo.
const FIGHT_WINDOW: Duration = Duration::from_secs(10);

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
    /// `aguardando o pet`: o personagem está, mas o pet (outro ator) não.
    WaitingPet,
    /// `alvo inválido`.
    InvalidTarget,
    /// `valor do jogo inválido`: o valor lido do jogo num `set` não é são; nada é escrito.
    InvalidGameValue,
    /// `o jogo não deixa`: o jogo reescreve o valor o tempo todo; o `set` parou neste objeto.
    GameRefuses,
}

/// Resultado de uma execução da ação de somar (spec 006, 6.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionOutcome {
    /// Somou e a releitura confirmou.
    Done { before: i32, after: i32 },
    /// Recusada nos passos 1 a 4, com o texto para o registro.
    Refused(String),
    /// O controller está ausente ou inválido: quem chama pede a procura.
    NeedsController,
    /// A releitura não bateu: o jogo mudou o valor durante a escrita (valor lido). Sem nova tentativa.
    Raced { read: i32 },
}

/// Janela de briga de um `set` com o jogo, por cheat e objeto.
#[derive(Debug, Default)]
struct Fight {
    /// Objeto a que a janela se refere.
    obj: u64,
    /// Quando o jogo mudou o valor (originais novos) dentro da janela.
    news: VecDeque<Instant>,
    /// O cheat desistiu de escrever neste objeto.
    blocked: bool,
}

impl Fight {
    fn new(obj: u64) -> Self {
        Self {
            obj,
            ..Self::default()
        }
    }

    /// Registra um original novo em `now`; `true` se já passa do limite da janela.
    fn record(&mut self, now: Instant) -> bool {
        while let Some(&first) = self.news.front() {
            if now.saturating_duration_since(first) >= FIGHT_WINDOW {
                self.news.pop_front();
            } else {
                break;
            }
        }
        self.news.push_back(now);
        self.news.len() > FIGHT_LIMIT
    }
}

/// O valor `v` com as casas e a unidade da grade (ou sem grade, simples).
fn format_value(grid: Option<&ValueGrid>, kind: FieldKind, v: f64) -> String {
    match grid {
        // Uma grade de um ponto só, em `v`, reaproveita a formatação sem prender `v` à faixa.
        Some(g) => ValueGrid {
            min: v,
            max: v,
            step: 1.0,
            default_k: 0,
            ..g.clone()
        }
        .format(0),
        None => match kind {
            FieldKind::Int => format!("{v:.0}"),
            FieldKind::Float => format!("{v}"),
        },
    }
}

/// Os 4 bytes de um campo como número (para mostrar).
fn decode_value(kind: FieldKind, bytes: &[u8]) -> f64 {
    let raw: [u8; 4] = bytes.try_into().unwrap_or_default();
    match kind {
        FieldKind::Int => f64::from(i32::from_le_bytes(raw)),
        FieldKind::Float => f64::from(f32::from_le_bytes(raw)),
    }
}

/// Guarda de identidade do objeto `obj` neste ciclo.
fn guard_of(snapshot: &PlayerSnapshot, obj: u64) -> Vec<GuardBytes> {
    snapshot
        .objects
        .iter()
        .find(|o| o.addr == obj)
        .map(|o| o.guard.clone())
        .unwrap_or_default()
}

/// Escreve um campo de `set`. Se a entrada viva do par é de outro objeto, ela é resolvida antes
/// (`restore_field`) e a escrita é tentada de novo, uma vez.
fn write_set_field(
    engine: &mut Engine,
    id: &str,
    addr: u64,
    desired: [u8; 4],
    guard: &[GuardBytes],
    kind: FieldKind,
) -> gt_core::Result<SetOutcome> {
    let accept =
        move |bytes: &[u8]| <[u8; 4]>::try_from(bytes).is_ok_and(|raw| value_is_sane(kind, raw));
    match engine.set_word(id, addr, &desired, guard, &accept) {
        Err(CoreError::DataEntryOtherObject { .. }) => {
            engine.restore_field(id)?;
            engine.set_word(id, addr, &desired, guard, &accept)
        }
        other => other,
    }
}

/// A marca de um cheat ligado cujo objeto falta: `aguardando o pet` ou `aguardando o personagem`.
pub fn missing_note(kind: MissingKind) -> Note {
    match kind {
        MissingKind::Pet => Note::WaitingPet,
        MissingKind::Component => Note::Waiting,
    }
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
    catalog: Arc<FieldCatalog>,
    /// Pawn do último ciclo com o personagem achado.
    last_pawn: Option<u64>,
    /// O personagem estava achado no último ciclo (`None` antes do primeiro).
    found: Option<bool>,
    /// `id` com o alvo inválido no último ciclo.
    invalid: HashSet<String>,
    pawn_null_since: Option<Instant>,
    /// Maior valor visto de cada trava de subir, por `id` e objeto, enquanto ligada: o máximo
    /// efetivo que o próprio jogo alcança (a stamina regenera até 120 com `MaxStamina` 100).
    learned: HashMap<(String, u64), f32>,
    /// Valor desejado de cada `set` com `[field.value]` (ou com `target` trocado), por `id`.
    desired: HashMap<String, f64>,
    /// Janela de briga e bloqueio de cada `set` ligado, por `id`.
    fights: HashMap<String, Fight>,
    /// `set` com o valor do jogo inválido no último ciclo (a linha do registro sai na transição).
    bad_value: HashSet<String>,
    /// Último endereço do objeto de cada ação, para reler o `maps` quando ele muda.
    action_objects: HashMap<String, u64>,
}

/// O que o ciclo precisa saber de um `set` ligado.
#[derive(Clone, Copy)]
struct SetJob<'a> {
    id: &'a str,
    kind: FieldKind,
    target: Option<f64>,
    obj: u64,
    fields: &'a [(u64, [u8; 4])],
}

/// Objeto dono de uma guarda (o primeiro trecho é a vtable, em `+0`).
fn owner(guard: &[GuardBytes]) -> Option<u64> {
    guard.first().map(|g| g.addr)
}

impl FieldRuntime {
    /// O catálogo pode vir num `Arc`, para ser lido também pela procura do controller.
    pub fn new(catalog: impl Into<Arc<FieldCatalog>>) -> Self {
        Self {
            catalog: catalog.into(),
            last_pawn: None,
            found: None,
            invalid: HashSet::new(),
            pawn_null_since: None,
            learned: HashMap::new(),
            desired: HashMap::new(),
            fights: HashMap::new(),
            bad_value: HashSet::new(),
            action_objects: HashMap::new(),
        }
    }

    /// Define o valor desejado de um `set` com `[field.value]` (a grade manda; o `target`, se
    /// houver, é só o padrão). O próximo ciclo troca o valor nos campos.
    pub fn set_desired(&mut self, id: &str, value: f64) {
        self.desired.insert(id.to_string(), value);
    }

    /// Valor desejado do `set` `id`: o pedido, senão o `target`, senão o padrão da grade.
    fn desired_value(&self, id: &str, target: Option<f64>) -> Option<f64> {
        self.desired
            .get(id)
            .copied()
            .or(target)
            .or_else(|| self.catalog.grid(id).map(|g| g.value(g.default_k)))
    }

    /// Nas travas de subir, o alvo nunca é menor que o maior valor já visto naquele objeto com o
    /// cheat ligado (o máximo efetivo, spec 005 6.5.1 e P3). Valor não finito ou fora da faixa
    /// sã não ensina nada.
    fn learn_targets(&mut self, snapshot: &mut PlayerSnapshot, on: &[String]) {
        self.learned.retain(|(id, _), _| on.contains(id));
        for (id, values) in snapshot.values.iter_mut() {
            if !on.contains(id) {
                continue;
            }
            let Some(Ok(resolved)) = self.catalog.get(id) else {
                continue;
            };
            let ResolvedRule::Lock {
                mode: LockMode::Raise,
                max,
                ..
            } = resolved.rule
            else {
                continue;
            };
            let FieldValues::Lock {
                obj,
                current,
                target,
                ..
            } = values
            else {
                continue;
            };
            let best = self.learned.entry((id.clone(), *obj)).or_insert(f32::MIN);
            if current.is_finite() && *current <= max {
                *best = best.max(*current);
            }
            if target.is_finite() && *best > *target {
                *target = *best;
            }
        }
    }

    pub fn catalog(&self) -> &FieldCatalog {
        &self.catalog
    }

    /// Um ciclo: lê o personagem pelo `controller` e escreve o que os cheats `on` pedem. Um
    /// `id` com resto pendente no motor é pulado, sem nota nova (spec 6.6).
    #[allow(clippy::too_many_arguments)]
    pub fn tick<M: ReadMem + ?Sized>(
        &mut self,
        engine: &mut Engine,
        mem: &M,
        index: &ReflectionIndex,
        objects: &ObjectArray,
        controller: u64,
        on: &[String],
        now: Instant,
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

        let mut snapshot = match read_player(mem, index, &self.catalog, objects, controller) {
            Ok(s) => s,
            Err(absent) => {
                if absent == Absent::PawnNull {
                    self.pawn_null_since.get_or_insert(now);
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
        self.learn_targets(&mut snapshot, &on);
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
        let catalog = Arc::clone(&self.catalog);
        for id in catalog.ids().filter(|id| on.iter().any(|o| o == id)) {
            let (Some(Ok(resolved)), Some(FieldValues::Set { obj, fields })) =
                (catalog.get(id), snapshot.values.get(id))
            else {
                continue;
            };
            let ResolvedRule::Set { kind, target, .. } = &resolved.rule else {
                continue;
            };
            let job = SetJob {
                id,
                kind: *kind,
                target: *target,
                obj: *obj,
                fields,
            };
            self.tick_set(engine, &snapshot, &job, now, &mut report);
        }
        for id in snapshot.missing.keys().filter(|id| on.contains(id)) {
            let kind = match self.catalog.get(id) {
                Some(Ok(resolved)) => missing_kind(resolved),
                _ => MissingKind::Component,
            };
            report.notes.insert(id.clone(), missing_note(kind));
        }
        report
    }

    /// Um ciclo de um `set` ligado: escreve o valor desejado em cada campo, aprende o que o jogo
    /// mudou e desiste se o jogo briga (spec 006, 6.4). Uma falha de um campo não impede o outro.
    fn tick_set(
        &mut self,
        engine: &mut Engine,
        snapshot: &PlayerSnapshot,
        job: &SetJob<'_>,
        now: Instant,
        report: &mut TickReport,
    ) {
        let (id, obj) = (job.id, job.obj);
        let mut fight = self.fights.remove(id).unwrap_or_default();
        if fight.obj != obj {
            // Objeto novo: a janela e o bloqueio do anterior não valem mais.
            fight = Fight::new(obj);
        }
        let outcome = self.run_set_fields(engine, snapshot, job, &mut fight, now, report);
        if fight.blocked {
            self.bad_value.remove(id);
            report.notes.insert(id.to_string(), Note::GameRefuses);
        } else if let Some(note) = outcome {
            report.notes.insert(id.to_string(), note);
        }
        self.fights.insert(id.to_string(), fight);
    }

    /// Escreve os campos do `set` e devolve a marca do ciclo, se houver. Bloqueado, não faz nada.
    fn run_set_fields(
        &mut self,
        engine: &mut Engine,
        snapshot: &PlayerSnapshot,
        job: &SetJob<'_>,
        fight: &mut Fight,
        now: Instant,
        report: &mut TickReport,
    ) -> Option<Note> {
        let SetJob {
            id,
            kind,
            target,
            obj,
            fields,
        } = *job;
        if fight.blocked {
            return None;
        }
        let desired = encode_value(kind, self.desired_value(id, target)?);
        let guard = guard_of(snapshot, obj);
        let name = self.catalog.name(id).unwrap_or(id).to_string();
        let grid = self.catalog.grid(id);

        // Objeto novo (outro pawn): a entrada antiga é resolvida antes de o novo ganhar a dele.
        if engine
            .data_entries()
            .iter()
            .any(|e| e.id == id && e.guard != guard)
        {
            match engine.restore_field(id) {
                Ok(r) => tracing::info!(
                    id,
                    restaurados = r.restored.len(),
                    descartados = r.foreign.len(),
                    "objeto novo: entrada do anterior resolvida"
                ),
                Err(e) => {
                    tracing::debug!(id, "entrada do objeto anterior não resolvida: {e}");
                    return Some(Note::Waiting);
                }
            }
        }

        let (mut failed, mut invalid) = (false, false);
        for &(addr, _) in fields {
            match write_set_field(engine, id, addr, desired, &guard, kind) {
                Ok(SetOutcome::Rejected(_)) => invalid = true,
                Ok(SetOutcome::NewOriginal(old)) => {
                    let shown = format_value(grid, kind, decode_value(kind, &old));
                    report.events.push(format!(
                        "{name}: o jogo mudou o valor para {shown}; novo original"
                    ));
                    if fight.record(now) {
                        // O jogo reescreve o valor o tempo todo: para e devolve o que é do jogo.
                        if let Err(e) = engine.restore_field(id) {
                            tracing::warn!(id, "entradas do cheat não resolvidas: {e}");
                        }
                        report.events.push(format!("{name}: o jogo não deixa"));
                        fight.blocked = true;
                        return None;
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::debug!(id, addr, "campo não escrito: {e}");
                    failed = true;
                }
            }
        }
        if invalid && self.bad_value.insert(id.to_string()) {
            tracing::warn!(id, "valor do jogo inválido; nada escrito");
            report
                .events
                .push(format!("{name}: valor do jogo inválido"));
        } else if !invalid {
            self.bad_value.remove(id);
        }
        if failed {
            Some(Note::Waiting)
        } else if invalid {
            Some(Note::InvalidGameValue)
        } else {
            None
        }
    }

    /// Executa a ação de somar `amount` ao campo do cheat `id` (spec 006, 6.6, passos 2 a 5; o
    /// motor `stuck` ou ausente, a janela de 1 s e o registro são de quem chama). Sem diário.
    #[allow(clippy::too_many_arguments)]
    pub fn run_action<M: ReadMem + ?Sized>(
        &mut self,
        engine: &mut Engine,
        mem: &M,
        index: &ReflectionIndex,
        objects: &ObjectArray,
        controller: Option<u64>,
        id: &str,
        amount: i32,
    ) -> ActionOutcome {
        let Some(controller) = controller else {
            return ActionOutcome::NeedsController;
        };
        let result_max = match self.catalog.get(id) {
            Some(Ok(ResolvedField {
                rule: ResolvedRule::Add { result_max, .. },
                ..
            })) => *result_max,
            _ => return ActionOutcome::Refused("ação indisponível".to_string()),
        };
        let (obj, addr, guard) =
            match read_game_state(mem, index, &self.catalog, objects, controller, id) {
                Ok(found) => found,
                Err(GameStateAbsent::ControllerInvalid) => return ActionOutcome::NeedsController,
                Err(GameStateAbsent::Missing(hop)) => {
                    tracing::debug!(id, hop, "estado do jogo ausente");
                    return ActionOutcome::Refused("estado do jogo ausente".to_string());
                }
            };
        // Objeto com endereço novo (ou a primeira execução): o `maps` é relido antes de escrever.
        if self.action_objects.get(id) != Some(&obj) {
            match engine.refresh_data_maps() {
                Ok(()) => {
                    self.action_objects.insert(id.to_string(), obj);
                }
                Err(e) => tracing::warn!("falha ao reler o maps: {e}"),
            }
        }
        let Ok(raw) = read_u32(mem, addr) else {
            return ActionOutcome::Refused("valor atual ilegível".to_string());
        };
        let before = raw as i32;
        let after = match plan_add(before, amount, result_max) {
            Ok(v) => v,
            Err(AddRefusal::OutOfRange) => {
                return ActionOutcome::Refused("valor atual fora da faixa".to_string());
            }
            Err(AddRefusal::WouldExceed) => {
                return ActionOutcome::Refused("passaria do máximo".to_string());
            }
        };
        match engine.write_data(addr, &after.to_le_bytes(), &guard) {
            Ok(()) => ActionOutcome::Done { before, after },
            Err(CoreError::BytesMismatch { .. }) => match read_u32(mem, addr) {
                Ok(read) => ActionOutcome::Raced { read: read as i32 },
                Err(_) => ActionOutcome::Refused("valor atual ilegível".to_string()),
            },
            Err(e) => {
                tracing::debug!(id, "escrita da ação recusada: {e}");
                ActionOutcome::Refused("escrita recusada".to_string())
            }
        }
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
            let r = engine.restore_field(id)?;
            tracing::info!(
                id,
                restaurados = r.restored.len(),
                descartados = r.foreign.len(),
                "objeto novo: entrada do anterior resolvida"
            );
        }
        engine.set_bits(id, addr, mask, guard)
    }

    /// Desliga o cheat `id`: uma trava só para; os bits voltam ao original (spec 6.5). Bits
    /// de um objeto que não existe mais são descartados sem escrever e o desligar conta como
    /// feito (spec 6.7), com uma linha no log; o `id` vem sempre em `restored`.
    pub fn turn_off(&mut self, engine: &mut Engine, id: &str) -> gt_core::Result<BatchReport> {
        self.invalid.remove(id);
        self.fights.remove(id);
        self.bad_value.remove(id);
        self.learned.retain(|(learned, _), _| learned != id);
        let done = BatchReport {
            restored: vec![id.to_string()],
            ..BatchReport::default()
        };
        match self.catalog.get(id) {
            Some(Ok(resolved))
                if matches!(
                    resolved.rule,
                    ResolvedRule::ClearBits { .. } | ResolvedRule::Set { .. }
                ) =>
            {
                let r = engine.restore_field(id)?;
                if !r.foreign.is_empty() {
                    tracing::info!(id, "bits de um objeto que não existe mais; nada escrito");
                }
                Ok(done)
            }
            Some(_) => Ok(done),
            None => Err(CoreError::NotActive { id: id.to_string() }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fight_counts_more_than_five_in_ten_seconds() {
        let t0 = Instant::now();
        let mut fight = Fight::new(1);
        // Cinco em 10 s ainda é tolerado; o sexto passa do limite.
        for i in 0..5 {
            assert!(!fight.record(t0 + Duration::from_secs(i)), "{i}");
        }
        assert!(fight.record(t0 + Duration::from_secs(5)));
    }

    #[test]
    fn fight_window_slides() {
        let t0 = Instant::now();
        let mut fight = Fight::new(1);
        // Um a cada 3 s: nunca mais de quatro dentro de 10 s.
        for i in 0..20 {
            assert!(!fight.record(t0 + Duration::from_secs(3 * i)), "{i}");
        }
        // Exatamente 10 s depois, o mais antigo já saiu da janela.
        let mut edge = Fight::new(1);
        for _ in 0..5 {
            edge.record(t0);
        }
        assert!(!edge.record(t0 + FIGHT_WINDOW));
    }

    #[test]
    fn values_use_the_grid_format() {
        let grid = ValueGrid {
            ty: gt_core::value::ValueType::F32,
            min: 0.5,
            max: 4.0,
            step: 0.25,
            default_k: 6,
            unit: Some("x".into()),
            decimals: 2,
        };
        assert_eq!(format_value(Some(&grid), FieldKind::Float, 1.0), "1.00x");
        // Fora da faixa da grade: o valor do jogo aparece como é, sem ser preso a ela.
        assert_eq!(format_value(Some(&grid), FieldKind::Float, 12.5), "12.50x");
        assert_eq!(format_value(None, FieldKind::Int, 1000.0), "1000");
        assert_eq!(decode_value(FieldKind::Int, &15i32.to_le_bytes()), 15.0);
        assert_eq!(decode_value(FieldKind::Float, &0.5f32.to_le_bytes()), 0.5);
    }

    #[test]
    fn missing_note_follows_the_kind() {
        assert_eq!(missing_note(MissingKind::Pet), Note::WaitingPet);
        assert_eq!(missing_note(MissingKind::Component), Note::Waiting);
    }
}
