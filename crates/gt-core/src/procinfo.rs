//! Informações de `/proc`: estado e identidade do processo, threads e posição de cada thread.

use std::fs;
use std::io;

use nix::errno::Errno;

use crate::{Error, Result};

/// Campos de `/proc/<pid>/stat` que o projeto usa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcStat {
    /// Estado (campo 3): `R`, `S`, `D`, `T`, `t`, `Z`, `X`...
    pub state: char,
    /// Instante de início, em ticks desde o boot (campo 22).
    pub start_time: u64,
}

/// Onde uma thread está, segundo `/proc/<pid>/task/<tid>/syscall`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadPos {
    /// A thread está rodando; o kernel não informa a posição.
    Running,
    /// A thread está parada.
    Stopped {
        /// Número da syscall em curso; `None` se está fora de syscall (`-1`).
        syscall: Option<i64>,
        /// Contador de programa.
        pc: u64,
    },
}

/// Identidade de um processo: protege contra a reutilização do PID e contra reboot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub pid: i32,
    pub start_time: u64,
    pub boot_id: String,
}

fn bad_proc(file: impl Into<String>, reason: impl Into<String>) -> Error {
    Error::BadProc {
        file: file.into(),
        reason: reason.into(),
    }
}

/// Interpreta o conteúdo de `/proc/<pid>/stat`.
///
/// O nome do comando (campo 2) pode ter espaços e parênteses; ele termina no
/// **último** `)` do texto.
pub fn parse_stat(text: &str) -> Result<ProcStat> {
    let close = text
        .rfind(')')
        .ok_or_else(|| bad_proc("stat", "sem ')' após o nome do comando"))?;
    let mut fields = text[close + 1..].split_whitespace();
    // `fields` começa no campo 3 (estado); o campo 22 é o de índice 19.
    let state = fields
        .next()
        .and_then(|f| f.chars().next())
        .ok_or_else(|| bad_proc("stat", "sem o campo de estado"))?;
    let start_time = fields
        .nth(18)
        .ok_or_else(|| bad_proc("stat", "faltam campos até o start_time"))?
        .parse::<u64>()
        .map_err(|e| bad_proc("stat", format!("start_time inválido: {e}")))?;
    Ok(ProcStat { state, start_time })
}

fn is_gone(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error().map(Errno::from_raw),
        Some(Errno::ENOENT | Errno::ESRCH)
    )
}

/// Lê `/proc/<pid>/stat`. Processo inexistente vira [`Error::ProcessGone`].
pub fn read_stat(pid: i32) -> Result<ProcStat> {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(text) => parse_stat(&text),
        Err(e) if is_gone(&e) => Err(Error::ProcessGone { pid }),
        Err(e) => Err(Error::Io(e)),
    }
}

/// Lê `/proc/<pid>/task/<tid>/stat`. `None` significa que a thread sumiu (não é
/// [`Error::ProcessGone`]: threads vêm e vão com o processo vivo).
pub fn read_task_stat(pid: i32, tid: i32) -> Result<Option<ProcStat>> {
    match fs::read_to_string(format!("/proc/{pid}/task/{tid}/stat")) {
        Ok(text) => parse_stat(&text).map(Some),
        Err(e) if is_gone(&e) => Ok(None),
        Err(e) => Err(Error::Io(e)),
    }
}

/// O processo morreu (zumbi ou removido).
pub fn is_dead(state: char) -> bool {
    matches!(state, 'Z' | 'X')
}

/// A thread está parada por sinal (`T`) ou por ptrace (`t`).
pub fn is_stopped(state: char) -> bool {
    matches!(state, 'T' | 't')
}

/// Interpreta o conteúdo de `/proc/<pid>/task/<tid>/syscall`.
///
/// Formatos: `running`; `-1 <sp> <pc>` (parada fora de syscall); ou
/// `<nº> <6 argumentos> <sp> <pc>` (parada dentro de syscall).
pub fn parse_syscall(text: &str) -> Result<ThreadPos> {
    let text = text.trim();
    if text == "running" {
        return Ok(ThreadPos::Running);
    }
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let (Some(first), Some(last)) = (tokens.first(), tokens.last()) else {
        return Err(bad_proc("syscall", "arquivo vazio"));
    };
    if tokens.len() < 3 {
        return Err(bad_proc("syscall", "poucos campos"));
    }
    let nr = first
        .parse::<i64>()
        .map_err(|e| bad_proc("syscall", format!("número da syscall inválido: {e}")))?;
    let pc = last
        .strip_prefix("0x")
        .and_then(|h| u64::from_str_radix(h, 16).ok())
        .ok_or_else(|| bad_proc("syscall", format!("contador de programa inválido: {last}")))?;
    Ok(ThreadPos::Stopped {
        syscall: (nr != -1).then_some(nr),
        pc,
    })
}

/// Lê a posição de uma thread. `None` se a thread sumiu ou o arquivo não pôde ser lido.
pub fn read_thread_pos(pid: i32, tid: i32) -> Result<Option<ThreadPos>> {
    match fs::read_to_string(format!("/proc/{pid}/task/{tid}/syscall")) {
        Ok(text) => parse_syscall(&text).map(Some),
        Err(e) => {
            if !is_gone(&e) {
                tracing::debug!(pid, tid, erro = %e, "syscall da thread ilegível");
            }
            Ok(None)
        }
    }
}

/// Lista os TIDs do processo, em ordem crescente.
pub fn list_tids(pid: i32) -> Result<Vec<i32>> {
    let dir = match fs::read_dir(format!("/proc/{pid}/task")) {
        Ok(d) => d,
        Err(e) if is_gone(&e) => return Err(Error::ProcessGone { pid }),
        Err(e) => return Err(Error::Io(e)),
    };
    let mut tids = Vec::new();
    for entry in dir {
        match entry {
            Ok(entry) => {
                if let Some(tid) = entry.file_name().to_str().and_then(|n| n.parse().ok()) {
                    tids.push(tid);
                }
            }
            // A thread sumiu durante a listagem.
            Err(e) if is_gone(&e) => {}
            Err(e) => return Err(Error::Io(e)),
        }
    }
    tids.sort_unstable();
    Ok(tids)
}

/// Identificador do boot atual (`/proc/sys/kernel/random/boot_id`), sem espaços.
pub fn boot_id() -> Result<String> {
    Ok(fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .to_string())
}

/// Identidade atual do processo. Processo morto (zumbi) vira [`Error::ProcessGone`].
pub fn identity(pid: i32) -> Result<Identity> {
    let stat = read_stat(pid)?;
    if is_dead(stat.state) {
        return Err(Error::ProcessGone { pid });
    }
    Ok(Identity {
        pid,
        start_time: stat.start_time,
        boot_id: boot_id()?,
    })
}

/// Confirma que `id` ainda é o mesmo processo vivo.
///
/// [`Error::ProcessGone`] se o PID sumiu, está morto, mudou de `start_time` (PID
/// reutilizado) ou o `boot_id` é outro (máquina reiniciada).
pub fn check_alive(id: &Identity) -> Result<()> {
    let stat = read_stat(id.pid)?;
    if is_dead(stat.state) || stat.start_time != id.start_time || boot_id()? != id.boot_id {
        return Err(Error::ProcessGone { pid: id.pid });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_with_spaces_and_parens_in_name() {
        let s =
            parse_stat("1234 (a b) (c)) T 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 987654 0")
                .unwrap();
        assert_eq!((s.state, s.start_time), ('T', 987654));
    }

    #[test]
    fn syscall_variants() {
        assert_eq!(parse_syscall("running\n").unwrap(), ThreadPos::Running);
        assert_eq!(
            parse_syscall("-1 0x7fe12e3fc6e0 0x7fe13ea38d4c\n").unwrap(),
            ThreadPos::Stopped {
                syscall: None,
                pc: 0x7fe13ea38d4c
            }
        );
        assert_eq!(
            parse_syscall(
                "202 0x7fe13eda65c0 0x89 0x166 0x7ffdf0fff480 0x0 0xffffffff 0x7ffdf0fff338 0x7fe13e682352\n"
            )
            .unwrap(),
            ThreadPos::Stopped {
                syscall: Some(202),
                pc: 0x7fe13e682352
            }
        );
    }

    #[test]
    fn bad_stat_and_syscall_are_errors() {
        for bad in ["", "1 (x", "abc"] {
            assert!(
                matches!(parse_stat(bad), Err(crate::Error::BadProc { .. })),
                "{bad:?}"
            );
            assert!(
                matches!(parse_syscall(bad), Err(crate::Error::BadProc { .. })),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn dead_and_stopped_states() {
        assert!(is_dead('Z'));
        assert!(is_dead('X'));
        assert!(!is_dead('T'));
        assert!(is_stopped('T'));
        assert!(is_stopped('t'));
        assert!(!is_stopped('R'));
    }
}
