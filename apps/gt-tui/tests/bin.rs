#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]
//! Integração do binário `gt-tui` (spec 003, 8 e 10): erros de início sem terminal e saída por
//! sinais ou terminal fechado, num pseudo-terminal.
//!
//! O jogo pode estar aberto na máquina de quem roda os testes: os de sinal e restauração usam
//! `--pid` de um processo filho já terminado e colhido, que leva ao estado `o processo terminou`.
//! Só o teste do `aguardando o jogo` (sem `--pid`) pula com o jogo aberto.
//!
//! O ratatui pode trocar espaços por movimentos de cursor: o que se procura na saída são
//! palavras soltas, nunca a frase inteira.

use std::io::Read;
use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use gt_core::journal::JournalStore;
use nix::errno::Errno;
use nix::fcntl::{FcntlArg, FdFlag, fcntl};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::pty::{Winsize, openpty};
use nix::sys::signal::{Signal, kill};
use nix::sys::termios::Termios;
use nix::unistd::Pid;

/// Sair da tela alternativa: o terminal foi restaurado.
const LEAVE_ALT_SCREEN: &str = "\x1b[?1049l";

/// Prazo para o binário mostrar algo ou sair.
const TIMEOUT: Duration = Duration::from_secs(5);

/// O jogo está aberto? Os testes que dependem da ausência dele pulam com aviso.
fn game_is_open() -> bool {
    let procs = gt_core::process::list_processes().unwrap();
    let open = procs
        .iter()
        .any(|(_, args)| gt_grounded2::target::is_game(args));
    if open {
        eprintln!("aviso: o jogo está aberto; teste ignorado");
    }
    open
}

/// Diretório do diário dentro de `state` (`$XDG_STATE_HOME/grounded-trainer`).
fn journal_dir(state: &Path) -> std::path::PathBuf {
    state.join("grounded-trainer")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Serializa os `spawn` dos testes: o `openpty` não marca os descritores com `CLOEXEC`, e um
/// filho de outro teste, criado entre o `openpty` e a marcação, herdaria o mestre e o manteria
/// aberto (o `gt-tui` nunca veria o hangup).
static SPAWN: Mutex<()> = Mutex::new(());

fn spawn_lock() -> MutexGuard<'static, ()> {
    SPAWN.lock().unwrap_or_else(|e| e.into_inner())
}

/// PID de um processo que já terminou e foi colhido.
fn dead_pid() -> i32 {
    let _guard = spawn_lock();
    let mut child = Command::new("true").spawn().unwrap();
    let pid = i32::try_from(child.id()).unwrap();
    child.wait().unwrap();
    pid
}

/// Lê o mestre do pseudo-terminal para um buffer compartilhado, até o fim (EIO quando o
/// escravo fecha) ou até `close`. A thread é a dona do mestre: ao terminar, ele fecha.
struct Master {
    buf: Arc<Mutex<Vec<u8>>>,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
}

impl Master {
    fn spawn(master: OwnedFd) -> Master {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let reader = {
            let (buf, stop) = (Arc::clone(&buf), Arc::clone(&stop));
            thread::spawn(move || read_master(master, &buf, &stop))
        };
        Master {
            buf,
            stop,
            reader: Some(reader),
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.buf.lock().unwrap()).into_owned()
    }

    /// Espera a saída conter todas as palavras (cada uma separada), até `TIMEOUT`.
    fn wait_words(&self, words: &[&str]) {
        let start = Instant::now();
        loop {
            let text = self.text();
            if words.iter().all(|w| text.contains(w)) {
                return;
            }
            assert!(
                start.elapsed() < TIMEOUT,
                "faltaram {words:?} na saída: {text:?}"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Espera a leitura acabar sozinha (o escravo fechou) e devolve tudo o que saiu.
    fn drain(&mut self) -> String {
        let start = Instant::now();
        while self.reader.as_ref().is_some_and(|r| !r.is_finished()) {
            assert!(start.elapsed() < TIMEOUT, "a leitura do mestre não acabou");
            thread::sleep(Duration::from_millis(10));
        }
        self.close()
    }

    /// Fecha o mestre (a thread solta o descritor) e devolve o que foi lido.
    fn close(&mut self) -> String {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(reader) = self.reader.take() {
            reader.join().unwrap();
        }
        self.text()
    }
}

fn read_master(master: OwnedFd, buf: &Mutex<Vec<u8>>, stop: &AtomicBool) {
    let mut file = std::fs::File::from(master.try_clone().unwrap());
    let mut chunk = [0u8; 4096];
    while !stop.load(Ordering::SeqCst) {
        let mut fds = [PollFd::new(master.as_fd(), PollFlags::POLLIN)];
        match poll(&mut fds, PollTimeout::from(50u8)) {
            Ok(0) | Err(Errno::EINTR) => continue,
            Ok(_) => {}
            Err(_) => return,
        }
        match file.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.lock().unwrap().extend_from_slice(&chunk[..n]),
        }
    }
}

/// O `gt-tui` rodando num pseudo-terminal de 80×24.
struct Tui {
    child: Child,
    master: Master,
}

impl Tui {
    fn spawn(state: &Path, args: &[&str]) -> Tui {
        let size = Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let _guard = spawn_lock();
        let pty = openpty(Some(&size), None::<&Termios>).unwrap();
        for fd in [&pty.master, &pty.slave] {
            fcntl(fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).unwrap();
        }
        let stdin = Stdio::from(pty.slave.try_clone().unwrap());
        let stdout = Stdio::from(pty.slave);
        let child = Command::new(env!("CARGO_BIN_EXE_gt-tui"))
            .env("XDG_STATE_HOME", state)
            .env("TERM", "xterm-256color")
            .args(args)
            .stdin(stdin)
            .stdout(stdout)
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // O `Command` já foi consumido pelo `spawn`: o processo de teste não guarda o escravo.
        Tui {
            child,
            master: Master::spawn(pty.master),
        }
    }

    fn signal(&self, signal: Signal) {
        let pid = Pid::from_raw(i32::try_from(self.child.id()).unwrap());
        kill(pid, signal).unwrap();
    }

    /// Espera a saída do processo (mata se passar do prazo) e devolve o código e o `stderr`.
    fn wait_exit(&mut self) -> (Option<i32>, String) {
        let start = Instant::now();
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            if start.elapsed() > TIMEOUT {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!("o gt-tui não saiu; saída: {:?}", self.master.text());
            }
            thread::sleep(Duration::from_millis(20));
        };
        let mut err = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut err)
            .unwrap();
        (status.code(), err)
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Roda o `gt-tui` sem terminal (`stdin` e `stdout` não são terminais).
fn run_plain(state: &Path) -> Output {
    let _guard = spawn_lock();
    Command::new(env!("CARGO_BIN_EXE_gt-tui"))
        .env("XDG_STATE_HOME", state)
        .output()
        .unwrap()
}

/// Sobe o `gt-tui --pid <morto>`, espera o estado final e termina pelo `signal`.
fn exit_by_signal_after_terminated(signal: Signal) {
    let state = tempfile::tempdir().unwrap();
    let pid = dead_pid().to_string();
    let mut tui = Tui::spawn(state.path(), &["--pid", &pid]);
    tui.master.wait_words(&["processo", "terminou"]);

    tui.signal(signal);
    let (code, err) = tui.wait_exit();
    let out = tui.master.drain();
    assert_eq!(code, Some(0), "stderr: {err:?}; saída: {out:?}");
    assert!(err.is_empty(), "stderr: {err:?}");
    assert!(out.contains(LEAVE_ALT_SCREEN), "saída: {out:?}");
}

#[test]
fn no_tty_exits_with_message() {
    let state = tempfile::tempdir().unwrap();
    let out = run_plain(state.path());
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("o gt-tui precisa de um terminal"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn lock_busy_exits_with_message() {
    let state = tempfile::tempdir().unwrap();
    let store = JournalStore::new(journal_dir(state.path()));
    let _lock = store.lock().unwrap();
    let out = run_plain(state.path());
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("outro gt-patch ou gt-tui já está em execução"),
        "{}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("feche o outro programa antes"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn waiting_then_sigterm() {
    if game_is_open() {
        return;
    }
    let state = tempfile::tempdir().unwrap();
    let mut tui = Tui::spawn(state.path(), &[]);
    tui.master.wait_words(&["aguardando", "jogo"]);

    tui.signal(Signal::SIGTERM);
    let (code, err) = tui.wait_exit();
    let out = tui.master.drain();
    assert_eq!(code, Some(0), "stderr: {err:?}; saída: {out:?}");
    assert!(err.is_empty(), "stderr: {err:?}");
    assert!(out.contains(LEAVE_ALT_SCREEN), "saída: {out:?}");
}

#[test]
fn terminated_then_sigterm() {
    exit_by_signal_after_terminated(Signal::SIGTERM);
}

#[test]
fn terminated_then_sigquit() {
    exit_by_signal_after_terminated(Signal::SIGQUIT);
}

#[test]
fn sighup_with_closed_pty_restores() {
    let state = tempfile::tempdir().unwrap();
    let pid = dead_pid().to_string();
    let mut tui = Tui::spawn(state.path(), &["--pid", &pid]);
    tui.master.wait_words(&["processo", "terminou"]);

    let seen = tui.master.close();
    assert!(!seen.contains(LEAVE_ALT_SCREEN), "saída: {seen:?}");
    tui.signal(Signal::SIGHUP);
    let (code, err) = tui.wait_exit();
    assert_eq!(code, Some(0), "stderr: {err:?}");
    assert!(err.is_empty(), "stderr: {err:?}");
}

#[test]
fn eio_without_signal_exits_cleanly() {
    let state = tempfile::tempdir().unwrap();
    let pid = dead_pid().to_string();
    let mut tui = Tui::spawn(state.path(), &["--pid", &pid]);
    tui.master.wait_words(&["processo", "terminou"]);

    // Fecha o mestre sem mandar sinal nenhum: a leitura e o desenho do filho falham com EIO.
    tui.master.close();
    let (code, err) = tui.wait_exit();
    assert_eq!(code, Some(0), "stderr: {err:?}");
    assert!(err.is_empty(), "stderr: {err:?}");
}
