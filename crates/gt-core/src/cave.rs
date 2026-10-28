//! Ferramentas de code cave: sequências de `CC`, alocador, desvio e relocação (spec 008, 6.2 e 6.4).

use std::ops::Range;

use iced_x86::{
    BlockEncoder, BlockEncoderOptions, Decoder, DecoderOptions, FlowControl, InstructionBlock,
    Mnemonic, OpKind,
};

use crate::error::{Error, Result};
use crate::patch::CAVE_FILL;

/// Bytes `CC` que ficam intactos no começo de cada sequência (o fim da função anterior).
pub const CAVE_MARGIN: usize = 16;

/// Tamanho de `jmp rel32` em bytes.
const JMP_REL32_LEN: usize = 5;

/// Opcode de `jmp rel32`.
const JMP_REL32: u8 = 0xE9;

/// `nop` de 1 byte.
const NOP: u8 = 0x90;

/// Tamanho máximo de uma instrução x86 em bytes.
const MAX_INSN_LEN: usize = 15;

fn bad(reason: impl Into<String>) -> Error {
    Error::BadCave {
        reason: reason.into(),
    }
}

/// Sequências de pelo menos `min_len` bytes `0xCC` em `bytes`, que começam no endereço `base`.
///
/// Devolve os intervalos de endereços (`[início, fim)`) em ordem crescente.
pub fn cc_runs(bytes: &[u8], base: u64, min_len: usize) -> Vec<Range<u64>> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    // Um byte que não é `CC` depois do fim fecha a última sequência.
    for (i, &b) in bytes.iter().chain(std::iter::once(&0u8)).enumerate() {
        match (b == CAVE_FILL && i < bytes.len(), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                if i - s >= min_len.max(1) {
                    out.push(base + s as u64..base + i as u64);
                }
                start = None;
            }
            _ => {}
        }
    }
    out
}

/// Alocador de caves nas sequências de `CC` (spec 008, 6.4).
///
/// Cada sequência guarda [`CAVE_MARGIN`] bytes intactos no começo; as caves seguintes ficam
/// encostadas umas nas outras, sem sobreposição. Uma sequência serve se `tamanho ≥ cave + 16`.
#[derive(Debug, Clone)]
pub struct CaveAllocator {
    /// Para cada sequência: o próximo endereço livre e o fim dela.
    slots: Vec<(u64, u64)>,
}

impl CaveAllocator {
    /// Alocador sobre as sequências `runs`.
    pub fn new(runs: Vec<Range<u64>>) -> Self {
        let slots = runs
            .into_iter()
            .filter_map(|r| Some((r.start.checked_add(CAVE_MARGIN as u64)?, r.end)))
            .collect();
        Self { slots }
    }

    /// Tira `range` do que o alocador ainda oferece (caves que já estão escritas, de uma conexão
    /// anterior ou deixadas no jogo): o trecho livre que ele corta se divide em dois, e o que
    /// sobra antes e depois continua alocável. Reservar de novo o mesmo trecho não muda nada.
    pub fn reserve(&mut self, range: Range<u64>) {
        if range.start >= range.end {
            return;
        }
        let mut out = Vec::with_capacity(self.slots.len() + 1);
        for (next, end) in self.slots.drain(..) {
            if range.end <= next || range.start >= end {
                out.push((next, end));
                continue;
            }
            if range.start > next {
                out.push((next, range.start));
            }
            if range.end < end {
                out.push((range.end, end));
            }
        }
        self.slots = out;
    }

    /// Reserva `len` bytes para uma cave e devolve o endereço dela.
    ///
    /// Usa a primeira sequência em que a cave cabe. `None` se `len` é 0 ou se nenhuma serve.
    pub fn alloc(&mut self, len: usize) -> Option<u64> {
        if len == 0 {
            return None;
        }
        let len = u64::try_from(len).ok()?;
        self.slots.iter_mut().find_map(|(next, end)| {
            let stop = next.checked_add(len)?;
            if stop > *end {
                return None;
            }
            let at = *next;
            *next = stop;
            Some(at)
        })
    }
}

/// Bytes do desvio `jmp rel32` para `target`, em `at`, completados com `NOP` até `insn_len`.
///
/// Erro se a instrução trocada tem menos de 5 bytes (ou mais que 15) ou se a distância não cabe
/// num `i32`.
pub fn hook_bytes(at: u64, insn_len: usize, target: u64) -> Result<Vec<u8>> {
    if !(JMP_REL32_LEN..=MAX_INSN_LEN).contains(&insn_len) {
        return Err(bad(format!(
            "a instrução do desvio tem {insn_len} bytes (precisa de 5 a 15)"
        )));
    }
    let next = at.wrapping_add(JMP_REL32_LEN as u64);
    let rel = i32::try_from(target.wrapping_sub(next) as i64).map_err(|_| {
        bad(format!(
            "o destino {target:#x} fica longe demais de {at:#x} para um jmp rel32"
        ))
    })?;
    let mut out = Vec::with_capacity(insn_len);
    out.push(JMP_REL32);
    out.extend(rel.to_le_bytes());
    out.resize(insn_len, NOP);
    Ok(out)
}

/// Reloca **uma** instrução de `from` para `to`.
///
/// `insn` tem de ser exatamente uma instrução inteira. Operandos relativos ao RIP (e desvios
/// relativos) são recalculados pelo `BlockEncoder` do `iced-x86` para manter o mesmo alvo; as
/// demais instruções saem iguais. Erro se o alvo ficar a mais de 2 GiB de `to`.
pub fn relocate(insn: &[u8], from: u64, to: u64) -> Result<Vec<u8>> {
    let mut decoder = Decoder::with_ip(64, insn, from, DecoderOptions::NONE);
    let decoded = decoder.decode();
    if decoded.is_invalid() || decoded.len() != insn.len() {
        return Err(bad(format!(
            "os bytes em {from:#x} não são exatamente uma instrução inteira"
        )));
    }
    let block = InstructionBlock::new(std::slice::from_ref(&decoded), to);
    let encoded = BlockEncoder::encode(64, block, BlockEncoderOptions::NONE)
        .map_err(|e| bad(format!("relocação de {from:#x} para {to:#x}: {e}")))?;
    Ok(encoded.code_buffer)
}

/// Desvio relativo direto (destino conhecido).
fn is_near_branch(kind: OpKind) -> bool {
    matches!(
        kind,
        OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64
    )
}

/// Confere o código de uma cave em `at` (spec 008, 6.2).
///
/// Recusa: código vazio, instrução inválida ou cortada, `call`, `ret`, `int3`/`int n`, desvios
/// indiretos e desvios diretos para fora da cave (a não ser para `back_to`). A última instrução
/// tem de ser `jmp back_to`.
pub fn check_cave_code(code: &[u8], at: u64, back_to: u64) -> Result<()> {
    let end = at
        .checked_add(code.len() as u64)
        .ok_or_else(|| bad("a cave passa do fim do espaço de endereços"))?;
    let mut decoder = Decoder::with_ip(64, code, at, DecoderOptions::NONE);
    let mut last = None;
    for insn in &mut decoder {
        let ip = insn.ip();
        if insn.is_invalid() {
            return Err(bad(format!("instrução inválida ou cortada em {ip:#x}")));
        }
        match insn.flow_control() {
            FlowControl::Call | FlowControl::IndirectCall => {
                return Err(bad(format!("call em {ip:#x}: a cave não chama nada")));
            }
            FlowControl::Return => return Err(bad(format!("ret em {ip:#x}"))),
            FlowControl::Interrupt | FlowControl::Exception => {
                return Err(bad(format!("int3 ou interrupção em {ip:#x}")));
            }
            FlowControl::IndirectBranch | FlowControl::XbeginXabortXend => {
                return Err(bad(format!("desvio indireto em {ip:#x}")));
            }
            FlowControl::UnconditionalBranch | FlowControl::ConditionalBranch => {
                if !is_near_branch(insn.op0_kind()) {
                    return Err(bad(format!("desvio sem destino direto em {ip:#x}")));
                }
                let t = insn.near_branch_target();
                if t != back_to && !(at..end).contains(&t) {
                    return Err(bad(format!("desvio em {ip:#x} para {t:#x}, fora da cave")));
                }
            }
            FlowControl::Next => {}
        }
        last = Some(insn);
    }
    let Some(last) = last else {
        return Err(bad("a cave está vazia"));
    };
    let ends_in_jmp_back = last.mnemonic() == Mnemonic::Jmp
        && is_near_branch(last.op0_kind())
        && last.near_branch_target() == back_to;
    if !ends_in_jmp_back {
        return Err(bad(format!("a cave não termina em jmp {back_to:#x}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_x86::{Decoder, DecoderOptions};

    fn decode_one(bytes: &[u8], ip: u64) -> iced_x86::Instruction {
        Decoder::with_ip(64, bytes, ip, DecoderOptions::NONE).decode()
    }

    #[test]
    fn reserve_cuts_what_is_already_taken() {
        let mut a = CaveAllocator::new(vec![0x1000..0x1200, 0x2000..0x2040]);
        // Trecho no começo da sequência (depois da margem): a alocação segue dali.
        a.reserve(0x1010..0x1050);
        assert_eq!(a.alloc(0x10), Some(0x1050));
        // Trecho no meio: o que sobra antes continua alocável, e o depois também.
        a.reserve(0x1100..0x1180);
        assert_eq!(a.alloc(0x80), Some(0x1060), "cabe entre 0x1060 e 0x1100");
        assert_eq!(a.alloc(0x80), Some(0x1180));
        // Reservar de novo (ou fora de qualquer sequência) não muda nada.
        a.reserve(0x1100..0x1180);
        a.reserve(0x9000..0x9100);
        a.reserve(0x1180..0x1180);
        // O que restou antes do trecho reservado ainda serve; depois, a segunda sequência.
        assert_eq!(a.alloc(0x10), Some(0x10e0));
        assert_eq!(a.alloc(0x20), Some(0x2010));
        assert_eq!(a.alloc(0x20), None);
    }

    #[test]
    fn cc_runs_finds_min_len() {
        // 3 CC (curta), código, 5 CC, código, 6 CC no fim.
        let mut b = vec![0xCC, 0xCC, 0xCC, 0x90];
        b.extend([0xCC; 5]);
        b.push(0xC3);
        b.extend([0xCC; 6]);
        assert_eq!(cc_runs(&b, 0x1000, 5), vec![0x1004..0x1009, 0x100A..0x1010]);
        assert_eq!(cc_runs(&b, 0x1000, 6), vec![0x100A..0x1010]);
        assert_eq!(cc_runs(&b, 0x1000, 3).len(), 3);
        assert!(cc_runs(&[0x90, 0x90], 0, 1).is_empty());
    }

    #[test]
    fn alloc_respects_margin_and_packs() {
        let run = 0x1_4000_0000..0x1_4000_0000 + 1871;
        let mut a = CaveAllocator::new(vec![run.clone()]);
        let x = a.alloc(150);
        let y = a.alloc(50);
        let z = a.alloc(60);
        assert_eq!(x, Some(run.start + 16));
        assert_eq!(y, Some(run.start + 16 + 150));
        assert_eq!(z, Some(run.start + 16 + 200));

        // 1856 + 16 = 1872 > 1871: não cabe; 1855 cabe exatamente.
        assert_eq!(CaveAllocator::new(vec![run.clone()]).alloc(1856), None);
        assert_eq!(
            CaveAllocator::new(vec![run.clone()]).alloc(1855),
            Some(run.start + 16)
        );

        // A segunda cave não cabe no resto da primeira sequência e vai para a seguinte.
        let small = 0x2000..0x2000 + 50;
        let big = 0x3000..0x3000 + 200;
        let mut b = CaveAllocator::new(vec![small, big]);
        assert_eq!(b.alloc(30), Some(0x2000 + 16));
        assert_eq!(b.alloc(30), Some(0x3000 + 16));
        assert_eq!(b.alloc(0), None);
    }

    #[test]
    fn hook_bytes_jmp_and_nops() {
        let at = 0x1_457B_D6D0u64;
        let target = 0x1_4000_1000u64;
        let b = hook_bytes(at, 7, target).unwrap();
        assert_eq!(b.len(), 7);
        assert_eq!(b[0], 0xE9);
        let rel = i32::from_le_bytes([b[1], b[2], b[3], b[4]]);
        assert_eq!((at as i64 + 5 + rel as i64) as u64, target);
        assert_eq!(&b[5..], &[0x90, 0x90]);
        assert_eq!(hook_bytes(at, 5, target).unwrap().len(), 5);
    }

    #[test]
    fn hook_bytes_rejects_short() {
        assert!(hook_bytes(0x1000, 4, 0x2000).is_err());
        // Distância fora de `i32`.
        assert!(hook_bytes(0x1000, 5, 0x1000 + (1u64 << 32)).is_err());
    }

    #[test]
    fn relocate_rip_relative() {
        // movss xmm1, [rip+0x100] em 0x140001000 (8 bytes): alvo 0x140001108.
        let from = 0x1_4000_1000u64;
        let insn = [0xF3, 0x0F, 0x10, 0x0D, 0x00, 0x01, 0x00, 0x00];
        let target = decode_one(&insn, from).ip_rel_memory_address();
        assert_eq!(target, 0x1_4000_1108);
        let to = 0x1_4080_0000u64;
        let moved = relocate(&insn, from, to).unwrap();
        let d = decode_one(&moved, to);
        assert_eq!(d.len(), moved.len());
        assert!(d.is_ip_rel_memory_operand());
        assert_eq!(d.ip_rel_memory_address(), target);
        assert_ne!(moved, insn);

        // Alvo a mais de 2 GiB do destino: erro.
        assert!(relocate(&insn, from, from + (3u64 << 30)).is_err());
    }

    #[test]
    fn relocate_plain_unchanged() {
        let insn = [0x48, 0x89, 0x5C, 0x24, 0x08]; // mov [rsp+8], rbx
        assert_eq!(relocate(&insn, 0x1000, 0x9_0000).unwrap(), insn);
        let movss = [0xF3, 0x0F, 0x10, 0x89, 0x74, 0x09, 0x00, 0x00]; // movss xmm1, [rcx+0x974]
        assert_eq!(relocate(&movss, 0x1000, 0x9_0000).unwrap(), movss);
        // Mais de uma instrução, ou instrução incompleta: erro.
        assert!(relocate(&[0x90, 0x90], 0x1000, 0x2000).is_err());
        assert!(relocate(&[0x48, 0x89], 0x1000, 0x2000).is_err());
    }

    /// `prefix` em `at`, seguido de `jmp back_to`.
    fn with_jmp_back(prefix: &[u8], at: u64, back_to: u64) -> Vec<u8> {
        let mut code = prefix.to_vec();
        code.extend(hook_bytes(at + prefix.len() as u64, 5, back_to).unwrap());
        code
    }

    #[test]
    fn check_rejects_call_ret_int3() {
        let at = 0x1_4000_2000u64;
        let back = 0x1_457B_D6D8u64;
        // nop; movss xmm1, [rcx+0x974]; jmp back
        let ok = with_jmp_back(
            &[0x90, 0xF3, 0x0F, 0x10, 0x89, 0x74, 0x09, 0x00, 0x00],
            at,
            back,
        );
        assert!(check_cave_code(&ok, at, back).is_ok());

        // call rel32 (para qualquer lugar)
        let call = with_jmp_back(&[0xE8, 0x00, 0x00, 0x00, 0x00], at, back);
        assert!(check_cave_code(&call, at, back).is_err());
        // call rax
        assert!(check_cave_code(&with_jmp_back(&[0xFF, 0xD0], at, back), at, back).is_err());
        // ret no meio
        assert!(check_cave_code(&with_jmp_back(&[0xC3], at, back), at, back).is_err());
        // int3 como instrução
        assert!(check_cave_code(&with_jmp_back(&[0xCC], at, back), at, back).is_err());
        // Termina num jmp para outro lugar, ou não termina em jmp.
        assert!(check_cave_code(&ok, at, back + 1).is_err());
        assert!(check_cave_code(&[0x90, 0x90], at, back).is_err());
        assert!(check_cave_code(&[], at, back).is_err());
        // Instrução cortada no fim.
        let mut cut = ok.clone();
        cut.push(0x48);
        assert!(check_cave_code(&cut, at, back).is_err());
    }

    #[test]
    fn check_accepts_internal_branches_only() {
        let at = 0x1_4000_2000u64;
        let back = 0x1_457B_D6D8u64;
        // je +0 (para a instrução seguinte, dentro da cave); jmp back
        let ok = with_jmp_back(&[0x74, 0x00], at, back);
        assert!(check_cave_code(&ok, at, back).is_ok());
        // je para fora da cave (nem dentro, nem back_to)
        let out = with_jmp_back(&[0x74, 0x40], at, back);
        assert!(check_cave_code(&out, at, back).is_err());
        // jmp rax (indireto)
        let ind = with_jmp_back(&[0xFF, 0xE0], at, back);
        assert!(check_cave_code(&ind, at, back).is_err());
    }
}
