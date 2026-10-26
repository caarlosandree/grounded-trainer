//! `gt-patch profile`: amostra o `pc` das threads e conta por função (spec 6.9 e 6.10).
//!
//! Não escreve na memória do jogo. Pega a trava e grava só o marcador "pausado por nós" antes
//! de cada pausa, para que um SIGKILL no meio dela seja recuperado como os demais.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::time::Duration;

use gt_core::journal::Journal;
use gt_core::procinfo::Identity;
use gt_core::suspend::{PauseConfig, open_checked, sample_pcs};
use gt_grounded2::GAME_EXE;
use gt_grounded2::profile::{FunctionHits, aggregate};
use gt_grounded2::resolve::Target;

use super::{begin_and_show, location};

/// Quantas funções aparecem no resultado.
const TOP: usize = 20;

/// `true` se `a` e `b` são o mesmo processo (mesmo PID, horário de início e boot). Se o jogo
/// reiniciou entre a leitura do alvo e a abertura do `pidfd`, as funções e o mapa são de outro.
fn same_process(a: &Identity, b: &Identity) -> bool {
    a == b
}

/// Texto das funções mais amostradas: `módulo+início-fim  N amostras` e, abaixo, cada `pc`.
fn write_profile(
    out: &mut impl Write,
    hits: &[FunctionHits],
    label: impl Fn(u64) -> String,
) -> io::Result<()> {
    for hit in hits.iter().take(TOP) {
        writeln!(
            out,
            "{GAME_EXE}+{:x}-{:x}  {} amostras",
            hit.start_rva,
            hit.end_rva,
            hit.pcs.len()
        )?;
        let mut by_pc: BTreeMap<u64, usize> = BTreeMap::new();
        for &pc in &hit.pcs {
            *by_pc.entry(pc).or_default() += 1;
        }
        for (pc, count) in by_pc {
            if count > 1 {
                writeln!(out, "    {} ({count} vezes)", label(pc))?;
            } else {
                writeln!(out, "    {}", label(pc))?;
            }
        }
    }
    Ok(())
}

pub fn run(pid: Option<i32>, samples: u32, interval_ms: u64) -> anyhow::Result<()> {
    let session = begin_and_show()?;
    let target = Target::open(pid)?;
    let (module, mem_headers) = target.module(GAME_EXE)?;
    let functions = target.open_disk(module, &mem_headers)?.functions()?;
    let (pidfd, identity) = open_checked(target.pid)?;
    if !same_process(&target.identity, &identity) {
        anyhow::bail!("o jogo reiniciou enquanto o perfil era preparado; rode o profile de novo");
    }

    let base = Journal {
        boot_id: identity.boot_id,
        pid: identity.pid,
        start_time: identity.start_time,
        version_key: mem_headers.version_key(),
        paused_by_us: false,
        entries: Vec::new(),
        data: Vec::new(),
    };
    let pcs = sample_pcs(
        &pidfd,
        target.pid,
        &PauseConfig::default(),
        samples,
        Duration::from_millis(interval_ms),
        |will_stop| {
            session.store.save(&Journal {
                paused_by_us: will_stop,
                ..base.clone()
            })
        },
        || session.store.clear(),
        || session.shutdown.requested(),
    )?;

    let hits = aggregate(&pcs, module.base, &functions);
    let mut out = io::stdout().lock();
    if hits.is_empty() {
        writeln!(out, "nenhuma amostra caiu numa função do executável")?;
    } else {
        write_profile(&mut out, &hits, |pc| location(&target, pc))?;
    }
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_refuses_other_identity() {
        let id = Identity {
            pid: 42,
            start_time: 1000,
            boot_id: "boot".to_string(),
        };
        assert!(same_process(&id, &id.clone()));
        let reborn = Identity {
            start_time: 1001,
            ..id.clone()
        };
        assert!(!same_process(&id, &reborn));
        let other_boot = Identity {
            boot_id: "outro".to_string(),
            ..id.clone()
        };
        assert!(!same_process(&id, &other_boot));
    }

    #[test]
    fn write_profile_lists_functions_and_pcs_with_counts() {
        let base = 0x1_4000_0000;
        let hits = [FunctionHits {
            start_rva: 0x1040,
            end_rva: 0x1100,
            pcs: vec![base + 0x1050, base + 0x1060, base + 0x1050],
        }];
        let mut out = Vec::new();
        write_profile(&mut out, &hits, |pc| format!("m+{:x}", pc - base)).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(
            text,
            format!("{GAME_EXE}+1040-1100  3 amostras\n    m+1050 (2 vezes)\n    m+1060\n")
        );
    }

    #[test]
    fn write_profile_shows_only_the_top_functions() {
        let base = 0x1_4000_0000;
        let hits: Vec<FunctionHits> = (0..30u32)
            .map(|i| FunctionHits {
                start_rva: 0x1000 + i * 0x10,
                end_rva: 0x1010 + i * 0x10,
                pcs: vec![base + u64::from(0x1000 + i * 0x10)],
            })
            .collect();
        let mut out = Vec::new();
        write_profile(&mut out, &hits, |pc| format!("{pc:#x}")).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.matches("amostras").count(), TOP);
    }
}
