//! `gt dump (<seção> | <endereço> <tamanho>) [-o arquivo]`: salva memória em arquivo (spec 6.8).

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use gt_core::addr::{parse_addr, parse_size};
use gt_core::guard;
use gt_core::modules::require_trusted;
use serde::Serialize;

use super::Global;
use crate::context::Target;
use crate::output::emit;

/// Limite de um dump: 1 GiB.
const MAX_SIZE: u64 = 1 << 30;

/// Opções do `dump` além das globais.
pub struct Options<'a> {
    /// Nome de uma seção, ou o endereço inicial quando `size` existe.
    pub target: &'a str,
    pub size: Option<&'a str>,
    pub output: Option<&'a Path>,
    pub force: bool,
}

/// Conteúdo do `.json` gravado ao lado do `.bin`.
///
/// `base` e `version_key` são `null` quando o alvo não tem um módulo PE em `--module`.
#[derive(Serialize)]
struct Sidecar {
    base: Option<String>,
    start: String,
    size: u64,
    version_key: Option<String>,
    holes: Vec<HoleOutput>,
}

#[derive(Serialize)]
struct HoleOutput {
    start: String,
    end: String,
}

/// Saída `--json` do comando.
#[derive(Serialize)]
struct DumpOutput {
    bin: String,
    json: String,
    start: String,
    size: u64,
    version_key: Option<String>,
    holes: Vec<HoleOutput>,
}

/// Intervalo a gravar e o nome-base padrão do arquivo.
struct Range {
    start: u64,
    size: u64,
    name: String,
}

pub fn run(global: &Global, opts: &Options<'_>) -> anyhow::Result<()> {
    // Valida o que não depende do processo antes de abri-lo.
    let literal = match opts.size {
        Some(size) => {
            let spec = parse_addr(opts.target)?;
            let size = parse_size(size)?;
            check_size(size)?;
            Some((spec, size))
        }
        None => None,
    };

    let target = Target::open(global.pid)?;
    // Sem módulo PE em `--module` (alvo ELF, por exemplo) o dump por intervalo continua valendo.
    let module = match target.module(&global.module) {
        Ok(found) => Some(found),
        Err(err) if literal.is_some() => {
            tracing::debug!(error = %err, "dump por intervalo sem módulo PE");
            None
        }
        Err(err) => return Err(err),
    };
    let base = module.as_ref().map(|(m, _)| m.base);
    let version_key = module.as_ref().map(|(_, headers)| headers.version_key());

    let range = match literal {
        Some((spec, size)) => {
            let start = target.resolve(&spec, &global.module)?;
            Range {
                start,
                size,
                name: format!("{start:x}"),
            }
        }
        None => {
            let (module, headers) = module.ok_or_else(|| anyhow!("módulo sem cabeçalho PE"))?;
            let name = opts.target;
            let section = headers
                .section(name)
                .or_else(|| headers.section(&format!(".{name}")))
                .ok_or_else(|| {
                    anyhow!("seção {name} não encontrada; `gt inspect` lista as seções")
                })?;
            if section.is_executable() {
                require_trusted(module)?;
            }
            let size = u64::from(section.virtual_size);
            if size == 0 {
                bail!("a seção {} está vazia", section.name);
            }
            check_size(size)?;
            Range {
                start: module.base.saturating_add(u64::from(section.rva)),
                size,
                name: section
                    .name
                    .trim_start_matches('.')
                    .replace(['/', '\\'], "_"),
            }
        }
    };
    if range.size == 0 {
        bail!("o tamanho precisa ser maior que zero");
    }

    let (bin_path, json_path) =
        destination(opts.output, &range.name, version_key.as_deref(), target.pid)?;
    if !opts.force {
        for path in [&bin_path, &json_path] {
            if path.exists() {
                bail!(
                    "{} já existe; use --force para sobrescrever",
                    path.display()
                );
            }
        }
    }

    guard::check(
        &target.maps,
        &target.modules,
        &target.non_pe,
        range.start,
        range.size,
    )?;
    // Cabe em `usize`: `size <= MAX_SIZE`.
    let read = target.reader.read_lossy(range.start, range.size as usize)?;

    let holes: Vec<HoleOutput> = read
        .holes
        .iter()
        .map(|hole| HoleOutput {
            start: format!("{:#x}", hole.start),
            end: format!("{:#x}", hole.end),
        })
        .collect();
    let sidecar = Sidecar {
        base: base.map(|b| format!("{b:#x}")),
        start: format!("{:#x}", range.start),
        size: range.size,
        version_key: version_key.clone(),
        holes,
    };
    if let Some(parent) = bin_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .with_context(|| format!("não foi possível criar {}", parent.display()))?;
    }
    write_file(&bin_path, &read.bytes, opts.force)?;
    let mut json = serde_json::to_vec_pretty(&sidecar).context("falha ao gerar o JSON")?;
    json.push(b'\n');
    write_file(&json_path, &json, opts.force)?;

    let output = DumpOutput {
        bin: bin_path.display().to_string(),
        json: json_path.display().to_string(),
        start: sidecar.start,
        size: range.size,
        version_key,
        holes: sidecar.holes,
    };
    emit(
        global.json,
        |out| {
            writeln!(
                out,
                "gravados {} bytes de {} em {}",
                output.size,
                target.label(range.start),
                output.bin
            )?;
            writeln!(out, "metadados em {}", output.json)?;
            if !output.holes.is_empty() {
                writeln!(
                    out,
                    "aviso: {} lacuna(s) ilegível(is) gravadas como zeros (veja o .json)",
                    output.holes.len()
                )?;
            }
            Ok(())
        },
        &output,
    )
}

fn check_size(size: u64) -> anyhow::Result<()> {
    if size > MAX_SIZE {
        bail!("tamanho acima do limite de 1 GiB");
    }
    Ok(())
}

/// Caminhos do `.bin` e do `.json`. Padrão: `scratch/dumps/<versão | pid<PID>>/<nome>.bin`.
fn destination(
    output: Option<&Path>,
    name: &str,
    version_key: Option<&str>,
    pid: i32,
) -> anyhow::Result<(PathBuf, PathBuf)> {
    let bin = match output {
        Some(path) => path.to_path_buf(),
        None => {
            let folder = version_key.map_or_else(|| format!("pid{pid}"), str::to_string);
            PathBuf::from("scratch")
                .join("dumps")
                .join(folder)
                .join(format!("{name}.bin"))
        }
    };
    let json = bin.with_extension("json");
    if json == bin {
        bail!("o arquivo de saída não pode ter a extensão .json (ela é do arquivo de metadados)");
    }
    Ok((bin, json))
}

/// Grava `bytes`; sem `force`, falha se o arquivo já existir (mesmo que tenha surgido agora).
fn write_file(path: &Path, bytes: &[u8], force: bool) -> anyhow::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true);
    if force {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    let mut file: File = options
        .open(path)
        .with_context(|| format!("não foi possível gravar {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("falha ao gravar {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn default_destination_uses_version_key() {
        let (bin, json) = destination(None, "text", Some("aaaa-bbbb"), 7).unwrap();
        assert_eq!(bin, PathBuf::from("scratch/dumps/aaaa-bbbb/text.bin"));
        assert_eq!(json, PathBuf::from("scratch/dumps/aaaa-bbbb/text.json"));
    }

    #[test]
    fn default_destination_falls_back_to_pid() {
        let (bin, _) = destination(None, "1f00", None, 42).unwrap();
        assert_eq!(bin, PathBuf::from("scratch/dumps/pid42/1f00.bin"));
    }

    #[test]
    fn explicit_output_swaps_the_extension_for_the_json() {
        let (bin, json) = destination(Some(Path::new("a/x.bin")), "n", None, 1).unwrap();
        assert_eq!(bin, PathBuf::from("a/x.bin"));
        assert_eq!(json, PathBuf::from("a/x.json"));
    }

    #[test]
    fn json_output_is_refused() {
        assert!(destination(Some(Path::new("x.json")), "n", None, 1).is_err());
    }

    #[test]
    fn size_over_one_gib_is_refused() {
        assert!(check_size(MAX_SIZE).is_ok());
        assert!(check_size(MAX_SIZE + 1).is_err());
    }
}
