//! Busca de padrões na memória de um processo e no arquivo em disco, com o veredito de unicidade.

use std::ops::Range;

use crate::error::{Error, Result};
use crate::mem::MemReader;
use crate::scan::{Pattern, find_all, scan_chunked_many};

/// Tamanho dos blocos lidos da memória: 1 MiB.
const CHUNK: usize = 1 << 20;

/// Veredito de unicidade de uma busca (`gt scan` e o motor de patches).
///
/// Só `Unique` serve como assinatura (`AGENTS.md` 4). Na memória, bytes ilegíveis podem esconder
/// uma ocorrência, então 0 ou 1 ocorrência viram `Inconclusive*`; 2 ou mais continuam valendo
/// como "não único", porque ocorrências a mais nunca invalidam as já achadas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
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
    pub fn decide(hits: usize, unreadable_bytes: u64, from_disk: bool) -> Verdict {
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
    pub fn json(self) -> &'static str {
        match self {
            Verdict::None => "none",
            Verdict::Unique => "unique",
            Verdict::Many(_) => "many",
            Verdict::InconclusiveNone | Verdict::InconclusiveUnique => "inconclusive",
        }
    }

    /// Texto do veredito entre parênteses.
    pub fn text(self, unreadable_bytes: u64) -> String {
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

/// Resultado da busca em memória de um padrão.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemScan {
    /// Endereços das ocorrências, em ordem crescente e sem duplicatas.
    pub hits: Vec<u64>,
    /// Total de bytes das regiões que não puderam ser lidos (igual para todos os padrões da busca).
    pub unreadable_bytes: u64,
    /// Veredito de unicidade, já considerando os bytes ilegíveis.
    pub verdict: Verdict,
}

/// Busca `patterns` na memória de `reader`, dentro de `regions`, numa única passada por região.
///
/// Devolve um [`MemScan`] por padrão, na ordem de `patterns`. A leitura é em blocos de 1 MiB e
/// tolerante: páginas ilegíveis viram zeros e entram em `unreadable_bytes`; um bloco inteiro
/// ilegível fica todo zerado. Ocorrências que tocam uma lacuna são descartadas (os zeros não são
/// memória do alvo).
///
/// Esta função **não** chama a guarda de módulos: quem chama confere as regiões antes.
pub fn scan_memory(
    reader: &MemReader,
    regions: &[Range<u64>],
    patterns: &[Pattern],
) -> Result<Vec<MemScan>> {
    let mut hits: Vec<Vec<u64>> = vec![Vec::new(); patterns.len()];
    let mut unreadable: Vec<Range<u64>> = Vec::new();
    for region in regions {
        let found = scan_chunked_many(
            region.end.saturating_sub(region.start),
            CHUNK,
            patterns,
            |offset, buf| read_block(reader, region.start + offset, buf, &mut unreadable),
        )?;
        for (all, region_hits) in hits.iter_mut().zip(found) {
            all.extend(region_hits.into_iter().map(|offset| region.start + offset));
        }
    }
    let unreadable = merge_ranges(unreadable);
    let unreadable_bytes: u64 = unreadable.iter().map(|hole| hole.end - hole.start).sum();
    Ok(patterns
        .iter()
        .zip(hits)
        .map(|(pattern, mut hits)| {
            let pattern_len = pattern.len() as u64;
            hits.retain(|&hit| {
                !unreadable
                    .iter()
                    .any(|hole| hit < hole.end && hit + pattern_len > hole.start)
            });
            hits.sort_unstable();
            hits.dedup();
            let verdict = Verdict::decide(hits.len(), unreadable_bytes, false);
            MemScan {
                hits,
                unreadable_bytes,
                verdict,
            }
        })
        .collect())
}

/// Busca `patterns` em trechos já carregados (a imagem em disco).
///
/// Cada região é `(endereço de carga, bytes)`; os resultados saem como endereços absolutos
/// (`endereço + offset`), um vetor por padrão, em ordem crescente e sem duplicatas.
pub fn scan_bytes(regions: &[(u64, &[u8])], patterns: &[Pattern]) -> Vec<Vec<u64>> {
    patterns
        .iter()
        .map(|pattern| {
            let mut hits: Vec<u64> = regions
                .iter()
                .flat_map(|&(start, bytes)| {
                    find_all(bytes, pattern)
                        .into_iter()
                        .map(move |offset| start + offset as u64)
                })
                .collect();
            hits.sort_unstable();
            hits.dedup();
            hits
        })
        .collect()
}

/// Lê `buf` da memória. Um bloco com páginas ilegíveis é lido com zeros nas lacunas, que entram
/// em `unreadable`; um bloco inteiro ilegível fica todo zerado.
fn read_block(
    reader: &MemReader,
    addr: u64,
    buf: &mut [u8],
    unreadable: &mut Vec<Range<u64>>,
) -> Result<()> {
    match reader.read_exact(addr, buf) {
        Ok(()) => Ok(()),
        Err(Error::Unmapped { .. }) => {
            match reader.read_lossy(addr, buf.len()) {
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
pub fn merge_ranges(mut ranges: Vec<Range<u64>>) -> Vec<Range<u64>> {
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
    fn scan_bytes_offsets_are_absolute() {
        let mut data = [0u8; 16];
        data[5..8].copy_from_slice(&[0xDE, 0xAD, 0xBE]);
        let pattern = Pattern::parse("DE AD BE").unwrap();
        assert_eq!(
            scan_bytes(&[(0x1000, &data)], &[pattern]),
            vec![vec![0x1005]]
        );
    }

    #[test]
    fn merge_of_nothing_is_nothing() {
        assert!(merge_ranges(Vec::new()).is_empty());
    }
}
