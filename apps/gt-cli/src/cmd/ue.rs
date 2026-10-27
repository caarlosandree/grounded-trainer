//! `gt ue class|player`: leitura da reflexão do Unreal (spec 005, 6.8). Só lê.
//!
//! A lógica fica no `gt-ue` (`describe_class`) e no `gt-grounded2` (`player`); aqui só se acham as
//! globais (pela busca de padrões, ou por `--gobjects`/`--names`) e se formata a saída.

use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use gt_core::addr::parse_addr;
use gt_grounded2::catalog;
use gt_grounded2::player::{
    Absent, FieldCatalog, FieldReportValue, PlayerReport, WatchOpts, watch_player,
};
use gt_grounded2::signatures::{Signatures, UeSigs};
use gt_grounded2::ue::{GlobalState, UE_ID};
use gt_ue::{ClassQuery, Names, ObjectArray, ReflectionIndex, ScanBudget, UeError, describe_class};
use serde::Serialize;

use super::Global;
use crate::context::Target;
use crate::output::emit;

#[derive(Debug, Subcommand)]
pub enum UeCommand {
    /// Campos de uma classe e das superclasses: offset, tamanho, tipo e, nos `bool`, byte e máscara.
    Class {
        /// `Pacote.Nome` (ex.: `/Script/Maine.SurvivalCharacter`) ou só o nome.
        class: String,
        #[command(flatten)]
        globals: GlobalArgs,
    },
    /// Acha o personagem e mostra os ponteiros do controller e os valores dos cheats de valor.
    Player {
        #[command(flatten)]
        globals: GlobalArgs,
        /// Endereço do controller; pula a varredura da tabela de objetos.
        #[arg(long, value_name = "ENDEREÇO")]
        controller: Option<String>,
        /// Repete a leitura a cada SEGUNDOS, sem varrer de novo (até Ctrl+C); com `--json`, um
        /// objeto por leitura.
        #[arg(long, value_name = "SEGUNDOS", value_parser = clap::value_parser!(u64).range(1..))]
        watch: Option<u64>,
    },
}

/// Endereços das globais, que pulam a busca de padrões (formato do `gt read`).
#[derive(Debug, Args)]
pub struct GlobalArgs {
    /// Endereço do `GUObjectArray`.
    #[arg(long, value_name = "ENDEREÇO", requires = "names")]
    gobjects: Option<String>,
    /// Endereço da lista de blocos do `FNamePool`.
    #[arg(long, value_name = "ENDEREÇO", requires = "gobjects")]
    names: Option<String>,
}

/// Erro do leitor; o do núcleo sai solto para o `main` achar a dica.
fn ue_err(e: UeError) -> anyhow::Error {
    match e {
        UeError::Core(core) => core.into(),
        other => other.into(),
    }
}

/// Alvo aberto, com o índice da reflexão montado.
struct Session {
    target: Target,
    sigs: UeSigs,
    gobjects: u64,
    index: ReflectionIndex,
    /// Duração da busca das globais (zero com `--gobjects`/`--names`).
    search: Duration,
    /// Duração da montagem do índice.
    build: Duration,
}

fn global_addr(state: &GlobalState, what: &str) -> anyhow::Result<u64> {
    match state {
        GlobalState::Verified(addr) => Ok(*addr),
        GlobalState::Unverified(addr) => {
            tracing::warn!("{what} não verificado (a versão não tem rva): {addr:#x}");
            Ok(*addr)
        }
        GlobalState::Unavailable(reason) => bail!("{what}: {reason}"),
    }
}

fn open(global: &Global, args: &GlobalArgs) -> anyhow::Result<Session> {
    let target = Target::open(global.pid)?;
    let signatures = Signatures::embedded()?;
    let started = Instant::now();
    let (sigs, gobjects, names) = match (&args.gobjects, &args.names) {
        (Some(gobjects), Some(names)) => {
            let (_, headers) = target.module(&global.module)?;
            let sigs = signatures
                .ue_for_version(&headers.version_key())
                .context("as assinaturas não têm o bloco [ue]")?;
            let gobjects = target.resolve(&parse_addr(gobjects)?, &global.module)?;
            let names = target.resolve(&parse_addr(names)?, &global.module)?;
            (sigs, gobjects, names)
        }
        _ => {
            let scan = catalog::scan_subset(&target, &signatures, &[UE_ID.to_string()])?;
            let ue = scan.ue.context("as assinaturas não têm o bloco [ue]")?;
            let gobjects = global_addr(&ue.gobjects, "GUObjectArray")?;
            let names = global_addr(&ue.names, "FNamePool")?;
            if let Err(detail) = &ue.cheap {
                bail!("reflexão não reconhecida ({detail})");
            }
            (ue.sigs, gobjects, names)
        }
    };
    let search = if args.gobjects.is_some() {
        Duration::ZERO
    } else {
        started.elapsed()
    };

    let started = Instant::now();
    let mem = &target.reader;
    let layout = sigs.layout;
    let names = Names::new(mem, names).map_err(ue_err)?;
    let objects = ObjectArray::read(mem, &layout, gobjects).map_err(ue_err)?;
    let index = ReflectionIndex::build(mem, &layout, &objects, &names, &ScanBudget::standard())
        .map_err(ue_err)?;
    if let Err(detail) = index.deep_check() {
        bail!("reflexão incoerente ({detail})");
    }
    let build = started.elapsed();
    tracing::info!(
        classes = index.class_count(),
        "globais em {search:.1?}, índice em {build:.1?}"
    );
    Ok(Session {
        target,
        sigs,
        gobjects,
        index,
        search,
        build,
    })
}

#[derive(Serialize)]
struct Timing {
    search_ms: u128,
    index_ms: u128,
}

impl Timing {
    fn of(s: &Session) -> Self {
        Self {
            search_ms: s.search.as_millis(),
            index_ms: s.build.as_millis(),
        }
    }
}

#[derive(Serialize)]
struct FieldOutput {
    owner: String,
    name: String,
    kind: String,
    offset: String,
    size: u32,
    byte: Option<String>,
    mask: Option<String>,
}

#[derive(Serialize)]
struct ClassOutput {
    class: String,
    fields: Vec<FieldOutput>,
    timing: Timing,
}

pub fn run(global: &Global, command: &UeCommand) -> anyhow::Result<()> {
    match command {
        UeCommand::Class { class, globals } => run_class(global, class, globals),
        UeCommand::Player {
            globals,
            controller,
            watch,
        } => run_player(global, globals, controller.as_deref(), *watch),
    }
}

fn run_class(global: &Global, query: &str, args: &GlobalArgs) -> anyhow::Result<()> {
    let s = open(global, args)?;
    let dump = match describe_class(&s.index, &s.target.reader, query).map_err(ue_err)? {
        ClassQuery::Found(dump) => dump,
        ClassQuery::Ambiguous(paths) => {
            bail!(
                "há {} classes chamadas {query}; use Pacote.Nome: {}",
                paths.len(),
                paths.join(", ")
            )
        }
    };
    let output = ClassOutput {
        class: dump.path.clone(),
        fields: dump
            .fields
            .iter()
            .map(|f| FieldOutput {
                owner: f.owner.clone(),
                name: f.name.clone(),
                kind: f.kind.clone(),
                offset: format!("{:#x}", f.offset),
                size: f.size,
                byte: f.bits.map(|b| format!("{:#x}", b.byte)),
                mask: f.bits.map(|b| format!("{:#04x}", b.mask)),
            })
            .collect(),
        timing: Timing::of(&s),
    };
    emit(
        global.json,
        |out| {
            writeln!(out, "{} ({} campos)", output.class, output.fields.len())?;
            let mut owner = "";
            for f in &output.fields {
                if f.owner != owner {
                    owner = &f.owner;
                    writeln!(out, "  {owner}:")?;
                }
                let bits = match (&f.byte, &f.mask) {
                    (Some(byte), Some(mask)) => format!("  byte {byte} máscara {mask}"),
                    _ => String::new(),
                };
                writeln!(
                    out,
                    "    {:>7}  {:>4}  {:<22} {}{bits}",
                    f.offset, f.size, f.kind, f.name
                )?;
            }
            Ok(())
        },
        &output,
    )
}

#[derive(Serialize)]
struct PlayerOutput<'a> {
    report: Option<&'a PlayerReport>,
    absent: Option<String>,
    timing: &'a Timing,
}

fn ptr(p: Option<u64>) -> String {
    match p {
        Some(0) => "nulo".to_string(),
        Some(addr) => format!("{addr:#x}"),
        None => "—".to_string(),
    }
}

fn write_report(out: &mut dyn Write, report: &PlayerReport) -> std::io::Result<()> {
    writeln!(out, "controller: {:#x}", report.controller)?;
    writeln!(
        out,
        "Pawn: {}  Character: {}  AcknowledgedPawn: {}",
        ptr(report.pawn),
        ptr(report.character),
        ptr(report.acknowledged_pawn)
    )?;
    for f in &report.fields {
        match &f.value {
            FieldReportValue::Unavailable { reason } => {
                writeln!(out, "  {:<20} indisponível ({reason})", f.id)?;
            }
            FieldReportValue::Lock {
                addr,
                current,
                target,
            } => {
                writeln!(
                    out,
                    "  {:<20} atual {current} alvo {target}  ({addr:#x})",
                    f.id
                )?;
            }
            FieldReportValue::Bits { bits } => {
                let list: Vec<String> = bits
                    .iter()
                    .map(|b| {
                        format!(
                            "{}={} ({:#x} & {:#04x})",
                            b.field,
                            u8::from(b.set),
                            b.addr,
                            b.mask
                        )
                    })
                    .collect();
                writeln!(out, "  {:<20} {}", f.id, list.join("  "))?;
            }
            FieldReportValue::Set { fields } => {
                let list: Vec<String> = fields
                    .iter()
                    .map(|v| format!("{}={} ({:#x}, cru {:#x})", v.name, v.value, v.addr, v.raw))
                    .collect();
                writeln!(out, "  {:<20} {}", f.id, list.join("  "))?;
            }
            FieldReportValue::Add { addr, current } => {
                writeln!(out, "  {:<20} atual {current}  ({addr:#x})", f.id)?;
            }
        }
    }
    if let Some(error) = &report.pawn_error {
        writeln!(out, "pawn: indisponível ({error})")?;
    }
    for gs in &report.game_state {
        writeln!(
            out,
            "GameState {}: Level {}  World {}  GameState {}  objeto {}",
            gs.path,
            ptr(gs.level),
            ptr(gs.world),
            ptr(gs.game_state),
            ptr(gs.object)
        )?;
        for hop in &gs.hops {
            writeln!(out, "  {} = {:#x} ({})", hop.name, hop.addr, hop.class)?;
        }
        if let Some(error) = &gs.error {
            writeln!(out, "  caminho interrompido: {error}")?;
        }
    }
    Ok(())
}

fn run_player(
    global: &Global,
    args: &GlobalArgs,
    controller: Option<&str>,
    watch: Option<u64>,
) -> anyhow::Result<()> {
    let s = open(global, args)?;
    let mem = &s.target.reader;
    let controller = controller
        .map(|c| s.target.resolve(&parse_addr(c)?, &global.module))
        .transpose()?;
    let signatures = Signatures::embedded()?;
    let catalog = FieldCatalog::resolve(&s.index, mem, &s.sigs.player, signatures.fields());
    let opts = WatchOpts {
        controller,
        every: Duration::from_secs(watch.unwrap_or(1)),
        rounds: if watch.is_some() { None } else { Some(1) },
        budget: ScanBudget::standard(),
    };
    let timing = Timing::of(&s);
    let mut last: Option<Absent> = None;
    let mut failed: Option<anyhow::Error> = None;
    let mut round = 0u32;
    watch_player(
        mem,
        &s.index,
        &catalog,
        s.gobjects,
        &opts,
        &mut std::thread::sleep,
        &mut |result| {
            round += 1;
            last = result.err().cloned();
            let output = PlayerOutput {
                report: result.ok(),
                absent: result.err().map(ToString::to_string),
                timing: &timing,
            };
            let shown = emit(
                global.json,
                |out| {
                    if watch.is_some() {
                        writeln!(out, "--- leitura {round} ---")?;
                    }
                    match result {
                        Ok(report) => write_report(out, report),
                        Err(absent) => writeln!(out, "personagem ausente: {absent}"),
                    }
                },
                &output,
            );
            if let Err(e) = shown {
                failed.get_or_insert(e);
            }
        },
    )
    .map_err(ue_err)?;
    if let Some(e) = failed {
        return Err(e);
    }
    match last {
        Some(absent) if watch.is_none() => bail!("personagem indisponível: {absent}"),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use gt_grounded2::player::{FieldReport, GameStateReport, HopReport, SetFieldReport};

    use super::*;

    fn report() -> PlayerReport {
        PlayerReport {
            controller: 0x10,
            pawn: Some(0),
            character: None,
            acknowledged_pawn: None,
            fields: vec![FieldReport {
                id: "construir-sem-ingredientes".into(),
                value: FieldReportValue::Set {
                    fields: vec![SetFieldReport {
                        name: "bRecipesRequireIngredients".into(),
                        addr: 0x5030,
                        raw: 1,
                        value: "true".into(),
                    }],
                },
            }],
            pawn_error: Some("Pawn nulo".into()),
            game_state: vec![
                GameStateReport {
                    level: Some(0x1),
                    world: Some(0x2),
                    game_state: Some(0x3),
                    path: "SurvivalModeManagerComponent.GameModeSettings".into(),
                    hops: vec![
                        HopReport {
                            name: "SurvivalModeManagerComponent".into(),
                            addr: 0x4000,
                            class: "SurvivalModeManagerComponent".into(),
                        },
                        HopReport {
                            name: "GameModeSettings".into(),
                            addr: 0x5000,
                            class: "BP_NormalModeSettings_C".into(),
                        },
                    ],
                    object: Some(0x5000),
                    error: None,
                },
                GameStateReport {
                    path: "PartyComponent".into(),
                    level: Some(0x1),
                    world: Some(0x2),
                    game_state: Some(0x3),
                    error: Some("PartyComponent ausente ou de classe inesperada".into()),
                    ..GameStateReport::default()
                },
            ],
        }
    }

    #[test]
    fn write_report_shows_world_path_bool_and_pawn_error() {
        let mut out = Vec::new();
        write_report(&mut out, &report()).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("pawn: indisponível (Pawn nulo)"), "{text}");
        assert!(
            text.contains("GameState SurvivalModeManagerComponent.GameModeSettings:"),
            "{text}"
        );
        assert!(
            text.contains("GameModeSettings = 0x5000 (BP_NormalModeSettings_C)"),
            "{text}"
        );
        assert!(
            text.contains("bRecipesRequireIngredients=true (0x5030, cru 0x1)"),
            "{text}"
        );
        assert!(
            text.contains("caminho interrompido: PartyComponent ausente"),
            "{text}"
        );
    }
}
