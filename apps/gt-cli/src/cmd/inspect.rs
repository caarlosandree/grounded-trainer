//! `gt inspect`: processo, módulo, versão e seções (spec 6.3).

use std::io::Write;

use gt_core::process::ptrace_scope;
use serde::Serialize;

use super::Global;
use crate::context::Target;
use crate::output::emit;

#[derive(Serialize)]
struct InspectOutput {
    pid: i32,
    discarded: Vec<DiscardedOutput>,
    module: ModuleOutput,
    version_key: String,
    product_version: Option<String>,
    sections: Vec<SectionOutput>,
    ptrace_scope: Option<u8>,
    memory_readable: bool,
}

#[derive(Serialize)]
struct DiscardedOutput {
    pid: i32,
    reason: String,
}

#[derive(Serialize)]
struct ModuleOutput {
    name: String,
    path: String,
    base: String,
    image_base: String,
    size_of_image: u32,
}

#[derive(Serialize)]
struct SectionOutput {
    name: String,
    address: String,
    virtual_size: u32,
    raw_size: u32,
    perms: String,
}

pub fn run(global: &Global) -> anyhow::Result<()> {
    let target = Target::open(global.pid)?;
    let (module, headers) = target.module(&global.module)?;

    // Sem a imagem em disco só falta o `ProductVersion`; o `inspect` não falha por isso. Módulo de
    // terceiros só tem o cabeçalho lido (spec 6.12): a imagem dele não é aberta nem realocada.
    let product_version = if module.trusted {
        match target.open_disk(module, &headers) {
            Ok(disk) => disk.product_version(),
            Err(err) => {
                tracing::debug!(error = %err, "ProductVersion indisponível");
                None
            }
        }
    } else {
        None
    };
    let memory_readable = target.reader.read_exact(module.base, &mut [0u8; 1]).is_ok();

    let output = InspectOutput {
        pid: target.pid,
        discarded: target
            .discarded
            .iter()
            .map(|(pid, reason)| DiscardedOutput {
                pid: *pid,
                reason: reason.clone(),
            })
            .collect(),
        module: ModuleOutput {
            name: module.name.clone(),
            path: module.path.display().to_string(),
            base: format!("{:#x}", module.base),
            image_base: format!("{:#x}", headers.image_base),
            size_of_image: headers.size_of_image,
        },
        version_key: headers.version_key(),
        product_version,
        sections: headers
            .sections
            .iter()
            .map(|s| SectionOutput {
                name: s.name.clone(),
                address: format!("{:#x}", module.base.saturating_add(u64::from(s.rva))),
                virtual_size: s.virtual_size,
                raw_size: s.raw_size,
                perms: s.perms_string(),
            })
            .collect(),
        ptrace_scope: ptrace_scope(),
        memory_readable,
    };
    emit(global.json, |out| write_text(out, &output), &output)
}

fn write_text(out: &mut dyn Write, o: &InspectOutput) -> std::io::Result<()> {
    writeln!(out, "Processo: {}", o.pid)?;
    if o.discarded.is_empty() {
        writeln!(out, "Descartados: nenhum")?;
    } else {
        writeln!(out, "Descartados:")?;
        for d in &o.discarded {
            writeln!(out, "  {}  {}", d.pid, d.reason)?;
        }
    }
    writeln!(out, "Módulo: {}", o.module.name)?;
    writeln!(out, "Caminho: {}", o.module.path)?;
    writeln!(out, "Base: {}", o.module.base)?;
    writeln!(out, "ImageBase: {}", o.module.image_base)?;
    writeln!(
        out,
        "SizeOfImage: {:#x} ({} bytes)",
        o.module.size_of_image, o.module.size_of_image
    )?;
    writeln!(out, "Chave de versão: {}", o.version_key)?;
    writeln!(
        out,
        "ProductVersion: {}",
        o.product_version.as_deref().unwrap_or("indisponível")
    )?;
    writeln!(out, "Seções:")?;
    writeln!(
        out,
        "  {:<10} {:<20} {:>12} {:>12}  perms",
        "nome", "endereço", "VirtualSize", "SizeOfRaw"
    )?;
    for s in &o.sections {
        writeln!(
            out,
            "  {:<10} {:<20} {:>12} {:>12}  {}",
            s.name,
            s.address,
            format!("{:#x}", s.virtual_size),
            format!("{:#x}", s.raw_size),
            s.perms
        )?;
    }
    match o.ptrace_scope {
        Some(scope) => writeln!(out, "ptrace_scope: {scope}")?,
        None => writeln!(out, "ptrace_scope: Yama ausente")?,
    }
    writeln!(
        out,
        "Leitura de memória: {}",
        if o.memory_readable { "ok" } else { "falhou" }
    )
}
