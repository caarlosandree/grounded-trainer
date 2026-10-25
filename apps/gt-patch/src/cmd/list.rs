//! `gt-patch list`: versão, estado de cada patch e diário pendente. Não escreve nada nem
//! pega a trava (spec 6.4 e 6.10).

use std::io::{self, Write};

use gt_core::journal::Loaded;
use gt_core::procinfo::identity;
use gt_grounded2::catalog::{self, CatalogScan};
use gt_grounded2::resolve::Target;
use gt_grounded2::signatures::Signatures;
use gt_grounded2::state::PatchState;

use super::{journal_store, location};

/// `módulo+offset` do patch no estado dado, ou `-` quando o estado não tem lugar.
fn state_location(target: &Target, scan: &CatalogScan, state: &PatchState) -> String {
    match state {
        PatchState::Verified { addr }
        | PatchState::Unverified { addr }
        | PatchState::Active { addr }
        | PatchState::Pending { addr }
        | PatchState::BytesDiffer { addr, .. } => location(target, *addr),
        PatchState::Misplaced { found_rva } => format!("{}+{found_rva:x}", scan.module_name),
        _ => "-".to_string(),
    }
}

pub fn run(pid: Option<i32>) -> anyhow::Result<()> {
    let target = Target::open(pid)?;
    let store = journal_store()?;
    let loaded = store.peek()?;
    let locked = store.is_locked()?;
    let journal = match &loaded {
        Loaded::Ok(journal) => Some(journal),
        _ => None,
    };
    let sigs = Signatures::embedded()?;
    let scan = catalog::scan(&target, &sigs, journal, locked)?;

    let mut out = io::stdout().lock();
    writeln!(out, "Chave de versão: {}", scan.version_key)?;
    if scan.has_version_file {
        writeln!(out, "Assinaturas: conferidas para esta versão")?;
    } else {
        writeln!(
            out,
            "Assinaturas: versão sem arquivo (patches marcados como não verificados)"
        )?;
    }
    for entry in &scan.entries {
        writeln!(
            out,
            "{}  {}  {}",
            entry.def.id,
            entry.state.label(),
            state_location(&target, &scan, &entry.state)
        )?;
    }
    // Só vale o diário do mesmo jogo, ainda vivo, e sem outro `gt-patch` cuidando dele.
    let same_game = match journal {
        Some(journal) => {
            let id = identity(target.pid)?;
            journal.pid == id.pid
                && journal.start_time == id.start_time
                && journal.boot_id == id.boot_id
        }
        None => false,
    };
    if same_game && !locked {
        writeln!(out, "Diário pendente: sim (rode gt-patch restore)")?;
    }
    if let Loaded::Corrupt { moved_to } = &loaded {
        writeln!(
            out,
            "Diário ilegível (rode gt-patch restore; ele vai para {})",
            moved_to.display()
        )?;
    }
    out.flush()?;
    Ok(())
}
