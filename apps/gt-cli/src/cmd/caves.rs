//! `gt caves`: as sequências de `CC` da `.text` (onde as caves moram), a folga da `.data` (onde ficam as
//! variáveis) e, pelo diário, o que é nosso (spec 008, 6.10). Só lê: o diário vem sem a trava e só vale
//! se é do mesmo processo ([`super::ours`]).

use std::io::Write;

use anyhow::Context;
use gt_core::cave::cc_runs;
use gt_core::guard;
use gt_core::journal::{Journal, JournalEntry};
use gt_core::modules::require_trusted;
use gt_core::patch::{CAVE_FILL, PatchKind};
use gt_grounded2::policy::var_region;
use serde::Serialize;

use super::Global;
use super::ours::{self, Diary};
use crate::context::Target;
use crate::output::emit;

/// Menor sequência de `CC` listada (o padrão): o alocador de caves precisa de 16 de margem mais a cave.
const MIN_RUN: usize = 64;

/// Opções do `caves` além das globais.
pub struct Options {
    /// Tamanho mínimo, em bytes, das sequências de `CC` listadas.
    pub min_len: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self { min_len: MIN_RUN }
    }
}

/// Uma sequência de `CC` do disco, com o que a memória tem nela agora.
#[derive(Debug, Serialize)]
struct RunOutput {
    address: String,
    label: Option<String>,
    len: usize,
    /// Bytes da memória que já não são `CC` (código nosso ou de outro); `null` se ilegível.
    used: Option<usize>,
    /// Caves nossas (do diário) que tocam a sequência.
    ours: Vec<String>,
}

/// A folga da `.data` depois do `VirtualSize`, onde ficam as variáveis das caves.
#[derive(Debug, Serialize)]
struct SlackOutput {
    address: Option<String>,
    len: Option<u64>,
    /// Bytes da memória diferentes de zero; `null` se ilegível ou indisponível.
    used: Option<usize>,
    /// Motivo, quando a folga não serve (`indisponível (sem espaço para variáveis)`).
    unavailable: Option<String>,
}

/// Uma entrada do diário nossa: desvio, patch comum ou cave.
#[derive(Debug, Serialize)]
struct EntryOutput {
    /// `desvio` (salta para uma cave nossa), `patch` ou `cave`.
    kind: &'static str,
    id: String,
    address: String,
    label: Option<String>,
    len: usize,
    /// Para um desvio, o `id` da cave para onde ele salta.
    cave: Option<String>,
}

#[derive(Debug, Serialize)]
struct VarOutput {
    address: String,
    len: u64,
}

#[derive(Debug, Serialize)]
struct CavesOutput {
    module: String,
    section: String,
    min_len: usize,
    /// O que o diário disse sobre este processo.
    diary: String,
    runs: Vec<RunOutput>,
    slack: SlackOutput,
    entries: Vec<EntryOutput>,
    vars: Vec<VarOutput>,
}

pub fn run(global: &Global, opts: &Options) -> anyhow::Result<()> {
    let target = Target::open(global.pid)?;
    let diary = ours::read_default(&target.identity);
    let output = build(&target, &global.module, opts, &diary)?;
    emit(global.json, |out| write_text(out, &output), &output)
}

/// Monta o relatório de `module_name`: sequências de `CC` do disco com o uso na memória, a folga e o
/// que o diário (se for do mesmo processo) diz que é nosso.
fn build(
    target: &Target,
    module_name: &str,
    opts: &Options,
    diary: &Diary,
) -> anyhow::Result<CavesOutput> {
    let (module, headers) = target.module(module_name)?;
    require_trusted(module)?;
    let disk = target.open_disk(module, &headers)?;
    let text = disk
        .headers
        .section(".text")
        .context("o executável não tem a seção .text")?;
    let text_bytes = disk
        .bytes(text.rva, text.virtual_size as usize)
        .context("a seção .text sai dos limites da imagem em disco")?;
    let text_start = module.base + u64::from(text.rva);
    let journal = diary.journal();

    let runs = cc_runs(text_bytes, text_start, opts.min_len)
        .into_iter()
        .map(|run| {
            let len = (run.end - run.start) as usize;
            RunOutput {
                address: format!("{:#x}", run.start),
                label: target.module_label(run.start),
                len,
                used: read_used(target, run.start, len, |b| *b != CAVE_FILL),
                ours: journal
                    .map(|j| caves_in(j, run.start, run.end))
                    .unwrap_or_default(),
            }
        })
        .collect();

    let slack = match var_region(module, &disk.headers, &target.maps) {
        Some(region) => {
            let len = region.end - region.start;
            SlackOutput {
                address: Some(format!("{:#x}", region.start)),
                len: Some(len),
                used: read_used(target, region.start, len as usize, |b| *b != 0),
                unavailable: None,
            }
        }
        None => SlackOutput {
            address: None,
            len: None,
            used: None,
            unavailable: Some("indisponível (sem espaço para variáveis)".to_string()),
        },
    };

    let (entries, vars) = match journal {
        Some(journal) => (entries_of(target, journal), vars_of(journal)),
        None => (Vec::new(), Vec::new()),
    };
    Ok(CavesOutput {
        module: module.name.clone(),
        section: text.name.clone(),
        min_len: opts.min_len,
        diary: diary.note().to_string(),
        runs,
        slack,
        entries,
        vars,
    })
}

/// Quantos bytes de `addr..addr+len` da memória satisfazem `count`; `None` se a guarda recusa ou a
/// leitura falha.
fn read_used(target: &Target, addr: u64, len: usize, count: impl Fn(&u8) -> bool) -> Option<usize> {
    guard::check(
        &target.maps,
        &target.modules,
        &target.non_pe,
        addr,
        len as u64,
    )
    .ok()?;
    let mut buf = vec![0u8; len];
    target.reader.read_exact(addr, &mut buf).ok()?;
    Some(buf.iter().filter(|b| count(b)).count())
}

/// `id` das caves do diário que tocam `start..end`.
fn caves_in(journal: &Journal, start: u64, end: u64) -> Vec<String> {
    journal
        .entries
        .iter()
        .filter(|e| e.kind == PatchKind::Cave)
        .filter(|e| e.range().start < end && start < e.range().end)
        .map(|e| e.id.clone())
        .collect()
}

/// Destino de um `jmp rel32` (`E9`) gravado em `entry`, se for um.
fn jmp_target(entry: &JournalEntry) -> Option<u64> {
    let bytes = entry.replacement.get(..5)?;
    if bytes[0] != 0xE9 {
        return None;
    }
    let rel = i32::from_le_bytes(bytes[1..5].try_into().ok()?);
    Some(
        entry
            .addr
            .wrapping_add(5)
            .wrapping_add_signed(i64::from(rel)),
    )
}

fn entries_of(target: &Target, journal: &Journal) -> Vec<EntryOutput> {
    let cave_at = |addr: u64| {
        journal
            .entries
            .iter()
            .find(|c| c.kind == PatchKind::Cave && c.range().contains(&addr))
    };
    journal
        .entries
        .iter()
        .map(|e| {
            let (kind, cave) = match e.kind {
                PatchKind::Cave => ("cave", None),
                PatchKind::Code => match jmp_target(e).and_then(cave_at) {
                    Some(cave) => ("desvio", Some(cave.id.clone())),
                    None => ("patch", None),
                },
            };
            EntryOutput {
                kind,
                id: e.id.clone(),
                address: format!("{:#x}", e.addr),
                label: target.module_label(e.addr),
                len: e.original.len(),
                cave,
            }
        })
        .collect()
}

fn vars_of(journal: &Journal) -> Vec<VarOutput> {
    journal
        .vars
        .iter()
        .map(|v| VarOutput {
            address: format!("{:#x}", v.addr),
            len: v.len,
        })
        .collect()
}

fn write_text(out: &mut dyn Write, output: &CavesOutput) -> std::io::Result<()> {
    writeln!(out, "módulo: {}  ({})", output.module, output.diary)?;
    writeln!(
        out,
        "sequências de CC em {} (a partir de {} bytes): {}",
        output.section,
        output.min_len,
        output.runs.len()
    )?;
    for run in &output.runs {
        let label = run.label.as_deref().unwrap_or("fora de módulo");
        let used = run
            .used
            .map_or_else(|| "ilegível".to_string(), |n| format!("{n} byte(s)"));
        let ours = if run.ours.is_empty() {
            String::new()
        } else {
            format!("  cave nossa: {}", run.ours.join(", "))
        };
        writeln!(
            out,
            "  {} ({label})  {} bytes  em uso: {used}{ours}",
            run.address, run.len
        )?;
    }
    match (&output.slack.address, &output.slack.unavailable) {
        (Some(address), _) => {
            let used = output
                .slack
                .used
                .map_or_else(|| "ilegível".to_string(), |n| format!("{n} byte(s)"));
            writeln!(
                out,
                "folga da .data: {address}  {} bytes  em uso: {used}",
                output.slack.len.unwrap_or(0)
            )?;
        }
        (None, reason) => writeln!(
            out,
            "folga da .data: {}",
            reason.as_deref().unwrap_or("indisponível")
        )?,
    }
    if output.entries.is_empty() && output.vars.is_empty() {
        writeln!(out, "nosso, pelo diário: nada")?;
        return Ok(());
    }
    writeln!(out, "nosso, pelo diário:")?;
    for e in &output.entries {
        let label = e.label.as_deref().unwrap_or("fora de módulo");
        let to = e
            .cave
            .as_ref()
            .map_or_else(String::new, |cave| format!("  salta para a cave {cave}"));
        writeln!(
            out,
            "  {:<6} {}  {} ({label})  {} bytes{to}",
            e.kind, e.id, e.address, e.len
        )?;
    }
    for v in &output.vars {
        writeln!(out, "  variáveis  {}  {} bytes", v.address, v.len)?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use gt_grounded2::caves::Register;
    use gt_testkit::rig::{MODULE_NAME, SynthRig};

    use super::*;

    fn report(rig: &SynthRig) -> CavesOutput {
        let diary = ours::read(&rig.store, &rig.target.identity);
        build(&rig.target, MODULE_NAME, &Options::default(), &diary).unwrap()
    }

    #[test]
    fn caves_lists_runs_and_ours() {
        let rig = SynthRig::new();
        let before = report(&rig);
        // Sem nada instalado: as duas sequências do módulo sintético, intactas, e a folga vazia.
        let lens: Vec<usize> = before.runs.iter().map(|r| r.len).collect();
        assert_eq!(lens, [300, 70]);
        assert!(before.runs.iter().all(|r| r.used == Some(0)));
        assert!(before.runs.iter().all(|r| r.ours.is_empty()));
        assert_eq!(
            before.runs[0].address,
            format!("{:#x}", rig.t.symbol("cc300"))
        );
        assert_eq!(before.slack.len, Some(0x130));
        assert_eq!(before.slack.used, Some(0));
        assert_eq!(before.slack.unavailable, None);
        assert!(before.entries.is_empty() && before.vars.is_empty());
        assert_eq!(before.diary, Diary::Missing.note());

        let mut engine = rig.engine();
        let mut alloc = rig.cave_allocator();
        let hook = rig.install_observe(&mut engine, &mut alloc, "obs", "sum", Register::RCX);
        // Uma passagem pela cave grava o contador e o anel nas variáveis.
        let mut rig = rig;
        assert_eq!(rig.t.call("sum", &[2, 3]) as i32, 5);
        let after = report(&rig);
        let cc300 = &after.runs[0];
        assert_eq!(cc300.used, Some(hook.cave.len()));
        assert_eq!(cc300.ours, vec![hook.cave_id.clone()]);
        assert!(after.runs[1].ours.is_empty());
        assert_eq!(
            after.slack.address,
            Some(format!("{:#x}", rig.var_region().start))
        );
        assert!(after.slack.used.unwrap() > 0, "o anel guarda a passagem");
        assert_eq!(after.diary, "diário do processo atual");

        let by_id = |id: &str| after.entries.iter().find(|e| e.id == id).unwrap();
        let cave = by_id(&hook.cave_id);
        assert_eq!((cave.kind, cave.len), ("cave", hook.cave.len()));
        let desvio = by_id(&hook.hook_id);
        assert_eq!(desvio.kind, "desvio");
        assert_eq!(desvio.cave.as_deref(), Some(hook.cave_id.as_str()));
        assert_eq!(desvio.address, format!("{:#x}", hook.hook_at));
        assert_eq!(after.vars.len(), 1);
        assert_eq!(
            after.vars[0].len,
            rig.var_region().end - rig.var_region().start
        );

        let mut text = Vec::new();
        write_text(&mut text, &after).unwrap();
        let text = String::from_utf8(text).unwrap();
        assert!(text.contains("sequências de CC em .text"), "{text}");
        assert!(
            text.contains(&format!("cave nossa: {}", hook.cave_id)),
            "{text}"
        );
        assert!(text.contains("folga da .data:"), "{text}");
        assert!(
            text.contains(&format!("salta para a cave {}", hook.cave_id)),
            "{text}"
        );
        assert!(text.contains("variáveis"), "{text}");

        // A saída devolve tudo: o relatório volta ao de antes (a folga zerada, a cave apagada).
        engine.undo_all().unwrap();
        let end = report(&rig);
        assert!(
            end.runs
                .iter()
                .all(|r| r.used == Some(0) && r.ours.is_empty())
        );
        assert_eq!(end.slack.used, Some(0));
        assert!(end.entries.is_empty() && end.vars.is_empty());
    }

    #[test]
    fn caves_without_slack_says_so_and_min_filters_runs() {
        let rig = SynthRig::new();
        let diary = Diary::Missing;
        let only_long = build(&rig.target, MODULE_NAME, &Options { min_len: 100 }, &diary).unwrap();
        assert_eq!(only_long.runs.len(), 1);
        assert_eq!(only_long.runs[0].len, 300);
        // Módulo sem folga: o texto pede o motivo da spec.
        let mut broken = report(&rig);
        broken.slack = SlackOutput {
            address: None,
            len: None,
            used: None,
            unavailable: Some("indisponível (sem espaço para variáveis)".into()),
        };
        let mut text = Vec::new();
        write_text(&mut text, &broken).unwrap();
        let text = String::from_utf8(text).unwrap();
        assert!(
            text.contains("folga da .data: indisponível (sem espaço para variáveis)"),
            "{text}"
        );
        assert!(text.contains("nosso, pelo diário: nada"), "{text}");
    }
}
