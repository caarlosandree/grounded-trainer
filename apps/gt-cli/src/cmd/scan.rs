//! `gt scan "<padrão>"`: busca AOB nas seções do módulo (spec 6.5).

use std::io::Write;
use std::ops::Range;

use anyhow::{anyhow, bail};
use gt_core::addr::parse_size;
use gt_core::guard;
use gt_core::memscan::{Verdict, scan_bytes, scan_memory};
use gt_core::modules::require_trusted;
use gt_core::pe::Section;
use gt_core::scan::Pattern;
use serde::Serialize;

use super::Global;
use crate::context::Target;
use crate::output::emit;

/// Quantas ocorrências aparecem por padrão.
const DEFAULT_LIMIT: &str = "50";

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
/// `uniqueness` é `none`, `unique`, `many` ou `inconclusive`; `total` é a contagem real e `hits`
/// respeita `--limit`. `unreadable_bytes` conta os bytes da memória que não puderam ser lidos
/// (sempre 0 com `--disk`). `inconclusive` aparece quando a busca na memória teve 0 ou 1
/// ocorrência mas parte da área não pôde ser lida: só `unique` serve como assinatura.
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

    let patterns = std::slice::from_ref(&pattern);
    let hits: Vec<u64>;
    let unreadable_bytes: u64;
    let verdict: Verdict;
    let source;
    if opts.disk {
        source = "disco";
        let disk = target.open_disk(module, &headers)?;
        let mut loaded: Vec<(u64, &[u8])> = Vec::with_capacity(sections.len());
        for section in &sections {
            let bytes = disk
                .bytes(section.rva, section.virtual_size as usize)
                .ok_or_else(|| {
                    anyhow!(
                        "a seção {} sai dos limites da imagem em disco",
                        section.name
                    )
                })?;
            loaded.push((module.base.saturating_add(u64::from(section.rva)), bytes));
        }
        hits = scan_bytes(&loaded, patterns).swap_remove(0);
        unreadable_bytes = 0;
        verdict = Verdict::decide(hits.len(), 0, true);
    } else {
        source = "memória";
        let mut regions: Vec<Range<u64>> = Vec::with_capacity(sections.len());
        for section in &sections {
            let start = module.base.saturating_add(u64::from(section.rva));
            // Mesma guarda do `diff`: as seções vêm do cabeçalho em memória e podem não caber no
            // módulo; nada fora de módulo confiável é lido.
            guard::check(
                &target.maps,
                &target.modules,
                &target.non_pe,
                start,
                u64::from(section.virtual_size),
            )?;
            regions.push(start..start.saturating_add(u64::from(section.virtual_size)));
        }
        let scan = scan_memory(&target.reader, &regions, patterns)?.swap_remove(0);
        hits = scan.hits;
        unreadable_bytes = scan.unreadable_bytes;
        verdict = scan.verdict;
    }

    let output = ScanOutput {
        source,
        total: hits.len(),
        uniqueness: verdict.json(),
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
        |out| write_text(out, &target, &output, &hits, verdict),
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

fn write_text(
    out: &mut dyn Write,
    target: &Target,
    output: &ScanOutput,
    hits: &[u64],
    verdict: Verdict,
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
    writeln!(
        out,
        "Total: {} ({}) (origem: {})",
        output.total,
        verdict.text(output.unreadable_bytes),
        output.source
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
