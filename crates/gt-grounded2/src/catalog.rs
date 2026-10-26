//! Catálogo de patches do processo: estado de cada definição e seleção para aplicar (spec 6.4).
//!
//! [`resolve`] é a parte pura (testável com buffers sintéticos); [`scan`] lê do processo e do
//! disco e chama [`resolve`].

use std::path::PathBuf;

use gt_core::guard;
use gt_core::journal::Journal;
use gt_core::memscan::{MemScan, Verdict, scan_bytes, scan_memory};
use gt_core::modules::{Module, open_in_root};
use gt_core::pe::{PeHeaders, check_same_version};
use gt_core::procinfo::{Identity, identity};
use gt_core::scan::Pattern;

use crate::policy::exec_ranges;
use crate::resolve::Target;
use crate::signatures::{PatchDef, Signatures};
use crate::state::{PatchState, StateInputs, compute_state};
use crate::{Error, GAME_EXE, Result};

/// Uma definição e o estado dela no processo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub def: PatchDef,
    pub state: PatchState,
}

/// Resultado da varredura do catálogo.
#[derive(Debug, Clone)]
pub struct CatalogScan {
    /// Chave de versão do módulo na memória.
    pub version_key: String,
    /// A versão tem arquivo em `signatures/`.
    pub has_version_file: bool,
    /// Módulo onde os patches valem.
    pub module_name: String,
    pub entries: Vec<Entry>,
    /// O módulo, para o cálculo da política de escrita (usado pelo `open_engine`).
    pub module: Module,
    /// Cabeçalho do arquivo em disco, já conferido com a memória. `None` se não pôde ser lido
    /// ou se a versão difere (sem ele não se escreve, spec 6.6).
    pub disk: Option<PeHeaders>,
}

/// Calcula o estado de cada definição a partir de leituras já feitas.
///
/// `mem` e, se `disk` for `Ok(Some(_))`, a lista de ocorrências em disco têm um item por
/// definição, na mesma ordem de `defs`. `disk` é `Err(causa)` quando o disco não pôde ser lido
/// e `Ok(None)` quando nenhuma definição precisava da busca em disco. `current(addr, len)` lê
/// os bytes da memória do alvo. O diário só vale se a identidade bate com `journal.1` e os
/// bytes no endereço registrado são os novos (ou, numa troca de valor em curso, os de `next`); `locked` diz se outro `gt-patch` segura a trava.
pub fn resolve(
    defs: &[PatchDef],
    module_base: u64,
    mem: &[MemScan],
    disk: std::result::Result<Option<Vec<Vec<u64>>>, String>,
    current: impl Fn(u64, usize) -> Option<Vec<u8>>,
    journal: Option<(&Journal, &Identity)>,
    locked: bool,
) -> Vec<Entry> {
    // Só vale o diário do mesmo processo (PID, horário de início e boot).
    let journal = journal.filter(|(j, id)| {
        j.pid == id.pid && j.start_time == id.start_time && j.boot_id == id.boot_id
    });
    let disk_err = disk.as_ref().err().cloned();
    let disk_lists = disk.ok().flatten();
    defs.iter()
        .enumerate()
        .map(|(i, def)| {
            let journal_hit = journal.and_then(|(j, _)| {
                let entry = j.entries.iter().find(|e| e.id == def.id)?;
                let now = current(entry.addr, entry.replacement.len())?;
                // Numa troca de valor em curso, os bytes atuais podem ser os de `next`.
                (now == entry.replacement || entry.next.as_deref() == Some(now.as_slice()))
                    .then_some((entry.addr, locked))
            });
            let memory = &mem[i];
            let current_bytes = match (memory.verdict, memory.hits.first()) {
                (Verdict::Unique, Some(&hit)) => {
                    current(hit + def.offset as u64, def.expected.len())
                }
                _ => None,
            };
            let inputs = StateInputs {
                journal_hit,
                disk: disk_err.clone().map_or(Ok(()), Err),
                memory,
                disk_hits: disk_lists
                    .as_ref()
                    .and_then(|lists| lists.get(i))
                    .map(Vec::as_slice),
                module_base,
                current: current_bytes.as_deref(),
            };
            Entry {
                def: def.clone(),
                state: compute_state(def, &inputs),
            }
        })
        .collect()
}

/// Lê o estado de todos os patches de `sigs` no processo `target`.
///
/// Busca o padrão de todos os patches numa única passada pelas seções executáveis do módulo
/// (as do disco, spec 6.6), depois de passar cada uma pela guarda de módulos. O disco inteiro só
/// é lido se algum patch não tem `rva`. Falha na leitura do cabeçalho do disco, ou versão
/// diferente da memória, deixa todos os patches em `DiskUnreadable`.
pub fn scan(
    target: &Target,
    sigs: &Signatures,
    journal: Option<&Journal>,
    locked: bool,
) -> Result<CatalogScan> {
    let (module, mem_headers) = target.module(GAME_EXE)?;
    let version_key = mem_headers.version_key();
    let has_version_file = sigs.has_version(&version_key);
    let defs = sigs.for_version(&version_key);

    let identity = match journal {
        Some(_) => Some(identity(target.pid)?),
        None => None,
    };
    let journal_arg = journal.zip(identity.as_ref());
    // O endereço do diário vem de um arquivo: a guarda vale também para ele.
    let current = |addr: u64, len: usize| {
        guard::check(
            &target.maps,
            &target.modules,
            &target.non_pe,
            addr,
            len as u64,
        )
        .ok()?;
        let mut buf = vec![0u8; len];
        target.reader.read_exact(addr, &mut buf).ok()?;
        Some(buf)
    };

    let root = PathBuf::from(format!("/proc/{}/root", target.pid));
    let disk_headers = open_in_root(&root, &module.path)
        .map_err(|err| err.to_string())
        .and_then(|disk| {
            check_same_version(&mem_headers, &disk)
                .map(|()| disk)
                .map_err(|_| "versão do disco difere da memória".to_string())
        });

    let (entries, disk) = match disk_headers {
        Err(cause) => {
            let empty: Vec<MemScan> = defs
                .iter()
                .map(|_| MemScan {
                    hits: Vec::new(),
                    unreadable_bytes: 0,
                    verdict: Verdict::None,
                })
                .collect();
            let entries = resolve(
                &defs,
                module.base,
                &empty,
                Err(cause),
                current,
                journal_arg,
                locked,
            );
            (entries, None)
        }
        Ok(headers) => {
            let ranges = exec_ranges(module, &headers);
            for range in &ranges {
                guard::check(
                    &target.maps,
                    &target.modules,
                    &target.non_pe,
                    range.start,
                    range.end - range.start,
                )?;
            }
            let patterns: Vec<Pattern> = defs.iter().map(|d| d.pattern.clone()).collect();
            let mem = if patterns.is_empty() {
                Vec::new()
            } else {
                scan_memory(&target.reader, &ranges, &patterns)?
            };
            let disk_hits = if defs.iter().any(|d| d.rva.is_none()) {
                disk_hits(target, module, &mem_headers, &defs, &ranges).map(Some)
            } else {
                Ok(None)
            };
            let entries = resolve(
                &defs,
                module.base,
                &mem,
                disk_hits,
                current,
                journal_arg,
                locked,
            );
            (entries, Some(headers))
        }
    };
    Ok(CatalogScan {
        version_key,
        has_version_file,
        module_name: GAME_EXE.to_string(),
        entries,
        module: module.clone(),
        disk,
    })
}

/// Ocorrências em disco dos padrões dos patches sem `rva`, nas mesmas seções da busca em
/// memória e em endereços da base atual. Os patches com `rva` ficam com lista vazia.
fn disk_hits(
    target: &Target,
    module: &Module,
    mem_headers: &PeHeaders,
    defs: &[PatchDef],
    ranges: &[std::ops::Range<u64>],
) -> std::result::Result<Vec<Vec<u64>>, String> {
    let image = target
        .open_disk(module, mem_headers)
        .map_err(|err| err.to_string())?;
    let mut regions = Vec::with_capacity(ranges.len());
    for range in ranges {
        let rva = u32::try_from(range.start - module.base)
            .map_err(|_| "seção fora da imagem em disco".to_string())?;
        let len = usize::try_from(range.end - range.start)
            .map_err(|_| "seção fora da imagem em disco".to_string())?;
        let bytes = image
            .bytes(rva, len)
            .ok_or_else(|| "seção fora da imagem em disco".to_string())?;
        regions.push((range.start, bytes));
    }
    let wanted: Vec<usize> = (0..defs.len()).filter(|&i| defs[i].rva.is_none()).collect();
    let patterns: Vec<Pattern> = wanted.iter().map(|&i| defs[i].pattern.clone()).collect();
    let found = scan_bytes(&regions, &patterns);
    let mut out = vec![Vec::new(); defs.len()];
    for (i, hits) in wanted.into_iter().zip(found) {
        out[i] = hits;
    }
    Ok(out)
}

/// Escolhe os patches de `ids` para aplicar: definição, endereço e se é "não verificado".
///
/// Um `id` desconhecido ([`Error::UnknownPatch`]) ou com estado que não permite aplicar
/// ([`Error::Unavailable`]) recusa tudo; nada sai parcial. `id` repetido vale uma vez.
pub fn select(scan: &CatalogScan, ids: &[String]) -> Result<Vec<(PatchDef, u64, bool)>> {
    let mut found: Vec<&Entry> = Vec::with_capacity(ids.len());
    for id in ids {
        let entry = scan
            .entries
            .iter()
            .find(|e| &e.def.id == id)
            .ok_or_else(|| Error::UnknownPatch { id: id.clone() })?;
        if !found.iter().any(|e| e.def.id == entry.def.id) {
            found.push(entry);
        }
    }
    found
        .into_iter()
        .map(|entry| match entry.state.apply_addr() {
            Some(addr) => Ok((entry.def.clone(), addr, entry.state.is_unverified())),
            None => Err(Error::Unavailable {
                id: entry.def.id.clone(),
                state: entry.state.label().to_string(),
            }),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signatures::Signatures;
    use gt_core::journal::JournalEntry;

    const BASE: u64 = 0x1_4000_0000;
    const HIT: u64 = BASE + 0x1000;
    const ORIGINAL: [u8; 3] = [0x48, 0x8B, 0xF9];
    const NEW: [u8; 3] = [0x48, 0x89, 0xCF];

    const GENERIC: &str = r#"
[[patch]]
id = "teste-codificacao"
description = "Troca sem efeito"
pattern = "48 8B F9 ?? 90"
offset = 0
expected = "48 8B F9"
replacement = "48 89 CF"

[[patch]]
id = "outro"
description = "Outro patch"
pattern = "90 90 ?? C3"
offset = 2
expected = "11"
replacement = "22"
"#;

    const VERSION: &str = r#"
version = "aaaa-bbbb"

[[patch]]
id = "teste-codificacao"
rva = 0x1000
"#;

    fn unique(hit: u64) -> MemScan {
        MemScan {
            hits: vec![hit],
            unreadable_bytes: 0,
            verdict: Verdict::Unique,
        }
    }

    fn none() -> MemScan {
        MemScan {
            hits: Vec::new(),
            unreadable_bytes: 0,
            verdict: Verdict::None,
        }
    }

    /// Memória sintética: os bytes originais em `HIT`, e os novos em `HIT + 0x100`.
    fn memory(addr: u64, len: usize) -> Option<Vec<u8>> {
        match addr {
            HIT => Some(ORIGINAL[..len].to_vec()),
            a if a == HIT + 0x100 => Some(NEW[..len].to_vec()),
            _ => None,
        }
    }

    fn identity(start_time: u64) -> Identity {
        Identity {
            pid: 10,
            start_time,
            boot_id: "boot".into(),
        }
    }

    fn journal() -> Journal {
        Journal {
            boot_id: "boot".into(),
            pid: 10,
            start_time: 5,
            version_key: "aaaa-bbbb".into(),
            paused_by_us: false,
            entries: vec![JournalEntry {
                id: "teste-codificacao".into(),
                addr: HIT + 0x100,
                original: ORIGINAL.to_vec(),
                replacement: NEW.to_vec(),
                next: None,
            }],
        }
    }

    #[test]
    fn resolve_chain_from_toml() {
        let sigs =
            Signatures::parse(&[("generic.toml", GENERIC), ("aaaa-bbbb.toml", VERSION)]).unwrap();
        let defs = sigs.for_version("aaaa-bbbb");
        let entries = resolve(
            &defs,
            BASE,
            &[unique(HIT), none()],
            Ok(None),
            memory,
            None,
            false,
        );
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].def.id, "teste-codificacao");
        assert_eq!(entries[0].state, PatchState::Verified { addr: HIT });
        assert_eq!(entries[1].state, PatchState::NotFound);

        // Versão sem arquivo: precisa do disco, que acha a mesma ocorrência.
        let defs = sigs.for_version("cccc-dddd");
        let entries = resolve(
            &defs,
            BASE,
            &[unique(HIT), none()],
            Ok(Some(vec![vec![HIT], vec![]])),
            memory,
            None,
            false,
        );
        assert_eq!(entries[0].state, PatchState::Unverified { addr: HIT });

        // Disco ilegível: todos indisponíveis, com a causa.
        let entries = resolve(
            &defs,
            BASE,
            &[unique(HIT), none()],
            Err("sem arquivo".into()),
            memory,
            None,
            false,
        );
        assert!(
            entries
                .iter()
                .all(|e| e.state == PatchState::DiskUnreadable("sem arquivo".into()))
        );
    }

    #[test]
    fn resolve_uses_journal_of_same_process() {
        let defs = Signatures::parse(&[("generic.toml", GENERIC)])
            .unwrap()
            .for_version("x");
        let mem = [unique(HIT), none()];
        let (j, id) = (journal(), identity(5));
        let state = |locked| {
            resolve(&defs, BASE, &mem, Ok(None), memory, Some((&j, &id)), locked)[0]
                .state
                .clone()
        };
        assert_eq!(state(false), PatchState::Pending { addr: HIT + 0x100 });
        assert_eq!(state(true), PatchState::Active { addr: HIT + 0x100 });
    }

    #[test]
    fn resolve_ignores_journal_of_other_process() {
        let sigs =
            Signatures::parse(&[("generic.toml", GENERIC), ("aaaa-bbbb.toml", VERSION)]).unwrap();
        let defs = sigs.for_version("aaaa-bbbb");
        let mem = [unique(HIT), none()];
        let j = journal();
        // Outro `start_time`: o PID foi reaproveitado, o diário não vale.
        let other_start = identity(6);
        let entries = resolve(
            &defs,
            BASE,
            &mem,
            Ok(None),
            memory,
            Some((&j, &other_start)),
            false,
        );
        assert_eq!(entries[0].state, PatchState::Verified { addr: HIT });
        // Outro `boot_id` também não vale.
        let other_boot = Identity {
            boot_id: "outro".into(),
            ..identity(5)
        };
        let entries = resolve(
            &defs,
            BASE,
            &mem,
            Ok(None),
            memory,
            Some((&j, &other_boot)),
            false,
        );
        assert_eq!(entries[0].state, PatchState::Verified { addr: HIT });
        // Mesmo processo, mas os bytes no endereço não são os novos: o diário não vale.
        let mut stale = journal();
        stale.entries[0].addr = HIT;
        let same = identity(5);
        let entries = resolve(
            &defs,
            BASE,
            &mem,
            Ok(None),
            memory,
            Some((&stale, &same)),
            false,
        );
        assert_eq!(entries[0].state, PatchState::Verified { addr: HIT });
    }

    #[test]
    fn journal_hit_accepts_next() {
        let defs = Signatures::parse(&[("generic.toml", GENERIC)])
            .unwrap()
            .for_version("x");
        let mem = [unique(HIT), none()];
        let id = identity(5);
        // Troca de valor interrompida: o diário guarda o valor antigo em `replacement`
        // (o que não está no jogo agora) e os bytes atuais são os de `next`.
        let mut j = journal();
        j.entries[0].replacement = vec![0x48, 0x90, 0x90];
        j.entries[0].next = Some(NEW.to_vec());
        let state = |j: &Journal, locked| {
            resolve(&defs, BASE, &mem, Ok(None), memory, Some((j, &id)), locked)[0]
                .state
                .clone()
        };
        assert_eq!(state(&j, false), PatchState::Pending { addr: HIT + 0x100 });
        assert_eq!(state(&j, true), PatchState::Active { addr: HIT + 0x100 });
        // Bytes que não são nem os de `replacement` nem os de `next`: o diário não vale.
        j.entries[0].next = Some(vec![0x48, 0x89, 0xCE]);
        assert!(!matches!(
            state(&j, true),
            PatchState::Active { .. } | PatchState::Pending { .. }
        ));
    }

    fn catalog(states: Vec<(&str, PatchState)>) -> CatalogScan {
        let sigs = Signatures::parse(&[("generic.toml", GENERIC)]).unwrap();
        let defs = sigs.for_version("x");
        let entries = states
            .into_iter()
            .map(|(id, state)| Entry {
                def: defs.iter().find(|d| d.id == id).unwrap().clone(),
                state,
            })
            .collect();
        CatalogScan {
            version_key: "x".into(),
            has_version_file: false,
            module_name: GAME_EXE.into(),
            entries,
            module: Module::default(),
            disk: None,
        }
    }

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn select_returns_addr_and_unverified_flag() {
        let scan = catalog(vec![
            ("teste-codificacao", PatchState::Verified { addr: 0x10 }),
            ("outro", PatchState::Unverified { addr: 0x20 }),
        ]);
        let picked = select(&scan, &ids(&["outro", "teste-codificacao", "outro"])).unwrap();
        let summary: Vec<_> = picked
            .iter()
            .map(|(def, addr, unverified)| (def.id.as_str(), *addr, *unverified))
            .collect();
        assert_eq!(
            summary,
            vec![("outro", 0x20, true), ("teste-codificacao", 0x10, false)]
        );
    }

    #[test]
    fn select_refuses_if_any_unavailable() {
        let scan = catalog(vec![
            ("teste-codificacao", PatchState::Verified { addr: 0x10 }),
            ("outro", PatchState::NotFound),
        ]);
        let err = select(&scan, &ids(&["teste-codificacao", "outro"])).unwrap_err();
        assert!(matches!(
            &err,
            Error::Unavailable { id, state } if id == "outro" && state == "não encontrado"
        ));
        assert_eq!(err.to_string(), "patch outro indisponível: não encontrado");
    }

    #[test]
    fn select_unknown_id() {
        let scan = catalog(vec![(
            "teste-codificacao",
            PatchState::Verified { addr: 0x10 },
        )]);
        // O desconhecido vence mesmo com um indisponível antes dele na lista.
        let scan_two = catalog(vec![
            ("teste-codificacao", PatchState::NotFound),
            ("outro", PatchState::Verified { addr: 0x20 }),
        ]);
        let err = select(&scan, &ids(&["teste-codificacao", "nao-existe"])).unwrap_err();
        assert!(matches!(&err, Error::UnknownPatch { id } if id == "nao-existe"));
        assert_eq!(err.to_string(), "patch desconhecido: nao-existe");
        let err = select(&scan_two, &ids(&["teste-codificacao", "nao-existe"])).unwrap_err();
        assert!(matches!(err, Error::UnknownPatch { .. }));
    }
}
