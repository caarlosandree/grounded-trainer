//! Cave de observação (spec 008, 6.7.3): ferramenta de pesquisa.
//!
//! A cada passagem soma 1 ao contador (`lock xadd`, que devolve a passagem `k`) e grava, na
//! entrada `k % 8` do anel, o `[rsp]` da entrada (o retorno, quando o ponto é o começo de uma
//! função) e o valor do registrador escolhido. Não desreferencia nada além do `[rsp]` e não muda
//! nada que o jogo veja.

use iced_x86::Register;
use iced_x86::code_asm::{get_gpr32, get_gpr64, qword_ptr, rsp};

use super::{CaveCtx, asm_err, assembler, build_err, disp, finish, restore, save, var};
use crate::error::Result;

/// Registradores de rascunho candidatos, na ordem de preferência.
const SCRATCH: [Register; 4] = [Register::RAX, Register::RDX, Register::R11, Register::R10];

pub(super) fn build(ctx: &CaveCtx, reg: Register) -> Result<Vec<u8>> {
    let watched = get_gpr64(reg)
        .filter(|_| reg != Register::RIP)
        .ok_or_else(|| build_err(format!("{reg:?} não é um registrador de 64 bits")))?;
    // Dois rascunhos diferentes do registrador observado.
    let mut free = SCRATCH.iter().copied().filter(|r| *r != reg);
    let (Some(base_reg), Some(slot_reg)) = (free.next(), free.next()) else {
        return Err(build_err("sem registrador de rascunho"));
    };
    let scratch = |r: Register| get_gpr64(r).ok_or_else(|| build_err("rascunho inválido"));
    let (base, slot) = (scratch(base_reg)?, scratch(slot_reg)?);
    let slot32 = get_gpr32(slot_reg.full_register32())
        .ok_or_else(|| build_err("rascunho sem parte de 32 bits"))?;

    let saved = [base, slot];
    // Distância do `rsp` atual até o da entrada: `pushfq` e os `push`.
    let pushed = disp(8 * (saved.len() as u64 + 1))?;

    let mut a = assembler()?;
    save(&mut a, &saved)?;
    a.mov(base, ctx.vars).map_err(asm_err)?;
    a.mov(slot32, 1).map_err(asm_err)?;
    a.lock()
        .xadd(qword_ptr(base + disp(var::OBSERVE_COUNT)?), slot)
        .map_err(asm_err)?;
    // slot = variáveis + (k % 8) × 16.
    a.and(slot32, (var::OBSERVE_RING_LEN - 1) as i32)
        .map_err(asm_err)?;
    a.shl(slot, var::OBSERVE_ENTRY.trailing_zeros())
        .map_err(asm_err)?;
    a.add(slot, base).map_err(asm_err)?;
    let ring = disp(var::OBSERVE_RING)?;
    // O retorno: `[rsp]` da entrada.
    a.mov(base, qword_ptr(rsp + pushed)).map_err(asm_err)?;
    a.mov(qword_ptr(slot + ring), base).map_err(asm_err)?;
    // O registrador, com o valor da entrada (o `rsp` de antes dos `push`).
    if reg == Register::RSP {
        a.lea(base, qword_ptr(rsp + pushed)).map_err(asm_err)?;
        a.mov(qword_ptr(slot + ring + 8), base).map_err(asm_err)?;
    } else {
        a.mov(qword_ptr(slot + ring + 8), watched)
            .map_err(asm_err)?;
    }
    restore(&mut a, &saved)?;
    finish(a, ctx)
}
