//! `gt disasm <endereço> [-n N]`: desmontagem Intel (spec 6.6).

use std::io::Write;

use anyhow::{anyhow, bail};
use gt_core::addr::{parse_addr, parse_size};
use gt_core::asm::{Insn, disassemble};
use gt_core::guard;
use serde::Serialize;

use super::Global;
use crate::context::Target;
use crate::output::emit;

/// Instruções por padrão.
const DEFAULT_COUNT: &str = "20";
/// Máximo de instruções por execução.
const MAX_COUNT: u64 = 10_000;
/// Bytes lidos por instrução pedida (tamanho máximo de uma instrução x86).
const BYTES_PER_INSN: u64 = 15;
/// Largura da coluna de bytes: 15 bytes em hex com espaços.
const BYTES_COLUMN: usize = BYTES_PER_INSN as usize * 3 - 1;

/// Valor padrão de `-n`, para o clap.
pub const fn default_count() -> &'static str {
    DEFAULT_COUNT
}

/// Saída `--json` do `disasm`.
#[derive(Serialize)]
struct DisasmOutput {
    instructions: Vec<InsnOutput>,
    /// Endereço da primeira lacuna ilegível, que interrompeu a desmontagem.
    stopped_at_hole: Option<String>,
}

#[derive(Serialize)]
struct InsnOutput {
    address: String,
    label: Option<String>,
    bytes_hex: String,
    text: String,
    target: Option<String>,
    target_label: Option<String>,
}

pub fn run(global: &Global, address: &str, count: Option<&str>) -> anyhow::Result<()> {
    let spec = parse_addr(address)?;
    let count = parse_size(count.unwrap_or(DEFAULT_COUNT))?;
    if count > MAX_COUNT {
        bail!("contagem acima do limite de {MAX_COUNT}");
    }
    if count == 0 {
        bail!("a contagem precisa ser maior que zero");
    }
    let len = count
        .checked_mul(BYTES_PER_INSN)
        .ok_or_else(|| anyhow!("contagem acima do limite de {MAX_COUNT}"))?;

    let target = Target::open(global.pid)?;
    let addr = target.resolve(&spec, &global.module)?;
    guard::check(&target.maps, &target.modules, &target.non_pe, addr, len)?;
    // Cabe em `usize`: `len <= MAX_COUNT * 15`.
    let read = target.reader.read_lossy(addr, len as usize)?;

    // Bytes zerados no lugar de uma página ilegível virariam instruções falsas: a desmontagem
    // para na primeira lacuna.
    let readable = match read.holes.first() {
        Some(hole) => (hole.start - addr) as usize,
        None => read.bytes.len(),
    };
    let insns = disassemble(&read.bytes[..readable], addr, count as usize);
    let output = DisasmOutput {
        instructions: insns
            .iter()
            .map(|insn| insn_output(&target, insn))
            .collect(),
        stopped_at_hole: read.holes.first().map(|hole| format!("{:#x}", hole.start)),
    };
    emit(
        global.json,
        |out| write_text(out, &target, &insns, &output),
        &output,
    )
}

fn insn_output(target: &Target, insn: &Insn) -> InsnOutput {
    InsnOutput {
        address: format!("{:#x}", insn.addr),
        label: target.module_label(insn.addr),
        bytes_hex: insn.bytes.iter().map(|b| format!("{b:02x}")).collect(),
        text: insn.text.clone(),
        target: insn.target.map(|t| format!("{t:#x}")),
        target_label: insn.target.and_then(|t| target.module_label(t)),
    }
}

fn write_text(
    out: &mut dyn Write,
    target: &Target,
    insns: &[Insn],
    output: &DisasmOutput,
) -> std::io::Result<()> {
    for insn in insns {
        let bytes: Vec<String> = insn.bytes.iter().map(|b| format!("{b:02x}")).collect();
        write!(
            out,
            "{}  {:<width$}  {}",
            target.label(insn.addr),
            bytes.join(" "),
            insn.text,
            width = BYTES_COLUMN
        )?;
        if let Some(label) = insn.target.and_then(|t| target.module_label(t)) {
            write!(out, "  → {label}")?;
        }
        writeln!(out)?;
    }
    if let Some(hole) = &output.stopped_at_hole {
        writeln!(
            out,
            "lacuna ilegível a partir de {hole}: desmontagem interrompida"
        )?;
    }
    Ok(())
}
