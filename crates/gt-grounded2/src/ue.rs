//! Globais do Unreal (`GUObjectArray` e `FNamePool`) achadas por padrão AOB e a conferência barata
//! da reflexão (spec 005, 6.2 e 6.7).
//!
//! A decisão sobre cada global é pura: recebe as ocorrências já achadas (memória e, se foi lida,
//! disco) com o `disp32` de cada uma, e devolve o estado. [`crate::catalog`] faz a busca e a
//! leitura dos bytes.

use std::ops::Range;

use gt_core::global::{GlobalVerdict, decide_global, rip_target};
use gt_core::memscan::Verdict;
use gt_core::modules::Module;
use gt_core::pe::PeHeaders;

use crate::Result;
use crate::signatures::{GlobalDef, UeSigs};

/// Id especial do `scan_subset` que pede só a resolução das globais (nova busca automática).
pub const UE_ID: &str = "@ue";

/// Texto de uma global indisponível porque o disco não pôde ser lido (o mesmo dos patches).
pub const DISK_UNREADABLE: &str = "disco ilegível";
/// Texto de uma global cujo endereço difere do `rva` da versão (o mesmo dos patches).
pub const MISPLACED: &str = "fora do lugar";

/// Estado de uma global do Unreal no processo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlobalState {
    /// Versão com `rva` conferido: o endereço resolvido é igual a `base + rva`.
    Verified(u64),
    /// Sem arquivo de versão, ou sem `rva` para esta global: endereço resolvido, não verificado.
    Unverified(u64),
    /// Indisponível; o texto é um dos dos patches (`não encontrado`, `ambíguo`, `inconclusivo`,
    /// `fora do lugar`, `disco ilegível`).
    Unavailable(&'static str),
}

impl GlobalState {
    /// Endereço da global, se disponível.
    pub fn addr(&self) -> Option<u64> {
        match self {
            GlobalState::Verified(addr) | GlobalState::Unverified(addr) => Some(*addr),
            GlobalState::Unavailable(_) => None,
        }
    }
}

/// Resultado da resolução das globais e da conferência barata do layout.
#[derive(Debug, Clone)]
pub struct UeScan {
    /// Bloco `[ue]` da versão usado na busca.
    pub sigs: UeSigs,
    pub gobjects: GlobalState,
    pub names: GlobalState,
    /// Conferência barata (spec 6.2). Só é calculada com as duas globais disponíveis; senão fica
    /// `Err("globais indisponíveis")`. O `Err` traz o detalhe para o log; ao usuário vale sempre
    /// `reflexão não reconhecida`.
    pub cheap: std::result::Result<(), String>,
}

/// Uma ocorrência de padrão de global com o `disp32` lido dos bytes dela.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Occurrence {
    /// Endereço da ocorrência (início do padrão).
    pub addr: u64,
    /// `i32` little-endian em `addr + disp_offset`; `None` se esses bytes não puderam ser lidos.
    pub disp: Option<i32>,
}

/// `i32` little-endian em `bytes[at..at + 4]`; `None` se não couber.
pub fn disp_at(bytes: &[u8], at: usize) -> Option<i32> {
    let end = at.checked_add(4)?;
    let raw: [u8; 4] = bytes.get(at..end)?.try_into().ok()?;
    Some(i32::from_le_bytes(raw))
}

/// Ocorrências de `hits` com o `disp32` dado por `read(addr_do_disp)` (4 bytes em
/// `hit + disp_offset`).
pub fn occurrences(
    hits: &[u64],
    disp_offset: usize,
    read: impl Fn(u64) -> Option<[u8; 4]>,
) -> Vec<Occurrence> {
    hits.iter()
        .map(|&addr| Occurrence {
            addr,
            disp: addr
                .checked_add(disp_offset as u64)
                .and_then(&read)
                .map(i32::from_le_bytes),
        })
        .collect()
}

/// Entrada da decisão de uma global: o que a busca em memória (e, se lida, em disco) achou.
pub struct GlobalSearch<'a> {
    /// Veredito da busca em memória (já considera bytes ilegíveis).
    pub mem_verdict: Verdict,
    pub mem: &'a [Occurrence],
    /// Ocorrências em disco, em endereços da base atual; `None` se o disco não foi lido (todos os
    /// patches têm `rva`): então o alvo da memória vale sozinho.
    pub disk: Option<&'a [Occurrence]>,
}

/// Alvo de cada ocorrência com `disp32` legível, e o veredito sobre esses alvos.
fn decide(def: &GlobalDef, occs: &[Occurrence], verdict: Verdict) -> GlobalVerdict {
    let targets: Vec<u64> = occs
        .iter()
        .filter_map(|occ| {
            let disp = occ.disp?;
            rip_target(occ.addr, disp, def.pattern.insn_end, def.pattern.adjust)
        })
        .collect();
    if targets.is_empty() && !occs.is_empty() && verdict != Verdict::None {
        // As ocorrências existem, mas nenhum `disp32` pôde ser lido: nada a afirmar.
        return GlobalVerdict::Inconclusive;
    }
    decide_global(&targets, verdict)
}

/// Resolve uma global (spec 6.2). Ordem das regras:
/// 1. a memória decide pelo `decide_global`: 0 ocorrências é `não encontrado`, alvos diferentes
///    são `ambíguo`, ilegível sem ocorrência é `inconclusivo`;
/// 2. com o disco lido, o alvo tem de ser o mesmo nos dois, senão `inconclusivo` (`ambíguo` se o
///    disco tem alvos diferentes);
/// 3. o alvo tem de cair numa seção de dados (`data`), senão `não encontrado`;
/// 4. com `rva`, o alvo tem de ser `module_base + rva` (`Verified`), senão `fora do lugar`;
///    sem `rva`, `Unverified`.
pub fn resolve_global(
    def: &GlobalDef,
    module_base: u64,
    data: &[Range<u64>],
    search: &GlobalSearch,
) -> GlobalState {
    let target = match decide(def, search.mem, search.mem_verdict) {
        GlobalVerdict::Found(target) => target,
        other => return GlobalState::Unavailable(other.label()),
    };
    if let Some(disk) = search.disk {
        let verdict = Verdict::decide(disk.len(), 0, true);
        match decide(def, disk, verdict) {
            GlobalVerdict::Found(on_disk) if on_disk == target => {}
            GlobalVerdict::Ambiguous(_) => {
                return GlobalState::Unavailable(GlobalVerdict::Ambiguous(Vec::new()).label());
            }
            _ => {
                return GlobalState::Unavailable(GlobalVerdict::Inconclusive.label());
            }
        }
    }
    if !data.iter().any(|range| range.contains(&target)) {
        return GlobalState::Unavailable(GlobalVerdict::NotFound.label());
    }
    match def.rva {
        Some(rva) if module_base.checked_add(rva) == Some(target) => GlobalState::Verified(target),
        Some(_) => GlobalState::Unavailable(MISPLACED),
        None => GlobalState::Unverified(target),
    }
}

/// Seções de dados (sem a flag de execução) do cabeçalho PE lido da memória, em endereços do
/// processo e cortadas na extensão do módulo.
pub fn data_ranges(module: &Module, memory: &PeHeaders) -> Vec<Range<u64>> {
    let limit = module.end();
    memory
        .sections
        .iter()
        .filter(|section| !section.is_executable())
        .filter_map(|section| {
            let start = module.base.checked_add(u64::from(section.rva))?;
            let end = start
                .checked_add(u64::from(section.virtual_size))?
                .min(limit);
            (start < end).then_some(start..end)
        })
        .collect()
}

/// Monta o [`UeScan`]: com as duas globais disponíveis, chama `check(gobjects, names)` (a
/// conferência barata); senão deixa `Err("globais indisponíveis")` sem chamá-la.
///
/// O `check` devolve `Err` externo para erro de processo (processo trocado ou morto), que sobe
/// sem virar `reflexão não reconhecida`; o `Err` interno é o detalhe da conferência.
pub fn build_scan(
    sigs: UeSigs,
    gobjects: GlobalState,
    names: GlobalState,
    check: impl FnOnce(u64, u64) -> Result<std::result::Result<(), String>>,
) -> Result<UeScan> {
    let cheap = match (gobjects.addr(), names.addr()) {
        (Some(gobjects_addr), Some(names_addr)) => check(gobjects_addr, names_addr)?,
        _ => Err("globais indisponíveis".to_string()),
    };
    Ok(UeScan {
        sigs,
        gobjects,
        names,
        cheap,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gt_core::global::GlobalPattern;
    use gt_core::memscan::scan_bytes;
    use gt_core::pe::Section;
    use gt_core::scan::Pattern;

    use crate::signatures::Signatures;

    const BASE: u64 = 0x1_4000_0000;
    /// Global resolvida nos testes: dentro da `.data` sintética (`BASE + 0x3000..0x4000`).
    const TARGET: u64 = BASE + 0x3010;
    const RVA: u64 = 0x3010;

    fn def(rva: Option<u64>) -> GlobalDef {
        GlobalDef {
            pattern: GlobalPattern {
                pattern: Pattern::parse("48 8b 05 ?? ?? ?? ?? 48").unwrap(),
                disp_offset: 3,
                insn_end: 7,
                adjust: 0,
            },
            rva,
        }
    }

    fn data() -> Vec<Range<u64>> {
        std::iter::once(BASE + 0x3000..BASE + 0x4000).collect()
    }

    /// Ocorrência em `hit` cujo `disp32` leva a `target` (instrução de 7 bytes, sem `adjust`).
    fn occ(hit: u64, target: u64) -> Occurrence {
        let disp = i32::try_from(i128::from(target) - i128::from(hit) - 7).unwrap();
        Occurrence {
            addr: hit,
            disp: Some(disp),
        }
    }

    fn state(
        def: &GlobalDef,
        mem: &[Occurrence],
        disk: Option<&[Occurrence]>,
        data: &[Range<u64>],
    ) -> GlobalState {
        let search = GlobalSearch {
            mem_verdict: Verdict::decide(mem.len(), 0, false),
            mem,
            disk,
        };
        resolve_global(def, BASE, data, &search)
    }

    #[test]
    fn global_verified_with_rva() {
        let mem = [occ(BASE + 0x1000, TARGET), occ(BASE + 0x1100, TARGET)];
        let disk = mem;
        assert_eq!(
            state(&def(Some(RVA)), &mem, Some(&disk), &data()),
            GlobalState::Verified(TARGET)
        );
        // Sem a busca em disco, a memória vale sozinha.
        assert_eq!(
            state(&def(Some(RVA)), &mem, None, &data()),
            GlobalState::Verified(TARGET)
        );
    }

    #[test]
    fn global_out_of_place() {
        let mem = [occ(BASE + 0x1000, TARGET)];
        assert_eq!(
            state(&def(Some(RVA + 8)), &mem, None, &data()),
            GlobalState::Unavailable("fora do lugar")
        );
    }

    #[test]
    fn global_unverified_without_version() {
        let mem = [occ(BASE + 0x1000, TARGET)];
        assert_eq!(
            state(&def(None), &mem, Some(&mem), &data()),
            GlobalState::Unverified(TARGET)
        );
    }

    #[test]
    fn global_target_in_text_is_not_found() {
        let in_text = BASE + 0x1800;
        let mem = [occ(BASE + 0x1000, in_text)];
        assert_eq!(
            state(&def(Some(in_text - BASE)), &mem, None, &data()),
            GlobalState::Unavailable("não encontrado")
        );
        assert_eq!(
            state(&def(None), &mem, None, &data()),
            GlobalState::Unavailable("não encontrado")
        );
    }

    #[test]
    fn global_disk_and_memory_disagree_inconclusive() {
        let mem = [occ(BASE + 0x1000, TARGET)];
        let disk = [occ(BASE + 0x1000, TARGET + 8)];
        assert_eq!(
            state(&def(None), &mem, Some(&disk), &data()),
            GlobalState::Unavailable("inconclusivo")
        );
        // Disco sem a ocorrência: também não concorda.
        assert_eq!(
            state(&def(None), &mem, Some(&[]), &data()),
            GlobalState::Unavailable("inconclusivo")
        );
    }

    #[test]
    fn global_ambiguous_not_found_and_inconclusive() {
        let two = [occ(BASE + 0x1000, TARGET), occ(BASE + 0x1100, TARGET + 8)];
        assert_eq!(
            state(&def(None), &two, None, &data()),
            GlobalState::Unavailable("ambíguo")
        );
        assert_eq!(
            state(&def(None), &[], None, &data()),
            GlobalState::Unavailable("não encontrado")
        );
        // Bytes ilegíveis sem nenhuma ocorrência legível.
        let search = GlobalSearch {
            mem_verdict: Verdict::InconclusiveNone,
            mem: &[],
            disk: None,
        };
        assert_eq!(
            resolve_global(&def(None), BASE, &data(), &search),
            GlobalState::Unavailable("inconclusivo")
        );
        // `disp32` ilegível em todas as ocorrências: nada a afirmar.
        let blind = [Occurrence {
            addr: BASE + 0x1000,
            disp: None,
        }];
        assert_eq!(
            state(&def(None), &blind, None, &data()),
            GlobalState::Unavailable("inconclusivo")
        );
    }

    #[test]
    fn global_many_agreeing_hits_resolve() {
        let mem: Vec<Occurrence> = (0..281)
            .map(|i| occ(BASE + 0x1000 + i * 0x10, TARGET))
            .collect();
        assert_eq!(
            state(&def(Some(RVA)), &mem, Some(&mem), &data()),
            GlobalState::Verified(TARGET)
        );
    }

    #[test]
    fn global_adjust_is_applied_before_rva() {
        let mut with_adjust = def(Some(RVA));
        with_adjust.pattern.adjust = 0x10;
        // A estrutura começa em TARGET - 0x10; o leitor usa TARGET.
        let mem = [occ(BASE + 0x1000, TARGET - 0x10)];
        assert_eq!(
            state(&with_adjust, &mem, None, &data()),
            GlobalState::Verified(TARGET)
        );
    }

    #[test]
    fn global_from_buffers_end_to_end() {
        // `.text` sintética em BASE + 0x1000 com duas ocorrências do padrão, ambas para TARGET.
        let mut text = vec![0x90u8; 0x100];
        for hit in [0x20usize, 0x80] {
            let insn_end = BASE + 0x1000 + hit as u64 + 7;
            let disp = i32::try_from(TARGET - insn_end).unwrap();
            text[hit..hit + 3].copy_from_slice(&[0x48, 0x8b, 0x05]);
            text[hit + 3..hit + 7].copy_from_slice(&disp.to_le_bytes());
            text[hit + 7] = 0x48;
        }
        let def = def(Some(RVA));
        let regions = [(BASE + 0x1000, text.as_slice())];
        let hits = scan_bytes(&regions, std::slice::from_ref(&def.pattern.pattern));
        assert_eq!(hits[0], vec![BASE + 0x1020, BASE + 0x1080]);
        let read = |addr: u64| {
            let at = usize::try_from(addr - (BASE + 0x1000)).ok()?;
            disp_at(&text, at).map(i32::to_le_bytes)
        };
        let occs = occurrences(&hits[0], def.pattern.disp_offset, read);
        assert!(occs.iter().all(|o| o.disp.is_some()));
        assert_eq!(
            state(&def, &occs, Some(&occs), &data()),
            GlobalState::Verified(TARGET)
        );
    }

    #[test]
    fn disp_at_bounds() {
        assert_eq!(disp_at(&[1, 0, 0, 0], 0), Some(1));
        assert_eq!(disp_at(&[0xff, 0xff, 0xff, 0xff], 0), Some(-1));
        assert_eq!(disp_at(&[1, 0, 0], 0), None);
        assert_eq!(disp_at(&[0; 8], usize::MAX), None);
    }

    fn section(name: &str, rva: u32, vsize: u32, characteristics: u32) -> Section {
        Section {
            name: name.into(),
            rva,
            virtual_size: vsize,
            raw_size: vsize,
            raw_offset: 0x400,
            characteristics,
        }
    }

    #[test]
    fn data_ranges_are_non_executable_sections_within_module() {
        let headers = PeHeaders {
            image_base: BASE,
            size_of_image: 0x6000,
            time_date_stamp: 1,
            size_of_headers: 0x400,
            sections: vec![
                section(".text", 0x1000, 0x2000, 0x6000_0020),
                section(".data", 0x3000, 0x1000, 0xC000_0040),
                section(".rdata", 0x4000, 0x4000, 0x4000_0040),
            ],
        };
        let module = Module {
            base: BASE,
            size: 0x6000,
            memory_size: 0x6000,
            ..Module::default()
        };
        assert_eq!(
            data_ranges(&module, &headers),
            vec![BASE + 0x3000..BASE + 0x4000, BASE + 0x4000..BASE + 0x6000]
        );
    }

    fn ue_sigs() -> UeSigs {
        Signatures::embedded().unwrap().ue_for_version("x").unwrap()
    }

    #[test]
    fn cheap_check_skipped_without_globals() {
        let called = std::cell::Cell::new(false);
        let check = |_: u64, _: u64| {
            called.set(true);
            Ok(Ok(()))
        };
        let scan = build_scan(
            ue_sigs(),
            GlobalState::Unavailable("não encontrado"),
            GlobalState::Verified(TARGET),
            check,
        )
        .unwrap();
        assert!(!called.get());
        assert_eq!(scan.cheap, Err("globais indisponíveis".to_string()));
        let scan = build_scan(
            ue_sigs(),
            GlobalState::Verified(TARGET),
            GlobalState::Unavailable("ambíguo"),
            check,
        )
        .unwrap();
        assert!(!called.get());
        assert_eq!(scan.cheap, Err("globais indisponíveis".to_string()));
    }

    #[test]
    fn cheap_check_runs_with_both_globals_and_passes_addresses() {
        let seen = std::cell::Cell::new((0u64, 0u64));
        let scan = build_scan(
            ue_sigs(),
            GlobalState::Verified(0x10),
            GlobalState::Unverified(0x20),
            |g, n| {
                seen.set((g, n));
                Ok(Err("nome 0 errado".to_string()))
            },
        )
        .unwrap();
        assert_eq!(seen.get(), (0x10, 0x20));
        assert_eq!(scan.cheap, Err("nome 0 errado".to_string()));
        // Erro de processo sobe, em vez de virar `reflexão não reconhecida`.
        let err = build_scan(
            ue_sigs(),
            GlobalState::Verified(0x10),
            GlobalState::Verified(0x20),
            |_, _| Err(gt_core::Error::ProcessGone { pid: 1 }.into()),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            crate::Error::Core(gt_core::Error::ProcessGone { pid: 1 })
        ));
    }
}
