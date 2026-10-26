//! Assinaturas dos patches: leitura dos `signatures/*.toml` e regra de carga (spec 002, 6.3).
//!
//! `generic.toml` define cada patch para qualquer versão do jogo; `<chave>.toml` registra o que
//! foi verificado numa build (o `rva`) e pode sobrescrever `pattern`, `offset`, `expected` e
//! `replacement`. Os arquivos são embutidos na compilação.
//!
//! Fase 5 (spec 005): o `generic.toml` também traz o bloco `[ue]` (layout das estruturas de
//! reflexão, padrões das globais e o caminho até o personagem) e os cheats de valor `[[field]]`.
//! O `<chave>.toml` registra o `rva` das globais e pode substituir o `[ue.layout]` inteiro.

use std::collections::{BTreeMap, HashSet};

use gt_core::global::GlobalPattern;
use gt_core::scan::Pattern;
use gt_core::value::{ValueSpec, ValueType};
use gt_ue::{
    FBoolLayout, FFieldLayout, FPropLayout, FStructLayout, ObjectsLayout, UObjectLayout,
    UStructLayout, UeLayout,
};
use serde::Deserialize;

use crate::GAME_EXE;
use crate::error::{Error, Result};

const GENERIC_FILE: &str = "generic.toml";

/// Arquivos embutidos: nome e conteúdo.
const EMBEDDED: &[(&str, &str)] = &[
    (GENERIC_FILE, include_str!("../signatures/generic.toml")),
    (
        "c935e799-0b27c000.toml",
        include_str!("../signatures/c935e799-0b27c000.toml"),
    ),
];

/// Categoria de um cheat na lista. A ordem de declaração é a ordem de exibição.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    Sobrevivencia,
    /// Cheats do pet (o animal domesticado que segue o personagem).
    Pet,
    Itens,
    Progressao,
    Movimento,
    Diagnostico,
}

impl Category {
    /// Todas as categorias, na ordem de exibição.
    pub const ALL: [Category; 6] = [
        Category::Sobrevivencia,
        Category::Pet,
        Category::Itens,
        Category::Progressao,
        Category::Movimento,
        Category::Diagnostico,
    ];

    /// Rótulo mostrado como cabeçalho da categoria.
    pub fn label(self) -> &'static str {
        match self {
            Category::Sobrevivencia => "SOBREVIVÊNCIA",
            Category::Pet => "PET",
            Category::Itens => "ITENS",
            Category::Progressao => "PROGRESSÃO",
            Category::Movimento => "MOVIMENTO",
            Category::Diagnostico => "DIAGNÓSTICO",
        }
    }

    /// Lê o nome usado no TOML (`sobrevivencia`, `pet`, `itens`, `progressao`, `movimento`,
    /// `diagnostico`).
    pub fn parse(text: &str) -> Option<Category> {
        match text {
            "sobrevivencia" => Some(Category::Sobrevivencia),
            "pet" => Some(Category::Pet),
            "itens" => Some(Category::Itens),
            "progressao" => Some(Category::Progressao),
            "movimento" => Some(Category::Movimento),
            "diagnostico" => Some(Category::Diagnostico),
            _ => None,
        }
    }
}

/// Definição de um patch, já validada pela regra de carga.
#[derive(Debug, Clone, PartialEq)]
pub struct PatchDef {
    pub id: String,
    /// Nome curto, mostrado na lista.
    pub name: String,
    pub category: Category,
    pub description: String,
    /// Módulo onde o patch vale (nesta fase, só o exe do jogo).
    pub module: String,
    /// Bytes originais, com curingas.
    pub pattern: Pattern,
    /// Distância do início da ocorrência até o início do patch.
    pub offset: usize,
    pub expected: Vec<u8>,
    pub replacement: Vec<u8>,
    /// Início do patch relativo à base do módulo, se a versão tem arquivo com entrada para o `id`.
    pub rva: Option<u64>,
    /// Faz do patch um cheat com valor ajustável.
    pub value: Option<ValueSpec>,
}

/// Global do motor achada por padrão (spec 005, 6.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalDef {
    pub pattern: GlobalPattern,
    /// `rva` da global **resolvida** (depois do `adjust`), se a versão tem arquivo com ele.
    pub rva: Option<u64>,
}

/// Caminho até o personagem (spec 005, 6.3), por nomes da reflexão.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerCfg {
    /// Classe do controller, como `Pacote.Nome`.
    pub controller_class: String,
    /// Campo do controller que aponta para o pawn.
    pub pawn_field: String,
    /// Classe esperada do pawn, como `Pacote.Nome`.
    pub pawn_class: String,
    /// Campos alternativos tentados quando `pawn_field` falha.
    pub alt_pawn_fields: Vec<String>,
}

/// Bloco `[ue]` já resolvido para uma versão do jogo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UeSigs {
    pub layout: UeLayout,
    pub gobjects: GlobalDef,
    pub names: GlobalDef,
    pub player: PlayerCfg,
}

/// Modo de uma trava: quando ela escreve (spec 005, 6.5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    /// Só sobe o valor até o alvo (barras).
    Raise,
    /// Só desce o valor até o alvo (dano acumulado).
    Lower,
    /// Leva o valor ao alvo sempre que for diferente (temperatura).
    Exact,
}

/// De onde vem o alvo de uma trava. Outro nome para não colidir com o `Target` do processo.
#[derive(Debug, Clone, PartialEq)]
pub enum LockTarget {
    /// Outro campo do mesmo objeto, relido a cada ciclo.
    Field(String),
    /// Constante do `generic.toml`.
    Const(f32),
}

/// Regra de um cheat de valor.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldRule {
    /// Reescreve um campo `float` com o alvo, dentro da faixa sã `[min, max]`.
    Lock {
        field: String,
        mode: LockMode,
        target: LockTarget,
        min: f32,
        max: f32,
    },
    /// Limpa os bits dos campos `bool`, restaurando o original ao desligar.
    ClearBits { fields: Vec<String> },
}

/// Cheat de valor (`[[field]]`), já validado pela regra de carga.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldDef {
    pub id: String,
    /// Nome curto, mostrado na lista.
    pub name: String,
    pub category: Category,
    pub description: String,
    /// Campo de componente no pawn, ou `"pawn"` para o próprio pawn.
    pub object: String,
    /// Classe esperada do objeto (`Pacote.Nome`), conferida com `is_a`.
    pub object_class: String,
    pub rule: FieldRule,
}

/// Conjunto das assinaturas carregadas: o genérico e uma lista de definições por versão.
#[derive(Debug, Clone)]
pub struct Signatures {
    generic: Vec<PatchDef>,
    versions: BTreeMap<String, VersionSigs>,
    ue: Option<GenericUe>,
    fields: Vec<FieldDef>,
}

#[derive(Debug, Clone)]
struct VersionSigs {
    product_version: Option<String>,
    defs: Vec<PatchDef>,
    ue: Option<VersionUe>,
}

/// `[ue]` do genérico, já validado.
#[derive(Debug, Clone)]
struct GenericUe {
    layout: UeLayout,
    gobjects: GlobalPattern,
    names: GlobalPattern,
    player: PlayerCfg,
}

/// `[ue]` de um `<chave>.toml`, já validado.
#[derive(Debug, Clone)]
struct VersionUe {
    gobjects_rva: Option<u64>,
    names_rva: Option<u64>,
    /// Substitui o layout genérico inteiro.
    layout: Option<UeLayout>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GenericFile {
    #[serde(default)]
    patch: Vec<GenericPatch>,
    ue: Option<GenericUeBlock>,
    #[serde(default)]
    field: Vec<GenericField>,
}

/// Bloco `[ue]` do `generic.toml`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GenericUeBlock {
    gobjects: GlobalBlock,
    names: GlobalBlock,
    layout: LayoutBlock,
    player: PlayerBlock,
}

/// `gobjects = { pattern, disp_offset, insn_end, adjust }`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GlobalBlock {
    pattern: String,
    disp_offset: usize,
    insn_end: usize,
    #[serde(default)]
    adjust: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlayerBlock {
    controller_class: String,
    pawn_field: String,
    pawn_class: String,
    #[serde(default)]
    alt_pawn_fields: Vec<String>,
}

// Espelhos de `UeLayout` para o serde (o `gt-ue` não depende dele). Sem `Default`: todo campo é
// obrigatório, porque o código nunca assume os valores de um build.

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LayoutBlock {
    objects: ObjectsBlock,
    uobject: UObjectBlock,
    ustruct: UStructBlock,
    ffield: FFieldBlock,
    fprop: FPropBlock,
    fbool: FBoolBlock,
    fstruct: FStructBlock,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ObjectsBlock {
    chunks: u32,
    max_elements: u32,
    num_elements: u32,
    max_chunks: u32,
    num_chunks: u32,
    items_per_chunk: u32,
    item_size: u32,
    item_object: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UObjectBlock {
    flags: u32,
    index: u32,
    class: u32,
    name: u32,
    outer: u32,
    cdo_flag: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UStructBlock {
    super_struct: u32,
    child_properties: u32,
    properties_size: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FFieldBlock {
    class_private: u32,
    class_name: u32,
    next: u32,
    name: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FPropBlock {
    element_size: u32,
    offset: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FBoolBlock {
    field_size: u32,
    byte_offset: u32,
    byte_mask: u32,
    field_mask: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FStructBlock {
    struct_ptr: u32,
}

/// Bloco `[[field]]`. Os números ficam como `toml::Value` porque `f32` direto recusa inteiro TOML.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GenericField {
    id: String,
    // `name`, `category` e `rule` são obrigatórios, mas a falta é reportada pela regra de carga
    // (com o campo certo), não pelo serde.
    name: Option<String>,
    category: Option<String>,
    description: String,
    object: String,
    object_class: String,
    rule: Option<String>,
    field: Option<String>,
    fields: Option<Vec<String>>,
    kind: Option<String>,
    mode: Option<String>,
    target_field: Option<String>,
    target: Option<toml::Value>,
    min: Option<toml::Value>,
    max: Option<toml::Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GenericPatch {
    id: String,
    // `name` e `category` são obrigatórios, mas a falta é reportada pela regra de carga
    // (com o campo certo), não pelo serde.
    name: Option<String>,
    category: Option<String>,
    description: String,
    module: Option<String>,
    pattern: String,
    offset: usize,
    expected: String,
    replacement: String,
    value: Option<ValueBlock>,
}

/// Bloco `[patch.value]`. Os números ficam como `toml::Value` porque `f64` direto recusa
/// inteiro TOML e `i64` recusa decimal; a conversão é à mão, por tipo (`build_value`).
#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct ValueBlock {
    #[serde(rename = "type")]
    ty: Option<toml::Value>,
    slot: Option<toml::Value>,
    min: Option<toml::Value>,
    max: Option<toml::Value>,
    step: Option<toml::Value>,
    default: Option<toml::Value>,
    unit: Option<toml::Value>,
    decimals: Option<toml::Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionFile {
    version: String,
    product_version: Option<String>,
    #[serde(default)]
    patch: Vec<VersionPatch>,
    ue: Option<VersionUeBlock>,
}

/// Bloco `[ue]` do `<chave>.toml`: `rva` das globais resolvidas e, se preciso, o layout inteiro.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionUeBlock {
    gobjects_rva: Option<u64>,
    names_rva: Option<u64>,
    layout: Option<LayoutBlock>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionPatch {
    id: String,
    rva: u64,
    name: Option<String>,
    category: Option<String>,
    pattern: Option<String>,
    offset: Option<usize>,
    expected: Option<String>,
    replacement: Option<String>,
    /// Substitui o bloco `value` inteiro, nunca campo a campo.
    value: Option<ValueBlock>,
}

/// Definição ainda em texto, antes de validar (o `<chave>.toml` sobrescreve campos em texto).
#[derive(Clone)]
struct RawDef {
    id: String,
    name: Option<String>,
    category: Option<String>,
    description: String,
    module: Option<String>,
    pattern: String,
    offset: usize,
    expected: String,
    replacement: String,
    rva: Option<u64>,
    value: Option<ValueBlock>,
}

fn sig_err(file: &str, field: &str, reason: impl Into<String>) -> Error {
    Error::Signature {
        file: file.to_string(),
        field: field.to_string(),
        reason: reason.into(),
    }
}

/// Bytes em hex separados por espaço (`48 8B F9`), sem curingas.
fn parse_hex(file: &str, field: &str, text: &str) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for token in text.split_whitespace() {
        if token.len() != 2 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(sig_err(
                file,
                field,
                format!("byte hexadecimal inválido: `{token}`"),
            ));
        }
        let byte = u8::from_str_radix(token, 16)
            .map_err(|_| sig_err(file, field, format!("byte hexadecimal inválido: `{token}`")))?;
        out.push(byte);
    }
    if out.is_empty() {
        return Err(sig_err(file, field, "vazio"));
    }
    Ok(out)
}

/// Número do bloco `value`: inteiro TOML sempre vale; decimal só para `f32`.
fn value_number(file: &str, field: &str, v: Option<&toml::Value>, ty: ValueType) -> Result<f64> {
    let Some(v) = v else {
        return Err(sig_err(file, field, "campo obrigatório ausente"));
    };
    match v {
        // Inteiros fora da faixa exata do `f64` acabam recusados pelas conferências de faixa.
        toml::Value::Integer(i) => Ok(*i as f64),
        toml::Value::Float(f) if ty == ValueType::F32 => Ok(*f),
        toml::Value::Float(f) => Err(sig_err(
            file,
            field,
            format!("{f} precisa ser inteiro neste tipo"),
        )),
        other => Err(sig_err(
            file,
            field,
            format!("esperava um número, veio {}", other.type_str()),
        )),
    }
}

/// Casas decimais mostradas quando o TOML não diz: as necessárias para o `step` (até 6).
fn decimals_for(step: f64) -> u8 {
    (0u8..6)
        .find(|&d| {
            let scaled = step * 10f64.powi(i32::from(d));
            (scaled - scaled.round()).abs() < 1e-6 * scaled.abs().max(1.0)
        })
        .unwrap_or(6)
}

/// Converte o bloco `value` no `ValueSpec` e confere as regras de carga do valor (6.3).
fn build_value(file: &str, block: &ValueBlock, replacement: &[u8]) -> Result<ValueSpec> {
    let ty = match block.ty.as_ref() {
        None => return Err(sig_err(file, "value.type", "campo obrigatório ausente")),
        Some(toml::Value::String(t)) => match t.as_str() {
            "i32" => ValueType::I32,
            "u32" => ValueType::U32,
            "f32" => ValueType::F32,
            other => {
                return Err(sig_err(
                    file,
                    "value.type",
                    format!("`{other}` não é i32, u32 nem f32"),
                ));
            }
        },
        Some(_) => return Err(sig_err(file, "value.type", "esperava um texto")),
    };
    let slot = match block.slot.as_ref() {
        None => return Err(sig_err(file, "value.slot", "campo obrigatório ausente")),
        Some(toml::Value::Integer(i)) => usize::try_from(*i)
            .map_err(|_| sig_err(file, "value.slot", format!("{i} não é um offset válido")))?,
        Some(_) => return Err(sig_err(file, "value.slot", "esperava um inteiro")),
    };
    let min = value_number(file, "value.min", block.min.as_ref(), ty)?;
    let max = value_number(file, "value.max", block.max.as_ref(), ty)?;
    let step = value_number(file, "value.step", block.step.as_ref(), ty)?;
    let default = value_number(file, "value.default", block.default.as_ref(), ty)?;
    let unit = match block.unit.as_ref() {
        None => None,
        Some(toml::Value::String(u)) => Some(u.clone()),
        Some(_) => return Err(sig_err(file, "value.unit", "esperava um texto")),
    };
    let decimals = match (block.decimals.as_ref(), ty) {
        (None, ValueType::F32) => None,
        (None, _) => Some(0),
        (Some(_), ValueType::I32 | ValueType::U32) => {
            return Err(sig_err(file, "value.decimals", "só vale para f32"));
        }
        (Some(toml::Value::Integer(d)), ValueType::F32) => Some(
            u8::try_from(*d)
                .ok()
                .filter(|&d| d <= 9)
                .ok_or_else(|| sig_err(file, "value.decimals", "precisa estar entre 0 e 9"))?,
        ),
        (Some(_), ValueType::F32) => {
            return Err(sig_err(file, "value.decimals", "esperava um inteiro"));
        }
    };

    // Conferências com campo próprio antes de calcular a grade (evita dividir por zero).
    if !step.is_finite() || step <= 0.0 {
        return Err(sig_err(
            file,
            "value.step",
            format!("{step} precisa ser finito e maior que 0"),
        ));
    }
    if !min.is_finite() || !max.is_finite() {
        return Err(sig_err(file, "value.min", "min e max precisam ser finitos"));
    }
    if min > max {
        return Err(sig_err(
            file,
            "value.max",
            format!("min {min} maior que max {max}"),
        ));
    }
    if !default.is_finite() {
        return Err(sig_err(file, "value.default", "não é um número finito"));
    }
    let slack = step * 1e-4;
    if default < min - slack || default > max + slack {
        return Err(sig_err(
            file,
            "value.default",
            format!("{default} fora de [{min}, {max}]"),
        ));
    }
    let k = ((default - min) / step).round();
    if (default - (min + k * step)).abs() >= slack {
        return Err(sig_err(
            file,
            "value.default",
            format!("{default} não cai na grade {min} + k·{step}"),
        ));
    }

    let spec = ValueSpec {
        ty,
        slot,
        min,
        max,
        step,
        // `k` é inteiro e `>= 0` aqui (o default está em `[min - slack, ...]`); a conversão
        // satura em 0 no caso de `-0.0`.
        default_k: k.max(0.0) as u64,
        unit,
        decimals: decimals.unwrap_or_else(|| decimals_for(step)),
    };
    spec.check().map_err(|e| sig_err(file, "value", e))?;
    spec.check_slot(replacement)
        .map_err(|e| sig_err(file, "value.slot", e))?;
    Ok(spec)
}

/// `name` obrigatório e não vazio.
fn check_name(file: &str, name: Option<String>) -> Result<String> {
    match name {
        None => Err(sig_err(file, "name", "campo obrigatório ausente")),
        Some(n) if n.trim().is_empty() => Err(sig_err(file, "name", "vazio")),
        Some(n) => Ok(n),
    }
}

/// `category` obrigatória e conhecida.
fn check_category(file: &str, category: Option<&str>) -> Result<Category> {
    match category {
        None => Err(sig_err(file, "category", "campo obrigatório ausente")),
        Some(text) => Category::parse(text).ok_or_else(|| {
            sig_err(
                file,
                "category",
                format!(
                    "`{text}` não é sobrevivencia, pet, itens, progressao, movimento nem diagnostico"
                ),
            )
        }),
    }
}

/// Aplica a regra de carga (6.3) a uma definição em texto.
fn build(file: &str, raw: RawDef) -> Result<PatchDef> {
    let name = check_name(file, raw.name)?;
    let category = check_category(file, raw.category.as_deref())?;
    let module = raw.module.unwrap_or_else(|| GAME_EXE.to_string());
    if module != GAME_EXE {
        return Err(sig_err(
            file,
            "module",
            format!("só `{GAME_EXE}` é aceito nesta fase, veio `{module}`"),
        ));
    }
    let pattern =
        Pattern::parse(&raw.pattern).map_err(|e| sig_err(file, "pattern", e.to_string()))?;
    let expected = parse_hex(file, "expected", &raw.expected)?;
    let replacement = parse_hex(file, "replacement", &raw.replacement)?;
    if expected.len() != replacement.len() {
        return Err(sig_err(
            file,
            "replacement",
            format!(
                "tamanho {} diferente do de expected ({})",
                replacement.len(),
                expected.len()
            ),
        ));
    }
    let end = raw
        .offset
        .checked_add(expected.len())
        .filter(|&end| end <= pattern.len())
        .ok_or_else(|| {
            sig_err(
                file,
                "offset",
                format!(
                    "o trecho [{}, +{}) não cabe no pattern de {} posições",
                    raw.offset,
                    expected.len(),
                    pattern.len()
                ),
            )
        })?;
    for (i, want) in (raw.offset..end).zip(&expected) {
        if let Some(Some(fixed)) = pattern.token(i)
            && fixed != *want
        {
            return Err(sig_err(
                file,
                "expected",
                format!(
                    "o byte fixo {fixed:02X} do pattern na posição {i} difere de expected ({want:02X})"
                ),
            ));
        }
    }
    let value = raw
        .value
        .as_ref()
        .map(|block| build_value(file, block, &replacement))
        .transpose()?;
    Ok(PatchDef {
        id: raw.id,
        name,
        category,
        description: raw.description,
        module,
        pattern,
        offset: raw.offset,
        expected,
        replacement,
        rva: raw.rva,
        value,
    })
}

/// Padrão de global: o `disp32` e o fim da instrução têm de caber no padrão.
fn build_global(file: &str, name: &str, block: &GlobalBlock) -> Result<GlobalPattern> {
    let field = |what: &str| format!("ue.{name}.{what}");
    let pattern = Pattern::parse(&block.pattern)
        .map_err(|e| sig_err(file, &field("pattern"), e.to_string()))?;
    let disp_end = block
        .disp_offset
        .checked_add(4)
        .filter(|&end| end <= pattern.len())
        .ok_or_else(|| {
            sig_err(
                file,
                &field("disp_offset"),
                format!(
                    "o disp32 em [{}, +4) não cabe no pattern de {} posições",
                    block.disp_offset,
                    pattern.len()
                ),
            )
        })?;
    if block.insn_end < disp_end || block.insn_end > pattern.len() {
        return Err(sig_err(
            file,
            &field("insn_end"),
            format!(
                "{} precisa estar entre o fim do disp32 ({disp_end}) e o fim do pattern ({})",
                block.insn_end,
                pattern.len()
            ),
        ));
    }
    Ok(GlobalPattern {
        pattern,
        disp_offset: block.disp_offset,
        insn_end: block.insn_end,
        adjust: block.adjust,
    })
}

/// Nome de classe como `Pacote.Nome`.
fn check_class_path(file: &str, field: &str, text: &str) -> Result<()> {
    match text.rsplit_once('.') {
        Some((pkg, name))
            if !pkg.is_empty() && !name.is_empty() && !text.chars().any(char::is_whitespace) =>
        {
            Ok(())
        }
        _ => Err(sig_err(
            file,
            field,
            format!("`{text}` não está no formato Pacote.Nome"),
        )),
    }
}

fn check_not_blank(file: &str, field: &str, text: &str) -> Result<()> {
    if text.trim().is_empty() {
        return Err(sig_err(file, field, "vazio"));
    }
    Ok(())
}

fn build_player(file: &str, block: PlayerBlock) -> Result<PlayerCfg> {
    check_class_path(file, "ue.player.controller_class", &block.controller_class)?;
    check_not_blank(file, "ue.player.pawn_field", &block.pawn_field)?;
    check_class_path(file, "ue.player.pawn_class", &block.pawn_class)?;
    for alt in &block.alt_pawn_fields {
        check_not_blank(file, "ue.player.alt_pawn_fields", alt)?;
    }
    Ok(PlayerCfg {
        controller_class: block.controller_class,
        pawn_field: block.pawn_field,
        pawn_class: block.pawn_class,
        alt_pawn_fields: block.alt_pawn_fields,
    })
}

fn build_layout(file: &str, block: LayoutBlock) -> Result<UeLayout> {
    let o = block.objects;
    if o.items_per_chunk == 0 {
        return Err(sig_err(
            file,
            "ue.layout.objects.items_per_chunk",
            "precisa ser maior que 0",
        ));
    }
    if o.item_size == 0 {
        return Err(sig_err(
            file,
            "ue.layout.objects.item_size",
            "precisa ser maior que 0",
        ));
    }
    let (u, s, f, p, b) = (
        block.uobject,
        block.ustruct,
        block.ffield,
        block.fprop,
        block.fbool,
    );
    Ok(UeLayout {
        objects: ObjectsLayout {
            chunks: o.chunks,
            max_elements: o.max_elements,
            num_elements: o.num_elements,
            max_chunks: o.max_chunks,
            num_chunks: o.num_chunks,
            items_per_chunk: o.items_per_chunk,
            item_size: o.item_size,
            item_object: o.item_object,
        },
        uobject: UObjectLayout {
            flags: u.flags,
            index: u.index,
            class: u.class,
            name: u.name,
            outer: u.outer,
            cdo_flag: u.cdo_flag,
        },
        ustruct: UStructLayout {
            super_struct: s.super_struct,
            child_properties: s.child_properties,
            properties_size: s.properties_size,
        },
        ffield: FFieldLayout {
            class_private: f.class_private,
            class_name: f.class_name,
            next: f.next,
            name: f.name,
        },
        fprop: FPropLayout {
            element_size: p.element_size,
            offset: p.offset,
        },
        fbool: FBoolLayout {
            field_size: b.field_size,
            byte_offset: b.byte_offset,
            byte_mask: b.byte_mask,
            field_mask: b.field_mask,
        },
        fstruct: FStructLayout {
            struct_ptr: block.fstruct.struct_ptr,
        },
    })
}

fn build_generic_ue(file: &str, block: GenericUeBlock) -> Result<GenericUe> {
    Ok(GenericUe {
        gobjects: build_global(file, "gobjects", &block.gobjects)?,
        names: build_global(file, "names", &block.names)?,
        layout: build_layout(file, block.layout)?,
        player: build_player(file, block.player)?,
    })
}

/// Número finito (`f32`) de um campo do `[[field]]`: inteiro ou decimal do TOML.
fn field_number(file: &str, field: &str, id: &str, v: &toml::Value) -> Result<f32> {
    let n = match v {
        toml::Value::Integer(i) => *i as f64,
        toml::Value::Float(f) => *f,
        other => {
            return Err(sig_err(
                file,
                field,
                format!("`{id}`: esperava um número, veio {}", other.type_str()),
            ));
        }
    };
    let n = n as f32;
    if !n.is_finite() {
        return Err(sig_err(
            file,
            field,
            format!("`{id}`: precisa ser um número finito"),
        ));
    }
    Ok(n)
}

/// Aplica a regra de carga (6.5) a um `[[field]]`.
fn build_field(file: &str, raw: GenericField) -> Result<FieldDef> {
    let id = raw.id.as_str();
    check_not_blank(file, "id", id)?;
    let err = |field: &str, reason: &str| sig_err(file, field, format!("`{id}`: {reason}"));
    let name = check_name(file, raw.name.clone())?;
    let category = check_category(file, raw.category.as_deref())?;
    check_not_blank(file, "object", &raw.object)?;
    check_class_path(file, "object_class", &raw.object_class)?;

    let rule = match raw.rule.as_deref() {
        None => return Err(err("rule", "campo obrigatório ausente")),
        Some("lock") => {
            if raw.fields.is_some() {
                return Err(err("fields", "só vale com rule = \"clear_bits\""));
            }
            let field = match raw.field.as_deref() {
                None => return Err(err("field", "campo obrigatório ausente")),
                Some(f) if f.trim().is_empty() => return Err(err("field", "vazio")),
                Some(f) => f.to_string(),
            };
            match raw.kind.as_deref() {
                None => return Err(err("kind", "campo obrigatório ausente")),
                Some("float") => {}
                Some(other) => {
                    return Err(err(
                        "kind",
                        &format!("`{other}` não é float (trava de valor)"),
                    ));
                }
            }
            let mode = match raw.mode.as_deref() {
                None => return Err(err("mode", "campo obrigatório ausente")),
                Some("raise") => LockMode::Raise,
                Some("lower") => LockMode::Lower,
                Some("exact") => LockMode::Exact,
                Some(other) => {
                    return Err(err(
                        "mode",
                        &format!("`{other}` não é raise, lower nem exact"),
                    ));
                }
            };
            let min = match raw.min.as_ref() {
                None => return Err(err("min", "campo obrigatório ausente")),
                Some(v) => field_number(file, "min", id, v)?,
            };
            let max = match raw.max.as_ref() {
                None => return Err(err("max", "campo obrigatório ausente")),
                Some(v) => field_number(file, "max", id, v)?,
            };
            if min > max {
                return Err(err("max", &format!("min {min} maior que max {max}")));
            }
            let target = match (raw.target_field, raw.target.as_ref()) {
                (Some(_), Some(_)) => {
                    return Err(err("target", "target e target_field não podem vir juntos"));
                }
                (None, None) => return Err(err("target", "falta target ou target_field")),
                (Some(f), None) if f.trim().is_empty() => return Err(err("target_field", "vazio")),
                (Some(f), None) => LockTarget::Field(f),
                (None, Some(v)) => {
                    let t = field_number(file, "target", id, v)?;
                    if t < min || t > max {
                        return Err(err("target", &format!("{t} fora de [{min}, {max}]")));
                    }
                    LockTarget::Const(t)
                }
            };
            FieldRule::Lock {
                field,
                mode,
                target,
                min,
                max,
            }
        }
        Some("clear_bits") => {
            for (present, field) in [
                (raw.field.is_some(), "field"),
                (raw.mode.is_some(), "mode"),
                (raw.target_field.is_some(), "target_field"),
                (raw.target.is_some(), "target"),
                (raw.min.is_some(), "min"),
                (raw.max.is_some(), "max"),
            ] {
                if present {
                    return Err(err(field, "só vale com rule = \"lock\""));
                }
            }
            match raw.kind.as_deref() {
                None => return Err(err("kind", "campo obrigatório ausente")),
                Some("bool") => {}
                Some(other) => {
                    return Err(err("kind", &format!("`{other}` não é bool (bits)")));
                }
            }
            let fields = match raw.fields {
                None => return Err(err("fields", "campo obrigatório ausente")),
                Some(f) if f.is_empty() => return Err(err("fields", "lista vazia")),
                Some(f) => f,
            };
            let mut seen = HashSet::new();
            for f in &fields {
                if f.trim().is_empty() {
                    return Err(err("fields", "nome vazio"));
                }
                if !seen.insert(f.as_str()) {
                    return Err(err("fields", &format!("`{f}` repetido")));
                }
            }
            FieldRule::ClearBits { fields }
        }
        Some(other) => {
            return Err(err("rule", &format!("`{other}` não é lock nem clear_bits")));
        }
    };
    Ok(FieldDef {
        id: raw.id,
        name,
        category,
        description: raw.description,
        object: raw.object,
        object_class: raw.object_class,
        rule,
    })
}

fn parse_toml<T: for<'de> Deserialize<'de>>(file: &str, text: &str) -> Result<T> {
    toml::from_str(text).map_err(|e| sig_err(file, "toml", e.to_string()))
}

impl Signatures {
    /// Carrega os arquivos (nome e conteúdo). `generic.toml` é obrigatório; os demais são
    /// `<chave>.toml` e o `version` interno precisa bater com o nome.
    pub fn parse(files: &[(&str, &str)]) -> Result<Signatures> {
        let mut generic_text = None;
        let mut version_files: Vec<(&str, &str)> = Vec::new();
        for &(name, text) in files {
            if name == GENERIC_FILE {
                if generic_text.replace(text).is_some() {
                    return Err(sig_err(name, "arquivo", "repetido"));
                }
            } else if name.ends_with(".toml") {
                version_files.push((name, text));
            } else {
                return Err(sig_err(name, "arquivo", "o nome deve terminar em .toml"));
            }
        }
        let generic_text =
            generic_text.ok_or_else(|| sig_err(GENERIC_FILE, "arquivo", "ausente"))?;

        let parsed: GenericFile = parse_toml(GENERIC_FILE, generic_text)?;
        let mut seen = HashSet::new();
        let mut raws = Vec::new();
        for p in parsed.patch {
            if !seen.insert(p.id.clone()) {
                return Err(sig_err(GENERIC_FILE, "id", format!("`{}` repetido", p.id)));
            }
            raws.push(RawDef {
                id: p.id,
                name: p.name,
                category: p.category,
                description: p.description,
                module: p.module,
                pattern: p.pattern,
                offset: p.offset,
                expected: p.expected,
                replacement: p.replacement,
                rva: None,
                value: p.value,
            });
        }
        let generic = raws
            .iter()
            .map(|r| build(GENERIC_FILE, r.clone()))
            .collect::<Result<Vec<_>>>()?;
        let ue = parsed
            .ue
            .map(|block| build_generic_ue(GENERIC_FILE, block))
            .transpose()?;
        // O `id` é único entre `[[patch]]` e `[[field]]`: `seen` já tem os dos patches.
        let mut fields = Vec::new();
        for f in parsed.field {
            if !seen.insert(f.id.clone()) {
                return Err(sig_err(
                    GENERIC_FILE,
                    "id",
                    format!("`{}` repetido entre [[patch]] e [[field]]", f.id),
                ));
            }
            fields.push(build_field(GENERIC_FILE, f)?);
        }

        let mut versions = BTreeMap::new();
        for (name, text) in version_files {
            let key = name.trim_end_matches(".toml");
            let file: VersionFile = parse_toml(name, text)?;
            if file.version != key {
                return Err(sig_err(
                    name,
                    "version",
                    format!(
                        "`{}` não bate com o nome do arquivo (`{key}`)",
                        file.version
                    ),
                ));
            }
            let mut merged: Vec<RawDef> = raws.clone();
            let mut seen = HashSet::new();
            for p in file.patch {
                if !seen.insert(p.id.clone()) {
                    return Err(sig_err(name, "id", format!("`{}` repetido", p.id)));
                }
                let Some(raw) = merged.iter_mut().find(|r| r.id == p.id) else {
                    return Err(sig_err(
                        name,
                        "id",
                        format!("`{}` não tem definição em {GENERIC_FILE}", p.id),
                    ));
                };
                raw.rva = Some(p.rva);
                if let Some(v) = p.pattern {
                    raw.pattern = v;
                }
                if let Some(v) = p.offset {
                    raw.offset = v;
                }
                if let Some(v) = p.expected {
                    raw.expected = v;
                }
                if let Some(v) = p.replacement {
                    raw.replacement = v;
                }
                if let Some(v) = p.name {
                    raw.name = Some(v);
                }
                if let Some(v) = p.category {
                    raw.category = Some(v);
                }
                if let Some(v) = p.value {
                    raw.value = Some(v);
                }
            }
            let defs = merged
                .into_iter()
                .map(|r| build(name, r))
                .collect::<Result<Vec<_>>>()?;
            let version_ue = match file.ue {
                None => None,
                Some(_) if ue.is_none() => {
                    return Err(sig_err(
                        name,
                        "ue",
                        format!("sem bloco [ue] em {GENERIC_FILE}"),
                    ));
                }
                Some(block) => Some(VersionUe {
                    gobjects_rva: block.gobjects_rva,
                    names_rva: block.names_rva,
                    layout: block
                        .layout
                        .map(|layout| build_layout(name, layout))
                        .transpose()?,
                }),
            };
            if versions
                .insert(
                    key.to_string(),
                    VersionSigs {
                        product_version: file.product_version,
                        defs,
                        ue: version_ue,
                    },
                )
                .is_some()
            {
                return Err(sig_err(name, "arquivo", "repetido"));
            }
        }
        Ok(Signatures {
            generic,
            versions,
            ue,
            fields,
        })
    }

    /// Carrega os arquivos de `signatures/` embutidos no binário.
    pub fn embedded() -> Result<Signatures> {
        Signatures::parse(EMBEDDED)
    }

    /// Há arquivo de assinaturas para a chave de versão?
    pub fn has_version(&self, key: &str) -> bool {
        self.versions.contains_key(key)
    }

    /// O `product_version` registrado no arquivo da versão, se houver.
    pub fn product_version(&self, key: &str) -> Option<&str> {
        self.versions.get(key)?.product_version.as_deref()
    }

    /// Definições para a versão: com as sobrescritas e o `rva` do arquivo dela, ou as genéricas
    /// (sem `rva`) quando não há arquivo.
    pub fn for_version(&self, key: &str) -> Vec<PatchDef> {
        match self.versions.get(key) {
            Some(v) => v.defs.clone(),
            None => self.generic.clone(),
        }
    }

    /// Os `id` do genérico, na ordem do arquivo.
    pub fn ids(&self) -> Vec<&str> {
        self.generic.iter().map(|d| d.id.as_str()).collect()
    }

    /// Bloco `[ue]` para a versão: o genérico com o `rva` e o layout do arquivo dela, se houver.
    /// `None` quando o `generic.toml` não tem `[ue]`.
    pub fn ue_for_version(&self, key: &str) -> Option<UeSigs> {
        let generic = self.ue.as_ref()?;
        let version = self.versions.get(key).and_then(|v| v.ue.as_ref());
        Some(UeSigs {
            layout: version.and_then(|v| v.layout).unwrap_or(generic.layout),
            gobjects: GlobalDef {
                pattern: generic.gobjects.clone(),
                rva: version.and_then(|v| v.gobjects_rva),
            },
            names: GlobalDef {
                pattern: generic.names.clone(),
                rva: version.and_then(|v| v.names_rva),
            },
            player: generic.player.clone(),
        })
    }

    /// Os cheats de valor (`[[field]]`), na ordem do arquivo. Valem para qualquer versão.
    pub fn fields(&self) -> &[FieldDef] {
        &self.fields
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GENERIC: &str = r#"
[[patch]]
id = "teste-codificacao"
name = "Patch de teste"
category = "diagnostico"
description = "Troca sem efeito"
pattern = "48 8B F9 ?? 90"
offset = 0
expected = "48 8B F9"
replacement = "48 89 CF"

[[patch]]
id = "outro"
name = "Outro"
category = "itens"
description = "Outro patch"
module = "Grounded2Steam-Win64-Shipping.exe"
pattern = "90 90 ?? C3"
offset = 2
expected = "11"
replacement = "22"
"#;

    fn parse(files: &[(&str, &str)]) -> Result<Signatures> {
        Signatures::parse(files)
    }

    #[test]
    fn parses_generic_and_version_with_override() {
        let version = r#"
version = "aaaa-bbbb"
product_version = "Build-1"

[[patch]]
id = "teste-codificacao"
rva = 0x56fc11c

[[patch]]
id = "outro"
rva = 0x10
pattern = "90 90 ?? ?? C3"
offset = 3
"#;
        let sigs = parse(&[("generic.toml", GENERIC), ("aaaa-bbbb.toml", version)]).unwrap();
        assert_eq!(sigs.ids(), vec!["teste-codificacao", "outro"]);
        assert!(sigs.has_version("aaaa-bbbb"));
        assert!(!sigs.has_version("cccc-dddd"));
        assert_eq!(sigs.product_version("aaaa-bbbb"), Some("Build-1"));

        let generic = sigs.for_version("cccc-dddd");
        assert_eq!(generic.len(), 2);
        assert_eq!(generic[0].module, crate::GAME_EXE);
        assert_eq!(generic[0].rva, None);
        assert_eq!(generic[0].expected, vec![0x48, 0x8B, 0xF9]);
        assert_eq!(generic[0].replacement, vec![0x48, 0x89, 0xCF]);
        assert_eq!(generic[0].pattern.len(), 5);

        let defs = sigs.for_version("aaaa-bbbb");
        assert_eq!(defs[0].rva, Some(0x56fc11c));
        assert_eq!(defs[0].pattern, generic[0].pattern);
        assert_eq!(defs[1].rva, Some(0x10));
        assert_eq!(defs[1].offset, 3);
        assert_eq!(defs[1].pattern.len(), 5);
        assert_eq!(defs[1].expected, vec![0x11]);
        assert_eq!(defs[1].description, "Outro patch");
    }

    #[test]
    fn load_rule_errors() {
        fn generic_with(patch: &str) -> String {
            format!(
                "[[patch]]\nid = \"p\"\nname = \"N\"\ncategory = \"itens\"\ndescription = \"d\"\n{patch}"
            )
        }
        let ok = "pattern = \"48 8B F9\"\noffset = 0\nexpected = \"48 8B F9\"\nreplacement = \"48 89 CF\"\n";
        type Files = Vec<(&'static str, String)>;
        let cases: Vec<(String, Files, &str, &str)> = vec![
            (
                "TOML inválido".into(),
                vec![("generic.toml", "[[patch".into())],
                "generic.toml",
                "toml",
            ),
            (
                "generic ausente".into(),
                vec![("aaaa-bbbb.toml", "version = \"aaaa-bbbb\"".into())],
                "generic.toml",
                "arquivo",
            ),
            (
                "module de outro exe".into(),
                vec![("generic.toml", generic_with(&format!("module = \"outra.dll\"\n{ok}")))],
                "generic.toml",
                "module",
            ),
            (
                "id repetido".into(),
                vec![(
                    "generic.toml",
                    format!("{}\n{}", generic_with(ok), generic_with(ok)),
                )],
                "generic.toml",
                "id",
            ),
            (
                "hex inválido em expected".into(),
                vec![(
                    "generic.toml",
                    generic_with(&ok.replace("expected = \"48 8B F9\"", "expected = \"48 8B G9\"")),
                )],
                "generic.toml",
                "expected",
            ),
            (
                "hex inválido em replacement".into(),
                vec![(
                    "generic.toml",
                    generic_with(&ok.replace("48 89 CF", "48 89 C")),
                )],
                "generic.toml",
                "replacement",
            ),
            (
                "padrão inválido".into(),
                vec![("generic.toml", generic_with(&ok.replace("\"48 8B F9\"\noffset", "\"?? ??\"\noffset")))],
                "generic.toml",
                "pattern",
            ),
            (
                "tamanhos diferentes".into(),
                vec![(
                    "generic.toml",
                    generic_with(&ok.replace("48 89 CF", "48 89 CF 90")),
                )],
                "generic.toml",
                "replacement",
            ),
            (
                "trecho fora do pattern".into(),
                vec![("generic.toml", generic_with(&ok.replace("offset = 0", "offset = 1")))],
                "generic.toml",
                "offset",
            ),
            (
                "byte fixo diferente de expected".into(),
                vec![(
                    "generic.toml",
                    generic_with(&ok.replace("expected = \"48 8B F9\"", "expected = \"48 8B F8\"")),
                )],
                "generic.toml",
                "expected",
            ),
            (
                "version diferente do nome".into(),
                vec![
                    ("generic.toml", generic_with(ok)),
                    ("aaaa-bbbb.toml", "version = \"cccc-dddd\"".into()),
                ],
                "aaaa-bbbb.toml",
                "version",
            ),
            (
                "id da versão sem definição genérica".into(),
                vec![
                    ("generic.toml", generic_with(ok)),
                    (
                        "aaaa-bbbb.toml",
                        "version = \"aaaa-bbbb\"\n[[patch]]\nid = \"fantasma\"\nrva = 1\n".into(),
                    ),
                ],
                "aaaa-bbbb.toml",
                "id",
            ),
            (
                "id repetido na versão".into(),
                vec![
                    ("generic.toml", generic_with(ok)),
                    (
                        "aaaa-bbbb.toml",
                        "version = \"aaaa-bbbb\"\n[[patch]]\nid = \"p\"\nrva = 1\n[[patch]]\nid = \"p\"\nrva = 2\n"
                            .into(),
                    ),
                ],
                "aaaa-bbbb.toml",
                "id",
            ),
            (
                "regra violada só depois da sobrescrita".into(),
                vec![
                    ("generic.toml", generic_with(ok)),
                    (
                        "aaaa-bbbb.toml",
                        "version = \"aaaa-bbbb\"\n[[patch]]\nid = \"p\"\nrva = 1\noffset = 2\n".into(),
                    ),
                ],
                "aaaa-bbbb.toml",
                "offset",
            ),
            (
                "campo desconhecido".into(),
                vec![("generic.toml", generic_with(&format!("rva = 3\n{ok}")))],
                "generic.toml",
                "toml",
            ),
        ];
        for (name, files, file, field) in cases {
            let borrowed: Vec<(&str, &str)> = files.iter().map(|(n, c)| (*n, c.as_str())).collect();
            match parse(&borrowed) {
                Err(Error::Signature {
                    file: f,
                    field: fl,
                    reason,
                }) => {
                    assert_eq!((f.as_str(), fl.as_str()), (file, field), "caso: {name}");
                    assert!(!reason.is_empty(), "caso: {name}");
                }
                other => panic!("caso {name}: esperava Error::Signature, veio {other:?}"),
            }
        }
    }

    #[test]
    fn error_message_names_file_and_field() {
        let err = parse(&[(
            "generic.toml",
            "[[patch]]\nid = \"p\"\nname = \"N\"\ncategory = \"itens\"\ndescription = \"d\"\nmodule = \"x\"\npattern = \"48\"\noffset = 0\nexpected = \"48\"\nreplacement = \"49\"\n",
        )])
        .unwrap_err();
        assert!(
            err.to_string()
                .starts_with("assinatura inválida em generic.toml, campo module: "),
            "{err}"
        );
    }

    #[test]
    fn embedded_files_load() {
        let sigs = Signatures::embedded().unwrap();
        assert!(sigs.has_version("c935e799-0b27c000"));
        assert_eq!(
            sigs.product_version("c935e799-0b27c000"),
            Some("Augusta-CL-3207493")
        );
    }

    #[test]
    fn real_files_load() {
        let sigs = Signatures::embedded().unwrap();
        let defs = sigs.for_version("c935e799-0b27c000");
        let test_patch = defs.iter().find(|d| d.id == "teste-codificacao").unwrap();
        assert_eq!(test_patch.name, "Patch de teste");
        assert_eq!(test_patch.category, Category::Diagnostico);
        assert_eq!(test_patch.value, None);
        // Fase 5: o `[ue]` carrega nas duas formas (genérico e versão verificada).
        assert!(sigs.ue_for_version("c935e799-0b27c000").is_some());
        assert!(sigs.ue_for_version("cccc-dddd").is_some());
        // Fase 5: os oito cheats de valor da spec 6.5, em sobrevivência, e os três do pet, em PET.
        let ids: Vec<&str> = sigs.fields().iter().map(|f| f.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "vida-infinita",
                "stamina-infinita",
                "comida-infinita",
                "agua-infinita",
                "oxigenio-infinito",
                "temperatura-estavel",
                "sem-dano-queda",
                "nao-morrer",
                "pet-vida-infinita",
                "pet-nao-morre",
                "pet-stamina-infinita",
            ]
        );
        let category = |id: &str| {
            if id.starts_with("pet-") {
                Category::Pet
            } else {
                Category::Sobrevivencia
            }
        };
        assert!(sigs.fields().iter().all(|f| f.category == category(&f.id)));
        let player = sigs.ue_for_version("c935e799-0b27c000").unwrap().player;
        assert_eq!(
            player.alt_pawn_fields,
            vec!["CachedSurvivalPlayerCharacter"]
        );
        // O genérico (versão sem arquivo) também carrega nome e categoria.
        assert!(
            sigs.for_version("cccc-dddd")
                .iter()
                .all(|d| !d.name.is_empty() && d.value.is_none())
        );
    }

    #[test]
    fn category_order_labels_and_parse() {
        assert!(Category::ALL.windows(2).all(|w| w[0] < w[1]));
        let labels: Vec<_> = Category::ALL.iter().map(|c| c.label()).collect();
        assert_eq!(
            labels,
            vec![
                "SOBREVIVÊNCIA",
                "PET",
                "ITENS",
                "PROGRESSÃO",
                "MOVIMENTO",
                "DIAGNÓSTICO"
            ]
        );
        for (text, cat) in [
            ("sobrevivencia", Category::Sobrevivencia),
            ("pet", Category::Pet),
            ("itens", Category::Itens),
            ("progressao", Category::Progressao),
            ("movimento", Category::Movimento),
            ("diagnostico", Category::Diagnostico),
        ] {
            assert_eq!(Category::parse(text), Some(cat));
        }
        assert_eq!(Category::parse("Itens"), None);
        assert_eq!(Category::parse(""), None);
    }

    #[test]
    fn every_signature_file_is_embedded() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("signatures");
        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".toml"))
            .collect();
        on_disk.sort();
        let mut embedded: Vec<String> = EMBEDDED.iter().map(|(n, _)| n.to_string()).collect();
        embedded.sort();
        assert_eq!(
            on_disk, embedded,
            "atualize EMBEDDED com os arquivos de signatures/"
        );
    }

    /// Patch com bytes `B8 00 00 80 3F` (mov eax, 1.0f) trocados por `replacement`, mais o bloco.
    fn value_generic(replacement: &str, block: &str) -> String {
        format!(
            "[[patch]]\nid = \"v\"\nname = \"Valor\"\ncategory = \"movimento\"\ndescription = \"d\"\n\
             pattern = \"B8 ?? ?? ?? ?? C3\"\noffset = 0\nexpected = \"B8 00 00 80 3F\"\n\
             replacement = \"{replacement}\"\n{block}"
        )
    }

    const F32_BLOCK: &str = "[patch.value]\ntype = \"f32\"\nslot = 1\nmin = 0.5\nmax = 4.0\n\
                             step = 0.5\ndefault = 1.5\nunit = \"x\"\ndecimals = 1\n";
    const F32_REPL: &str = "B8 00 00 C0 3F";

    fn value_err(generic: String) -> (String, String) {
        match parse(&[("generic.toml", &generic)]) {
            Err(Error::Signature { file, field, .. }) => (file, field),
            other => panic!("esperava Error::Signature, veio {other:?}"),
        }
    }

    #[test]
    fn value_block_builds_valuespec() {
        let sigs = parse(&[("generic.toml", &value_generic(F32_REPL, F32_BLOCK))]).unwrap();
        let def = &sigs.for_version("x")[0];
        assert_eq!(def.name, "Valor");
        assert_eq!(def.category, Category::Movimento);
        let spec = def.value.as_ref().unwrap();
        assert_eq!(spec.ty, ValueType::F32);
        assert_eq!(spec.slot, 1);
        assert_eq!((spec.min, spec.max, spec.step), (0.5, 4.0, 0.5));
        assert_eq!(spec.default_k, 2);
        assert_eq!(spec.unit.as_deref(), Some("x"));
        assert_eq!(spec.decimals, 1);
        assert_eq!(spec.bytes(&def.replacement, 2), def.replacement);
        assert_eq!(
            spec.bytes(&def.replacement, 3),
            vec![0xB8, 0, 0, 0x00, 0x40]
        );
        assert_eq!(spec.format(2), "1.5x");
    }

    #[test]
    fn f32_accepts_integer_and_decimal_toml() {
        // min e default inteiros; max e step decimais.
        let block = "[patch.value]\ntype = \"f32\"\nslot = 1\nmin = 0\nmax = 4.0\nstep = 0.5\n\
                     default = 1\n";
        let repl = "B8 00 00 80 3F";
        let sigs = parse(&[("generic.toml", &value_generic(repl, block))]).unwrap();
        let spec = sigs.for_version("x")[0].value.clone().unwrap();
        assert_eq!((spec.min, spec.default_k), (0.0, 2));
        // Sem `decimals`, deriva do passo.
        assert_eq!(spec.decimals, 1);
        // Tudo inteiro também vale.
        let block =
            "[patch.value]\ntype = \"f32\"\nslot = 1\nmin = 0\nmax = 4\nstep = 1\ndefault = 1\n";
        let sigs = parse(&[("generic.toml", &value_generic(repl, block))]).unwrap();
        assert_eq!(sigs.for_version("x")[0].value.as_ref().unwrap().decimals, 0);
    }

    #[test]
    fn integer_types_accept_only_integers() {
        // i32 em `mov eax, imm32`: B8 05 00 00 00.
        let int_block = |extra: &str| {
            format!(
                "[patch.value]\ntype = \"i32\"\nslot = 1\nmin = 0\nmax = 10\nstep = 1\ndefault = 5\n{extra}"
            )
        };
        let repl = "B8 05 00 00 00";
        let sigs = parse(&[("generic.toml", &value_generic(repl, &int_block("")))]).unwrap();
        let spec = sigs.for_version("x")[0].value.clone().unwrap();
        assert_eq!(
            (spec.ty, spec.default_k, spec.decimals),
            (ValueType::I32, 5, 0)
        );
        for (what, block, field) in [
            (
                "default decimal",
                int_block("").replace("default = 5", "default = 5.0"),
                "value.default",
            ),
            (
                "min decimal",
                int_block("").replace("min = 0", "min = 0.0"),
                "value.min",
            ),
            (
                "max decimal",
                int_block("").replace("max = 10", "max = 10.0"),
                "value.max",
            ),
            (
                "step decimal",
                int_block("").replace("step = 1", "step = 1.0"),
                "value.step",
            ),
            (
                "decimals em inteiro",
                int_block("decimals = 1\n"),
                "value.decimals",
            ),
        ] {
            let (file, got) = value_err(value_generic(repl, &block));
            assert_eq!(
                (file.as_str(), got.as_str()),
                ("generic.toml", field),
                "{what}"
            );
        }
    }

    #[test]
    fn name_and_category_errors() {
        let body = "pattern = \"48 8B F9\"\noffset = 0\nexpected = \"48 8B F9\"\nreplacement = \"48 89 CF\"\n";
        for (what, head, field) in [
            ("sem name", "category = \"itens\"\n", "name"),
            (
                "name vazio",
                "name = \"  \"\ncategory = \"itens\"\n",
                "name",
            ),
            ("sem category", "name = \"N\"\n", "category"),
            (
                "category inválida",
                "name = \"N\"\ncategory = \"extras\"\n",
                "category",
            ),
            (
                "category com maiúscula",
                "name = \"N\"\ncategory = \"Itens\"\n",
                "category",
            ),
        ] {
            let generic = format!("[[patch]]\nid = \"p\"\n{head}description = \"d\"\n{body}");
            let (file, got) = value_err(generic);
            assert_eq!(
                (file.as_str(), got.as_str()),
                ("generic.toml", field),
                "{what}"
            );
        }
    }

    #[test]
    fn value_load_errors() {
        let blk = |from: &str, to: &str| F32_BLOCK.replace(from, to);
        let cases: Vec<(&str, String, &str)> = vec![
            (
                "sem type",
                value_generic(F32_REPL, &blk("type = \"f32\"\n", "")),
                "value.type",
            ),
            (
                "type inválido",
                value_generic(F32_REPL, &blk("\"f32\"", "\"i64\"")),
                "value.type",
            ),
            (
                "sem slot",
                value_generic(F32_REPL, &blk("slot = 1\n", "")),
                "value.slot",
            ),
            (
                "sem default",
                value_generic(F32_REPL, &blk("default = 1.5\n", "")),
                "value.default",
            ),
            (
                "sem min",
                value_generic(F32_REPL, &blk("min = 0.5\n", "")),
                "value.min",
            ),
            (
                "min maior que max",
                value_generic(F32_REPL, &blk("min = 0.5", "min = 9.0")),
                "value.max",
            ),
            (
                "step zero",
                value_generic(F32_REPL, &blk("step = 0.5", "step = 0.0")),
                "value.step",
            ),
            (
                "step negativo",
                value_generic(F32_REPL, &blk("step = 0.5", "step = -0.5")),
                "value.step",
            ),
            (
                "default acima de max",
                value_generic(F32_REPL, &blk("default = 1.5", "default = 5.0")),
                "value.default",
            ),
            (
                "default abaixo de min",
                value_generic(F32_REPL, &blk("default = 1.5", "default = 0.0")),
                "value.default",
            ),
            (
                "default fora da grade",
                value_generic(F32_REPL, &blk("default = 1.5", "default = 1.3")),
                "value.default",
            ),
            (
                "default não finito",
                value_generic(F32_REPL, &blk("default = 1.5", "default = nan")),
                "value.default",
            ),
            (
                "campo desconhecido no bloco",
                value_generic(F32_REPL, &format!("{F32_BLOCK}extra = 1\n")),
                "toml",
            ),
            (
                "número onde se espera texto",
                value_generic(F32_REPL, &blk("unit = \"x\"", "unit = 3")),
                "value.unit",
            ),
            (
                "decimals fora de 0..=9",
                value_generic(F32_REPL, &blk("decimals = 1", "decimals = 10")),
                "value.decimals",
            ),
            (
                "faixa fora do tipo",
                value_generic(
                    "B8 00 00 00 00",
                    "[patch.value]\ntype = \"i32\"\nslot = 1\nmin = -3000000000\nmax = 0\nstep = 1\ndefault = 0\n",
                ),
                "value",
            ),
            (
                "slot fora de replacement",
                value_generic(F32_REPL, &blk("slot = 1", "slot = 2")),
                "value.slot",
            ),
            (
                "slot fora do imediato",
                value_generic(F32_REPL, &blk("slot = 1", "slot = 0")),
                "value.slot",
            ),
            (
                "imediato de tamanho diferente do tipo",
                // mov al, imm8 seguido de nops: não há imediato de 4 bytes no offset 1.
                value_generic("B0 00 90 90 90", &blk("slot = 1", "slot = 1")),
                "value.slot",
            ),
            (
                "imediato de desvio relativo",
                value_generic(
                    "E9 00 00 00 00",
                    "[patch.value]\ntype = \"i32\"\nslot = 1\nmin = 0\nmax = 10\nstep = 1\ndefault = 0\n",
                ),
                "value.slot",
            ),
            (
                "u32 acima de i32::MAX em imediato estendido com sinal",
                // mov rax, imm32 (48 C7 C0): o imediato é estendido com sinal até 64 bits.
                "[[patch]]\nid = \"v\"\nname = \"V\"\ncategory = \"itens\"\ndescription = \"d\"\n\
                 pattern = \"48 C7 C0 ?? ?? ?? ?? C3\"\noffset = 0\nexpected = \"48 C7 C0 00 00 00 00\"\n\
                 replacement = \"48 C7 C0 00 00 00 00\"\n\
                 [patch.value]\ntype = \"u32\"\nslot = 3\nmin = 0\nmax = 4000000000\nstep = 1\ndefault = 0\n"
                    .to_string(),
                "value.slot",
            ),
            (
                "bytes do slot diferentes do default",
                value_generic(F32_REPL, &blk("default = 1.5", "default = 2.0")),
                "value.slot",
            ),
        ];
        for (what, generic, field) in cases {
            let (file, got) = value_err(generic);
            assert_eq!(
                (file.as_str(), got.as_str()),
                ("generic.toml", field),
                "caso: {what}"
            );
        }
    }

    #[test]
    fn version_overrides_value_whole_block() {
        let generic = value_generic(F32_REPL, F32_BLOCK);
        // A versão troca replacement e o bloco inteiro (max, default e unit novos).
        let version = "version = \"aaaa-bbbb\"\n[[patch]]\nid = \"v\"\nrva = 1\nname = \"Outro nome\"\n\
                       category = \"itens\"\nreplacement = \"B8 00 00 00 40\"\n\
                       [patch.value]\ntype = \"f32\"\nslot = 1\nmin = 1\nmax = 8\nstep = 1\n\
                       default = 2\nunit = \"%\"\n";
        let sigs = parse(&[("generic.toml", &generic), ("aaaa-bbbb.toml", version)]).unwrap();
        let generic_def = &sigs.for_version("outra")[0];
        let version_def = &sigs.for_version("aaaa-bbbb")[0];
        assert_eq!(generic_def.value.as_ref().unwrap().max, 4.0);
        assert_eq!(generic_def.name, "Valor");
        let spec = version_def.value.as_ref().unwrap();
        assert_eq!(
            (spec.min, spec.max, spec.step, spec.default_k),
            (1.0, 8.0, 1.0, 1)
        );
        assert_eq!(spec.unit.as_deref(), Some("%"));
        // `decimals` não herdado do bloco genérico (era 1); sem ele, deriva do passo (0).
        assert_eq!(spec.decimals, 0);
        assert_eq!(version_def.name, "Outro nome");
        assert_eq!(version_def.category, Category::Itens);

        // Um bloco parcial na versão não completa o genérico: falta `type`.
        let partial =
            "version = \"aaaa-bbbb\"\n[[patch]]\nid = \"v\"\nrva = 1\n[patch.value]\nmax = 8\n";
        match parse(&[("generic.toml", &generic), ("aaaa-bbbb.toml", partial)]) {
            Err(Error::Signature { file, field, .. }) => {
                assert_eq!(
                    (file.as_str(), field.as_str()),
                    ("aaaa-bbbb.toml", "value.type")
                );
            }
            other => panic!("esperava Error::Signature, veio {other:?}"),
        }
        // As regras valem para o resultado: replacement novo com o bloco genérico não bate.
        let stale = "version = \"aaaa-bbbb\"\n[[patch]]\nid = \"v\"\nrva = 1\n\
                     replacement = \"B8 00 00 00 40\"\n";
        match parse(&[("generic.toml", &generic), ("aaaa-bbbb.toml", stale)]) {
            Err(Error::Signature { file, field, .. }) => {
                assert_eq!(
                    (file.as_str(), field.as_str()),
                    ("aaaa-bbbb.toml", "value.slot")
                );
            }
            other => panic!("esperava Error::Signature, veio {other:?}"),
        }
    }

    /// (id, name, rva, expected, replacement) dos cheats da Fase 4, na ordem do generic.toml (spec 004).
    const PHASE4: [(&str, &str, u64, &str, &str); 3] = [
        (
            "durabilidade-infinita",
            "Durabilidade infinita",
            0x5a4e777,
            "f3 41 0f 5c d0",
            "0f 1f 44 00 00",
        ),
        (
            "energia-infinita",
            "Tocha infinita",
            0x5a5744b,
            "f3 0f 58 cb",
            "0f 1f 40 00",
        ),
        (
            "recarga-instantanea",
            "Recarga instantânea",
            0x5c3bbfc,
            "0f 28 c7",
            "0f 57 c0",
        ),
    ];

    fn hex(text: &str) -> Vec<u8> {
        parse_hex("teste", "teste", text).unwrap()
    }

    #[test]
    fn phase4_cheats_load_verified() {
        let defs = Signatures::embedded()
            .unwrap()
            .for_version("c935e799-0b27c000");
        for (id, name, rva, expected, replacement) in PHASE4 {
            let def = defs
                .iter()
                .find(|d| d.id == id)
                .unwrap_or_else(|| panic!("{id} ausente"));
            assert_eq!(def.name, name);
            assert_eq!(def.category, Category::Itens);
            assert_eq!(def.value, None);
            assert_eq!(def.rva, Some(rva), "{id}");
            assert_eq!(def.expected, hex(expected), "{id}");
            assert_eq!(def.replacement, hex(replacement), "{id}");
            assert!(!def.description.is_empty(), "{id}");
        }
        let itens: Vec<_> = defs
            .iter()
            .filter(|d| d.category == Category::Itens)
            .map(|d| d.id.as_str())
            .collect();
        assert_eq!(
            itens,
            PHASE4.iter().map(|p| p.0).collect::<Vec<_>>(),
            "ordem em Itens"
        );
    }

    #[test]
    fn phase4_cheats_generic_without_rva() {
        let defs = Signatures::embedded().unwrap().for_version("cccc-dddd");
        for (id, _, _, expected, replacement) in PHASE4 {
            let def = defs
                .iter()
                .find(|d| d.id == id)
                .unwrap_or_else(|| panic!("{id} ausente"));
            assert_eq!(def.rva, None, "{id}");
            assert_eq!(def.expected, hex(expected), "{id}");
            assert_eq!(def.replacement, hex(replacement), "{id}");
        }
    }

    #[test]
    fn phase4_cheats_keep_instruction_boundaries() {
        use gt_core::patch::{PatchSpec, overlaps, validate};
        let mut ranges = Vec::new();
        ranges.push(0x112c468..0x112c46b); // teste-codificacao
        for (id, _, rva, expected, replacement) in PHASE4 {
            let v = validate(PatchSpec {
                id: id.into(),
                addr: rva,
                expected: hex(expected),
                replacement: hex(replacement),
            })
            .unwrap_or_else(|e| panic!("{id}: {e}"));
            assert!(v.lost_on_apply().is_empty(), "{id}");
            assert!(v.lost_on_undo().is_empty(), "{id}");
            ranges.push(v.spec.range());
        }
        for (i, a) in ranges.iter().enumerate() {
            for b in &ranges[i + 1..] {
                assert!(!overlaps(a, b), "{a:x?} e {b:x?} se sobrepõem");
            }
        }
    }

    // ---- Fase 5: `[ue]` e `[[field]]` ----

    const UE_PATTERNS: &str = r#"
[ue]
gobjects = { pattern = "48 8b 05 ?? ?? ?? ?? 48 8b 0c c8 48 8d 04 d1", disp_offset = 3, insn_end = 7, adjust = 0 }
names = { pattern = "74 09 ?? 8d ?? ?? ?? ?? ?? eb ?? 48 8d 0d ?? ?? ?? ?? e8", disp_offset = 14, insn_end = 18, adjust = 0x10 }
"#;

    const UE_LAYOUT: &str = r#"
[ue.layout.objects]
chunks = 0x0
max_elements = 0x10
num_elements = 0x14
max_chunks = 0x18
num_chunks = 0x1c
items_per_chunk = 65536
item_size = 24
item_object = 0x8

[ue.layout.uobject]
flags = 0x8
index = 0xc
class = 0x10
name = 0x18
outer = 0x20
cdo_flag = 0x10

[ue.layout.ustruct]
super_struct = 0x40
child_properties = 0x50
properties_size = 0x58

[ue.layout.ffield]
class_private = 0x8
class_name = 0x0
next = 0x18
name = 0x20

[ue.layout.fprop]
element_size = 0x34
offset = 0x44

[ue.layout.fbool]
field_size = 0x70
byte_offset = 0x71
byte_mask = 0x72
field_mask = 0x73

[ue.layout.fstruct]
struct_ptr = 0x70
"#;

    const UE_PLAYER: &str = r#"
[ue.player]
controller_class = "/Script/Maine.SurvivalPlayerController"
pawn_field = "Pawn"
pawn_class = "/Script/Maine.SurvivalPlayerCharacter"
alt_pawn_fields = []
"#;

    fn ue_generic() -> String {
        format!("{GENERIC}{UE_PATTERNS}{UE_LAYOUT}{UE_PLAYER}")
    }

    /// Erro de carga: (arquivo, campo).
    fn load_err(files: &[(&str, &str)]) -> (String, String) {
        match parse(files) {
            Err(Error::Signature {
                file,
                field,
                reason,
            }) => {
                assert!(!reason.is_empty());
                (file, field)
            }
            other => panic!("esperava Error::Signature, veio {other:?}"),
        }
    }

    #[test]
    fn ue_block_loads_and_version_rva() {
        let version =
            "version = \"aaaa-bbbb\"\n[ue]\ngobjects_rva = 0xa53bc30\nnames_rva = 0xa499e50\n";
        let generic = ue_generic();
        let sigs = parse(&[("generic.toml", &generic), ("aaaa-bbbb.toml", version)]).unwrap();

        let ue = sigs.ue_for_version("cccc-dddd").unwrap();
        assert_eq!(ue.layout, gt_ue::UeLayout::observed());
        assert_eq!(ue.gobjects.rva, None);
        assert_eq!(ue.names.rva, None);
        assert_eq!(ue.gobjects.pattern.disp_offset, 3);
        assert_eq!(ue.gobjects.pattern.insn_end, 7);
        assert_eq!(ue.gobjects.pattern.adjust, 0);
        assert_eq!(ue.gobjects.pattern.pattern.len(), 15);
        assert_eq!(ue.names.pattern.disp_offset, 14);
        assert_eq!(ue.names.pattern.insn_end, 18);
        assert_eq!(ue.names.pattern.adjust, 0x10);
        assert_eq!(ue.names.pattern.pattern.len(), 19);
        assert_eq!(
            ue.player,
            PlayerCfg {
                controller_class: "/Script/Maine.SurvivalPlayerController".into(),
                pawn_field: "Pawn".into(),
                pawn_class: "/Script/Maine.SurvivalPlayerCharacter".into(),
                alt_pawn_fields: vec![],
            }
        );

        let versioned = sigs.ue_for_version("aaaa-bbbb").unwrap();
        assert_eq!(versioned.gobjects.rva, Some(0xa53bc30));
        assert_eq!(versioned.names.rva, Some(0xa499e50));
        assert_eq!(versioned.gobjects.pattern, ue.gobjects.pattern);
        assert_eq!(versioned.layout, ue.layout);

        // `[ue]` da versão com só uma global: a outra fica sem `rva`.
        let partial = "version = \"eeee-ffff\"\n[ue]\nnames_rva = 0x10\n";
        let sigs = parse(&[("generic.toml", &generic), ("eeee-ffff.toml", partial)]).unwrap();
        let ue = sigs.ue_for_version("eeee-ffff").unwrap();
        assert_eq!((ue.gobjects.rva, ue.names.rva), (None, Some(0x10)));

        // Sem `[ue]` no genérico não há reflexão.
        let sigs = parse(&[("generic.toml", GENERIC)]).unwrap();
        assert!(sigs.ue_for_version("aaaa-bbbb").is_none());
        // Versão sem bloco `[ue]` usa o genérico.
        let plain = "version = \"gggg-hhhh\"\n";
        let sigs = parse(&[("generic.toml", &generic), ("gggg-hhhh.toml", plain)]).unwrap();
        assert_eq!(sigs.ue_for_version("gggg-hhhh").unwrap().gobjects.rva, None);
    }

    #[test]
    fn ue_layout_override_replaces_block() {
        let generic = ue_generic();
        let layout = UE_LAYOUT
            .replace(
                "[ue.layout.uobject]\nflags = 0x8",
                "[ue.layout.uobject]\nflags = 0x9",
            )
            .replace("item_size = 24", "item_size = 32");
        let version = format!("version = \"aaaa-bbbb\"\n[ue]\ngobjects_rva = 1\n{layout}");
        let sigs = parse(&[("generic.toml", &generic), ("aaaa-bbbb.toml", &version)]).unwrap();
        let ue = sigs.ue_for_version("aaaa-bbbb").unwrap();
        assert_eq!(ue.layout.uobject.flags, 0x9);
        assert_eq!(ue.layout.objects.item_size, 32);
        assert_eq!(ue.gobjects.rva, Some(1));
        // Outras versões continuam com o genérico.
        let other = sigs.ue_for_version("outra").unwrap();
        assert_eq!(other.layout, gt_ue::UeLayout::observed());

        // O bloco substitui inteiro: tabela faltando na versão é erro, não herança.
        let partial = "version = \"aaaa-bbbb\"\n[ue.layout.objects]\nchunks = 0\nmax_elements = 0x10\n\
                       num_elements = 0x14\nmax_chunks = 0x18\nnum_chunks = 0x1c\n\
                       items_per_chunk = 65536\nitem_size = 24\nitem_object = 8\n";
        let (file, _) = load_err(&[("generic.toml", &generic), ("aaaa-bbbb.toml", partial)]);
        assert_eq!(file, "aaaa-bbbb.toml");
    }

    #[test]
    fn ue_load_errors() {
        let generic_ok = ue_generic();
        let gen_with = |ue: String| format!("{GENERIC}{ue}{UE_LAYOUT}{UE_PLAYER}");
        let bad_patterns = |from: &str, to: &str| gen_with(UE_PATTERNS.replace(from, to));
        type Case<'a> = (&'a str, Vec<(&'a str, String)>, &'a str, &'a str);
        let cases: Vec<Case> = vec![
            (
                "padrão inválido",
                vec![("generic.toml", bad_patterns("48 8b 05 ??", "48 8b 0g ??"))],
                "generic.toml",
                "ue.gobjects.pattern",
            ),
            (
                "disp fora do padrão",
                vec![(
                    "generic.toml",
                    bad_patterns("disp_offset = 14", "disp_offset = 17"),
                )],
                "generic.toml",
                "ue.names.disp_offset",
            ),
            (
                "fim da instrução depois do padrão",
                vec![(
                    "generic.toml",
                    bad_patterns("insn_end = 18", "insn_end = 20"),
                )],
                "generic.toml",
                "ue.names.insn_end",
            ),
            (
                "fim da instrução antes do disp",
                vec![("generic.toml", bad_patterns("insn_end = 7", "insn_end = 6"))],
                "generic.toml",
                "ue.gobjects.insn_end",
            ),
            (
                "chave desconhecida",
                vec![(
                    "generic.toml",
                    bad_patterns("adjust = 0 }", "adjust = 0, x = 1 }"),
                )],
                "generic.toml",
                "toml",
            ),
            (
                "layout sem tabela",
                vec![("generic.toml", format!("{GENERIC}{UE_PATTERNS}{UE_PLAYER}"))],
                "generic.toml",
                "toml",
            ),
            (
                "classe sem pacote",
                vec![(
                    "generic.toml",
                    generic_ok.replace(
                        "/Script/Maine.SurvivalPlayerController",
                        "SurvivalPlayerController",
                    ),
                )],
                "generic.toml",
                "ue.player.controller_class",
            ),
            (
                "campo do pawn vazio",
                vec![(
                    "generic.toml",
                    generic_ok.replace("pawn_field = \"Pawn\"", "pawn_field = \" \""),
                )],
                "generic.toml",
                "ue.player.pawn_field",
            ),
            (
                "tamanho de item zero",
                vec![(
                    "generic.toml",
                    generic_ok.replace("item_size = 24", "item_size = 0"),
                )],
                "generic.toml",
                "ue.layout.objects.item_size",
            ),
            (
                "[ue] da versão sem [ue] no genérico",
                vec![
                    ("generic.toml", GENERIC.to_string()),
                    (
                        "aaaa-bbbb.toml",
                        "version = \"aaaa-bbbb\"\n[ue]\nnames_rva = 1\n".to_string(),
                    ),
                ],
                "aaaa-bbbb.toml",
                "ue",
            ),
        ];
        for (what, files, file, field) in cases {
            let borrowed: Vec<(&str, &str)> = files.iter().map(|(n, c)| (*n, c.as_str())).collect();
            let (f, fl) = load_err(&borrowed);
            assert_eq!((f.as_str(), fl.as_str()), (file, field), "caso: {what}");
        }
    }

    const LOCK_FIELD: &str = r#"
[[field]]
id = "stamina-infinita"
name = "Stamina infinita"
category = "sobrevivencia"
description = "A stamina não acaba"
object = "StaminaComponent"
object_class = "/Script/Maine.StaminaComponent"
rule = "lock"
field = "CurrentStamina"
kind = "float"
mode = "raise"
target_field = "MaxStamina"
min = 0
max = 10000.0
"#;

    const BITS_FIELD: &str = r#"
[[field]]
id = "nao-morrer"
name = "Não morrer"
category = "sobrevivencia"
description = "Não morre"
object = "HealthComponent"
object_class = "/Script/Maine.HealthComponent"
rule = "clear_bits"
fields = ["bCanDieFromDamage", "bCanDieEver"]
kind = "bool"
"#;

    #[test]
    fn field_block_lock_and_bits() {
        let const_lock = LOCK_FIELD
            .replace("stamina-infinita", "vida-infinita")
            .replace("target_field = \"MaxStamina\"", "target = 0")
            .replace("mode = \"raise\"", "mode = \"exact\"")
            .replace("object = \"StaminaComponent\"", "object = \"pawn\"");
        let generic = format!("{GENERIC}{LOCK_FIELD}{BITS_FIELD}{const_lock}");
        let sigs = parse(&[("generic.toml", &generic)]).unwrap();
        let fields = sigs.fields();
        assert_eq!(
            fields.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
            vec!["stamina-infinita", "nao-morrer", "vida-infinita"]
        );
        assert_eq!(
            fields[0],
            FieldDef {
                id: "stamina-infinita".into(),
                name: "Stamina infinita".into(),
                category: Category::Sobrevivencia,
                description: "A stamina não acaba".into(),
                object: "StaminaComponent".into(),
                object_class: "/Script/Maine.StaminaComponent".into(),
                rule: FieldRule::Lock {
                    field: "CurrentStamina".into(),
                    mode: LockMode::Raise,
                    target: LockTarget::Field("MaxStamina".into()),
                    min: 0.0,
                    max: 10000.0,
                },
            }
        );
        assert_eq!(
            fields[1].rule,
            FieldRule::ClearBits {
                fields: vec!["bCanDieFromDamage".into(), "bCanDieEver".into()]
            }
        );
        assert_eq!(fields[2].object, "pawn");
        assert!(matches!(
            fields[2].rule,
            FieldRule::Lock { mode: LockMode::Exact, target: LockTarget::Const(t), .. } if t == 0.0
        ));
        // Os `[[field]]` não entram nos patches.
        assert_eq!(sigs.ids(), vec!["teste-codificacao", "outro"]);
        assert_eq!(sigs.for_version("x").len(), 2);
        // Os modos `lower` e `raise` carregam.
        let lower = LOCK_FIELD.replace("raise", "lower");
        let sigs = parse(&[("generic.toml", &lower)]).unwrap();
        assert!(matches!(
            sigs.fields()[0].rule,
            FieldRule::Lock {
                mode: LockMode::Lower,
                ..
            }
        ));
        // Sem nenhum `[[field]]` a lista é vazia.
        assert!(
            parse(&[("generic.toml", GENERIC)])
                .unwrap()
                .fields()
                .is_empty()
        );
    }

    #[test]
    fn field_id_collides_with_patch() {
        let generic = format!(
            "{GENERIC}{}",
            LOCK_FIELD.replace("stamina-infinita", "outro")
        );
        assert_eq!(
            load_err(&[("generic.toml", &generic)]),
            ("generic.toml".into(), "id".into())
        );
        // Entre dois `[[field]]` também.
        let generic = format!("{LOCK_FIELD}{LOCK_FIELD}");
        assert_eq!(
            load_err(&[("generic.toml", &generic)]),
            ("generic.toml".into(), "id".into())
        );
    }

    #[test]
    fn field_target_and_target_field_both_is_error() {
        let both = LOCK_FIELD.replace("min = 0", "target = 5\nmin = 0");
        assert_eq!(
            load_err(&[("generic.toml", &both)]),
            ("generic.toml".into(), "target".into())
        );
        let none = LOCK_FIELD.replace("target_field = \"MaxStamina\"\n", "");
        assert_eq!(
            load_err(&[("generic.toml", &none)]),
            ("generic.toml".into(), "target".into())
        );
    }

    #[test]
    fn field_min_gt_max_is_error() {
        let bad = LOCK_FIELD.replace("min = 0", "min = 20000");
        assert_eq!(
            load_err(&[("generic.toml", &bad)]),
            ("generic.toml".into(), "max".into())
        );
    }

    #[test]
    fn unknown_field_key_is_error() {
        let bad = format!("{LOCK_FIELD}extra = 1\n");
        assert_eq!(
            load_err(&[("generic.toml", &bad)]),
            ("generic.toml".into(), "toml".into())
        );
    }

    #[test]
    fn field_load_rule_errors() {
        let cases: Vec<(&str, String, &str)> = vec![
            (
                "sem name",
                LOCK_FIELD.replace("name = \"Stamina infinita\"\n", ""),
                "name",
            ),
            (
                "name vazio",
                LOCK_FIELD.replace("\"Stamina infinita\"", "\" \""),
                "name",
            ),
            (
                "sem category",
                LOCK_FIELD.replace("category = \"sobrevivencia\"\n", ""),
                "category",
            ),
            (
                "category inválida",
                LOCK_FIELD.replace("\"sobrevivencia\"", "\"extras\""),
                "category",
            ),
            (
                "rule desconhecida",
                LOCK_FIELD.replace("\"lock\"", "\"set\""),
                "rule",
            ),
            (
                "object vazio",
                LOCK_FIELD.replace("object = \"StaminaComponent\"", "object = \"\""),
                "object",
            ),
            (
                "object_class sem pacote",
                LOCK_FIELD.replace("/Script/Maine.StaminaComponent", "StaminaComponent"),
                "object_class",
            ),
            (
                "lock sem field",
                LOCK_FIELD.replace("field = \"CurrentStamina\"\n", ""),
                "field",
            ),
            (
                "lock sem kind",
                LOCK_FIELD.replace("kind = \"float\"\n", ""),
                "kind",
            ),
            (
                "lock com kind bool",
                LOCK_FIELD.replace("\"float\"", "\"bool\""),
                "kind",
            ),
            (
                "lock sem mode",
                LOCK_FIELD.replace("mode = \"raise\"\n", ""),
                "mode",
            ),
            (
                "mode inválido",
                LOCK_FIELD.replace("\"raise\"", "\"set\""),
                "mode",
            ),
            ("lock sem min", LOCK_FIELD.replace("min = 0\n", ""), "min"),
            (
                "lock sem max",
                LOCK_FIELD.replace("max = 10000.0\n", ""),
                "max",
            ),
            (
                "min não finito",
                LOCK_FIELD.replace("min = 0", "min = nan"),
                "min",
            ),
            (
                "max infinito",
                LOCK_FIELD.replace("max = 10000.0", "max = inf"),
                "max",
            ),
            (
                "max texto",
                LOCK_FIELD.replace("max = 10000.0", "max = \"x\""),
                "max",
            ),
            (
                "target não finito",
                LOCK_FIELD.replace("target_field = \"MaxStamina\"", "target = nan"),
                "target",
            ),
            (
                "target fora da faixa",
                LOCK_FIELD.replace("target_field = \"MaxStamina\"", "target = 20000"),
                "target",
            ),
            (
                "target_field vazio",
                LOCK_FIELD.replace("\"MaxStamina\"", "\"\""),
                "target_field",
            ),
            (
                "lock com fields",
                LOCK_FIELD.replace("min = 0", "fields = [\"a\"]\nmin = 0"),
                "fields",
            ),
            (
                "bits sem fields",
                BITS_FIELD.replace("fields = [\"bCanDieFromDamage\", \"bCanDieEver\"]\n", ""),
                "fields",
            ),
            (
                "bits com fields vazio",
                BITS_FIELD.replace("[\"bCanDieFromDamage\", \"bCanDieEver\"]", "[]"),
                "fields",
            ),
            (
                "bits com campo vazio",
                BITS_FIELD.replace("\"bCanDieEver\"", "\"\""),
                "fields",
            ),
            (
                "bits com campo repetido",
                BITS_FIELD.replace("\"bCanDieEver\"", "\"bCanDieFromDamage\""),
                "fields",
            ),
            (
                "bits sem kind",
                BITS_FIELD.replace("kind = \"bool\"\n", ""),
                "kind",
            ),
            (
                "bits com kind float",
                BITS_FIELD.replace("\"bool\"", "\"float\""),
                "kind",
            ),
            (
                "bits com mode",
                format!("{BITS_FIELD}mode = \"raise\"\n"),
                "mode",
            ),
            ("bits com min", format!("{BITS_FIELD}min = 0\n"), "min"),
            (
                "bits com target",
                format!("{BITS_FIELD}target = 0\n"),
                "target",
            ),
            (
                "bits com field",
                format!("{BITS_FIELD}field = \"x\"\n"),
                "field",
            ),
        ];
        for (what, text, field) in cases {
            let (file, got) = load_err(&[("generic.toml", &text)]);
            assert_eq!(
                (file.as_str(), got.as_str()),
                ("generic.toml", field),
                "caso: {what}"
            );
        }
    }

    #[test]
    fn real_files_ue_block() {
        let sigs = Signatures::embedded().unwrap();
        // Genérico (versão sem arquivo): layout, padrões e jogador; sem `rva`.
        let ue = sigs.ue_for_version("cccc-dddd").unwrap();
        assert_eq!(ue.layout, gt_ue::UeLayout::observed());
        assert_eq!((ue.gobjects.rva, ue.names.rva), (None, None));
        assert_eq!(ue.gobjects.pattern.disp_offset, 3);
        assert_eq!(ue.gobjects.pattern.insn_end, 7);
        assert_eq!(ue.names.pattern.disp_offset, 14);
        assert_eq!(ue.names.pattern.insn_end, 18);
        assert_eq!(ue.names.pattern.adjust, 0x10);
        assert_eq!(
            ue.player.controller_class,
            "/Script/Maine.SurvivalPlayerController"
        );
        assert_eq!(ue.player.pawn_field, "Pawn");
        assert_eq!(
            ue.player.pawn_class,
            "/Script/Maine.SurvivalPlayerCharacter"
        );
        // Montado, o `Pawn` é a montaria (P7): o campo alternativo leva ao personagem.
        assert_eq!(
            ue.player.alt_pawn_fields,
            vec!["CachedSurvivalPlayerCharacter"]
        );
        // Build verificada: o `rva` é o da global resolvida (depois do `adjust`).
        let ue = sigs.ue_for_version("c935e799-0b27c000").unwrap();
        assert_eq!(ue.gobjects.rva, Some(0xa53bc30));
        assert_eq!(ue.names.rva, Some(0xa499e50));
    }
}
