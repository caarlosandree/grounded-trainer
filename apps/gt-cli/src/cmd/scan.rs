//! `gt scan "<padrão>"`: busca AOB nas seções do módulo (spec 6.5).

use std::io::Write;
use std::ops::Range;

use anyhow::{anyhow, bail};
use gt_core::Error;
use gt_core::addr::parse_size;
use gt_core::modules::require_trusted;
use gt_core::pe::Section;
use gt_core::scan::{Pattern, Uniqueness, find_all, scan_chunked};
use serde::Serialize;

use super::Global;
use crate::context::Target;
use crate::output::emit;

/// Quantas ocorrências aparecem por padrão.
const DEFAULT_LIMIT: &str = "50";
/// Tamanho dos blocos lidos da memória: 1 MiB.
const CHUNK: usize = 1 << 20;

/// Opções do `scan` além das globais.
pub struct Options<'a> {
    pub pattern: &'a str,
    pub section: Option<&'a str>,
    pub all: bool,
    pub disk: bool,
    pub limit: Option<&'a str>,
}

/// Valor padrão de `--limit`, para o clap.
pub const fn default_limit() -> &'static str {
    DEFAULT_LIMIT
}

/// Saída `--json` do `scan`.
///
/// `uniqueness` é `none`, `unique` ou `many`; `total` é a contagem real e `hits` respeita
/// `--limit`. `unreadable_bytes` conta os bytes da memória que não puderam ser lidos (sempre 0
/// com `--disk`).
#[derive(Serialize)]
struct ScanOutput {
    source: &'static str,
    total: usize,
    uniqueness: &'static str,
    unreadable_bytes: u64,
    hits: Vec<HitOutput>,
}

#[derive(Serialize)]
struct HitOutput {
    address: String,
    label: Option<String>,
}

pub fn run(global: &Global, opts: &Options<'_>) -> anyhow::Result<()> {
    let target = Target::open(global.pid)?;
    // O módulo vem antes de tudo: num alvo que não é PE, o erro é "não é uma imagem PE".
    let (module, headers) = target.module(&global.module)?;
    require_trusted(module)?;

    let pattern = Pattern::parse(opts.pattern)?;
    let limit = parse_size(opts.limit.unwrap_or(DEFAULT_LIMIT))?;
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let sections = select_sections(&headers.sections, opts)?;

    let mut hits: Vec<u64> = Vec::new();
    let mut unreadable: Vec<Range<u64>> = Vec::new();
    let source;
    if opts.disk {
        source = "disco";
        let disk = target.open_disk(module, &headers)?;
        for section in &sections {
            let bytes = disk
                .bytes(section.rva, section.virtual_size as usize)
                .ok_or_else(|| {
                    anyhow!(
                        "a seção {} sai dos limites da imagem em disco",
                        section.name
                    )
                })?;
            let start = module.base.saturating_add(u64::from(section.rva));
            hits.extend(
                find_all(bytes, &pattern)
                    .into_iter()
                    .map(|offset| start + offset as u64),
            );
        }
    } else {
        source = "memória";
        for section in &sections {
            let start = module.base.saturating_add(u64::from(section.rva));
            let found = scan_chunked(
                u64::from(section.virtual_size),
                CHUNK,
                &pattern,
                |offset, buf| read_block(&target, start + offset, buf, &mut unreadable),
            )?;
            hits.extend(found.into_iter().map(|offset| start + offset));
        }
    }

    let unreadable = merge_ranges(unreadable);
    // Os zeros no lugar de bytes ilegíveis não são memória do jogo: não contam como ocorrência.
    let pattern_len = pattern.len() as u64;
    hits.retain(|&hit| {
        !unreadable
            .iter()
            .any(|hole| hit < hole.end && hit + pattern_len > hole.start)
    });
    hits.sort_unstable();
    hits.dedup();

    let unreadable_bytes: u64 = unreadable.iter().map(|hole| hole.end - hole.start).sum();
    let uniqueness = Uniqueness::from_hits(&hits);
    let output = ScanOutput {
        source,
        total: hits.len(),
        uniqueness: match uniqueness {
            Uniqueness::None => "none",
            Uniqueness::Unique(_) => "unique",
            Uniqueness::Many(_) => "many",
        },
        unreadable_bytes,
        hits: hits
            .iter()
            .take(limit)
            .map(|&addr| HitOutput {
                address: format!("{addr:#x}"),
                label: target.module_label(addr),
            })
            .collect(),
    };
    emit(
        global.json,
        |out| write_text(out, &target, &output, &hits, uniqueness),
        &output,
    )
}

/// Seções a varrer: a pedida, todas (`--all`) ou as executáveis.
fn select_sections<'a>(
    all_sections: &'a [Section],
    opts: &Options<'_>,
) -> anyhow::Result<Vec<&'a Section>> {
    let chosen: Vec<&Section> = match opts.section {
        Some(name) => {
            let section = all_sections
                .iter()
                .find(|s| s.name == name)
                .ok_or_else(|| {
                    anyhow!("seção {name} não encontrada; `gt inspect` lista as seções")
                })?;
            vec![section]
        }
        None if opts.all => all_sections.iter().collect(),
        None => all_sections.iter().filter(|s| s.is_executable()).collect(),
    };
    let chosen: Vec<&Section> = chosen.into_iter().filter(|s| s.virtual_size > 0).collect();
    if chosen.is_empty() {
        bail!("nenhuma seção para varrer; use --section NOME ou --all");
    }
    Ok(chosen)
}

/// Lê `buf` da memória. Um bloco com páginas ilegíveis é lido com zeros nas lacunas, que entram
/// em `unreadable`; um bloco inteiro ilegível fica todo zerado.
fn read_block(
    target: &Target,
    addr: u64,
    buf: &mut [u8],
    unreadable: &mut Vec<Range<u64>>,
) -> gt_core::Result<()> {
    match target.reader.read_exact(addr, buf) {
        Ok(()) => Ok(()),
        Err(Error::Unmapped { .. }) => {
            match target.reader.read_lossy(addr, buf.len()) {
                Ok(read) => {
                    buf.copy_from_slice(&read.bytes);
                    unreadable.extend(read.holes);
                }
                Err(Error::Unmapped { .. }) => {
                    buf.fill(0);
                    unreadable.push(addr..addr + buf.len() as u64);
                }
                Err(err) => return Err(err),
            }
            Ok(())
        }
        Err(err) => Err(err),
    }
}

/// Ordena e funde intervalos que se tocam (os blocos se sobrepõem).
fn merge_ranges(mut ranges: Vec<Range<u64>>) -> Vec<Range<u64>> {
    ranges.sort_by_key(|r| r.start);
    let mut merged: Vec<Range<u64>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

fn write_text(
    out: &mut dyn Write,
    target: &Target,
    output: &ScanOutput,
    hits: &[u64],
    uniqueness: Uniqueness,
) -> std::io::Result<()> {
    for &addr in hits.iter().take(output.hits.len()) {
        writeln!(out, "{}", target.label(addr))?;
    }
    if output.hits.len() < output.total {
        writeln!(
            out,
            "... mostrando {} de {} (use --limit para ver mais)",
            output.hits.len(),
            output.total
        )?;
    }
    let verdict = match uniqueness {
        Uniqueness::None => "nenhum".to_string(),
        Uniqueness::Unique(_) => "único".to_string(),
        Uniqueness::Many(n) => format!("{n} ocorrências"),
    };
    writeln!(
        out,
        "Total: {} ({verdict}) (origem: {})",
        output.total, output.source
    )?;
    if output.unreadable_bytes > 0 {
        writeln!(
            out,
            "aviso: {} bytes ilegíveis na memória foram ignorados na busca",
            output.unreadable_bytes
        )?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    #[test]
    fn merge_joins_overlapping_and_adjacent_ranges() {
        let merged = merge_ranges(vec![30..40, 0..10, 5..15, 15..20]);
        assert_eq!(merged, vec![0..20, 30..40]);
    }

    #[test]
    fn merge_of_nothing_is_nothing() {
        assert!(merge_ranges(Vec::new()).is_empty());
    }
}
