//! Leitura e escrita cruas em `/proc/<pid>/mem` (feature `write`).

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;

use nix::errno::Errno;

use crate::{Error, Result};

/// Escritor (e leitor) da memória de um processo via `/proc/<pid>/mem`.
///
/// O descritor fica preso à memória do processo aberto: um PID reaproveitado depois
/// da abertura nunca é lido nem escrito por ele. Bytes esperados, pausa e restauração
/// ficam com o motor (`gt_core::engine`).
#[derive(Debug)]
pub struct MemWriter {
    pid: i32,
    file: File,
}

impl MemWriter {
    /// Abre `/proc/<pid>/mem` para leitura e escrita.
    pub fn open(pid: i32) -> Result<MemWriter> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!("/proc/{pid}/mem"))
            .map_err(|e| map_io(pid, e, 0, 0))?;
        Ok(MemWriter { pid, file })
    }

    /// Escreve `data` em `addr`.
    pub fn write(&self, addr: u64, data: &[u8]) -> Result<()> {
        let len = data.len();
        if addr.checked_add(len as u64).is_none() {
            return Err(Error::Unmapped { addr, len });
        }
        self.file
            .write_all_at(data, addr)
            .map_err(|e| map_io(self.pid, e, addr, data.len()))
    }

    /// Lê exatamente `buf.len()` bytes a partir de `addr` pelo mesmo descritor.
    ///
    /// Endereço não mapeado vira [`Error::Unmapped`]; processo sem memória (terminou
    /// com o arquivo aberto) vira [`Error::ProcessGone`].
    pub fn read_exact(&self, addr: u64, buf: &mut [u8]) -> Result<()> {
        let len = buf.len();
        if addr.checked_add(len as u64).is_none() {
            return Err(Error::Unmapped { addr, len });
        }
        self.file
            .read_exact_at(buf, addr)
            .map_err(|e| map_io(self.pid, e, addr, len))
    }
}

/// Traduz um erro de E/S de `/proc/<pid>/mem` para o erro comum do núcleo.
fn map_io(pid: i32, e: io::Error, addr: u64, len: usize) -> Error {
    // Processo que terminou com o arquivo ainda aberto: o kernel devolve 0 bytes
    // escritos, que `write_all_at` reporta como `WriteZero` (sem `errno`).
    if e.kind() == io::ErrorKind::WriteZero && e.raw_os_error().is_none() {
        return Error::ProcessGone { pid };
    }
    // Na leitura é o mesmo caso: 0 bytes lidos, que `read_exact_at` reporta como
    // `UnexpectedEof` (endereço não mapeado num processo vivo dá `EIO`, não 0).
    if e.kind() == io::ErrorKind::UnexpectedEof && e.raw_os_error().is_none() {
        return Error::ProcessGone { pid };
    }
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
