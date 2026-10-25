//! Sessão de escrita: pedido de encerramento por sinal, laço do `run` e `toggle`
//! (spec 6.7 e 6.10; feature `write`).
//!
//! O tratador de sinal só marca o pedido; quem desfaz é o laço principal. Sinais
//! repetidos durante o desfazer só marcam o pedido de novo e nunca o interrompem.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use signal_hook::consts::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};

use crate::engine::{BatchReport, Engine};
use crate::mem::ignore_job_control;
use crate::patch::Validated;
use crate::{Error, Result};

/// Pedido de encerramento (SIGINT, SIGTERM, SIGHUP ou SIGQUIT).
#[derive(Debug, Clone)]
pub struct Shutdown {
    flag: Arc<AtomicBool>,
}

impl Shutdown {
    /// Ignora o controle de tarefas (Ctrl+Z não para o processo no meio de uma pausa
    /// do alvo) e registra os sinais de encerramento.
    pub fn install() -> Result<Shutdown> {
        ignore_job_control()?;
        let shutdown = Shutdown::manual();
        for sig in [SIGINT, SIGTERM, SIGHUP, SIGQUIT] {
            signal_hook::flag::register(sig, Arc::clone(&shutdown.flag)).map_err(Error::Io)?;
        }
        Ok(shutdown)
    }

    /// Algum sinal de encerramento chegou.
    pub fn requested(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Pedido sem sinais (para testes).
    #[doc(hidden)]
    pub fn manual() -> Shutdown {
        Shutdown {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Marca o pedido como se um sinal tivesse chegado (para testes).
    #[doc(hidden)]
    pub fn request(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }
}

/// Por que o laço do `run` terminou.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunEnd {
    /// Pedido de encerramento por sinal.
    Signal,
    /// O processo-alvo terminou.
    ProcessGone,
}

/// Espera até um sinal de encerramento ou o fim do alvo, acordando a cada `tick`.
///
/// Quem chama desfaz depois (`undo_all`; o `Drop` do motor cobre o resto). Com
/// [`RunEnd::ProcessGone`], o `Err(ProcessGone)` do `undo_all` é o esperado.
pub fn run_until_shutdown(
    engine: &mut Engine,
    shutdown: &Shutdown,
    tick: Duration,
) -> Result<RunEnd> {
    loop {
        if shutdown.requested() {
            return Ok(RunEnd::Signal);
        }
        // Um sinal no meio da espera interrompe o `poll` (`EINTR` vira `false`).
        if engine.pidfd().exited(tick)? {
            return Ok(RunEnd::ProcessGone);
        }
    }
}

/// Resultado de [`toggle`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToggleReport {
    /// Ciclos completos (ligar e desligar).
    pub cycles: u32,
    /// Novas tentativas por thread em ponto perigoso, somadas.
    pub danger_retries: u32,
    /// Novas tentativas por prazo estourado, somadas.
    pub timeout_retries: u32,
    /// Pausa média de cada troca (do SIGSTOP ao SIGCONT da tentativa que escreveu).
    pub pause_avg: Duration,
    /// Maior pausa de uma troca.
    pub pause_max: Duration,
}

/// Liga e desliga `patch` `times` vezes, dormindo `interval` entre as trocas.
///
/// `after_first_apply` roda uma vez, logo depois do primeiro `apply` bem-sucedido (ex.: o
/// aviso de patch não verificado, que só vale quando o patch foi de fato aplicado).
///
/// Termina com o patch desligado. Para no primeiro erro (devolvido; um patch que não
/// pôde ser desligado continua ativo no motor, cujo `Drop` tenta de novo) ou num pedido
/// de encerramento, sempre antes de ligar.
pub fn toggle(
    engine: &mut Engine,
    patch: Validated,
    times: u32,
    interval: Duration,
    shutdown: &Shutdown,
    after_first_apply: impl FnOnce(),
) -> Result<ToggleReport> {
    let mut after_first_apply = Some(after_first_apply);
    let mut report = ToggleReport::default();
    let mut total = Duration::ZERO;
    let mut switches = 0u32;
    let mut record = |b: &BatchReport, report: &mut ToggleReport| {
        report.danger_retries += b.danger_retries;
        report.timeout_retries += b.timeout_retries;
        report.pause_max = report.pause_max.max(b.pause);
        total += b.pause;
        switches += 1;
        report.pause_avg = total / switches;
    };
    for i in 0..times {
        if shutdown.requested() {
            break;
        }
        if i > 0 {
            std::thread::sleep(interval);
        }
        let on = engine.apply(vec![patch.clone()])?;
        if let Some(f) = after_first_apply.take() {
            f();
        }
        record(&on, &mut report);
        std::thread::sleep(interval);
        let off = engine.undo_all()?;
        record(&off, &mut report);
        report.cycles += 1;
    }
    Ok(report)
}
