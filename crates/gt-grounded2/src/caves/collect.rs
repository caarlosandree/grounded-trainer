//! Cave da coleta multiplicada (spec 008, 6.7.4).
//!
//! O ponto é a primeira instrução da implementação nativa do `ServerAddItem` (`mov [rsp+8],rbx`,
//! `rcx` = `InventoryComponent`, `rdx` = o `Item` que vai entrar, `r8b` = sinalizador). Com
//! `N > 0`, o inventário do jogador (variável) não nulo e igual a `rcx`, `rdx` não nulo e da classe
//! `Item` (ou subclasse) e `StackSize > 0`, grava `StackSize = min(StackSize × N, i32::MAX)`, com a
//! multiplicação em 64 bits. Senão passa direto.
//!
//! `N` e o inventário são lidos **uma vez** cada, para um registrador: o trainer pode trocar as
//! variáveis enquanto uma thread do jogo está aqui dentro, e uma segunda leitura poderia ver outro
//! valor (um `N` 0 lido depois do teste zeraria a pilha).
//!
//! Depois do ponto o jogo lê `rbx` (pela instrução original, guardado em `[rsp+8]`), `rbp`, `rsi`,
//! `rdi` (salvos no prólogo), `rcx`, `rdx` e `r8b` (os argumentos); `rax`, `r9`..`r11` e as flags são
//! escritos antes de lidos. A cave salva e devolve tudo o que toca (`rax`, `r8`..`r11`) e as flags,
//! e só lê `rcx` e `rdx`, então não depende dessa lista.

use iced_x86::code_asm::{dword_ptr, eax, qword_ptr, r8, r8d, r9, r10, r11, rax, rcx, rdx};

use super::{
    CaveCtx, FIELD_STACK_SIZE, asm_err, assembler, class_walk, disp, finish, restore, save, var,
};
use crate::error::Result;

pub(super) fn build(ctx: &CaveCtx) -> Result<Vec<u8>> {
    let stack = ctx.field(FIELD_STACK_SIZE)?;

    let mut a = assembler()?;
    let mut done = a.create_label();
    let mut matched = a.create_label();

    // `rcx` (inventário) e `rdx` (item) só são lidos.
    let saved = [rax, r8, r9, r10, r11];
    save(&mut a, &saved)?;
    a.mov(r11, ctx.vars).map_err(asm_err)?;
    // N (uma leitura só): ≤ 0 passa direto.
    a.movsxd(r10, dword_ptr(r11 + disp(var::COLLECT_N)?))
        .map_err(asm_err)?;
    a.test(r10, r10).map_err(asm_err)?;
    a.jle(done).map_err(asm_err)?;
    // Inventário do jogador (uma leitura só): nulo ou outro inventário passa direto.
    a.mov(r9, qword_ptr(r11 + disp(var::COLLECT_INVENTORY)?))
        .map_err(asm_err)?;
    a.test(r9, r9).map_err(asm_err)?;
    a.jz(done).map_err(asm_err)?;
    a.cmp(r9, rcx).map_err(asm_err)?;
    a.jne(done).map_err(asm_err)?;
    // Item não nulo e da classe `Item` (ponteiro da classe conferido ≠ 0).
    a.test(rdx, rdx).map_err(asm_err)?;
    a.jz(done).map_err(asm_err)?;
    a.mov(r9, qword_ptr(r11 + disp(var::ITEM_CLASS)?))
        .map_err(asm_err)?;
    a.test(r9, r9).map_err(asm_err)?;
    a.jz(done).map_err(asm_err)?;
    class_walk(&mut a, &ctx.layout, rdx, r9, (rax, r8d), matched, done)?;
    a.set_label(&mut matched).map_err(asm_err)?;
    // StackSize > 0: × N em 64 bits (os dois < 2^31, o produto < 2^62), limitado ao i32.
    a.movsxd(rax, dword_ptr(rdx + stack)).map_err(asm_err)?;
    a.test(rax, rax).map_err(asm_err)?;
    a.jle(done).map_err(asm_err)?;
    a.imul_2(rax, r10).map_err(asm_err)?;
    a.mov(r8d, i32::MAX as u32).map_err(asm_err)?;
    a.cmp(rax, r8).map_err(asm_err)?;
    a.cmovg(rax, r8).map_err(asm_err)?;
    a.mov(dword_ptr(rdx + stack), eax).map_err(asm_err)?;
    a.set_label(&mut done).map_err(asm_err)?;
    restore(&mut a, &saved)?;
    // A instrução original (`mov [rsp+8],rbx`) vem depois do `popfq`: `rsp` é o da entrada.
    finish(a, ctx)
}
