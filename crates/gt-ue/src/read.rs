//! Leituras com os limites do leitor: só endereços canônicos de usuário.

use gt_core::Error as CoreError;
use gt_core::mem::ReadMem;

use crate::error::UeError;

/// Menor endereço aceito numa leitura de ponteiro.
pub const MIN_USER_ADDR: u64 = 0x1_0000;
/// Primeiro endereço fora do espaço canônico de usuário.
pub const MAX_USER_ADDR: u64 = 0x8000_0000_0000;

/// `true` se `addr` é um endereço canônico de usuário plausível (`0x10000 ≤ a < 0x8000_0000_0000`).
pub fn is_user_addr(addr: u64) -> bool {
    (MIN_USER_ADDR..MAX_USER_ADDR).contains(&addr)
}

/// Lê `buf.len()` bytes em `addr`. Endereço fora do espaço de usuário ou memória não mapeada
/// viram [`UeError::Read`]; os demais erros do núcleo passam como [`UeError::Core`].
pub fn read_bytes<M: ReadMem + ?Sized>(mem: &M, addr: u64, buf: &mut [u8]) -> Result<(), UeError> {
    let end = addr.checked_add(buf.len() as u64);
    if !is_user_addr(addr) || end.is_none_or(|e| e > MAX_USER_ADDR) {
        return Err(UeError::Read { addr });
    }
    match mem.read_exact(addr, buf) {
        Ok(()) => Ok(()),
        Err(CoreError::Unmapped { .. }) => Err(UeError::Read { addr }),
        Err(e) => Err(UeError::Core(e)),
    }
}

/// Lê `len` bytes em `addr`.
pub fn read_vec<M: ReadMem + ?Sized>(mem: &M, addr: u64, len: usize) -> Result<Vec<u8>, UeError> {
    let mut buf = vec![0u8; len];
    read_bytes(mem, addr, &mut buf)?;
    Ok(buf)
}

/// Lê um byte em `addr`.
pub fn read_u8<M: ReadMem + ?Sized>(mem: &M, addr: u64) -> Result<u8, UeError> {
    let mut buf = [0u8; 1];
    read_bytes(mem, addr, &mut buf)?;
    Ok(buf[0])
}

/// Lê um `u32` little-endian em `addr`.
pub fn read_u32<M: ReadMem + ?Sized>(mem: &M, addr: u64) -> Result<u32, UeError> {
    let mut buf = [0u8; 4];
    read_bytes(mem, addr, &mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

/// Lê um `u64` little-endian em `addr` (um ponteiro, que pode ser nulo).
pub fn read_u64<M: ReadMem + ?Sized>(mem: &M, addr: u64) -> Result<u64, UeError> {
    let mut buf = [0u8; 8];
    read_bytes(mem, addr, &mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

/// `u32` little-endian em `buf[off..]`; `0` se o trecho não cabe.
pub fn u32_at(buf: &[u8], off: usize) -> u32 {
    buf.get(off..off + 4)
        .and_then(|s| <[u8; 4]>::try_from(s).ok())
        .map_or(0, u32::from_le_bytes)
}

/// `u64` little-endian em `buf[off..]`; `0` se o trecho não cabe.
pub fn u64_at(buf: &[u8], off: usize) -> u64 {
    buf.get(off..off + 8)
        .and_then(|s| <[u8; 8]>::try_from(s).ok())
        .map_or(0, u64::from_le_bytes)
}
