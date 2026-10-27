//! Corpos das code caves (spec 008, 6.6.1, 6.7.1, 6.7.2 e 6.7.3), montados com `iced-x86`.
//!
//! Toda cave tem a mesma forma:
//!
//! 1. `pushfq` e `push` de cada registrador que o corpo usa (nada abaixo do `rsp` é do jogo: a
//!    convenção x64 do Windows não tem *red zone*);
//! 2. o corpo, que só lê ponteiros que o próprio jogo lê naquele ponto ou que vêm das variáveis
//!    conferidas como não nulas, e nunca chama nada;
//! 3. `pop` de tudo e `popfq`: registradores, flags e `rsp` voltam a ser os da entrada;
//! 4. a instrução original, relocada ([`cave::relocate`]), executada com o `rsp` da entrada;
//! 5. `jmp` de volta para a instrução seguinte ao desvio.
//!
//! Com as variáveis zeradas todo corpo passa direto para a instrução original (spec 008, 6.3). O
//! código final passa por [`cave::check_cave_code`] antes de sair daqui.

mod analyzer;
mod brain;
mod drag;
mod observe;

use std::collections::HashMap;

use gt_core::cave;
use gt_ue::UeLayout;
use iced_x86::IcedError;
pub use iced_x86::Register;
use iced_x86::code_asm::{AsmRegister32, AsmRegister64, CodeAssembler, CodeLabel, qword_ptr};

use crate::error::{Error, Result};
use crate::signatures::CaveBody;

/// Deslocamentos das variáveis a partir do começo da folga da `.data` (spec 008, 6.4).
pub mod var {
    /// Brain Power: multiplicador `N` (`i32`).
    pub const BRAIN_N: u64 = 0;
    /// Brain Power: último ganho original (`i32`).
    pub const BRAIN_GAIN: u64 = 4;
    /// Brain Power: último ganho aplicado (`i32`).
    pub const BRAIN_APPLIED: u64 = 8;
    /// Brain Power: próximo limite de nível acima do valor antigo, 0 se não houver (`i32`).
    pub const BRAIN_L1: u64 = 12;
    /// Brain Power: segundo limite distinto acima do antigo, 0 se não houver (`i32`).
    pub const BRAIN_L2: u64 = 16;
    /// Brain Power: contador de sequência (`u32`): ímpar durante a gravação, sobe 2 por ganho.
    pub const BRAIN_SEQ: u64 = 20;
    /// Arrastar: quantidade armada (`i32`); 0 = desarmado.
    pub const DRAG_N: u64 = 24;
    /// Arrastar: contador de aplicadas (`u64`).
    pub const DRAG_APPLIED: u64 = 32;
    /// Analisador: ligado (`u64`, 0 ou 1).
    pub const ANALYZER_ON: u64 = 40;
    /// Analisador: ponteiro da classe `ResourceAnalyzerBuilding` (`u64`).
    pub const ANALYZER_CLASS: u64 = 48;
    /// Coleta: multiplicador (`i32`).
    pub const COLLECT_N: u64 = 56;
    /// Coleta: inventário do jogador (`u64`).
    pub const COLLECT_INVENTORY: u64 = 64;
    /// Coleta: 8 endereços de retorno permitidos (`u64` cada).
    pub const COLLECT_RETURNS: u64 = 72;
    /// Arrastar e coleta: ponteiro da classe `Item` (`u64`).
    pub const ITEM_CLASS: u64 = 136;
    /// Observação: contador de passagens (`u64`).
    pub const OBSERVE_COUNT: u64 = 144;
    /// Observação: anel de [`OBSERVE_RING_LEN`] entradas de [`OBSERVE_ENTRY`] bytes (retorno e
    /// registrador, `u64` cada). A passagem `k` (a partir de 0) grava a entrada `k % 8`.
    pub const OBSERVE_RING: u64 = 152;
    /// Entradas do anel da observação.
    pub const OBSERVE_RING_LEN: u64 = 8;
    /// Bytes por entrada do anel da observação.
    pub const OBSERVE_ENTRY: u64 = 16;
    /// Tamanho total da tabela de variáveis.
    pub const TOTAL: u64 = 280;
}

/// Nome do campo `PartyComponent.ResearchPoints` em [`CaveCtx::fields`].
pub const FIELD_RESEARCH_POINTS: &str = "ResearchPoints";
/// Nome do campo `Item.StackSize` em [`CaveCtx::fields`].
pub const FIELD_STACK_SIZE: &str = "StackSize";
/// Nome do campo `ResourceAnalyzerBuilding.MaxPowerCapacity` em [`CaveCtx::fields`].
pub const FIELD_MAX_POWER_CAPACITY: &str = "MaxPowerCapacity";

/// Subidas de `SuperStruct` ao conferir a classe de um objeto (a classe dele e mais 16).
pub const MAX_CLASS_DEPTH: u32 = 16;

/// Tudo que um corpo precisa para ser montado.
#[derive(Debug, Clone)]
pub struct CaveCtx {
    /// Endereço da instrução trocada pelo desvio.
    pub hook_at: u64,
    /// Endereço onde a cave vai ficar.
    pub cave_at: u64,
    /// Bytes da instrução original (uma instrução inteira, 5+ bytes).
    pub insn: Vec<u8>,
    /// Começo da folga da `.data` (base das variáveis, [`var`]).
    pub vars: u64,
    /// Ocorrência inteira do padrão (o Brain Power lê os curingas daqui).
    pub captures: Vec<u8>,
    /// Offsets da reflexão por nome do campo ([`FIELD_RESEARCH_POINTS`] etc.).
    pub fields: HashMap<&'static str, u32>,
    /// Layout da reflexão (`ClassPrivate` e `SuperStruct`).
    pub layout: UeLayout,
}

impl CaveCtx {
    /// Offset de um campo da reflexão, como deslocamento de instrução.
    fn field(&self, name: &str) -> Result<i32> {
        let off = self
            .fields
            .get(name)
            .ok_or_else(|| build_err(format!("campo {name} não encontrado")))?;
        disp(u64::from(*off))
    }

    /// Endereço de volta (a instrução seguinte ao desvio).
    fn back_to(&self) -> u64 {
        self.hook_at.wrapping_add(self.insn.len() as u64)
    }
}

/// Monta o corpo `body` para o desvio descrito em `ctx`.
///
/// O código devolvido já passou por [`cave::check_cave_code`]. O tamanho não depende de
/// `cave_at` (só de `ctx.insn`): quem aloca pode montar uma vez para medir e de novo no lugar.
pub fn build(body: CaveBody, ctx: &CaveCtx) -> Result<Vec<u8>> {
    match body {
        CaveBody::Brain => brain::build(ctx),
        CaveBody::Analyzer => analyzer::build(ctx),
        CaveBody::Drag => drag::build(ctx),
        CaveBody::Collect => Err(build_err(
            "o corpo da coleta ainda não existe (depende da pesquisa no jogo)",
        )),
    }
}

/// Registrador de 64 bits pelo nome (`rax`..`r15`, `rsp` incluído; sem `rip`), para
/// `gt-patch observe`. Maiúsculas e espaços nas pontas são aceitos.
pub fn parse_register(name: &str) -> Result<Register> {
    const GPRS: [(&str, Register); 16] = [
        ("rax", Register::RAX),
        ("rcx", Register::RCX),
        ("rdx", Register::RDX),
        ("rbx", Register::RBX),
        ("rsp", Register::RSP),
        ("rbp", Register::RBP),
        ("rsi", Register::RSI),
        ("rdi", Register::RDI),
        ("r8", Register::R8),
        ("r9", Register::R9),
        ("r10", Register::R10),
        ("r11", Register::R11),
        ("r12", Register::R12),
        ("r13", Register::R13),
        ("r14", Register::R14),
        ("r15", Register::R15),
    ];
    let wanted = name.trim().to_ascii_lowercase();
    GPRS.iter()
        .find(|(n, _)| *n == wanted)
        .map(|(_, r)| *r)
        .ok_or_else(|| {
            build_err(format!(
                "`{}` não é um registrador de 64 bits (rax, rcx, rdx, rbx, rsp, rbp, rsi, rdi, r8..r15)",
                name.trim()
            ))
        })
}

/// Monta a cave de observação (spec 008, 6.7.3) que guarda `[rsp]` e `reg` a cada passagem.
///
/// `reg` é um registrador de 64 bits (inclusive `rsp`, gravado com o valor da entrada).
pub fn build_observe(ctx: &CaveCtx, reg: Register) -> Result<Vec<u8>> {
    observe::build(ctx, reg)
}

pub(crate) fn build_err(reason: impl Into<String>) -> Error {
    Error::CaveBuild {
        reason: reason.into(),
    }
}

pub(crate) fn asm_err(err: IcedError) -> Error {
    build_err(format!("montador: {err}"))
}

/// Deslocamento de 32 bits com sinal para um operando de memória.
pub(crate) fn disp(off: u64) -> Result<i32> {
    i32::try_from(off).map_err(|_| build_err(format!("deslocamento {off:#x} fora do i32")))
}

/// Salva as flags e `regs`, na ordem.
pub(crate) fn save(a: &mut CodeAssembler, regs: &[AsmRegister64]) -> Result<()> {
    a.pushfq().map_err(asm_err)?;
    for r in regs {
        a.push(*r).map_err(asm_err)?;
    }
    Ok(())
}

/// Devolve `regs` (na ordem inversa da de [`save`]) e as flags.
pub(crate) fn restore(a: &mut CodeAssembler, regs: &[AsmRegister64]) -> Result<()> {
    for r in regs.iter().rev() {
        a.pop(*r).map_err(asm_err)?;
    }
    a.popfq().map_err(asm_err)?;
    Ok(())
}

/// Confere se a classe de `obj` é `target` ou descende dela, subindo `ClassPrivate` →
/// `SuperStruct` no máximo [`MAX_CLASS_DEPTH`] vezes, sem `call` (spec 008, 6.7.1 e 6.7.2).
///
/// Salta para `matched` se achar e para `fail` se não (ponteiro nulo na cadeia ou fundo demais).
/// `obj` e `target` não podem ser nulos (quem chama confere). Estraga `cur`, `count` e as flags.
pub(crate) fn class_walk(
    a: &mut CodeAssembler,
    layout: &UeLayout,
    obj: AsmRegister64,
    target: AsmRegister64,
    (cur, count): (AsmRegister64, AsmRegister32),
    matched: CodeLabel,
    fail: CodeLabel,
) -> Result<()> {
    let class = disp(u64::from(layout.uobject.class))?;
    let super_struct = disp(u64::from(layout.ustruct.super_struct))?;
    let mut top = a.create_label();
    a.mov(cur, qword_ptr(obj + class)).map_err(asm_err)?;
    a.mov(count, MAX_CLASS_DEPTH + 1).map_err(asm_err)?;
    a.set_label(&mut top).map_err(asm_err)?;
    a.test(cur, cur).map_err(asm_err)?;
    a.jz(fail).map_err(asm_err)?;
    a.cmp(cur, target).map_err(asm_err)?;
    a.je(matched).map_err(asm_err)?;
    a.dec(count).map_err(asm_err)?;
    a.jz(fail).map_err(asm_err)?;
    a.mov(cur, qword_ptr(cur + super_struct)).map_err(asm_err)?;
    a.jmp(top).map_err(asm_err)?;
    Ok(())
}

/// Fecha a cave: monta o corpo `a` em `ctx.cave_at`, acrescenta a instrução original relocada e o
/// `jmp` de volta e confere tudo com [`cave::check_cave_code`].
///
/// O corpo tem de terminar devolvendo registradores e flags (cai na instrução original).
pub(crate) fn finish(mut a: CodeAssembler, ctx: &CaveCtx) -> Result<Vec<u8>> {
    let mut code = a.assemble(ctx.cave_at).map_err(asm_err)?;
    let orig_at = ctx.cave_at + code.len() as u64;
    code.extend(cave::relocate(&ctx.insn, ctx.hook_at, orig_at)?);
    let jmp_at = ctx.cave_at + code.len() as u64;
    code.extend(cave::hook_bytes(jmp_at, 5, ctx.back_to())?);
    cave::check_cave_code(&code, ctx.cave_at, ctx.back_to())?;
    Ok(code)
}

/// Novo montador de 64 bits.
pub(crate) fn assembler() -> Result<CodeAssembler> {
    CodeAssembler::new(64).map_err(asm_err)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn parse_register_takes_the_64_bit_gprs_only() {
        for (name, reg) in [
            ("rax", Register::RAX),
            ("RCX", Register::RCX),
            (" rdx ", Register::RDX),
            ("rbx", Register::RBX),
            ("rsp", Register::RSP),
            ("rbp", Register::RBP),
            ("rsi", Register::RSI),
            ("rdi", Register::RDI),
            ("r8", Register::R8),
            ("r15", Register::R15),
        ] {
            assert_eq!(parse_register(name).unwrap(), reg, "{name}");
        }
        for bad in ["", "eax", "cl", "xmm0", "rip", "r16", "rcx,rdx", "foo"] {
            let err = parse_register(bad).unwrap_err();
            assert!(
                matches!(err, Error::CaveBuild { .. }) && err.to_string().contains("registrador"),
                "{bad}: {err}"
            );
        }
    }

    #[test]
    fn var_layout_matches_the_spec_table() {
        // Brain 0..24, arrastar 24..40, analisador 40..56, coleta 56..136, Item 136, observação
        // 144..280 (spec 008, 6.4).
        assert_eq!(var::BRAIN_SEQ + 4, var::DRAG_N);
        assert_eq!(var::DRAG_APPLIED + 8, var::ANALYZER_ON);
        assert_eq!(var::ANALYZER_CLASS + 8, var::COLLECT_N);
        assert_eq!(var::COLLECT_RETURNS + 8 * 8, var::ITEM_CLASS);
        assert_eq!(var::ITEM_CLASS + 8, var::OBSERVE_COUNT);
        assert_eq!(
            var::OBSERVE_RING + var::OBSERVE_RING_LEN * var::OBSERVE_ENTRY,
            var::TOTAL
        );
        for off in [
            var::DRAG_APPLIED,
            var::ANALYZER_ON,
            var::ANALYZER_CLASS,
            var::COLLECT_INVENTORY,
            var::ITEM_CLASS,
            var::OBSERVE_COUNT,
            var::OBSERVE_RING,
        ] {
            assert_eq!(off % 8, 0, "{off}");
        }
    }
}

#[cfg(test)]
mod game_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

    use super::*;

    /// Ocorrência do padrão do Brain Power com os valores do build documentado em `RESEARCH.md`.
    const OCC: &str = "8b b1 14 05 00 00 4d 89 73 d8 89 75 fc 8d 04 16 48 8b 51 10 89 81 14 05 00 00 e8 00 00 00 00 e8 00 00 00 00 4c 8b b0 18 01 00 00 48 63 80 20 01 00 00 48 8d 0c 40 48 c1 e1 04 49 03 ce 4c 3b f1 0f 84 00 00 00 00 0f 1f 80 00 00 00 00 41 8b 46 10 3b f0 7d 00 3b 83 14 05 00 00";
    const BASE: u64 = 0x1_4000_0000;
    /// A sequência de 1871 `CC` do jogo (RVA `0xdfe8b1`).
    const RUN: (u64, u64) = (0xdfe8b1, 1871);

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .map(|t| u8::from_str_radix(t, 16).unwrap())
            .collect()
    }

    fn ctx(hook_rva: u64, insn: &str, captures: Vec<u8>) -> CaveCtx {
        CaveCtx {
            hook_at: BASE + hook_rva,
            cave_at: BASE + RUN.0 + cave::CAVE_MARGIN as u64,
            insn: hex(insn),
            vars: BASE + 0xa840000 - 0x130,
            captures,
            fields: HashMap::from([
                (FIELD_RESEARCH_POINTS, 0x514),
                (FIELD_STACK_SIZE, 0x2b8),
                (FIELD_MAX_POWER_CAPACITY, 0x980),
            ]),
            layout: UeLayout::observed(),
        }
    }

    /// Os corpos com os pontos do jogo: (nome, ctx, código).
    fn game_bodies() -> Vec<(&'static str, CaveCtx, Vec<u8>)> {
        let occ = hex(OCC);
        let brain = ctx(0x5970ee7, "4c 8b b0 18 01 00 00", occ);
        let analyzer = ctx(0x57bd6d0, "f3 0f 10 89 74 09 00 00", vec![]);
        let drag = ctx(0x5bbfe70, "48 89 5c 24 08", vec![]);
        vec![
            (
                "brain",
                brain.clone(),
                build(CaveBody::Brain, &brain).unwrap(),
            ),
            (
                "analyzer",
                analyzer.clone(),
                build(CaveBody::Analyzer, &analyzer).unwrap(),
            ),
            ("drag", drag.clone(), build(CaveBody::Drag, &drag).unwrap()),
            (
                "observe rcx",
                drag.clone(),
                build_observe(&drag, Register::RCX).unwrap(),
            ),
        ]
    }

    #[test]
    fn game_bodies_fit_the_cc_run() {
        let total: usize = game_bodies().iter().map(|(_, _, c)| c.len()).sum();
        // Os quatro juntos, mais a coleta estimada em 150 bytes, cabem na sequência de 1871.
        assert!(
            total + 150 + cave::CAVE_MARGIN <= RUN.1 as usize,
            "{total} bytes"
        );
        // A instrução original sai igual (nenhuma é relativa ao RIP) e o tamanho não depende do
        // endereço da cave.
        for (name, c, code) in game_bodies() {
            let tail = &code[code.len() - 5 - c.insn.len()..code.len() - 5];
            assert_eq!(tail, &c.insn[..], "{name}");
            let mut moved = c.clone();
            moved.cave_at += 0x1234;
            let again = if name == "observe rcx" {
                build_observe(&moved, Register::RCX).unwrap()
            } else {
                let body = match name {
                    "brain" => CaveBody::Brain,
                    "analyzer" => CaveBody::Analyzer,
                    _ => CaveBody::Drag,
                };
                build(body, &moved).unwrap()
            };
            assert_eq!(again.len(), code.len(), "{name}");
        }
    }

    /// Imprime a desmontagem de cada corpo (para conferência manual):
    /// `cargo test -p gt-grounded2 --lib caves::game_tests::dump_bodies -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn dump_bodies() {
        use iced_x86::{Decoder, DecoderOptions, Formatter, IntelFormatter};
        let mut f = IntelFormatter::new();
        for (name, c, code) in game_bodies() {
            println!("== {name} ({} bytes)", code.len());
            for i in Decoder::with_ip(64, &code, c.cave_at, DecoderOptions::NONE) {
                let mut s = String::new();
                f.format(&i, &mut s);
                println!("+{:03x} {:2}  {}", i.ip() - c.cave_at, i.len(), s);
            }
        }
    }
}
