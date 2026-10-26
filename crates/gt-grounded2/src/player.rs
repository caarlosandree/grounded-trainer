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
use gt_ue::read::{read_u8, read_u32, read_u64, read_vec};
use gt_ue::{Field, ObjectArray, ReflectionIndex, ScanBudget, UeError, UeLayout, is_object_family};
use serde::Serialize;

use crate::signatures::{FieldDef, FieldRule, LockMode, LockTarget, PlayerCfg};

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
}

/// Cheat de valor resolvido pela reflexão.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedField {
    /// Campo do pawn que aponta para o objeto; `None` quando o objeto é o próprio pawn.
    pub object_field: Option<Field>,
    /// Classe esperada do objeto, conferida com `is_a` a cada leitura.
    pub object_class: u64,
    pub rule: ResolvedRule,
}

/// Classes e campos do caminho até o pawn.
#[derive(Debug, Clone)]
struct PlayerPath {
    controller_class: u64,
    pawn_field: Field,
    pawn_class: u64,
    alt_pawn_fields: Vec<Field>,
}

#[derive(Debug, Clone)]
struct CatalogEntry {
    id: String,
    resolved: Result<ResolvedField, String>,
}

/// Cheats de valor de uma conexão, resolvidos uma vez pelo índice da reflexão.
pub struct FieldCatalog {
    layout: UeLayout,
    path: Result<PlayerPath, String>,
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
        let entries = defs
            .iter()
            .map(|def| {
                let resolved = match &path {
                    Ok(p) => resolve_field(index, mem, p.pawn_class, def),
                    Err(reason) => Err(reason.clone()),
                };
                if let Err(reason) = &resolved {
                    tracing::debug!(id = %def.id, "cheat de valor indisponível: {reason}");
                }
                CatalogEntry {
                    id: def.id.clone(),
                    resolved,
                }
            })
            .collect();
        Self {
            layout: *index.layout(),
            path,
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

/// Campo `float` de 4 bytes.
fn float_field<M: ReadMem + ?Sized>(
    index: &ReflectionIndex,
    mem: &M,
    class: u64,
    name: &str,
) -> Result<Field, String> {
    let field = find_field(index, mem, class, name)?;
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
    let pawn_class = find_class(index, &cfg.pawn_class)?;
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
        pawn_class,
        alt_pawn_fields,
    })
}

fn resolve_field<M: ReadMem + ?Sized>(
    index: &ReflectionIndex,
    mem: &M,
    pawn_class: u64,
    def: &FieldDef,
) -> Result<ResolvedField, String> {
    let object_class = find_class(index, &def.object_class)?;
    let object_field = if def.object == PAWN_OBJECT {
        None
    } else {
        Some(object_field(index, mem, pawn_class, &def.object)?)
    };
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
    };
    Ok(ResolvedField {
        object_field,
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
    let found = index.instances_of(mem, objects, path.controller_class, budget)?;
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
}

/// O personagem lido num ciclo.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerSnapshot {
    pub controller: u64,
    pub pawn: u64,
    pub objects: Vec<ObjSnap>,
    /// Por `id`, só dos cheats resolvidos.
    pub values: HashMap<String, FieldValues>,
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

    /// Registra o objeto no caminho (com a guarda), uma vez.
    fn visit(&mut self, obj: u64) -> Result<(), Absent> {
        if self.objects.iter().all(|o| o.addr != obj) {
            let guard = read_guard(self.mem, &self.catalog.layout, obj).map_err(unreadable)?;
            self.objects.push(ObjSnap { addr: obj, guard });
        }
        Ok(())
    }

    /// Objeto de um cheat: o pawn ou o componente apontado pelo campo do pawn.
    fn object(&mut self, pawn: u64, r: &ResolvedField) -> Result<u64, Absent> {
        let (obj, what) = match &r.object_field {
            None => (pawn, PAWN_OBJECT),
            Some(f) => (self.pointer(pawn, f)?, f.name.as_str()),
        };
        let ok = obj != 0
            && self
                .catalog
                .object_is_a(self.index, self.mem, obj, r.object_class)?;
        if !ok {
            return Err(Absent::ComponentWrongClass(what.to_string()));
        }
        self.visit(obj)?;
        Ok(obj)
    }

    fn values(&mut self, obj: u64, rule: &ResolvedRule) -> Result<FieldValues, Absent> {
        Ok(match rule {
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
        })
    }
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
    let layout = &catalog.layout;

    // 1. O item da tabela no `InternalIndex` ainda aponta para o controller, da mesma classe.
    let internal = read_u32(mem, controller + u64::from(layout.uobject.index))
        .map_err(|_| Absent::ControllerInvalid)?;
    match objects.object_at(mem, layout, internal) {
        Ok(Some(obj)) if obj == controller => {}
        _ => return Err(Absent::ControllerInvalid),
    }
    match catalog.object_is_a(index, mem, controller, path.controller_class) {
        Ok(true) => {}
        _ => return Err(Absent::ControllerInvalid),
    }

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
    let pawn = if catalog.object_is_a(index, mem, pawn, path.pawn_class)? {
        pawn
    } else {
        let mut alt = None;
        for f in &path.alt_pawn_fields {
            let p = r.pointer(controller, f)?;
            if p != 0 && catalog.object_is_a(index, mem, p, path.pawn_class)? {
                alt = Some(p);
                break;
            }
        }
        alt.ok_or(Absent::PawnWrongClass)?
    };
    r.visit(pawn)?;

    // 4. Cada componente, conferido com a classe esperada.
    let mut values = HashMap::new();
    for entry in &catalog.entries {
        let Ok(resolved) = &entry.resolved else {
            continue;
        };
        let obj = r.object(pawn, resolved)?;
        values.insert(entry.id.clone(), r.values(obj, &resolved.rule)?);
    }
    Ok(PlayerSnapshot {
        controller,
        pawn,
        objects: r.objects,
        values,
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
}

/// Lê o personagem e monta o relatório do `gt ue player`.
pub fn player_report<M: ReadMem + ?Sized>(
    mem: &M,
    index: &ReflectionIndex,
    catalog: &FieldCatalog,
    objects: &ObjectArray,
    controller: u64,
) -> Result<PlayerReport, Absent> {
    let snapshot = read_player(mem, index, catalog, objects, controller)?;
    let class = index.class_of(mem, controller).map_err(unreadable)?;
    let [pawn, character, acknowledged_pawn] = REPORT_POINTERS.map(|name| {
        index
            .find_field(mem, class, name)
            .ok()
            .filter(|f| is_object_family(&f.kind))
            .and_then(|f| read_u64(mem, controller + u64::from(f.offset)).ok())
    });
    let fields = catalog
        .entries
        .iter()
        .map(|entry| FieldReport {
            id: entry.id.clone(),
            value: report_value(&snapshot, entry),
        })
        .collect();
    Ok(PlayerReport {
        controller,
        pawn,
        character,
        acknowledged_pawn,
        fields,
    })
}

fn report_value(snapshot: &PlayerSnapshot, entry: &CatalogEntry) -> FieldReportValue {
    let resolved = match &entry.resolved {
        Ok(r) => r,
        Err(reason) => {
            return FieldReportValue::Unavailable {
                reason: reason.clone(),
            };
        }
    };
    match (&resolved.rule, snapshot.values.get(&entry.id)) {
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
        _ => FieldReportValue::Unavailable {
            reason: INCOHERENT.to_string(),
        },
    }
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
