#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Desvios (spec 008, 6.2 a 6.4) contra o módulo sintético do `gt-testtarget --module`: o motor
//! liga e desliga um desvio de `sum` para uma cave que multiplica o resultado por uma variável
//! da folga da `.data`, tudo montado com as ferramentas de `gt_core::cave`.
//!
//! A cave de teste entra em `sum(a, b)` no `mov [rsp+8],rbx` (5 bytes), multiplica `rcx` e `rdx`
//! por `N` (o `i32` no começo da folga) quando `N != 0`, executa a instrução original relocada e
//! volta para a instrução seguinte; `sum(a, b)` passa a devolver `(a + b) * N`. Com `N = 0` a
//! cave passa direto: o resultado é o original.

use std::io::{BufRead, BufReader, Read};
use std::ops::Range;
use std::path::Path;
use std::process::{Child, ChildStdout, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

use gt_core::Error;
use gt_core::cave::{CAVE_MARGIN, CaveAllocator, cc_runs, check_cave_code, hook_bytes, relocate};
use gt_core::engine::{Engine, EngineOptions, PolicyFn};
use gt_core::journal::{JournalLock, JournalStore, Loaded};
use gt_core::maps::{MapRegion, read_maps};
use gt_core::mem::{MemReader, MemWriter};
use gt_core::modules::{self, Module, TrustPolicy};
use gt_core::patch::{CAVE_FILL, PatchKind, PatchSpec, Validated, validate};
use gt_core::pe::{DiskImage, PeHeaders};
use gt_core::procinfo::{list_tids, read_task_stat};
use gt_core::suspend::{PauseConfig, open_checked, sample_pcs, wait_until};
use gt_grounded2::policy::{var_region, write_policy};
use gt_testkit::{TargetOpts, TestTarget, bin_path};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

/// Prazo para o harness terminar depois de um sinal.
const EXIT_TIMEOUT: Duration = Duration::from_secs(10);

/// `id` do desvio e da cave nos testes que usam o motor direto.
const HOOK_ID: &str = "hook";
const CAVE_ID: &str = "cave";
/// `id`s que o `cave-run` do harness usa.
const HARNESS_HOOK_ID: &str = "harness";
const HARNESS_CAVE_ID: &str = "harness-cave";

// ---------------------------------------------------------------------------
// Montagem da cave de teste
// ---------------------------------------------------------------------------

/// Cave de `sum`: `mov eax,[rip+N]; test eax,eax; jz +6; imul ecx,eax; imul edx,eax`, a
/// instrução original relocada e `jmp` de volta. Termina em `jmp hook_addr + orig.len()`.
fn build_cave(cave_addr: u64, var_n: u64, hook_addr: u64, orig: &[u8]) -> Vec<u8> {
    let mut c: Vec<u8> = vec![0x8B, 0x05];
    let disp = i32::try_from(var_n as i64 - (cave_addr as i64 + 6)).unwrap();
    c.extend(disp.to_le_bytes());
    c.extend([0x85, 0xC0]); // test eax,eax
    c.extend([0x74, 0x06]); // jz  <original>
    c.extend([0x0F, 0xAF, 0xC8]); // imul ecx,eax
    c.extend([0x0F, 0xAF, 0xD0]); // imul edx,eax
    let at = cave_addr + c.len() as u64;
    c.extend(relocate(orig, hook_addr, at).unwrap());
    let at = cave_addr + c.len() as u64;
    c.extend(hook_bytes(at, 5, hook_addr + orig.len() as u64).unwrap());
    check_cave_code(&c, cave_addr, hook_addr + orig.len() as u64).unwrap();
    c
}

/// Cauda que prende uma thread dentro da cave: `cmp dword [rip+flag],0; je <início>; jmp rel32
/// <saída>`. A thread gira enquanto a palavra `flag` for 0 e sai para `exit` (fora da cave) quando
/// ela passa a ser diferente de zero. Código morto para o fluxo normal (fica depois do `jmp` de
/// volta).
fn build_tail(at: u64, flag: u64, exit: u64) -> Vec<u8> {
    let mut t: Vec<u8> = vec![0x83, 0x3D];
    let disp = i32::try_from(flag as i64 - (at as i64 + 7)).unwrap();
    t.extend(disp.to_le_bytes());
    t.push(0x00); // cmp dword [rip+flag], 0
    t.extend([0x74, 0xF7]); // je início (-9)
    let jmp_at = at + t.len() as u64;
    let rel = i32::try_from(exit as i64 - (jmp_at as i64 + 5)).unwrap();
    t.push(0xE9);
    t.extend(rel.to_le_bytes());
    t
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// O alvo com o módulo sintético e tudo que os testes precisam de `sum`.
struct Rig {
    t: TestTarget,
    module: Module,
    headers: PeHeaders,
    /// `.text` em endereços do processo.
    text: Range<u64>,
    /// Bytes da `.text` no arquivo ("disco").
    text_disk: Vec<u8>,
    /// Folga da `.data` (região de variáveis).
    region: Range<u64>,
    /// `N`: o `i32` no começo da folga.
    var_n: u64,
    /// Instrução trocada de `sum` e o endereço dela.
    hook_addr: u64,
    orig: Vec<u8>,
    /// Onde a cave foi alocada e os bytes dela.
    cave_addr: u64,
    cave: Vec<u8>,
    /// Início da cauda de espera (só com `tail`), a palavra que a solta e o `jmp` de saída.
    tail_addr: Option<u64>,
    flag: u64,
}

fn rig(tail: bool) -> Rig {
    let t = TestTarget::spawn_with(TargetOpts {
        module: true,
        ..TargetOpts::default()
    });
    let maps = read_maps(t.pid).unwrap();
    let found = modules::discover(
        &maps,
        &MemReader::new(t.pid),
        &TrustPolicy::new(vec![t.module_dir().to_path_buf()]),
        Path::new(&format!("/proc/{}/root", t.pid)),
    )
    .unwrap();
    let module = modules::find(&found, "gt-synthetic.exe").unwrap().clone();
    assert!(module.trusted);
    let disk = DiskImage::load(t.module_path()).unwrap();
    let headers = disk.headers.clone();
    let section = headers.section(".text").unwrap().clone();
    let text_disk = disk
        .bytes(section.rva, section.virtual_size as usize)
        .unwrap()
        .to_vec();
    let text_start = module.base + u64::from(section.rva);
    let text = text_start..text_start + u64::from(section.virtual_size);
    let region = var_region(&module, &headers, &maps).expect("folga da .data");
    assert_eq!(region.start, t.symbol("slack"));
    assert_eq!(region.end, t.symbol("slack_end"));

    // `sum` e a instrução trocada: o primeiro `mov [rsp+8],rbx`.
    let hook_addr = t.symbol("sum");
    let mut orig = vec![0u8; 5];
    MemReader::new(t.pid)
        .read_exact(hook_addr, &mut orig)
        .unwrap();
    assert_eq!(orig, [0x48, 0x89, 0x5C, 0x24, 0x08]);

    // A cave vai para a primeira sequência de `CC` que a comporta (o alocador de verdade).
    let flag = t.symbol("research_obj") + 0x300;
    let head_len = build_cave(text_start + 0x800, region.start, hook_addr, &orig).len();
    let total = head_len + if tail { 14 } else { 0 };
    let runs = cc_runs(&text_disk, text_start, total + CAVE_MARGIN);
    let cave_addr = CaveAllocator::new(runs)
        .alloc(total)
        .expect("sequência de CC");
    let mut cave = build_cave(cave_addr, region.start, hook_addr, &orig);
    let tail_addr = tail.then_some(cave_addr + cave.len() as u64);
    if let Some(at) = tail_addr {
        cave.extend(build_tail(at, flag, t.symbol("spin")));
    }
    assert_eq!(cave.len(), total);
    Rig {
        t,
        module,
        headers,
        text,
        text_disk,
        region: region.clone(),
        var_n: region.start,
        hook_addr,
        orig,
        cave_addr,
        cave,
        tail_addr,
        flag,
    }
}

impl Rig {
    fn cave_patch(&self) -> Validated {
        validate(PatchSpec {
            id: CAVE_ID.into(),
            addr: self.cave_addr,
            expected: vec![CAVE_FILL; self.cave.len()],
            replacement: self.cave.clone(),
            kind: PatchKind::Cave,
        })
        .unwrap()
    }

    /// Bytes do desvio: `jmp rel32` para a cave sobre a instrução inteira.
    fn hook_bytes(&self) -> Vec<u8> {
        hook_bytes(self.hook_addr, self.orig.len(), self.cave_addr).unwrap()
    }

    fn hook_patch(&self) -> Validated {
        validate(PatchSpec {
            id: HOOK_ID.into(),
            addr: self.hook_addr,
            expected: self.orig.clone(),
            replacement: self.hook_bytes(),
            kind: PatchKind::Code,
        })
        .unwrap()
    }

    /// Motor com a política de escrita do jogo (`.text` executável do módulo).
    fn engine(&self, env: &Env) -> Engine {
        self.engine_with(env, PauseConfig::default())
    }

    fn engine_with(&self, env: &Env, pause: PauseConfig) -> Engine {
        let (module, headers) = (self.module.clone(), self.headers.clone());
        let policy: PolicyFn =
            Box::new(move |maps: &[MapRegion]| write_policy(&module, &headers, maps));
        let opts = EngineOptions {
            version_key: "teste".into(),
            pause,
        };
        Engine::open(self.t.pid, None, env.store.clone(), &env.lock, policy, opts).unwrap()
    }

    fn bytes(&self, addr: u64, len: usize) -> Vec<u8> {
        let mut buf = vec![0u8; len];
        MemReader::new(self.t.pid)
            .read_exact(addr, &mut buf)
            .unwrap();
        buf
    }

    fn poke(&self, addr: u64, bytes: &[u8]) {
        MemWriter::open(self.t.pid)
            .unwrap()
            .write(addr, bytes)
            .unwrap();
    }

    fn sum(&mut self, a: i32, b: i32) -> i32 {
        self.t.call("sum", &[a as i64 as u64, b as i64 as u64]) as i32
    }

    /// `.text` na memória igual à do arquivo, folga toda zero, sem diário e o alvo rodando.
    fn assert_clean(&self, store: &JournalStore) {
        let len = (self.text.end - self.text.start) as usize;
        assert_eq!(
            self.bytes(self.text.start, len),
            self.text_disk,
            ".text diferente do arquivo"
        );
        let slack = self.bytes(
            self.region.start,
            (self.region.end - self.region.start) as usize,
        );
        assert!(
            slack.iter().all(|b| *b == 0),
            "folga não zerada: {slack:x?}"
        );
        assert_eq!(store.peek().unwrap(), Loaded::Missing);
        assert!(
            wait_until(
                || Ok(!states(self.t.pid).contains(&'T')),
                Duration::from_secs(2),
                Duration::from_millis(1),
            )
            .unwrap(),
            "o alvo ficou parado"
        );
    }
}

/// Diário num diretório temporário, com a trava segura pelo teste.
struct Env {
    _dir: tempfile::TempDir,
    store: JournalStore,
    lock: JournalLock,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let store = JournalStore::new(dir.path().join("state"));
    let lock = store.lock().unwrap();
    Env {
        _dir: dir,
        store,
        lock,
    }
}

fn states(pid: i32) -> Vec<char> {
    list_tids(pid)
        .unwrap()
        .into_iter()
        .filter_map(|tid| read_task_stat(pid, tid).unwrap())
        .map(|s| s.state)
        .collect()
}

/// Espera (até 5 s) alguma thread do alvo ser vista com o `pc` em `range`.
fn wait_pc_in(r: &Rig, range: Range<u64>) {
    let (pidfd, _id) = open_checked(r.t.pid).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let pcs = sample_pcs(
            &pidfd,
            r.t.pid,
            &PauseConfig::default(),
            1,
            Duration::from_millis(1),
            |_| Ok(()),
            || Ok(()),
            || false,
        )
        .unwrap();
        if pcs.iter().any(|pc| range.contains(pc)) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "nenhuma thread chegou a {range:x?}: {pcs:x?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn n_bytes(n: i32) -> [u8; 4] {
    n.to_le_bytes()
}

// ---------------------------------------------------------------------------
// Motor direto
// ---------------------------------------------------------------------------

#[test]
fn hook_multiplies_and_restores() {
    let mut r = rig(false);
    let env = env();
    let mut engine = r.engine(&env);
    engine.claim_vars(r.region.clone()).unwrap();
    engine.write_var(r.var_n, &n_bytes(3)).unwrap();
    engine.apply(vec![r.cave_patch()]).unwrap();
    // A cave está escrita, mas nada aponta para ela: `sum` continua o original.
    assert_eq!(r.bytes(r.cave_addr, r.cave.len()), r.cave);
    assert_eq!(r.sum(2, 3), 5);

    // Liga: `sum 2 3` = 5 × N.
    engine.apply(vec![r.hook_patch()]).unwrap();
    assert_eq!(r.bytes(r.hook_addr, 5), r.hook_bytes());
    assert_eq!(r.sum(2, 3), 15);
    assert_eq!(r.sum(-7, 3), -12);
    // Troca N pela variável, sem pausa e sem mexer no código.
    engine.write_var(r.var_n, &n_bytes(4)).unwrap();
    assert!(!states(r.t.pid).contains(&'T'));
    assert_eq!(r.bytes(r.hook_addr, 5), r.hook_bytes());
    assert_eq!(r.sum(2, 3), 20);
    // Desliga: devolve só os bytes da instrução; cave e variável ficam.
    engine.undo(&[HOOK_ID]).unwrap();
    assert_eq!(r.bytes(r.hook_addr, 5), r.orig);
    assert_eq!(r.sum(2, 3), 5);
    assert_eq!(r.bytes(r.cave_addr, r.cave.len()), r.cave);
    assert_eq!(r.bytes(r.var_n, 4), n_bytes(4));
    // Religa: reaproveita a cave e o valor que estava na variável.
    engine.apply(vec![r.hook_patch()]).unwrap();
    assert_eq!(r.sum(2, 3), 20);

    // Sai: `.text` igual ao arquivo e folga zerada.
    engine.undo_all().unwrap();
    assert!(engine.vars().is_empty());
    assert!(engine.pending_ids().is_empty());
    assert_eq!(r.sum(2, 3), 5);
    r.assert_clean(&env.store);
}

#[test]
fn caves_safe_with_zero_vars() {
    let mut r = rig(false);
    let env = env();
    let mut engine = r.engine(&env);
    engine.claim_vars(r.region.clone()).unwrap();
    // Variáveis zeradas (nunca escritas): o desvio passa direto.
    engine.apply(vec![r.cave_patch()]).unwrap();
    engine.apply(vec![r.hook_patch()]).unwrap();
    for (a, b) in [(2, 3), (-7, 3), (0, 0), (1_000_000, 2_000_000)] {
        assert_eq!(r.sum(a, b), a.wrapping_add(b), "sum({a}, {b})");
    }
    // Com valor e de volta a zero: de novo o original.
    engine.write_var(r.var_n, &n_bytes(5)).unwrap();
    assert_eq!(r.sum(2, 3), 25);
    engine.write_var(r.var_n, &n_bytes(0)).unwrap();
    assert_eq!(r.sum(2, 3), 5);
    // O desvio em si não mudou nada que o jogo veja.
    assert_eq!(r.bytes(r.hook_addr, 5), r.hook_bytes());
    engine.undo_all().unwrap();
    r.assert_clean(&env.store);
}

#[test]
fn toggle_1000_with_thread() {
    let mut r = rig(false);
    let env = env();
    let mut engine = r.engine(&env);
    engine.claim_vars(r.region.clone()).unwrap();
    engine.write_var(r.var_n, &n_bytes(3)).unwrap();
    engine.apply(vec![r.cave_patch()]).unwrap();
    // Duas threads chamando `sum 2 3` em laço enquanto o desvio liga e desliga.
    r.t.start_loop("sum", &[2, 3]);
    r.t.start_loop("sum", &[2, 3]);
    for i in 0..1000 {
        engine.apply(vec![r.hook_patch()]).unwrap();
        // De vez em quando troca N sem pausa.
        if i % 50 == 0 {
            let n = if i % 100 == 0 { 7 } else { 3 };
            engine.write_var(r.var_n, &n_bytes(n)).unwrap();
        }
        engine.undo(&[HOOK_ID]).unwrap();
    }
    engine.apply(vec![r.hook_patch()]).unwrap();
    engine.write_var(r.var_n, &n_bytes(3)).unwrap();
    // A saída com as threads ainda rodando.
    engine.undo_all().unwrap();
    let stats = r.t.stop_loops();
    assert!(stats.calls > 0);
    // Só o original (5) ou o multiplicado (5 × 3 ou 5 × 7); nunca um valor intermediário.
    for result in &stats.results {
        assert!(
            [5, 15, 35].contains(&(*result as i32)),
            "resultado inesperado {result:#x} em {:?}",
            stats.results
        );
    }
    // Os dois lados aconteceram (senão o teste não provaria nada): original e multiplicado.
    assert!(stats.results.contains(&5), "{stats:?}");
    assert!(
        stats.results.iter().any(|v| *v as i32 != 5),
        "nenhuma chamada passou pela cave: {stats:?}"
    );
    assert_eq!(r.sum(2, 3), 5);
    r.assert_clean(&env.store);
}

#[test]
fn refuses_foreign_bytes() {
    let mut r = rig(false);
    let env = env();
    let mut engine = r.engine(&env);
    engine.claim_vars(r.region.clone()).unwrap();
    engine.apply(vec![r.cave_patch()]).unwrap();

    // 1. Instrução do desvio diferente da esperada (o jogo mudou): recusa, nada escrito.
    let other: Vec<u8> = vec![0x48, 0x89, 0x5C, 0x24, 0x10]; // mov [rsp+0x10],rbx
    let wrong = validate(PatchSpec {
        id: HOOK_ID.into(),
        addr: r.hook_addr,
        expected: other.clone(),
        replacement: r.hook_bytes(),
        kind: PatchKind::Code,
    })
    .unwrap();
    let err = engine.apply(vec![wrong]).unwrap_err();
    assert!(
        matches!(&err, Error::BytesMismatch { id, .. } if id == HOOK_ID),
        "{err:?}"
    );
    assert_eq!(r.bytes(r.hook_addr, 5), r.orig);
    assert_eq!(r.sum(2, 3), 5);
    assert!(engine.active().iter().all(|p| p.id != HOOK_ID));
    engine.undo_all().unwrap();
    r.assert_clean(&env.store);

    // 2. `CC` sobrescrito dentro do trecho da cave: recusa, nada escrito.
    let spoiled = r.cave_addr + 3;
    r.poke(spoiled, &[0x90]);
    let err = engine.apply(vec![r.cave_patch()]).unwrap_err();
    assert!(
        matches!(&err, Error::BytesMismatch { id, .. } if id == CAVE_ID),
        "{err:?}"
    );
    assert_eq!(r.bytes(spoiled, 1), [0x90]);
    assert_eq!(r.bytes(r.cave_addr, 3), [CAVE_FILL; 3]);
    assert!(engine.active().is_empty());
    assert!(engine.pending_ids().is_empty());
    // Uma cave cujo `expected` não é todo `CC` nem passa da validação.
    let err = validate(PatchSpec {
        id: CAVE_ID.into(),
        addr: r.cave_addr,
        expected: r.cave.clone(),
        replacement: r.cave.clone(),
        kind: PatchKind::Cave,
    })
    .unwrap_err();
    assert!(matches!(err, Error::BadPatch { .. }), "{err:?}");
    r.poke(spoiled, &[CAVE_FILL]);

    // 3. Folga com byte diferente de zero no primeiro uso: não é reclamada.
    let dirty = r.region.start + 100;
    r.poke(dirty, &[0x01]);
    let err = engine.claim_vars(r.region.clone()).unwrap_err();
    assert!(
        matches!(err, Error::VarsNotZero { addr } if addr == dirty),
        "{err:?}"
    );
    assert!(engine.vars().is_empty());
    r.poke(dirty, &[0x00]);

    // Nada disso deixou diário, pausa nem byte nosso.
    drop(engine);
    r.assert_clean(&env.store);
}

#[test]
fn cave_kept_while_thread_inside() {
    let mut r = rig(true);
    let env = env();
    let mut engine = r.engine(&env);
    engine.claim_vars(r.region.clone()).unwrap();
    engine.write_var(r.var_n, &n_bytes(3)).unwrap();
    engine.apply(vec![r.cave_patch()]).unwrap();
    engine.apply(vec![r.hook_patch()]).unwrap();
    assert_eq!(r.sum(2, 3), 15);
    // Uma thread presa dentro da cave (na cauda de espera, que só a `park` alcança).
    let tail = r.tail_addr.unwrap();
    let tid = r.t.park(tail);
    assert!(list_tids(r.t.pid).unwrap().contains(&tid));
    // `park` volta quando a thread já roda, não quando ela já chegou à cave: espera o `pc` dela
    // aparecer dentro da cave (sem isso, sob carga, o `undo_all` podia passar antes).
    wait_pc_in(&r, r.cave_addr..r.cave_addr + r.cave.len() as u64);

    // Sair: o desvio sai, a cave fica (5 tentativas de 100 ms) e vai para as pendentes.
    let started = Instant::now();
    let err = engine.undo_all().unwrap_err();
    assert!(
        matches!(&err, Error::ThreadsInPatch { id, attempts } if id == CAVE_ID && *attempts == 5),
        "{err:?}"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(400),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(r.bytes(r.hook_addr, 5), r.orig, "o desvio devia ter saído");
    assert_eq!(r.sum(2, 3), 5);
    assert_eq!(
        r.bytes(r.cave_addr, r.cave.len()),
        r.cave,
        "a cave devia ficar"
    );
    assert!(engine.active().is_empty());
    assert_eq!(engine.pending_ids(), vec![CAVE_ID.to_string()]);
    // A folga continua como está (a cave ainda pode gravar nas variáveis) e o diário guarda tudo.
    assert_eq!(r.bytes(r.var_n, 4), n_bytes(3));
    assert_eq!(engine.vars().len(), 1);
    match env.store.peek().unwrap() {
        Loaded::Ok(j) => {
            let ids: Vec<_> = j.entries.iter().map(|e| (e.id.as_str(), e.kind)).collect();
            assert_eq!(ids, vec![(CAVE_ID, PatchKind::Cave)]);
            assert_eq!(j.vars.len(), 1);
        }
        other => panic!("diário inesperado: {other:?}"),
    }
    // Com a thread ainda dentro, restaurar de novo também não apaga a cave.
    let err = engine.restore_pending().unwrap_err();
    assert!(matches!(err, Error::ThreadsInPatch { .. }), "{err:?}");
    assert_eq!(r.bytes(r.cave_addr, r.cave.len()), r.cave);
    assert_eq!(engine.pending_ids(), vec![CAVE_ID.to_string()]);

    // A thread sai da cave (a palavra que ela vigia muda): agora o `restore_pending` limpa a cave
    // e a folga.
    r.poke(r.flag, &1u32.to_le_bytes());
    let report = engine.restore_pending().unwrap();
    assert_eq!(report.restored, vec![CAVE_ID.to_string()], "{report:?}");
    assert!(engine.pending_ids().is_empty());
    assert!(engine.vars().is_empty());
    r.poke(r.flag, &0u32.to_le_bytes());
    r.assert_clean(&env.store);
}

/// Recuperação de um SIGKILL com um desvio (thread presa na cave) e um dado inteiro (como a
/// entrada `set` do `congelar-hora`): a cave não sai, mas o dado não pode sumir do diário antes
/// de resolvido. Uma recuperação seguinte, com a thread solta, resolve o resto.
#[test]
fn recovery_keeps_data_when_cave_restore_fails() {
    use gt_core::journal::{DataEntry, DataMode};
    use gt_core::recovery::recover;
    let mut r = rig(true);
    let env = env();
    let mut engine = r.engine(&env);
    engine.claim_vars(r.region.clone()).unwrap();
    engine.write_var(r.var_n, &n_bytes(3)).unwrap();
    engine.apply(vec![r.cave_patch()]).unwrap();
    engine.apply(vec![r.hook_patch()]).unwrap();
    let tail = r.tail_addr.unwrap();
    r.t.park(tail);
    wait_pc_in(&r, r.cave_addr..r.cave_addr + r.cave.len() as u64);
    // SIGKILL do trainer: o motor some sem desfazer nada.
    std::mem::forget(engine);
    // Um campo inteiro "congelado" (original 1.0, escrito 0.0), com a entrada no diário.
    let field = r.flag + 0x10;
    let original = 1.0f32.to_le_bytes().to_vec();
    let written = 0.0f32.to_le_bytes().to_vec();
    r.poke(field, &written);
    let entry = DataEntry {
        id: "congelar-hora".into(),
        addr: field,
        mode: DataMode::Whole,
        original: original.clone(),
        written: written.clone(),
        mask: 0,
        next: None,
        guard: Vec::new(),
    };
    let mut journal = match env.store.peek().unwrap() {
        Loaded::Ok(j) => j,
        other => panic!("diário inesperado: {other:?}"),
    };
    journal.data.push(entry.clone());
    env.store.save(&journal).unwrap();
    let journal_data = |env: &Env| match env.store.peek().unwrap() {
        Loaded::Ok(j) => j.data,
        other => panic!("diário inesperado: {other:?}"),
    };

    // O que a recuperação faz, passo a passo: com os dados adotados, o `Err` da cave não os tira
    // do disco. Um SIGKILL logo depois (o motor some) deixa o diário com o dado.
    let mut engine = r.engine(&env);
    engine.adopt_data(&journal.data);
    let err = engine.restore_entries(journal.entries.clone()).unwrap_err();
    assert!(matches!(err, Error::ThreadsInPatch { .. }), "{err:?}");
    assert_eq!(journal_data(&env), vec![entry.clone()]);
    std::mem::forget(engine);
    assert_eq!(r.bytes(field, 4), written);

    // A recuperação inteira: a cave não sai (thread dentro), mas o dado não se perde; a saída do
    // motor da recuperação o restaura (ele é pendente do motor, que tenta de novo no `Drop`).
    let err = recover(&env.store, &env.lock, &PauseConfig::default()).unwrap_err();
    assert!(matches!(err, Error::ThreadsInPatch { .. }), "{err:?}");
    assert_eq!(r.bytes(r.cave_addr, r.cave.len()), r.cave);
    assert_eq!(r.bytes(field, 4), original, "o dado devia ter voltado");
    match env.store.peek().unwrap() {
        Loaded::Ok(j) => {
            assert!(j.data.is_empty(), "{:?}", j.data);
            assert!(j.entries.iter().any(|e| e.id == CAVE_ID), "{j:?}");
        }
        other => panic!("diário inesperado: {other:?}"),
    }

    // A thread sai da cave: a recuperação seguinte resolve tudo.
    r.poke(r.flag, &1u32.to_le_bytes());
    let report = recover(&env.store, &env.lock, &PauseConfig::default()).unwrap();
    assert!(report.kept.is_empty(), "{report:?}");
    assert_eq!(r.bytes(field, 4), original);
    assert_eq!(
        r.bytes(r.cave_addr, r.cave.len()),
        vec![CAVE_FILL; r.cave.len()]
    );
    r.poke(r.flag, &0u32.to_le_bytes());
    r.poke(field, &[0; 4]);
    r.assert_clean(&env.store);
}

// ---------------------------------------------------------------------------
// Harness: SIGKILL, pânico e sinais
// ---------------------------------------------------------------------------

/// Linhas de `stdout` de um filho que ainda roda.
struct Live {
    child: Child,
    reader: BufReader<ChildStdout>,
    /// Tudo que o filho imprimiu até agora (uma linha por item).
    seen: Vec<String>,
}

impl Live {
    fn new(mut cmd: Command) -> Live {
        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let reader = BufReader::new(child.stdout.take().unwrap());
        Live {
            child,
            reader,
            seen: Vec::new(),
        }
    }

    /// Lê até a linha `want`; pânico se o filho fechar antes.
    fn wait_line(&mut self, want: &str) {
        loop {
            let mut line = String::new();
            let n = self.reader.read_line(&mut line).unwrap();
            if n == 0 {
                let mut err = String::new();
                if let Some(mut e) = self.child.stderr.take() {
                    e.read_to_string(&mut err).unwrap();
                }
                panic!(
                    "o harness fechou sem imprimir {want:?}; stdout: {:?}; stderr: {err}",
                    self.seen
                );
            }
            let line = line.trim_end().to_string();
            let done = line == want;
            self.seen.push(line);
            if done {
                return;
            }
        }
    }

    fn signal(&self, sig: Signal) {
        kill(Pid::from_raw(self.child.id() as i32), sig).unwrap();
    }

    /// Espera o filho terminar e devolve o status e o resto do `stdout`.
    fn finish(mut self) -> (ExitStatus, String) {
        let start = Instant::now();
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            if start.elapsed() > EXIT_TIMEOUT {
                let _ = self.child.kill();
                panic!("o harness não terminou no prazo");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let mut rest = String::new();
        self.reader.read_to_string(&mut rest).unwrap();
        (status, rest)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `gt-patch-harness cave-run` desta fixture, com opções globais (`--hook`, `--hook-pause`).
fn cave_run(r: &Rig, dir: &Path, globals: &[&str]) -> Command {
    let mut cmd = Command::new(bin_path("gt-patch-harness"));
    cmd.arg("--journal-dir").arg(dir);
    cmd.args([
        "--pid".to_string(),
        r.t.pid.to_string(),
        "--allow".to_string(),
        format!("{:#x}:{}", r.text.start, r.text.end - r.text.start),
    ]);
    cmd.args(globals);
    cmd.args([
        "cave-run".to_string(),
        "--vars".to_string(),
        format!("{:#x}:{}", r.region.start, r.region.end - r.region.start),
        "--var".to_string(),
        format!("{:#x}:{}", r.var_n, hex(&n_bytes(3))),
        "--cave-addr".to_string(),
        format!("{:#x}", r.cave_addr),
        "--cave".to_string(),
        hex(&r.cave),
        "--addr".to_string(),
        format!("{:#x}", r.hook_addr),
        "--expected".to_string(),
        hex(&r.orig),
        "--replacement".to_string(),
        hex(&r.hook_bytes()),
    ]);
    cmd
}

fn restore(dir: &Path) -> Output {
    let out = Command::new(bin_path("gt-patch-harness"))
        .arg("--journal-dir")
        .arg(dir)
        .arg("restore")
        .output()
        .unwrap();
    assert!(out.status.success(), "restore falhou: {out:?}");
    out
}

fn journal(dir: &Path) -> Loaded {
    JournalStore::new(dir.to_path_buf()).peek().unwrap()
}

/// `assert_clean` do `Rig`, com o diário do harness em `dir`.
fn assert_clean_in(r: &Rig, dir: &Path) {
    r.assert_clean(&JournalStore::new(dir.to_path_buf()));
}

#[test]
fn sigkill_between_cave_and_hook() {
    // 1. A próxima execução do harness (a recuperação do `cave-run`) devolve tudo; 2. o
    // `restore` também. As duas partem de um SIGKILL dentro da pausa do desvio, depois de a
    // cave e as variáveis estarem escritas e antes de o desvio ser escrito.
    for via_restore in [true, false] {
        let mut r = rig(false);
        let dir = tempfile::tempdir().unwrap();
        let out = cave_run(
            &r,
            dir.path(),
            &["--hook", "kill-in-pause", "--hook-pause", "2"],
        )
        .output()
        .unwrap();
        assert!(!out.status.success(), "{out:?}");
        // A cave e a variável estão escritas, o desvio não; o alvo ficou parado por nós.
        assert_eq!(r.bytes(r.cave_addr, r.cave.len()), r.cave);
        assert_eq!(r.bytes(r.var_n, 4), n_bytes(3));
        assert_eq!(r.bytes(r.hook_addr, 5), r.orig);
        assert!(states(r.t.pid).iter().all(|s| *s == 'T'));
        match journal(dir.path()) {
            Loaded::Ok(j) => {
                assert!(j.paused_by_us);
                let mut ids: Vec<_> = j.entries.iter().map(|e| e.id.as_str()).collect();
                ids.sort();
                assert_eq!(ids, vec![HARNESS_HOOK_ID, HARNESS_CAVE_ID]);
                assert_eq!(j.vars.len(), 1);
            }
            other => panic!("diário inesperado: {other:?}"),
        }

        let recovery = if via_restore {
            let out = restore(dir.path());
            String::from_utf8_lossy(&out.stdout).into_owned()
        } else {
            // A próxima execução: recupera, instala de novo e sai por SIGTERM.
            let mut live = Live::new(cave_run(&r, dir.path(), &[]));
            live.wait_line("ativo");
            let printed = live.seen.join("\n");
            assert_eq!(r.sum(2, 3), 15, "o desvio da segunda execução");
            live.signal(Signal::SIGTERM);
            let (status, rest) = live.finish();
            assert_eq!(status.code(), Some(0), "{status:?}");
            assert!(rest.contains("restaurado"), "{rest}");
            printed
        };
        assert!(recovery.contains("retomado"), "{recovery}");
        assert!(
            recovery.contains(&format!("restaurado: {HARNESS_CAVE_ID}")),
            "{recovery}"
        );
        // O desvio nunca foi escrito: só esquecido.
        assert!(
            recovery.contains(&format!("já original: {HARNESS_HOOK_ID}")),
            "{recovery}"
        );
        assert_eq!(r.sum(2, 3), 5);
        assert_clean_in(&r, dir.path());
    }
}

#[test]
fn sigkill_with_hook_active() {
    let mut r = rig(false);
    let dir = tempfile::tempdir().unwrap();
    let mut live = Live::new(cave_run(&r, dir.path(), &[]));
    live.wait_line("ativo");
    assert_eq!(r.sum(2, 3), 15);
    live.child.kill().unwrap();
    live.child.wait().unwrap();
    // Tudo continua no jogo: o desvio, a cave e a variável.
    assert_eq!(r.bytes(r.hook_addr, 5), r.hook_bytes());
    assert_eq!(r.sum(2, 3), 15);
    let out = restore(dir.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&format!("restaurado: {HARNESS_HOOK_ID}")),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("restaurado: {HARNESS_CAVE_ID}")),
        "{stdout}"
    );
    assert_eq!(r.sum(2, 3), 5);
    assert_clean_in(&r, dir.path());
}

#[test]
fn panic_and_sigterm_restore() {
    // SIGTERM com o desvio ligado.
    let mut r = rig(false);
    let dir = tempfile::tempdir().unwrap();
    let mut live = Live::new(cave_run(&r, dir.path(), &[]));
    live.wait_line("ativo");
    assert_eq!(r.sum(2, 3), 15);
    live.signal(Signal::SIGTERM);
    let (status, rest) = live.finish();
    assert_eq!(status.code(), Some(0), "{status:?}");
    assert!(rest.contains("restaurado"), "{rest}");
    assert_eq!(r.sum(2, 3), 5);
    assert_clean_in(&r, dir.path());

    // Pânico com o desvio ligado e pânico entre a cave e o desvio: o `Drop` devolve tudo.
    for hook in ["panic-after-hook", "panic-after-apply"] {
        let mut r = rig(false);
        let dir = tempfile::tempdir().unwrap();
        let out = cave_run(&r, dir.path(), &["--hook", hook])
            .output()
            .unwrap();
        assert!(!out.status.success(), "{hook}: {out:?}");
        assert_eq!(r.sum(2, 3), 5, "{hook}");
        assert_clean_in(&r, dir.path());
    }
}
