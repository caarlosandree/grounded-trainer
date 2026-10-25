//! Leitura (e, com a feature `write`, escrita crua) da memória de outro processo.

#[cfg(feature = "test-support")]
pub mod test_support;
#[cfg(feature = "write")]
mod writer;

#[cfg(feature = "write")]
pub use writer::MemWriter;

use std::io::{self, IoSliceMut};
use std::ops::Range;

use nix::errno::Errno;
use nix::sys::uio::{RemoteIoVec, process_vm_readv};
use nix::unistd::Pid;

use crate::{Error, Result};

/// Tamanho máximo de cada chamada a `process_vm_readv`.
const CHUNK: usize = 1 << 20;

/// Tamanho da página usada para localizar buracos na leitura tolerante.
const PAGE: u64 = 4096;

/// Leitor da memória de um processo via `process_vm_readv`.
#[derive(Debug, Clone, Copy)]
pub struct MemReader {
    pid: i32,
}

/// Resultado de [`MemReader::read_lossy`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LossyRead {
    /// Bytes lidos; as páginas ilegíveis ficam zeradas.
    pub bytes: Vec<u8>,
    /// Intervalos absolutos ilegíveis, já mesclados.
    pub holes: Vec<Range<u64>>,
}

impl MemReader {
    /// Cria um leitor para o processo `pid`.
    pub fn new(pid: i32) -> MemReader {
        MemReader { pid }
    }

    /// Lê exatamente `buf.len()` bytes a partir de `addr`, em blocos de até 1 MiB.
    ///
    /// Leitura curta ou `EFAULT` viram [`Error::Unmapped`]; `ESRCH` vira
    /// [`Error::ProcessGone`]; `EPERM` vira [`Error::PermissionDenied`].
    pub fn read_exact(&self, addr: u64, buf: &mut [u8]) -> Result<()> {
        let total = buf.len();
        let unmapped = |at: u64| Error::Unmapped {
            addr: at,
            len: total,
        };
        let mut done = 0usize;
        while done < total {
            let n = (total - done).min(CHUNK);
            let at = addr.checked_add(done as u64).ok_or(unmapped(addr))?;
            // O fim do bloco também precisa caber no espaço de endereços.
            at.checked_add(n as u64).ok_or(unmapped(at))?;
            let got = self.readv(at, &mut buf[done..done + n])?;
            if got < n {
                return Err(unmapped(at + got as u64));
            }
            done += n;
        }
        Ok(())
    }

    /// Lê `len` bytes a partir de `addr`, tolerando páginas ilegíveis.
    ///
    /// Tenta o bloco inteiro; se falhar por memória não mapeada, repete página a
    /// página (4096 bytes, alinhadas). As páginas ilegíveis ficam zeradas e entram
    /// em `holes`. Se nenhuma página for legível, devolve [`Error::Unmapped`].
    pub fn read_lossy(&self, addr: u64, len: usize) -> Result<LossyRead> {
        let unmapped = Error::Unmapped { addr, len };
        let end = addr.checked_add(len as u64).ok_or(unmapped)?;
        let mut bytes = vec![0u8; len];
        match self.read_exact(addr, &mut bytes) {
            Ok(()) => {
                return Ok(LossyRead {
                    bytes,
                    holes: Vec::new(),
                });
            }
            Err(Error::Unmapped { .. }) => {}
            Err(e) => return Err(e),
        }

        let mut holes: Vec<Range<u64>> = Vec::new();
        let mut readable = false;
        let mut at = addr;
        while at < end {
            let next = (at / PAGE + 1) * PAGE;
            let piece_end = next.min(end);
            let range = (at - addr) as usize..(piece_end - addr) as usize;
            match self.read_exact(at, &mut bytes[range.clone()]) {
                Ok(()) => readable = true,
                Err(Error::Unmapped { .. }) => {
                    bytes[range].fill(0);
                    match holes.last_mut() {
                        Some(last) if last.end == at => last.end = piece_end,
                        _ => holes.push(at..piece_end),
                    }
                }
                Err(e) => return Err(e),
            }
            at = piece_end;
        }
        if !readable {
            return Err(Error::Unmapped { addr, len });
        }
        Ok(LossyRead { bytes, holes })
    }

    /// Uma única chamada a `process_vm_readv`; devolve quantos bytes foram lidos.
    fn readv(&self, addr: u64, buf: &mut [u8]) -> Result<usize> {
        let remote = [RemoteIoVec {
            base: addr as usize,
            len: buf.len(),
        }];
        let mut local = [IoSliceMut::new(buf)];
        match process_vm_readv(Pid::from_raw(self.pid), &mut local, &remote) {
            Ok(n) => Ok(n),
            Err(Errno::ESRCH) => Err(Error::ProcessGone { pid: self.pid }),
            Err(Errno::EPERM) => Err(Error::PermissionDenied {
                pid: self.pid,
                ptrace_scope: crate::process::ptrace_scope(),
            }),
            // Começar a leitura numa página ilegível: nada foi lido.
            Err(Errno::EFAULT) => Ok(0),
            Err(e) => Err(Error::Io(io::Error::from(e))),
        }
    }
}
