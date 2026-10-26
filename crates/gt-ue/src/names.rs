//! `FNamePool`: decodificação das entradas e consulta por índice.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use gt_core::mem::ReadMem;

use crate::error::UeError;
use crate::read::{is_user_addr, read_bytes, read_vec, u64_at};

/// Teto de blocos do pool.
const MAX_BLOCKS: usize = 8192;
/// Ponteiros de bloco lidos por vez.
const BLOCK_PTRS_PER_READ: usize = 64;
/// Teto do tamanho de um nome, em bytes.
const MAX_NAME_BYTES: usize = 1024;

/// Tamanho em bytes do texto de uma entrada, a partir do cabeçalho.
fn entry_bytes(header: u16) -> usize {
    let len = usize::from(header >> 6);
    if header & 1 == 1 { len * 2 } else { len }
}

/// Decodifica uma entrada do pool: `header >> 6` é o tamanho em caracteres, o bit 0 marca UTF-16LE
/// (senão é latin-1). `raw` traz os bytes do texto, sem o cabeçalho.
pub fn decode_entry(header: u16, raw: &[u8]) -> Result<String, UeError> {
    let nbytes = entry_bytes(header);
    if nbytes > MAX_NAME_BYTES {
        return Err(UeError::Malformed(format!(
            "nome com {nbytes} bytes (máximo {MAX_NAME_BYTES})"
        )));
    }
    let Some(text) = raw.get(..nbytes) else {
        return Err(UeError::Malformed(format!(
            "entrada de nome truncada: {} de {nbytes} bytes",
            raw.len()
        )));
    };
    if header & 1 == 1 {
        let units: Vec<u16> = text
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| u16::from_le_bytes(*p))
            .collect();
        String::from_utf16(&units)
            .map_err(|_| UeError::Malformed("nome UTF-16 inválido".to_string()))
    } else {
        Ok(text.iter().map(|&b| char::from(b)).collect())
    }
}

/// Consulta de nomes por índice, com cache (os nomes não mudam com o jogo rodando).
pub struct Names<'m, M: ReadMem> {
    mem: &'m M,
    /// Ponteiros dos blocos já lidos, na ordem.
    blocks: Vec<u64>,
    cache: Mutex<HashMap<u32, String>>,
}

impl<'m, M: ReadMem> Names<'m, M> {
    /// `pool_blocks` é o endereço do ponteiro do bloco 0; os seguintes vêm a cada 8 bytes. A leitura
    /// para no primeiro ponteiro nulo ou não canônico (os blocos são alocados em ordem).
    pub fn new(mem: &'m M, pool_blocks: u64) -> Result<Self, UeError> {
        let mut blocks = Vec::new();
        'read: for round in 0..MAX_BLOCKS / BLOCK_PTRS_PER_READ {
            let addr = pool_blocks + (round * BLOCK_PTRS_PER_READ * 8) as u64;
            let mut buf = [0u8; BLOCK_PTRS_PER_READ * 8];
            match read_bytes(mem, addr, &mut buf) {
                Ok(()) => {}
                // Depois da primeira leitura, um trecho ilegível só encerra a lista.
                Err(UeError::Read { .. }) if round > 0 => break,
                Err(e) => return Err(e),
            }
            for k in 0..BLOCK_PTRS_PER_READ {
                let ptr = u64_at(&buf, k * 8);
                if !is_user_addr(ptr) {
                    break 'read;
                }
                blocks.push(ptr);
            }
        }
        if blocks.is_empty() {
            return Err(UeError::Malformed(
                "o pool de nomes não tem nenhum bloco".to_string(),
            ));
        }
        Ok(Self {
            mem,
            blocks,
            cache: Mutex::new(HashMap::new()),
        })
    }

    /// Quantidade de blocos lidos.
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Nome do índice de comparação `index` (bloco `index >> 16`, deslocamento `(index & 0xffff) * 2`).
    pub fn name(&self, index: u32) -> Result<String, UeError> {
        if let Some(hit) = self
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&index)
        {
            return Ok(hit.clone());
        }
        let block = (index >> 16) as usize;
        let Some(&base) = self.blocks.get(block) else {
            return Err(UeError::Malformed(format!(
                "nome {index:#x}: o bloco {block} não existe"
            )));
        };
        let entry = base + u64::from(index & 0xffff) * 2;
        let mut head = [0u8; 2];
        read_bytes(self.mem, entry, &mut head)?;
        let header = u16::from_le_bytes(head);
        let nbytes = entry_bytes(header);
        if nbytes == 0 {
            return Err(UeError::Malformed(format!("nome {index:#x} vazio")));
        }
        if nbytes > MAX_NAME_BYTES {
            return Err(UeError::Malformed(format!(
                "nome {index:#x} com {nbytes} bytes (máximo {MAX_NAME_BYTES})"
            )));
        }
        let raw = read_vec(self.mem, entry + 2, nbytes)?;
        let text = decode_entry(header, &raw)?;
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(index, text.clone());
        Ok(text)
    }

    /// Nome de um `FName` (`ComparisonIndex`, `Number`): com `number > 0` acrescenta `_{number-1}`.
    pub fn fname(&self, comparison: u32, number: u32) -> Result<String, UeError> {
        let base = self.name(comparison)?;
        Ok(if number > 0 {
            format!("{base}_{}", number - 1)
        } else {
            base
        })
    }
}
