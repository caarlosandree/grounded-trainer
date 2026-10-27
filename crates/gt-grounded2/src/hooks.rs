//! Execução dos desvios com code cave (spec 008, 6.3 a 6.7; feature `write`).
//!
//! O [`HookRuntime`] cuida do que a TUI faz com os `[[hook]]` achados na memória:
//!
//! - [`HookRuntime::resolve`] confere cada ocorrência (bytes contra o disco, folga das variáveis,
//!   campos da reflexão, coerência do padrão do Brain Power) e reserva o lugar da cave. O que não
//!   serve volta com o motivo (`bytes diferentes`, `padrão incoerente`, `sem espaço para
//!   variáveis`, `sem espaço para a cave`, `campo X não encontrado`), sem o `indisponível (…)` em
//!   volta (a tela é que o põe);
//! - [`HookRuntime::enable`] reclama a folga na primeira vez, escreve classes e valor nas
//!   variáveis e aplica `[Cave, Code]` num lote só; [`HookRuntime::disable`] devolve só o desvio
//!   (a cave fica até sair, spec 6.3);
//! - [`HookRuntime::set_value`] é só uma escrita na variável (6.5);
//! - [`HookRuntime::arm`] arma e desarma o arrastar (6.7.2);
//! - [`HookRuntime::tick`] roda a ~10 Hz: registro do Brain Power, acompanhamento do arrastar e o
//!   inventário do jogador para a coleta (6.7.4).
//!
//! As variáveis das caves moram na folga da `.data` ([`crate::caves::var`]); quem sai do jogo (o
//! motor) as zera. Nada aqui chama `undo_all`: o motor faz isso no `Drop`.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gt_core::Error as CoreError;
use gt_core::cave::{CaveAllocator, hook_bytes};
use gt_core::engine::Engine;
use gt_core::maps::MapRegion;
use gt_core::mem::ReadMem;
use gt_core::modules::Module;
use gt_core::patch::{CAVE_FILL, PatchKind, PatchSpec, validate};
use gt_core::pe::DiskImage;
use gt_ue::read::is_user_addr;
use gt_ue::{ReflectionIndex, UeLayout, is_object_family};

use crate::catalog::HookHit;
use crate::caves::{
    self, CaveCtx, FIELD_MAX_POWER_CAPACITY, FIELD_RESEARCH_POINTS, FIELD_STACK_SIZE, var,
};
use crate::error::{Error, Result};
use crate::policy::{cave_allocator, var_region};
use crate::signatures::{CaveBody, HookDef, PlayerCfg, brain_captures_from_hit};

/// Quanto o trainer espera, depois de escrever 0 no arme do arrastar, antes de reler o contador
/// de aplicadas (spec 6.7.2).
pub const DISARM_WAIT: Duration = Duration::from_millis(50);
/// Depois deste tempo armado e sem uso, o arrastar é desarmado (spec 6.7.2).
pub const DRAG_EXPIRY: Duration = Duration::from_secs(60);
/// Tentativas de uma leitura estável do registro do Brain Power por ciclo.
const LOG_TRIES: usize = 4;

/// O que o trainer sabe, no início da conexão, para planejar os desvios.
pub struct HookEnv {
    /// Layout da reflexão (`ClassPrivate` e `SuperStruct` para as caves).
    pub layout: UeLayout,
    /// O módulo do jogo (a base converte endereço em RVA do disco).
    pub module: Module,
    /// O arquivo do módulo no disco, para conferir os bytes da instrução trocada.
    pub disk: Arc<DiskImage>,
    /// Folga da `.data` para as variáveis; `None` se não há (`sem espaço para variáveis`).
    pub var_region: Option<Range<u64>>,
    /// Alocador das sequências de `CC` da `.text` do disco.
    pub allocator: CaveAllocator,
    /// O `[ue.player]` das assinaturas: o pawn, a `party_class` e o campo de `ResearchPoints` do
    /// Brain Power, e o campo do inventário da coleta. As classes dos outros desvios vêm do
    /// `object_class` de cada `[[hook]]`.
    pub player: PlayerCfg,
}

impl HookEnv {
    /// Monta o ambiente de um módulo confiável: a folga pelo `maps` e as sequências de `CC` pelo
    /// disco ([`var_region`], [`cave_allocator`]). Sem `.text` legível, o alocador fica vazio
    /// (`sem espaço para a cave`).
    pub fn new(
        module: Module,
        disk: Arc<DiskImage>,
        maps: &[MapRegion],
        layout: UeLayout,
        player: PlayerCfg,
    ) -> HookEnv {
        let var_region = var_region(&module, &disk.headers, maps);
        let allocator =
            cave_allocator(&module, &disk).unwrap_or_else(|| CaveAllocator::new(Vec::new()));
        HookEnv {
            layout,
            module,
            disk,
            var_region,
            allocator,
            player,
        }
    }
}

/// Disponibilidade de um desvio depois da conferência.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookAvail {
    /// Pode ser ligado.
    Available,
    /// Não pode ser ligado; o motivo vem sem o `indisponível (…)` em volta.
    Unavailable(String),
}

/// O pawn (personagem) do ciclo, de onde se acha o inventário (spec 6.7.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PawnView {
    /// Endereço do `SurvivalPlayerCharacter`.
    pub addr: u64,
}

impl From<u64> for PawnView {
    fn from(addr: u64) -> Self {
        PawnView { addr }
    }
}

/// O que `arm` fez.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmOutcome {
    /// Estava desarmado: a quantidade foi para a variável.
    Armed,
    /// Estava armado: foi desarmado sem o jogo ter aplicado.
    Disarmed,
    /// Estava armado, mas o jogo aplicou antes do desarme.
    Applied,
}

/// Acontecimento do ciclo, para o registro da TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookEvent {
    /// Um ganho de Brain Power (o último visto) e quantos outros não foram mostrados.
    BrainGain {
        /// Ganho original.
        gain: i32,
        /// Ganho aplicado pela cave.
        applied: i32,
        /// Próximo nível em valor absoluto de `ResearchPoints`; 0 se não há.
        l1: i32,
        /// Ganhos que aconteceram entre duas leituras e não foram mostrados.
        skipped: u32,
    },
    /// O jogo aplicou a quantidade armada (N).
    DragApplied(i32),
    /// Expirou (60 s armado sem uso).
    DragExpired,
    /// A variável foi zerada por fora, sem aplicação: o arme acabou.
    DragDisarmed,
}

/// Ciclo de vida de um desvio no runtime.
#[derive(Debug, Clone)]
struct Slot {
    def: HookDef,
    avail: HookAvail,
    plan: Option<Plan>,
    /// A cave já foi escrita (fica até sair).
    cave_written: bool,
    /// O desvio está instalado.
    hooked: bool,
}

/// O que escrever para ligar um desvio.
#[derive(Debug, Clone)]
struct Plan {
    /// Instrução trocada e os bytes dela.
    hook_at: u64,
    insn: Vec<u8>,
    /// A cave, onde fica e os bytes.
    cave_at: u64,
    cave: Vec<u8>,
    /// Ponteiros de classe para as variáveis: `(deslocamento, valor)`.
    class_vars: Vec<(u64, u64)>,
    /// A cave já está escrita no jogo (uma cave nossa sem desvio, reconhecida pelo motor): só o
    /// desvio entra ao ligar.
    cave_present: bool,
}

/// Arrastar armado.
#[derive(Debug, Clone, Copy)]
struct Armed {
    n: i32,
    /// Contador de aplicadas quando armou.
    counter0: u64,
    since: Instant,
    /// Ciclos seguidos em que a variável estava 0 sem o contador ter subido.
    zero_ticks: u32,
}

/// Registro do Brain Power lido de forma estável.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BrainLog {
    gain: i32,
    applied: i32,
    l1: i32,
    seq: u32,
}

/// Lê o registro do Brain Power (24 bytes) com o contador de sequência conferido antes e depois:
/// só vale se for par (a cave o incrementa antes e depois de gravar) e igual nas três leituras.
/// `read(deslocamento, tamanho)` devolve `None` se a leitura falhou (desiste).
fn read_brain_log(mut read: impl FnMut(u64, usize) -> Option<Vec<u8>>) -> Option<BrainLog> {
    let word =
        |bytes: &[u8], at: usize| -> Option<[u8; 4]> { bytes.get(at..at + 4)?.try_into().ok() };
    for _ in 0..LOG_TRIES {
        let before = u32::from_le_bytes(word(&read(var::BRAIN_SEQ, 4)?, 0)?);
        if before % 2 == 1 {
            continue;
        }
        let body = read(var::BRAIN_N, 24)?;
        let inside = u32::from_le_bytes(word(&body, 20)?);
        let after = u32::from_le_bytes(word(&read(var::BRAIN_SEQ, 4)?, 0)?);
        if before != inside || before != after {
            continue;
        }
        return Some(BrainLog {
            gain: i32::from_le_bytes(word(&body, 4)?),
            applied: i32::from_le_bytes(word(&body, 8)?),
            l1: i32::from_le_bytes(word(&body, 12)?),
            seq: before,
        });
    }
    None
}

/// Estado dos desvios de uma conexão.
pub struct HookRuntime {
    env: HookEnv,
    /// Por `id`.
    slots: HashMap<String, Slot>,
    /// A folga já foi reclamada no motor.
    claimed: bool,
    /// Deslocamento do `InventoryComponent` no personagem, se a reflexão o achou.
    inventory_off: Option<u32>,
    /// Último contador de sequência do Brain Power visto (ou o da hora de ligar).
    brain_seq: Option<u32>,
    drag: Option<Armed>,
    /// Caves nossas escritas e sem desvio ativo no motor, por `id`: `(lugar, bytes)`. O `resolve`
    /// as reaproveita se a cave montada agora for igual.
    orphan_caves: HashMap<String, (u64, Vec<u8>)>,
    /// Último valor escrito na variável do inventário (para escrever só na mudança).
    inventory_written: Option<u64>,
}

fn core(e: CoreError) -> Error {
    Error::Core(e)
}

/// Bytes de uma variável `u64`/`i32`: `0` para o motor sem a variável.
fn le_u64(bytes: &[u8]) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(..8)?.try_into().ok()?))
}

fn le_i32(bytes: &[u8]) -> Option<i32> {
    Some(i32::from_le_bytes(bytes.get(..4)?.try_into().ok()?))
}

/// Até onde (deslocamento final, exclusivo) as variáveis do corpo vão: o que não couber na folga
/// deixa o desvio indisponível (spec 6.4, corte por cheat com as dependências: a classe `Item`
/// só sai junto com o arrastar e a coleta).
fn var_need(body: CaveBody) -> u64 {
    match body {
        CaveBody::Brain => var::BRAIN_SEQ + 4,
        CaveBody::Analyzer => var::ANALYZER_CLASS + 8,
        CaveBody::Drag | CaveBody::Collect => var::ITEM_CLASS + 8,
    }
}

fn split_class(path: &str) -> (&str, &str) {
    path.rsplit_once('.').unwrap_or(("", path))
}

/// A classe `path` (`Pacote.Nome`) pela reflexão, com o motivo para a tela.
fn class_of(refl: &ReflectionIndex, path: &str) -> std::result::Result<u64, String> {
    let (pkg, name) = split_class(path);
    refl.find_class(pkg, name).map_err(|e| {
        tracing::debug!("classe {path}: {e}");
        match e {
            gt_ue::UeError::Ambiguous(_) => format!("classe {name} ambígua"),
            _ => format!("classe {name} não encontrada"),
        }
    })
}

/// O deslocamento do campo `name` da classe `class`, com o motivo para a tela.
fn field_off<M: ReadMem + ?Sized>(
    refl: &ReflectionIndex,
    mem: &M,
    class: u64,
    name: &str,
) -> std::result::Result<u32, String> {
    refl.find_field(mem, class, name)
        .map(|f| f.offset)
        .map_err(|e| {
            tracing::debug!("campo {name}: {e}");
            format!("campo {name} não encontrado")
        })
}

fn cave_id(id: &str) -> String {
    format!("{id}-cave")
}

fn hook_id(id: &str) -> String {
    format!("{id}-hook")
}

impl HookRuntime {
    /// Runtime sem nenhum desvio resolvido.
    pub fn new(env: HookEnv) -> Self {
        Self {
            env,
            slots: HashMap::new(),
            claimed: false,
            inventory_off: None,
            brain_seq: None,
            drag: None,
            orphan_caves: HashMap::new(),
            inventory_written: None,
        }
    }

    /// Base das variáveis (começo da folga), se há folga.
    fn base(&self) -> Option<u64> {
        self.env.var_region.as_ref().map(|r| r.start)
    }

    /// Tamanho da folga em bytes.
    fn region_len(&self) -> u64 {
        self.env
            .var_region
            .as_ref()
            .map_or(0, |r| r.end.saturating_sub(r.start))
    }

    /// Confere as ocorrências `hits` e planeja a cave de cada uma. Devolve, na ordem, o estado de
    /// cada desvio (e, no fim, o dos que já estão instalados por nós e por isso não aparecem mais
    /// como ocorrência do padrão: o nosso desvio tirou o padrão da memória).
    ///
    /// Chamar de novo (índice melhor da reflexão, nova varredura) mantém o que já foi escrito:
    /// uma cave escrita não muda de lugar.
    pub fn resolve<M: ReadMem + ?Sized>(
        &mut self,
        hits: &[HookHit],
        mem: &M,
        refl: &ReflectionIndex,
    ) -> Vec<(String, HookAvail)> {
        self.inventory_off = self.env.player.inventory_field.as_deref().and_then(|name| {
            let class = class_of(refl, &self.env.player.pawn_class).ok()?;
            let field = refl.find_field(mem, class, name).ok()?;
            is_object_family(&field.kind).then_some(field.offset)
        });
        let mut out = Vec::new();
        for hit in hits {
            let id = hit.def.id.clone();
            // Cave já escrita: o lugar e a instrução ficam como estão; só o motivo muda.
            if self
                .slots
                .get(&id)
                .is_some_and(|s| s.cave_written && s.plan.is_some())
            {
                if let Some(slot) = self.slots.get_mut(&id) {
                    slot.avail = HookAvail::Available;
                }
                out.push((id, HookAvail::Available));
                continue;
            }
            let (plan, avail) = match self.plan(hit, mem, refl) {
                Ok(plan) => (Some(plan), HookAvail::Available),
                Err(reason) => (None, HookAvail::Unavailable(reason)),
            };
            let (cave_written, hooked) = self
                .slots
                .get(&id)
                .map_or((false, false), |s| (s.cave_written, s.hooked));
            let reused = plan.as_ref().is_some_and(|p| p.cave_present);
            if reused {
                self.orphan_caves.remove(&id);
            }
            let cave_written = cave_written || reused;
            self.slots.insert(
                id.clone(),
                Slot {
                    def: hit.def.clone(),
                    avail: avail.clone(),
                    plan,
                    cave_written,
                    hooked,
                },
            );
            out.push((id, avail));
        }
        // Instalados por nós: o padrão não está mais na memória, mas o desvio está disponível.
        let mut ours: Vec<&String> = self
            .slots
            .iter()
            .filter(|(id, s)| {
                (s.hooked || s.cave_written) && !hits.iter().any(|h| h.def.id == **id)
            })
            .map(|(id, _)| id)
            .collect();
        ours.sort();
        for id in ours {
            out.push((id.clone(), HookAvail::Available));
        }
        out
    }

    /// Sincroniza com as caves que o motor já tem no jogo (o diário do mesmo processo): chame
    /// antes do [`HookRuntime::resolve`] de um runtime novo sobre um motor que já escreveu.
    ///
    /// - os trechos de **todas** as caves do motor (ativas e pendentes) saem do alocador, para a
    ///   cave de outro desvio não cair em cima delas;
    /// - cave e desvio ativos: o desvio é reconhecido como ligado (o padrão dele sumiu da memória e
    ///   a varredura diria `não encontrado`); a instrução original vem do `expected` do desvio;
    /// - só a cave ativa (o desvio foi desligado): ela é lembrada e o `resolve` a reaproveita,
    ///   se a cave de agora for igual, de modo que religar escreva só o desvio.
    ///
    /// Devolve os `id` reconhecidos como ligados.
    pub fn recognize_installed(&mut self, engine: &Engine, defs: &[HookDef]) -> Vec<String> {
        for range in engine.cave_ranges() {
            self.env.allocator.reserve(range);
        }
        let active = engine.active();
        let find = |id: &str, kind: PatchKind| {
            active
                .iter()
                .find(|s| s.id == id && s.kind == kind)
                .map(|s| (*s).clone())
        };
        let mut found = Vec::new();
        for def in defs {
            if self.slots.get(&def.id).is_some_and(|s| s.hooked) {
                continue;
            }
            let Some(cave) = find(&cave_id(&def.id), PatchKind::Cave) else {
                continue;
            };
            let Some(hook) = find(&hook_id(&def.id), PatchKind::Code) else {
                self.orphan_caves
                    .insert(def.id.clone(), (cave.addr, cave.replacement));
                continue;
            };
            self.orphan_caves.remove(&def.id);
            self.slots.insert(
                def.id.clone(),
                Slot {
                    def: def.clone(),
                    avail: HookAvail::Available,
                    plan: Some(Plan {
                        hook_at: hook.addr,
                        insn: hook.expected,
                        cave_at: cave.addr,
                        cave: cave.replacement,
                        class_vars: Vec::new(),
                        cave_present: true,
                    }),
                    cave_written: true,
                    hooked: true,
                },
            );
            found.push(def.id.clone());
        }
        if self
            .env
            .var_region
            .as_ref()
            .is_some_and(|r| engine.vars().iter().any(|v| v.addr == r.start))
        {
            self.claimed = true;
        }
        found
    }

    /// Confere uma ocorrência e monta o plano; o `Err` é o motivo da tela.
    fn plan<M: ReadMem + ?Sized>(
        &mut self,
        hit: &HookHit,
        mem: &M,
        refl: &ReflectionIndex,
    ) -> std::result::Result<Plan, String> {
        let def = &hit.def;
        let insn = hit.insn().map_err(|e| e.to_string())?;
        // Bytes da instrução contra o disco.
        let rva = hit
            .addr
            .checked_sub(self.env.module.base)
            .and_then(|d| u32::try_from(d).ok())
            .ok_or("fora do módulo")?;
        if self.env.disk.bytes(rva, insn.len()) != Some(&insn[..]) {
            return Err("bytes diferentes".to_string());
        }
        let base = self.base().ok_or("sem espaço para variáveis")?;
        if self.region_len() < var_need(def.body) {
            return Err("sem espaço para variáveis".to_string());
        }

        // Campos e classes da reflexão: o Brain Power usa a `party_class` e o campo do
        // `[ue.player]`; os outros, o `object_class` do próprio `[[hook]]`.
        let mut fields: HashMap<&'static str, u32> = HashMap::new();
        let mut class_vars = Vec::new();
        let owner = |def: &HookDef| {
            def.object_class
                .clone()
                .ok_or_else(|| "classe do objeto não configurada".to_string())
        };
        match def.body {
            CaveBody::Brain => {
                let research = self
                    .env
                    .player
                    .research
                    .as_ref()
                    .ok_or(format!("campo {FIELD_RESEARCH_POINTS} não encontrado"))?;
                let class = class_of(refl, &research.party_class)?;
                fields.insert(
                    FIELD_RESEARCH_POINTS,
                    field_off(refl, mem, class, &research.points_field)?,
                );
            }
            CaveBody::Analyzer => {
                let class = class_of(refl, &owner(def)?)?;
                fields.insert(
                    FIELD_MAX_POWER_CAPACITY,
                    field_off(refl, mem, class, FIELD_MAX_POWER_CAPACITY)?,
                );
                class_vars.push((var::ANALYZER_CLASS, class));
            }
            CaveBody::Drag | CaveBody::Collect => {
                let class = class_of(refl, &owner(def)?)?;
                fields.insert(
                    FIELD_STACK_SIZE,
                    field_off(refl, mem, class, FIELD_STACK_SIZE)?,
                );
                class_vars.push((var::ITEM_CLASS, class));
                if def.body == CaveBody::Collect && self.inventory_off.is_none() {
                    let name = self.env.player.inventory_field.as_deref();
                    return Err(format!(
                        "campo {} não encontrado",
                        name.unwrap_or("do inventário")
                    ));
                }
            }
        }
        // Brain Power: os três `disp` do padrão têm de ser iguais entre si e ao da reflexão.
        if def.body == CaveBody::Brain {
            let caps = brain_captures_from_hit(&hit.captures).map_err(|_| "padrão incoerente")?;
            if fields.get(FIELD_RESEARCH_POINTS) != Some(&caps.research) {
                return Err("padrão incoerente".to_string());
            }
        }

        // A cave: monta para medir, reserva o lugar e monta de novo nele.
        let ctx = |cave_at: u64| CaveCtx {
            hook_at: hit.addr,
            cave_at,
            insn: insn.clone(),
            vars: base,
            captures: hit.captures.clone(),
            fields: fields.clone(),
            layout: self.env.layout,
        };
        let probe = caves::build(def.body, &ctx(hit.addr)).map_err(|e| match e {
            Error::IncoherentPattern { .. } => "padrão incoerente".to_string(),
            Error::CaveBuild { reason } => reason,
            other => other.to_string(),
        })?;
        // Uma cave nossa que já está no jogo (sem desvio) e é igual à de agora é reaproveitada.
        let build_at = |at: u64| caves::build(def.body, &ctx(at)).map_err(|e| e.to_string());
        let present = match self.orphan_caves.get(&def.id) {
            Some((at, bytes)) if bytes.len() == probe.len() => {
                (build_at(*at)? == *bytes).then_some(*at)
            }
            _ => None,
        };
        // Senão, um slot ainda sem cave escrita pode ter reservado um lugar de igual tamanho.
        let reuse = self
            .slots
            .get(&def.id)
            .and_then(|s| s.plan.as_ref())
            .filter(|p| p.cave.len() == probe.len())
            .map(|p| p.cave_at);
        let cave_present = present.is_some();
        let cave_at = match present.or(reuse) {
            Some(at) => at,
            None => self
                .env
                .allocator
                .alloc(probe.len())
                .ok_or("sem espaço para a cave")?,
        };
        let cave = build_at(cave_at)?;
        if cave.len() != probe.len() {
            return Err("cave de tamanho variável".to_string());
        }
        Ok(Plan {
            hook_at: hit.addr,
            insn,
            cave_at,
            cave,
            class_vars,
            cave_present,
        })
    }

    /// Valor `k` da grade do desvio como `i32` (`None` usa o padrão da grade).
    fn value_of(def: &HookDef, k: Option<u64>) -> Result<Option<i32>> {
        let Some(grid) = &def.value else {
            return Ok(None);
        };
        let v = grid.value(k.unwrap_or(grid.default_k)).round();
        i32::try_from(v as i64)
            .map(Some)
            .map_err(|_| Error::Unavailable {
                id: def.id.clone(),
                state: format!("valor {v} fora do i32"),
            })
    }

    fn slot_ready(&self, id: &str) -> Result<&Slot> {
        let slot = self
            .slots
            .get(id)
            .ok_or_else(|| Error::UnknownPatch { id: id.to_string() })?;
        if let HookAvail::Unavailable(reason) = &slot.avail {
            return Err(Error::Unavailable {
                id: id.to_string(),
                state: reason.clone(),
            });
        }
        Ok(slot)
    }

    fn claim(&mut self, engine: &mut Engine) -> Result<u64> {
        let region = self
            .env
            .var_region
            .clone()
            .ok_or_else(|| Error::Unavailable {
                id: String::new(),
                state: "sem espaço para variáveis".to_string(),
            })?;
        if !self.claimed {
            engine.claim_vars(region.clone()).map_err(core)?;
            self.claimed = true;
        }
        Ok(region.start)
    }

    /// Liga o desvio `id` com o valor da grade `value_k` (`None`: o padrão da grade). Reclama a
    /// folga na primeira vez, escreve as classes e o valor e aplica `[Cave, Code]` num lote.
    /// Ligar um desvio já ligado só troca o valor.
    pub fn enable(&mut self, engine: &mut Engine, id: &str, value_k: Option<u64>) -> Result<()> {
        let slot = self.slot_ready(id)?;
        let def = slot.def.clone();
        let Some(plan) = slot.plan.clone() else {
            return Err(Error::Unavailable {
                id: id.to_string(),
                state: "sem plano".to_string(),
            });
        };
        let (cave_written, hooked) = (slot.cave_written, slot.hooked);
        let base = self.claim(engine)?;
        let value = Self::value_of(&def, value_k)?;
        if hooked {
            return self.write_value(engine, base, &def, value);
        }
        for (off, ptr) in &plan.class_vars {
            engine
                .write_var(base + off, &ptr.to_le_bytes())
                .map_err(core)?;
        }
        self.write_value(engine, base, &def, value)?;
        if def.body == CaveBody::Analyzer {
            engine
                .write_var(base + var::ANALYZER_ON, &1u64.to_le_bytes())
                .map_err(core)?;
        }
        let mut batch = Vec::new();
        if !cave_written {
            batch.push(
                validate(PatchSpec {
                    id: cave_id(id),
                    addr: plan.cave_at,
                    expected: vec![CAVE_FILL; plan.cave.len()],
                    replacement: plan.cave.clone(),
                    kind: PatchKind::Cave,
                })
                .map_err(core)?,
            );
        }
        let jump = hook_bytes(plan.hook_at, plan.insn.len(), plan.cave_at).map_err(core)?;
        batch.push(
            validate(PatchSpec {
                id: hook_id(id),
                addr: plan.hook_at,
                expected: plan.insn.clone(),
                replacement: jump,
                kind: PatchKind::Code,
            })
            .map_err(core)?,
        );
        engine.apply(batch).map_err(core)?;
        if def.body == CaveBody::Brain {
            // O registro só mostra o que acontece daqui em diante.
            self.brain_seq =
                read_brain_log(|off, len| engine.read_var(base + off, len).ok()).map(|log| log.seq);
        }
        if let Some(slot) = self.slots.get_mut(id) {
            slot.cave_written = true;
            slot.hooked = true;
        }
        Ok(())
    }

    /// Escreve o valor `value` na variável do corpo (Brain Power e coleta); os outros não têm.
    fn write_value(
        &self,
        engine: &mut Engine,
        base: u64,
        def: &HookDef,
        value: Option<i32>,
    ) -> Result<()> {
        let (Some(v), Some(off)) = (
            value,
            match def.body {
                CaveBody::Brain => Some(var::BRAIN_N),
                CaveBody::Collect => Some(var::COLLECT_N),
                _ => None,
            },
        ) else {
            return Ok(());
        };
        engine.write_var(base + off, &v.to_le_bytes()).map_err(core)
    }

    /// Desliga o desvio `id`: só o `Code` volta; a cave e as variáveis ficam até sair (spec 6.3).
    /// Desligar o arrastar armado desarma antes (sem o registro).
    pub fn disable(&mut self, engine: &mut Engine, id: &str) -> Result<()> {
        let Some(slot) = self.slots.get(id) else {
            return Err(Error::UnknownPatch { id: id.to_string() });
        };
        if !slot.hooked {
            return Ok(());
        }
        if slot.def.body == CaveBody::Drag && self.drag.is_some() {
            self.disarm(engine)?;
        }
        engine.undo(&[&hook_id(id)]).map_err(core)?;
        if let Some(slot) = self.slots.get_mut(id) {
            slot.hooked = false;
        }
        Ok(())
    }

    /// Troca o valor de um desvio ligado (ou não): só uma escrita na variável, sem pausa (6.5).
    pub fn set_value(&mut self, engine: &mut Engine, id: &str, k: u64) -> Result<()> {
        let slot = self.slot_ready(id)?;
        let def = slot.def.clone();
        let base = self.claim(engine)?;
        let value = Self::value_of(&def, Some(k))?;
        if !matches!(def.body, CaveBody::Brain | CaveBody::Collect) {
            return Err(Error::Unavailable {
                id: id.to_string(),
                state: "não tem valor ajustável".to_string(),
            });
        }
        self.write_value(engine, base, &def, value)
    }

    /// O desvio `id` está instalado?
    pub fn is_hooked(&self, id: &str) -> bool {
        self.slots.get(id).is_some_and(|s| s.hooked)
    }

    /// Quantidade e hora do arme do arrastar, se está armado.
    pub fn armed(&self) -> Option<(i32, Instant)> {
        self.drag.map(|a| (a.n, a.since))
    }

    /// O deslocamento do inventário no personagem, se a reflexão o achou.
    pub fn inventory_offset(&self) -> Option<u32> {
        self.inventory_off
    }

    /// `id` do desvio do arrastar entre os resolvidos.
    fn drag_id(&self) -> Option<String> {
        self.slots
            .values()
            .find(|s| s.def.body == CaveBody::Drag)
            .map(|s| s.def.id.clone())
    }

    /// Arma o arrastar com o ponto `k` da grade (a quantidade); com ele já armado, desarma.
    /// Instala o desvio se ainda não está (spec 6.7.2).
    pub fn arm(&mut self, engine: &mut Engine, k: u64) -> Result<ArmOutcome> {
        self.arm_at(engine, k, Instant::now())
    }

    /// [`HookRuntime::arm`] com o relógio injetado.
    pub fn arm_at(&mut self, engine: &mut Engine, k: u64, now: Instant) -> Result<ArmOutcome> {
        if self.drag.is_some() {
            return self.disarm(engine);
        }
        let id = self.drag_id().ok_or_else(|| Error::UnknownPatch {
            id: "quantidade-arrastar".to_string(),
        })?;
        let def = self.slot_ready(&id)?.def.clone();
        if !self.is_hooked(&id) {
            self.enable(engine, &id, None)?;
        }
        let base = self.claim(engine)?;
        let n = Self::value_of(&def, Some(k))?.unwrap_or(0);
        let counter0 = self.drag_counter(engine, base).unwrap_or(0);
        // A cave pode levar o valor (e zerar a variável) antes da releitura do motor: a escrita
        // valeu, e quem decide se foi aplicado é o contador.
        match engine.write_var(base + var::DRAG_N, &n.to_le_bytes()) {
            Ok(()) | Err(CoreError::BytesMismatch { .. }) => {}
            Err(e) => return Err(core(e)),
        }
        self.drag = Some(Armed {
            n,
            counter0,
            since: now,
            zero_ticks: 0,
        });
        Ok(ArmOutcome::Armed)
    }

    fn drag_counter(&self, engine: &Engine, base: u64) -> Option<u64> {
        le_u64(&engine.read_var(base + var::DRAG_APPLIED, 8).ok()?)
    }

    /// Desarma: escreve 0, espera [`DISARM_WAIT`] e relê o contador; se ele subiu, o jogo aplicou
    /// antes (spec 6.7.2). Só o `xchg` da cave aplica: a escrita do 0 nunca causa duas aplicações.
    fn disarm(&mut self, engine: &mut Engine) -> Result<ArmOutcome> {
        let Some(armed) = self.drag else {
            return Ok(ArmOutcome::Disarmed);
        };
        let base = self.claim(engine)?;
        match engine.write_var(base + var::DRAG_N, &0i32.to_le_bytes()) {
            Ok(()) | Err(CoreError::BytesMismatch { .. }) => {}
            Err(e) => return Err(core(e)),
        }
        std::thread::sleep(DISARM_WAIT);
        self.drag = None;
        let applied = self
            .drag_counter(engine, base)
            .is_some_and(|c| c > armed.counter0);
        Ok(if applied {
            ArmOutcome::Applied
        } else {
            ArmOutcome::Disarmed
        })
    }

    /// Um ciclo (~10 Hz): o registro do Brain Power, o arrastar e o inventário do jogador.
    ///
    /// `pawn` é o personagem achado neste ciclo (`None` se não há: o inventário vira 0 e a coleta
    /// não multiplica nada até o personagem voltar).
    pub fn tick<M: ReadMem + ?Sized>(
        &mut self,
        engine: &mut Engine,
        mem: &M,
        pawn: Option<PawnView>,
        now: Instant,
    ) -> Vec<HookEvent> {
        let mut events = Vec::new();
        let Some(base) = self.base().filter(|_| self.claimed) else {
            return events;
        };
        self.tick_brain(engine, base, &mut events);
        self.tick_drag(engine, base, now, &mut events);
        self.tick_inventory(engine, base, mem, pawn);
        events
    }

    fn tick_brain(&mut self, engine: &Engine, base: u64, events: &mut Vec<HookEvent>) {
        let hooked = self
            .slots
            .values()
            .any(|s| s.def.body == CaveBody::Brain && s.hooked);
        if !hooked {
            return;
        }
        let Some(log) = read_brain_log(|off, len| engine.read_var(base + off, len).ok()) else {
            return;
        };
        match self.brain_seq {
            Some(last) if log.seq > last => {
                // A cave soma 2 por ganho: o que passa de um par entre duas leituras não foi mostrado.
                let skipped = ((log.seq - last) / 2).saturating_sub(1);
                events.push(HookEvent::BrainGain {
                    gain: log.gain,
                    applied: log.applied,
                    l1: log.l1,
                    skipped,
                });
                self.brain_seq = Some(log.seq);
            }
            // Primeira leitura, ou o contador voltou (variáveis zeradas): só a referência.
            _ => self.brain_seq = Some(log.seq),
        }
    }

    fn tick_drag(
        &mut self,
        engine: &mut Engine,
        base: u64,
        now: Instant,
        events: &mut Vec<HookEvent>,
    ) {
        let Some(armed) = self.drag else {
            return;
        };
        if self
            .drag_counter(engine, base)
            .is_some_and(|c| c > armed.counter0)
        {
            events.push(HookEvent::DragApplied(armed.n));
            self.drag = None;
            return;
        }
        if now.saturating_duration_since(armed.since) >= DRAG_EXPIRY {
            match self.disarm(engine) {
                Ok(ArmOutcome::Applied) => events.push(HookEvent::DragApplied(armed.n)),
                Ok(_) => events.push(HookEvent::DragExpired),
                Err(e) => tracing::warn!("falha ao desarmar o arrastar: {e}"),
            }
            return;
        }
        // Variável zerada e contador parado em dois ciclos seguidos: zerou por fora. (Em um ciclo
        // só pode ser a cave entre o `xchg` e o incremento do contador.)
        let zero = engine
            .read_var(base + var::DRAG_N, 4)
            .ok()
            .and_then(|b| le_i32(&b))
            == Some(0);
        if let Some(a) = self.drag.as_mut() {
            a.zero_ticks = if zero { a.zero_ticks + 1 } else { 0 };
            if a.zero_ticks >= 2 {
                self.drag = None;
                events.push(HookEvent::DragDisarmed);
            }
        }
    }

    /// Põe na variável da coleta o inventário do jogador (o campo `InventoryComponent` do pawn);
    /// 0 sem pawn ou com um ponteiro que não é de usuário (spec 6.7.4). Só escreve na mudança.
    fn tick_inventory<M: ReadMem + ?Sized>(
        &mut self,
        engine: &mut Engine,
        base: u64,
        mem: &M,
        pawn: Option<PawnView>,
    ) {
        if self.region_len() < var::COLLECT_INVENTORY + 8 {
            return;
        }
        let Some(off) = self.inventory_off else {
            return;
        };
        let inventory = pawn
            .and_then(|p| {
                let mut raw = [0u8; 8];
                mem.read_exact(p.addr.checked_add(u64::from(off))?, &mut raw)
                    .ok()?;
                Some(u64::from_le_bytes(raw))
            })
            .filter(|ptr| is_user_addr(*ptr))
            .unwrap_or(0);
        if self.inventory_written == Some(inventory) {
            return;
        }
        match engine.write_var(base + var::COLLECT_INVENTORY, &inventory.to_le_bytes()) {
            Ok(()) => self.inventory_written = Some(inventory),
            Err(e) => tracing::warn!("inventário do jogador não gravado: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Registro de 24 bytes com a sequência `seq` nos dois lugares.
    fn log_bytes(gain: i32, applied: i32, l1: i32, seq: u32) -> Vec<u8> {
        let mut b = vec![0u8; 24];
        b[4..8].copy_from_slice(&gain.to_le_bytes());
        b[8..12].copy_from_slice(&applied.to_le_bytes());
        b[12..16].copy_from_slice(&l1.to_le_bytes());
        b[20..24].copy_from_slice(&seq.to_le_bytes());
        b
    }

    /// Lê de uma lista de respostas, na ordem; cada leitura consome uma.
    fn scripted(mut answers: Vec<Vec<u8>>) -> impl FnMut(u64, usize) -> Option<Vec<u8>> {
        answers.reverse();
        move |_, _| answers.pop()
    }

    #[test]
    fn brain_log_reads_when_even_and_equal() {
        let read = scripted(vec![
            4u32.to_le_bytes().to_vec(),
            log_bytes(8, 40, 1500, 4),
            4u32.to_le_bytes().to_vec(),
        ]);
        let log = read_brain_log(read).unwrap();
        assert_eq!(
            log,
            BrainLog {
                gain: 8,
                applied: 40,
                l1: 1500,
                seq: 4
            }
        );
    }

    #[test]
    fn brain_log_torn_read_retries() {
        // 1: sequência ímpar (gravação em andamento). 2: mudou entre antes e depois. 3: o corpo
        // veio com outra sequência. 4: tudo par e igual.
        let read = scripted(vec![
            5u32.to_le_bytes().to_vec(),
            //
            4u32.to_le_bytes().to_vec(),
            log_bytes(1, 1, 1, 4),
            6u32.to_le_bytes().to_vec(),
            //
            6u32.to_le_bytes().to_vec(),
            log_bytes(2, 2, 2, 8),
            6u32.to_le_bytes().to_vec(),
            //
            8u32.to_le_bytes().to_vec(),
            log_bytes(3, 30, 300, 8),
            8u32.to_le_bytes().to_vec(),
        ]);
        let log = read_brain_log(read).unwrap();
        assert_eq!((log.gain, log.applied, log.l1, log.seq), (3, 30, 300, 8));
    }

    #[test]
    fn brain_log_gives_up_after_the_tries_or_a_failed_read() {
        // Sempre ímpar: desiste depois das tentativas.
        let read = scripted(vec![3u32.to_le_bytes().to_vec(); LOG_TRIES]);
        assert_eq!(read_brain_log(read), None);
        // Leitura que falha: desiste na hora.
        assert_eq!(read_brain_log(|_, _| None), None);
    }

    #[test]
    fn var_need_cuts_by_cheat_with_dependencies() {
        // O corpo do Brain Power cabe em 24 bytes, o analisador em 56; arrastar e coleta
        // levam a classe `Item` (até 144).
        assert_eq!(var_need(CaveBody::Brain), 24);
        assert_eq!(var_need(CaveBody::Analyzer), 56);
        assert_eq!(var_need(CaveBody::Drag), 144);
        assert_eq!(var_need(CaveBody::Collect), 144);
        assert!(var_need(CaveBody::Collect) <= var::TOTAL);
    }
}
