//! Leitura (e, com a feature `write`, escrita crua) da memória de outro processo.

#[cfg(feature = "write")]
mod sys;
#[cfg(feature = "test-support")]
pub mod test_support;
#[cfg(feature = "write")]
mod writer;

#[cfg(feature = "write")]
pub use sys::{Pidfd, ignore_job_control};
#[cfg(feature = "write")]
pub use writer::MemWriter;

use std::io::{self, IoSliceMut};
use std::ops::Range;
use std::sync::OnceLock;

use nix::errno::Errno;
use nix::sys::uio::{RemoteIoVec, process_vm_readv};
use nix::unistd::Pid;

use crate::{Error, Result};

/// Tamanho máximo de cada chamada a `process_vm_readv`.
const CHUNK: usize = 1 << 20;

/// Máximo de `iovec` remotos por chamada de `process_vm_readv` em [`ReadMem::read_many`]
/// (o kernel aceita até `UIO_MAXIOV` = 1024).
const IOV_MAX: usize = 1024;

/// Tamanho de página assumido se `sysconf(_SC_PAGE_SIZE)` falhar.
const DEFAULT_PAGE: u64 = 4096;

/// Tamanho da página do sistema, em bytes (consultado uma vez; 4096 se a consulta falhar).
pub fn page_size() -> u64 {
    static PAGE_SIZE: OnceLock<u64> = OnceLock::new();
    *PAGE_SIZE.get_or_init(
        || match nix::unistd::sysconf(nix::unistd::SysconfVar::PAGE_SIZE) {
            Ok(Some(n)) if n > 0 => n as u64,
            _ => DEFAULT_PAGE,
        },
    )
}

/// Interface de leitura da memória de um processo.
///
/// Existe para que as camadas acima (ex. o leitor de reflexão do Unreal) leiam sem
/// depender de [`MemReader`] e possam ser testadas com uma memória sintética.
pub trait ReadMem {
    /// Lê exatamente `buf.len()` bytes a partir de `addr`.
    ///
    /// Memória não mapeada vira [`Error::Unmapped`]; processo morto, [`Error::ProcessGone`];
    /// falta de permissão, [`Error::PermissionDenied`].
    fn read_exact(&self, addr: u64, buf: &mut [u8]) -> Result<()>;

    /// Lê vários trechos `(endereço, tamanho)`; o resultado tem a ordem dos pedidos.
    ///
    /// Um trecho em memória não mapeada ([`Error::Unmapped`]) vira `None`; qualquer outro erro
    /// (`ProcessGone`, `PermissionDenied`, ...) interrompe a leitura e volta como `Err`.
    /// A implementação padrão faz um [`ReadMem::read_exact`] por pedido.
    fn read_many(&self, reqs: &[(u64, usize)]) -> Result<Vec<Option<Vec<u8>>>> {
        reqs.iter()
            .map(|&(addr, len)| {
                let mut buf = vec![0u8; len];
                match self.read_exact(addr, &mut buf) {
                    Ok(()) => Ok(Some(buf)),
                    Err(Error::Unmapped { .. }) => Ok(None),
                    Err(e) => Err(e),
                }
            })
            .collect()
    }
}

impl ReadMem for MemReader {
    fn read_exact(&self, addr: u64, buf: &mut [u8]) -> Result<()> {
        MemReader::read_exact(self, addr, buf)
    }

    /// Agrupa os pedidos em chamadas de `process_vm_readv` com até 1024 `iovec` remotos.
    ///
    /// Numa leitura curta, o pedido em que ela parou é relido sozinho com
    /// [`MemReader::read_exact`] (que decide entre `None` e o erro) e o lote segue do pedido
    /// seguinte.
    fn read_many(&self, reqs: &[(u64, usize)]) -> Result<Vec<Option<Vec<u8>>>> {
        let mut out: Vec<Option<Vec<u8>>> = vec![None; reqs.len()];
        let mut next = 0usize;
        while next < reqs.len() {
            // Pedidos vazios não viram `iovec`; endereço que estoura o espaço é `None`.
            let mut batch: Vec<usize> = Vec::with_capacity(IOV_MAX.min(reqs.len() - next));
            while next < reqs.len() && batch.len() < IOV_MAX {
                let (addr, len) = reqs[next];
                if len == 0 {
                    out[next] = Some(Vec::new());
                } else if addr.checked_add(len as u64).is_some() {
                    batch.push(next);
                }
                next += 1;
            }
            if batch.is_empty() {
                continue;
            }
            let mut bufs: Vec<Vec<u8>> = batch.iter().map(|&i| vec![0u8; reqs[i].1]).collect();
            let remote: Vec<RemoteIoVec> = batch
                .iter()
                .map(|&i| RemoteIoVec {
                    base: reqs[i].0 as usize,
                    len: reqs[i].1,
                })
                .collect();
            let mut local: Vec<IoSliceMut<'_>> =
                bufs.iter_mut().map(|b| IoSliceMut::new(b)).collect();
            let got = self.readv_many(&mut local, &remote)?;
            drop(local);

            // Pedidos inteiros lidos antes do ponto em que a chamada parou.
            let mut consumed = 0usize;
            let mut stopped = None;
            for (k, &i) in batch.iter().enumerate() {
                let len = reqs[i].1;
                if consumed + len <= got {
                    consumed += len;
                    out[i] = Some(std::mem::take(&mut bufs[k]));
                } else {
                    stopped = Some(i);
                    // Os pedidos depois do que parou não foram lidos: voltam à fila.
                    next = i + 1;
                    break;
                }
            }
            if let Some(i) = stopped {
                let (addr, len) = reqs[i];
                let mut buf = vec![0u8; len];
                match MemReader::read_exact(self, addr, &mut buf) {
                    Ok(()) => out[i] = Some(buf),
                    Err(Error::Unmapped { .. }) => {}
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(out)
    }
}

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
    /// página (tamanho de [`page_size`], alinhadas). As páginas ilegíveis ficam zeradas e entram
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

        let page = page_size();
        let mut holes: Vec<Range<u64>> = Vec::new();
        let mut readable = false;
        let mut at = addr;
        while at < end {
            // Próximo limite de página; se estourar o espaço de endereços, é o fim.
            let piece_end = at
                .checked_add(page - at % page)
                .map_or(end, |next| next.min(end));
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
        self.readv_many(&mut local, &remote)
    }

    /// Uma única chamada a `process_vm_readv` com vários `iovec`; devolve quantos bytes
    /// foram lidos (a chamada para no primeiro trecho ilegível).
    fn readv_many(&self, local: &mut [IoSliceMut<'_>], remote: &[RemoteIoVec]) -> Result<usize> {
        match process_vm_readv(Pid::from_raw(self.pid), local, remote) {
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// Memória sintética: só `0x1000..0x1008` é legível; `0x2000` simula processo morto.
    struct Fake;

    impl ReadMem for Fake {
        fn read_exact(&self, addr: u64, buf: &mut [u8]) -> Result<()> {
            match addr {
                0x1000 if buf.len() <= 8 => {
                    buf.fill(0x42);
                    Ok(())
                }
                0x2000 => Err(Error::ProcessGone { pid: 1 }),
                _ => Err(Error::Unmapped {
                    addr,
                    len: buf.len(),
                }),
            }
        }
    }

    #[test]
    fn default_read_many_maps_unmapped_to_none() {
        let got = Fake
            .read_many(&[(0x1000, 2), (0x3000, 4), (0x1000, 0)])
            .unwrap();
        assert_eq!(got, vec![Some(vec![0x42; 2]), None, Some(vec![])]);
    }

    #[test]
    fn default_read_many_stops_on_other_errors() {
        let r = Fake.read_many(&[(0x1000, 2), (0x2000, 4), (0x1000, 2)]);
        assert!(matches!(r, Err(Error::ProcessGone { .. })), "{r:?}");
    }

    #[test]
    fn page_size_is_power_of_two_and_at_least_4096() {
        let p = page_size();
        assert!(p >= 4096, "{p}");
        assert!(p.is_power_of_two(), "{p}");
    }
}
