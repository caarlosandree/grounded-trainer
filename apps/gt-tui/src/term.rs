//! Terminal da TUI (spec 003, 6.8): tela alternativa, modo cru, restauração uma vez só e o
//! gancho de pânico.
//!
//! O que o laço (`app`) precisa do terminal fica na trait [`Term`], para os testes usarem um
//! terminal falso. A restauração é protegida por [`RestoreOnce`]: o `main`, o gancho de pânico
//! e o fim normal compartilham uma marca, e só o primeiro a chegar restaura.

use std::io::{self, Stdout};
use std::mem::ManuallyDrop;
use std::os::fd::AsFd;
use std::panic;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, ThreadId};
use std::time::Duration;

use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyEvent};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::model::Model;
use crate::ui;

/// O que o laço precisa do terminal.
pub trait Term {
    /// Tamanho atual (largura, altura).
    fn size(&self) -> io::Result<(u16, u16)>;
    /// Desenha o modelo.
    fn draw(&mut self, model: &Model) -> io::Result<()>;
    /// Espera até `timeout` por teclas e devolve as que chegaram (pode ser vazio).
    fn read_keys(&mut self, timeout: Duration) -> io::Result<Vec<KeyEvent>>;
    /// Restaura o terminal (sai do modo cru e da tela alternativa, mostra o cursor).
    fn restore(&mut self) -> io::Result<()>;
}

/// Marca compartilhada de "o terminal já foi restaurado": só a primeira chamada de
/// [`run`](RestoreOnce::run) roda a restauração.
#[derive(Debug)]
pub struct RestoreOnce {
    done: AtomicBool,
}

impl RestoreOnce {
    /// Marca nova (a restauração ainda não aconteceu). `const` para valer num `static`.
    pub const fn new() -> RestoreOnce {
        RestoreOnce {
            done: AtomicBool::new(false),
        }
    }

    /// Roda `f` se for a primeira chamada; nas seguintes, não faz nada e devolve `Ok`.
    pub fn run(&self, f: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
        if self.done.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        f()
    }

    /// Se a restauração já começou (depois dela, ninguém mais desenha).
    pub fn done(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }
}

impl Default for RestoreOnce {
    fn default() -> RestoreOnce {
        RestoreOnce::new()
    }
}

/// Restaura o terminal pelo `stdout`: sai do modo cru e da tela alternativa e mostra o cursor.
/// Tenta os dois passos e devolve o primeiro erro.
pub fn restore_terminal() -> io::Result<()> {
    let raw = disable_raw_mode();
    let screen = execute!(io::stdout(), LeaveAlternateScreen, Show);
    raw.and(screen)
}

/// Instala o gancho de pânico (spec 6.8). Guarda o gancho anterior. Na thread principal,
/// restaura o terminal **antes** de chamar o anterior (que imprime a mensagem). Em outra
/// thread, só marca `elsewhere` e registra no log: o terminal não é tocado e o gancho anterior
/// não roda, porque ele escreveria no `stderr` com a TUI desenhada.
pub fn install_panic_hook(
    main_thread: ThreadId,
    restore: Arc<dyn Fn() + Send + Sync>,
    elsewhere: Arc<AtomicBool>,
) {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        if thread::current().id() == main_thread {
            restore();
            previous(info);
        } else {
            elsewhere.store(true, Ordering::SeqCst);
            tracing::error!("pânico em outra thread: {info}");
        }
    }));
}

/// Terminal de verdade: `crossterm` em tela alternativa e modo cru, com o cursor escondido.
///
/// As teclas são lidas por uma thread própria. Num terminal fechado (hangup), a leitura do
/// `crossterm` não devolve erro: ela repete o `read` com `EIO` sem parar, e uma chamada dela
/// na thread do laço nunca voltaria (nem para o laço ver o SIGHUP). Na thread de leitura, o
/// pior caso é ela girar até o processo sair; o laço só espera num canal e confere o hangup
/// com `poll(2)`.
///
/// O `Terminal` do `ratatui` fica em `ManuallyDrop`: o `Drop` dele tenta mostrar o cursor e, se
/// falhar (terminal fechado), escreve no `stderr` com `eprintln!`, que entra em pânico quando
/// o `stderr` também foi fechado. A restauração do terminal é só a de [`restore_terminal`].
pub struct CrosstermTerm {
    terminal: ManuallyDrop<Terminal<CrosstermBackend<Stdout>>>,
    keys: Receiver<KeyEvent>,
}

impl CrosstermTerm {
    /// Entra na tela alternativa e no modo cru. Se algum passo falhar, desfaz o que fez.
    pub fn new() -> io::Result<CrosstermTerm> {
        enable_raw_mode()?;
        let mut out = io::stdout();
        if let Err(e) = execute!(out, EnterAlternateScreen, Hide) {
            let _ = restore_terminal();
            return Err(e);
        }
        let started = Terminal::new(CrosstermBackend::new(out)).and_then(|terminal| {
            let (tx, keys) = mpsc::channel();
            thread::Builder::new()
                .name("gt-keys".to_string())
                .spawn(move || read_forever(&tx))?;
            Ok(CrosstermTerm {
                terminal: ManuallyDrop::new(terminal),
                keys,
            })
        });
        if started.is_err() {
            let _ = restore_terminal();
        }
        started
    }
}

/// Corpo da thread de leitura: manda as teclas pelo canal até o laço soltar o outro lado ou a
/// leitura falhar (o canal desconectado vira erro de leitura no laço).
fn read_forever(tx: &mpsc::Sender<KeyEvent>) {
    loop {
        match event::read() {
            Ok(Event::Key(key)) => {
                if tx.send(key).is_err() {
                    return;
                }
            }
            Ok(_) => {}
            Err(_) => return,
        }
    }
}

/// `Err` se o terminal (o `stdin`) foi fechado: `poll(2)` sempre informa `POLLHUP`, `POLLERR`
/// e `POLLNVAL`, mesmo sem pedir evento nenhum.
fn check_hangup() -> io::Result<()> {
    let stdin = io::stdin();
    let mut fds = [PollFd::new(stdin.as_fd(), PollFlags::empty())];
    poll(&mut fds, PollTimeout::ZERO).map_err(io::Error::from)?;
    let bad = PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL;
    if fds[0].revents().is_some_and(|r| r.intersects(bad)) {
        return Err(io::Error::other("o terminal foi fechado"));
    }
    Ok(())
}

impl Term for CrosstermTerm {
    fn size(&self) -> io::Result<(u16, u16)> {
        crossterm::terminal::size()
    }

    fn draw(&mut self, model: &Model) -> io::Result<()> {
        self.terminal
            .draw(|frame| ui::render(model, frame))
            .map(|_| ())
    }

    fn read_keys(&mut self, timeout: Duration) -> io::Result<Vec<KeyEvent>> {
        check_hangup()?;
        let mut keys = Vec::new();
        match self.keys.recv_timeout(timeout) {
            Ok(key) => keys.push(key),
            Err(RecvTimeoutError::Timeout) => return Ok(keys),
            Err(RecvTimeoutError::Disconnected) => {
                return Err(io::Error::other("a leitura do terminal terminou"));
            }
        }
        keys.extend(self.keys.try_iter());
        Ok(keys)
    }

    fn restore(&mut self) -> io::Result<()> {
        restore_terminal()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn restore_once_runs_once() {
        let once = RestoreOnce::new();
        assert!(!once.done());
        let mut calls = 0;
        assert!(
            once.run(|| {
                calls += 1;
                Ok(())
            })
            .is_ok()
        );
        assert!(once.done());
        // A segunda chamada não roda `f`, mesmo que ele falhasse.
        assert!(
            once.run(|| {
                calls += 1;
                Err(io::Error::other("não deveria rodar"))
            })
            .is_ok()
        );
        assert_eq!(calls, 1);
    }

    #[test]
    fn restore_once_marks_done_even_when_restore_fails() {
        let once = RestoreOnce::new();
        let res = once.run(|| Err(io::Error::from_raw_os_error(5)));
        assert!(res.is_err());
        assert!(once.done());
    }
}
