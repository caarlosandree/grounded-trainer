//! Estado de cada patch (spec 002, 6.4), calculado de forma pura a partir de entradas já lidas.

use gt_core::memscan::{MemScan, Verdict};

use crate::signatures::PatchDef;

/// Estado de um patch no processo do jogo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchState {
    /// Versão com `rva` conferido e bytes atuais iguais aos esperados.
    Verified {
        addr: u64,
    },
    /// Sem `rva`: ocorrência única na memória e no disco, no mesmo lugar, bytes esperados.
    Unverified {
        addr: u64,
    },
    /// Aplicado e a trava do diário está ocupada por outro `gt-patch`.
    Active {
        addr: u64,
    },
    /// O diário mostra o patch aplicado, mas nenhum `gt-patch` roda (sobrou de uma queda).
    Pending {
        addr: u64,
    },
    BytesDiffer {
        addr: u64,
        found: Vec<u8>,
    },
    Misplaced {
        found_rva: u64,
    },
    NotFound,
    Ambiguous(usize),
    Inconclusive,
    DiskUnreadable(String),
}

impl PatchState {
    /// Texto do estado, como na tabela da spec 6.4.
    pub fn label(&self) -> &'static str {
        match self {
            PatchState::Verified { .. } => "disponível (verificado)",
            PatchState::Unverified { .. } => "disponível (não verificado)",
            PatchState::Active { .. } => "ativo",
            PatchState::Pending { .. } => "pendente",
            PatchState::BytesDiffer { .. } => "bytes diferentes",
            PatchState::Misplaced { .. } => "fora do lugar",
            PatchState::NotFound => "não encontrado",
            PatchState::Ambiguous(_) => "ambíguo",
            PatchState::Inconclusive => "inconclusivo",
            PatchState::DiskUnreadable(_) => "disco ilegível",
        }
    }

    /// Endereço onde aplicar, só para os estados "disponível".
    pub fn apply_addr(&self) -> Option<u64> {
        match self {
            PatchState::Verified { addr } | PatchState::Unverified { addr } => Some(*addr),
            _ => None,
        }
    }

    pub fn is_unverified(&self) -> bool {
        matches!(self, PatchState::Unverified { .. })
    }
}

/// Entradas do cálculo do estado de um patch.
pub struct StateInputs<'a> {
    /// Endereço do patch no diário e se a trava está ocupada, já filtrado por mesmo processo
    /// e por bytes atuais = bytes novos.
    pub journal_hit: Option<(u64, bool)>,
    /// Leitura do cabeçalho do disco (a causa, se falhou).
    pub disk: std::result::Result<(), String>,
    /// Resultado da busca do padrão na memória.
    pub memory: &'a MemScan,
    /// Ocorrências do padrão no disco, em endereços da base atual (só para patch sem `rva`).
    pub disk_hits: Option<&'a [u64]>,
    pub module_base: u64,
    /// Bytes atuais em ocorrência + `offset`, se legíveis.
    pub current: Option<&'a [u8]>,
}

/// Calcula o estado de `def` (ordem da spec 6.4: diário, disco, veredito da memória, lugar, bytes).
pub fn compute_state(def: &PatchDef, inp: &StateInputs) -> PatchState {
    if let Some((addr, locked)) = inp.journal_hit {
        return if locked {
            PatchState::Active { addr }
        } else {
            PatchState::Pending { addr }
        };
    }
    if let Err(cause) = &inp.disk {
        return PatchState::DiskUnreadable(cause.clone());
    }
    let hit = match (inp.memory.verdict, inp.memory.hits.first()) {
        (Verdict::Many(n), _) => return PatchState::Ambiguous(n),
        (Verdict::InconclusiveNone | Verdict::InconclusiveUnique, _) => {
            return PatchState::Inconclusive;
        }
        (Verdict::None, _) | (Verdict::Unique, None) => return PatchState::NotFound,
        (Verdict::Unique, Some(&hit)) => hit,
    };
    let addr = hit + def.offset as u64;
    match def.rva {
        Some(rva) => {
            let found_rva = addr.wrapping_sub(inp.module_base);
            if found_rva != rva {
                return PatchState::Misplaced { found_rva };
            }
        }
        None => match inp.disk_hits {
            Some([only]) if *only == hit => {}
            Some([]) | Some([_]) => return PatchState::NotFound,
            Some(many) => return PatchState::Ambiguous(many.len()),
            // Sem `rva`, o disco é obrigatório; sem a busca, nada a afirmar.
            None => return PatchState::DiskUnreadable("busca em disco ausente".to_string()),
        },
    }
    match inp.current {
        None => PatchState::Inconclusive,
        Some(current) if current == def.expected.as_slice() => {
            if def.rva.is_some() {
                PatchState::Verified { addr }
            } else {
                PatchState::Unverified { addr }
            }
        }
        Some(current) => PatchState::BytesDiffer {
            addr,
            found: current.to_vec(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signatures::PatchDef;
    use gt_core::memscan::{MemScan, Verdict};
    use gt_core::scan::Pattern;

    const BASE: u64 = 0x1_4000_0000;

    fn def(rva: Option<u64>) -> PatchDef {
        PatchDef {
            id: "p".into(),
            name: "Patch".into(),
            category: crate::signatures::Category::Itens,
            description: "d".into(),
            module: crate::GAME_EXE.into(),
            pattern: Pattern::parse("48 8B ?? 90").unwrap(),
            offset: 1,
            expected: vec![0x8B, 0xF9],
            replacement: vec![0x89, 0xCF],
            rva,
            value: None,
        }
    }

    fn mem(hits: &[u64], unreadable: u64) -> MemScan {
        MemScan {
            hits: hits.to_vec(),
            unreadable_bytes: unreadable,
            verdict: Verdict::decide(hits.len(), unreadable, false),
        }
    }

    const HIT: u64 = BASE + 0x1000;

    fn inputs<'a>(
        memory: &'a MemScan,
        disk_hits: Option<&'a [u64]>,
        current: Option<&'a [u8]>,
    ) -> StateInputs<'a> {
        StateInputs {
            journal_hit: None,
            disk: Ok(()),
            memory,
            disk_hits,
            module_base: BASE,
            current,
        }
    }

    const GOOD: &[u8] = &[0x8B, 0xF9];

    #[test]
    fn state_verified() {
        let m = mem(&[HIT], 0);
        let st = compute_state(&def(Some(0x1001)), &inputs(&m, None, Some(GOOD)));
        assert_eq!(st, PatchState::Verified { addr: HIT + 1 });
        assert_eq!(st.apply_addr(), Some(HIT + 1));
        assert!(!st.is_unverified());
    }

    #[test]
    fn state_unverified_needs_unique_disk_at_same_offset() {
        let m = mem(&[HIT], 0);
        let disk = [HIT];
        let st = compute_state(&def(None), &inputs(&m, Some(&disk), Some(GOOD)));
        assert_eq!(st, PatchState::Unverified { addr: HIT + 1 });
        assert_eq!(st.apply_addr(), Some(HIT + 1));
        assert!(st.is_unverified());
    }

    #[test]
    fn state_unverified_disk_elsewhere_is_not_found() {
        let m = mem(&[HIT], 0);
        for disk in [vec![], vec![HIT + 0x40]] {
            let st = compute_state(&def(None), &inputs(&m, Some(&disk), Some(GOOD)));
            assert_eq!(st, PatchState::NotFound);
        }
    }

    #[test]
    fn state_unverified_disk_many_is_ambiguous() {
        let m = mem(&[HIT], 0);
        let disk = [HIT, HIT + 0x40, HIT + 0x80];
        let st = compute_state(&def(None), &inputs(&m, Some(&disk), Some(GOOD)));
        assert_eq!(st, PatchState::Ambiguous(3));
    }

    #[test]
    fn state_active_and_pending_from_journal_win_over_scan() {
        // A varredura diria "não encontrado" (o padrão cobre os bytes trocados), mas o diário vem antes.
        let m = mem(&[], 0);
        let mut inp = inputs(&m, None, None);
        inp.disk = Err("ilegível".into());
        inp.journal_hit = Some((HIT + 1, true));
        assert_eq!(
            compute_state(&def(None), &inp),
            PatchState::Active { addr: HIT + 1 }
        );
        inp.journal_hit = Some((HIT + 1, false));
        let st = compute_state(&def(None), &inp);
        assert_eq!(st, PatchState::Pending { addr: HIT + 1 });
        assert_eq!(st.apply_addr(), None);
    }

    #[test]
    fn state_bytes_differ_with_wildcards() {
        let m = mem(&[HIT], 0);
        let found = [0x8B, 0xD2];
        let st = compute_state(&def(Some(0x1001)), &inputs(&m, None, Some(&found)));
        assert_eq!(
            st,
            PatchState::BytesDiffer {
                addr: HIT + 1,
                found: found.to_vec()
            }
        );
        assert_eq!(st.apply_addr(), None);
    }

    #[test]
    fn state_misplaced() {
        let m = mem(&[HIT], 0);
        let st = compute_state(&def(Some(0x2000)), &inputs(&m, None, Some(GOOD)));
        assert_eq!(st, PatchState::Misplaced { found_rva: 0x1001 });
    }

    #[test]
    fn state_not_found() {
        let m = mem(&[], 0);
        assert_eq!(
            compute_state(&def(Some(1)), &inputs(&m, None, None)),
            PatchState::NotFound
        );
    }

    #[test]
    fn state_ambiguous() {
        let m = mem(&[HIT, HIT + 8], 0);
        assert_eq!(
            compute_state(&def(Some(1)), &inputs(&m, None, None)),
            PatchState::Ambiguous(2)
        );
    }

    #[test]
    fn state_inconclusive() {
        for hits in [&[][..], &[HIT][..]] {
            let m = mem(hits, 4096);
            assert_eq!(
                compute_state(&def(Some(0x1001)), &inputs(&m, None, Some(GOOD))),
                PatchState::Inconclusive
            );
        }
    }

    #[test]
    fn state_disk_unreadable() {
        let m = mem(&[HIT], 0);
        let mut inp = inputs(&m, None, Some(GOOD));
        inp.disk = Err("sem arquivo".into());
        let st = compute_state(&def(Some(0x1001)), &inp);
        assert_eq!(st, PatchState::DiskUnreadable("sem arquivo".into()));
        assert_eq!(st.apply_addr(), None);
    }

    #[test]
    fn labels_match_spec() {
        let all = [
            (PatchState::Verified { addr: 0 }, "disponível (verificado)"),
            (
                PatchState::Unverified { addr: 0 },
                "disponível (não verificado)",
            ),
            (PatchState::Active { addr: 0 }, "ativo"),
            (PatchState::Pending { addr: 0 }, "pendente"),
            (
                PatchState::BytesDiffer {
                    addr: 0,
                    found: vec![],
                },
                "bytes diferentes",
            ),
            (PatchState::Misplaced { found_rva: 0 }, "fora do lugar"),
            (PatchState::NotFound, "não encontrado"),
            (PatchState::Ambiguous(2), "ambíguo"),
            (PatchState::Inconclusive, "inconclusivo"),
            (PatchState::DiskUnreadable("x".into()), "disco ilegível"),
        ];
        for (state, label) in all {
            assert_eq!(state.label(), label);
        }
    }
}
