//! Interface do jogador no terminal.
//!
//! Ordem do início (spec 003, 6.1): sinais → trava do diário → terminal → log → gancho de
//! pânico → tela alternativa → recuperação do diário → laço → saída.

use std::io::{self, IsTerminal, Write};
use std::process;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::Context;
use clap::Parser;
use gt_core::Error as CoreError;
use gt_core::journal::JournalStore;
use gt_core::session::Shutdown;
use gt_tui::app::{self, RunEnd};
use gt_tui::controller::Controller;
use gt_tui::game::GameBackend;
use gt_tui::logfile::open_log;
use gt_tui::term::{CrosstermTerm, RestoreOnce, Term, install_panic_hook, restore_terminal};
use tracing_subscriber::EnvFilter;

/// Marca de "o terminal já foi restaurado", compartilhada com o gancho de pânico.
static RESTORE: RestoreOnce = RestoreOnce::new();

/// Código de saída dos erros de início (trava ocupada, sem terminal).
const START_ERROR_CODE: i32 = 2;

/// Interface no terminal para ligar e desligar os cheats do Grounded 2.
#[derive(Debug, Parser)]
#[command(name = "gt-tui", version, about)]
struct Cli {
    /// Processo alvo; sem isto, procura o Grounded 2 em /proc.
    #[arg(long, value_name = "PID", value_parser = clap::value_parser!(i32).range(1..))]
    pid: Option<i32>,

    /// Mostra também o patch de teste (categoria de diagnóstico).
    #[arg(long)]
    diagnostico: bool,
}

/// Erro de início: a mensagem (e a dica, se houver) vai para o `stderr`.
fn start_error(message: &str, hint: Option<&str>) -> i32 {
    let mut err = io::stderr().lock();
    let _ = writeln!(err, "erro: {message}");
    if let Some(hint) = hint {
        let _ = writeln!(err, "dica: {hint}");
    }
    START_ERROR_CODE
}

fn clock() -> String {
    chrono::Local::now().format("%H:%M").to_string()
}

/// Inicia o log em arquivo. Devolve `false` se o arquivo não pôde ser aberto (sem log nenhum).
fn init_log(dir: &std::path::Path) -> bool {
    let Some(file) = open_log(dir) else {
        return false;
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(Mutex::new(file))
        .try_init();
    true
}

fn real_main(cli: &Cli) -> anyhow::Result<i32> {
    let shutdown = Shutdown::install().context("não foi possível registrar os sinais")?;

    let dir = JournalStore::default_dir()
        .context("não achei onde guardar o diário; defina XDG_STATE_HOME ou HOME")?;
    let store = JournalStore::new(dir.clone());
    let lock = match store.lock() {
        Ok(lock) => Arc::new(lock),
        Err(CoreError::AlreadyRunning) => {
            return Ok(start_error(
                &CoreError::AlreadyRunning.to_string(),
                Some("feche o outro programa antes."),
            ));
        }
        Err(e) => return Err(e).context("não foi possível pegar a trava do diário"),
    };

    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Ok(start_error("o gt-tui precisa de um terminal", None));
    }

    let logged = init_log(&dir);

    let backend = GameBackend::new(cli.pid, store.clone(), lock);
    let mut controller =
        Controller::new(backend, Box::new(clock), cli.diagnostico, cli.pid.is_some());
    if !logged {
        controller
            .model
            .push_event(&clock(), "log em arquivo indisponível".to_string());
    }

    // O gancho vem antes de qualquer thread de busca: um pânico nela não pode escrever no
    // `stderr` com a TUI desenhada.
    let elsewhere = Arc::new(AtomicBool::new(false));
    install_panic_hook(
        thread::current().id(),
        Arc::new(|| {
            let _ = RESTORE.run(restore_terminal);
        }),
        Arc::clone(&elsewhere),
    );

    let mut term = CrosstermTerm::new().context("não foi possível preparar o terminal")?;
    let start = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        controller.start(&mut |model| {
            if !RESTORE.done() {
                let _ = term.draw(model);
            }
        });
    }));
    let end = if start.is_ok() {
        app::run(&mut term, &mut controller, &shutdown, &RESTORE, &elsewhere)
    } else {
        // Pânico no início: o gancho já restaurou o terminal; o `Drop` do motor desfaz o resto.
        RunEnd {
            restored: Vec::new(),
            foreign: Vec::new(),
            panicked: true,
        }
    };
    let summary = app::finish(&mut term, controller, &RESTORE, &store, end, logged);

    let mut out = io::stdout().lock();
    for line in &summary.lines {
        let _ = writeln!(out, "{line}");
        tracing::info!("{line}");
    }
    let _ = out.flush();
    Ok(summary.code)
}

fn main() {
    let cli = Cli::parse();
    let code = match real_main(&cli) {
        Ok(code) => code,
        Err(err) => {
            let mut e = io::stderr().lock();
            let _ = writeln!(e, "erro: {err}");
            for cause in err.chain().skip(1) {
                let _ = writeln!(e, "  causa: {cause}");
            }
            START_ERROR_CODE
        }
    };
    process::exit(code);
}
