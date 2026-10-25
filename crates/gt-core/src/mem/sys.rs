//! Chamadas de sistema cruas (feature `write`): `pidfd` e sinais de controle de job.
//!
//! O `nix` 0.31 não expõe `pidfd_open` nem `pidfd_send_signal`; usamos `libc::syscall`.

use std::io;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::ptr;
use std::time::Duration;

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::signal::{SigHandler, Signal, signal};

use crate::{Error, Result};

/// Descritor de arquivo que representa um processo (`pidfd`).
///
/// Não sofre a reutilização de PID: sinaliza sempre o processo aberto, nunca outro.
#[derive(Debug)]
pub struct Pidfd {
    fd: OwnedFd,
    pid: i32,
}

impl Pidfd {
    /// Abre um `pidfd` para `pid`. Processo inexistente vira [`Error::ProcessGone`].
    pub fn open(pid: i32) -> Result<Pidfd> {
        // SAFETY: `pidfd_open(pid, 0)` só recebe inteiros e não toca memória do
        // processo; devolve um fd novo ou -1 com `errno`.
        let ret = unsafe { libc::syscall(libc::SYS_pidfd_open, libc::c_int::from(pid), 0) };
        if ret < 0 {
            return Err(match Errno::last() {
                Errno::ESRCH => Error::ProcessGone { pid },
                e => Error::Io(io::Error::from(e)),
            });
        }
        // SAFETY: `ret` é um fd recém-criado e válido, que ninguém mais possui.
        let fd = unsafe { OwnedFd::from_raw_fd(ret as RawFd) };
        Ok(Pidfd { fd, pid })
    }

    /// Envia `sig` ao processo. `ESRCH` vira [`Error::ProcessGone`].
    pub fn send(&self, sig: Signal) -> Result<()> {
        // SAFETY: o fd é válido enquanto `self` existir; `info` nulo e `flags` 0 são
        // valores permitidos por `pidfd_send_signal(2)` (o kernel preenche `siginfo`).
        let ret = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.fd.as_raw_fd(),
                sig as libc::c_int,
                ptr::null::<libc::siginfo_t>(),
                0u32,
            )
        };
        if ret < 0 {
            return Err(match Errno::last() {
                Errno::ESRCH => Error::ProcessGone { pid: self.pid },
                Errno::EPERM => Error::PermissionDenied {
                    pid: self.pid,
                    ptrace_scope: crate::process::ptrace_scope(),
                },
                e => Error::Io(io::Error::from(e)),
            });
        }
        Ok(())
    }

    /// Espera até `timeout` o processo terminar; `true` se já terminou.
    ///
    /// `EINTR` vira `Ok(false)`: é o caminho normal quando chega SIGINT/SIGTERM, e
    /// quem chama reavalia o que fazer.
    pub fn exited(&self, timeout: Duration) -> Result<bool> {
        let mut fds = [PollFd::new(self.fd.as_fd(), PollFlags::POLLIN)];
        let timeout = PollTimeout::try_from(timeout).unwrap_or(PollTimeout::MAX);
        match poll(&mut fds, timeout) {
            Ok(n) => Ok(n > 0),
            Err(Errno::EINTR) => Ok(false),
            Err(e) => Err(Error::Io(io::Error::from(e))),
        }
    }
}

/// Ignora SIGTSTP, SIGTTIN e SIGTTOU.
///
/// Evita que Ctrl+Z ou a leitura/escrita no terminal parem o `gt-patch` no meio de
/// uma pausa do alvo, deixando-o parado.
pub fn ignore_job_control() -> Result<()> {
    for sig in [Signal::SIGTSTP, Signal::SIGTTIN, Signal::SIGTTOU] {
        // SAFETY: `SigIgn` não instala função alguma, então não há handler que precise
        // ser async-signal-safe; só muda a disposição do sinal.
        unsafe { signal(sig, SigHandler::SigIgn) }.map_err(|e| Error::Io(io::Error::from(e)))?;
    }
    Ok(())
}
