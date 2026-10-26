//! Índice de classes e campos da reflexão (spec 005, 6.2): âncora `Class`, busca de classes por
//! `(pacote, nome)`, campos por nome subindo pela `SuperStruct`, `is_a` e instâncias.
//!
//! Nenhum offset do jogo vive aqui: tudo vem do [`UeLayout`] e dos nomes da própria reflexão.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::time::Instant;

use gt_core::mem::ReadMem;

use crate::error::UeError;
use crate::layout::UeLayout;
use crate::names::{NameLookup, Names};
use crate::objects::{ObjectArray, ScanBudget, ScanEnd};
use crate::read::{is_user_addr, read_bytes, read_u8, read_u32, read_u64, u32_at};

/// Teto da cadeia de `SuperStruct`.
const MAX_SUPER_CHAIN: usize = 64;
/// Teto de campos por struct.
const MAX_FIELDS: usize = 4096;

/// Pacote e nome da âncora (a própria `Class`).
const ANCHOR: (&str, &str) = ("/Script/CoreUObject", "Class");
/// Classes que a conferência profunda exige.
const REQUIRED_CLASSES: [(&str, &str); 3] = [
    ("/Script/CoreUObject", "Object"),
    ("/Script/CoreUObject", "Class"),
    ("/Script/Engine", "Actor"),
];
/// Campo de `Actor` que a conferência profunda exige, de um tipo da família de objeto.
const ACTOR_ROOT_FIELD: &str = "RootComponent";

/// Byte e máscara de um campo `bool` (campo de bits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoolBits {
    /// `Offset_Internal + ByteOffset`: o byte, a partir do início do objeto.
    pub byte: u32,
    /// `FieldMask`.
    pub mask: u8,
}

/// Um campo de reflexão.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// Nome da classe (ou struct) onde o campo está declarado.
    pub owner: String,
    pub name: String,
    /// Nome da `FFieldClass` (`FloatProperty`, `BoolProperty`, `ObjectProperty`, ...).
    pub kind: String,
    /// `Offset_Internal`.
    pub offset: u32,
    /// `ElementSize`.
    pub size: u32,
    /// Só para `BoolProperty`.
    pub bits: Option<BoolBits>,
}

/// `true` para os tipos que guardam um ponteiro para objeto: `ObjectProperty` e, no UE5 com
/// `TObjectPtr`, `ObjectPtrProperty`.
pub fn is_object_family(kind: &str) -> bool {
    matches!(kind, "ObjectProperty" | "ObjectPtrProperty")
}

/// Um objeto de classe guardado pela passada da tabela.
struct Entry {
    addr: u64,
    name: (u32, u32),
    outer: u64,
}

/// Índice das classes (objetos cuja classe é a `Class` ou filha dela) de uma conexão.
pub struct ReflectionIndex {
    layout: UeLayout,
    names: NameLookup,
    anchor: u64,
    /// `(pacote, nome) → classes`. Mais de uma é ambíguo.
    classes: HashMap<(String, String), Vec<u64>>,
    /// Resultado da conferência profunda, feito na construção.
    deep: Result<(), String>,
}

impl ReflectionIndex {
    /// Uma passada pela tabela de objetos: acha a âncora `Class`, guarda todo objeto cuja classe
    /// é `Class` ou filha dela e confere a parte profunda do layout (`deep_check`).
    ///
    /// Estouro do teto de tempo da varredura vira [`UeError::Deadline`].
    pub fn build<M: ReadMem + ?Sized>(
        mem: &M,
        layout: &UeLayout,
        objects: &ObjectArray,
        names: &Names<'_, M>,
        budget: &ScanBudget,
    ) -> Result<Self, UeError> {
        let started = Instant::now();
        let cdo = layout.uobject.cdo_flag;

        // Objetos agrupados pela classe: só depois da passada se sabe quais classes são filhas da
        // `Class`, porque a âncora pode vir depois dos objetos que dependem dela.
        let mut by_class: HashMap<u64, Vec<Entry>> = HashMap::new();
        let mut anchors = Vec::new();
        let end = objects.scan(mem, layout, budget, |h| {
            if h.class == h.addr {
                anchors.push((h.addr, h.name, h.outer));
            }
            if h.flags & cdo != 0 || !is_user_addr(h.class) {
                return;
            }
            by_class.entry(h.class).or_default().push(Entry {
                addr: h.addr,
                name: h.name,
                outer: h.outer,
            });
        })?;
        if end == ScanEnd::DeadlineExceeded {
            return Err(UeError::Deadline);
        }

        let lookup = names.lookup();
        let anchor = find_anchor(mem, layout, lookup, &anchors)?;
        let mut index = Self {
            layout: *layout,
            names: lookup.snapshot(),
            anchor,
            classes: HashMap::new(),
            deep: Ok(()),
        };

        // Classes das quais os objetos são instâncias e que descendem da `Class`.
        let mut family: Vec<Entry> = Vec::new();
        for (class, entries) in by_class {
            if started.elapsed() >= budget.deadline {
                return Err(UeError::Deadline);
            }
            match index.is_a(mem, class, anchor) {
                Ok(true) => family.extend(entries),
                Ok(false) => {}
                // Classe ilegível ou cadeia corrompida: os objetos dela não entram.
                Err(e) => tracing::debug!(class, "classe descartada no índice: {e}"),
            }
        }

        let outers = index.read_outer_names(mem, &family, budget, started)?;
        for (i, e) in family.iter().enumerate() {
            if i % 1024 == 0 && started.elapsed() >= budget.deadline {
                return Err(UeError::Deadline);
            }
            let Some(pkg) = outers.get(&e.outer) else {
                continue;
            };
            match names.fname(e.name.0, e.name.1) {
                Ok(name) => index
                    .classes
                    .entry((pkg.clone(), name))
                    .or_default()
                    .push(e.addr),
                Err(err) => tracing::debug!(addr = e.addr, "classe sem nome legível: {err}"),
            }
        }
        for list in index.classes.values_mut() {
            list.sort_unstable();
        }
        index.deep = index.compute_deep_check(mem);
        tracing::debug!(
            classes = index.classes.len(),
            anchor = format_args!("{anchor:#x}"),
            "índice de classes montado"
        );
        Ok(index)
    }

    /// Endereço da âncora (`/Script/CoreUObject.Class`).
    pub fn anchor(&self) -> u64 {
        self.anchor
    }

    /// Quantidade de nomes de classe `(pacote, nome)` no índice.
    pub fn class_count(&self) -> usize {
        self.classes.len()
    }

    /// Conferência profunda do layout (spec 6.2): `Object`, `Class` e `Actor` achados, e
    /// `Actor.RootComponent` de um tipo da família de objeto. O `Err` é o detalhe para o log; quem
    /// mostra ao usuário usa `reflexão incoerente`.
    pub fn deep_check(&self) -> Result<(), String> {
        self.deep.clone()
    }

    fn compute_deep_check<M: ReadMem + ?Sized>(&self, mem: &M) -> Result<(), String> {
        let mut actor = 0;
        for (pkg, name) in REQUIRED_CLASSES {
            let class = self
                .find_class(pkg, name)
                .map_err(|e| format!("{pkg}.{name}: {e}"))?;
            if name == "Actor" {
                actor = class;
            }
        }
        let root = self
            .find_field(mem, actor, ACTOR_ROOT_FIELD)
            .map_err(|e| format!("Actor.{ACTOR_ROOT_FIELD}: {e}"))?;
        if !is_object_family(&root.kind) {
            return Err(format!(
                "Actor.{ACTOR_ROOT_FIELD} é {}, esperado um tipo de objeto",
                root.kind
            ));
        }
        Ok(())
    }

    /// A classe `name` do pacote `pkg`. Nenhuma: `NotFound`; mais de uma: `Ambiguous`.
    pub fn find_class(&self, pkg: &str, name: &str) -> Result<u64, UeError> {
        let key = (pkg.to_string(), name.to_string());
        match self.classes.get(&key).map(Vec::as_slice) {
            None | Some([]) => Err(UeError::NotFound(format!("classe {pkg}.{name}"))),
            Some([only]) => Ok(*only),
            Some(many) => Err(UeError::Ambiguous(format!(
                "classe {pkg}.{name} ({} objetos)",
                many.len()
            ))),
        }
    }

    /// Todas as classes chamadas `name`, com o pacote de cada uma (ordenadas por pacote).
    pub fn classes_named(&self, name: &str) -> Vec<(String, u64)> {
        let mut out: Vec<(String, u64)> = self
            .classes
            .iter()
            .filter(|((_, n), _)| n == name)
            .flat_map(|((pkg, _), list)| list.iter().map(move |&c| (pkg.clone(), c)))
            .collect();
        out.sort();
        out
    }

    /// `ClassPrivate` do objeto.
    pub fn class_of<M: ReadMem + ?Sized>(&self, mem: &M, obj: u64) -> Result<u64, UeError> {
        let class = read_u64(mem, obj + u64::from(self.layout.uobject.class))?;
        if !is_user_addr(class) {
            return Err(UeError::Malformed(format!(
                "ClassPrivate {class:#x} do objeto {obj:#x} fora do espaço de usuário"
            )));
        }
        Ok(class)
    }

    /// `PropertiesSize` da classe.
    pub fn props_size<M: ReadMem + ?Sized>(&self, mem: &M, class: u64) -> Result<u32, UeError> {
        read_u32(mem, class + u64::from(self.layout.ustruct.properties_size))
    }

    /// `true` se `class` é `ancestor` ou descende dele pela `SuperStruct`.
    pub fn is_a<M: ReadMem + ?Sized>(
        &self,
        mem: &M,
        class: u64,
        ancestor: u64,
    ) -> Result<bool, UeError> {
        let mut found = false;
        self.walk_supers(mem, class, |c| {
            found = c == ancestor;
            Ok(if found {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            })
        })?;
        Ok(found)
    }

    /// Instâncias (sem CDO) de `class` ou de suas filhas, numa varredura da tabela.
    pub fn instances_of<M: ReadMem + ?Sized>(
        &self,
        mem: &M,
        objects: &ObjectArray,
        class: u64,
        budget: &ScanBudget,
    ) -> Result<Vec<u64>, UeError> {
        let cdo = self.layout.uobject.cdo_flag;
        let mut verdicts: HashMap<u64, bool> = HashMap::new();
        let mut out = Vec::new();
        let end = objects.scan(mem, &self.layout, budget, |h| {
            if h.flags & cdo != 0 || !is_user_addr(h.class) {
                return;
            }
            let is_child = *verdicts
                .entry(h.class)
                .or_insert_with(|| self.is_a(mem, h.class, class).unwrap_or(false));
            if is_child {
                out.push(h.addr);
            }
        })?;
        if end == ScanEnd::DeadlineExceeded {
            return Err(UeError::Deadline);
        }
        Ok(out)
    }

    /// O campo `name` de `class`, subindo pela `SuperStruct` (o mais derivado primeiro).
    pub fn find_field<M: ReadMem + ?Sized>(
        &self,
        mem: &M,
        class: u64,
        name: &str,
    ) -> Result<Field, UeError> {
        let mut found = None;
        self.walk_supers(mem, class, |strukt| {
            self.walk_fields(mem, strukt, |field, field_name| {
                if field_name == name {
                    found = Some((strukt, field));
                    Ok(ControlFlow::Break(()))
                } else {
                    Ok(ControlFlow::Continue(()))
                }
            })?;
            Ok(if found.is_some() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            })
        })?;
        match found {
            Some((strukt, field)) => self.read_field(mem, strukt, field, name),
            None => Err(UeError::NotFound(format!(
                "campo {name} em {} ou nas superclasses",
                self.object_name(mem, class)
                    .unwrap_or_else(|_| format!("{class:#x}"))
            ))),
        }
    }

    /// Todos os campos de `class` e das superclasses (a classe primeiro), na ordem da reflexão.
    pub fn fields_of<M: ReadMem + ?Sized>(
        &self,
        mem: &M,
        class: u64,
    ) -> Result<Vec<Field>, UeError> {
        let mut out = Vec::new();
        self.walk_supers(mem, class, |strukt| {
            let mut found = Vec::new();
            self.walk_fields(mem, strukt, |field, name| {
                found.push((field, name.to_string()));
                Ok(ControlFlow::Continue(()))
            })?;
            for (field, name) in found {
                out.push(self.read_field(mem, strukt, field, &name)?);
            }
            Ok(ControlFlow::Continue(()))
        })?;
        Ok(out)
    }

    /// Nome do objeto (`NamePrivate`).
    pub fn object_name<M: ReadMem + ?Sized>(&self, mem: &M, obj: u64) -> Result<String, UeError> {
        self.fname_at(mem, obj + u64::from(self.layout.uobject.name))
    }

    /// Lê um `FName` (comparison, number) em `addr` e o converte em texto.
    fn fname_at<M: ReadMem + ?Sized>(&self, mem: &M, addr: u64) -> Result<String, UeError> {
        let mut raw = [0u8; 8];
        read_bytes(mem, addr, &mut raw)?;
        self.names.fname(mem, u32_at(&raw, 0), u32_at(&raw, 4))
    }

    /// Lê um ponteiro de ligação (`SuperStruct`, `ChildProperties`, `Next`): nulo é o fim.
    fn read_link<M: ReadMem + ?Sized>(&self, mem: &M, addr: u64) -> Result<Option<u64>, UeError> {
        let ptr = read_u64(mem, addr)?;
        if ptr == 0 {
            Ok(None)
        } else if is_user_addr(ptr) {
            Ok(Some(ptr))
        } else {
            Err(UeError::Malformed(format!(
                "ponteiro {ptr:#x} em {addr:#x} fora do espaço de usuário"
            )))
        }
    }

    /// Visita `class` e as superclasses, em ordem. Cadeia acima de 64 ou com ciclo é `Malformed`.
    fn walk_supers<M: ReadMem + ?Sized>(
        &self,
        mem: &M,
        class: u64,
        mut visit: impl FnMut(u64) -> Result<ControlFlow<()>, UeError>,
    ) -> Result<(), UeError> {
        let mut seen: Vec<u64> = Vec::new();
        let mut cur = class;
        loop {
            if seen.contains(&cur) {
                return Err(UeError::Malformed(format!(
                    "ciclo na cadeia de SuperStruct em {cur:#x}"
                )));
            }
            if seen.len() >= MAX_SUPER_CHAIN {
                return Err(UeError::Malformed(format!(
                    "cadeia de SuperStruct acima de {MAX_SUPER_CHAIN}"
                )));
            }
            seen.push(cur);
            if visit(cur)?.is_break() {
                return Ok(());
            }
            let link = cur + u64::from(self.layout.ustruct.super_struct);
            match self.read_link(mem, link)? {
                Some(next) => cur = next,
                None => return Ok(()),
            }
        }
    }

    /// Visita os campos de uma struct (`ChildProperties` e `Next`), com o endereço e o nome.
    /// Mais de 4096 campos é `Malformed`.
    fn walk_fields<M: ReadMem + ?Sized>(
        &self,
        mem: &M,
        strukt: u64,
        mut visit: impl FnMut(u64, &str) -> Result<ControlFlow<()>, UeError>,
    ) -> Result<(), UeError> {
        let f = self.layout.ffield;
        let first = strukt + u64::from(self.layout.ustruct.child_properties);
        let mut cur = self.read_link(mem, first)?;
        let mut count = 0usize;
        while let Some(field) = cur {
            if count >= MAX_FIELDS {
                return Err(UeError::Malformed(format!(
                    "mais de {MAX_FIELDS} campos na struct {strukt:#x}"
                )));
            }
            count += 1;
            let name = self.fname_at(mem, field + u64::from(f.name))?;
            if visit(field, &name)?.is_break() {
                return Ok(());
            }
            cur = self.read_link(mem, field + u64::from(f.next))?;
        }
        Ok(())
    }

    /// Lê tipo, tamanho, offset e (para `BoolProperty`) byte e máscara do campo em `field`.
    fn read_field<M: ReadMem + ?Sized>(
        &self,
        mem: &M,
        strukt: u64,
        field: u64,
        name: &str,
    ) -> Result<Field, UeError> {
        let l = &self.layout;
        let class = self
            .read_link(mem, field + u64::from(l.ffield.class_private))?
            .ok_or_else(|| UeError::Malformed(format!("o campo {name} não tem FFieldClass")))?;
        let kind = self.fname_at(mem, class + u64::from(l.ffield.class_name))?;
        let size = read_u32(mem, field + u64::from(l.fprop.element_size))?;
        let offset = read_u32(mem, field + u64::from(l.fprop.offset))?;
        let bits = if kind == "BoolProperty" {
            let byte_offset = read_u8(mem, field + u64::from(l.fbool.byte_offset))?;
            let mask = read_u8(mem, field + u64::from(l.fbool.field_mask))?;
            Some(BoolBits {
                byte: offset + u32::from(byte_offset),
                mask,
            })
        } else {
            None
        };
        Ok(Field {
            owner: self.object_name(mem, strukt)?,
            name: name.to_string(),
            kind,
            offset,
            size,
            bits,
        })
    }

    /// Nome do pacote (outer) de cada objeto de `family`, lido em lotes. Outer nulo vira `""`;
    /// outer ilegível não entra no mapa.
    fn read_outer_names<M: ReadMem + ?Sized>(
        &self,
        mem: &M,
        family: &[Entry],
        budget: &ScanBudget,
        started: Instant,
    ) -> Result<HashMap<u64, String>, UeError> {
        let mut outers: Vec<u64> = family
            .iter()
            .map(|e| e.outer)
            .filter(|&o| is_user_addr(o))
            .collect();
        outers.sort_unstable();
        outers.dedup();
        let name_off = u64::from(self.layout.uobject.name);
        let mut out: HashMap<u64, String> = HashMap::new();
        out.insert(0, String::new());
        for group in outers.chunks(budget.batch.max(1)) {
            if started.elapsed() >= budget.deadline {
                return Err(UeError::Deadline);
            }
            let reqs: Vec<(u64, usize)> = group.iter().map(|&o| (o + name_off, 8)).collect();
            let data = mem.read_many(&reqs)?;
            for (&outer, bytes) in group.iter().zip(data) {
                let Some(bytes) = bytes else { continue };
                match self.names.fname(mem, u32_at(&bytes, 0), u32_at(&bytes, 4)) {
                    Ok(name) => {
                        out.insert(outer, name);
                    }
                    Err(e) => tracing::debug!(outer, "outer sem nome legível: {e}"),
                }
            }
            (budget.sleep)(budget.pause);
        }
        Ok(out)
    }
}

/// Escolhe a âncora entre os objetos cuja classe é eles mesmos: `/Script/CoreUObject.Class`.
fn find_anchor<M: ReadMem + ?Sized>(
    mem: &M,
    layout: &UeLayout,
    names: &NameLookup,
    candidates: &[(u64, (u32, u32), u64)],
) -> Result<u64, UeError> {
    let mut hits = Vec::new();
    for &(addr, name, outer) in candidates {
        if names.fname(mem, name.0, name.1).ok().as_deref() != Some(ANCHOR.1) {
            continue;
        }
        if !is_user_addr(outer) {
            continue;
        }
        let mut raw = [0u8; 8];
        if read_bytes(mem, outer + u64::from(layout.uobject.name), &mut raw).is_err() {
            continue;
        }
        let pkg = names.fname(mem, u32_at(&raw, 0), u32_at(&raw, 4));
        if pkg.ok().as_deref() == Some(ANCHOR.0) {
            hits.push(addr);
        }
    }
    match hits.as_slice() {
        [] => Err(UeError::NotFound(format!(
            "âncora {}.{} (objeto que é classe de si mesmo)",
            ANCHOR.0, ANCHOR.1
        ))),
        [only] => Ok(*only),
        _ => Err(UeError::Ambiguous(format!(
            "âncora {}.{} ({} objetos)",
            ANCHOR.0,
            ANCHOR.1,
            hits.len()
        ))),
    }
}
