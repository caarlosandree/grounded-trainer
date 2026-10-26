//! Resolução de globais por padrão AOB com deslocamento RIP-relativo (função pura, sem o jogo).

use crate::memscan::Verdict;
use crate::scan::Pattern;

/// Padrão AOB de uma global, com onde fica o `disp32` RIP-relativo dentro da ocorrência.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalPattern {
    /// Padrão a buscar no módulo.
    pub pattern: Pattern,
    /// Deslocamento do `disp32` dentro da ocorrência.
    pub disp_offset: usize,
    /// Distância do início da ocorrência até o fim da instrução que contém o `disp32`.
    pub insn_end: usize,
    /// Somado ao endereço resolvido (por exemplo, para chegar a um campo a partir do início da estrutura).
    pub adjust: i64,
}

/// Endereço resolvido de uma ocorrência: `hit + insn_end + disp + adjust`.
///
/// `None` se a soma sair de `u64`.
pub fn rip_target(hit: u64, disp: i32, insn_end: usize, adjust: i64) -> Option<u64> {
    // i128 cabe qualquer soma destes termos, então o estouro aparece só na conversão final.
    let sum = i128::from(hit) + insn_end as i128 + i128::from(disp) + i128::from(adjust);
    u64::try_from(sum).ok()
}

/// Veredito sobre o endereço de uma global.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlobalVerdict {
    /// Todas as ocorrências legíveis resolvem para este endereço.
    Found(u64),
    /// Nenhuma ocorrência.
    NotFound,
    /// Ocorrências resolvem para endereços diferentes; a lista é sem repetição e ordenada.
    Ambiguous(Vec<u64>),
    /// Nenhuma ocorrência legível, mas parte da memória não pôde ser lida.
    Inconclusive,
}

/// Decide a global a partir do veredito da busca e dos alvos já calculados das ocorrências legíveis.
pub fn decide_global(targets: &[u64], verdict: Verdict) -> GlobalVerdict {
    match verdict {
        Verdict::None => GlobalVerdict::NotFound,
        Verdict::InconclusiveNone => GlobalVerdict::Inconclusive,
        Verdict::Unique | Verdict::Many(_) | Verdict::InconclusiveUnique => {
            // Ocorrências sem alvo calculado não decidem nada: tratadas como ausentes.
            let mut distinct = targets.to_vec();
            distinct.sort_unstable();
            distinct.dedup();
            match distinct.as_slice() {
                [] => GlobalVerdict::NotFound,
                [only] => GlobalVerdict::Found(*only),
                _ => GlobalVerdict::Ambiguous(distinct),
            }
        }
    }
}

impl GlobalVerdict {
    /// Texto curto do veredito, com os mesmos termos dos patches.
    pub fn label(&self) -> &'static str {
        match self {
            GlobalVerdict::Found(_) => "disponível",
            GlobalVerdict::NotFound => "não encontrado",
            GlobalVerdict::Ambiguous(_) => "ambíguo",
            GlobalVerdict::Inconclusive => "inconclusivo",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rip_target_positive_and_negative_disp() {
        // Instrução de 7 bytes com disp positivo: 0x1000 + 7 + 0x20 = 0x1027.
        assert_eq!(rip_target(0x1000, 0x20, 7, 0), Some(0x1027));
        // disp negativo com adjust negativo: 0x1000 + 7 - 0x10 - 0x4 = 0xFF3.
        assert_eq!(rip_target(0x1000, -0x10, 7, -0x4), Some(0xFF3));
    }

    #[test]
    fn rip_target_overflow_is_none() {
        assert_eq!(rip_target(u64::MAX, 1, 0, 0), None);
        assert_eq!(rip_target(0, -1, 0, 0), None);
        assert_eq!(rip_target(0, 0, 0, -1), None);
    }

    #[test]
    fn decide_one_hit() {
        assert_eq!(
            decide_global(&[0x2000], Verdict::Unique),
            GlobalVerdict::Found(0x2000)
        );
    }

    #[test]
    fn decide_many_agreeing() {
        let targets = vec![0x3000; 281];
        assert_eq!(
            decide_global(&targets, Verdict::Many(281)),
            GlobalVerdict::Found(0x3000)
        );
    }

    #[test]
    fn decide_many_disagreeing() {
        let targets = [0x5000, 0x4000, 0x5000];
        assert_eq!(
            decide_global(&targets, Verdict::Many(3)),
            GlobalVerdict::Ambiguous(vec![0x4000, 0x5000])
        );
    }

    #[test]
    fn decide_inconclusive_none() {
        assert_eq!(
            decide_global(&[], Verdict::InconclusiveNone),
            GlobalVerdict::Inconclusive
        );
    }

    #[test]
    fn decide_inconclusive_unique_counts() {
        assert_eq!(
            decide_global(&[0x6000], Verdict::InconclusiveUnique),
            GlobalVerdict::Found(0x6000)
        );
    }

    #[test]
    fn decide_none_is_not_found() {
        assert_eq!(decide_global(&[], Verdict::None), GlobalVerdict::NotFound);
    }

    #[test]
    fn decide_empty_targets_with_occurrences_is_not_found() {
        assert_eq!(
            decide_global(&[], Verdict::Many(2)),
            GlobalVerdict::NotFound
        );
        assert_eq!(
            decide_global(&[], Verdict::InconclusiveUnique),
            GlobalVerdict::NotFound
        );
    }

    #[test]
    fn labels_match_patch_texts() {
        assert_eq!(GlobalVerdict::Found(1).label(), "disponível");
        assert_eq!(GlobalVerdict::NotFound.label(), "não encontrado");
        assert_eq!(GlobalVerdict::Ambiguous(vec![1, 2]).label(), "ambíguo");
        assert_eq!(GlobalVerdict::Inconclusive.label(), "inconclusivo");
    }
}
