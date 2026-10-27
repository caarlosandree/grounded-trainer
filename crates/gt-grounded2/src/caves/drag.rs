//! Cave da quantidade ao arrastar (spec 008, 6.7.2).
//!
//! No ponto (primeira instrução da função, `mov [rsp+8],rbx`, relativa ao `rsp`): com `rcx` ≠ 0
//! da classe `Item` (ou subclasse; ponteiro da classe na variável [`var::ITEM_CLASS`], ≠ 0),
//! troca a variável `N` por 0 com `xchg` (atômico: duas threads nunca aplicam o mesmo arme) e,
//! se era `N > 0`, grava `N` em `[rcx+StackSize]` e soma 1 ao contador de aplicadas (`lock`).
//! A instrução original roda depois dos `pop`, com o `rsp` da entrada.

use iced_x86::code_asm::{dword_ptr, eax, qword_ptr, r8, r8d, r11, rax, rcx, rdx};

use super::{
    CaveCtx, FIELD_STACK_SIZE, asm_err, assembler, class_walk, disp, finish, restore, save, var,
};
use crate::error::Result;

pub(super) fn build(ctx: &CaveCtx) -> Result<Vec<u8>> {
    let stack_size = ctx.field(FIELD_STACK_SIZE)?;

    let mut a = assembler()?;
    let mut done = a.create_label();
    let mut matched = a.create_label();

    // `rcx` (o item) só é lido.
    let saved = [rax, rdx, r8, r11];
    save(&mut a, &saved)?;
    a.mov(r11, ctx.vars).map_err(asm_err)?;
    // Desarmado: sai antes de olhar o item (só leitura da variável).
    a.cmp(dword_ptr(r11 + disp(var::DRAG_N)?), 0)
        .map_err(asm_err)?;
    a.jle(done).map_err(asm_err)?;
    a.test(rcx, rcx).map_err(asm_err)?;
    a.jz(done).map_err(asm_err)?;
    a.mov(rdx, qword_ptr(r11 + disp(var::ITEM_CLASS)?))
        .map_err(asm_err)?;
    a.test(rdx, rdx).map_err(asm_err)?;
    a.jz(done).map_err(asm_err)?;
    class_walk(&mut a, &ctx.layout, rcx, rdx, (rax, r8d), matched, done)?;
    a.set_label(&mut matched).map_err(asm_err)?;
    // `xchg` com memória tem `lock` implícito.
    a.xor(eax, eax).map_err(asm_err)?;
    a.xchg(dword_ptr(r11 + disp(var::DRAG_N)?), eax)
        .map_err(asm_err)?;
    a.test(eax, eax).map_err(asm_err)?;
    a.jle(done).map_err(asm_err)?;
    a.mov(dword_ptr(rcx + stack_size), eax).map_err(asm_err)?;
    a.lock()
        .inc(qword_ptr(r11 + disp(var::DRAG_APPLIED)?))
        .map_err(asm_err)?;
    a.set_label(&mut done).map_err(asm_err)?;
    restore(&mut a, &saved)?;
    finish(a, ctx)
}
