//! Recuperação do diário no início de todo comando que escreve ou pausa (spec 6.8;
//! feature `write`).
//!
//! A identidade do processo vem do próprio diário (PID, horário de início, estado e
//! `boot_id` lidos no `/proc`), sem procurar o jogo pelo nome: assim a recuperação
//! funciona com o jogo fechado. A recuperação é genérica (não conhece o jogo) e só
//! escreve onde cada byte é o original ou o nosso; por isso a política de escrita é
//! só "a entrada cabe inteira em memória executável do `maps` atual e tem no máximo
//! [`MAX_PATCH_LEN`] bytes" ([`MAX_CAVE_LEN`] para uma cave). Entrada fora disso é de outro:
//! não é tocada e sai do diário.
//!
//! A ordem é a do motor (spec 008, 6.3): primeiro os desvios (`Code`), depois as caves, estas
//! com a pausa mais paciente e só se nenhum desvio ficou sem resolver (ver
//! [`Engine::restore_entries`]).
//!
//! As regiões de variáveis das caves (`vars` do diário) entram no motor antes de tudo e só
//! são zeradas no fim, se nenhum desvio ficou sem resolver e o `maps` ainda as mostra `rw-`
//! (a recuperação é genérica e não conhece o módulo: vale a identidade do processo do diário).
//! Região mantida aparece como [`VARS_ID`] em `kept`.
//!
//! As entradas de dados (bits, spec 005 6.6; valor inteiro, spec 006 6.8) não passam por essa
//! política: cada uma é decidida por [`crate::journal::decide_data`], com a guarda do objeto
//! relida e o intervalo inteiro
//! numa região `rw` do `maps` atual.

use std::ops::Range;
use std::path::PathBuf;

use nix::sys::signal::Signal;

use crate::engine::{Engine, EngineOptions, PolicyFn};
use crate::journal::{
    DiscardReason, Journal, JournalEntry, JournalLock, JournalStore, Loaded, Recovery, decide,
};
use crate::maps::{MapRegion, read_maps};
use crate::patch::{MAX_CAVE_LEN, MAX_PATCH_LEN, PatchKind, WritePolicy};
use crate::procinfo::{Identity, ProcStat, boot_id, read_stat};
use crate::suspend::{PauseConfig, open_checked};
use crate::{Error, Result};

/// `id` com que as regiões de variáveis não zeradas aparecem em [`RecoveryReport::kept`].
pub const VARS_ID: &str = "variáveis";

/// O que a recuperação fez.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// O jogo estava parado por nós (SIGKILL no meio de uma pausa) e recebeu SIGCONT.
    pub resumed: bool,
    /// O jogo estava parado por outro (ex.: depurador): não recebeu SIGCONT.
    pub stopped_by_other: bool,
    /// Entradas reescritas com os bytes originais.
    pub restored: Vec<String>,
    /// Entradas que já tinham os bytes originais (só esquecidas).
    pub dropped: Vec<String>,
    /// Entradas com bytes de outro (ou fora de memória executável): não tocadas e
    /// tiradas do diário.
    pub foreign: Vec<String>,
    /// Entradas que não puderam ser resolvidas agora: continuam no diário para a
    /// próxima execução.
    pub kept: Vec<String>,
    /// O diário era de outro boot ou processo (ou o jogo fechou) e foi descartado.
    pub discarded: Option<DiscardReason>,
    /// O diário estava ilegível e foi movido para este caminho.
    pub corrupt: Option<PathBuf>,
}

impl RecoveryReport {
    /// Não havia nada a recuperar.
    pub fn is_empty(&self) -> bool {
        *self == RecoveryReport::default()
    }

    fn discard(reason: DiscardReason) -> RecoveryReport {
        RecoveryReport {
            discarded: Some(reason),
            ..RecoveryReport::default()
        }
    }
}

/// Recupera o diário: descarta o que não vale mais, retoma o jogo pausado por nós e
/// restaura as entradas pendentes.
///
/// Quem chama segura `lock` (ordem da spec 6.8: trava → recuperação → escritas). O
/// diário só é limpo quando todas as entradas foram resolvidas; falha por perigo ou
/// prazo devolve o erro e mantém as entradas no diário.
pub fn recover(
    store: &JournalStore,
    lock: &JournalLock,
    cfg: &PauseConfig,
) -> Result<RecoveryReport> {
    let journal = match store.load()? {
        Loaded::Missing => return Ok(RecoveryReport::default()),
        Loaded::Corrupt { moved_to } => {
            tracing::warn!(caminho = %moved_to.display(), "diário ilegível; nada foi escrito no jogo");
            return Ok(RecoveryReport {
                corrupt: Some(moved_to),
                ..RecoveryReport::default()
            });
        }
        Loaded::Ok(j) => j,
    };
    let live = live_stat(journal.pid)?;
    // Os bytes de cada entrada são decididos de novo pelo motor, sob pausa; aqui só
    // interessa a identidade.
    let (resume, stopped_by_other) = match decide(&journal, &boot_id()?, live, |_| None) {
        Recovery::Nothing => return Ok(RecoveryReport::default()),
        Recovery::Discard(reason) => return discard(store, reason),
        Recovery::SameProcess {
            resume,
            stopped_by_other,
            ..
        } => (resume, stopped_by_other),
    };

    let mut report = RecoveryReport {
        stopped_by_other,
        ..RecoveryReport::default()
    };
    // O `pidfd` aberto com a identidade conferida: um PID reaproveitado nunca recebe o
    // SIGCONT nem escrita.
    let pidfd = match open_checked(journal.pid) {
        Ok((pidfd, id)) if id.start_time == journal.start_time => pidfd,
        Ok(_) => return discard(store, DiscardReason::OtherProcess),
        Err(Error::ProcessGone { .. }) => return discard(store, DiscardReason::ProcessGone),
        Err(e) => return Err(e),
    };
    if stopped_by_other {
        tracing::warn!(
            pid = journal.pid,
            "o jogo está parado por outro programa; não vou retomá-lo"
        );
    }
    if resume {
        match pidfd.send(Signal::SIGCONT) {
            Ok(()) => {}
            Err(Error::ProcessGone { .. }) => return discard(store, DiscardReason::ProcessGone),
            Err(e) => return Err(e),
        }
        tracing::warn!(
            pid = journal.pid,
            "o jogo estava pausado por uma execução interrompida; retomado"
        );
        report.resumed = true;
        // O marcador sai do disco antes de restaurar: uma queda daqui em diante nunca
        // manda SIGCONT a um jogo que outro tenha parado.
        store.save(&Journal {
            paused_by_us: false,
            ..journal.clone()
        })?;
    }

    let maps = match read_maps(journal.pid) {
        Ok(maps) => maps,
        Err(Error::ProcessGone { .. }) => return finish_gone(store, report),
        Err(e) => return Err(e),
    };
    let exec = exec_policy(&maps);
    let (eligible, outside): (Vec<JournalEntry>, Vec<JournalEntry>) = journal
        .entries
        .iter()
        .cloned()
        .partition(|e| e.original.len() <= max_entry_len(e.kind) && exec.allows(&e.range()));
    for e in outside {
        tracing::warn!(
            id = %e.id,
            addr = e.addr,
            "entrada fora de memória executável ou grande demais; não tocada"
        );
        report.foreign.push(e.id);
    }
    if eligible.is_empty() && journal.data.is_empty() && journal.vars.is_empty() {
        store.clear()?;
        return Ok(report);
    }

    let ranges: Vec<Range<u64>> = eligible.iter().map(JournalEntry::range).collect();
    let policy: PolicyFn = Box::new(move |maps: &[MapRegion]| {
        let exec = exec_policy(maps);
        WritePolicy::new(ranges.iter().filter(|r| exec.allows(r)).cloned().collect())
    });
    let opts = EngineOptions {
        version_key: journal.version_key.clone(),
        pause: cfg.clone(),
    };
    // A identidade do diário: se o processo mudou desde a conferência acima, o motor
    // recusa a abertura e nada é escrito.
    let expected = Identity {
        pid: journal.pid,
        start_time: journal.start_time,
        boot_id: journal.boot_id.clone(),
    };
    let opened = Engine::open(
        journal.pid,
        Some(&expected),
        store.clone(),
        lock,
        policy,
        opts,
    );
    let mut engine = match opened {
        Ok(engine) => engine,
        Err(Error::ProcessGone { .. }) => return finish_gone(store, report),
        Err(e) => return Err(e),
    };
    // As regiões de variáveis entram no registro do motor antes de tudo: o diário que ele
    // regrava as mantém mesmo que uma falha abandone a recuperação no meio.
    // Região do diário que não passa no filtro do motor (tamanho, alinhamento, sobreposição)
    // não é tocada: sai do diário como entrada de outro.
    let rejected = engine.adopt_vars(&journal.vars);
    if !rejected.is_empty() {
        report.foreign.push(VARS_ID.to_owned());
        engine.save_journal_now();
    }
    // O motor regrava o diário só com as entradas que ainda não resolveu (as de fora
    // de memória executável já saíram) e o apaga quando não sobra nenhuma. As de dados
    // entram como pendentes do motor e saem quando resolvidas.
    match engine.restore_entries(eligible) {
        Ok(r) => {
            report.restored = r.restored;
            report.dropped = r.dropped;
            report.foreign.extend(r.foreign);
            report.kept = r.outside_policy;
            report.kept.extend(r.deferred);
        }
        Err(Error::ProcessGone { .. }) => {
            report.discarded = Some(DiscardReason::ProcessGone);
            return Ok(report);
        }
        Err(e) => return Err(e),
    }
    if !journal.data.is_empty() {
        match engine.restore_data(journal.data.clone()) {
            Ok(r) => {
                report.restored.extend(r.restored);
                report.dropped.extend(r.dropped);
                report.foreign.extend(r.foreign);
            }
            Err(Error::ProcessGone { .. }) => {
                report.discarded = Some(DiscardReason::ProcessGone);
                return Ok(report);
            }
            // A escrita de um bit falhou: a entrada continua no diário para a próxima execução.
            Err(e) => {
                tracing::warn!(erro = %e, "bits não restaurados agora");
                report.kept.extend(engine.pending_ids());
            }
        }
    }
    // Por último (spec 008, 6.3): as variáveis, que só são zeradas se nenhum desvio nosso
    // continua sem resolver.
    if !engine.vars().is_empty() {
        match engine.clear_vars() {
            Ok(()) => {}
            Err(Error::ProcessGone { .. }) => {
                report.discarded = Some(DiscardReason::ProcessGone);
                return Ok(report);
            }
            Err(e) => tracing::warn!(erro = %e, "variáveis não zeradas agora"),
        }
        if !engine.vars().is_empty() {
            report.kept.push(VARS_ID.to_owned());
        }
    }
    if !report.kept.is_empty() {
        tracing::warn!(
            ids = ?report.kept,
            "entradas não restauradas; a próxima execução do trainer vai tentar de novo"
        );
    }
    Ok(report)
}

/// Maior entrada que a recuperação aceita tocar, por tipo.
fn max_entry_len(kind: PatchKind) -> usize {
    match kind {
        PatchKind::Code => MAX_PATCH_LEN,
        PatchKind::Cave => MAX_CAVE_LEN,
    }
}

/// Estado atual do processo do diário; `None` se ele não existe mais.
fn live_stat(pid: i32) -> Result<Option<ProcStat>> {
    match read_stat(pid) {
        Ok(stat) => Ok(Some(stat)),
        Err(Error::ProcessGone { .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

fn discard(store: &JournalStore, reason: DiscardReason) -> Result<RecoveryReport> {
    tracing::warn!(motivo = ?reason, "a memória do diário não existe mais; diário descartado");
    store.clear()?;
    Ok(RecoveryReport::discard(reason))
}

/// O jogo fechou no meio da recuperação: descarta sem escrever.
fn finish_gone(store: &JournalStore, mut report: RecoveryReport) -> Result<RecoveryReport> {
    store.clear()?;
    report.discarded = Some(DiscardReason::ProcessGone);
    Ok(report)
}

/// Intervalos executáveis do `maps`.
fn exec_policy(maps: &[MapRegion]) -> WritePolicy {
    WritePolicy::new(
        maps.iter()
            .filter(|r| r.perms.exec)
            .map(|r| r.start..r.end)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::maps::{MapPath, Perms};

    fn region(start: u64, end: u64, exec: bool) -> MapRegion {
        MapRegion {
            start,
            end,
            perms: Perms {
                read: true,
                write: !exec,
                exec,
                shared: false,
            },
            offset: 0,
            path: MapPath::Anonymous,
        }
    }

    #[test]
    fn exec_policy_merges_neighbours_and_skips_data() {
        let maps = [
            region(0x1000, 0x2000, true),
            region(0x2000, 0x3000, true),
            region(0x3000, 0x4000, false),
        ];
        let p = exec_policy(&maps);
        assert!(p.allows(&(0x1ffe..0x2002)));
        assert!(!p.allows(&(0x2ffe..0x3002)));
        assert!(!p.allows(&(0x3000..0x3003)));
    }

    #[test]
    fn entry_length_limit_depends_on_kind() {
        assert_eq!(max_entry_len(PatchKind::Code), 64);
        assert_eq!(max_entry_len(PatchKind::Cave), 4096);
    }

    #[test]
    fn empty_report_is_empty() {
        assert!(RecoveryReport::default().is_empty());
        assert!(!RecoveryReport::discard(DiscardReason::ProcessGone).is_empty());
    }
}
