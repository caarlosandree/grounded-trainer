//! Desmontagem x86-64 e destino de desvios.

use iced_x86::{
    Decoder, DecoderOptions, FlowControl, Formatter, Instruction, IntelFormatter, MemorySize,
    Mnemonic, OpKind,
};

/// Texto de uma instrução que o decodificador não reconheceu.
const INVALID_TEXT: &str = "(inválida)";

/// Tamanho máximo de uma instrução x86 em bytes.
const MAX_INSN_LEN: usize = 15;

/// Tamanho de `jmp rel32` e `call rel32` em bytes.
const REL32_BRANCH_LEN: usize = 5;

/// Quantos bytes antes do trecho `disassemble_around` tenta alinhar.
const AROUND_LOOKBACK: usize = 64;

/// Uma instrução desmontada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Insn {
    /// Endereço da instrução.
    pub addr: u64,
    /// Bytes da instrução.
    pub bytes: Vec<u8>,
    /// Texto em sintaxe Intel (ex.: `call 0x1439e6d10`).
    pub text: String,
    /// Destino de um desvio near ou endereço efetivo de um operando relativo ao RIP.
    pub target: Option<u64>,
}

/// Destino de um desvio na primeira instrução de um trecho.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Branch {
    /// `jmp rel32` / `call rel32`: o destino já está resolvido.
    Direct(u64),
    /// `jmp qword ptr [rip+x]`: o destino é lido do endereço `slot`.
    RipIndirect {
        /// Endereço do ponteiro que guarda o destino.
        slot: u64,
    },
}

fn formatter() -> IntelFormatter {
    let mut f = IntelFormatter::new();
    let o = f.options_mut();
    o.set_hex_prefix("0x");
    o.set_hex_suffix("");
    o.set_uppercase_hex(false);
    o.set_leading_zeros(false);
    o.set_branch_leading_zeros(false);
    o.set_space_after_operand_separator(true);
    f
}

fn is_near_branch(insn: &Instruction) -> bool {
    matches!(
        insn.flow_control(),
        FlowControl::Call | FlowControl::UnconditionalBranch | FlowControl::ConditionalBranch
    ) && matches!(
        insn.op0_kind(),
        OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64
    )
}

fn target_of(insn: &Instruction) -> Option<u64> {
    if insn.is_ip_rel_memory_operand() {
        Some(insn.ip_rel_memory_address())
    } else if is_near_branch(insn) {
        Some(insn.near_branch_target())
    } else {
        None
    }
}

/// Decodifica `bytes` (em `ip`) até `max` instruções ou até a posição `stop_pos`
/// (em bytes desde o início de `bytes`) ser alcançada ou ultrapassada.
fn decode(bytes: &[u8], ip: u64, max: usize, stop_pos: usize) -> Vec<Insn> {
    let mut decoder = Decoder::with_ip(64, bytes, ip, DecoderOptions::NONE);
    let mut fmt = formatter();
    let mut out = Vec::new();
    let mut insn = Instruction::default();
    while out.len() < max && decoder.can_decode() && decoder.position() < stop_pos {
        let pos = decoder.position();
        decoder.decode_out(&mut insn);
        let addr = ip.wrapping_add(pos as u64);
        if insn.is_invalid() {
            // O iced devolve a inválida com tamanho maior que 1: recomeça em +1.
            out.push(Insn {
                addr,
                bytes: vec![bytes[pos]],
                text: INVALID_TEXT.to_string(),
                target: None,
            });
            // `pos < bytes.len()` (can_decode), então `pos + 1` é uma posição válida.
            if decoder.set_position(pos + 1).is_ok() {
                decoder.set_ip(addr.wrapping_add(1));
            } else {
                break;
            }
            continue;
        }
        let mut text = String::new();
        fmt.format(&insn, &mut text);
        out.push(Insn {
            addr,
            bytes: bytes[pos..pos + insn.len()].to_vec(),
            text,
            target: target_of(&insn),
        });
    }
    out
}

/// Desmonta até `max` instruções de `bytes`, que começam no endereço `ip`.
///
/// Para ao atingir `max` ou o fim dos bytes. Uma instrução inválida vira uma
/// `Insn` de 1 byte com texto `(inválida)` e a desmontagem continua no byte seguinte.
pub fn disassemble(bytes: &[u8], ip: u64, max: usize) -> Vec<Insn> {
    decode(bytes, ip, max, usize::MAX)
}

/// Destino do desvio na **primeira** instrução de `bytes` (em `ip`), se houver.
pub fn branch_at(bytes: &[u8], ip: u64) -> Option<Branch> {
    let mut decoder = Decoder::with_ip(64, bytes, ip, DecoderOptions::NONE);
    if !decoder.can_decode() {
        return None;
    }
    let insn = decoder.decode();
    if insn.is_invalid() {
        return None;
    }
    match insn.mnemonic() {
        // Só `jmp rel32` / `call rel32` (5 bytes): `jmp rel8` e `rel16` não servem de desvio de
        // patch e o destino delas não é o que um `e9`/`e8` mostraria.
        Mnemonic::Jmp | Mnemonic::Call
            if insn.op0_kind() == OpKind::NearBranch64 && insn.len() == REL32_BRANCH_LEN =>
        {
            Some(Branch::Direct(insn.near_branch_target()))
        }
        // Só `jmp qword ptr [rip+x]`: o slot lido tem 8 bytes (nada de `jmp far`). O iced
        // reporta o operando de memória desse `jmp` como `QwordOffset`.
        Mnemonic::Jmp
            if insn.op0_kind() == OpKind::Memory
                && matches!(
                    insn.memory_size(),
                    MemorySize::QwordOffset | MemorySize::UInt64
                )
                && insn.is_ip_rel_memory_operand() =>
        {
            Some(Branch::RipIndirect {
                slot: insn.ip_rel_memory_address(),
            })
        }
        _ => None,
    }
}

/// Desmonta o trecho `[start, end)` com até `context` instruções antes e depois.
///
/// `bytes` começa em `base`. Como x86 tem instruções de tamanho variável, tenta
/// começar em `start - 64 + k` (`k` de 0 a 15) e usa o primeiro `k` cuja
/// decodificação chegue exatamente em `start` sem instrução inválida; se nenhum
/// servir, começa em `start` (sem instruções de contexto antes).
pub fn disassemble_around(
    bytes: &[u8],
    base: u64,
    start: u64,
    end: u64,
    context: usize,
) -> Vec<Insn> {
    // Um fim antes do início vira um trecho vazio no início.
    let end = end.max(start);
    let Some(start_off) = start
        .checked_sub(base)
        .and_then(|o| usize::try_from(o).ok())
        .filter(|&o| o < bytes.len())
    else {
        return Vec::new();
    };
    let end_off = end
        .saturating_sub(base)
        .try_into()
        .unwrap_or(usize::MAX)
        .max(start_off);

    let aligned_from = (0..MAX_INSN_LEN + 1)
        .map(|k| {
            (start_off + k)
                .saturating_sub(AROUND_LOOKBACK)
                .min(start_off)
        })
        .find(|&from| {
            let before = decode(
                &bytes[from..],
                base + from as u64,
                usize::MAX,
                start_off - from,
            );
            let consumed: usize = before.iter().map(|i| i.bytes.len()).sum();
            consumed == start_off - from && before.iter().all(|i| i.text != INVALID_TEXT)
        })
        .unwrap_or(start_off);

    // Decodifica até o fim do trecho mais folga para `context` instruções depois.
    let stop = end_off
        .saturating_add(context.saturating_mul(MAX_INSN_LEN))
        .saturating_sub(aligned_from);
    let all = decode(
        &bytes[aligned_from..],
        base + aligned_from as u64,
        usize::MAX,
        stop,
    );

    let first = all
        .iter()
        .position(|i| i.addr >= start)
        .unwrap_or(all.len());
    let after = all.iter().position(|i| i.addr >= end).unwrap_or(all.len());
    let lo = first.saturating_sub(context);
    let hi = after.saturating_add(context).min(all.len());
    all[lo..hi].to_vec()
}

/// Um imediato (ou deslocamento relativo de desvio) dentro de uma instrução decodificada.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImmSlot {
    /// Offset do imediato desde o início dos bytes passados a [`immediate_slot`].
    pub offset: usize,
    /// Tamanho do imediato em bytes.
    pub size: usize,
    /// O imediato é estendido com sinal até 64 bits (ex.: `mov rax, imm32`).
    pub sign_extended_to_64: bool,
    /// A instrução é um desvio relativo e o item é o deslocamento dele, não um valor.
    pub is_branch: bool,
}

/// Imediatos de cada instrução de `code` (em `ip`), segundo os `ConstantOffsets` do `iced-x86`.
///
/// Dá um item por imediato (a quase totalidade das instruções tem um só; `enter` tem dois) e um
/// para o deslocamento relativo de desvios, com `is_branch = true`. Para na primeira instrução
/// inválida.
pub fn immediate_slot(code: &[u8], ip: u64) -> Vec<ImmSlot> {
    let mut decoder = Decoder::with_ip(64, code, ip, DecoderOptions::NONE);
    let mut out = Vec::new();
    let mut insn = Instruction::default();
    while decoder.can_decode() {
        let pos = decoder.position();
        decoder.decode_out(&mut insn);
        if insn.is_invalid() {
            break;
        }
        // Os offsets valem para a instrução recém-decodificada: pegar antes da próxima.
        let offsets = decoder.get_constant_offsets(&insn);
        let kinds: Vec<OpKind> = (0..insn.op_count()).map(|i| insn.op_kind(i)).collect();
        // O `iced-x86` 1.21 informa o deslocamento de um `jmp`/`call`/`jcc` relativo como imediato.
        let is_branch = kinds.iter().any(|k| {
            matches!(
                k,
                OpKind::NearBranch16
                    | OpKind::NearBranch32
                    | OpKind::NearBranch64
                    | OpKind::FarBranch16
                    | OpKind::FarBranch32
            )
        });
        let sign_extended_to_64 = kinds
            .iter()
            .any(|k| matches!(k, OpKind::Immediate32to64 | OpKind::Immediate8to64));
        let found = [
            (
                offsets.has_immediate(),
                offsets.immediate_offset(),
                offsets.immediate_size(),
            ),
            (
                offsets.has_immediate2(),
                offsets.immediate_offset2(),
                offsets.immediate_size2(),
            ),
        ];
        for (present, offset, size) in found {
            if present {
                out.push(ImmSlot {
                    offset: pos + offset,
                    size,
                    sign_extended_to_64,
                    is_branch,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // bytes de RESEARCH.md, +57BD6D0 (FLiNG): e9 4b 29 ac 86 0f 1f 00 em ip 0x1457BD6D0
    #[test]
    fn fling_jump_destination() {
        let b = [0xe9, 0x4b, 0x29, 0xac, 0x86, 0x0f, 0x1f, 0x00];
        assert!(matches!(
            branch_at(&b, 0x1457BD6D0),
            Some(Branch::Direct(0xCC280020))
        ));
    }

    #[test]
    fn rip_indirect_slot() {
        let b = [0xff, 0x25, 0x10, 0x00, 0x00, 0x00]; // jmp [rip+0x10]
        assert!(matches!(
            branch_at(&b, 0x1000),
            Some(Branch::RipIndirect { slot: 0x1016 })
        ));
    }

    #[test]
    fn short_and_word_branches_are_not_direct() {
        // jmp rel8, jmp rel16 (prefixo 66) e call rel16: o destino não é o de um rel32.
        assert_eq!(branch_at(&[0xeb, 0x05], 0x1000), None);
        assert_eq!(branch_at(&[0x66, 0xe9, 0x10, 0x00], 0x1000), None);
        assert_eq!(branch_at(&[0x66, 0xe8, 0x10, 0x00], 0x1000), None);
    }

    #[test]
    fn far_rip_jumps_are_not_rip_indirect() {
        // jmp far [rip+0x10] (ff /5) e jmp far m16:64 (48 ff /5): o slot não é um ponteiro de 8 bytes.
        assert_eq!(
            branch_at(&[0xff, 0x2d, 0x10, 0x00, 0x00, 0x00], 0x1000),
            None
        );
        assert_eq!(
            branch_at(&[0x48, 0xff, 0x2d, 0x10, 0x00, 0x00, 0x00], 0x1000),
            None
        );
    }

    #[test]
    fn call_rel32_is_direct() {
        let b = [0xe8, 0xef, 0xab, 0x2e, 0xfe];
        assert!(matches!(
            branch_at(&b, 0x1456FC11C),
            Some(Branch::Direct(0x1439E6D10))
        ));
    }

    #[test]
    fn around_with_end_before_start_does_not_panic() {
        let mut b = vec![0x90u8; 10];
        b.push(0xc3);
        let base = 0x1000;
        let start = base + 10;
        let inverted = disassemble_around(&b, base, start, start - 4, 3);
        let empty = disassemble_around(&b, base, start, start, 3);
        assert_eq!(inverted, empty);
        // Com `context = 0` o intervalo invertido dava `lo > hi` e derrubava o processo.
        assert_eq!(
            disassemble_around(&b, base, start, start - 4, 0),
            disassemble_around(&b, base, start, start, 0)
        );
        assert_eq!(inverted.last().map(|i| i.addr), Some(start));
    }

    #[test]
    fn call_target_resolved() {
        let b = [0xe8, 0xef, 0xab, 0x2e, 0xfe]; // RESEARCH: +56FC11C -> call 0x1439E6D10
        let i = &disassemble(&b, 0x1456FC11C, 1)[0];
        assert_eq!(i.target, Some(0x1439E6D10));
        assert_eq!(i.text, "call 0x1439e6d10");
    }

    #[test]
    fn non_rip_memory_has_no_target() {
        let b = [0xf3, 0x0f, 0x10, 0x89, 0x74, 0x09, 0x00, 0x00]; // movss xmm1, [rcx+0x974]
        assert_eq!(disassemble(&b, 0x1457BD6D0, 1)[0].target, None);
    }

    #[test]
    fn invalid_byte_does_not_stop() {
        assert_eq!(disassemble(&[0x06, 0x90], 0, 5).len(), 2);
    }

    #[test]
    fn around_aligns_to_start() {
        // 10 `nop` de 1 byte seguidos do alvo (`ret`) em 0x100A.
        let mut b = vec![0x90u8; 10];
        b.push(0xc3);
        let base = 0x1000;
        let start = base + 10;
        let out = disassemble_around(&b, base, start, start + 1, 3);
        assert_eq!(out.first().map(|i| i.addr), Some(start - 3));
        assert_eq!(out.len(), 3 + 1);
        assert_eq!(out.last().map(|i| i.addr), Some(start));
    }

    #[test]
    fn immediate_slot_of_mov_eax_imm32() {
        // mov eax, 0x3FC00000 ; nop ; mov rax, imm32 (estendido com sinal)
        let code = [
            0xB8, 0x00, 0x00, 0xC0, 0x3F, 0x90, 0x48, 0xC7, 0xC0, 0x01, 0x00, 0x00, 0x00,
        ];
        assert_eq!(
            immediate_slot(&code, 0x1000),
            vec![
                ImmSlot {
                    offset: 1,
                    size: 4,
                    sign_extended_to_64: false,
                    is_branch: false
                },
                ImmSlot {
                    offset: 9,
                    size: 4,
                    sign_extended_to_64: true,
                    is_branch: false
                },
            ]
        );
    }

    #[test]
    fn immediate_slot_marks_branches_and_small_immediates() {
        // jmp rel32 ; push imm8 ; je rel8
        let code = [0xE9, 0x10, 0x00, 0x00, 0x00, 0x6A, 0x05, 0x74, 0x02];
        let slots = immediate_slot(&code, 0x1000);
        assert_eq!(slots.len(), 3);
        assert_eq!(
            (slots[0].offset, slots[0].size, slots[0].is_branch),
            (1, 4, true)
        );
        assert_eq!(
            (slots[1].offset, slots[1].size, slots[1].is_branch),
            (6, 1, false)
        );
        assert_eq!(
            (slots[2].offset, slots[2].size, slots[2].is_branch),
            (8, 1, true)
        );
    }

    #[test]
    fn immediate_slot_ignores_instructions_without_immediate() {
        assert!(immediate_slot(&[0x90, 0x48, 0x89, 0xCF, 0xC3], 0).is_empty());
    }
}
