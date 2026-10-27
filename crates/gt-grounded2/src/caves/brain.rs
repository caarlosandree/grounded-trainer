//! Cave do Brain Power (spec 008, 6.6.1).
//!
//! No ponto (`mov r14,[rax+T0]`, depois de o jogo gravar o novo total): `esi` = `ResearchPoints`
//! antigo, `[rbx+R]` = novo, `rax` = objeto da tabela de níveis. O padrão estendido confere esses
//! registradores; `T0`, `T1` e `L` vêm dos curingas dele e o passo `0x30` dos bytes fixos
//! (`lea rcx,[rax+rax*2]; shl rcx,4`), conferidos por [`brain_captures_from_hit`].
//!
//! Só lê o que o jogo lê logo depois do ponto: `[rbx+R]`, `[rax+T0]`, `[rax+T1]` e o limite das
//! entradas `0..contagem` da tabela (com a contagem limitada a 256 e o início não nulo).
//!
//! Os limites são guardados como distância ao valor antigo (`d = limite − antigo`, de 1 a
//! 2³² − 1 para os que estão acima dele), com "nenhum" = `u64::MAX`; as contas são em 64 bits,
//! sem estouro.

use iced_x86::code_asm::{
    dword_ptr, eax, edx, esi, qword_ptr, r8, r9, r9d, r10, r11, rax, rbx, rcx, rdi, rdx, rsi,
};

use super::{CaveCtx, FIELD_RESEARCH_POINTS, asm_err, assembler, disp, finish, restore, save, var};
use crate::error::{Error, Result};
use crate::signatures::brain_captures_from_hit;

/// Máximo de entradas da tabela que a cave percorre; contagem maior ou negativa: não multiplica.
pub const MAX_TABLE_ENTRIES: i32 = 256;

/// Bytes por entrada da tabela de níveis: fixo no padrão (`lea rcx,[rax+rax*2]; shl rcx,4`).
const TABLE_STRIDE: i32 = 0x30;

pub(super) fn build(ctx: &CaveCtx) -> Result<Vec<u8>> {
    let caps = brain_captures_from_hit(&ctx.captures)?;
    let research = ctx.field(FIELD_RESEARCH_POINTS)?;
    if i64::from(caps.research) != i64::from(research) {
        return Err(Error::IncoherentPattern {
            reason: format!(
                "ResearchPoints no padrão ({:#x}) diferente do da reflexão ({research:#x})",
                caps.research
            ),
        });
    }
    let start = disp(u64::from(caps.table))?;
    let count_off = disp(u64::from(caps.count))?;
    let limit = disp(u64::from(caps.limit))?;
    let v = |off: u64| disp(off);

    let mut a = assembler()?;
    let mut done = a.create_label();
    let mut scan = a.create_label();
    let mut next = a.create_label();
    let mut below_l1 = a.create_label();
    let mut caps_l2 = a.create_label();
    let mut no_l2 = a.create_label();

    // `rbx` só é lido. `rax` (a tabela) é usado pela instrução original e `esi` (o antigo) pelo
    // código do jogo depois do ponto: os dois voltam no `pop`. `rsi` só é estragado depois de
    // copiado para `r8`.
    let saved = [rax, rcx, rdx, rsi, rdi, r8, r9, r10, r11];
    save(&mut a, &saved)?;
    // r11 = variáveis; rcx = N.
    a.mov(r11, ctx.vars).map_err(asm_err)?;
    a.movsxd(rcx, dword_ptr(r11 + v(var::BRAIN_N)?))
        .map_err(asm_err)?;
    a.test(rcx, rcx).map_err(asm_err)?;
    a.jle(done).map_err(asm_err)?;
    // r8 = antigo, rdx = ganho = novo − antigo.
    a.movsxd(r8, esi).map_err(asm_err)?;
    a.movsxd(rdx, dword_ptr(rbx + research)).map_err(asm_err)?;
    a.sub(rdx, r8).map_err(asm_err)?;
    a.jle(done).map_err(asm_err)?;

    // Passo 1: r9 = min(ganho × N, int32_max, int32_max − antigo). ganho < 2³² e N < 2³¹.
    a.mov(r9, rdx).map_err(asm_err)?;
    a.imul_2(r9, rcx).map_err(asm_err)?;
    a.mov(esi, i32::MAX).map_err(asm_err)?;
    a.cmp(r9, rsi).map_err(asm_err)?;
    a.cmovg(r9, rsi).map_err(asm_err)?;
    a.sub(rsi, r8).map_err(asm_err)?;
    a.cmp(r9, rsi).map_err(asm_err)?;
    a.cmovg(r9, rsi).map_err(asm_err)?;

    // Passo 2: rdi = d(L1), rsi = d(L2), as duas menores distâncias distintas > 0.
    a.mov(r10, qword_ptr(rax + start)).map_err(asm_err)?;
    a.movsxd(rcx, dword_ptr(rax + count_off)).map_err(asm_err)?;
    // Sem sinal: contagem negativa vira enorme e também não multiplica.
    a.cmp(rcx, MAX_TABLE_ENTRIES).map_err(asm_err)?;
    a.ja(done).map_err(asm_err)?;
    a.or(rdi, -1).map_err(asm_err)?;
    a.or(rsi, -1).map_err(asm_err)?;
    a.jrcxz(caps_l2).map_err(asm_err)?;
    a.test(r10, r10).map_err(asm_err)?;
    a.jz(done).map_err(asm_err)?;
    a.set_label(&mut scan).map_err(asm_err)?;
    a.movsxd(rax, dword_ptr(r10 + limit)).map_err(asm_err)?;
    a.sub(rax, r8).map_err(asm_err)?;
    a.jle(next).map_err(asm_err)?;
    a.cmp(rax, rdi).map_err(asm_err)?;
    a.je(next).map_err(asm_err)?;
    a.jb(below_l1).map_err(asm_err)?;
    a.cmp(rax, rsi).map_err(asm_err)?;
    a.jae(next).map_err(asm_err)?;
    a.mov(rsi, rax).map_err(asm_err)?;
    a.jmp(next).map_err(asm_err)?;
    a.set_label(&mut below_l1).map_err(asm_err)?;
    a.mov(rsi, rdi).map_err(asm_err)?;
    a.mov(rdi, rax).map_err(asm_err)?;
    a.set_label(&mut next).map_err(asm_err)?;
    a.add(r10, TABLE_STRIDE).map_err(asm_err)?;
    a.dec(rcx).map_err(asm_err)?;
    a.jnz(scan).map_err(asm_err)?;

    // Com L2: r9 = min(r9, L2 − 1 − antigo) (≥ 1, porque L2 > L1 > antigo).
    a.set_label(&mut caps_l2).map_err(asm_err)?;
    a.cmp(rsi, -1).map_err(asm_err)?;
    a.je(no_l2).map_err(asm_err)?;
    a.lea(rax, qword_ptr(rsi - 1)).map_err(asm_err)?;
    a.cmp(r9, rax).map_err(asm_err)?;
    a.cmovg(r9, rax).map_err(asm_err)?;
    a.set_label(&mut no_l2).map_err(asm_err)?;

    // Passo 3: nunca abaixo do ganho original. Passo 4: grava antigo + aplicado.
    a.cmp(r9, rdx).map_err(asm_err)?;
    a.cmovl(r9, rdx).map_err(asm_err)?;
    a.lea(rax, qword_ptr(r8 + r9)).map_err(asm_err)?;
    a.mov(dword_ptr(rbx + research), eax).map_err(asm_err)?;

    // Registro: sequência ímpar, ganho, aplicado, L1, L2 (absolutos; 0 = nenhum), sequência par.
    a.lock()
        .inc(dword_ptr(r11 + v(var::BRAIN_SEQ)?))
        .map_err(asm_err)?;
    a.mov(dword_ptr(r11 + v(var::BRAIN_GAIN)?), edx)
        .map_err(asm_err)?;
    a.mov(dword_ptr(r11 + v(var::BRAIN_APPLIED)?), r9d)
        .map_err(asm_err)?;
    for (d, off) in [(rdi, var::BRAIN_L1), (rsi, var::BRAIN_L2)] {
        let mut store = a.create_label();
        a.xor(eax, eax).map_err(asm_err)?;
        a.cmp(d, -1).map_err(asm_err)?;
        a.je(store).map_err(asm_err)?;
        a.lea(eax, qword_ptr(d + r8)).map_err(asm_err)?;
        a.set_label(&mut store).map_err(asm_err)?;
        a.mov(dword_ptr(r11 + v(off)?), eax).map_err(asm_err)?;
    }
    a.lock()
        .inc(dword_ptr(r11 + v(var::BRAIN_SEQ)?))
        .map_err(asm_err)?;

    a.set_label(&mut done).map_err(asm_err)?;
    restore(&mut a, &saved)?;
    finish(a, ctx)
}
