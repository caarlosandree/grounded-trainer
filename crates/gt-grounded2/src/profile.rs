//! Agregação das amostras do `gt-patch profile` por função (spec 002, 6.9/6.10).

use std::collections::BTreeMap;
use std::ops::Range;

/// Amostras que caíram numa função.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionHits {
    /// RVA do início da função.
    pub start_rva: u32,
    /// RVA do fim da função (exclusivo).
    pub end_rva: u32,
    /// Os `pc` amostrados dentro dela (endereços absolutos).
    pub pcs: Vec<u64>,
}

/// Agrupa `pcs` pelas `functions` (RVAs, ordenadas por início, sem sobreposição).
///
/// A saída vem com as funções mais amostradas primeiro (empate: menor RVA). Um `pc` fora de
/// qualquer função, ou abaixo de `base`, é ignorado.
pub fn aggregate(pcs: &[u64], base: u64, functions: &[Range<u32>]) -> Vec<FunctionHits> {
    let mut by_function: BTreeMap<usize, Vec<u64>> = BTreeMap::new();
    for &pc in pcs {
        let Some(rva) = pc.checked_sub(base).and_then(|r| u32::try_from(r).ok()) else {
            continue;
        };
        // Última função que começa em `rva` ou antes; vale só se `rva` ainda está dentro dela.
        let idx = functions.partition_point(|f| f.start <= rva);
        if let Some(i) = idx.checked_sub(1)
            && rva < functions[i].end
        {
            by_function.entry(i).or_default().push(pc);
        }
    }
    let mut hits: Vec<FunctionHits> = by_function
        .into_iter()
        .map(|(i, pcs)| FunctionHits {
            start_rva: functions[i].start,
            end_rva: functions[i].end,
            pcs,
        })
        .collect();
    hits.sort_by(|a, b| {
        b.pcs
            .len()
            .cmp(&a.pcs.len())
            .then(a.start_rva.cmp(&b.start_rva))
    });
    hits
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    const BASE: u64 = 0x1_4000_0000;

    #[test]
    fn aggregate_counts_by_function() {
        let functions = [0x1000..0x1040, 0x1040..0x1100, 0x2000..0x2010];
        let pcs = [
            BASE + 0x1050,
            BASE + 0x1000,
            BASE + 0x1060,
            BASE + 0x1050,
            BASE + 0x103f,
            // Fora de qualquer função (entre 0x1100 e 0x2000) e abaixo da base.
            BASE + 0x1500,
            0x10,
        ];
        let hits = aggregate(&pcs, BASE, &functions);
        assert_eq!(hits.len(), 2);
        assert_eq!((hits[0].start_rva, hits[0].end_rva), (0x1040, 0x1100));
        assert_eq!(
            hits[0].pcs,
            vec![BASE + 0x1050, BASE + 0x1060, BASE + 0x1050]
        );
        assert_eq!((hits[1].start_rva, hits[1].end_rva), (0x1000, 0x1040));
        assert_eq!(hits[1].pcs, vec![BASE + 0x1000, BASE + 0x103f]);
    }

    #[test]
    fn aggregate_empty_inputs() {
        assert!(aggregate(&[], BASE, &[0x1000..0x1040]).is_empty());
        assert!(aggregate(&[BASE + 0x1000], BASE, &[]).is_empty());
    }
}
