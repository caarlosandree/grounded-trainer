//! Assinaturas dos patches: leitura dos `signatures/*.toml` e regra de carga (spec 002, 6.3).
//!
//! `generic.toml` define cada patch para qualquer versão do jogo; `<chave>.toml` registra o que
//! foi verificado numa build (o `rva`) e pode sobrescrever `pattern`, `offset`, `expected` e
//! `replacement`. Os arquivos são embutidos na compilação.

use std::collections::{BTreeMap, HashSet};

use gt_core::scan::Pattern;
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

/// Definição de um patch, já validada pela regra de carga.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchDef {
    pub id: String,
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
    description: String,
    module: Option<String>,
    pattern: String,
    offset: usize,
    expected: String,
    replacement: String,
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
    pattern: Option<String>,
    offset: Option<usize>,
    expected: Option<String>,
    replacement: Option<String>,
}

/// Definição ainda em texto, antes de validar (o `<chave>.toml` sobrescreve campos em texto).
#[derive(Clone)]
struct RawDef {
    id: String,
    description: String,
    module: Option<String>,
    pattern: String,
    offset: usize,
    expected: String,
    replacement: String,
    rva: Option<u64>,
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

/// Aplica a regra de carga (6.3) a uma definição em texto.
fn build(file: &str, raw: RawDef) -> Result<PatchDef> {
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
    Ok(PatchDef {
        id: raw.id,
        description: raw.description,
        module,
        pattern,
        offset: raw.offset,
        expected,
        replacement,
        rva: raw.rva,
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
                description: p.description,
                module: p.module,
                pattern: p.pattern,
                offset: p.offset,
                expected: p.expected,
                replacement: p.replacement,
                rva: None,
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
description = "Troca sem efeito"
pattern = "48 8B F9 ?? 90"
offset = 0
expected = "48 8B F9"
replacement = "48 89 CF"

[[patch]]
id = "outro"
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
            format!("[[patch]]\nid = \"p\"\ndescription = \"d\"\n{patch}")
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
            "[[patch]]\nid = \"p\"\ndescription = \"d\"\nmodule = \"x\"\npattern = \"48\"\noffset = 0\nexpected = \"48\"\nreplacement = \"49\"\n",
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
}
