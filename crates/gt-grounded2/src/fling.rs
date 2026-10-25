//! Leitura do `symbols.ini` que o trainer FLiNG grava para cada versão do jogo.
//!
//! O arquivo fica em UTF-16LE com BOM, com quebras CRLF, e os símbolos ficam dentro da seção
//! `[default]`. Cada símbolo é `nome=Grounded2Steam-Win64-Shipping.exe+OFFSET`.

use std::path::{Path, PathBuf};

use gt_core::addr::{AddrSpec, parse_addr};

use crate::{APP_ID, FLING_GAME_DIR};

/// Nome da seção que envolve os símbolos. É a única seção aceita.
const SECTION: &str = "[default]";

/// Nome do arquivo de símbolos dentro da pasta de cada versão.
const INI_NAME: &str = "symbols.ini";

/// Quantidade de bytes comparados por [`is_changed`], a partir do início do buffer.
pub const CHANGE_WINDOW: usize = 16;

/// Um símbolo do `symbols.ini`: nome, módulo e deslocamento a partir da base do módulo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlingSymbol {
    pub name: String,
    pub module: String,
    pub offset: u64,
}

/// Erro ao ler o `symbols.ini` ou localizar a pasta de versão do FLiNG.
#[derive(Debug, thiserror::Error)]
pub enum FlingError {
    /// O arquivo não começa com o BOM `FF FE` de UTF-16LE, ou não decodifica.
    #[error("symbols.ini não está em UTF-16LE com BOM (FF FE)")]
    NotUtf16,

    /// Linha que não é `nome=módulo+offset`, com o offset em hexadecimal.
    #[error("linha {line} do symbols.ini inválida: {text}")]
    BadLine { line: usize, text: String },

    /// Nenhuma pasta de versão do FLiNG tem o `TimeDateStamp` pedido.
    #[error(
        "nenhuma pasta de versão do FLiNG corresponde ao TimeDateStamp {wanted}; pastas encontradas: {found:?}"
    )]
    NoMatchingVersion { wanted: u32, found: Vec<String> },

    /// Erro de entrada e saída ao ler a pasta do FLiNG.
    #[error("erro de E/S: {0}")]
    Io(#[from] std::io::Error),
}

/// Interpreta o conteúdo bruto de um `symbols.ini` do FLiNG.
pub fn parse_symbols_ini(raw: &[u8]) -> Result<Vec<FlingSymbol>, FlingError> {
    let body = raw
        .strip_prefix(&[0xFF, 0xFE])
        .ok_or(FlingError::NotUtf16)?;
    if body.len() % 2 != 0 {
        return Err(FlingError::NotUtf16);
    }
    let (pairs, _) = body.as_chunks::<2>();
    let units: Vec<u16> = pairs.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
    let text = String::from_utf16(&units).map_err(|_| FlingError::NotUtf16)?;

    let mut symbols = Vec::new();
    for (index, raw_line) in text.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with(';') || line.eq_ignore_ascii_case(SECTION) {
            continue;
        }
        let bad = || FlingError::BadLine {
            line: index + 1,
            text: line.to_string(),
        };
        let (name, value) = line.split_once('=').ok_or_else(bad)?;
        let name = name.trim();
        if name.is_empty() {
            return Err(bad());
        }
        match parse_addr(value.trim()) {
            Ok(AddrSpec::ModuleOffset {
                module: Some(module),
                offset,
            }) => symbols.push(FlingSymbol {
                name: name.to_string(),
                module,
                offset,
            }),
            _ => return Err(bad()),
        }
    }
    Ok(symbols)
}

/// Pasta de versões do trainer FLiNG dentro da biblioteca da Steam (`steamapps`).
pub fn version_dir(library: &Path) -> PathBuf {
    library
        .join("compatdata")
        .join(APP_ID.to_string())
        .join("pfx/drive_c/users/steamuser/AppData/Local/FLiNGTrainer/trainer-logs")
        .join(FLING_GAME_DIR)
        .join("version")
}

/// Caminho do `symbols.ini` da versão com o `TimeDateStamp` dado.
///
/// Falha com [`FlingError::NoMatchingVersion`] se a pasta da versão não existir.
pub fn locate_ini(library: &Path, time_date_stamp: u32) -> Result<PathBuf, FlingError> {
    let versions = version_dir(library);
    let ini = versions.join(time_date_stamp.to_string()).join(INI_NAME);
    if ini.is_file() {
        return Ok(ini);
    }
    let mut found = Vec::new();
    match std::fs::read_dir(&versions) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    found.push(entry.file_name().to_string_lossy().into_owned());
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    found.sort();
    Err(FlingError::NoMatchingVersion {
        wanted: time_date_stamp,
        found,
    })
}

/// Indica se a pasta-mãe do `symbols.ini` não é o `TimeDateStamp` em decimal.
pub fn ini_version_mismatch(ini: &Path, time_date_stamp: u32) -> bool {
    let expected = time_date_stamp.to_string();
    ini.parent()
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
        != Some(expected.as_str())
}

/// Indica se a memória difere do disco nos primeiros [`CHANGE_WINDOW`] bytes.
///
/// Se um dos lados tiver menos bytes que a janela, compara só o prefixo comum.
pub fn is_changed(memory: &[u8], disk: &[u8]) -> bool {
    let n = CHANGE_WINDOW.min(memory.len()).min(disk.len());
    memory[..n] != disk[..n]
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &[u8] = include_bytes!("../tests/fixtures/symbols.ini");

    /// Monta um `symbols.ini` em UTF-16LE com BOM a partir de texto.
    fn utf16le_bom(text: &str) -> Vec<u8> {
        let mut out = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out
    }

    #[test]
    fn parses_real_file() {
        let s = parse_symbols_ini(REAL).unwrap();
        assert_eq!(s.len(), 11);
        let god = s.iter().find(|x| x.name == "aobgodmode").unwrap();
        assert_eq!(
            (god.module.as_str(), god.offset),
            ("Grounded2Steam-Win64-Shipping.exe", 0x56FC11C)
        );
        assert!(
            s.iter()
                .any(|x| x.name == "aobcharges" && x.offset == 0x57BD6D0)
        );
    }

    #[test]
    fn rejects_without_bom() {
        assert!(matches!(
            parse_symbols_ini(b"[default]\r\n"),
            Err(FlingError::NotUtf16)
        ));
    }

    #[test]
    fn accepts_lf_comments_and_blank_lines() {
        let text =
            "[default]\n; comentário\n\naobspeed=Grounded2Steam-Win64-Shipping.exe+37EEE9B\n";
        let s = parse_symbols_ini(&utf16le_bom(text)).unwrap();
        assert_eq!(
            s,
            vec![FlingSymbol {
                name: "aobspeed".into(),
                module: "Grounded2Steam-Win64-Shipping.exe".into(),
                offset: 0x37EEE9B,
            }]
        );
    }

    #[test]
    fn rejects_value_without_module() {
        let text = "[default]\r\nx=0x1456FC11C\r\n";
        match parse_symbols_ini(&utf16le_bom(text)) {
            Err(FlingError::BadLine { line, text }) => {
                assert_eq!(line, 2);
                assert_eq!(text, "x=0x1456FC11C");
            }
            other => panic!("esperava BadLine, veio {other:?}"),
        }
    }

    #[test]
    fn rejects_offset_only_value() {
        let text = "[default]\r\nx=+56FC11C\r\n";
        assert!(matches!(
            parse_symbols_ini(&utf16le_bom(text)),
            Err(FlingError::BadLine { line: 2, .. })
        ));
    }

    #[test]
    fn decimal_folder_name() {
        let dir = tempfile::tempdir().unwrap();
        let v = version_dir(dir.path()).join("3375753113");
        std::fs::create_dir_all(&v).unwrap();
        std::fs::write(v.join("symbols.ini"), REAL).unwrap();
        assert!(locate_ini(dir.path(), 0xc935e799).is_ok());
        assert!(matches!(
            locate_ini(dir.path(), 1),
            Err(FlingError::NoMatchingVersion { ref found, .. }) if found == &vec!["3375753113".to_string()]
        ));
    }

    #[test]
    fn version_dir_path_layout() {
        let dir = version_dir(Path::new("/lib"));
        assert_eq!(
            dir,
            Path::new(
                "/lib/compatdata/2661300/pfx/drive_c/users/steamuser/AppData/Local/FLiNGTrainer/trainer-logs/grounded-2-trainer/version"
            )
        );
    }

    #[test]
    fn ini_mismatch_compares_decimal_folder() {
        let ok = Path::new("/x/version/3375753113/symbols.ini");
        let other = Path::new("/x/version/1/symbols.ini");
        assert!(!ini_version_mismatch(ok, 0xc935e799));
        assert!(ini_version_mismatch(other, 0xc935e799));
    }

    #[test]
    fn change_window() {
        let disk = [0u8; 32];
        let mut mem = disk;
        mem[15] = 1;
        assert!(is_changed(&mem, &disk));
        mem[15] = 0;
        mem[16] = 1;
        assert!(!is_changed(&mem, &disk));
    }
}
