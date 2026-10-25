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
    /// `SizeOfImage` do arquivo em disco; só existe para módulo confiável com o disco lido.
    disk_size_of_image: Option<u32>,
    /// `true` se a extensão do módulo foi conferida com o disco (spec 002, 6.12).
    extent_checked: bool,
    /// `false` para módulo de terceiros, cujo disco não é aberto.
    trusted: bool,
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
        disk_size_of_image: module.disk_size.and_then(|size| u32::try_from(size).ok()),
        extent_checked: module.extent_checked(),
        trusted: module.trusted,
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
    if let Some(disk) = o.disk_size_of_image
        && disk != o.module.size_of_image
    {
        writeln!(out, "SizeOfImage (disco): {disk:#x}")?;
    }
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
    let extent = if o.extent_checked {
        "conferida com o disco"
    } else if o.trusted {
        "não conferida (disco ilegível)"
    } else {
        "não conferida (módulo de terceiros)"
    };
    writeln!(out, "Extensão do módulo: {extent}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output() -> InspectOutput {
        InspectOutput {
            pid: 42,
            discarded: Vec::new(),
            module: ModuleOutput {
                name: "a.dll".into(),
                path: "/opt/a.dll".into(),
                base: "0x1000".into(),
                image_base: "0x1000".into(),
                size_of_image: 0x3000,
            },
            version_key: "00000000-00003000".into(),
            product_version: None,
            sections: Vec::new(),
            ptrace_scope: Some(0),
            disk_size_of_image: None,
            extent_checked: false,
            trusted: true,
        }
    }

    fn text(o: &InspectOutput) -> String {
        let mut buf = Vec::new();
        write_text(&mut buf, o).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn text_shows_extent_status_and_disk_size() {
        // Disco ilegível: sem linha do disco.
        let t = text(&output());
        assert!(t.contains("Extensão do módulo: não conferida (disco ilegível)"));
        assert!(!t.contains("SizeOfImage (disco)"));
        assert!(!t.contains("Leitura de memória"));

        // Conferida e diferente: a linha do disco vem logo após a do SizeOfImage.
        let t = text(&InspectOutput {
            disk_size_of_image: Some(0x2000),
            extent_checked: true,
            ..output()
        });
        assert!(t.contains("Extensão do módulo: conferida com o disco"));
        assert!(t.contains("SizeOfImage (disco): 0x2000"));
        let lines: Vec<_> = t.lines().collect();
        let at = lines
            .iter()
            .position(|l| l.starts_with("SizeOfImage:"))
            .unwrap();
        assert!(lines[at + 1].starts_with("SizeOfImage (disco): 0x2000"));

        // Conferida e igual: a linha do disco não aparece.
        let t = text(&InspectOutput {
            disk_size_of_image: Some(0x3000),
            extent_checked: true,
            ..output()
        });
        assert!(!t.contains("SizeOfImage (disco)"));

        // Terceiros: o disco não é aberto.
        let t = text(&InspectOutput {
            trusted: false,
            ..output()
        });
        assert!(t.contains("Extensão do módulo: não conferida (módulo de terceiros)"));
    }
}
