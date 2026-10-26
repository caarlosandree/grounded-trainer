//! Assinaturas dos patches: leitura dos `signatures/*.toml` e regra de carga (spec 002, 6.3).
//!
//! `generic.toml` define cada patch para qualquer versão do jogo; `<chave>.toml` registra o que
//! foi verificado numa build (o `rva`) e pode sobrescrever `pattern`, `offset`, `expected` e
//! `replacement`. Os arquivos são embutidos na compilação.

use std::collections::{BTreeMap, HashSet};

use gt_core::scan::Pattern;
use gt_core::value::{ValueSpec, ValueType};
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
    Itens,
    Progressao,
    Movimento,
    Diagnostico,
}

impl Category {
    /// Todas as categorias, na ordem de exibição.
    pub const ALL: [Category; 5] = [
        Category::Sobrevivencia,
        Category::Itens,
        Category::Progressao,
        Category::Movimento,
        Category::Diagnostico,
    ];

    /// Rótulo mostrado como cabeçalho da categoria.
    pub fn label(self) -> &'static str {
        match self {
            Category::Sobrevivencia => "SOBREVIVÊNCIA",
            Category::Itens => "ITENS",
            Category::Progressao => "PROGRESSÃO",
            Category::Movimento => "MOVIMENTO",
            Category::Diagnostico => "DIAGNÓSTICO",
        }
    }

    /// Lê o nome usado no TOML (`sobrevivencia`, `itens`, `progressao`, `movimento`,
    /// `diagnostico`).
    pub fn parse(text: &str) -> Option<Category> {
        match text {
            "sobrevivencia" => Some(Category::Sobrevivencia),
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

/// Conjunto das assinaturas carregadas: o genérico e uma lista de definições por versão.
#[derive(Debug, Clone)]
pub struct Signatures {
    generic: Vec<PatchDef>,
    versions: BTreeMap<String, VersionSigs>,
}

#[derive(Debug, Clone)]
struct VersionSigs {
    product_version: Option<String>,
    defs: Vec<PatchDef>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GenericFile {
    #[serde(default)]
    patch: Vec<GenericPatch>,
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

/// Aplica a regra de carga (6.3) a uma definição em texto.
fn build(file: &str, raw: RawDef) -> Result<PatchDef> {
    let name = match raw.name {
        None => return Err(sig_err(file, "name", "campo obrigatório ausente")),
        Some(n) if n.trim().is_empty() => return Err(sig_err(file, "name", "vazio")),
        Some(n) => n,
    };
    let category = match raw.category.as_deref() {
        None => return Err(sig_err(file, "category", "campo obrigatório ausente")),
        Some(text) => Category::parse(text).ok_or_else(|| {
            sig_err(
                file,
                "category",
                format!(
                    "`{text}` não é sobrevivencia, itens, progressao, movimento nem diagnostico"
                ),
            )
        })?,
    };
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
            if versions
                .insert(
                    key.to_string(),
                    VersionSigs {
                        product_version: file.product_version,
                        defs,
                    },
                )
                .is_some()
            {
                return Err(sig_err(name, "arquivo", "repetido"));
            }
        }
        Ok(Signatures { generic, versions })
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
                "ITENS",
                "PROGRESSÃO",
                "MOVIMENTO",
                "DIAGNÓSTICO"
            ]
        );
        for (text, cat) in [
            ("sobrevivencia", Category::Sobrevivencia),
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
}
