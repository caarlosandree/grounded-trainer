//! Subcomandos do `gt-patch` e o que eles compartilham: a abertura da sessão (spec 6.8), a
//! escolha dos patches e o texto do relatório de recuperação.

pub mod list;
pub mod observe;
pub mod profile;
pub mod restore;
pub mod run;
pub mod toggle;

use std::io::{self, Write};

use anyhow::Context;
use gt_core::engine::{BatchReport, Engine};
use gt_core::journal::{DiscardReason, JournalLock, JournalStore};
use gt_core::patch::{PatchKind, PatchSpec, Validated, validate};
use gt_core::recovery::{RecoveryReport, recover};
use gt_core::session::Shutdown;
use gt_core::suspend::PauseConfig;
use gt_grounded2::catalog::{self, CatalogScan};
use gt_grounded2::resolve::Target;
use gt_grounded2::signatures::Signatures;

/// Quem escreve ou pausa o jogo: tratador de sinais, diário e a trava dele.
///
/// A trava vive até o fim do comando, depois do `drop` do motor (que é local ao comando).
pub struct Session {
    pub shutdown: Shutdown,
    pub store: JournalStore,
    pub lock: JournalLock,
}

/// Armazenamento do diário no diretório de estado do usuário.
pub fn journal_store() -> anyhow::Result<JournalStore> {
    let dir = JournalStore::default_dir()
        .context("não achei onde guardar o diário; defina XDG_STATE_HOME ou HOME")?;
    Ok(JournalStore::new(dir))
}

/// Início de todo comando que escreve ou pausa: sinais → trava → recuperação (spec 6.8).
pub fn begin() -> anyhow::Result<(Session, RecoveryReport)> {
    let shutdown = Shutdown::install()?;
    let store = journal_store()?;
    let lock = store.lock()?;
    let report = recover(&store, &lock, &PauseConfig::default())?;
    Ok((
        Session {
            shutdown,
            store,
            lock,
        },
        report,
    ))
}

/// Como [`begin`], imprimindo o relatório da recuperação se ela fez alguma coisa, para os
/// comandos que seguem para o jogo (`run`, `toggle`, `profile`): recusa se a recuperação deixou
/// entradas no diário ([`ensure_recovered`]).
pub fn begin_and_show() -> anyhow::Result<Session> {
    let (session, report) = begin()?;
    if !report.is_empty() {
        print_recovery(&mut io::stdout().lock(), &report)?;
    }
    ensure_recovered(&report)?;
    Ok(session)
}

/// Entradas que a recuperação não resolveu continuam no diário; o comando não segue (um motor
/// novo regravaria o diário só com os patches dele e as apagaria). O erro traz a mensagem da
/// spec 6.7 para cada uma.
pub fn ensure_recovered(report: &RecoveryReport) -> anyhow::Result<()> {
    if report.kept.is_empty() {
        return Ok(());
    }
    let lines: Vec<String> = report.kept.iter().map(|id| unrestored_text(id)).collect();
    anyhow::bail!("{}", lines.join("\n"))
}

/// Por que o diário foi descartado.
fn discard_text(reason: DiscardReason) -> &'static str {
    match reason {
        DiscardReason::ProcessGone => "o jogo fechou",
        DiscardReason::OtherProcess => "o jogo reiniciou",
        DiscardReason::OtherBoot => "o sistema reiniciou",
    }
}

/// Linhas do relatório de recuperação. As entradas mantidas no diário (`kept`) não entram: vão
/// no erro de [`ensure_recovered`].
pub fn print_recovery(out: &mut impl Write, report: &RecoveryReport) -> io::Result<()> {
    if let Some(path) = &report.corrupt {
        writeln!(out, "diário corrompido movido para {}", path.display())?;
    }
    if let Some(reason) = report.discarded {
        writeln!(out, "diário descartado: {}", discard_text(reason))?;
    }
    if report.resumed {
        writeln!(out, "retomado o jogo parado")?;
    }
    if report.stopped_by_other {
        writeln!(
            out,
            "o jogo estava parado por outro processo; não foi retomado"
        )?;
    }
    for id in &report.restored {
        writeln!(out, "restaurado: {id}")?;
    }
    for id in &report.dropped {
        writeln!(out, "descartado: {id} (bytes já originais)")?;
    }
    for id in &report.foreign {
        writeln!(out, "não restaurado: {id} (bytes alterados por outro)")?;
    }
    out.flush()
}

/// Mensagem de um patch que não pôde ser desfeito agora (spec 6.7).
pub fn unrestored_text(id: &str) -> String {
    format!("não foi possível restaurar {id} agora; a próxima execução do gt-patch vai restaurar")
}

/// Avisa, em stderr, dos patches que a versão do jogo não conferiu. Falha ao escrever é ignorada.
pub fn warn_unverified(selected: &[Selected]) {
    let mut err = io::stderr().lock();
    for s in selected.iter().filter(|s| s.unverified) {
        let _ = writeln!(
            err,
            "aviso: a versão do jogo não tem assinaturas conferidas; {} foi aplicado como não verificado",
            s.patch.spec.id
        );
    }
}

/// Avisa, em stderr, dos patches que ficaram aplicados no jogo. Falha ao escrever é ignorada.
pub fn report_unrestored(engine: &Engine) {
    let mut err = io::stderr().lock();
    for spec in engine.active() {
        let _ = writeln!(err, "{}", unrestored_text(&spec.id));
    }
}

/// Fecha o comando depois do `undo_all` (spec 6.7): lista o que foi restaurado e o que não foi
/// (bytes de outro), trata o jogo que fechou no meio e deixa o aviso dos patches que ficaram.
///
/// Daqui em diante o terminal pode ter fechado: falha ao escrever em `out` não interrompe nada.
pub fn report_undone(
    engine: &Engine,
    undone: Result<BatchReport, gt_core::Error>,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    match undone {
        Ok(batch) => {
            for id in &batch.restored {
                let _ = writeln!(out, "restaurado: {id}");
            }
            for id in &batch.foreign {
                let _ = writeln!(out, "não restaurado: {id} (bytes alterados por outro)");
            }
            let _ = out.flush();
            Ok(())
        }
        Err(gt_core::Error::ProcessGone { .. }) => {
            let _ = writeln!(out, "o jogo fechou; nada a restaurar");
            let _ = out.flush();
            Ok(())
        }
        Err(err) => {
            report_unrestored(engine);
            Err(err.into())
        }
    }
}

/// Alvo aberto e o estado de cada patch do catálogo.
pub fn open_catalog(pid: Option<i32>) -> anyhow::Result<(Target, CatalogScan)> {
    let target = Target::open(pid)?;
    tracing::info!(pid = target.pid, "jogo encontrado");
    let sigs = Signatures::embedded()?;
    // O diário já foi recuperado: o que sobrou nele não vale para o estado dos patches.
    let scan = catalog::scan(&target, &sigs, None, false)?;
    Ok((target, scan))
}

/// Patch escolhido, validado, e se a versão do jogo não o conferiu.
pub struct Selected {
    pub patch: Validated,
    pub unverified: bool,
}

/// Escolhe e valida os patches de `ids`; qualquer `id` desconhecido ou indisponível recusa tudo.
pub fn select_patches(scan: &CatalogScan, ids: &[String]) -> anyhow::Result<Vec<Selected>> {
    catalog::select(scan, ids)?
        .into_iter()
        .map(|(def, addr, unverified)| {
            let patch = validate(PatchSpec {
                id: def.id,
                addr,
                expected: def.expected,
                replacement: def.replacement,
                kind: PatchKind::Code,
            })?;
            Ok(Selected { patch, unverified })
        })
        .collect()
}

/// Abre o motor de patches para o alvo, com a trava da `session`.
pub fn open_engine(
    target: &Target,
    scan: &CatalogScan,
    session: &Session,
) -> anyhow::Result<Engine> {
    let disk = scan.disk.clone().context(
        "não consegui ler o cabeçalho do executável em disco; sem ele o gt-patch não escreve",
    )?;
    Ok(gt_grounded2::patching::open_engine(
        target,
        session.store.clone(),
        &session.lock,
        &scan.module,
        disk,
    )?)
}

/// `módulo+offset` do endereço, ou o endereço absoluto se não estiver num módulo conhecido.
pub fn location(target: &Target, addr: u64) -> String {
    target
        .module_label(addr)
        .unwrap_or_else(|| format!("{addr:#x}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_without_kept_entries_lets_the_command_go_on() {
        ensure_recovered(&RecoveryReport::default()).unwrap();
        let report = RecoveryReport {
            restored: vec!["a".into()],
            foreign: vec!["b".into()],
            ..RecoveryReport::default()
        };
        ensure_recovered(&report).unwrap();
    }

    #[test]
    fn kept_entries_stop_the_command_with_the_spec_message() {
        let report = RecoveryReport {
            kept: vec!["a".into(), "b".into()],
            ..RecoveryReport::default()
        };
        let err = ensure_recovered(&report).unwrap_err().to_string();
        assert_eq!(
            err,
            format!("{}\n{}", unrestored_text("a"), unrestored_text("b"))
        );
    }

    #[test]
    fn print_recovery_leaves_kept_entries_to_the_error() {
        let report = RecoveryReport {
            restored: vec!["a".into()],
            kept: vec!["b".into()],
            ..RecoveryReport::default()
        };
        let mut out = Vec::new();
        print_recovery(&mut out, &report).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "restaurado: a\n");
    }
}
