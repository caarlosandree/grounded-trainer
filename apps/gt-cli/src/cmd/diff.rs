//! `gt diff`: compara o código na memória com o executável em disco (spec 6.7).

use std::io::Write;

use anyhow::{anyhow, bail};
use gt_core::Error;
use gt_core::asm::{Insn, disassemble_around};
use gt_core::guard;
use gt_core::journal::Journal;
use gt_core::mem::LossyRead;
use gt_core::modules::require_trusted;
use gt_core::pe::Section;
use serde::Serialize;

use super::Global;
use super::compare::{
    AsmLine, BranchOutput, Chunk, asm_lines, branch_output, compare_section, hex, hex_spaced,
};
use super::ours;
use crate::context::Target;
use crate::output::emit;

/// Tamanho dos blocos lidos da memória: 4 MiB.
const BLOCK: usize = 4 << 20;
/// Instruções de contexto antes e depois do trecho na desmontagem.
const CONTEXT: usize = 3;
/// Bytes antes do trecho que a desmontagem precisa para se alinhar (`disassemble_around`).
const LOOKBACK: u64 = 64;
/// Bytes depois do trecho lidos para o contexto posterior.
const LOOKAHEAD: u64 = (CONTEXT as u64) * 15 + 16;
/// Só os primeiros bytes de um trecho são desmontados.
const MAX_ASM_SPAN: u64 = 256;
/// Só os primeiros bytes de um trecho vão para os campos hexadecimais.
const MAX_HEX_BYTES: usize = 256;

/// Aviso da spec 6.7 para seções que não são código.
const NON_EXEC_WARNING: &str = "fora das seções executáveis (.rdata, .data) as diferenças são \
     normais (tabela de importação preenchida, dados do jogo) e não indicam outro trainer";

/// Opções do `diff` além das globais.
pub struct Options<'a> {
    pub section: Option<&'a str>,
}

/// Saída `--json` do `diff`.
///
/// `section` é o nome da seção (várias, separadas por vírgula). Cada trecho traz o endereço, o
/// rótulo `módulo+offset`, o tamanho real em `len`, os bytes originais (disco) e atuais
/// (memória) em hexadecimal, limitados a 256 bytes (`hex_truncated`), a desmontagem dos dois
/// lados e, se o início do trecho atual é um desvio, o destino. `unreadable` marca trechos que
/// tocam páginas ilegíveis da memória (`current_hex` vazio quando é só lacuna). `ours` lista os
/// `id` das entradas do diário (do mesmo processo) que explicam todas as diferenças do trecho
/// (spec 008, 6.10); `null` se o trecho tem alguma diferença que não é nossa.
#[derive(Serialize)]
struct DiffOutput {
    section: String,
    warning: Option<String>,
    chunks: Vec<ChunkOutput>,
    total: usize,
}

#[derive(Serialize)]
struct ChunkOutput {
    address: String,
    label: Option<String>,
    len: usize,
    unreadable: bool,
    original_hex: String,
    current_hex: String,
    hex_truncated: bool,
    original_asm: Vec<AsmLine>,
    current_asm: Vec<AsmLine>,
    branch: Option<BranchOutput>,
    ours: Option<Vec<String>>,
}

/// O que o modo legível mostra a mais que o JSON (instruções completas e o texto do desvio).
struct TextExtra {
    original_insns: Vec<Insn>,
    current_insns: Vec<Insn>,
    branch_text: Option<String>,
}

pub fn run(global: &Global, opts: &Options<'_>) -> anyhow::Result<()> {
    let target = Target::open(global.pid)?;
    // O diário vem sem a trava (só leitura) e só vale se for do mesmo processo.
    let diary = ours::read_default(&target.identity);
    let (output, extras) = build(&target, &global.module, opts, diary.journal())?;
    emit(
        global.json,
        |out| write_text(out, &output, &extras),
        &output,
    )
}

/// Compara as seções de `module_name` com o disco e marca como nossas as diferenças que o `journal`
/// (do mesmo processo) explica.
fn build(
    target: &Target,
    module_name: &str,
    opts: &Options<'_>,
    journal: Option<&Journal>,
) -> anyhow::Result<(DiffOutput, Vec<TextExtra>)> {
    // O módulo vem antes de tudo: num alvo que não é PE, o erro é "não é uma imagem PE".
    let (module, headers) = target.module(module_name)?;
    require_trusted(module)?;

    let sections = select_sections(&headers.sections, opts.section)?;
    let disk = target.open_disk(module, &headers)?;

    let mut chunk_outputs: Vec<ChunkOutput> = Vec::new();
    let mut extras: Vec<TextExtra> = Vec::new();
    for section in &sections {
        let start = module.base.saturating_add(u64::from(section.rva));
        let size = u64::from(section.virtual_size);
        let disk_bytes = disk
            .bytes(section.rva, section.virtual_size as usize)
            .ok_or_else(|| {
                anyhow!(
                    "a seção {} sai dos limites da imagem em disco",
                    section.name
                )
            })?;
        guard::check(&target.maps, &target.modules, &target.non_pe, start, size)?;
        let chunks = compare_section(start, disk_bytes, BLOCK, |addr, len| {
            match target.reader.read_lossy(addr, len) {
                // Bloco inteiro ilegível: tudo é lacuna.
                #[allow(clippy::single_range_in_vec_init)]
                Err(Error::Unmapped { .. }) => Ok(LossyRead {
                    bytes: vec![0; len],
                    holes: vec![addr..addr + len as u64],
                }),
                other => other,
            }
        })?;
        for chunk in &chunks {
            let (json, extra) = render(target, chunk, start, disk_bytes, start + size, journal)?;
            chunk_outputs.push(json);
            extras.push(extra);
        }
    }

    let output = DiffOutput {
        section: sections
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        warning: sections
            .iter()
            .any(|s| !s.is_executable())
            .then(|| NON_EXEC_WARNING.to_string()),
        total: chunk_outputs.len(),
        chunks: chunk_outputs,
    };
    Ok((output, extras))
}

/// Seções a comparar: a pedida ou as executáveis.
fn select_sections<'a>(
    all_sections: &'a [Section],
    name: Option<&str>,
) -> anyhow::Result<Vec<&'a Section>> {
    let chosen: Vec<&Section> = match name {
        Some(name) => vec![
            all_sections
                .iter()
                .find(|s| s.name == name)
                .ok_or_else(|| {
                    anyhow!("seção {name} não encontrada; `gt inspect` lista as seções")
                })?,
        ],
        None => all_sections.iter().filter(|s| s.is_executable()).collect(),
    };
    let chosen: Vec<&Section> = chosen.into_iter().filter(|s| s.virtual_size > 0).collect();
    if chosen.is_empty() {
        bail!("nenhuma seção para comparar; use --section NOME");
    }
    Ok(chosen)
}

fn render(
    target: &Target,
    chunk: &Chunk,
    section_start: u64,
    section_disk: &[u8],
    section_end: u64,
    journal: Option<&Journal>,
) -> anyhow::Result<(ChunkOutput, TextExtra)> {
    let span_end = chunk.end().min(chunk.addr + MAX_ASM_SPAN);
    let pure_hole = chunk.unreadable && chunk.current.is_empty();

    let (original_insns, current_insns) = if pure_hole {
        (Vec::new(), Vec::new())
    } else {
        let original =
            disassemble_around(section_disk, section_start, chunk.addr, span_end, CONTEXT);
        let current = if chunk.unreadable {
            Vec::new()
        } else {
            current_insns(target, chunk.addr, span_end, section_start, section_end)?
        };
        (original, current)
    };

    let (branch, branch_text) = match chunk.branch {
        Some(branch) => {
            let (out, text) = branch_output(target, branch);
            (Some(out), Some(text))
        }
        None => (None, None),
    };
    let capped = |bytes: &[u8]| hex(&bytes[..bytes.len().min(MAX_HEX_BYTES)]);
    let json = ChunkOutput {
        address: format!("{:#x}", chunk.addr),
        label: target.module_label(chunk.addr),
        len: chunk.len(),
        unreadable: chunk.unreadable,
        original_hex: capped(&chunk.original),
        current_hex: capped(&chunk.current),
        hex_truncated: chunk.len() > MAX_HEX_BYTES,
        original_asm: asm_lines(&original_insns),
        current_asm: asm_lines(&current_insns),
        branch,
        ours: journal.and_then(|journal| explained_by(journal, chunk)),
    };
    Ok((
        json,
        TextExtra {
            original_insns,
            current_insns,
            branch_text,
        },
    ))
}

/// Desmonta o código atual (da memória) em volta do trecho. Janela dentro da seção, liberada pela
/// guarda; vazia se a guarda recusar ou se a janela tiver páginas ilegíveis.
fn current_insns(
    target: &Target,
    addr: u64,
    span_end: u64,
    section_start: u64,
    section_end: u64,
) -> anyhow::Result<Vec<Insn>> {
    let lo = addr.saturating_sub(LOOKBACK).max(section_start);
    let hi = span_end.saturating_add(LOOKAHEAD).min(section_end);
    if hi <= lo {
        return Ok(Vec::new());
    }
    let len = hi - lo;
    if guard::check(&target.maps, &target.modules, &target.non_pe, lo, len).is_err() {
        return Ok(Vec::new());
    }
    // Cabe em `usize`: a janela tem poucas centenas de bytes.
    match target.reader.read_lossy(lo, len as usize) {
        Ok(read) if read.holes.is_empty() => {
            Ok(disassemble_around(&read.bytes, lo, addr, span_end, CONTEXT))
        }
        Ok(_) | Err(Error::Unmapped { .. }) => Ok(Vec::new()),
        Err(err) => Err(err.into()),
    }
}

fn write_text(
    out: &mut dyn Write,
    output: &DiffOutput,
    extras: &[TextExtra],
) -> std::io::Result<()> {
    if let Some(warning) = &output.warning {
        writeln!(out, "aviso: {warning}")?;
    }
    writeln!(out, "seção: {}", output.section)?;
    for (chunk, extra) in output.chunks.iter().zip(extras) {
        let label = chunk.label.as_deref().unwrap_or("fora de módulo");
        writeln!(out)?;
        writeln!(
            out,
            "{} ({label})  {} bytes{}",
            chunk.address,
            chunk.len,
            if chunk.unreadable {
                "  [ilegível na memória]"
            } else {
                ""
            }
        )?;
        if let Some(ids) = &chunk.ours {
            writeln!(out, "  nosso ({})", ids.join(", "))?;
        }
        let more = if chunk.hex_truncated { " ..." } else { "" };
        writeln!(out, "  original: {}{more}", spaced(&chunk.original_hex))?;
        if chunk.unreadable && chunk.current_hex.is_empty() {
            writeln!(out, "  atual:    ilegível")?;
        } else {
            writeln!(out, "  atual:    {}{more}", spaced(&chunk.current_hex))?;
        }
        write_insns(out, "original", &extra.original_insns)?;
        write_insns(out, "atual", &extra.current_insns)?;
        if let Some(text) = &extra.branch_text {
            writeln!(out, "  desvio: {text}")?;
        }
    }
    writeln!(out)?;
    if output.total == 0 {
        writeln!(out, "nenhuma alteração")?;
    } else {
        let ours = output.chunks.iter().filter(|c| c.ours.is_some()).count();
        if ours == 0 {
            writeln!(out, "Total: {} trecho(s)", output.total)?;
        } else {
            writeln!(
                out,
                "Total: {} trecho(s), {ours} nosso(s) (pelo diário)",
                output.total
            )?;
        }
    }
    Ok(())
}

/// Os `id` das entradas do `journal` que explicam **todas** as diferenças do trecho: cada byte
/// que difere do disco está dentro de uma entrada e é um dos bytes que ela escreveu (os
/// `replacement`, ou os `next` de uma troca de valor em curso). `None` se o trecho é ilegível, não
/// tem diferença ou tem alguma que não é nossa (outro trainer mexeu no mesmo lugar).
fn explained_by(journal: &Journal, chunk: &Chunk) -> Option<Vec<String>> {
    if chunk.unreadable || chunk.current.len() != chunk.original.len() {
        return None;
    }
    let mut ids: Vec<String> = Vec::new();
    let mut any = false;
    for (i, (orig, now)) in chunk.original.iter().zip(&chunk.current).enumerate() {
        if orig == now {
            continue;
        }
        any = true;
        let addr = chunk.addr + i as u64;
        let entry = journal.entries.iter().find(|e| {
            let Some(at) = addr
                .checked_sub(e.addr)
                .and_then(|d| usize::try_from(d).ok())
            else {
                return false;
            };
            let writes = |bytes: &[u8]| bytes.get(at) == Some(now);
            writes(&e.replacement) || e.next.as_deref().is_some_and(writes)
        })?;
        if !ids.contains(&entry.id) {
            ids.push(entry.id.clone());
        }
    }
    any.then_some(ids)
}

fn write_insns(out: &mut dyn Write, title: &str, insns: &[Insn]) -> std::io::Result<()> {
    if insns.is_empty() {
        return Ok(());
    }
    writeln!(out, "  desmontagem ({title}):")?;
    for insn in insns {
        writeln!(
            out,
            "    {:#x}  {:<30}  {}",
            insn.addr,
            hex_spaced(&insn.bytes),
            insn.text
        )?;
    }
    Ok(())
}

/// `aabbcc` → `aa bb cc`.
fn spaced(compact: &str) -> String {
    compact
        .as_bytes()
        .chunks(2)
        .map(|pair| String::from_utf8_lossy(pair).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use gt_core::journal::{JournalEntry, JournalStore, Loaded};
    use gt_core::mem::MemWriter;
    use gt_grounded2::caves::Register;
    use gt_testkit::rig::{MODULE_NAME, SynthRig};

    use super::*;

    /// O diário do `rig` como o `gt diff` o lê: sem a trava, com a identidade conferida.
    fn diary(rig: &SynthRig) -> ours::Diary {
        ours::read(&rig.store, &rig.target.identity)
    }

    fn only_text(rig: &SynthRig) -> (DiffOutput, Vec<TextExtra>) {
        let diary = diary(rig);
        build(
            &rig.target,
            MODULE_NAME,
            &Options {
                section: Some(".text"),
            },
            diary.journal(),
        )
        .unwrap()
    }

    #[test]
    fn diff_marks_ours() {
        let rig = SynthRig::new();
        let mut engine = rig.engine();
        let mut alloc = rig.cave_allocator();
        let hook = rig.install_observe(&mut engine, &mut alloc, "obs", "sum", Register::RCX);
        // Outro trainer troca um byte no meio de `getter`: fora do diário.
        let foreign = rig.t.symbol("getter") + 9;
        let mut before = [0u8; 1];
        rig.target.reader.read_exact(foreign, &mut before).unwrap();
        MemWriter::open(rig.t.pid)
            .unwrap()
            .write(foreign, &[before[0] ^ 0xFF])
            .unwrap();

        let (output, extras) = only_text(&rig);
        let at = |addr: u64| {
            output
                .chunks
                .iter()
                .find(|c| c.address == format!("{addr:#x}"))
                .unwrap_or_else(|| panic!("sem trecho em {addr:#x}"))
        };
        // O desvio (5 bytes) e a cave (do `CC` ao código) saem como nossos, com o `id` do diário.
        assert_eq!(at(hook.hook_at).ours, Some(vec![hook.hook_id.clone()]));
        assert_eq!(at(hook.cave_at).ours, Some(vec![hook.cave_id.clone()]));
        assert_eq!(at(hook.hook_at).len, hook.hook_len);
        // A alteração de outro trainer não é marcada.
        assert_eq!(at(foreign).ours, None);
        assert_eq!(output.total, 3);

        let mut text = Vec::new();
        write_text(&mut text, &output, &extras).unwrap();
        let text = String::from_utf8(text).unwrap();
        assert!(
            text.contains(&format!("nosso ({})", hook.hook_id)),
            "{text}"
        );
        assert!(
            text.contains(&format!("nosso ({})", hook.cave_id)),
            "{text}"
        );
        assert_eq!(text.matches("nosso (").count(), 2, "{text}");
        assert!(text.contains("2 nosso(s) (pelo diário)"), "{text}");

        // Outro trainer escreve por cima do nosso desvio: o trecho deixa de ser explicado.
        MemWriter::open(rig.t.pid)
            .unwrap()
            .write(hook.hook_at + 1, &[0x12])
            .unwrap();
        let (output, _) = only_text(&rig);
        let changed = output
            .chunks
            .iter()
            .find(|c| c.address == format!("{:#x}", hook.hook_at))
            .unwrap();
        assert_eq!(changed.ours, None);
        // Devolvemos o byte antes de sair, para o motor desfazer sem recusar.
        let our_byte = hook_bytes_of(&rig, &hook);
        MemWriter::open(rig.t.pid)
            .unwrap()
            .write(hook.hook_at, &our_byte)
            .unwrap();

        // Sem diário (ou de outro processo), nada é marcado.
        let (output, _) = build(
            &rig.target,
            MODULE_NAME,
            &Options {
                section: Some(".text"),
            },
            None,
        )
        .unwrap();
        assert!(output.chunks.iter().all(|c| c.ours.is_none()));
        let mut other = match rig.store.peek().unwrap() {
            Loaded::Ok(journal) => journal,
            other => panic!("{other:?}"),
        };
        other.pid += 1;
        let elsewhere = tempfile::tempdir().unwrap();
        let store = JournalStore::new(elsewhere.path().join("state"));
        store.save(&other).unwrap();
        assert_eq!(
            ours::read(&store, &rig.target.identity),
            ours::Diary::OtherProcess
        );
        drop(engine);
    }

    /// Os bytes do desvio que o motor escreveu (do diário).
    fn hook_bytes_of(rig: &SynthRig, hook: &gt_testkit::rig::InstalledHook) -> Vec<u8> {
        match rig.store.peek().unwrap() {
            Loaded::Ok(journal) => journal
                .entries
                .iter()
                .find(|e: &&JournalEntry| e.id == hook.hook_id)
                .unwrap()
                .replacement
                .clone(),
            other => panic!("{other:?}"),
        }
    }

    fn render_text(output: &DiffOutput, extras: &[TextExtra]) -> String {
        let mut buf = Vec::new();
        write_text(&mut buf, output, extras).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn no_chunks_says_no_changes() {
        let output = DiffOutput {
            section: ".text".into(),
            warning: None,
            chunks: Vec::new(),
            total: 0,
        };
        let text = render_text(&output, &[]);
        assert!(text.contains("nenhuma alteração"), "{text}");
        assert!(!text.contains("aviso"), "{text}");
    }

    #[test]
    fn warning_comes_first_and_chunk_is_listed() {
        let output = DiffOutput {
            section: ".rdata".into(),
            warning: Some(NON_EXEC_WARNING.into()),
            chunks: vec![ChunkOutput {
                address: "0x1457bd6d0".into(),
                label: Some("Grounded2Steam-Win64-Shipping.exe+57bd6d0".into()),
                len: 2,
                unreadable: false,
                original_hex: "f30f".into(),
                current_hex: "e94b".into(),
                hex_truncated: false,
                original_asm: Vec::new(),
                current_asm: Vec::new(),
                branch: None,
                ours: None,
            }],
            total: 1,
        };
        let extras = [TextExtra {
            original_insns: Vec::new(),
            current_insns: Vec::new(),
            branch_text: Some("0xcc280020 → fora de todos os módulos".into()),
        }];
        let text = render_text(&output, &extras);
        assert!(
            text.starts_with("aviso: fora das seções executáveis"),
            "{text}"
        );
        assert!(text.contains("original: f3 0f"), "{text}");
        assert!(text.contains("atual:    e9 4b"), "{text}");
        assert!(text.contains("desvio: 0xcc280020"), "{text}");
        assert!(text.contains("Total: 1 trecho(s)"), "{text}");
    }
}
