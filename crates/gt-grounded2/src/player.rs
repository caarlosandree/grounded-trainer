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

use crate::signatures::{FieldDef, FieldRoot, FieldRule, LockMode, LockTarget, PlayerCfg};

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
    /// Nome curto do cheat (para os eventos).
    name: String,
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
                    name: def.name.clone(),
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

    /// Nome curto do cheat `id`.
    pub fn name(&self, id: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.name.as_str())
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

/// Motivo de indisponível das regras e raízes ainda sem resolução (Tarefa 5 do plano 006).
const UNSUPPORTED_RULE: &str = "regra ainda não suportada";

fn resolve_field<M: ReadMem + ?Sized>(
    index: &ReflectionIndex,
    mem: &M,
    pawn_class: u64,
    def: &FieldDef,
) -> Result<ResolvedField, String> {
    // TODO(Tarefa 5): raiz `game_state` e regras `set`/`add`; por ora esses cheats ficam indisponíveis.
    if def.root != FieldRoot::Pawn {
        return Err(UNSUPPORTED_RULE.to_string());
    }
    let object_class = find_class(index, &def.object_class)?;
    let mut object_path = Vec::new();
    if def.object != PAWN_OBJECT {
        let hops: Vec<&str> = def.object.split('.').collect();
        let mut class = pawn_class;
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
        FieldRule::Set { .. } | FieldRule::Add { .. } => {
            return Err(UNSUPPORTED_RULE.to_string());
        }
    };
    Ok(ResolvedField {
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
}

/// O personagem lido num ciclo.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerSnapshot {
    pub controller: u64,
    pub pawn: u64,
    pub objects: Vec<ObjSnap>,
    /// Por `id`, só dos cheats resolvidos.
    pub values: HashMap<String, FieldValues>,
    /// Cheats de outro ator (o pet) cujo objeto não está disponível neste ciclo, com o motivo.
    /// O personagem continua valendo para os outros.
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
    let mut missing = HashMap::new();
    for entry in &catalog.entries {
        let Ok(resolved) = &entry.resolved else {
            continue;
        };
        let read = r
            .object(pawn, resolved)
            .and_then(|obj| r.values(obj, &resolved.rule));
        match read {
            Ok(v) => {
                values.insert(entry.id.clone(), v);
            }
            // O pet (outro ator) pode não estar no mundo: só este cheat espera.
            Err(absent) if resolved.is_secondary() => {
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
    if let Some(absent) = snapshot.missing.get(&entry.id) {
        return FieldReportValue::Unavailable {
            reason: absent.to_string(),
        };
    }
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
