//! Aplica e desfaz patches de bytes no Grounded 2: `list`, `run`, `toggle`, `restore` e `profile`.
//!
//! É o único aplicativo que escreve na memória do jogo (spec 6.10). O trabalho de verdade mora
//! no `gt-core` e no `gt-grounded2`; aqui só se juntam argumentos, assinaturas embutidas e saída.

mod cmd;

use std::io::Write;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use gt_grounded2::state::PatchState;
use tracing_subscriber::EnvFilter;

/// Padrão de `toggle --interval` (ms), spec 12.
const TOGGLE_INTERVAL_MS: &str = "50";
/// Padrões de `profile` (spec 12): 200 amostras a cada 300 ms (60 s).
const PROFILE_SAMPLES: &str = "200";
const PROFILE_INTERVAL_MS: &str = "300";

/// Aplica e desfaz patches de bytes no Grounded 2 (escreve na memória do jogo).
#[derive(Debug, Parser)]
#[command(name = "gt-patch", version, about)]
struct Cli {
    /// Processo alvo; sem isto, procura o Grounded 2 em /proc.
    #[arg(long, global = true, value_name = "PID", value_parser = clap::value_parser!(i32).range(1..))]
    pid: Option<i32>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Mostra a chave de versão, o estado de cada patch e se há diário pendente (não escreve).
    List,
    /// Aplica os patches e fica rodando até Ctrl+C (ou o jogo fechar); então desfaz.
    Run {
        /// Identificadores dos patches (veja `gt-patch list`).
        #[arg(required = true, value_name = "ID")]
        ids: Vec<String>,
    },
    /// Liga e desliga um patch várias vezes e mede a pausa do jogo.
    Toggle {
        /// Identificador do patch.
        id: String,
        /// Quantos ciclos (ligar e desligar).
        #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
        times: u32,
        /// Espera entre as trocas, em milissegundos.
        #[arg(long, value_name = "MS", default_value = TOGGLE_INTERVAL_MS)]
        interval: u64,
    },
    /// Só recupera o diário (desfaz o que uma execução interrompida deixou) e sai.
    Restore,
    /// Amostra o contador de programa das threads e mostra as funções mais executadas.
    Profile {
        /// Quantas pausas (amostras).
        #[arg(long, value_name = "N", default_value = PROFILE_SAMPLES, value_parser = clap::value_parser!(u32).range(1..))]
        samples: u32,
        /// Espera entre as amostras, em milissegundos.
        #[arg(long, value_name = "MS", default_value = PROFILE_INTERVAL_MS)]
        interval: u64,
    },
}

/// Erro do núcleo dentro de `err`, direto ou embrulhado em `gt_grounded2::Error`.
fn core_error(err: &anyhow::Error) -> Option<&gt_core::Error> {
    if let Some(core) = err.downcast_ref::<gt_core::Error>() {
        return Some(core);
    }
    match err.downcast_ref::<gt_grounded2::Error>()? {
        gt_grounded2::Error::Core(core) | gt_grounded2::Error::DiskOpen { source: core, .. } => {
            Some(core)
        }
        _ => None,
    }
}

/// Sugestão de ação para o erro, quando há uma (spec 6.11).
fn hint(err: &anyhow::Error) -> Option<String> {
    use gt_core::Error;
    if let Some(gt_grounded2::Error::Unavailable { state, .. }) =
        err.downcast_ref::<gt_grounded2::Error>()
        && state == PatchState::NotFound.label()
    {
        return Some(
            "confira com `gt scan --disk`; outro trainer pode ter alterado o local.".to_string(),
        );
    }
    match core_error(err)? {
        Error::PermissionDenied { ptrace_scope, .. } => {
            Some(gt_grounded2::resolve::permission_hint(
                "gt-patch",
                gt_core::process::ptrace_scope().or(*ptrace_scope),
            ))
        }
        Error::ProcessGone { .. } => {
            Some("o jogo fechou; abra-o de novo e tente outra vez.".to_string())
        }
        Error::AlreadyRunning => Some("feche o outro programa antes.".to_string()),
        Error::WriteRefused { .. } => Some(
            "escrever em código depende de o kernel permitir escrita forçada em \
             /proc/<pid>/mem (CONFIG_PROC_MEM_ALWAYS_FORCE=y ou proc_mem.force_override=always); \
             o gt-patch não altera isso."
                .to_string(),
        ),
        Error::PauseTimeout { .. } => {
            Some("o jogo não parou a tempo; feche programas pesados e tente de novo.".to_string())
        }
        Error::ThreadsInPatch { .. } => Some(
            "o jogo está executando esse trecho sem parar; tente de novo em outro momento."
                .to_string(),
        ),
        Error::BytesMismatch { .. } => Some(
            "a versão do jogo mudou ou outro trainer alterou o local; confira com `gt diff`."
                .to_string(),
        ),
        _ => None,
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    // Ctrl+Z não pode parar o processo no meio de uma pausa do jogo (spec 6.5).
    gt_core::mem::ignore_job_control()?;
    match cli.command {
        Command::List => cmd::list::run(cli.pid),
        Command::Run { ids } => cmd::run::run(cli.pid, &ids),
        Command::Toggle {
            id,
            times,
            interval,
        } => cmd::toggle::run(cli.pid, &id, times, interval),
        Command::Restore => cmd::restore::run(),
        Command::Profile { samples, interval } => cmd::profile::run(cli.pid, samples, interval),
    }
}

/// Escreve o erro, a cadeia de causas e a sugestão em stderr.
fn report(err: &anyhow::Error) {
    let mut out = std::io::stderr().lock();
    let _ = writeln!(out, "erro: {err}");
    for cause in err.chain().skip(1) {
        let _ = writeln!(out, "  causa: {cause}");
    }
    if let Some(hint) = hint(err) {
        let _ = writeln!(out, "dica: {hint}");
    }
}

fn main() -> ExitCode {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            report(&err);
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gt_core::session::ToggleReport;

    use super::*;

    #[test]
    fn toggle_report_text_has_cycles_and_warns_on_long_pause() {
        let text = cmd::toggle::toggle_report_text(&ToggleReport {
            cycles: 100,
            danger_retries: 0,
            timeout_retries: 0,
            pause_avg: Duration::from_millis(2),
            pause_max: Duration::from_millis(60),
        });
        assert!(text.contains("ciclos: 100"), "{text}");
        assert!(
            text.contains("aviso: pausa máxima acima de 50 ms"),
            "{text}"
        );
    }

    #[test]
    fn toggle_report_text_is_quiet_on_short_pauses() {
        let text = cmd::toggle::toggle_report_text(&ToggleReport {
            cycles: 3,
            danger_retries: 2,
            timeout_retries: 1,
            pause_avg: Duration::from_micros(1500),
            pause_max: Duration::from_millis(49),
        });
        assert!(
            text.contains("novas tentativas: 2 (threads no trecho) + 1 (prazo)"),
            "{text}"
        );
        assert!(
            text.contains("pausa média: 1.5 ms, máxima: 49.0 ms"),
            "{text}"
        );
        assert!(!text.contains("aviso"), "{text}");
    }

    #[test]
    fn hint_finds_core_error_inside_grounded2_error() {
        let err = anyhow::Error::from(gt_grounded2::Error::from(gt_core::Error::ProcessGone {
            pid: 1,
        }));
        let hint = hint(&err).expect("deveria haver dica");
        assert!(hint.contains("o jogo fechou"), "{hint}");
    }

    #[test]
    fn hint_for_already_running_and_direct_core_errors() {
        let err = anyhow::Error::from(gt_core::Error::AlreadyRunning);
        assert!(hint(&err).is_some_and(|h| h.contains("feche o outro programa antes")));
        let err = anyhow::Error::from(gt_core::Error::WriteRefused { addr: 0x1000 });
        assert!(hint(&err).is_some_and(|h| h.contains("proc_mem.force_override")));
    }

    #[test]
    fn hint_suggests_scan_for_not_found_patch_only() {
        let not_found = anyhow::Error::from(gt_grounded2::Error::Unavailable {
            id: "x".into(),
            state: PatchState::NotFound.label().into(),
        });
        assert!(hint(&not_found).is_some_and(|h| h.contains("gt scan --disk")));
        let other = anyhow::Error::from(gt_grounded2::Error::Unavailable {
            id: "x".into(),
            state: PatchState::Inconclusive.label().into(),
        });
        assert!(hint(&other).is_none());
    }

    #[test]
    fn hint_is_none_for_errors_without_a_suggestion() {
        assert!(hint(&anyhow::Error::from(gt_grounded2::Error::GameNotFound)).is_none());
    }
}
