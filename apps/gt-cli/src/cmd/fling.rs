//! `gt fling`: estado de cada ponto do `symbols.ini` do FLiNG (spec 6.9).

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};
use gt_core::Error;
use gt_core::asm::{Insn, branch_at, disassemble};
use gt_core::guard;
use gt_core::pe::DiskImage;
use gt_grounded2::fling::{
    CHANGE_WINDOW, FlingSymbol, ini_version_mismatch, is_changed, locate_ini, parse_symbols_ini,
};
use serde::Serialize;

use super::Global;
use super::compare::{AsmLine, BranchOutput, asm_lines, branch_output, hex_spaced};
use crate::context::Target;
use crate::output::emit;

/// Instruções desmontadas por símbolo.
const ASM_COUNT: usize = 3;
/// Bytes lidos para essas instruções (3 instruções x 15 bytes).
const ASM_BYTES: usize = ASM_COUNT * 15;

/// Nota final do modo legível (spec 6.9: o que "original" quer dizer).
const ORIGINAL_NOTE: &str = "original = sem alteração nos 16 bytes a partir do endereço";

/// Opções do `fling` além das globais.
pub struct Options<'a> {
    pub steam_library: Option<&'a Path>,
    pub ini: Option<&'a Path>,
}

/// Estado de um símbolo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Os 16 bytes da memória são iguais aos do disco.
    Original,
    /// Algum byte dos 16 difere do disco.
    Changed,
    /// A janela de 16 bytes não é toda legível na memória ou no disco.
    Unreadable,
    /// O módulo do símbolo não é confiável (spec 6.12): nada foi lido.
    Blocked,
    /// O módulo do símbolo não está carregado no processo.
    ModuleMissing,
}

impl State {
    /// Valor estável no JSON.
    fn key(self) -> &'static str {
        match self {
            State::Original => "original",
            State::Changed => "changed",
            State::Unreadable => "unreadable",
            State::Blocked => "blocked",
            State::ModuleMissing => "module_missing",
        }
    }

    /// Texto do modo legível.
    fn text(self) -> &'static str {
        match self {
            State::Original => "original",
            State::Changed => "alterado",
            State::Unreadable => "ilegível",
            State::Blocked => "bloqueado",
            State::ModuleMissing => "módulo ausente",
        }
    }
}

/// Compara a janela de [`CHANGE_WINDOW`] bytes da memória com a do disco.
///
/// Os dois lados precisam ter a janela inteira: `is_changed` compara só o prefixo comum, e uma
/// memória curta viraria "original". Janela incompleta é `Unreadable`, nunca `Original`.
fn window_state(memory: Option<&[u8]>, disk: Option<&[u8]>) -> State {
    match (memory, disk) {
        (Some(mem), Some(disk)) if mem.len() >= CHANGE_WINDOW && disk.len() >= CHANGE_WINDOW => {
            if is_changed(&mem[..CHANGE_WINDOW], &disk[..CHANGE_WINDOW]) {
                State::Changed
            } else {
                State::Original
            }
        }
        _ => State::Unreadable,
    }
}

/// Saída `--json` do `fling`.
///
/// `state` é `original`, `changed`, `unreadable`, `blocked` ou `module_missing`; `original`
/// significa só "sem alteração nos 16 bytes a partir do endereço". `warning` existe quando o
/// `--ini` é de outra versão do jogo.
#[derive(Serialize)]
struct FlingOutput {
    ini: String,
    warning: Option<String>,
    symbols: Vec<SymbolOutput>,
}

#[derive(Serialize)]
struct SymbolOutput {
    name: String,
    address: Option<String>,
    label: String,
    state: &'static str,
    asm: Vec<AsmLine>,
    branch: Option<BranchOutput>,
}

/// O que o modo legível mostra a mais que o JSON.
struct TextExtra {
    state: State,
    insns: Vec<Insn>,
    branch_text: Option<String>,
}

/// Módulo do `.ini` já resolvido, com a imagem em disco (aberta uma vez por módulo).
struct Resolved {
    base: u64,
    size: u64,
    trusted: bool,
    /// Só abre para módulo confiável: um módulo de terceiros nem chega a ser lido.
    disk: Option<DiskImage>,
}

pub fn run(global: &Global, opts: &Options<'_>) -> anyhow::Result<()> {
    let target = Target::open(global.pid)?;
    // O módulo vem antes de procurar o ini: num alvo que não é PE, o erro é "não é uma imagem PE".
    let (_, headers) = target.module(&global.module)?;
    let time_date_stamp = headers.time_date_stamp;

    let mut warning = None;
    let ini_path: PathBuf = match opts.ini {
        Some(ini) => {
            if ini_version_mismatch(ini, time_date_stamp) {
                let text = format!(
                    "a pasta de {} não é o TimeDateStamp do jogo aberto ({time_date_stamp}); os \
                     offsets podem ser de outra versão e dar resultados errados",
                    ini.display()
                );
                tracing::warn!("{text}");
                warning = Some(text);
            }
            ini.to_path_buf()
        }
        None => {
            let library = match opts.steam_library {
                Some(dir) => dir.to_path_buf(),
                None => default_library()?,
            };
            locate_ini(&library, time_date_stamp)?
        }
    };
    let raw = std::fs::read(&ini_path)
        .with_context(|| format!("não foi possível ler {}", ini_path.display()))?;
    let symbols = parse_symbols_ini(&raw)
        .with_context(|| format!("falha ao interpretar {}", ini_path.display()))?;

    let mut resolved: HashMap<String, Option<Resolved>> = HashMap::new();
    let mut outputs = Vec::with_capacity(symbols.len());
    let mut extras = Vec::with_capacity(symbols.len());
    for symbol in &symbols {
        let (output, extra) = inspect_symbol(&target, &mut resolved, symbol)?;
        outputs.push(output);
        extras.push(extra);
    }

    let output = FlingOutput {
        ini: ini_path.display().to_string(),
        warning,
        symbols: outputs,
    };
    emit(
        global.json,
        |out| write_text(out, &output, &extras),
        &output,
    )
}

/// `$HOME/.local/share/Steam/steamapps`.
fn default_library() -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .ok_or_else(|| anyhow!("HOME não definido; use --steam-library ou --ini"))?;
    Ok(PathBuf::from(home).join(".local/share/Steam/steamapps"))
}

/// Resolve o módulo do símbolo (pelo nome que vem do `.ini`) e abre a imagem em disco, uma vez.
fn resolve<'a>(
    target: &Target,
    cache: &'a mut HashMap<String, Option<Resolved>>,
    name: &str,
) -> anyhow::Result<Option<&'a Resolved>> {
    let key = name.to_ascii_lowercase();
    if !cache.contains_key(&key) {
        let entry = match target.module(name) {
            Ok((module, headers)) => {
                let disk = if module.trusted {
                    Some(target.open_disk(module, &headers)?)
                } else {
                    None
                };
                Some(Resolved {
                    base: module.base,
                    size: module.size,
                    trusted: module.trusted,
                    disk,
                })
            }
            Err(err) => match err.downcast_ref::<Error>() {
                Some(Error::ModuleNotFound { .. } | Error::NotPe { .. }) => None,
                _ => return Err(err),
            },
        };
        cache.insert(key.clone(), entry);
    }
    Ok(cache.get(&key).and_then(Option::as_ref))
}

fn inspect_symbol(
    target: &Target,
    cache: &mut HashMap<String, Option<Resolved>>,
    symbol: &FlingSymbol,
) -> anyhow::Result<(SymbolOutput, TextExtra)> {
    let label = format!("{}+{:x}", symbol.module, symbol.offset);
    let make = |address: Option<u64>,
                state: State,
                insns: Vec<Insn>,
                branch: Option<(BranchOutput, String)>| {
        let (branch, branch_text) = match branch {
            Some((out, text)) => (Some(out), Some(text)),
            None => (None, None),
        };
        (
            SymbolOutput {
                name: symbol.name.clone(),
                address: address.map(|a| format!("{a:#x}")),
                label: label.clone(),
                state: state.key(),
                asm: asm_lines(&insns),
                branch,
            },
            TextExtra {
                state,
                insns,
                branch_text,
            },
        )
    };

    let Some(module) = resolve(target, cache, &symbol.module)? else {
        return Ok(make(None, State::ModuleMissing, Vec::new(), None));
    };
    let Some(addr) = module.base.checked_add(symbol.offset) else {
        return Ok(make(None, State::Unreadable, Vec::new(), None));
    };
    if !module.trusted {
        return Ok(make(Some(addr), State::Blocked, Vec::new(), None));
    }
    // O ponto precisa caber no módulo e a guarda precisa liberar a janela inteira.
    let window = CHANGE_WINDOW as u64;
    if symbol.offset.saturating_add(window) > module.size {
        return Ok(make(Some(addr), State::Unreadable, Vec::new(), None));
    }
    if guard::check(&target.maps, &target.modules, &target.non_pe, addr, window).is_err() {
        return Ok(make(Some(addr), State::Blocked, Vec::new(), None));
    }

    // Janela de 16 bytes na memória: ou toda legível, ou "ilegível".
    let mut memory = [0u8; CHANGE_WINDOW];
    let memory_ok = match target.reader.read_exact(addr, &mut memory) {
        Ok(()) => true,
        Err(Error::Unmapped { .. }) => false,
        Err(err) => return Err(err.into()),
    };
    let disk_window = u32::try_from(symbol.offset)
        .ok()
        .zip(module.disk.as_ref())
        .and_then(|(rva, disk)| disk.bytes(rva, CHANGE_WINDOW));
    let state = window_state(memory_ok.then_some(&memory[..]), disk_window);

    // Desmontagem curta do código atual, se a guarda liberar a janela maior.
    let mut insns = Vec::new();
    let mut code = memory.to_vec();
    if memory_ok {
        let room = module
            .size
            .saturating_sub(symbol.offset)
            .min(ASM_BYTES as u64);
        // Cabe em `usize`: `room <= ASM_BYTES`.
        let want = room as usize;
        if want > CHANGE_WINDOW
            && guard::check(&target.maps, &target.modules, &target.non_pe, addr, room).is_ok()
        {
            match target.reader.read_lossy(addr, want) {
                Ok(read) => {
                    // Zeros no lugar de página ilegível virariam instruções falsas.
                    let readable = read
                        .holes
                        .first()
                        .map_or(read.bytes.len(), |hole| (hole.start - addr) as usize);
                    code = read.bytes[..readable].to_vec();
                }
                Err(Error::Unmapped { .. }) => {}
                Err(err) => return Err(err.into()),
            }
        }
        insns = disassemble(&code, addr, ASM_COUNT);
    }

    let branch = if state == State::Changed {
        branch_at(&code, addr).map(|branch| branch_output(target, branch))
    } else {
        None
    };
    Ok(make(Some(addr), state, insns, branch))
}

fn write_text(
    out: &mut dyn Write,
    output: &FlingOutput,
    extras: &[TextExtra],
) -> std::io::Result<()> {
    writeln!(out, "ini: {}", output.ini)?;
    if let Some(warning) = &output.warning {
        writeln!(out, "aviso: {warning}")?;
    }
    for (symbol, extra) in output.symbols.iter().zip(extras) {
        let address = symbol.address.as_deref().unwrap_or("?");
        writeln!(
            out,
            "{}  {address} ({})  {}",
            symbol.name,
            symbol.label,
            extra.state.text()
        )?;
        for insn in &extra.insns {
            writeln!(
                out,
                "    {:#x}  {:<30}  {}",
                insn.addr,
                hex_spaced(&insn.bytes),
                insn.text
            )?;
        }
        if let Some(text) = &extra.branch_text {
            writeln!(out, "    desvio: {text}")?;
        }
    }
    writeln!(out)?;
    writeln!(out, "{ORIGINAL_NOTE}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISK: [u8; 16] = [0x11; 16];

    #[test]
    fn equal_full_windows_are_original() {
        assert_eq!(window_state(Some(&DISK), Some(&DISK)), State::Original);
    }

    #[test]
    fn a_difference_in_the_window_is_changed() {
        let mut mem = DISK;
        mem[15] = 0;
        assert_eq!(window_state(Some(&mem), Some(&DISK)), State::Changed);
    }

    #[test]
    fn short_memory_is_unreadable_never_original() {
        // O prefixo curto é igual ao disco: `is_changed` diria "não alterado".
        assert_eq!(
            window_state(Some(&DISK[..8]), Some(&DISK)),
            State::Unreadable
        );
        assert_eq!(window_state(Some(&[]), Some(&DISK)), State::Unreadable);
    }

    #[test]
    fn missing_memory_or_disk_is_unreadable() {
        assert_eq!(window_state(None, Some(&DISK)), State::Unreadable);
        assert_eq!(window_state(Some(&DISK), None), State::Unreadable);
        assert_eq!(
            window_state(Some(&DISK), Some(&DISK[..4])),
            State::Unreadable
        );
    }

    #[test]
    fn states_have_distinct_json_keys() {
        let keys = [
            State::Original,
            State::Changed,
            State::Unreadable,
            State::Blocked,
            State::ModuleMissing,
        ]
        .map(State::key);
        let unique: std::collections::HashSet<_> = keys.iter().collect();
        assert_eq!(unique.len(), keys.len());
    }
}
