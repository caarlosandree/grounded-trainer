//! Escrita crua em `/proc/<pid>/mem` (feature `write`).

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;

use nix::errno::Errno;

use crate::{Error, Result};

/// Escritor da memória de um processo via `/proc/<pid>/mem`.
///
/// Escrita crua; a Fase 2 adiciona bytes esperados, pausa e restauração.
#[derive(Debug)]
pub struct MemWriter {
    pid: i32,
    file: File,
}

impl MemWriter {
    /// Abre `/proc/<pid>/mem` para escrita.
    pub fn open(pid: i32) -> Result<MemWriter> {
        let file = OpenOptions::new()
            .write(true)
            .open(format!("/proc/{pid}/mem"))
            .map_err(|e| map_io(pid, e, 0, 0))?;
        Ok(MemWriter { pid, file })
    }

    /// Escreve `data` em `addr`.
    pub fn write(&self, addr: u64, data: &[u8]) -> Result<()> {
        self.file
            .write_all_at(data, addr)
            .map_err(|e| map_io(self.pid, e, addr, data.len()))
    }
}

/// Traduz um erro de E/S de `/proc/<pid>/mem` para o erro comum do núcleo.
fn map_io(pid: i32, e: io::Error, addr: u64, len: usize) -> Error {
    match e.raw_os_error().map(Errno::from_raw) {
        Some(Errno::EACCES | Errno::EPERM) => Error::PermissionDenied {
            pid,
            ptrace_scope: crate::process::ptrace_scope(),
        },
        Some(Errno::ESRCH | Errno::ENOENT) => Error::ProcessGone { pid },
        // Escrever em endereço não mapeado devolve EIO (ou EFAULT).
        Some(Errno::EIO | Errno::EFAULT) => Error::Unmapped { addr, len },
        _ => Error::Io(e),
    }
}
