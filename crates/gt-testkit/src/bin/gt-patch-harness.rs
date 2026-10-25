//! Binário de apoio a testes: chama a **mesma** biblioteca do `gt-patch` (recuperação,
//! sinais, laço do `run` e motor) com um patch cru passado pelo teste. Nunca é o `gt-patch`
//! e não conhece o jogo.
//!
//! Uso:
//!
//! ```text
//! gt-patch-harness --journal-dir D [--pid P --allow START:LEN]
//!     [--hook panic-in-pause|kill-in-pause|sigint-in-pause|panic-after-apply]
//!     (run --addr A --expected HEX --replacement HEX | restore)
//! ```
//!
//! `run` imprime `ativo` depois de aplicar e `restaurado` (ou `o jogo fechou`) ao sair.

use std::io::Write;
use std::ops::Range;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use gt_core::engine::{Engine, EngineOptions, PolicyFn};
use gt_core::journal::{DiscardReason, JournalStore};
use gt_core::patch::{PatchSpec, WritePolicy, validate};
use gt_core::procinfo::identity;
use gt_core::recovery::{RecoveryReport, recover};
use gt_core::session::{RunEnd, Shutdown, run_until_shutdown};
use gt_core::suspend::{HookPoint, PauseConfig};
use nix::sys::signal::{Signal, raise};

/// Intervalo em que o laço do `run` acorda (spec 12).
const TICK: Duration = Duration::from_millis(200);

/// `id` do patch cru.
const PATCH_ID: &str = "harness";

enum Cmd {
    Run {
        addr: u64,
        expected: Vec<u8>,
        replacement: Vec<u8>,
    },
    Restore,
}

struct Args {
    journal_dir: PathBuf,
    pid: Option<i32>,
    allow: Option<Range<u64>>,
    hook: Option<fn(HookPoint)>,
    cmd: Cmd,
}

fn panic_in_pause(point: HookPoint) {
    if point == HookPoint::InPause {
        panic!("pânico de teste dentro da pausa");
    }
}

fn kill_in_pause(point: HookPoint) {
    if point == HookPoint::InPause {
        let _ = raise(Signal::SIGKILL);
    }
}

fn sigint_in_pause(point: HookPoint) {
    if point == HookPoint::InPause {
        let _ = raise(Signal::SIGINT);
    }
}

fn panic_after_apply(point: HookPoint) {
    if point == HookPoint::AfterApply {
        panic!("pânico de teste depois de aplicar");
    }
}

/// Número em hexadecimal (`0x...`) ou decimal.
fn parse_num(s: &str) -> Result<u64> {
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => s.parse(),
    }
    .with_context(|| format!("número inválido: {s}"))
}

/// Bytes em hexadecimal, com ou sem espaços (ex. `909090` ou `90 90 90`).
fn parse_hex(s: &str) -> Result<Vec<u8>> {
    let digits: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if !digits.len().is_multiple_of(2) {
        bail!("hexadecimal com número ímpar de dígitos: {s}");
    }
    (0..digits.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&digits[i..i + 2], 16)
                .with_context(|| format!("hexadecimal inválido: {s}"))
        })
        .collect()
}

fn parse_allow(s: &str) -> Result<Range<u64>> {
    let (start, len) = s
        .split_once(':')
        .with_context(|| format!("--allow espera START:LEN, veio {s}"))?;
    let start = parse_num(start)?;
    let end = start
        .checked_add(parse_num(len)?)
        .context("--allow passa do fim do espaço de endereços")?;
    Ok(start..end)
}

fn parse_hook(s: &str) -> Result<fn(HookPoint)> {
    Ok(match s {
        "panic-in-pause" => panic_in_pause,
        "kill-in-pause" => kill_in_pause,
        "sigint-in-pause" => sigint_in_pause,
        "panic-after-apply" => panic_after_apply,
        other => bail!("gancho desconhecido: {other}"),
    })
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args> {
    let mut it = args.into_iter();
    let value = |it: &mut dyn Iterator<Item = String>, flag: &str| {
        it.next().with_context(|| format!("{flag} sem valor"))
    };
    let (mut journal_dir, mut pid, mut allow, mut hook) = (None, None, None, None);
    let cmd = loop {
        let Some(arg) = it.next() else {
            bail!("falta o subcomando (run ou restore)");
        };
        match arg.as_str() {
            "--journal-dir" => journal_dir = Some(PathBuf::from(value(&mut it, &arg)?)),
            "--pid" => {
                let v = value(&mut it, &arg)?;
                pid = Some(v.parse().with_context(|| format!("PID inválido: {v}"))?);
            }
            "--allow" => allow = Some(parse_allow(&value(&mut it, &arg)?)?),
            "--hook" => hook = Some(parse_hook(&value(&mut it, &arg)?)?),
            "restore" => break Cmd::Restore,
            "run" => {
                let (mut addr, mut expected, mut replacement) = (None, None, None);
                while let Some(flag) = it.next() {
                    let v = value(&mut it, &flag)?;
                    match flag.as_str() {
                        "--addr" => addr = Some(parse_num(&v)?),
                        "--expected" => expected = Some(parse_hex(&v)?),
                        "--replacement" => replacement = Some(parse_hex(&v)?),
                        other => bail!("opção desconhecida do run: {other}"),
                    }
                }
                break Cmd::Run {
                    addr: addr.context("run sem --addr")?,
                    expected: expected.context("run sem --expected")?,
                    replacement: replacement.context("run sem --replacement")?,
                };
            }
            other => bail!("argumento desconhecido: {other}"),
        }
    };
    if let Some(extra) = it.next() {
        bail!("argumento sobrando: {extra}");
    }
    Ok(Args {
        journal_dir: journal_dir.context("falta --journal-dir")?,
        pid,
        allow,
        hook,
        cmd,
    })
}

fn discard_reason(reason: DiscardReason) -> &'static str {
    match reason {
        DiscardReason::OtherBoot => "outro boot",
        DiscardReason::ProcessGone => "o jogo fechou",
        DiscardReason::OtherProcess => "outro processo",
    }
}

fn print_recovery(out: &mut impl Write, r: &RecoveryReport) -> std::io::Result<()> {
    if let Some(path) = &r.corrupt {
        writeln!(out, "diário ilegível; movido para {}", path.display())?;
    }
    if let Some(reason) = r.discarded {
        writeln!(out, "diário descartado ({})", discard_reason(reason))?;
    }
    if r.resumed {
        writeln!(
            out,
            "jogo retomado (estava pausado por uma execução interrompida)"
        )?;
    }
    if r.stopped_by_other {
        writeln!(out, "jogo parado por outro programa; não retomado")?;
    }
    for id in &r.restored {
        writeln!(out, "restaurado: {id}")?;
    }
    for id in &r.dropped {
        writeln!(out, "já original: {id}")?;
    }
    for id in &r.foreign {
        writeln!(out, "bytes de outro, não tocado: {id}")?;
    }
    for id in &r.kept {
        writeln!(out, "pendente (a próxima execução vai restaurar): {id}")?;
    }
    Ok(())
}

fn restore(args: Args) -> Result<()> {
    if args.pid.is_some() || args.allow.is_some() {
        bail!("restore não aceita --pid nem --allow");
    }
    let store = JournalStore::new(args.journal_dir);
    let lock = store.lock()?;
    let report = recover(&store, &lock, &PauseConfig::default())?;
    let mut out = std::io::stdout().lock();
    if report.is_empty() {
        writeln!(out, "nada a restaurar")?;
    } else {
        print_recovery(&mut out, &report)?;
    }
    Ok(())
}

fn run(args: Args, addr: u64, expected: Vec<u8>, replacement: Vec<u8>) -> Result<()> {
    let pid = args.pid.context("run exige --pid")?;
    let allow = args.allow.context("run exige --allow")?;
    let shutdown = Shutdown::install()?;
    let store = JournalStore::new(args.journal_dir);
    // A trava vive mais que o motor (declarada antes, solta depois).
    let lock = store.lock()?;
    let report = recover(&store, &lock, &PauseConfig::default())?;
    if !report.is_empty() {
        print_recovery(&mut std::io::stdout().lock(), &report)?;
    }
    // Identidade do alvo lida depois da recuperação, como o `Target::open` do `gt-patch`: o
    // motor só abre se o processo ainda for o mesmo.
    let target_id = identity(pid)?;
    let patch = validate(PatchSpec {
        id: PATCH_ID.into(),
        addr,
        expected,
        replacement,
    })?;
    let policy: PolicyFn = Box::new(move |_maps| WritePolicy::new(vec![allow.clone()]));
    let opts = EngineOptions {
        version_key: "harness".into(),
        pause: PauseConfig {
            hook: args.hook,
            ..PauseConfig::default()
        },
    };
    let mut engine = Engine::open(pid, Some(&target_id), store.clone(), &lock, policy, opts)?;
    engine.apply(vec![patch])?;
    {
        let mut out = std::io::stdout().lock();
        writeln!(out, "ativo")?;
        out.flush()?;
    }
    let end = run_until_shutdown(&mut engine, &shutdown, TICK)?;
    // Daqui em diante o terminal pode ter fechado: falha de saída não interrompe nada.
    let undone = engine.undo_all();
    let mut out = std::io::stdout().lock();
    match (end, undone) {
        (RunEnd::Signal, Ok(_)) => {
            let _ = writeln!(out, "restaurado");
        }
        (RunEnd::ProcessGone, Ok(_)) | (_, Err(gt_core::Error::ProcessGone { .. })) => {
            let _ = writeln!(out, "o jogo fechou");
        }
        (_, Err(e)) => return Err(e.into()),
    }
    let _ = out.flush();
    Ok(())
}

fn main() -> Result<()> {
    let mut args = parse_args(std::env::args().skip(1))?;
    match std::mem::replace(&mut args.cmd, Cmd::Restore) {
        Cmd::Restore => restore(args),
        Cmd::Run {
            addr,
            expected,
            replacement,
        } => run(args, addr, expected, replacement),
    }
}
