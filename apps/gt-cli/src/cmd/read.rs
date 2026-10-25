//! `gt read <endereço> [tamanho]`: hexdump da memória do processo (spec 6.4).

use std::io::Write;
use std::ops::Range;

use anyhow::bail;
use gt_core::addr::{parse_addr, parse_size};
use gt_core::guard;
use serde::Serialize;

use super::Global;
use crate::context::Target;
use crate::output::{emit, hexdump};

/// Tamanho padrão da leitura, em bytes.
const DEFAULT_SIZE: u64 = 64;
/// Limite de uma leitura: 1 MiB.
const MAX_SIZE: u64 = 1 << 20;

/// Saída `--json` do `read`.
#[derive(Serialize)]
struct ReadOutput {
    address: String,
    label: Option<String>,
    size: usize,
    bytes_hex: String,
    holes: Vec<HoleOutput>,
}

#[derive(Serialize)]
struct HoleOutput {
    start: String,
    end: String,
}

pub fn run(global: &Global, address: &str, size: Option<&str>) -> anyhow::Result<()> {
    let spec = parse_addr(address)?;
    let size = size.map_or(Ok(DEFAULT_SIZE), parse_size)?;
    if size > MAX_SIZE {
        bail!("tamanho acima do limite de 1 MiB");
    }
    if size == 0 {
        bail!("o tamanho precisa ser maior que zero");
    }
    // Cabe em `usize`: `size <= MAX_SIZE`.
    let len = size as usize;

    let target = Target::open(global.pid)?;
    let addr = target.resolve(&spec, &global.module)?;
    guard::check(&target.maps, &target.modules, &target.non_pe, addr, size)?;
    let read = target.reader.read_lossy(addr, len)?;

    let output = ReadOutput {
        address: format!("{addr:#x}"),
        label: target.module_label(addr),
        size: read.bytes.len(),
        bytes_hex: read.bytes.iter().map(|b| format!("{b:02x}")).collect(),
        holes: read
            .holes
            .iter()
            .map(|hole| HoleOutput {
                start: format!("{:#x}", hole.start),
                end: format!("{:#x}", hole.end),
            })
            .collect(),
    };
    emit(
        global.json,
        |out| write_text(out, &target, addr, &read.bytes, &read.holes),
        &output,
    )
}

fn write_text(
    out: &mut dyn Write,
    target: &Target,
    addr: u64,
    bytes: &[u8],
    holes: &[Range<u64>],
) -> std::io::Result<()> {
    write!(out, "{}", hexdump(addr, bytes, holes))?;
    for hole in holes {
        writeln!(
            out,
            "lacuna ilegível (??): {:#x}..{:#x}",
            hole.start, hole.end
        )?;
    }
    if target.module_label(addr).is_some() {
        writeln!(out, "origem: {}", target.label(addr))?;
    }
    Ok(())
}
