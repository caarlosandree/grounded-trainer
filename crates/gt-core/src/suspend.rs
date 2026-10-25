//! Pausa do processo com conferência das threads.
//!
//! Pausar é enviar `SIGSTOP` ao processo todo e conferir, thread por thread em
//! `/proc/<pid>/task`, que todas as vivas pararam. As funções puras (`resume_points`,
//! `all_stopped`, `wait_until`) e a configuração existem sempre; o que envia sinais
//! (`pause`, `PauseGuard`, `sample_pcs`, `is_stopped_now`, `open_checked`) exige a
//! feature `write`.

use std::time::{Duration, Instant};

use crate::Result;
use crate::procinfo::{ThreadPos, is_dead, is_stopped};

/// Pontos de um [`PauseConfig::hook`] (só para testes do motor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookPoint {
    /// Dentro da pausa, com o alvo parado e antes de qualquer escrita.
    InPause,
    /// Logo depois de aplicar o lote, ainda com o alvo parado.
    AfterApply,
}

/// Parâmetros da pausa.
#[derive(Debug, Clone)]
pub struct PauseConfig {
    /// Prazo para todas as threads pararem depois do `SIGSTOP`.
    pub stop_timeout: Duration,
    /// Tentativas por lote (perigo e prazo somados).
    pub max_attempts: u32,
    /// Espera entre tentativas.
    pub retry_wait: Duration,
    /// Gancho de teste, chamado em pontos fixos da pausa.
    #[doc(hidden)]
    pub hook: Option<fn(HookPoint)>,
}

impl Default for PauseConfig {
    fn default() -> PauseConfig {
        PauseConfig {
            stop_timeout: Duration::from_millis(500),
            max_attempts: 10,
            retry_wait: Duration::from_millis(20),
            hook: None,
        }
    }
}

/// Endereços onde uma thread parada pode retomar a execução.
///
/// Fora de syscall é só o `pc`. Dentro de uma syscall interrompida o kernel pode
/// reiniciá-la voltando 2 bytes (o tamanho de `syscall`), então `pc - 2` também conta.
/// Thread rodando não tem posição conhecida.
pub fn resume_points(pos: &ThreadPos) -> Vec<u64> {
    match *pos {
        ThreadPos::Stopped { syscall: None, pc } => vec![pc],
        ThreadPos::Stopped {
            syscall: Some(_),
            pc,
        } => vec![pc, pc.wrapping_sub(2)],
        ThreadPos::Running => Vec::new(),
    }
}

/// Todas as threads vivas estão paradas (`T`/`t`). Threads mortas são ignoradas;
/// lista vazia dá `true`.
pub fn all_stopped(states: &[char]) -> bool {
    states.iter().all(|&s| is_dead(s) || is_stopped(s))
}

/// Chama `probe` a cada `step` até ele dar `true` ou o prazo `timeout` acabar.
///
/// `Ok(true)` se `probe` deu `true` dentro do prazo; `Ok(false)` se estourou. Erro do
/// `probe` encerra a espera na hora.
pub fn wait_until(
    mut probe: impl FnMut() -> Result<bool>,
    timeout: Duration,
    step: Duration,
) -> Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        if probe()? {
            return Ok(true);
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(false);
        }
        std::thread::sleep(step.min(deadline - now));
    }
}

#[cfg(feature = "write")]
mod live {
    use std::time::Duration;

    use nix::sys::signal::Signal;

    use super::{HookPoint, PauseConfig, all_stopped, wait_until};
    use crate::mem::Pidfd;
    use crate::procinfo::{
        Identity, ThreadPos, identity, is_dead, list_tids, read_task_stat, read_thread_pos,
    };
    use crate::{Error, Result};

    /// Intervalo de conferência do estado das threads depois do `SIGSTOP`.
    const POLL_STEP: Duration = Duration::from_millis(1);

    /// Quantas vezes relê a lista de threads até ela ficar estável.
    const LIST_PASSES: usize = 10;

    /// Uma thread viva do alvo no momento da pausa.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Thread {
        pub tid: i32,
        /// Onde a thread parou; `None` se não foi pedida ou o arquivo não pôde ser lido.
        pub pos: Option<ThreadPos>,
    }

    /// Resultado de uma pausa. Ao sair de escopo sem [`resume`](PauseGuard::resume),
    /// envia `SIGCONT` se foi ele quem enviou o `SIGSTOP`.
    #[derive(Debug)]
    pub struct PauseGuard<'a> {
        pidfd: &'a Pidfd,
        pid: i32,
        threads: Vec<Thread>,
        sent_stop: bool,
        resumed: bool,
    }

    impl PauseGuard<'_> {
        /// Threads vivas no momento da pausa (mortas e sumidas ficam de fora).
        pub fn threads(&self) -> &[Thread] {
            &self.threads
        }

        /// Fomos nós que enviamos o `SIGSTOP` (senão o alvo já estava parado).
        pub fn sent_stop(&self) -> bool {
            self.sent_stop
        }

        /// Todas as threads vivas continuam paradas.
        pub fn still_stopped(&self) -> Result<bool> {
            is_stopped_now(self.pid)
        }

        /// Retoma o alvo (`SIGCONT`), se foi pausado por nós.
        pub fn resume(mut self) -> Result<()> {
            self.send_cont()
        }

        fn send_cont(&mut self) -> Result<()> {
            if self.sent_stop && !self.resumed {
                self.resumed = true;
                self.pidfd.send(Signal::SIGCONT)?;
            }
            Ok(())
        }
    }

    impl Drop for PauseGuard<'_> {
        fn drop(&mut self) {
            match self.send_cont() {
                Ok(()) => {}
                Err(Error::ProcessGone { pid }) => {
                    tracing::debug!(pid, "alvo já terminou; SIGCONT dispensado");
                }
                Err(e) => tracing::error!(pid = self.pid, erro = %e, "falha ao enviar SIGCONT"),
            }
        }
    }

    /// Estados das threads vivas. Sem nenhuma viva, o processo terminou.
    fn live_states(pid: i32) -> Result<Vec<char>> {
        let mut states = Vec::new();
        for tid in list_tids(pid)? {
            if let Some(stat) = read_task_stat(pid, tid)?
                && !is_dead(stat.state)
            {
                states.push(stat.state);
            }
        }
        if states.is_empty() {
            return Err(Error::ProcessGone { pid });
        }
        Ok(states)
    }

    /// Todas as threads vivas do alvo estão paradas agora.
    ///
    /// [`Error::ProcessGone`] se não há mais thread viva.
    pub fn is_stopped_now(pid: i32) -> Result<bool> {
        Ok(all_stopped(&live_states(pid)?))
    }

    /// Abre o `pidfd` conferindo a identidade antes e depois.
    ///
    /// Se o horário de início mudou entre as duas leituras, o PID foi reutilizado e o
    /// `pidfd` pode apontar para outro processo: [`Error::ProcessGone`].
    pub fn open_checked(pid: i32) -> Result<(Pidfd, Identity)> {
        let before = identity(pid)?;
        let pidfd = Pidfd::open(pid)?;
        let after = identity(pid)?;
        if before != after {
            return Err(Error::ProcessGone { pid });
        }
        Ok((pidfd, before))
    }

    /// Lê as threads vivas, relendo a lista de TIDs até ela ficar estável.
    fn collect_threads(pid: i32, need_positions: bool) -> Result<Vec<Thread>> {
        let mut tids = list_tids(pid)?;
        for _ in 0..LIST_PASSES {
            let mut threads = Vec::with_capacity(tids.len());
            for &tid in &tids {
                let Some(stat) = read_task_stat(pid, tid)? else {
                    continue;
                };
                if is_dead(stat.state) {
                    continue;
                }
                let pos = if need_positions {
                    read_thread_pos(pid, tid)?
                } else {
                    None
                };
                threads.push(Thread { tid, pos });
            }
            let again = list_tids(pid)?;
            if again == tids {
                return Ok(threads);
            }
            tids = again;
        }
        Err(Error::BadProc {
            file: format!("/proc/{pid}/task"),
            reason: "a lista de threads não estabilizou com o alvo parado".into(),
        })
    }

    /// Pausa o alvo e confere que todas as threads vivas pararam.
    ///
    /// Se já estavam todas paradas, não envia sinal (`sent_stop() == false`). Senão envia
    /// `SIGSTOP` e espera até `cfg.stop_timeout`; no estouro retoma o alvo e devolve
    /// [`Error::PauseTimeout`]. Com `need_positions`, lê a posição de cada thread.
    pub fn pause<'a>(
        pidfd: &'a Pidfd,
        pid: i32,
        cfg: &PauseConfig,
        need_positions: bool,
    ) -> Result<PauseGuard<'a>> {
        let already = is_stopped_now(pid)?;
        // O guarda nasce antes do sinal: qualquer saída (erro ou pânico) envia SIGCONT.
        let mut guard = PauseGuard {
            pidfd,
            pid,
            threads: Vec::new(),
            sent_stop: false,
            resumed: false,
        };
        if !already {
            pidfd.send(Signal::SIGSTOP)?;
            guard.sent_stop = true;
            let stopped = wait_until(|| is_stopped_now(pid), cfg.stop_timeout, POLL_STEP)?;
            if !stopped {
                return Err(Error::PauseTimeout { attempts: 1 });
            }
        }
        guard.threads = collect_threads(pid, need_positions)?;
        if let Some(hook) = cfg.hook
            && !std::thread::panicking()
        {
            hook(HookPoint::InPause);
        }
        Ok(guard)
    }

    /// Colhe `samples` amostras do contador de programa: em cada uma pausa o alvo, anota o
    /// `pc` de todas as threads paradas e retoma.
    ///
    /// `should_stop` é consultado antes de cada amostra; `true` devolve o que já foi
    /// colhido. `before_pause` recebe `will_stop` (vamos enviar `SIGSTOP`) antes de cada
    /// pausa e `after_resume` roda depois de cada retomada (também quando a pausa
    /// falha). `PauseTimeout` repete a amostra até `cfg.max_attempts` vezes seguidas e
    /// depois aborta; qualquer outro erro, inclusive `ProcessGone`, aborta na hora.
    #[allow(clippy::too_many_arguments)]
    pub fn sample_pcs(
        pidfd: &Pidfd,
        pid: i32,
        cfg: &PauseConfig,
        samples: u32,
        interval: Duration,
        mut before_pause: impl FnMut(bool) -> Result<()>,
        mut after_resume: impl FnMut() -> Result<()>,
        mut should_stop: impl FnMut() -> bool,
    ) -> Result<Vec<u64>> {
        let mut pcs = Vec::new();
        for i in 0..samples {
            if should_stop() {
                break;
            }
            if i > 0 {
                std::thread::sleep(interval);
            }
            let mut attempts = 0;
            loop {
                attempts += 1;
                let will_stop = !is_stopped_now(pid)?;
                before_pause(will_stop)?;
                match pause(pidfd, pid, cfg, true) {
                    Ok(guard) => {
                        pcs.extend(guard.threads().iter().filter_map(|t| match t.pos {
                            Some(ThreadPos::Stopped { pc, .. }) => Some(pc),
                            _ => None,
                        }));
                        guard.resume()?;
                        after_resume()?;
                        break;
                    }
                    Err(Error::PauseTimeout { .. }) if attempts < cfg.max_attempts => {
                        after_resume()?;
                        std::thread::sleep(cfg.retry_wait);
                    }
                    Err(Error::PauseTimeout { .. }) => {
                        // Já vale o erro do prazo; a falha de limpeza só vai para o log.
                        if let Err(e) = after_resume() {
                            tracing::warn!(erro = %e, "falha ao limpar depois da pausa");
                        }
                        return Err(Error::PauseTimeout { attempts });
                    }
                    Err(e) => {
                        if let Err(e) = after_resume() {
                            tracing::warn!(erro = %e, "falha ao limpar depois da pausa");
                        }
                        return Err(e);
                    }
                }
            }
        }
        Ok(pcs)
    }
}

#[cfg(feature = "write")]
pub use live::{PauseGuard, Thread, is_stopped_now, open_checked, pause, sample_pcs};

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::time::Duration;

    use super::*;
    use crate::procinfo::ThreadPos;

    #[test]
    fn resume_points_variants() {
        let pc = 0x1000;
        assert_eq!(
            resume_points(&ThreadPos::Stopped { syscall: None, pc }),
            vec![pc]
        );
        assert_eq!(
            resume_points(&ThreadPos::Stopped {
                syscall: Some(0),
                pc
            }),
            vec![pc, pc - 2]
        );
        assert_eq!(
            resume_points(&ThreadPos::Stopped {
                syscall: Some(0),
                pc: 1
            }),
            vec![1, u64::MAX]
        );
        assert!(resume_points(&ThreadPos::Running).is_empty());
    }

    #[test]
    fn all_stopped_ignores_dead() {
        assert!(all_stopped(&[]));
        assert!(all_stopped(&['T', 't']));
        assert!(all_stopped(&['T', 'Z', 'X']));
        assert!(all_stopped(&['Z']));
        assert!(!all_stopped(&['T', 'S']));
        assert!(!all_stopped(&['R']));
    }

    #[test]
    fn wait_until_times_out() {
        let calls = Cell::new(0);
        let ok = wait_until(
            || {
                calls.set(calls.get() + 1);
                Ok(false)
            },
            Duration::from_millis(30),
            Duration::from_millis(5),
        )
        .unwrap();
        assert!(!ok);
        assert!(calls.get() >= 2);
    }

    #[test]
    fn wait_until_succeeds() {
        let calls = Cell::new(0);
        let ok = wait_until(
            || {
                calls.set(calls.get() + 1);
                Ok(calls.get() >= 3)
            },
            Duration::from_secs(5),
            Duration::from_millis(1),
        )
        .unwrap();
        assert!(ok);
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn wait_until_propagates_probe_error() {
        let r = wait_until(
            || Err(crate::Error::ProcessGone { pid: 1 }),
            Duration::from_secs(1),
            Duration::from_millis(1),
        );
        assert!(matches!(r, Err(crate::Error::ProcessGone { pid: 1 })));
    }

    #[test]
    fn pause_config_defaults() {
        let c = PauseConfig::default();
        assert_eq!(c.stop_timeout, Duration::from_millis(500));
        assert_eq!(c.max_attempts, 10);
        assert_eq!(c.retry_wait, Duration::from_millis(20));
        assert!(c.hook.is_none());
    }
}
