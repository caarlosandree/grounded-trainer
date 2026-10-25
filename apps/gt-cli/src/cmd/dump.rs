//! `gt dump (<seção> | <endereço> <tamanho>) [-o arquivo]`: salva memória em arquivo (spec 6.8).

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use gt_core::addr::{parse_addr, parse_size};
use gt_core::guard;
use gt_core::modules::require_trusted;
use nix::fcntl::{AT_FDCWD, RenameFlags, renameat2};
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
        Err(err) => return Err(err.into()),
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
    check_destination(&bin_path, &json_path, opts.force)?;

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
    let mut json = serde_json::to_vec_pretty(&sidecar).context("falha ao gerar o JSON")?;
    json.push(b'\n');
    write_pair(&bin_path, &json_path, &read.bytes, &json, opts.force)?;

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

/// Valida o destino antes de gravar. Sem `force`, recusa se o `.bin` ou o `.json` já existir; com
/// `force`, recusa se algum deles for um diretório.
fn check_destination(bin: &Path, json: &Path, force: bool) -> anyhow::Result<()> {
    for path in [bin, json] {
        if force {
            if path.is_dir() {
                bail!("{} é um diretório", path.display());
            }
        } else if path.symlink_metadata().is_ok() {
            bail!(
                "{} já existe; use --force para sobrescrever",
                path.display()
            );
        }
    }
    Ok(())
}

/// Temporário `.<nome>.tmp-<pid>` no mesmo diretório de `path` (o `rename` precisa do mesmo
/// sistema de arquivos).
fn temp_path(path: &Path) -> PathBuf {
    let mut name = OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(format!(".tmp-{}", std::process::id()));
    path.with_file_name(name)
}

/// Grava `bytes` num arquivo novo; falha se ele já existir.
fn write_temp(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("não foi possível gravar {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("falha ao gravar {}", path.display()))
}

/// Move o temporário para o destino. Sem `force`, o `rename` falha se o destino existir
/// (`RENAME_NOREPLACE`), mesmo que ele tenha surgido agora; com `force`, substitui.
fn publish(temp: &Path, dest: &Path, force: bool) -> anyhow::Result<()> {
    let renamed = if force {
        fs::rename(temp, dest)
    } else {
        renameat2(
            AT_FDCWD,
            temp,
            AT_FDCWD,
            dest,
            RenameFlags::RENAME_NOREPLACE,
        )
        .map_err(std::io::Error::from)
    };
    renamed.with_context(|| format!("não foi possível gravar {}", dest.display()))
}

/// Remove temporários que sobraram; falha de remoção só é registrada.
fn remove_temps(temps: &[&Path]) {
    for path in temps {
        if let Err(err) = fs::remove_file(path)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %path.display(), error = %err, "temporário não removido");
        }
    }
}

/// Grava o `.bin` e o `.json` de forma que uma falha não deixe um par pela metade.
///
/// Pré-valida o destino, grava os dois em temporários e só então renomeia: primeiro o `.json`,
/// depois o `.bin`. Falha em qualquer etapa apaga os temporários. Sem `force`, se o `.bin` falhar
/// depois do `.json` já renomeado, o `.json` recém-criado também é apagado. Com `force`, esse
/// caso (raro: o `rename` do `.bin` falhar depois de o do `.json` ter dado certo) deixa o `.json`
/// novo ao lado do `.bin` antigo, porque o `.json` anterior já foi substituído e não há como
/// restaurá-lo sem uma cópia.
fn write_pair(
    bin: &Path,
    json: &Path,
    bin_bytes: &[u8],
    json_bytes: &[u8],
    force: bool,
) -> anyhow::Result<()> {
    check_destination(bin, json, force)?;
    let (bin_temp, json_temp) = (temp_path(bin), temp_path(json));
    if let Err(err) =
        write_temp(&bin_temp, bin_bytes).and_then(|()| write_temp(&json_temp, json_bytes))
    {
        remove_temps(&[&bin_temp, &json_temp]);
        return Err(err);
    }
    if let Err(err) = publish(&json_temp, json, force) {
        remove_temps(&[&bin_temp, &json_temp]);
        return Err(err);
    }
    if let Err(err) = publish(&bin_temp, bin, force) {
        remove_temps(&[&bin_temp]);
        if !force {
            remove_temps(&[json]);
        }
        return Err(err);
    }
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
    fn failed_temp_write_removes_temps() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("x.bin");
        fs::write(&bin, b"OLD").unwrap();
        // O `.json` fica num diretório que não existe: a gravação do temporário dele falha.
        let json = dir.path().join("faltando").join("x.json");
        let err = write_pair(&bin, &json, b"NEW", b"{}", true);
        assert!(err.is_err());
        assert_eq!(fs::read(&bin).unwrap(), b"OLD");
        let left: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, vec![std::ffi::OsString::from("x.bin")]);
    }

    #[test]
    fn write_pair_writes_both_and_leaves_no_temps() {
        let dir = tempfile::tempdir().unwrap();
        let (bin, json) = (dir.path().join("x.bin"), dir.path().join("x.json"));
        write_pair(&bin, &json, b"BIN", b"JSON", false).unwrap();
        assert_eq!(fs::read(&bin).unwrap(), b"BIN");
        assert_eq!(fs::read(&json).unwrap(), b"JSON");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
        // Sem --force, o par existente não é tocado; com --force, é trocado.
        assert!(write_pair(&bin, &json, b"B2", b"J2", false).is_err());
        assert_eq!(fs::read(&bin).unwrap(), b"BIN");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
        write_pair(&bin, &json, b"B2", b"J2", true).unwrap();
        assert_eq!(fs::read(&bin).unwrap(), b"B2");
        assert_eq!(fs::read(&json).unwrap(), b"J2");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn size_over_one_gib_is_refused() {
        assert!(check_size(MAX_SIZE).is_ok());
        assert!(check_size(MAX_SIZE + 1).is_err());
    }
}
