//! Caminho até o personagem e planejamento das escritas de valor (spec 005, 6.3 a 6.5).
//!
//! Tudo aqui só lê a memória do jogo. Classes e campos vêm da reflexão, por nome
//! ([`FieldCatalog::resolve`]); a cada ciclo, [`read_player`] revalida o caminho do controller até
//! cada componente e [`plan_tick`] decide, sem ler nada, o que escrever. Quem escreve é o motor.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use gt_core::journal::GuardBytes;
use gt_core::mem::ReadMem;
use gt_core::value::ValueGrid;
use gt_ue::read::{read_u8, read_u32, read_u64, read_vec};
use gt_ue::{Field, ObjectArray, ReflectionIndex, ScanBudget, UeError, UeLayout, is_object_family};
use serde::Serialize;

use crate::signatures::{
    FieldDef, FieldKind, FieldRoot, FieldRule, LockMode, LockTarget, PlayerCfg,
};

/// Com o `Pawn` nulo há mais que isto, o controller guardado é procurado de novo (spec 6.3).
const PAWN_NULL_RESEARCH: Duration = Duration::from_secs(30);
/// Valor de `object` que indica o próprio pawn.
const PAWN_OBJECT: &str = "pawn";
/// Motivo de um campo com tipo, tamanho ou máscara inesperados.
const INCOHERENT: &str = "reflexão incoerente";
/// Campos do controller mostrados pelo `gt ue player`.
const REPORT_POINTERS: [&str; 3] = ["Pawn", "Character", "AcknowledgedPawn"];

/// Alvo de uma trava, resolvido.
#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedTarget {
    /// Campo `float` do mesmo objeto, relido a cada ciclo.
    Field(Field),
    Const(f32),
}

/// Regra de um cheat de valor, com os campos já achados na reflexão.
#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedRule {
    Lock {
        field: Field,
        mode: LockMode,
        target: ResolvedTarget,
        min: f32,
        max: f32,
    },
    /// Campos `bool`, todos com `bits`.
    ClearBits { fields: Vec<Field> },
    /// Campos do mesmo objeto e `kind`, escritos com o mesmo valor (spec 006, 6.4). Com `target`
    /// o valor é constante; sem ele, vem do `[field.value]`.
    Set {
        fields: Vec<Field>,
        kind: FieldKind,
        target: Option<f64>,
        /// Valor de cada campo, só no `kind = "bool"` (spec 007, 6.4).
        bools: Option<Vec<bool>>,
    },
    /// Soma a um campo `int`, sem diário (spec 006, 6.6).
    Add { field: Field, result_max: i64 },
}

/// Um salto do caminho até o objeto de um cheat: o campo de objeto e a classe esperada do
/// objeto para onde ele aponta (conferida com `is_a` a cada leitura).
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectHop {
    pub field: Field,
    pub class: u64,
}

/// Cheat de valor resolvido pela reflexão.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedField {
    /// Raiz do caminho até o objeto.
    pub root: FieldRoot,
    /// Caminho de campos de objeto a partir do pawn; vazio quando o objeto é o próprio pawn. Com
    /// mais de um salto (`PetMasterComponent.CurrentPetFull.HealthComponent`), o objeto é de
    /// outro ator (o pet), e a falta dele só afeta este cheat.
    pub object_path: Vec<ObjectHop>,
    /// Classe esperada do objeto final, conferida com `is_a` a cada leitura.
    pub object_class: u64,
    pub rule: ResolvedRule,
}

impl ResolvedField {
    /// O objeto não é do personagem (caminho com mais de um salto, como o do pet).
    pub fn is_secondary(&self) -> bool {
        self.object_path.len() > 1
    }
}

/// De quem é o objeto que falta num cheat em espera: decide o texto da marca na lista.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingKind {
    /// O pet (outro ator): `aguardando o pet`.
    Pet,
    /// Um componente do personagem: `aguardando o personagem`.
    Component,
    /// O objeto do mundo (raiz `game_state`): `aguardando o mundo` (spec 007, 6.3).
    World,
}

/// Tipo do objeto que falta no cheat `resolved` (a nota em si mora em `fields.rs`).
pub fn missing_kind(resolved: &ResolvedField) -> MissingKind {
    // Na raiz `game_state` um caminho de vários saltos nunca é o pet.
    if resolved.root == FieldRoot::GameState {
        MissingKind::World
    } else if resolved.is_secondary() {
        MissingKind::Pet
    } else {
        MissingKind::Component
    }
}

/// Classe do controller e campos que levam dele ao pawn. Falta aqui afeta todos os cheats.
#[derive(Debug, Clone)]
struct PlayerPath {
    controller_class: u64,
    pawn_field: Field,
    alt_pawn_fields: Vec<Field>,
}

/// Classes e campos do caminho controller → `Outer` → `Level.OwningWorld` → `World.GameState`.
#[derive(Debug, Clone)]
struct GameStatePath {
    level_class: u64,
    world_field: Field,
    world_class: u64,
    game_state_field: Field,
    game_state_class: u64,
}

#[derive(Debug, Clone)]
struct CatalogEntry {
    id: String,
    /// Nome curto do cheat (para os eventos).
    name: String,
    /// Grade do valor ajustável (`[field.value]`), se houver.
    grid: Option<ValueGrid>,
    resolved: Result<ResolvedField, String>,
}

/// Cheats de valor de uma conexão, resolvidos uma vez pelo índice da reflexão.
pub struct FieldCatalog {
    layout: UeLayout,
    path: Result<PlayerPath, String>,
    /// Classe do pawn; a falta dela afeta só os cheats com raiz no pawn.
    pawn_class: Result<u64, String>,
    /// Caminho do `GameState`; a falta dele afeta só os cheats com essa raiz.
    game_state: Result<GameStatePath, String>,
    entries: Vec<CatalogEntry>,
    /// `(classe, ancestral) → is_a`. As classes não mudam com o jogo rodando.
    is_a_cache: Mutex<HashMap<(u64, u64), bool>>,
}

impl FieldCatalog {
    /// Resolve o caminho do `[ue.player]` e o campo de cada `[[field]]`. O que faltar vira o
    /// motivo de indisponibilidade do cheat, com os textos da spec 6.7.
    pub fn resolve<M: ReadMem + ?Sized>(
        index: &ReflectionIndex,
        mem: &M,
        cfg: &PlayerCfg,
        defs: &[FieldDef],
    ) -> Self {
        let path = resolve_path(index, mem, cfg);
        let pawn_class = find_class(index, &cfg.pawn_class);
        let game_state = resolve_game_state(index, mem, cfg);
        let entries = defs
            .iter()
            .map(|def| {
                let base = match (&path, def.root) {
                    (Err(reason), _) => Err(reason.clone()),
                    (Ok(_), FieldRoot::Pawn) => pawn_class.clone(),
                    (Ok(_), FieldRoot::GameState) => game_state
                        .as_ref()
                        .map(|g| g.game_state_class)
                        .map_err(Clone::clone),
                };
                let resolved = base.and_then(|class| resolve_field(index, mem, class, def));
                if let Err(reason) = &resolved {
                    tracing::debug!(id = %def.id, "cheat de valor indisponível: {reason}");
                }
                CatalogEntry {
                    id: def.id.clone(),
                    name: def.name.clone(),
                    grid: def.value.clone(),
                    resolved,
                }
            })
            .collect();
        Self {
            layout: *index.layout(),
            path,
            pawn_class,
            game_state,
            entries,
            is_a_cache: Mutex::new(HashMap::new()),
        }
    }

    /// `Ok` se o cheat `id` foi resolvido; senão, o motivo exato.
    pub fn avail(&self, id: &str) -> Result<(), String> {
        match self.get(id) {
            Some(r) => r.as_ref().map(|_| ()).map_err(Clone::clone),
            None => Err(format!("{id} desconhecido")),
        }
    }

    /// Nome curto do cheat `id`.
    pub fn name(&self, id: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.name.as_str())
    }

    /// Grade do valor ajustável do cheat `id` (`[field.value]`), se ele tiver uma.
    pub fn grid(&self, id: &str) -> Option<&ValueGrid> {
        self.entries
            .iter()
            .find(|e| e.id == id)
            .and_then(|e| e.grid.as_ref())
    }

    /// `id` dos cheats, na ordem das definições.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.id.as_str())
    }

    /// Resultado da resolução do cheat `id`.
    pub fn get(&self, id: &str) -> Option<&Result<ResolvedField, String>> {
        self.entries
            .iter()
            .find(|e| e.id == id)
            .map(|e| &e.resolved)
    }

    /// `is_a` com cache por (classe, ancestral). Erros de leitura não ficam no cache.
    fn is_a<M: ReadMem + ?Sized>(
        &self,
        index: &ReflectionIndex,
        mem: &M,
        class: u64,
        ancestor: u64,
    ) -> Result<bool, UeError> {
        let key = (class, ancestor);
        let cached = self
            .is_a_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .copied();
        if let Some(v) = cached {
            return Ok(v);
        }
        let v = index.is_a(mem, class, ancestor)?;
        self.is_a_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, v);
        Ok(v)
    }

    /// `true` se o objeto é da classe `class` (ou filha). Objeto ilegível: `Unreadable`.
    fn object_is_a<M: ReadMem + ?Sized>(
        &self,
        index: &ReflectionIndex,
        mem: &M,
        obj: u64,
        class: u64,
    ) -> Result<bool, Absent> {
        let own = index.class_of(mem, obj).map_err(unreadable)?;
        self.is_a(index, mem, own, class).map_err(unreadable)
    }
}

/// Divide `Pacote.Nome` (o formato já foi validado na carga das assinaturas).
fn split_class(path: &str) -> (&str, &str) {
    path.rsplit_once('.').unwrap_or(("", path))
}

fn find_class(index: &ReflectionIndex, path: &str) -> Result<u64, String> {
    let (pkg, name) = split_class(path);
    index.find_class(pkg, name).map_err(|e| {
        tracing::debug!("classe {path}: {e}");
        match e {
            UeError::Ambiguous(_) => format!("classe {name} ambígua"),
            _ => format!("classe {name} não encontrada"),
        }
    })
}

fn find_field<M: ReadMem + ?Sized>(
    index: &ReflectionIndex,
    mem: &M,
    class: u64,
    name: &str,
) -> Result<Field, String> {
    index.find_field(mem, class, name).map_err(|e| {
        tracing::debug!("campo {name}: {e}");
        match e {
            UeError::NotFound(_) => format!("campo {name} não encontrado"),
            _ => INCOHERENT.to_string(),
        }
    })
}

/// Campo que guarda um ponteiro para objeto.
fn object_field<M: ReadMem + ?Sized>(
    index: &ReflectionIndex,
    mem: &M,
    class: u64,
    name: &str,
) -> Result<Field, String> {
    let field = find_field(index, mem, class, name)?;
    if is_object_family(&field.kind) && field.size == 8 {
        Ok(field)
    } else {
        tracing::debug!(
            "campo {name} é {} de {} bytes, esperado ponteiro",
            field.kind,
            field.size
        );
        Err(INCOHERENT.to_string())
    }
}

/// Campo `float` de 4 bytes; `name` aceita caminho com ponto (`HungerSettings.MaxValue`).
fn float_field<M: ReadMem + ?Sized>(
    index: &ReflectionIndex,
    mem: &M,
    class: u64,
    name: &str,
) -> Result<Field, String> {
    let field = index.find_field_path(mem, class, name).map_err(|e| {
        tracing::debug!("campo {name}: {e}");
        match e {
            UeError::NotFound(_) => format!("campo {name} não encontrado"),
            _ => INCOHERENT.to_string(),
        }
    })?;
    if field.kind == "FloatProperty" && field.size == 4 {
        Ok(field)
    } else {
        tracing::debug!(
            "campo {name} é {} de {} bytes, esperado float",
            field.kind,
            field.size
        );
        Err(INCOHERENT.to_string())
    }
}

/// Campo `int` (`IntProperty`) de 4 bytes.
fn int_field<M: ReadMem + ?Sized>(
    index: &ReflectionIndex,
    mem: &M,
    class: u64,
    name: &str,
) -> Result<Field, String> {
    let field = index.find_field_path(mem, class, name).map_err(|e| {
        tracing::debug!("campo {name}: {e}");
        match e {
            UeError::NotFound(_) => format!("campo {name} não encontrado"),
            _ => INCOHERENT.to_string(),
        }
    })?;
    if field.kind == "IntProperty" && field.size == 4 {
        Ok(field)
    } else {
        tracing::debug!(
            "campo {name} é {} de {} bytes, esperado int",
            field.kind,
            field.size
        );
        Err(INCOHERENT.to_string())
    }
}

/// Campo `bool` de byte inteiro (`bool` nativo do C++): `BoolProperty` de 1 byte, máscara
/// `0xff` e o byte da máscara igual ao `offset` (spec 007, 6.4). Bits de verdade ficam para o
/// [`bool_field`] do `clear_bits`.
fn byte_bool_field<M: ReadMem + ?Sized>(
    index: &ReflectionIndex,
    mem: &M,
    class: u64,
    name: &str,
) -> Result<Field, String> {
    let field = index.find_field_path(mem, class, name).map_err(|e| {
        tracing::debug!("campo {name}: {e}");
        match e {
            UeError::NotFound(_) => format!("campo {name} não encontrado"),
            _ => INCOHERENT.to_string(),
        }
    })?;
    match field.bits {
        Some(bits)
            if field.kind == "BoolProperty"
                && field.size == 1
                && bits.mask == 0xff
                && bits.byte == field.offset =>
        {
            Ok(field)
        }
        _ => {
            tracing::debug!(
                "campo {name} é {} de {} bytes, esperado bool de byte inteiro",
                field.kind,
                field.size
            );
            Err(INCOHERENT.to_string())
        }
    }
}

/// Campo `bool` com máscara não nula.
fn bool_field<M: ReadMem + ?Sized>(
    index: &ReflectionIndex,
    mem: &M,
    class: u64,
    name: &str,
) -> Result<Field, String> {
    let field = find_field(index, mem, class, name)?;
    match field.bits {
        Some(bits) if field.kind == "BoolProperty" && bits.mask != 0 => Ok(field),
        _ => {
            tracing::debug!("campo {name} é {}, esperado bool com máscara", field.kind);
            Err(INCOHERENT.to_string())
        }
    }
}

fn resolve_path<M: ReadMem + ?Sized>(
    index: &ReflectionIndex,
    mem: &M,
    cfg: &PlayerCfg,
) -> Result<PlayerPath, String> {
    let controller_class = find_class(index, &cfg.controller_class)?;
    let pawn_field = object_field(index, mem, controller_class, &cfg.pawn_field)?;
    // Um campo alternativo que falta só não é tentado: o caminho principal continua valendo.
    let alt_pawn_fields = cfg
        .alt_pawn_fields
        .iter()
        .filter_map(|name| {
            object_field(index, mem, controller_class, name)
                .inspect_err(|reason| tracing::debug!("campo alternativo {name}: {reason}"))
                .ok()
        })
        .collect();
    Ok(PlayerPath {
        controller_class,
        pawn_field,
        alt_pawn_fields,
    })
}

/// Resolve o caminho do `GameState`, separado do do pawn: o que faltar aqui deixa indisponíveis
/// só os cheats dessa raiz.
fn resolve_game_state<M: ReadMem + ?Sized>(
    index: &ReflectionIndex,
    mem: &M,
    cfg: &PlayerCfg,
) -> Result<GameStatePath, String> {
    let Some(gs) = &cfg.game_state else {
        return Err("caminho do game_state não configurado".to_string());
    };
    let level_class = find_class(index, &gs.level_class)?;
    let world_field = object_field(index, mem, level_class, &gs.world_field)?;
    let world_class = find_class(index, &gs.world_class)?;
    let game_state_field = object_field(index, mem, world_class, &gs.game_state_field)?;
    let game_state_class = find_class(index, &gs.game_state_class)?;
    Ok(GameStatePath {
        level_class,
        world_field,
        world_class,
        game_state_field,
        game_state_class,
    })
}

/// Resolve um `[[field]]`; `base_class` é a classe de onde parte o caminho do objeto (a do pawn
/// ou a do `GameState`, conforme a raiz).
fn resolve_field<M: ReadMem + ?Sized>(
    index: &ReflectionIndex,
    mem: &M,
    base_class: u64,
    def: &FieldDef,
) -> Result<ResolvedField, String> {
    let object_class = find_class(index, &def.object_class)?;
    let mut object_path = Vec::new();
    if def.object != PAWN_OBJECT {
        let hops: Vec<&str> = def.object.split('.').collect();
        let mut class = base_class;
        for (i, name) in hops.iter().enumerate() {
            if i + 1 == hops.len() {
                let field = object_field(index, mem, class, name)?;
                object_path.push(ObjectHop {
                    field,
                    class: object_class,
                });
                break;
            }
            // Salto do meio: a classe seguinte vem do próprio campo (`PropertyClass`).
            let (field, next) = index.object_field_class(mem, class, name).map_err(|e| {
                tracing::debug!("campo {name}: {e}");
                match e {
                    UeError::NotFound(_) => format!("campo {name} não encontrado"),
                    _ => INCOHERENT.to_string(),
                }
            })?;
            object_path.push(ObjectHop { field, class: next });
            class = next;
        }
    }
    let rule = match &def.rule {
        FieldRule::Lock {
            field,
            mode,
            target,
            min,
            max,
        } => ResolvedRule::Lock {
            field: float_field(index, mem, object_class, field)?,
            mode: *mode,
            target: match target {
                LockTarget::Field(name) => {
                    ResolvedTarget::Field(float_field(index, mem, object_class, name)?)
                }
                LockTarget::Const(v) => ResolvedTarget::Const(*v),
            },
            min: *min,
            max: *max,
        },
        FieldRule::ClearBits { fields } => ResolvedRule::ClearBits {
            fields: fields
                .iter()
                .map(|name| bool_field(index, mem, object_class, name))
                .collect::<Result<_, _>>()?,
        },
        FieldRule::Set {
            fields,
            kind,
            target,
            bools,
        } => ResolvedRule::Set {
            fields: fields
                .iter()
                .map(|name| match kind {
                    FieldKind::Float => float_field(index, mem, object_class, name),
                    FieldKind::Int => int_field(index, mem, object_class, name),
                    FieldKind::Bool => byte_bool_field(index, mem, object_class, name),
                })
                .collect::<Result<_, _>>()?,
            kind: *kind,
            target: *target,
            bools: bools.clone(),
        },
        FieldRule::Add { field, result_max } => ResolvedRule::Add {
            field: int_field(index, mem, object_class, field)?,
            result_max: *result_max,
        },
    };
    Ok(ResolvedField {
        root: def.root,
        object_path,
        object_class,
        rule,
    })
}

/// Resultado da procura do controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerFind {
    Found(u64),
    /// Nenhuma instância (menu, carregamento) ou caminho não resolvido.
    Absent,
    /// Mais de uma instância com `Pawn` não nulo, ou várias e todas com `Pawn` nulo.
    Ambiguous,
}

/// Procura o controller do jogador numa varredura da tabela de objetos (cara; spec 6.3).
pub fn find_controller<M: ReadMem + ?Sized>(
    index: &ReflectionIndex,
    mem: &M,
    objects: &ObjectArray,
    catalog: &FieldCatalog,
    budget: &ScanBudget,
) -> Result<ControllerFind, UeError> {
    let Ok(path) = &catalog.path else {
        return Ok(ControllerFind::Absent);
    };
    let mut found = index.instances_of(mem, objects, path.controller_class, budget)?;
    // O mesmo objeto em dois itens da tabela (um item reaproveitado no meio da varredura) conta uma vez.
    found.sort_unstable();
    found.dedup();
    Ok(match found.as_slice() {
        [] => ControllerFind::Absent,
        [only] => ControllerFind::Found(*only),
        many => {
            let pawn_at = u64::from(path.pawn_field.offset);
            let with_pawn: Vec<u64> = many
                .iter()
                .copied()
                .filter(|&c| read_u64(mem, c + pawn_at).is_ok_and(|p| p != 0))
                .collect();
            match with_pawn.as_slice() {
                [only] => ControllerFind::Found(*only),
                _ => {
                    tracing::debug!(
                        instances = many.len(),
                        with_pawn = with_pawn.len(),
                        "controller ambíguo"
                    );
                    ControllerFind::Ambiguous
                }
            }
        }
    })
}

/// Por que o personagem não está disponível neste ciclo.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Absent {
    #[error("controller não encontrado")]
    NoController,
    #[error("controller ambíguo (mais de um candidato)")]
    ControllerAmbiguous,
    #[error("o controller guardado não é mais válido")]
    ControllerInvalid,
    #[error("Pawn nulo")]
    PawnNull,
    #[error("o Pawn não é o personagem do jogador")]
    PawnWrongClass,
    /// Nome do campo do pawn (ou `pawn`) cujo objeto não é da classe esperada.
    #[error("{0} não é da classe esperada")]
    ComponentWrongClass(String),
    /// Nome do campo que passa do `PropertiesSize` da classe do objeto.
    #[error("o campo {0} passa do tamanho do objeto")]
    FieldOutsideObject(String),
    #[error("leitura da memória do jogo falhou")]
    Unreadable,
}

fn unreadable(e: UeError) -> Absent {
    tracing::debug!("leitura do personagem falhou: {e}");
    Absent::Unreadable
}

/// Objeto do caminho, com a guarda lida neste ciclo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjSnap {
    pub addr: u64,
    /// Vtable (8 bytes em `+0`), `InternalIndex` e `ClassPrivate`.
    pub guard: Vec<GuardBytes>,
}

/// Byte de bits de um cheat, com a máscara unida dos campos que caem nele.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BitByte {
    pub addr: u64,
    pub value: u8,
    pub mask: u8,
}

/// Valores lidos de um cheat.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldValues {
    Lock {
        obj: u64,
        addr: u64,
        current: f32,
        target: f32,
    },
    Bits {
        obj: u64,
        bytes: Vec<BitByte>,
    },
    /// Endereço e bytes atuais de cada campo de um `set` (`FieldKind::size` bytes cada).
    Set {
        obj: u64,
        fields: Vec<(u64, Vec<u8>)>,
    },
}

/// O personagem lido num ciclo.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerSnapshot {
    pub controller: u64,
    pub pawn: u64,
    pub objects: Vec<ObjSnap>,
    /// Por `id`, só dos cheats resolvidos.
    pub values: HashMap<String, FieldValues>,
    /// Cheats cujo objeto (um componente ou o pet) não está disponível neste ciclo, com o motivo.
    /// O personagem e os outros cheats continuam valendo.
    pub missing: HashMap<String, Absent>,
}

impl PlayerSnapshot {
    fn guard_of(&self, obj: u64) -> Vec<GuardBytes> {
        self.objects
            .iter()
            .find(|o| o.addr == obj)
            .map(|o| o.guard.clone())
            .unwrap_or_default()
    }
}

/// Lê a guarda de identidade do objeto: vtable, `InternalIndex` e `ClassPrivate`.
fn read_guard<M: ReadMem + ?Sized>(
    mem: &M,
    layout: &UeLayout,
    obj: u64,
) -> Result<Vec<GuardBytes>, UeError> {
    let u = &layout.uobject;
    [(0, 8), (u.index, 4), (u.class, 8)]
        .into_iter()
        .map(|(off, len)| {
            let addr = obj + u64::from(off);
            Ok(GuardBytes {
                addr,
                bytes: read_vec(mem, addr, len)?,
            })
        })
        .collect()
}

/// Leituras de um ciclo, com o `PropertiesSize` de cada objeto em cache.
struct Reader<'a, M: ReadMem + ?Sized> {
    mem: &'a M,
    index: &'a ReflectionIndex,
    catalog: &'a FieldCatalog,
    props: HashMap<u64, u32>,
    objects: Vec<ObjSnap>,
}

impl<M: ReadMem + ?Sized> Reader<'_, M> {
    /// Confere `offset + tamanho ≤ PropertiesSize` da classe do objeto (spec 6.4).
    fn check_fits(&mut self, obj: u64, name: &str, offset: u32, size: u32) -> Result<(), Absent> {
        let props = match self.props.get(&obj) {
            Some(&p) => p,
            None => {
                let class = self.index.class_of(self.mem, obj).map_err(unreadable)?;
                let p = self.index.props_size(self.mem, class).map_err(unreadable)?;
                self.props.insert(obj, p);
                p
            }
        };
        if u64::from(offset) + u64::from(size) <= u64::from(props) {
            Ok(())
        } else {
            tracing::debug!(obj, name, offset, size, props, "campo fora do objeto");
            Err(Absent::FieldOutsideObject(name.to_string()))
        }
    }

    /// Ponteiro guardado no campo `field` do objeto.
    fn pointer(&mut self, obj: u64, field: &Field) -> Result<u64, Absent> {
        self.check_fits(obj, &field.name, field.offset, field.size)?;
        read_u64(self.mem, obj + u64::from(field.offset)).map_err(unreadable)
    }

    fn float(&mut self, obj: u64, field: &Field) -> Result<f32, Absent> {
        self.check_fits(obj, &field.name, field.offset, field.size)?;
        let raw = read_u32(self.mem, obj + u64::from(field.offset)).map_err(unreadable)?;
        Ok(f32::from_bits(raw))
    }

    /// Os `len` bytes do campo, como estão na memória.
    fn raw(&mut self, obj: u64, field: &Field, len: u32) -> Result<Vec<u8>, Absent> {
        self.check_fits(obj, &field.name, field.offset, len)?;
        read_vec(self.mem, obj + u64::from(field.offset), len as usize).map_err(unreadable)
    }

    /// O objeto é um CDO (bit `cdo_flag` nas `ObjectFlags`)? Nunca se escreve num CDO (spec 007, 6.3).
    fn is_cdo(&self, obj: u64) -> Result<bool, Absent> {
        let u = &self.catalog.layout.uobject;
        let flags = read_u32(self.mem, obj + u64::from(u.flags)).map_err(unreadable)?;
        Ok(flags & u.cdo_flag != 0)
    }

    /// Registra o objeto no caminho (com a guarda), uma vez.
    fn visit(&mut self, obj: u64) -> Result<(), Absent> {
        if self.objects.iter().all(|o| o.addr != obj) {
            let guard = read_guard(self.mem, &self.catalog.layout, obj).map_err(unreadable)?;
            self.objects.push(ObjSnap { addr: obj, guard });
        }
        Ok(())
    }

    /// Objeto de um cheat: o pawn ou o fim do caminho de campos de objeto, com a classe de cada
    /// salto conferida.
    fn object(&mut self, pawn: u64, r: &ResolvedField) -> Result<u64, Absent> {
        let mut obj = pawn;
        if r.object_path.is_empty() {
            let ok = self
                .catalog
                .object_is_a(self.index, self.mem, pawn, r.object_class)?;
            if !ok {
                return Err(Absent::ComponentWrongClass(PAWN_OBJECT.to_string()));
            }
        }
        for hop in &r.object_path {
            let next = self.pointer(obj, &hop.field)?;
            let ok = next != 0
                && self
                    .catalog
                    .object_is_a(self.index, self.mem, next, hop.class)?;
            if !ok {
                return Err(Absent::ComponentWrongClass(hop.field.name.clone()));
            }
            obj = next;
        }
        self.visit(obj)?;
        Ok(obj)
    }

    /// Valores do cheat no ciclo; `None` para a ação (`add`), lida só sob demanda.
    fn values(&mut self, obj: u64, rule: &ResolvedRule) -> Result<Option<FieldValues>, Absent> {
        Ok(Some(match rule {
            ResolvedRule::Lock { field, target, .. } => FieldValues::Lock {
                obj,
                addr: obj + u64::from(field.offset),
                current: self.float(obj, field)?,
                target: match target {
                    ResolvedTarget::Field(f) => self.float(obj, f)?,
                    ResolvedTarget::Const(v) => *v,
                },
            },
            ResolvedRule::ClearBits { fields } => {
                let mut bytes: Vec<BitByte> = Vec::new();
                for f in fields {
                    let Some(bits) = f.bits else { continue };
                    self.check_fits(obj, &f.name, bits.byte, 1)?;
                    let addr = obj + u64::from(bits.byte);
                    if let Some(b) = bytes.iter_mut().find(|b| b.addr == addr) {
                        b.mask |= bits.mask;
                        continue;
                    }
                    let value = read_u8(self.mem, addr).map_err(unreadable)?;
                    bytes.push(BitByte {
                        addr,
                        value,
                        mask: bits.mask,
                    });
                }
                FieldValues::Bits { obj, bytes }
            }
            ResolvedRule::Set { fields, kind, .. } => {
                let len = kind.size();
                let mut out = Vec::with_capacity(fields.len());
                for f in fields {
                    out.push((obj + u64::from(f.offset), self.raw(obj, f, len)?));
                }
                FieldValues::Set { obj, fields: out }
            }
            // A soma só é lida sob demanda (`read_game_state`), nunca no ciclo.
            ResolvedRule::Add { .. } => return Ok(None),
        }))
    }
}

/// O item da tabela no `InternalIndex` do controller ainda aponta para ele e a classe é a esperada.
fn check_controller<M: ReadMem + ?Sized>(
    mem: &M,
    index: &ReflectionIndex,
    catalog: &FieldCatalog,
    objects: &ObjectArray,
    controller: u64,
    path: &PlayerPath,
) -> Result<(), Absent> {
    let layout = &catalog.layout;
    let internal = read_u32(mem, controller + u64::from(layout.uobject.index))
        .map_err(|_| Absent::ControllerInvalid)?;
    match objects.object_at(mem, layout, internal) {
        Ok(Some(obj)) if obj == controller => {}
        _ => return Err(Absent::ControllerInvalid),
    }
    match catalog.object_is_a(index, mem, controller, path.controller_class) {
        Ok(true) => Ok(()),
        _ => Err(Absent::ControllerInvalid),
    }
}

/// Por que o objeto do `GameState` não está disponível agora.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GameStateAbsent {
    #[error("o controller guardado não é mais válido")]
    ControllerInvalid,
    /// Nome do salto (`Outer`, `OwningWorld`, `GameState`, o componente) que não confere.
    #[error("{0} ausente ou de classe inesperada")]
    Missing(String),
}

/// Lê, sob demanda, o caminho controller → `Outer` → `Level.OwningWorld` → `World.GameState` →
/// objeto do cheat `id` (spec 006, 6.3), sem tocar no pawn. Devolve o objeto, o endereço do
/// campo e a guarda de identidade do objeto.
pub fn read_game_state<M: ReadMem + ?Sized>(
    mem: &M,
    index: &ReflectionIndex,
    catalog: &FieldCatalog,
    objects: &ObjectArray,
    controller: u64,
    id: &str,
) -> Result<(u64, u64, Vec<GuardBytes>), GameStateAbsent> {
    walk_game_state(
        mem,
        index,
        catalog,
        objects,
        controller,
        id,
        &mut GameStateReport::default(),
    )
}

/// Converte a falha de uma leitura no `GameState` em `Missing(salto)`: o nome do campo que não
/// confere, ou `hop` quando a leitura em si falhou.
fn gone(hop: &str) -> impl Fn(Absent) -> GameStateAbsent {
    let hop = hop.to_string();
    move |absent: Absent| match absent {
        Absent::ComponentWrongClass(name) | Absent::FieldOutsideObject(name) => {
            GameStateAbsent::Missing(name)
        }
        _ => GameStateAbsent::Missing(hop.clone()),
    }
}

/// Anda do controller (já conferido) até o `GameState`: `Outer` → `Level.OwningWorld` →
/// `World.GameState`, anotando em `hops` cada salto que deu certo. Usada pela ação sob demanda
/// ([`walk_game_state`]) e pela leitura do mundo no ciclo ([`read_world`]).
fn walk_to_game_state<M: ReadMem + ?Sized>(
    r: &mut Reader<'_, M>,
    controller: u64,
    gs: &GameStatePath,
    hops: &mut GameStateReport,
) -> Result<u64, GameStateAbsent> {
    let missing = |hop: &str| GameStateAbsent::Missing(hop.to_string());
    // O `Outer` do controller é o nível; ele mora no cabeçalho do `UObject`, não na reflexão.
    let outer = read_u64(
        r.mem,
        controller + u64::from(r.catalog.layout.uobject.outer),
    )
    .map_err(|_| missing("Outer"))?;
    if outer == 0
        || !r
            .catalog
            .object_is_a(r.index, r.mem, outer, gs.level_class)
            .map_err(|_| missing("Outer"))?
    {
        return Err(missing("Outer"));
    }
    hops.level = Some(outer);
    let mut next = |from: u64, hop: &Field, class: u64| -> Result<u64, GameStateAbsent> {
        let ptr = r.pointer(from, hop).map_err(gone(&hop.name))?;
        let ok = ptr != 0
            && r.catalog
                .object_is_a(r.index, r.mem, ptr, class)
                .map_err(gone(&hop.name))?;
        if ok { Ok(ptr) } else { Err(missing(&hop.name)) }
    };
    let world = next(outer, &gs.world_field, gs.world_class)?;
    hops.world = Some(world);
    let state = next(world, &gs.game_state_field, gs.game_state_class)?;
    hops.game_state = Some(state);
    Ok(state)
}

/// Como [`read_game_state`], anotando em `hops` o endereço de cada salto que deu certo (para o
/// relatório mostrar até onde o caminho chegou quando um deles falha).
fn walk_game_state<M: ReadMem + ?Sized>(
    mem: &M,
    index: &ReflectionIndex,
    catalog: &FieldCatalog,
    objects: &ObjectArray,
    controller: u64,
    id: &str,
    hops: &mut GameStateReport,
) -> Result<(u64, u64, Vec<GuardBytes>), GameStateAbsent> {
    let Ok(path) = &catalog.path else {
        return Err(GameStateAbsent::ControllerInvalid);
    };
    check_controller(mem, index, catalog, objects, controller, path)
        .map_err(|_| GameStateAbsent::ControllerInvalid)?;
    let missing = |hop: &str| GameStateAbsent::Missing(hop.to_string());
    let (Some(Ok(resolved)), Ok(gs)) = (catalog.get(id), &catalog.game_state) else {
        return Err(missing(id));
    };
    // Só a ação de raiz `game_state` é lida aqui; um `add` de raiz `pawn` não é lido nesta spec.
    let field = match &resolved.rule {
        ResolvedRule::Add { field, .. } if resolved.root == FieldRoot::GameState => field,
        _ => return Err(missing(id)),
    };
    let mut r = Reader {
        mem,
        index,
        catalog,
        props: HashMap::new(),
        objects: Vec::new(),
    };
    let state = walk_to_game_state(&mut r, controller, gs, hops)?;
    let obj = r.object(state, resolved).map_err(gone(id))?;
    hops.object = Some(obj);
    r.check_fits(obj, &field.name, field.offset, field.size)
        .map_err(gone(&field.name))?;
    let guard = r
        .objects
        .iter()
        .find(|o| o.addr == obj)
        .map(|o| o.guard.clone())
        .unwrap_or_default();
    Ok((obj, obj + u64::from(field.offset), guard))
}

/// Os `set` da raiz `game_state` lidos num ciclo (spec 007, 6.3).
#[derive(Debug, Clone, PartialEq)]
pub struct WorldSnapshot {
    /// O `GameState` vivo, de onde saem os caminhos.
    pub game_state: u64,
    pub objects: Vec<ObjSnap>,
    /// Por `id`, só dos cheats do mundo resolvidos e com o objeto no lugar.
    pub values: HashMap<String, FieldValues>,
    /// Cheats do mundo cujo objeto não está disponível neste ciclo, com o salto que falhou.
    pub missing: HashMap<String, GameStateAbsent>,
}

/// Lê os `set` de raiz `game_state` pelo caminho controller → `Outer` → `World` → `GameState`,
/// sem tocar no pawn (spec 007, 6.3). Controller inválido ou um salto até o `GameState` falhando
/// derrubam a leitura; um salto do objeto de um cheat (nulo, classe errada, ilegível, fora do
/// objeto ou um CDO) só deixa aquele `id` em `missing`.
pub fn read_world<M: ReadMem + ?Sized>(
    mem: &M,
    index: &ReflectionIndex,
    catalog: &FieldCatalog,
    objects: &ObjectArray,
    controller: u64,
) -> Result<WorldSnapshot, GameStateAbsent> {
    let Ok(path) = &catalog.path else {
        return Err(GameStateAbsent::ControllerInvalid);
    };
    check_controller(mem, index, catalog, objects, controller, path)
        .map_err(|_| GameStateAbsent::ControllerInvalid)?;
    let Ok(gs) = &catalog.game_state else {
        return Err(GameStateAbsent::Missing("GameState".to_string()));
    };
    let mut r = Reader {
        mem,
        index,
        catalog,
        props: HashMap::new(),
        objects: Vec::new(),
    };
    let state = walk_to_game_state(&mut r, controller, gs, &mut GameStateReport::default())?;

    let mut values = HashMap::new();
    let mut missing = HashMap::new();
    for entry in &catalog.entries {
        let Ok(resolved) = &entry.resolved else {
            continue;
        };
        if resolved.root != FieldRoot::GameState
            || !matches!(resolved.rule, ResolvedRule::Set { .. })
        {
            continue;
        }
        // Falha de leitura sem nome de campo: o último salto do caminho (as configurações).
        let last = resolved
            .object_path
            .last()
            .map_or(entry.id.as_str(), |h| h.field.name.as_str());
        let before = r.objects.len();
        let read = r
            .object(state, resolved)
            .and_then(|obj| match r.is_cdo(obj)? {
                true => {
                    tracing::debug!(id = %entry.id, obj, "objeto do mundo é um CDO");
                    Err(Absent::ComponentWrongClass(last.to_string()))
                }
                false => r.values(obj, &resolved.rule),
            });
        match read {
            Ok(Some(v)) => {
                values.insert(entry.id.clone(), v);
            }
            Ok(None) => {}
            Err(absent) => {
                // Um objeto rejeitado não fica entre os do ciclo.
                r.objects.truncate(before);
                missing.insert(entry.id.clone(), gone(last)(absent));
            }
        }
    }
    Ok(WorldSnapshot {
        game_state: state,
        objects: r.objects,
        values,
        missing,
    })
}

/// Lê o personagem a partir do controller guardado, revalidando o caminho inteiro (spec 6.3,
/// passos 1 a 4, e as conferências de 6.4 que não dependem do `maps`).
pub fn read_player<M: ReadMem + ?Sized>(
    mem: &M,
    index: &ReflectionIndex,
    catalog: &FieldCatalog,
    objects: &ObjectArray,
    controller: u64,
) -> Result<PlayerSnapshot, Absent> {
    let Ok(path) = &catalog.path else {
        return Err(Absent::NoController);
    };
    // 1. O item da tabela no `InternalIndex` ainda aponta para o controller, da mesma classe.
    check_controller(mem, index, catalog, objects, controller, path)?;
    // Sem a classe do pawn nenhum cheat do pawn existe (todos indisponíveis).
    let Ok(pawn_class) = catalog.pawn_class.clone() else {
        return Err(Absent::PawnWrongClass);
    };

    let mut r = Reader {
        mem,
        index,
        catalog,
        props: HashMap::new(),
        objects: Vec::new(),
    };

    // 2. O pawn.
    let pawn = r.pointer(controller, &path.pawn_field)?;
    if pawn == 0 {
        return Err(Absent::PawnNull);
    }

    // 3. Só o personagem do jogador; uma montaria no `Pawn` nunca é escrita.
    let pawn = if catalog.object_is_a(index, mem, pawn, pawn_class)? {
        pawn
    } else {
        let mut alt = None;
        for f in &path.alt_pawn_fields {
            let p = r.pointer(controller, f)?;
            if p != 0 && catalog.object_is_a(index, mem, p, pawn_class)? {
                alt = Some(p);
                break;
            }
        }
        alt.ok_or(Absent::PawnWrongClass)?
    };
    r.visit(pawn)?;

    // 4. Cada componente, conferido com a classe esperada.
    let mut values = HashMap::new();
    let mut missing = HashMap::new();
    for entry in &catalog.entries {
        let Ok(resolved) = &entry.resolved else {
            continue;
        };
        // Raiz `game_state` e ações (`add`) são lidas sob demanda, fora deste snapshot.
        if resolved.root != FieldRoot::Pawn || matches!(resolved.rule, ResolvedRule::Add { .. }) {
            continue;
        }
        let read = r
            .object(pawn, resolved)
            .and_then(|obj| r.values(obj, &resolved.rule));
        match read {
            Ok(Some(v)) => {
                values.insert(entry.id.clone(), v);
            }
            Ok(None) => {}
            // Um componente ou o pet pode não estar no objeto neste ciclo: só este cheat espera.
            // Só a falha no próprio pawn (sem caminho) derruba a leitura.
            Err(absent) if !resolved.object_path.is_empty() => {
                missing.insert(entry.id.clone(), absent);
            }
            Err(absent) => return Err(absent),
        }
    }
    Ok(PlayerSnapshot {
        controller,
        pawn,
        objects: r.objects,
        values,
        missing,
    })
}

/// Uma escrita (ou recusa) planejada para este ciclo.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldAction {
    WriteF32 {
        id: String,
        addr: u64,
        value: f32,
        guard: Vec<GuardBytes>,
    },
    ClearBits {
        id: String,
        addr: u64,
        mask: u8,
        guard: Vec<GuardBytes>,
    },
    /// Alvo não finito ou fora da faixa sã: nada é escrito.
    InvalidTarget { id: String, target: f32 },
}

/// As escritas dos cheats ligados (`on`), na ordem do catálogo, com as regras da spec 6.5.
pub fn plan_tick(
    snapshot: &PlayerSnapshot,
    catalog: &FieldCatalog,
    on: &[String],
) -> Vec<FieldAction> {
    let mut out = Vec::new();
    for entry in &catalog.entries {
        if !on.contains(&entry.id) {
            continue;
        }
        let (Ok(resolved), Some(values)) = (&entry.resolved, snapshot.values.get(&entry.id)) else {
            continue;
        };
        match (&resolved.rule, values) {
            (
                ResolvedRule::Lock { mode, min, max, .. },
                &FieldValues::Lock {
                    obj,
                    addr,
                    current,
                    target,
                },
            ) => {
                if !target.is_finite() || target < *min || target > *max {
                    out.push(FieldAction::InvalidTarget {
                        id: entry.id.clone(),
                        target,
                    });
                    continue;
                }
                let write = match mode {
                    LockMode::Raise => current < target,
                    LockMode::Lower => current > target,
                    LockMode::Exact => current != target,
                };
                if write {
                    out.push(FieldAction::WriteF32 {
                        id: entry.id.clone(),
                        addr,
                        value: target,
                        guard: snapshot.guard_of(obj),
                    });
                }
            }
            (ResolvedRule::ClearBits { .. }, FieldValues::Bits { obj, bytes }) => {
                for b in bytes.iter().filter(|b| b.value & b.mask != 0) {
                    out.push(FieldAction::ClearBits {
                        id: entry.id.clone(),
                        addr: b.addr,
                        mask: b.mask,
                        guard: snapshot.guard_of(*obj),
                    });
                }
            }
            _ => {}
        }
    }
    out
}

/// Faixa de um valor do jogo são: finito e dentro de `[-1e9, 1e9]` (spec 006, 6.4).
const SANE_LIMIT: f64 = 1.0e9;

/// `true` se os bytes lidos do jogo são um valor são para `kind` (tamanho certo; vale também
/// para `int`). No `bool` o byte é `0` ou `1` (spec 007, 6.4).
pub fn value_is_sane(kind: FieldKind, bytes: &[u8]) -> bool {
    let v = match kind {
        FieldKind::Float => match <[u8; 4]>::try_from(bytes) {
            Ok(raw) => f64::from(f32::from_le_bytes(raw)),
            Err(_) => return false,
        },
        FieldKind::Int => match <[u8; 4]>::try_from(bytes) {
            Ok(raw) => f64::from(i32::from_le_bytes(raw)),
            Err(_) => return false,
        },
        FieldKind::Bool => return matches!(bytes, [0] | [1]),
    };
    v.is_finite() && (-SANE_LIMIT..=SANE_LIMIT).contains(&v)
}

/// Os bytes (little-endian) de `v` como `kind`: 4 para `float`/`int` e 1 para `bool`. O `int` é
/// arredondado ao inteiro mais próximo; o `bool` é `0x01` para `v != 0` e `0x00` senão.
pub fn encode_value(kind: FieldKind, v: f64) -> Vec<u8> {
    match kind {
        // `as` satura nos limites do tipo e vira 0 para `NaN`, o que serve ao valor desejado.
        FieldKind::Int => (v.round() as i32).to_le_bytes().to_vec(),
        FieldKind::Float => (v as f32).to_le_bytes().to_vec(),
        FieldKind::Bool => vec![u8::from(v != 0.0)],
    }
}

/// Os bytes desejados de cada campo de um `set`: no `bool` cada campo tem o seu valor (`bools`);
/// nos outros todos recebem `value`. `None` se falta o valor (o ciclo pula o cheat).
pub fn desired_bytes(rule: &ResolvedRule, value: Option<f64>) -> Option<Vec<Vec<u8>>> {
    let ResolvedRule::Set {
        fields,
        kind,
        bools,
        ..
    } = rule
    else {
        return None;
    };
    if let Some(bools) = bools {
        return Some(bools.iter().map(|&b| vec![u8::from(b)]).collect());
    }
    let one = encode_value(*kind, value?);
    Some(vec![one; fields.len()])
}

/// O que o `set` precisa saber de um campo neste ciclo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetStep {
    pub addr: u64,
    /// O valor atual do jogo é são (pode virar original).
    pub sane: bool,
    /// Os bytes atuais são exatamente os desejados (comparação por bytes: `-0.0` não é `0.0`).
    pub same: bool,
}

/// Um passo por campo do `set`, sem decidir nada: quem executa usa os endereços e a sanidade.
/// `desired` traz os bytes de cada campo, na ordem.
pub fn plan_set(values: &FieldValues, desired: &[Vec<u8>], kind: FieldKind) -> Vec<SetStep> {
    let FieldValues::Set { fields, .. } = values else {
        return Vec::new();
    };
    fields
        .iter()
        .zip(desired)
        .map(|((addr, current), want)| SetStep {
            addr: *addr,
            sane: value_is_sane(kind, current),
            same: current == want,
        })
        .collect()
}

/// Por que a ação de somar não pode escrever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddRefusal {
    /// O valor atual está fora de `[0, result_max]`.
    OutOfRange,
    /// A soma passaria de `result_max`.
    WouldExceed,
}

/// O valor a escrever ao somar `amount` a `current` (a soma é em 64 bits; spec 006, 6.6).
pub fn plan_add(current: i32, amount: i32, result_max: i64) -> Result<i32, AddRefusal> {
    let current = i64::from(current);
    if current < 0 || current > result_max {
        return Err(AddRefusal::OutOfRange);
    }
    let sum = current + i64::from(amount);
    if sum > result_max {
        return Err(AddRefusal::WouldExceed);
    }
    // Um teto acima de `i32::MAX` não cabe no campo.
    i32::try_from(sum).map_err(|_| AddRefusal::WouldExceed)
}

/// Um bit de um cheat, no relatório.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BitReport {
    pub field: String,
    pub addr: u64,
    pub mask: u8,
    pub set: bool,
}

/// Estado de um cheat de valor no relatório.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FieldReportValue {
    Unavailable {
        reason: String,
    },
    Lock {
        addr: u64,
        current: f32,
        target: f32,
    },
    Bits {
        bits: Vec<BitReport>,
    },
    /// Campos de um `set`: endereço e valor bruto (bytes zero-estendidos para `u32`) de cada um.
    Set {
        fields: Vec<SetFieldReport>,
    },
    /// Valor atual do campo de uma ação (`add`), lido sob demanda pelo caminho do `GameState`.
    Add {
        addr: u64,
        current: i32,
    },
}

/// Um campo de um `set`, no relatório.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetFieldReport {
    /// Nome do campo na reflexão.
    pub name: String,
    pub addr: u64,
    /// Bytes atuais (little-endian, zero-estendidos para `u32`; um `bool` ocupa o byte baixo).
    pub raw: u32,
    /// Valor atual formatado pela grade do cheat (ou simples, sem grade).
    pub value: String,
}

/// Um salto do caminho do `GameState` até o objeto de um cheat, como a instância foi lida.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HopReport {
    /// Nome do campo de objeto do salto.
    pub name: String,
    pub addr: u64,
    /// Nome da classe da instância (a subclasse, quando houver), pela reflexão.
    pub class: String,
}

/// Caminho controller → `Level` → `World` → `GameState` → objeto de um cheat, até onde foi lido.
/// Um salto que falha deixa os seguintes em `None` e o motivo em `error`. Há uma entrada por
/// caminho de objeto distinto entre os cheats de raiz `game_state` (spec 007, 6.7).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct GameStateReport {
    pub level: Option<u64>,
    pub world: Option<u64>,
    pub game_state: Option<u64>,
    /// Campos de objeto do caminho, a partir do `GameState`, separados por ponto.
    pub path: String,
    /// Saltos do caminho que deram certo, na ordem.
    pub hops: Vec<HopReport>,
    /// Objeto final do caminho (o `PartyComponent`, as configurações do modo), se foi lido.
    pub object: Option<u64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldReport {
    pub id: String,
    #[serde(flatten)]
    pub value: FieldReportValue,
}

/// O que o `gt ue player` mostra.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlayerReport {
    pub controller: u64,
    /// `Pawn`, `Character` e `AcknowledgedPawn` do controller; `None` se o campo não existe.
    pub pawn: Option<u64>,
    pub character: Option<u64>,
    pub acknowledged_pawn: Option<u64>,
    pub fields: Vec<FieldReport>,
    /// Por que o personagem não foi lido (`Pawn` nulo, montaria, ...); os cheats do pawn ficam
    /// indisponíveis e o `game_state` continua valendo.
    pub pawn_error: Option<String>,
    /// Um caminho por objeto distinto entre os cheats de raiz `game_state`, na ordem do catálogo.
    pub game_state: Vec<GameStateReport>,
}

/// Lê o personagem e monta o relatório do `gt ue player`. Só um controller ausente ou inválido
/// devolve `Err`; um pawn que falha vira `pawn_error` e a parte do `GameState` continua (spec 007,
/// 6.7).
pub fn player_report<M: ReadMem + ?Sized>(
    mem: &M,
    index: &ReflectionIndex,
    catalog: &FieldCatalog,
    objects: &ObjectArray,
    controller: u64,
) -> Result<PlayerReport, Absent> {
    let snapshot = match read_player(mem, index, catalog, objects, controller) {
        Ok(snapshot) => Ok(snapshot),
        Err(
            absent @ (Absent::NoController
            | Absent::ControllerAmbiguous
            | Absent::ControllerInvalid),
        ) => return Err(absent),
        Err(absent) => Err(absent),
    };
    let class = index.class_of(mem, controller).map_err(unreadable)?;
    let [pawn, character, acknowledged_pawn] = REPORT_POINTERS.map(|name| {
        index
            .find_field(mem, class, name)
            .ok()
            .filter(|f| is_object_family(&f.kind))
            .and_then(|f| read_u64(mem, controller + u64::from(f.offset)).ok())
    });

    // Os `set` do mundo, lidos uma vez; só se houver algum resolvido.
    let has_world_set = catalog.entries.iter().any(|e| world_set(e).is_some());
    let world = has_world_set.then(|| read_world(mem, index, catalog, objects, controller));
    let mut game_state = world_paths(mem, index, catalog, controller);

    let fields = catalog
        .entries
        .iter()
        .map(|entry| {
            let value = report_entry(
                mem,
                index,
                catalog,
                entry,
                snapshot.as_ref(),
                world.as_ref(),
                &mut game_state,
            );
            FieldReport {
                id: entry.id.clone(),
                value,
            }
        })
        .collect();
    Ok(PlayerReport {
        controller,
        pawn,
        character,
        acknowledged_pawn,
        fields,
        pawn_error: snapshot.err().map(|absent| absent.to_string()),
        game_state,
    })
}

/// O cheat de raiz `game_state` resolvido de `entry`, qualquer que seja a regra.
fn game_state_resolved(entry: &CatalogEntry) -> Option<&ResolvedField> {
    match &entry.resolved {
        Ok(resolved) if resolved.root == FieldRoot::GameState => Some(resolved),
        _ => None,
    }
}

/// O `set` de raiz `game_state` resolvido de `entry` (o que `read_world` lê).
fn world_set(entry: &CatalogEntry) -> Option<&ResolvedField> {
    game_state_resolved(entry).filter(|r| matches!(r.rule, ResolvedRule::Set { .. }))
}

/// Campos de objeto do caminho de `resolved`, separados por ponto.
fn path_key(resolved: &ResolvedField) -> String {
    let names: Vec<&str> = resolved
        .object_path
        .iter()
        .map(|h| h.field.name.as_str())
        .collect();
    names.join(".")
}

/// Uma entrada por caminho de objeto distinto entre os cheats de raiz `game_state`, na ordem do
/// catálogo, cada uma com os saltos lidos até onde o caminho vai.
fn world_paths<M: ReadMem + ?Sized>(
    mem: &M,
    index: &ReflectionIndex,
    catalog: &FieldCatalog,
    controller: u64,
) -> Vec<GameStateReport> {
    let mut out: Vec<GameStateReport> = Vec::new();
    for resolved in catalog.entries.iter().filter_map(game_state_resolved) {
        let key = path_key(resolved);
        if out.iter().all(|g| g.path != key) {
            out.push(report_path(mem, index, catalog, controller, resolved, key));
        }
    }
    out
}

/// Nome da classe da instância `obj`, pela reflexão (`?` se não der para ler).
fn class_name<M: ReadMem + ?Sized>(index: &ReflectionIndex, mem: &M, obj: u64) -> String {
    index
        .class_of(mem, obj)
        .and_then(|class| index.object_name(mem, class))
        .unwrap_or_else(|_| "?".to_string())
}

/// Anda do controller (já validado) até o objeto de `resolved`, anotando cada salto que deu certo.
/// O objeto final de um `set` que é um CDO não conta (nunca se escreve nele).
fn report_path<M: ReadMem + ?Sized>(
    mem: &M,
    index: &ReflectionIndex,
    catalog: &FieldCatalog,
    controller: u64,
    resolved: &ResolvedField,
    path: String,
) -> GameStateReport {
    let mut report = GameStateReport {
        path,
        ..GameStateReport::default()
    };
    let Ok(gs) = &catalog.game_state else {
        report.error = Some(GameStateAbsent::Missing("GameState".to_string()).to_string());
        return report;
    };
    let mut r = Reader {
        mem,
        index,
        catalog,
        props: HashMap::new(),
        objects: Vec::new(),
    };
    let mut obj = match walk_to_game_state(&mut r, controller, gs, &mut report) {
        Ok(state) => state,
        Err(absent) => {
            report.error = Some(absent.to_string());
            return report;
        }
    };
    let check_cdo = !matches!(resolved.rule, ResolvedRule::Add { .. });
    let last = resolved.object_path.len().saturating_sub(1);
    for (i, hop) in resolved.object_path.iter().enumerate() {
        let name = hop.field.name.as_str();
        let next = r.pointer(obj, &hop.field).and_then(|ptr| {
            if ptr != 0 && catalog.object_is_a(index, mem, ptr, hop.class)? {
                Ok(ptr)
            } else {
                Err(Absent::ComponentWrongClass(name.to_string()))
            }
        });
        let cdo = |ptr: u64| i == last && check_cdo && r.is_cdo(ptr).unwrap_or(false);
        match next {
            Ok(ptr) if !cdo(ptr) => {
                report.hops.push(HopReport {
                    name: name.to_string(),
                    addr: ptr,
                    class: class_name(index, mem, ptr),
                });
                obj = ptr;
            }
            Ok(_) => {
                report.error = Some(GameStateAbsent::Missing(name.to_string()).to_string());
                return report;
            }
            Err(absent) => {
                report.error = Some(gone(name)(absent).to_string());
                return report;
            }
        }
    }
    report.object = Some(obj);
    report
}

/// O estado de um cheat no relatório: pawn pelo snapshot (ou o erro dele), `game_state` pelo
/// caminho e por `read_world`.
fn report_entry<M: ReadMem + ?Sized>(
    mem: &M,
    index: &ReflectionIndex,
    catalog: &FieldCatalog,
    entry: &CatalogEntry,
    snapshot: Result<&PlayerSnapshot, &Absent>,
    world: Option<&Result<WorldSnapshot, GameStateAbsent>>,
    paths: &mut [GameStateReport],
) -> FieldReportValue {
    let resolved = match &entry.resolved {
        Ok(r) => r,
        Err(reason) => {
            return FieldReportValue::Unavailable {
                reason: reason.clone(),
            };
        }
    };
    let unavailable = |reason: String| FieldReportValue::Unavailable { reason };
    if resolved.root != FieldRoot::GameState {
        return match snapshot {
            Ok(snap) => report_value(
                entry,
                &snap.values,
                snap.missing.get(&entry.id).map(ToString::to_string),
            ),
            Err(absent) => unavailable(absent.to_string()),
        };
    }
    let key = path_key(resolved);
    let Some(path) = paths.iter_mut().find(|g| g.path == key) else {
        return unavailable(INCOHERENT.to_string());
    };
    // O motivo do caminho vale para o cheat: os dois mostram o mesmo salto.
    if let Some(error) = &path.error {
        return unavailable(error.clone());
    }
    match (&resolved.rule, path.object, world) {
        (ResolvedRule::Add { field, .. }, Some(obj), _) => {
            report_add(mem, index, catalog, obj, field, path)
        }
        (ResolvedRule::Set { .. }, _, Some(Ok(snap))) => report_value(
            entry,
            &snap.values,
            snap.missing.get(&entry.id).map(ToString::to_string),
        ),
        (ResolvedRule::Set { .. }, _, Some(Err(absent))) => unavailable(absent.to_string()),
        _ => unavailable(INCOHERENT.to_string()),
    }
}

/// Valor atual do campo de uma ação (`add`) no objeto `obj`; a falha vira o `error` do caminho.
fn report_add<M: ReadMem + ?Sized>(
    mem: &M,
    index: &ReflectionIndex,
    catalog: &FieldCatalog,
    obj: u64,
    field: &Field,
    path: &mut GameStateReport,
) -> FieldReportValue {
    let mut r = Reader {
        mem,
        index,
        catalog,
        props: HashMap::new(),
        objects: Vec::new(),
    };
    let addr = obj + u64::from(field.offset);
    let read = r
        .check_fits(obj, &field.name, field.offset, field.size)
        .map_err(|absent| gone(&field.name)(absent).to_string())
        .and_then(|()| read_u32(mem, addr).map_err(|_| format!("{} ilegível", field.name)));
    match read {
        Ok(raw) => FieldReportValue::Add {
            addr,
            current: i32::from_le_bytes(raw.to_le_bytes()),
        },
        Err(reason) => {
            path.error = Some(reason.clone());
            FieldReportValue::Unavailable { reason }
        }
    }
}

/// Estado de um cheat lido por `values` (ou o motivo em `missing`).
fn report_value(
    entry: &CatalogEntry,
    values: &HashMap<String, FieldValues>,
    missing: Option<String>,
) -> FieldReportValue {
    if let Some(reason) = missing {
        return FieldReportValue::Unavailable { reason };
    }
    let resolved = match &entry.resolved {
        Ok(r) => r,
        Err(reason) => {
            return FieldReportValue::Unavailable {
                reason: reason.clone(),
            };
        }
    };
    match (&resolved.rule, values.get(&entry.id)) {
        (
            _,
            Some(&FieldValues::Lock {
                addr,
                current,
                target,
                ..
            }),
        ) => FieldReportValue::Lock {
            addr,
            current,
            target,
        },
        (ResolvedRule::ClearBits { fields }, Some(FieldValues::Bits { obj, bytes })) => {
            let bits = fields
                .iter()
                .filter_map(|f| {
                    let bits = f.bits?;
                    let addr = obj + u64::from(bits.byte);
                    let byte = bytes.iter().find(|b| b.addr == addr)?;
                    Some(BitReport {
                        field: f.name.clone(),
                        addr,
                        mask: bits.mask,
                        set: byte.value & bits.mask != 0,
                    })
                })
                .collect();
            FieldReportValue::Bits { bits }
        }
        (
            ResolvedRule::Set {
                fields: defs, kind, ..
            },
            Some(FieldValues::Set { fields, .. }),
        ) => FieldReportValue::Set {
            fields: fields
                .iter()
                .zip(defs)
                .map(|((addr, bytes), def)| {
                    let mut raw = [0u8; 4];
                    let n = bytes.len().min(4);
                    raw[..n].copy_from_slice(&bytes[..n]);
                    SetFieldReport {
                        name: def.name.clone(),
                        addr: *addr,
                        raw: u32::from_le_bytes(raw),
                        value: format_value(entry.grid.as_ref(), *kind, decode_value(*kind, bytes)),
                    }
                })
                .collect(),
        },
        _ => FieldReportValue::Unavailable {
            reason: INCOHERENT.to_string(),
        },
    }
}

/// O valor `v` com as casas e a unidade da grade (ou sem grade, simples).
pub(crate) fn format_value(grid: Option<&ValueGrid>, kind: FieldKind, v: f64) -> String {
    match grid {
        // Uma grade de um ponto só, em `v`, reaproveita a formatação sem prender `v` à faixa.
        Some(g) => ValueGrid {
            min: v,
            max: v,
            step: 1.0,
            default_k: 0,
            points: None,
            ..g.clone()
        }
        .format(0),
        None => match kind {
            FieldKind::Int => format!("{v:.0}"),
            FieldKind::Float => format!("{v}"),
            FieldKind::Bool => (v != 0.0).to_string(),
        },
    }
}

/// Os bytes de um campo como número (para mostrar); `bool` é `0.0` ou `1.0`.
pub(crate) fn decode_value(kind: FieldKind, bytes: &[u8]) -> f64 {
    match kind {
        FieldKind::Bool => f64::from(u8::from(bytes.first().is_some_and(|&b| b != 0))),
        FieldKind::Int => f64::from(i32::from_le_bytes(bytes.try_into().unwrap_or_default())),
        FieldKind::Float => f64::from(f32::from_le_bytes(bytes.try_into().unwrap_or_default())),
    }
}

/// Opções do [`watch_player`].
#[derive(Debug, Clone, Copy)]
pub struct WatchOpts {
    /// Controller dado pelo usuário; sem ele, a primeira rodada varre a tabela.
    pub controller: Option<u64>,
    /// Intervalo entre as rodadas.
    pub every: Duration,
    /// Quantidade de rodadas; `None` repete até o processo sumir.
    pub rounds: Option<u32>,
    pub budget: ScanBudget,
}

/// Procura o controller e o converte no resultado de uma rodada.
fn search<M: ReadMem + ?Sized>(
    index: &ReflectionIndex,
    mem: &M,
    objects: &ObjectArray,
    catalog: &FieldCatalog,
    budget: &ScanBudget,
) -> Result<Result<u64, Absent>, UeError> {
    Ok(
        match find_controller(index, mem, objects, catalog, budget)? {
            ControllerFind::Found(c) => Ok(c),
            ControllerFind::Absent => Err(Absent::NoController),
            ControllerFind::Ambiguous => Err(Absent::ControllerAmbiguous),
        },
    )
}

/// Lê o personagem em rodadas, para o `gt ue player --watch` (spec 6.8). A varredura do
/// controller só acontece na primeira rodada sem `opts.controller`, e de novo quando o controller
/// guardado deixa de ser válido. `out` recebe o relatório de cada rodada, ou por que o personagem
/// está ausente. Erro de leitura das globais (processo encerrado) interrompe.
pub fn watch_player<M: ReadMem + ?Sized>(
    mem: &M,
    index: &ReflectionIndex,
    catalog: &FieldCatalog,
    gobjects: u64,
    opts: &WatchOpts,
    sleep: &mut dyn FnMut(Duration),
    out: &mut dyn FnMut(Result<&PlayerReport, &Absent>),
) -> Result<(), UeError> {
    let mut controller = opts.controller;
    let mut round = 0u32;
    while opts.rounds.is_none_or(|n| round < n) {
        if round > 0 {
            sleep(opts.every);
        }
        round += 1;
        // O cabeçalho da tabela é relido a cada rodada: ela cresce com o jogo rodando.
        let objects = ObjectArray::read(mem, &catalog.layout, gobjects)?;
        let mut rescanned = false;
        let result = loop {
            let current = match controller {
                Some(c) => c,
                None => {
                    rescanned = true;
                    match search(index, mem, &objects, catalog, &opts.budget)? {
                        Ok(c) => {
                            controller = Some(c);
                            c
                        }
                        Err(absent) => break Err(absent),
                    }
                }
            };
            match player_report(mem, index, catalog, &objects, current) {
                // Controller guardado que deixou de valer: uma nova varredura, na mesma rodada.
                Err(Absent::ControllerInvalid) if !rescanned => controller = None,
                other => break other,
            }
        };
        out(result.as_ref());
    }
    Ok(())
}

/// `true` se o controller tem de ser procurado de novo: o guardado não é válido, ou o `Pawn` dele
/// está nulo há mais de 30 s (um controller antigo depois de carregar outro save).
pub fn controller_needs_search(
    valid: bool,
    pawn_null_since: Option<Instant>,
    now: Instant,
) -> bool {
    !valid || pawn_null_since.is_some_and(|since| now.duration_since(since) > PAWN_NULL_RESEARCH)
}

#[cfg(test)]
mod tests;
