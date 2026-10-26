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

/// Copia `bytes` para `addr` na memória do **próprio** processo.
///
/// Só para testes: `addr` deve estar numa região `rw` alocada pelo próprio teste (ex. de
/// [`anon_rw`]) com pelo menos `bytes.len()` bytes a partir de `addr`.
pub fn write_local(addr: u64, bytes: &[u8]) -> Result<()> {
    // SAFETY: pelo contrato acima, `addr` aponta para uma região `rw` do próprio processo com
    // espaço para `bytes.len()` bytes; origem (slice do chamador) e destino (região do mapeamento
    // do teste) não se sobrepõem.
    unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), addr as *mut u8, bytes.len()) };
    Ok(())
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

/// Cria `pages` páginas anônimas preenchidas com `0x90` (`nop`), copia `code` no
/// início e as deixa `PROT_READ | PROT_EXEC`; devolve o endereço.
///
/// [`io::ErrorKind::InvalidInput`] se `code` não couber nas páginas.
pub fn exec_code(code: &[u8], pages: usize) -> Result<u64> {
    let len = pages * page();
    if code.len() > len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "código maior que as páginas pedidas",
        )
        .into());
    }
    let p = map_anon(len)?;
    // SAFETY: `p` aponta para `len` bytes graváveis recém-mapeados e `code.len() <= len`;
    // origem e destino não se sobrepõem (o destino acabou de ser criado).
    unsafe {
        ptr::write_bytes(p.cast::<u8>(), 0x90, len);
        ptr::copy_nonoverlapping(code.as_ptr(), p.cast::<u8>(), code.len());
    }
    protect(p, len, libc::PROT_READ | libc::PROT_EXEC)?;
    Ok(p as u64)
}

/// Imediato inicial do laço de valor ([`value_code`]).
pub const VALUE_INITIAL: u32 = 0x1111_1111;

/// Laço que escreve um valor numa palavra: `mov eax, imm32` (`B8 xx xx xx xx`, imediato
/// no deslocamento 1), `mov [abs], eax` (`A3` + endereço de 64 bits de `word`) e
/// `jmp` para o início. O imediato inicial é [`VALUE_INITIAL`].
///
/// O trecho de 5 bytes `B8 imm32` é uma instrução inteira: trocar o imediato não muda as
/// fronteiras de instrução, como numa troca de valor de um cheat.
pub fn value_code(word: u64) -> Vec<u8> {
    let mut code = vec![0xB8];
    code.extend_from_slice(&VALUE_INITIAL.to_le_bytes());
    code.push(0xA3);
    code.extend_from_slice(&word.to_le_bytes());
    // 5 + 9 + 2 = 16 bytes; o salto curto volta 16 bytes (do fim do `jmp` ao início).
    code.extend_from_slice(&[0xEB, 0xF0]);
    code
}

/// Cria uma thread que salta para `addr` e nunca volta.
///
/// Só para testes: `addr` deve ser código do próprio teste (ex. de [`exec_code`]) que
/// nunca retorna e só usa a zona vermelha da pilha (sem `push`/`call`).
pub fn spawn_thread_at(addr: u64) -> Result<()> {
    // SAFETY: o contrato acima (código do próprio teste, executável, que nunca retorna
    // e só usa a zona vermelha da pilha) é o que `extern "C" fn()` exige; a thread
    // nunca chega ao fim da função.
    let entry: extern "C" fn() =
        unsafe { std::mem::transmute::<usize, extern "C" fn()>(addr as usize) };
    // A thread fica solta: o processo-alvo de teste termina com ela ainda rodando.
    std::thread::Builder::new().spawn(move || entry())?;
    Ok(())
}

/// Cria um pipe e devolve o fd de **leitura**; a ponta de escrita fica aberta e
/// vazada, então ler dele bloqueia para sempre sem gastar CPU.
pub fn pipe_never_written() -> Result<i32> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` tem espaço para os 2 descritores que `pipe` escreve.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(fds[0])
}

/// Cria uma página `MAP_SHARED` somente leitura de um arquivo temporário preenchido
/// com `fill` e devolve o endereço.
///
/// O arquivo é aberto com `O_RDONLY`: o mapeamento fica sem `VM_MAYWRITE`, então a
/// leitura por outro processo funciona, mas a escrita forçada por `/proc/<pid>/mem`
/// falha (com `O_RDWR` ela passaria e alteraria o arquivo). O arquivo é apagado logo
/// depois de aberto; a página continua válida.
pub fn readonly_shared_page(fill: u8) -> Result<u64> {
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::sync::atomic::{AtomicU32, Ordering};

    static NEXT: AtomicU32 = AtomicU32::new(0);
    let page = page();
    let path = std::env::temp_dir().join(format!(
        "gt-ro-shared-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(&vec![fill; page])?;
    }
    let opened = std::fs::File::open(&path);
    let removed = std::fs::remove_file(&path);
    let file = opened?;
    removed?;
    // SAFETY: mapeamento novo (endereço nulo: o kernel escolhe uma região livre) de
    // `page` bytes de um arquivo que tem exatamente `page` bytes; nenhuma memória
    // existente é afetada. O mapeamento sobrevive ao fechamento do `fd`.
    let p = unsafe {
        libc::mmap(
            ptr::null_mut(),
            page,
            libc::PROT_READ,
            libc::MAP_SHARED,
            file.as_raw_fd(),
            0,
        )
    };
    if p == libc::MAP_FAILED {
        Err(io::Error::last_os_error().into())
    } else {
        Ok(p as u64)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::mem::{MemReader, ReadMem};

    #[test]
    fn write_local_roundtrips_through_read_many_on_self() {
        let page = page();
        let base = anon_rw(2 * page).unwrap();
        write_local(base, &[1, 2, 3, 4]).unwrap();
        write_local(base + page as u64, &[9, 8]).unwrap();
        let reader = MemReader::new(std::process::id() as i32);
        let got = reader
            .read_many(&[(base, 4), (base + page as u64, 2), (base + 2, 0)])
            .unwrap();
        assert_eq!(
            got,
            vec![Some(vec![1, 2, 3, 4]), Some(vec![9, 8]), Some(vec![])]
        );
    }

    #[test]
    fn read_many_guard_page_is_none_and_batch_continues() {
        let page = page();
        let base = guarded_pages().unwrap();
        write_local(base, &[7; 4]).unwrap();
        write_local(base + 2 * page as u64, &[5; 4]).unwrap();
        let reader = MemReader::new(std::process::id() as i32);
        let got = reader
            .read_many(&[
                (base, 4),
                (base + page as u64, 4),
                (base + 2 * page as u64, 4),
                // Atravessa a guarda: curto, vira None.
                (base + page as u64 - 2, 4),
                (u64::MAX - 1, 4),
                (base, 4),
            ])
            .unwrap();
        assert_eq!(
            got,
            vec![
                Some(vec![7; 4]),
                None,
                Some(vec![5; 4]),
                None,
                None,
                Some(vec![7; 4]),
            ]
        );
    }
}
