//! `GUObjectArray`: leitura do cabeçalho, consulta por índice e varredura em lotes.

use std::time::{Duration, Instant};

use gt_core::mem::ReadMem;

use crate::error::UeError;
use crate::layout::UeLayout;
use crate::read::{is_user_addr, read_bytes, read_u32, read_u64, u32_at, u64_at};

/// Teto de blocos aceitos no cabeçalho (o jogo usa dezenas).
const MAX_CHUNKS_SANE: u32 = 4096;

/// Limites de uma varredura da tabela: blocos pequenos e pausas, para não pesar no jogo.
#[derive(Debug, Clone, Copy)]
pub struct ScanBudget {
    /// Bytes da tabela de itens lidos por vez.
    pub block_bytes: usize,
    /// Cabeçalhos de objeto lidos por chamada em lote.
    pub batch: usize,
    /// Pausa depois de cada bloco e de cada lote.
    pub pause: Duration,
    /// Teto de tempo da varredura inteira.
    pub deadline: Duration,
    /// Quem dorme durante as pausas (nos testes, uma função que não dorme).
    pub sleep: fn(Duration),
}

impl ScanBudget {
    /// Valores da spec: blocos de 1 MiB, lotes de 1024, 1 ms de pausa, teto de 60 s.
    pub fn standard() -> Self {
        Self {
            block_bytes: 1 << 20,
            batch: 1024,
            pause: Duration::from_millis(1),
            deadline: Duration::from_secs(60),
            sleep: std::thread::sleep,
        }
    }
}

/// Cabeçalho de um objeto, como lido pela varredura.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjHeader {
    /// Índice do item na tabela.
    pub index: u32,
    /// Endereço do objeto.
    pub addr: u64,
    pub flags: u32,
    /// `InternalIndex` gravado no próprio objeto.
    pub internal_index: u32,
    /// `ClassPrivate`.
    pub class: u64,
    /// `NamePrivate`: (`ComparisonIndex`, `Number`).
    pub name: (u32, u32),
    /// `OuterPrivate`.
    pub outer: u64,
}

/// Como a varredura terminou.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanEnd {
    /// Percorreu até o `NumElements` lido no início.
    Done,
    /// O teto de tempo estourou antes do fim.
    DeadlineExceeded,
}

/// Faixa do cabeçalho do `UObject` que a varredura lê de cada objeto.
struct HeaderSpan {
    start: u32,
    len: usize,
}

impl HeaderSpan {
    fn new(layout: &UeLayout) -> Self {
        let u = &layout.uobject;
        let start = u.flags.min(u.index).min(u.class).min(u.name).min(u.outer);
        let end = (u.flags + 4)
            .max(u.index + 4)
            .max(u.class + 8)
            .max(u.name + 8)
            .max(u.outer + 8);
        Self {
            start,
            len: (end - start) as usize,
        }
    }
}

/// Cabeçalho do `GUObjectArray` com a lista de blocos já lida.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectArray {
    /// Ponteiros dos blocos de itens (`num_chunks` entradas).
    pub chunks: Vec<u64>,
    pub num_elements: u32,
    pub max_elements: u32,
    pub num_chunks: u32,
    pub max_chunks: u32,
}

impl ObjectArray {
    /// Lê o cabeçalho em `gobjects` e a lista de blocos.
    pub fn read<M: ReadMem + ?Sized>(
        mem: &M,
        layout: &UeLayout,
        gobjects: u64,
    ) -> Result<Self, UeError> {
        let o = &layout.objects;
        let list = read_u64(mem, gobjects + u64::from(o.chunks))?;
        let max_elements = read_u32(mem, gobjects + u64::from(o.max_elements))?;
        let num_elements = read_u32(mem, gobjects + u64::from(o.num_elements))?;
        let max_chunks = read_u32(mem, gobjects + u64::from(o.max_chunks))?;
        let num_chunks = read_u32(mem, gobjects + u64::from(o.num_chunks))?;
        if num_chunks > MAX_CHUNKS_SANE {
            return Err(UeError::Malformed(format!(
                "NumChunks = {num_chunks} passa do teto de {MAX_CHUNKS_SANE}"
            )));
        }
        let mut chunks = Vec::new();
        if num_chunks > 0 {
            if !is_user_addr(list) {
                return Err(UeError::Malformed(format!(
                    "lista de blocos em {list:#x} fora do espaço de usuário"
                )));
            }
            let mut buf = vec![0u8; num_chunks as usize * 8];
            read_bytes(mem, list, &mut buf)?;
            chunks = (0..num_chunks as usize)
                .map(|i| u64_at(&buf, i * 8))
                .collect();
        }
        Ok(Self {
            chunks,
            num_elements,
            max_elements,
            num_chunks,
            max_chunks,
        })
    }

    /// Endereço do item `index`, se o bloco dele existe.
    fn item_addr(&self, layout: &UeLayout, index: u64) -> Option<u64> {
        let per = u64::from(layout.objects.items_per_chunk);
        if per == 0 {
            return None;
        }
        let chunk = *self.chunks.get(usize::try_from(index / per).ok()?)?;
        if !is_user_addr(chunk) {
            return None;
        }
        Some(chunk + (index % per) * u64::from(layout.objects.item_size))
    }

    /// Objeto do item `index`. `None` para item vazio, índice fora de `NumElements` ou bloco ausente.
    pub fn object_at<M: ReadMem + ?Sized>(
        &self,
        mem: &M,
        layout: &UeLayout,
        index: u32,
    ) -> Result<Option<u64>, UeError> {
        if index >= self.num_elements {
            return Ok(None);
        }
        let Some(item) = self.item_addr(layout, u64::from(index)) else {
            return Ok(None);
        };
        let obj = read_u64(mem, item + u64::from(layout.objects.item_object))?;
        Ok(is_user_addr(obj).then_some(obj))
    }

    /// Percorre os itens de `0` a `NumElements` (o lido no início) e chama `visit` para cada objeto
    /// não nulo e legível, em ordem de índice. Lê a tabela em blocos de `budget.block_bytes` e os
    /// cabeçalhos em lotes de `budget.batch`, com `budget.pause` entre uns e outros.
    pub fn scan<M: ReadMem + ?Sized>(
        &self,
        mem: &M,
        layout: &UeLayout,
        budget: &ScanBudget,
        mut visit: impl FnMut(ObjHeader),
    ) -> Result<ScanEnd, UeError> {
        let started = Instant::now();
        let o = &layout.objects;
        let item = o.item_size as usize;
        let per_chunk = u64::from(o.items_per_chunk);
        if item == 0 || per_chunk == 0 || (o.item_object as usize) + 8 > item {
            return Err(UeError::Malformed(
                "layout da tabela de objetos incoerente".to_string(),
            ));
        }
        let items_per_block = (budget.block_bytes / item).max(1) as u64;
        let batch = budget.batch.max(1);
        let span = HeaderSpan::new(layout);
        let u = &layout.uobject;
        let rel = |off: u32| (off - span.start) as usize;
        let total = u64::from(self.num_elements);
        let mut index = 0u64;
        let mut visited = 0u64;
        while index < total {
            if started.elapsed() >= budget.deadline {
                tracing::debug!(index, total, "varredura de objetos abandonada pelo teto");
                return Ok(ScanEnd::DeadlineExceeded);
            }
            let Some(first) = self.item_addr(layout, index) else {
                return Err(UeError::Malformed(format!(
                    "o item {index} não tem bloco na lista"
                )));
            };
            let in_chunk = per_chunk - index % per_chunk;
            let count = items_per_block.min(in_chunk).min(total - index);
            let mut table = vec![0u8; count as usize * item];
            read_bytes(mem, first, &mut table)?;
            (budget.sleep)(budget.pause);

            let pending: Vec<(u32, u64)> = (0..count as usize)
                .filter_map(|k| {
                    let obj = u64_at(&table, k * item + o.item_object as usize);
                    is_user_addr(obj).then_some(((index + k as u64) as u32, obj))
                })
                .collect();
            for group in pending.chunks(batch) {
                if started.elapsed() >= budget.deadline {
                    tracing::debug!(index, total, "varredura de objetos abandonada pelo teto");
                    return Ok(ScanEnd::DeadlineExceeded);
                }
                let reqs: Vec<(u64, usize)> = group
                    .iter()
                    .map(|&(_, obj)| (obj + u64::from(span.start), span.len))
                    .collect();
                let data = mem.read_many(&reqs)?;
                for (&(idx, obj), bytes) in group.iter().zip(data) {
                    // Objeto que sumiu entre a leitura do item e a do cabeçalho: ignorado.
                    let Some(bytes) = bytes else { continue };
                    visited += 1;
                    visit(ObjHeader {
                        index: idx,
                        addr: obj,
                        flags: u32_at(&bytes, rel(u.flags)),
                        internal_index: u32_at(&bytes, rel(u.index)),
                        class: u64_at(&bytes, rel(u.class)),
                        name: (u32_at(&bytes, rel(u.name)), u32_at(&bytes, rel(u.name) + 4)),
                        outer: u64_at(&bytes, rel(u.outer)),
                    });
                }
                (budget.sleep)(budget.pause);
            }
            index += count;
        }
        tracing::debug!(visited, total, "varredura de objetos concluída");
        Ok(ScanEnd::Done)
    }
}
