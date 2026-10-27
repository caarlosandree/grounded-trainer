#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Testes de sinais, pânico, SIGKILL e recuperação do diário pelo `gt-patch-harness`,
//! sempre contra o processo-alvo de teste (nunca contra o jogo).
//!
//! Nenhum teste deste binário muda a disposição de sinais do próprio processo: assim o
//! harness, iniciado daqui, nasce com SIGTSTP no padrão (uma disposição `SIG_IGN`
//! herdada pelo `exec` faria `sigtstp_is_ignored` passar à toa; o teste confere isso).

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

use gt_core::journal::{Journal, JournalEntry, JournalStore, Loaded};
use gt_core::mem::MemReader;
use gt_core::procinfo::{boot_id, list_tids, read_task_stat};
use gt_core::suspend::wait_until;
use gt_testkit::{TestTarget, bin_path};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

const NOP3: [u8; 3] = [0x90, 0x90, 0x90];
const NOP2_NOP: [u8; 3] = [0x66, 0x90, 0x90];

/// Prazo para o harness terminar depois de um sinal.
const EXIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Número do SIGTSTP no Linux (bit `sinal - 1` das máscaras de `/proc/<pid>/status`).
const SIGTSTP_NUM: u32 = 20;

fn store(dir: &Path) -> JournalStore {
    JournalStore::new(dir.to_path_buf())
}

fn journal(dir: &Path) -> Loaded {
    store(dir).peek().unwrap()
}

fn bytes_at(pid: i32, addr: u64, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    MemReader::new(pid).read_exact(addr, &mut buf).unwrap();
    buf
}

/// Todas as threads vivas do processo estão em `T`.
fn stopped(pid: i32) -> bool {
    let states: Vec<char> = list_tids(pid)
        .unwrap()
        .into_iter()
        .filter_map(|tid| read_task_stat(pid, tid).unwrap())
        .map(|s| s.state)
        .collect();
    !states.is_empty() && states.iter().all(|s| *s == 'T')
}

/// Espera (até 2 s) o processo ficar no estado pedido.
fn wait_stopped(pid: i32, want: bool) -> bool {
    wait_until(
        || Ok(stopped(pid) == want),
        Duration::from_secs(2),
        Duration::from_millis(1),
    )
    .unwrap()
}

/// Máscara `SigIgn` de `/proc/<pid>/status`.
fn sig_ignored(pid: u32) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    status
        .lines()
        .find_map(|l| l.strip_prefix("SigIgn:"))
        .map(|h| u64::from_str_radix(h.trim(), 16).unwrap())
        .unwrap()
}

fn tstp_bit() -> u64 {
    1 << (SIGTSTP_NUM - 1)
}

/// Harness com o diário em `dir` (sem subcomando).
fn harness(dir: &Path) -> Command {
    let mut cmd = Command::new(bin_path("gt-patch-harness"));
    cmd.arg("--journal-dir").arg(dir);
    cmd
}

/// `run` do patch `90 90 90` → `66 90 90` no início do `exec2`, com o gancho opcional.
fn run_cmd(t: &TestTarget, dir: &Path, hook: Option<&str>) -> Command {
    match hook {
        Some(hook) => run_cmd_with(t, dir, &["--hook", hook]),
        None => run_cmd_with(t, dir, &[]),
    }
}

/// Como [`run_cmd`], com opções globais do harness quaisquer (`--hook`, `--fault`...).
fn run_cmd_with(t: &TestTarget, dir: &Path, globals: &[&str]) -> Command {
    let mut cmd = harness(dir);
    cmd.args([
        "--pid".to_string(),
        t.pid.to_string(),
        "--allow".to_string(),
        format!("{:#x}:{}", t.exec2, 2 * t.page),
    ]);
    cmd.args(globals);
    cmd.args([
        "run".to_string(),
        "--addr".to_string(),
        format!("{:#x}", t.exec2),
        "--expected".to_string(),
        "909090".to_string(),
        "--replacement".to_string(),
        "669090".to_string(),
    ]);
    cmd
}

fn spawn(mut cmd: Command) -> Child {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// Lê o `stdout` do filho até a linha `want`. Entra em pânico se o filho fechar antes.
fn wait_line(child: &mut Child, want: &str) -> BufReader<ChildStdout> {
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut seen = String::new();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).unwrap();
        if n == 0 {
            let mut err = String::new();
            if let Some(mut e) = child.stderr.take() {
                e.read_to_string(&mut err).unwrap();
            }
            panic!("o harness fechou sem imprimir {want:?}; stdout: {seen:?}; stderr: {err}");
        }
        if line.trim_end() == want {
            return reader;
        }
        seen.push_str(&line);
    }
}

/// Espera o filho terminar (até [`EXIT_TIMEOUT`]).
fn wait_exit(child: &mut Child) -> ExitStatus {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if start.elapsed() > EXIT_TIMEOUT {
            let _ = child.kill();
            panic!("o harness não terminou no prazo");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Resto do `stdout` depois de `wait_line`.
fn rest(mut reader: BufReader<ChildStdout>) -> String {
    let mut out = String::new();
    reader.read_to_string(&mut out).unwrap();
    out
}

fn restore(dir: &Path) -> Output {
    let out = harness(dir).arg("restore").output().unwrap();
    assert!(out.status.success(), "restore falhou: {out:?}");
    out
}

/// `run` até `ativo`, envia `sig` e confere a restauração.
fn signal_restores(sig: Signal) {
    let t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn(run_cmd(&t, dir.path(), None));
    let reader = wait_line(&mut child, "ativo");
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP2_NOP);
    assert!(matches!(journal(dir.path()), Loaded::Ok(_)));
    kill(Pid::from_raw(child.id() as i32), sig).unwrap();
    let status = wait_exit(&mut child);
    assert_eq!(status.code(), Some(0), "{status:?}");
    assert!(rest(reader).contains("restaurado"));
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
    assert_eq!(journal(dir.path()), Loaded::Missing);
}

#[test]
fn sigint_restores() {
    signal_restores(Signal::SIGINT);
}

#[test]
fn sigterm_restores() {
    signal_restores(Signal::SIGTERM);
}

#[test]
fn sighup_restores() {
    signal_restores(Signal::SIGHUP);
}

#[test]
fn sigtstp_is_ignored() {
    // O harness herda daqui a disposição do SIGTSTP; ela precisa estar no padrão para
    // que o `SigIgn` do harness prove que foi o `Shutdown::install` que o ignorou.
    assert_eq!(
        sig_ignored(std::process::id()) & tstp_bit(),
        0,
        "o processo de teste já ignora SIGTSTP; o teste passaria à toa"
    );
    let t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn(run_cmd(&t, dir.path(), None));
    let reader = wait_line(&mut child, "ativo");
    let hpid = child.id();
    assert_ne!(
        sig_ignored(hpid) & tstp_bit(),
        0,
        "harness não ignora SIGTSTP"
    );
    kill(Pid::from_raw(hpid as i32), Signal::SIGTSTP).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert!(!stopped(hpid as i32), "o harness parou com SIGTSTP");
    kill(Pid::from_raw(hpid as i32), Signal::SIGINT).unwrap();
    let status = wait_exit(&mut child);
    assert_eq!(status.code(), Some(0), "{status:?}");
    assert!(rest(reader).contains("restaurado"));
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
    assert_eq!(journal(dir.path()), Loaded::Missing);
}

#[test]
fn panic_after_apply_restores() {
    let t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    let out = run_cmd(&t, dir.path(), Some("panic-after-apply"))
        .output()
        .unwrap();
    assert!(!out.status.success(), "{out:?}");
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
    assert!(wait_stopped(t.pid, false));
    assert_eq!(journal(dir.path()), Loaded::Missing);
}

#[test]
fn panic_in_pause_resumes_and_leaves_bytes() {
    let t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    let out = run_cmd(&t, dir.path(), Some("panic-in-pause"))
        .output()
        .unwrap();
    assert!(!out.status.success(), "{out:?}");
    assert!(wait_stopped(t.pid, false));
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
    assert_eq!(journal(dir.path()), Loaded::Missing);
}

#[test]
fn sigint_during_pause_restores() {
    let t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    let out = run_cmd(&t, dir.path(), Some("sigint-in-pause"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("ativo") && stdout.contains("restaurado"),
        "{stdout}"
    );
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
    assert!(wait_stopped(t.pid, false));
    assert_eq!(journal(dir.path()), Loaded::Missing);
}

#[test]
fn sigkill_then_restore() {
    let t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn(run_cmd(&t, dir.path(), None));
    let _reader = wait_line(&mut child, "ativo");
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP2_NOP);
    match journal(dir.path()) {
        Loaded::Ok(j) => {
            assert!(!j.paused_by_us);
            assert_eq!(j.entries.len(), 1);
            assert_eq!(j.entries[0].addr, t.exec2);
        }
        other => panic!("diário inesperado: {other:?}"),
    }
    let out = restore(dir.path());
    assert!(String::from_utf8_lossy(&out.stdout).contains("restaurado"));
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
    assert_eq!(journal(dir.path()), Loaded::Missing);
}

#[test]
fn sigkill_in_pause_then_restore_resumes() {
    let t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    let out = run_cmd(&t, dir.path(), Some("kill-in-pause"))
        .output()
        .unwrap();
    assert!(!out.status.success(), "{out:?}");
    assert!(stopped(t.pid), "o alvo deveria continuar parado");
    match journal(dir.path()) {
        Loaded::Ok(j) => assert!(j.paused_by_us),
        other => panic!("diário inesperado: {other:?}"),
    }
    let out = restore(dir.path());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("retomado"),
        "{out:?}"
    );
    assert!(wait_stopped(t.pid, false));
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
    assert_eq!(journal(dir.path()), Loaded::Missing);
}

#[test]
fn second_instance_is_refused() {
    let t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    let mut first = spawn(run_cmd(&t, dir.path(), None));
    let reader = wait_line(&mut first, "ativo");
    let out = run_cmd(&t, dir.path(), None).output().unwrap();
    assert!(!out.status.success(), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("outro gt-patch ou gt-tui já está em execução"),
        "{stderr}"
    );
    // O primeiro continua ativo e restaura normalmente.
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP2_NOP);
    kill(Pid::from_raw(first.id() as i32), Signal::SIGINT).unwrap();
    assert_eq!(wait_exit(&mut first).code(), Some(0));
    assert!(rest(reader).contains("restaurado"));
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
}

#[test]
fn run_ends_when_target_exits() {
    let mut t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn(run_cmd(&t, dir.path(), None));
    let reader = wait_line(&mut child, "ativo");
    t.child.kill().unwrap();
    t.child.wait().unwrap();
    let status = wait_exit(&mut child);
    assert_eq!(status.code(), Some(0), "{status:?}");
    assert!(rest(reader).contains("o jogo fechou"));
    assert_eq!(journal(dir.path()), Loaded::Missing);
}

#[test]
fn restore_discards_dead_process_journal() {
    let dir = tempfile::tempdir().unwrap();
    let mut gone = Command::new("true").spawn().unwrap();
    let pid = gone.id() as i32;
    gone.wait().unwrap();
    store(dir.path())
        .save(&Journal {
            boot_id: boot_id().unwrap(),
            pid,
            start_time: 1,
            version_key: "teste".into(),
            paused_by_us: false,
            entries: vec![JournalEntry {
                id: "p".into(),
                addr: 0x1000,
                original: NOP3.to_vec(),
                replacement: NOP2_NOP.to_vec(),
                next: None,
                kind: gt_core::patch::PatchKind::Code,
            }],
            data: Vec::new(),
        })
        .unwrap();
    let out = restore(dir.path());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("descartado"),
        "{out:?}"
    );
    assert_eq!(journal(dir.path()), Loaded::Missing);
}

#[test]
fn restore_without_journal_says_nothing_to_restore() {
    let dir = tempfile::tempdir().unwrap();
    let out = restore(dir.path());
    assert!(String::from_utf8_lossy(&out.stdout).contains("nada a restaurar"));
}

#[test]
fn restore_skips_entry_outside_exec_region() {
    let t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    // Região `rw` (não executável): a recuperação genérica nunca escreve nela.
    gt_core::mem::MemWriter::open(t.pid)
        .unwrap()
        .write(t.big_region, &NOP2_NOP)
        .unwrap();
    store(dir.path())
        .save(&Journal {
            boot_id: boot_id().unwrap(),
            pid: t.pid,
            start_time: gt_core::procinfo::read_stat(t.pid).unwrap().start_time,
            version_key: "teste".into(),
            paused_by_us: false,
            entries: vec![JournalEntry {
                id: "fora".into(),
                addr: t.big_region,
                original: NOP3.to_vec(),
                replacement: NOP2_NOP.to_vec(),
                next: None,
                kind: gt_core::patch::PatchKind::Code,
            }],
            data: Vec::new(),
        })
        .unwrap();
    let out = restore(dir.path());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("fora"),
        "{out:?}"
    );
    assert_eq!(bytes_at(t.pid, t.big_region, 3), NOP2_NOP);
    assert_eq!(journal(dir.path()), Loaded::Missing);
}

/// `run` do laço de valor (`B8 11 11 11 11` → `B8 22 22 22 22`) com troca para `set_value`.
fn value_run_cmd(t: &TestTarget, dir: &Path, globals: &[&str], set_value: &str) -> Command {
    let mut cmd = harness(dir);
    cmd.args([
        "--pid".to_string(),
        t.pid.to_string(),
        "--allow".to_string(),
        format!("{:#x}:{}", t.value_page, t.page),
    ]);
    cmd.args(globals);
    cmd.args([
        "run".to_string(),
        "--addr".to_string(),
        format!("{:#x}", t.value_page),
        "--expected".to_string(),
        "b811111111".to_string(),
        "--replacement".to_string(),
        "b822222222".to_string(),
        "--set-value".to_string(),
        set_value.to_string(),
    ]);
    cmd
}

#[test]
fn sigkill_during_value_swap_then_recover() {
    let t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    // O gancho mata o harness dentro da segunda pausa (a da troca de valor).
    let out = value_run_cmd(
        &t,
        dir.path(),
        &["--hook", "kill-in-pause", "--hook-pause", "2"],
        "b833333333",
    )
    .output()
    .unwrap();
    assert!(!out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("ativo"), "{stdout}");
    assert!(!stdout.contains("valor trocado"), "{stdout}");
    assert!(stopped(t.pid), "o alvo deveria continuar parado");
    // O diário guarda os três conjuntos de bytes da troca interrompida.
    match journal(dir.path()) {
        Loaded::Ok(j) => {
            assert!(j.paused_by_us);
            assert_eq!(j.entries.len(), 1);
            assert_eq!(j.entries[0].original, [0xB8, 0x11, 0x11, 0x11, 0x11]);
            assert_eq!(j.entries[0].replacement, [0xB8, 0x22, 0x22, 0x22, 0x22]);
            assert_eq!(j.entries[0].next, Some(vec![0xB8, 0x33, 0x33, 0x33, 0x33]));
        }
        other => panic!("diário inesperado: {other:?}"),
    }
    let out = restore(dir.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("retomado"), "{stdout}");
    assert!(stdout.contains("restaurado: harness"), "{stdout}");
    assert!(wait_stopped(t.pid, false));
    assert_eq!(
        bytes_at(t.pid, t.value_page, 5),
        [0xB8, 0x11, 0x11, 0x11, 0x11]
    );
    assert_eq!(journal(dir.path()), Loaded::Missing);
}

#[test]
fn value_swap_runs_until_signal_and_restores() {
    let t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn(value_run_cmd(&t, dir.path(), &[], "b833333333"));
    let reader = wait_line(&mut child, "valor trocado");
    assert_eq!(
        bytes_at(t.pid, t.value_page, 5),
        [0xB8, 0x33, 0x33, 0x33, 0x33]
    );
    kill(Pid::from_raw(child.id() as i32), Signal::SIGINT).unwrap();
    assert_eq!(wait_exit(&mut child).code(), Some(0));
    assert!(rest(reader).contains("restaurado"));
    assert_eq!(
        bytes_at(t.pid, t.value_page, 5),
        [0xB8, 0x11, 0x11, 0x11, 0x11]
    );
    assert_eq!(journal(dir.path()), Loaded::Missing);
}

#[test]
fn cont_always_then_restore_resumes() {
    let t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    // Todo SIGCONT do motor falha: o `run` sai com erro e o alvo fica parado por nós.
    let out = run_cmd_with(&t, dir.path(), &["--fault", "cont-always"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "{out:?}");
    assert!(stopped(t.pid), "o alvo deveria continuar parado");
    match journal(dir.path()) {
        Loaded::Ok(j) => assert!(j.paused_by_us),
        other => panic!("diário inesperado: {other:?}"),
    }
    // Sem `--fault`, o `restore` retoma o alvo e restaura os originais.
    let out = restore(dir.path());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("retomado"),
        "{out:?}"
    );
    assert!(wait_stopped(t.pid, false));
    assert_eq!(bytes_at(t.pid, t.exec2, 3), NOP3);
    assert_eq!(journal(dir.path()), Loaded::Missing);
}

// ---------------------------------------------------------------------------
// Bits em dados (plano 005, tarefa 12)
// ---------------------------------------------------------------------------

/// Guarda de 8 bytes no início do `big_region` e o byte `0x07` em `+0x100`.
fn bits_object(t: &TestTarget) -> (u64, String) {
    let w = gt_core::mem::MemWriter::open(t.pid).unwrap();
    w.write(
        t.big_region,
        &[0xC1, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8],
    )
    .unwrap();
    w.write(t.big_region + 0x100, &[0x07]).unwrap();
    (
        t.big_region + 0x100,
        format!("{:#x}:c1c2c3c4c5c6c7c8", t.big_region),
    )
}

/// `set-bits` do `id` `nao-morrer`, máscara `0x06`, com opções globais quaisquer.
fn bits_cmd(t: &TestTarget, dir: &Path, globals: &[&str]) -> Command {
    let (byte, guard) = bits_object(t);
    let mut cmd = harness(dir);
    cmd.args(["--pid".to_string(), t.pid.to_string()]);
    cmd.args(globals);
    cmd.args([
        "set-bits".to_string(),
        "--id".to_string(),
        "nao-morrer".to_string(),
        "--addr".to_string(),
        format!("{byte:#x}"),
        "--mask".to_string(),
        "0x06".to_string(),
        "--guard".to_string(),
        guard,
    ]);
    cmd
}

#[test]
fn set_bits_sigkill_then_recover() {
    let t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn(bits_cmd(&t, dir.path(), &[]));
    let _reader = wait_line(&mut child, "ativo");
    let byte = t.big_region + 0x100;
    assert_eq!(bytes_at(t.pid, byte, 1), [0x01]);
    child.kill().unwrap();
    child.wait().unwrap();
    match journal(dir.path()) {
        Loaded::Ok(j) => {
            assert!(j.entries.is_empty());
            assert_eq!(j.data.len(), 1);
            assert_eq!(
                (j.data[0].original.as_slice(), j.data[0].mask),
                ([0x07].as_slice(), 0x06)
            );
        }
        other => panic!("diário inesperado: {other:?}"),
    }
    let out = restore(dir.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("restaurado: nao-morrer"), "{stdout}");
    assert_eq!(bytes_at(t.pid, byte, 1), [0x07]);
    assert_eq!(journal(dir.path()), Loaded::Missing);
}

#[test]
fn set_bits_sigkill_object_gone_discards() {
    let t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn(bits_cmd(&t, dir.path(), &[]));
    let _reader = wait_line(&mut child, "ativo");
    child.kill().unwrap();
    child.wait().unwrap();
    // Outro objeto no mesmo endereço: a guarda não bate mais.
    gt_core::mem::MemWriter::open(t.pid)
        .unwrap()
        .write(t.big_region, &[0x00])
        .unwrap();
    let out = restore(dir.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("bytes de outro, não tocado: nao-morrer"),
        "{stdout}"
    );
    assert_eq!(bytes_at(t.pid, t.big_region + 0x100, 1), [0x01]);
    assert_eq!(journal(dir.path()), Loaded::Missing);
}

#[test]
fn set_bits_sigterm_restores() {
    let t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn(bits_cmd(&t, dir.path(), &[]));
    let reader = wait_line(&mut child, "ativo");
    kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM).unwrap();
    let status = wait_exit(&mut child);
    assert_eq!(status.code(), Some(0), "{status:?}");
    assert!(rest(reader).contains("restaurado"));
    assert_eq!(bytes_at(t.pid, t.big_region + 0x100, 1), [0x07]);
    assert_eq!(journal(dir.path()), Loaded::Missing);
}

#[test]
fn set_bits_panic_restores() {
    let t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    let out = bits_cmd(&t, dir.path(), &["--hook", "panic-after-bits"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("pânico de teste"),
        "{out:?}"
    );
    // O `Drop` do motor restaurou os bits.
    assert_eq!(bytes_at(t.pid, t.big_region + 0x100, 1), [0x07]);
    assert_eq!(journal(dir.path()), Loaded::Missing);
}

// ---------------------------------------------------------------------------
// Valor inteiro em dados (plano 006, tarefa 3)
// ---------------------------------------------------------------------------

const WORD_OLD: [u8; 4] = [1, 2, 3, 4];

/// Palavra `[1, 2, 3, 4]` em `+0x200` do `big_region`, guarda de 8 bytes no início.
fn word_object(t: &TestTarget) -> (u64, String) {
    let w = gt_core::mem::MemWriter::open(t.pid).unwrap();
    w.write(
        t.big_region,
        &[0xC1, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8],
    )
    .unwrap();
    w.write(t.big_region + 0x200, &WORD_OLD).unwrap();
    (
        t.big_region + 0x200,
        format!("{:#x}:c1c2c3c4c5c6c7c8", t.big_region),
    )
}

/// `set-word` do `id` `carga` com os valores dados (o segundo é a troca).
fn word_cmd(t: &TestTarget, dir: &Path, globals: &[&str], values: &[&str]) -> Command {
    let (word, guard) = word_object(t);
    let mut cmd = harness(dir);
    cmd.args(["--pid".to_string(), t.pid.to_string()]);
    cmd.args(globals);
    cmd.args([
        "set-word".to_string(),
        "--id".to_string(),
        "carga".to_string(),
        "--addr".to_string(),
        format!("{word:#x}"),
    ]);
    for v in values {
        cmd.args(["--value", v]);
    }
    cmd.args(["--guard".to_string(), guard]);
    cmd
}

#[test]
fn set_word_sigkill_then_recover() {
    let t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn(word_cmd(&t, dir.path(), &[], &["09090909"]));
    let _reader = wait_line(&mut child, "ativo");
    let word = t.big_region + 0x200;
    assert_eq!(bytes_at(t.pid, word, 4), [9, 9, 9, 9]);
    child.kill().unwrap();
    child.wait().unwrap();
    match journal(dir.path()) {
        Loaded::Ok(j) => {
            assert_eq!(j.data.len(), 1);
            assert_eq!(j.data[0].original, WORD_OLD.to_vec());
            assert_eq!(j.data[0].written, vec![9, 9, 9, 9]);
        }
        other => panic!("diário inesperado: {other:?}"),
    }
    let out = restore(dir.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("restaurado: carga"), "{stdout}");
    assert_eq!(bytes_at(t.pid, word, 4), WORD_OLD);
    assert_eq!(journal(dir.path()), Loaded::Missing);
}

/// Um `set` de 1 byte (o `bool` do mundo, spec 007): SIGKILL e `restore` pelo diário devolvem só
/// o byte original, sem tocar nos vizinhos.
#[test]
fn world_sigkill_restores_by_journal() {
    let t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn(word_cmd(&t, dir.path(), &[], &["00"]));
    let _reader = wait_line(&mut child, "ativo");
    let word = t.big_region + 0x200;
    assert_eq!(bytes_at(t.pid, word, 4), [0, 2, 3, 4]);
    child.kill().unwrap();
    child.wait().unwrap();
    match journal(dir.path()) {
        Loaded::Ok(j) => {
            assert_eq!(j.data.len(), 1);
            assert_eq!(j.data[0].original, vec![WORD_OLD[0]]);
            assert_eq!(j.data[0].written, vec![0]);
        }
        other => panic!("diário inesperado: {other:?}"),
    }
    let out = restore(dir.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("restaurado: carga"), "{stdout}");
    assert_eq!(bytes_at(t.pid, word, 4), WORD_OLD);
    assert_eq!(journal(dir.path()), Loaded::Missing);
}

#[test]
fn set_word_sigkill_between_next_and_write() {
    let t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn(word_cmd(
        &t,
        dir.path(),
        &["--hook", "kill-after-next"],
        &["09090909", "07070707"],
    ));
    let status = wait_exit(&mut child);
    assert!(!status.success(), "{status:?}");
    let word = t.big_region + 0x200;
    // O `next` está no diário e a escrita nova não aconteceu.
    assert_eq!(bytes_at(t.pid, word, 4), [9, 9, 9, 9]);
    match journal(dir.path()) {
        Loaded::Ok(j) => {
            assert_eq!(j.data.len(), 1);
            assert_eq!(j.data[0].original, WORD_OLD.to_vec());
            assert_eq!(j.data[0].written, vec![9, 9, 9, 9]);
            assert_eq!(j.data[0].next, Some(vec![7, 7, 7, 7]));
        }
        other => panic!("diário inesperado: {other:?}"),
    }
    let out = restore(dir.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("restaurado: carga"), "{stdout}");
    assert_eq!(bytes_at(t.pid, word, 4), WORD_OLD);
    assert_eq!(journal(dir.path()), Loaded::Missing);
}

#[test]
fn set_word_sigterm_restores() {
    let t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn(word_cmd(&t, dir.path(), &[], &["09090909", "07070707"]));
    let reader = wait_line(&mut child, "ativo");
    assert_eq!(bytes_at(t.pid, t.big_region + 0x200, 4), [7, 7, 7, 7]);
    kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM).unwrap();
    let status = wait_exit(&mut child);
    assert_eq!(status.code(), Some(0), "{status:?}");
    assert!(rest(reader).contains("restaurado"));
    assert_eq!(bytes_at(t.pid, t.big_region + 0x200, 4), WORD_OLD);
    assert_eq!(journal(dir.path()), Loaded::Missing);
}

#[test]
fn set_word_panic_restores() {
    let t = TestTarget::spawn();
    let dir = tempfile::tempdir().unwrap();
    let out = word_cmd(
        &t,
        dir.path(),
        &["--hook", "panic-after-set"],
        &["09090909"],
    )
    .output()
    .unwrap();
    assert!(!out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("pânico de teste"),
        "{out:?}"
    );
    // O `Drop` do motor devolveu o original.
    assert_eq!(bytes_at(t.pid, t.big_region + 0x200, 4), WORD_OLD);
    assert_eq!(journal(dir.path()), Loaded::Missing);
}
