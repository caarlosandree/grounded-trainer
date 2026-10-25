//! Apoio aos testes (feature `test-support`): `mmap`/`prctl` para o processo-alvo de teste.
//!
//! Nunca escreve em outro processo. As páginas criadas aqui vivem até o processo
//! terminar (não há `munmap` final).

use std::io;
use std::ptr;

use crate::Result;

pub use super::page_size;

/// Tamanho da página do sistema como `usize`.
fn page() -> usize {
    page_size() as usize
}

/// Permite que qualquer processo do mesmo usuário nos inspecione
/// (`prctl(PR_SET_PTRACER, PR_SET_PTRACER_ANY)`), mesmo com `ptrace_scope = 1`.
pub fn allow_any_tracer() -> Result<()> {
    // SAFETY: `prctl` com `PR_SET_PTRACER` só lê os argumentos inteiros (sem
    // ponteiros) e altera um atributo do próprio processo.
    let rc = unsafe { libc::prctl(libc::PR_SET_PTRACER, libc::PR_SET_PTRACER_ANY, 0, 0, 0) };
    if rc == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    // EINVAL: kernel sem Yama; não há restrição a relaxar.
    if err.raw_os_error() == Some(libc::EINVAL) {
        Ok(())
    } else {
        Err(err.into())
    }
}

/// Mapeia `len` bytes anônimos de leitura e escrita e devolve o início.
fn map_anon(len: usize) -> Result<*mut libc::c_void> {
    // SAFETY: mapeamento anônimo e privado com endereço nulo: o kernel escolhe uma
    // região nova e livre; nenhuma memória existente é afetada.
    let p = unsafe {
        libc::mmap(
            ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if p == libc::MAP_FAILED {
        Err(io::Error::last_os_error().into())
    } else {
        Ok(p)
    }
}

/// Muda a proteção de `[p, p + len)`.
fn protect(p: *mut libc::c_void, len: usize, prot: libc::c_int) -> Result<()> {
    // SAFETY: `p` e `len` descrevem páginas alinhadas que este módulo acabou de
    // mapear com `map_anon` e que ninguém mais referencia.
    if unsafe { libc::mprotect(p, len, prot) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error().into())
    }
}

/// Cria um mapeamento anônimo `rw`, zerado, de `len` bytes e devolve o início.
pub fn anon_rw(len: usize) -> Result<u64> {
    Ok(map_anon(len)? as u64)
}

/// Cria uma página anônima `PROT_READ | PROT_EXEC` preenchida com
/// `fill` (via `PROT_WRITE` temporário) e devolve o endereço.
pub fn exec_page(fill: u8) -> Result<u64> {
    let page = page();
    let p = map_anon(page)?;
    // SAFETY: `p` aponta para `page` bytes graváveis recém-mapeados.
    unsafe { ptr::write_bytes(p.cast::<u8>(), fill, page) };
    protect(p, page, libc::PROT_READ | libc::PROT_EXEC)?;
    Ok(p as u64)
}

/// Cria 3 páginas anônimas `rw` e troca a do meio para `PROT_NONE`; devolve o
/// início das 3.
///
/// A página `PROT_NONE` é ilegível para `process_vm_readv` (`EFAULT`) e, ao contrário
/// de um `munmap`, não é reaproveitada por `mmap`s posteriores.
pub fn guarded_pages() -> Result<u64> {
    let page = page();
    let p = map_anon(3 * page)?;
    // SAFETY: a página do meio está dentro do mapeamento de 3 páginas acima.
    let middle = unsafe { p.cast::<u8>().add(page) }.cast::<libc::c_void>();
    protect(middle, page, libc::PROT_NONE)?;
    Ok(p as u64)
}
