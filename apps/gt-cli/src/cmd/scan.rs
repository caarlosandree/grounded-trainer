//! `gt scan "<padrão>"`: busca AOB nas seções do módulo (spec 6.5).

use std::io::Write;
use std::ops::Range;

use anyhow::{anyhow, bail};
use gt_core::Error;
use gt_core::addr::parse_size;
use gt_core::guard;
use gt_core::modules::require_trusted;
use gt_core::pe::Section;
use gt_core::scan::{Pattern, find_all, scan_chunked};
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
            // Mesma guarda do `diff`: as seções vêm do cabeçalho em memória e podem não caber no
            // módulo; nada fora de módulo confiável é lido.
            guard::check(
                &target.maps,
                &target.modules,
                &target.non_pe,
                start,
                u64::from(section.virtual_size),
            )?;
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
    let verdict = Verdict::decide(hits.len(), unreadable_bytes, opts.disk);
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

/// Veredito de unicidade do `scan`.
///
/// Só `Unique` serve como assinatura (`AGENTS.md` 4). Na memória, bytes ilegíveis podem esconder
/// uma ocorrência, então 0 ou 1 ocorrência viram `Inconclusive*`; 2 ou mais continuam valendo
/// como "não único", porque ocorrências a mais nunca invalidam as já achadas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    None,
    Unique,
    Many(usize),
    /// 0 ocorrências, mas parte da memória não pôde ser lida.
    InconclusiveNone,
    /// 1 ocorrência, mas parte da memória não pôde ser lida.
    InconclusiveUnique,
}

impl Verdict {
    /// `from_disk`: a busca foi no arquivo em disco, que é sempre lido por inteiro.
    fn decide(hits: usize, unreadable_bytes: u64, from_disk: bool) -> Verdict {
        let partial = !from_disk && unreadable_bytes > 0;
        match (hits, partial) {
            (0, false) => Verdict::None,
            (0, true) => Verdict::InconclusiveNone,
            (1, false) => Verdict::Unique,
            (1, true) => Verdict::InconclusiveUnique,
            (n, _) => Verdict::Many(n),
        }
    }

    /// Valor do campo `uniqueness` do JSON.
    fn json(self) -> &'static str {
        match self {
            Verdict::None => "none",
            Verdict::Unique => "unique",
            Verdict::Many(_) => "many",
            Verdict::InconclusiveNone | Verdict::InconclusiveUnique => "inconclusive",
        }
    }

    /// Texto do veredito entre parênteses.
    fn text(self, unreadable_bytes: u64) -> String {
        match self {
            Verdict::None => "nenhum".to_string(),
            Verdict::Unique => "único".to_string(),
            Verdict::Many(n) if unreadable_bytes > 0 => {
                format!("pelo menos {n} ocorrências — não único")
            }
            Verdict::Many(n) => format!("{n} ocorrências"),
            Verdict::InconclusiveNone => format!(
                "nenhum nas partes legíveis — inconclusivo ({unreadable_bytes} bytes ilegíveis; \
                 confirme com --disk)"
            ),
            Verdict::InconclusiveUnique => format!(
                "único nas partes legíveis — inconclusivo ({unreadable_bytes} bytes ilegíveis; \
                 confirme com --disk)"
            ),
        }
    }
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
    fn verdict_without_unreadable_bytes() {
        assert_eq!(Verdict::decide(0, 0, false), Verdict::None);
        assert_eq!(Verdict::decide(1, 0, false), Verdict::Unique);
        assert_eq!(Verdict::decide(2, 0, false), Verdict::Many(2));
    }

    #[test]
    fn verdict_with_unreadable_bytes_in_memory_is_inconclusive() {
        assert_eq!(Verdict::decide(0, 16, false), Verdict::InconclusiveNone);
        assert_eq!(Verdict::decide(1, 16, false), Verdict::InconclusiveUnique);
        // 2 ou mais continua sendo "não único" com certeza.
        assert_eq!(Verdict::decide(2, 16, false), Verdict::Many(2));
        assert_eq!(Verdict::decide(5, 1, false), Verdict::Many(5));
    }

    #[test]
    fn verdict_from_disk_ignores_unreadable_bytes() {
        assert_eq!(Verdict::decide(1, 16, true), Verdict::Unique);
        assert_eq!(Verdict::decide(0, 16, true), Verdict::None);
    }

    #[test]
    fn verdict_json_and_text() {
        assert_eq!(Verdict::Unique.json(), "unique");
        assert_eq!(Verdict::None.json(), "none");
        assert_eq!(Verdict::Many(3).json(), "many");
        assert_eq!(Verdict::InconclusiveNone.json(), "inconclusive");
        assert_eq!(Verdict::InconclusiveUnique.json(), "inconclusive");
        assert_eq!(Verdict::Unique.text(0), "único");
        assert_eq!(Verdict::Many(2).text(0), "2 ocorrências");
        assert_eq!(
            Verdict::Many(2).text(8),
            "pelo menos 2 ocorrências — não único"
        );
        let text = Verdict::InconclusiveUnique.text(4096);
        assert!(text.contains("inconclusivo") && text.contains("4096 bytes ilegíveis"));
        assert!(text.contains("--disk"));
        assert!(!text.starts_with("único)"));
        assert!(
            Verdict::InconclusiveNone
                .text(1)
                .starts_with("nenhum nas partes legíveis")
        );
    }

    #[test]
    fn merge_of_nothing_is_nothing() {
        assert!(merge_ranges(Vec::new()).is_empty());
    }
}
