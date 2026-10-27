//! Cave do analisador infinito (spec 008, 6.7.1).
//!
//! No ponto (primeira instrução do getter das cargas, `movss xmmK,[rcx+C]`): com a variável
//! "ligado" ≠ 0, o ponteiro da classe ≠ 0 e o `rcx` da classe `ResourceAnalyzerBuilding` (ou
//! subclasse), grava `float(MaxPowerCapacity)` em `[rcx+C]` se `MaxPowerCapacity > 0`. `C` é o
//! `disp32` da própria instrução.
//!
//! Usa o `xmmK` da instrução original como rascunho da conversão: a instrução seguinte (o
//! `movss` de carga) escreve o `xmmK` inteiro (bits 0–31 da memória, 32–127 zerados), então o
//! valor que a cave deixa nele nunca é visto pelo jogo.

use iced_x86::code_asm::{dword_ptr, eax, get_xmm, qword_ptr, r8, r8d, r11, rax, rcx, rdx};
use iced_x86::{Code, Decoder, DecoderOptions, OpKind, Register};

use super::{
    CaveCtx, FIELD_MAX_POWER_CAPACITY, asm_err, assembler, build_err, class_walk, disp, finish,
    restore, save, var,
};
use crate::error::Result;

/// `C` e o `xmm` de destino de `movss xmmK,[rcx+C]`.
fn charges_insn(insn: &[u8]) -> Result<(i32, Register)> {
    let i = Decoder::with_ip(64, insn, 0, DecoderOptions::NONE).decode();
    let ok = !i.is_invalid()
        && i.len() == insn.len()
        && i.code() == Code::Movss_xmm_xmmm32
        && i.op1_kind() == OpKind::Memory
        && i.memory_base() == Register::RCX
        && i.memory_index() == Register::None;
    if !ok {
        return Err(build_err(
            "a instrução do analisador não é movss xmm,[rcx+deslocamento]",
        ));
    }
    let charges = i32::try_from(i.memory_displacement64() as i64)
        .map_err(|_| build_err("deslocamento das cargas fora do i32"))?;
    Ok((charges, i.op0_register()))
}

pub(super) fn build(ctx: &CaveCtx) -> Result<Vec<u8>> {
    let (charges, xmm) = charges_insn(&ctx.insn)?;
    let xmm = get_xmm(xmm).ok_or_else(|| build_err("destino do movss não é xmm"))?;
    let max_power = ctx.field(FIELD_MAX_POWER_CAPACITY)?;

    let mut a = assembler()?;
    let mut done = a.create_label();
    let mut matched = a.create_label();

    // `rcx` (o objeto) só é lido.
    let saved = [rax, rdx, r8, r11];
    save(&mut a, &saved)?;
    a.mov(r11, ctx.vars).map_err(asm_err)?;
    a.cmp(qword_ptr(r11 + disp(var::ANALYZER_ON)?), 0)
        .map_err(asm_err)?;
    a.je(done).map_err(asm_err)?;
    a.mov(rdx, qword_ptr(r11 + disp(var::ANALYZER_CLASS)?))
        .map_err(asm_err)?;
    a.test(rdx, rdx).map_err(asm_err)?;
    a.jz(done).map_err(asm_err)?;
    a.test(rcx, rcx).map_err(asm_err)?;
    a.jz(done).map_err(asm_err)?;
    class_walk(&mut a, &ctx.layout, rcx, rdx, (rax, r8d), matched, done)?;
    a.set_label(&mut matched).map_err(asm_err)?;
    a.mov(eax, dword_ptr(rcx + max_power)).map_err(asm_err)?;
    a.test(eax, eax).map_err(asm_err)?;
    a.jle(done).map_err(asm_err)?;
    a.cvtsi2ss(xmm, eax).map_err(asm_err)?;
    a.movss(dword_ptr(rcx + charges), xmm).map_err(asm_err)?;
    a.set_label(&mut done).map_err(asm_err)?;
    restore(&mut a, &saved)?;
    finish(a, ctx)
}
