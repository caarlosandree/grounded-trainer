//! Subcomandos do `gt-patch` e o que eles compartilham: a abertura da sessão (spec 6.8), a
//! escolha dos patches e o texto do relatório de recuperação.

pub mod list;
pub mod profile;
pub mod restore;
pub mod run;
pub mod toggle;

use std::io::{self, Write};

use anyhow::Context;
use gt_core::engine::Engine;
use gt_core::journal::{DiscardReason, JournalLock, JournalStore};
use gt_core::patch::{PatchSpec, Validated, validate};
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

/// Como [`begin`], imprimindo o relatório da recuperação se ela fez alguma coisa.
pub fn begin_and_show() -> anyhow::Result<Session> {
    let (session, report) = begin()?;
    if !report.is_empty() {
        print_recovery(&mut io::stdout().lock(), &report)?;
    }
    Ok(session)
}

/// Por que o diário foi descartado.
fn discard_text(reason: DiscardReason) -> &'static str {
    match reason {
        DiscardReason::ProcessGone => "o jogo fechou",
        DiscardReason::OtherProcess => "o jogo reiniciou",
        DiscardReason::OtherBoot => "o sistema reiniciou",
    }
}

/// Linhas do relatório de recuperação.
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
    for id in &report.kept {
        writeln!(out, "{}", unrestored_text(id))?;
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
