#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Corpos das caves (spec 008, 6.6.1, 6.7.1, 6.7.2 e 6.7.3) rodando de verdade no módulo sintético
//! do `gt-testtarget --module`: o motor escreve a cave (montada por `gt_grounded2::caves`) e o
//! desvio, e os testes chamam as funções-alvo (`level_gain`, `getter`, `drag`, `sum`).

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::time::{Duration, Instant};

use gt_core::cave::{CAVE_MARGIN, CaveAllocator, cc_runs, hook_bytes};
use gt_core::engine::{Engine, EngineOptions, PolicyFn};
use gt_core::journal::{JournalLock, JournalStore};
use gt_core::maps::{MapRegion, read_maps};
use gt_core::mem::{MemReader, MemWriter};
use gt_core::modules::{self, Module, TrustPolicy};
use gt_core::patch::{CAVE_FILL, PatchKind, PatchSpec, validate};
use gt_core::pe::{DiskImage, PeHeaders};
use gt_core::scan::Pattern;
use gt_core::suspend::PauseConfig;
use gt_grounded2::caves::{self, CaveCtx, var};
use gt_grounded2::policy::{var_region, write_policy};
use gt_grounded2::signatures::CaveBody;
use gt_testkit::module::{
    ANALYZER_CHARGES, ANALYZER_MAX_POWER, ITEM_STACK, RESEARCH_LEVEL, RESEARCH_POINTS, TABLE_COUNT,
    TABLE_LIMIT, TABLE_STRIDE,
};
use gt_testkit::{TargetOpts, TestTarget};
use gt_ue::UeLayout;
use iced_x86::{Decoder, DecoderOptions, FlowControl, Mnemonic, Register};

/// Padrão estendido do Brain Power (spec 008, 6.6.1): a ocorrência inteira vai em `captures`.
const BRAIN_PATTERN: &str = "8b b1 ?? ?? 00 00 4d 89 73 ?? 89 75 ?? 8d 04 16 48 8b 51 ?? 89 81 ?? ?? 00 00 e8 ?? ?? ?? ?? e8 ?? ?? ?? ?? 4c 8b b0 ?? ?? 00 00 48 63 80 ?? ?? 00 00 48 8d 0c 40 48 c1 e1 04 49 03 ce 4c 3b f1 0f 84 ?? ?? ?? ?? 0f 1f 80 00 00 00 00 41 8b 46 ?? 3b f0 7d ?? 3b 83 ?? ?? 00 00";

const INT32_MAX: i32 = i32::MAX;

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

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

/// O alvo com o módulo sintético, o motor e o alocador de caves.
struct Rig {
    t: TestTarget,
    text_start: u64,
    region: Range<u64>,
    alloc: CaveAllocator,
    engine: Engine,
    _env: Env,
    caves: usize,
}

fn rig() -> Rig {
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
    let module: Module = modules::find(&found, "gt-synthetic.exe").unwrap().clone();
    let disk = DiskImage::load(t.module_path()).unwrap();
    let headers: PeHeaders = disk.headers.clone();
    let section = headers.section(".text").unwrap().clone();
    let text_disk = disk
        .bytes(section.rva, section.virtual_size as usize)
        .unwrap()
        .to_vec();
    let text_start = module.base + u64::from(section.rva);
    let region = var_region(&module, &headers, &maps).expect("folga da .data");
    assert!(region.end - region.start >= var::TOTAL);
    let alloc = CaveAllocator::new(cc_runs(&text_disk, text_start, 64));

    let env = env();
    let policy: PolicyFn =
        Box::new(move |maps: &[MapRegion]| write_policy(&module, &headers, maps));
    let opts = EngineOptions {
        version_key: "teste".into(),
        pause: PauseConfig::default(),
    };
    let mut engine = Engine::open(t.pid, None, env.store.clone(), &env.lock, policy, opts).unwrap();
    engine.claim_vars(region.clone()).unwrap();
    Rig {
        t,
        text_start,
        region,
        alloc,
        engine,
        _env: env,
        caves: 0,
    }
}

fn fields() -> HashMap<&'static str, u32> {
    HashMap::from([
        ("ResearchPoints", RESEARCH_POINTS as u32),
        ("StackSize", ITEM_STACK as u32),
        ("MaxPowerCapacity", ANALYZER_MAX_POWER as u32),
    ])
}

impl Rig {
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

    fn i32_at(&self, addr: u64) -> i32 {
        i32::from_le_bytes(self.bytes(addr, 4).try_into().unwrap())
    }

    fn u64_at(&self, addr: u64) -> u64 {
        u64::from_le_bytes(self.bytes(addr, 8).try_into().unwrap())
    }

    fn f32_at(&self, addr: u64) -> f32 {
        f32::from_le_bytes(self.bytes(addr, 4).try_into().unwrap())
    }

    fn var(&self, off: u64) -> u64 {
        self.region.start + off
    }

    fn set_var(&mut self, off: u64, bytes: &[u8]) {
        let at = self.var(off);
        self.engine.write_var(at, bytes).unwrap();
    }

    fn var_i32(&self, off: u64) -> i32 {
        self.i32_at(self.var(off))
    }

    /// Contexto da cave para um desvio em `hook_at` (instrução de `len` bytes), com a cave em
    /// `cave_at` e a ocorrência do padrão em `captures`.
    fn ctx(&self, hook_at: u64, len: usize, cave_at: u64, captures: Vec<u8>) -> CaveCtx {
        CaveCtx {
            hook_at,
            cave_at,
            insn: self.bytes(hook_at, len),
            vars: self.region.start,
            captures,
            fields: fields(),
            layout: UeLayout::observed(),
        }
    }

    /// Monta a cave com `build`, escreve-a numa sequência de `CC` e liga o desvio.
    fn install(
        &mut self,
        hook_at: u64,
        len: usize,
        captures: Vec<u8>,
        build: impl Fn(&CaveCtx) -> Vec<u8>,
    ) -> (u64, Vec<u8>) {
        // Mede numa posição qualquer da `.text`, aloca e monta de novo no lugar certo.
        let probe = build(&self.ctx(hook_at, len, self.text_start + 0x800, captures.clone()));
        let cave_at = self.alloc.alloc(probe.len()).expect("sequência de CC");
        let cave = build(&self.ctx(hook_at, len, cave_at, captures));
        assert_eq!(
            cave.len(),
            probe.len(),
            "o tamanho da cave não depende do lugar"
        );
        self.caves += 1;
        let cave_id = format!("cave-{}", self.caves);
        let hook_id = format!("hook-{}", self.caves);
        let orig = self.bytes(hook_at, len);
        self.engine
            .apply(vec![
                validate(PatchSpec {
                    id: cave_id,
                    addr: cave_at,
                    expected: vec![CAVE_FILL; cave.len()],
                    replacement: cave.clone(),
                    kind: PatchKind::Cave,
                })
                .unwrap(),
            ])
            .unwrap();
        self.engine
            .apply(vec![
                validate(PatchSpec {
                    id: hook_id,
                    addr: hook_at,
                    expected: orig,
                    replacement: hook_bytes(hook_at, len, cave_at).unwrap(),
                    kind: PatchKind::Code,
                })
                .unwrap(),
            ])
            .unwrap();
        (cave_at, cave)
    }

    /// Sai: desvios, caves e variáveis devolvidos.
    fn finish(mut self) {
        self.engine.undo_all().unwrap();
        let slack = self.bytes(
            self.region.start,
            (self.region.end - self.region.start) as usize,
        );
        assert!(slack.iter().all(|b| *b == 0));
    }
}

// ---------------------------------------------------------------------------
// Brain Power
// ---------------------------------------------------------------------------

/// Rig com a cave do Brain Power ligada em `level_point` e o multiplicador `n`.
fn brain_rig(n: i32) -> Rig {
    let mut r = rig();
    let len = Pattern::parse(BRAIN_PATTERN).unwrap().len();
    let captures = r.bytes(r.t.symbol("level_pattern"), len);
    let point = r.t.symbol("level_point");
    r.set_var(var::BRAIN_N, &n.to_le_bytes());
    r.install(point, 7, captures, |ctx| {
        caves::build(CaveBody::Brain, ctx).unwrap()
    });
    r
}

impl Rig {
    fn research(&self) -> u64 {
        self.t.symbol("research_obj")
    }

    fn points(&self) -> i32 {
        self.i32_at(self.research() + RESEARCH_POINTS as u64)
    }

    fn level(&self) -> i32 {
        self.i32_at(self.research() + RESEARCH_LEVEL as u64)
    }

    fn set_points(&self, p: i32) {
        self.poke(self.research() + RESEARCH_POINTS as u64, &p.to_le_bytes());
        self.poke(self.research() + RESEARCH_LEVEL as u64, &0i32.to_le_bytes());
    }

    /// Reescreve a tabela de níveis (limites e contagem).
    fn set_table(&self, limits: &[i32], count: i32) {
        let entries = self.t.symbol("level_entries");
        for (i, l) in limits.iter().enumerate() {
            let at = entries + (i as u64) * TABLE_STRIDE as u64 + TABLE_LIMIT as u64;
            self.poke(at, &l.to_le_bytes());
        }
        let obj = self.t.symbol("level_table_obj");
        self.poke(obj + TABLE_COUNT as u64, &count.to_le_bytes());
    }

    /// `level_gain(research_obj, gain)`: devolve o nível depois do ganho.
    fn gain(&mut self, gain: i32) -> i32 {
        let obj = self.research();
        self.t.call("level_gain", &[obj, gain as i64 as u64]) as i32
    }

    /// Registro do Brain Power: (ganho, aplicado, L1, L2, sequência).
    fn brain_log(&self) -> (i32, i32, i32, i32, u32) {
        (
            self.var_i32(var::BRAIN_GAIN),
            self.var_i32(var::BRAIN_APPLIED),
            self.var_i32(var::BRAIN_L1),
            self.var_i32(var::BRAIN_L2),
            self.var_i32(var::BRAIN_SEQ) as u32,
        )
    }
}

#[test]
fn brain_multiplies() {
    let mut r = brain_rig(3);
    r.set_points(0);
    // 10 × 3 = 30: abaixo do primeiro limite (100), sem nível.
    assert_eq!(r.gain(10), 0);
    assert_eq!(r.points(), 30);
    // 30 + 20 × 3 = 90.
    assert_eq!(r.gain(20), 0);
    assert_eq!(r.points(), 90);
    // Troca N pela variável: 90 + 5 × 2 = 100, atravessa o limite 100: um nível.
    r.set_var(var::BRAIN_N, &2i32.to_le_bytes());
    assert_eq!(r.gain(5), 1);
    assert_eq!(r.points(), 100);
    // `rbx`, `rsi`, `rbp` e `r14` do jogo preservados: a função sintética devolveu o nível certo
    // e as chamadas seguintes continuam funcionando.
    r.set_var(var::BRAIN_N, &0i32.to_le_bytes());
    r.set_points(0);
    assert_eq!(r.gain(10), 0);
    assert_eq!(r.points(), 10, "N = 0 passa direto");
    r.finish();
}

#[test]
fn brain_caps_at_l2_minus_one() {
    let mut r = brain_rig(10);
    r.set_points(50);
    // 100 × 10 = 1000, limitado a L2 − 1 = 299: atravessa só o limite 100 (um nível).
    assert_eq!(r.gain(100), 1);
    assert_eq!(r.points(), 299);
    assert_eq!(r.level(), 1, "um nível só");
    // Do 299: L1 = 300, L2 = 700; 50 × 10 = 500 → limitado a 699 − 299 = 400.
    r.set_points(299);
    assert_eq!(r.gain(50), 1);
    assert_eq!(r.points(), 699);
    r.finish();
}

#[test]
fn brain_never_below_original_gain() {
    let mut r = brain_rig(2);
    r.set_points(50);
    // O ganho original (400) já atravessa 100 e 300; ×2 = 800 seria limitado a 249, mas o ganho
    // nunca diminui: fica 400.
    r.gain(400);
    assert_eq!(r.points(), 450);
    let (gain, applied, l1, l2, _) = r.brain_log();
    assert_eq!((gain, applied, l1, l2), (400, 400, 100, 300));
    r.finish();
}

#[test]
fn brain_unsorted_table() {
    let mut r = brain_rig(100);
    // Fora de ordem e com o próximo limite repetido: L1 = 100, L2 = 300 (distinto).
    r.set_table(&[3000, 100, 700, 100, 300, 50], 6);
    r.set_points(60);
    assert_eq!(r.gain(10), 1);
    assert_eq!(r.points(), 299);
    let (gain, applied, l1, l2, _) = r.brain_log();
    assert_eq!((gain, applied, l1, l2), (10, 239, 100, 300));

    // Contagem acima de 256 ou negativa: não multiplica. A primeira entrada (55) faz o laço do
    // jogo parar nela, sem ler além da tabela.
    for count in [257, -1, i32::MIN] {
        r.set_table(&[55], count);
        r.set_points(50);
        assert_eq!(r.gain(10), 1, "contagem {count}");
        assert_eq!(r.points(), 60, "contagem {count}");
    }
    // Contagem válida de novo: multiplica. Tabela [55, 100, 700, 100, 300, 50]: L1 = 55,
    // L2 = 100, ganho limitado a 100 − 1 − 50 = 49.
    r.set_table(&[55], 6);
    r.set_points(50);
    assert_eq!(r.gain(10), 1);
    assert_eq!(r.points(), 99);
    let (_, applied, l1, l2, _) = r.brain_log();
    assert_eq!((applied, l1, l2), (49, 55, 100));
    r.finish();
}

#[test]
fn brain_no_l2_caps_int32() {
    let mut r = brain_rig(10);
    // Acima do penúltimo limite: só L1 = 6000, sem L2. O ganho só é limitado pelo int32.
    r.set_points(5000);
    r.gain(1_000_000);
    assert_eq!(r.points(), 10_005_000);
    let (_, applied, l1, l2, _) = r.brain_log();
    assert_eq!((applied, l1, l2), (10_000_000, 6000, 0));
    // Acima de todos os limites: L1 = L2 = 0; ×10 passaria do int32: fica no máximo.
    r.set_points(7000);
    r.gain(1_000_000_000);
    assert_eq!(r.points(), INT32_MAX);
    let (gain, applied, l1, l2, _) = r.brain_log();
    assert_eq!(
        (gain, applied, l1, l2),
        (1_000_000_000, INT32_MAX - 7000, 0, 0)
    );
    // Tabela vazia: sem limites, só o int32.
    r.set_table(&[], 0);
    r.set_points(0);
    r.gain(7);
    assert_eq!(r.points(), 70);
    r.finish();
}

#[test]
fn brain_nonpositive_gain_untouched() {
    let mut r = brain_rig(5);
    r.set_points(200);
    assert_eq!(r.gain(0), 0);
    assert_eq!(r.points(), 200);
    assert_eq!(r.gain(-30), 0);
    assert_eq!(r.points(), 170);
    // Nada registrado.
    assert_eq!(r.brain_log(), (0, 0, 0, 0, 0));
    // N negativo também passa direto.
    r.set_var(var::BRAIN_N, &(-3i32).to_le_bytes());
    assert_eq!(r.gain(10), 0);
    assert_eq!(r.points(), 180);
    assert_eq!(r.brain_log().4, 0);
    r.finish();
}

#[test]
fn brain_log_vars_and_even_seq() {
    let mut r = brain_rig(5);
    r.set_points(0);
    r.gain(8);
    assert_eq!(r.brain_log(), (8, 40, 100, 300, 2));
    r.gain(4);
    // 40 + 20 = 60.
    assert_eq!(r.brain_log(), (4, 20, 100, 300, 4));
    assert_eq!(r.points(), 60);
    // Ganho zero não registra: a sequência fica.
    r.gain(0);
    assert_eq!(r.brain_log().4, 4);
    r.finish();
}

// ---------------------------------------------------------------------------
// Analisador
// ---------------------------------------------------------------------------

fn analyzer_rig() -> Rig {
    let mut r = rig();
    let getter = r.t.symbol("getter");
    let insn = r.bytes(getter, 8);
    r.install(getter, 8, insn, |ctx| {
        caves::build(CaveBody::Analyzer, ctx).unwrap()
    });
    r
}

impl Rig {
    fn set_analyzer(&self, obj: u64, charges: f32, max: i32) {
        self.poke(obj + ANALYZER_CHARGES as u64, &charges.to_le_bytes());
        self.poke(obj + ANALYZER_MAX_POWER as u64, &max.to_le_bytes());
    }

    fn charges(&self, obj: u64) -> f32 {
        self.f32_at(obj + ANALYZER_CHARGES as u64)
    }

    fn analyzer_on(&mut self, class: u64) {
        self.set_var(var::ANALYZER_CLASS, &class.to_le_bytes());
        self.set_var(var::ANALYZER_ON, &1u64.to_le_bytes());
    }
}

#[test]
fn analyzer_fills_only_right_class() {
    let mut r = analyzer_rig();
    let analyzer = r.t.symbol("analyzer_obj");
    let other = r.t.symbol("other_obj");
    r.set_analyzer(analyzer, 3.0, 30);
    r.set_analyzer(other, 3.0, 30);
    r.analyzer_on(r.t.symbol("class_analyzer"));
    // O getter devolve as cargas cheias e elas ficam gravadas.
    assert_eq!(r.t.call("getter", &[analyzer]) as i32, 30);
    assert_eq!(r.charges(analyzer), 30.0);
    // Outra classe: nada muda.
    assert_eq!(r.t.call("getter", &[other]) as i32, 3);
    assert_eq!(r.charges(other), 3.0);
    // Subclasse (o analisador da classe "Object" como alvo: tudo é subclasse dela).
    r.set_analyzer(analyzer, 1.0, 12);
    r.set_var(
        var::ANALYZER_CLASS,
        &r.t.symbol("class_object").to_le_bytes(),
    );
    assert_eq!(r.t.call("getter", &[analyzer]) as i32, 12);
    // `MaxPowerCapacity` zero ou negativo: não grava.
    r.set_analyzer(analyzer, 2.0, 0);
    assert_eq!(r.t.call("getter", &[analyzer]) as i32, 2);
    r.set_analyzer(analyzer, 2.0, -5);
    assert_eq!(r.t.call("getter", &[analyzer]) as i32, 2);
    assert_eq!(r.charges(analyzer), 2.0);
    r.finish();
}

#[test]
fn analyzer_off_passes_through() {
    let mut r = analyzer_rig();
    let analyzer = r.t.symbol("analyzer_obj");
    r.set_analyzer(analyzer, 4.0, 30);
    // Variáveis zeradas.
    assert_eq!(r.t.call("getter", &[analyzer]) as i32, 4);
    // Ligado sem a classe.
    r.set_var(var::ANALYZER_ON, &1u64.to_le_bytes());
    assert_eq!(r.t.call("getter", &[analyzer]) as i32, 4);
    // Classe sem ligado.
    r.set_var(var::ANALYZER_ON, &0u64.to_le_bytes());
    r.set_var(
        var::ANALYZER_CLASS,
        &r.t.symbol("class_analyzer").to_le_bytes(),
    );
    assert_eq!(r.t.call("getter", &[analyzer]) as i32, 4);
    assert_eq!(r.charges(analyzer), 4.0);
    r.finish();
}

#[test]
fn class_walk_stops_at_16_levels() {
    let mut r = analyzer_rig();
    let layout = UeLayout::observed();
    // Uma cadeia de 18 classes: a classe do objeto é a 0, cada uma tem a seguinte como
    // `SuperStruct`. A subida só lê o `SuperStruct` (`+0x40`), então as "classes" podem se
    // sobrepor com passo 8. Ficam entre a última classe fixa do módulo (`class_other`) e o começo
    // da folga das variáveis, sem tocar nenhuma das duas.
    let super_off = u64::from(layout.ustruct.super_struct);
    let first = r.t.symbol("class_other") + 0x80;
    let chain: Vec<u64> = (0..18).map(|i| first + i * 8).collect();
    let written = chain[0] + super_off..chain[17] + super_off + 8;
    assert!(written.start >= r.t.symbol("class_other") + super_off + 8);
    assert!(written.end <= r.region.start, "a cadeia entra na folga");
    for w in chain.windows(2) {
        r.poke(w[0] + super_off, &w[1].to_le_bytes());
    }
    let obj = r.t.symbol("other_obj");
    r.poke(
        obj + u64::from(layout.uobject.class),
        &chain[0].to_le_bytes(),
    );
    r.set_analyzer(obj, 1.0, 30);
    // A classe do objeto e 16 subidas: até `chain[16]` acha; `chain[17]` não.
    r.analyzer_on(chain[16]);
    assert_eq!(r.t.call("getter", &[obj]) as i32, 30);
    r.set_analyzer(obj, 1.0, 30);
    r.set_var(var::ANALYZER_CLASS, &chain[17].to_le_bytes());
    assert_eq!(r.t.call("getter", &[obj]) as i32, 1);
    assert_eq!(r.charges(obj), 1.0);
    r.finish();
}

// ---------------------------------------------------------------------------
// Arrastar
// ---------------------------------------------------------------------------

fn drag_rig() -> Rig {
    let mut r = rig();
    let drag = r.t.symbol("drag");
    let insn = r.bytes(drag, 5);
    r.set_var(var::ITEM_CLASS, &r.t.symbol("class_item").to_le_bytes());
    r.install(drag, 5, insn, |ctx| {
        caves::build(CaveBody::Drag, ctx).unwrap()
    });
    r
}

impl Rig {
    fn drag(&mut self, item: u64, n: i32) -> i32 {
        self.t.call("drag", &[item, n as i64 as u64]) as i32
    }

    fn stack(&self, item: u64) -> i32 {
        self.i32_at(item + ITEM_STACK as u64)
    }

    fn set_stack(&self, item: u64, n: i32) {
        self.poke(item + ITEM_STACK as u64, &n.to_le_bytes());
    }

    fn applied(&self) -> u64 {
        self.u64_at(self.var(var::DRAG_APPLIED))
    }
}

#[test]
fn drag_applies_once_and_counts() {
    let mut r = drag_rig();
    let (a, b) = (r.t.symbol("item_a"), r.t.symbol("item_b"));
    // Desarmado: nada muda.
    assert_eq!(r.drag(a, 0), 1);
    assert_eq!(r.applied(), 0);
    // Armado com 250: o próximo item recebe 250 e a variável volta a 0.
    r.set_var(var::DRAG_N, &250i32.to_le_bytes());
    assert_eq!(r.drag(a, 0), 250);
    assert_eq!(r.stack(a), 250);
    assert_eq!(r.var_i32(var::DRAG_N), 0);
    assert_eq!(r.applied(), 1);
    // Só uma vez.
    assert_eq!(r.drag(b, 0), 5);
    assert_eq!(r.applied(), 1);
    // Subclasse de `Item` (item_b) também; `rdx` e o resto da função preservados (+ n).
    r.set_var(var::DRAG_N, &999i32.to_le_bytes());
    assert_eq!(r.drag(b, 1), 1000);
    assert_eq!(r.stack(b), 999);
    assert_eq!(r.applied(), 2);
    r.finish();
}

#[test]
fn drag_two_threads_apply_once() {
    let mut r = drag_rig();
    let (a, b) = (r.t.symbol("item_a"), r.t.symbol("item_b"));
    r.t.start_loop("drag", &[a, 0]);
    r.t.start_loop("drag", &[b, 0]);
    for i in 0..40i32 {
        r.set_stack(a, 1);
        r.set_stack(b, 5);
        let n = 100 + i;
        // Escrita simples (o `write_var` do motor relê e falharia: a cave zera a variável logo).
        r.poke(r.var(var::DRAG_N), &n.to_le_bytes());
        let deadline = Instant::now() + Duration::from_secs(5);
        while r.var_i32(var::DRAG_N) != 0 {
            assert!(Instant::now() < deadline, "ninguém aplicou o arme {i}");
            std::thread::sleep(Duration::from_millis(1));
        }
        // Mais um pouco de laço: um segundo `xchg` não pode aplicar de novo.
        std::thread::sleep(Duration::from_millis(5));
        let (sa, sb) = (r.stack(a), r.stack(b));
        assert!(
            (sa == n && sb == 5) || (sa == 1 && sb == n),
            "arme {i}: pilhas {sa} e {sb}"
        );
        assert_eq!(r.applied(), (i + 1) as u64);
    }
    let stats = r.t.stop_loops();
    assert!(stats.calls > 0);
    r.finish();
}

#[test]
fn drag_wrong_class_or_null_untouched() {
    let mut r = drag_rig();
    let other = r.t.symbol("other_obj");
    r.set_stack(other, 7);
    r.set_var(var::DRAG_N, &50i32.to_le_bytes());
    // Classe errada: não aplica e continua armado.
    assert_eq!(r.drag(other, 0), 7);
    assert_eq!(r.var_i32(var::DRAG_N), 50);
    // Ponteiro nulo: a função devolve -1 e nada muda.
    assert_eq!(r.drag(0, 0), -1);
    assert_eq!(r.var_i32(var::DRAG_N), 50);
    // Ponteiro da classe zerado: não aplica.
    r.set_var(var::ITEM_CLASS, &0u64.to_le_bytes());
    let a = r.t.symbol("item_a");
    assert_eq!(r.drag(a, 0), 1);
    assert_eq!(r.var_i32(var::DRAG_N), 50);
    assert_eq!(r.applied(), 0);
    r.finish();
}

// ---------------------------------------------------------------------------
// Observação
// ---------------------------------------------------------------------------

#[test]
fn observe_counts_and_rings_without_side_effects() {
    for reg in [Register::RCX, Register::RDX, Register::RAX] {
        let mut r = rig();
        let sum = r.t.symbol("sum");
        let insn = r.bytes(sum, 5);
        r.install(sum, 5, insn, |ctx| caves::build_observe(ctx, reg).unwrap());
        let calls = 11u64;
        for i in 0..calls {
            let (x, y) = (i as i32, 1000 + i as i32);
            assert_eq!(r.t.call("sum", &[x as u64, y as u64]) as i32, x + y);
        }
        assert_eq!(r.u64_at(r.var(var::OBSERVE_COUNT)), calls);
        // O retorno é o mesmo em todas as entradas e fica num trecho executável do alvo.
        let maps = read_maps(r.t.pid).unwrap();
        let ret0 = r.u64_at(r.var(var::OBSERVE_RING));
        assert!(
            maps.iter()
                .any(|m| m.perms.exec && (m.start..m.end).contains(&ret0)),
            "{ret0:#x} fora de código"
        );
        for slot in 0..var::OBSERVE_RING_LEN {
            let at = r.var(var::OBSERVE_RING) + slot * var::OBSERVE_ENTRY;
            assert_eq!(r.u64_at(at), ret0, "{reg:?} slot {slot}");
            // A última chamada que caiu neste slot.
            let last = (0..calls)
                .filter(|k| k % var::OBSERVE_RING_LEN == slot)
                .max()
                .unwrap();
            let want = match reg {
                Register::RCX => last,
                Register::RDX => 1000 + last,
                _ => continue,
            };
            assert_eq!(r.u64_at(at + 8), want, "{reg:?} slot {slot}");
        }
        r.finish();
    }
}

// ---------------------------------------------------------------------------
// Conferência estática de todos os corpos
// ---------------------------------------------------------------------------

/// `insn` e `captures` têm de vir do mesmo achado: a instrução trocada é o `mov r14,[rax+T0]` do
/// padrão, com o mesmo `T0` das capturas.
#[test]
fn brain_refuses_an_insn_from_another_hit() {
    let r = rig();
    let len = Pattern::parse(BRAIN_PATTERN).unwrap().len();
    let captures = r.bytes(r.t.symbol("level_pattern"), len);
    let point = r.t.symbol("level_point");
    let cave_at = r.text_start + 0x900;
    // A instrução certa monta.
    let ctx = r.ctx(point, 7, cave_at, captures.clone());
    assert!(caves::build(CaveBody::Brain, &ctx).is_ok());
    // Outra instrução (o `mov [rsp+8],rbx` de `sum`, de 5 bytes): incoerente.
    let ctx = r.ctx(r.t.symbol("sum"), 5, cave_at, captures.clone());
    let err = caves::build(CaveBody::Brain, &ctx).unwrap_err();
    assert!(
        matches!(err, gt_grounded2::Error::IncoherentPattern { .. }),
        "{err}"
    );
    // O mesmo `mov r14,[rax+X]`, mas com outro `X` que o `T0` das capturas.
    let mut ctx = r.ctx(point, 7, cave_at, captures.clone());
    ctx.insn[3] ^= 0x08;
    let err = caves::build(CaveBody::Brain, &ctx).unwrap_err();
    assert!(
        matches!(err, gt_grounded2::Error::IncoherentPattern { .. }),
        "{err}"
    );
    // Outra base (`mov r14,[rcx+T0]`).
    let mut ctx = r.ctx(point, 7, cave_at, captures);
    ctx.insn[2] = 0xb1;
    assert!(caves::build(CaveBody::Brain, &ctx).is_err());
}

#[test]
fn all_bodies_pass_check() {
    let r = rig();
    let len = Pattern::parse(BRAIN_PATTERN).unwrap().len();
    let captures = r.bytes(r.t.symbol("level_pattern"), len);
    let cave_at = r.text_start + 0x900;
    let mut built: Vec<(String, u64, Vec<u8>)> = Vec::new();
    for (name, body, hook, n, caps) in [
        ("brain", CaveBody::Brain, "level_point", 7, captures.clone()),
        ("analyzer", CaveBody::Analyzer, "getter", 8, vec![]),
        ("drag", CaveBody::Drag, "drag", 5, vec![]),
    ] {
        let at = r.t.symbol(hook);
        let ctx = r.ctx(at, n, cave_at, caps);
        built.push((
            name.into(),
            at + n as u64,
            caves::build(body, &ctx).unwrap(),
        ));
    }
    for reg in [Register::RAX, Register::RCX, Register::R11, Register::RSP] {
        let at = r.t.symbol("sum");
        let ctx = r.ctx(at, 5, cave_at, vec![]);
        built.push((
            format!("observe {reg:?}"),
            at + 5,
            caves::build_observe(&ctx, reg).unwrap(),
        ));
    }
    for (name, back, code) in &built {
        gt_core::cave::check_cave_code(code, cave_at, *back).unwrap();
        let insns: Vec<_> = Decoder::with_ip(64, code, cave_at, DecoderOptions::NONE)
            .into_iter()
            .collect();
        for i in &insns {
            assert!(!i.is_invalid(), "{name}");
            assert!(
                !matches!(
                    i.flow_control(),
                    FlowControl::Call
                        | FlowControl::IndirectCall
                        | FlowControl::Return
                        | FlowControl::Interrupt
                        | FlowControl::IndirectBranch
                ),
                "{name}: {i}"
            );
        }
        // Salva e devolve as flags; a última instrução é o `jmp` de volta.
        assert_eq!(
            insns.first().unwrap().mnemonic(),
            Mnemonic::Pushfq,
            "{name}"
        );
        assert_eq!(insns.last().unwrap().mnemonic(), Mnemonic::Jmp, "{name}");
        // Antes da instrução original, tudo já foi devolvido (`popfq` logo antes dela).
        assert_eq!(insns[insns.len() - 3].mnemonic(), Mnemonic::Popfq, "{name}");
        assert!(
            code.len() + CAVE_MARGIN <= 300,
            "{name}: {} bytes",
            code.len()
        );
    }
    // Registrador inválido para a observação e corpo da coleta (Tarefa 12): erro.
    let at = r.t.symbol("sum");
    let ctx = r.ctx(at, 5, cave_at, vec![]);
    assert!(caves::build_observe(&ctx, Register::EAX).is_err());
    assert!(caves::build_observe(&ctx, Register::RIP).is_err());
    assert!(caves::build(CaveBody::Collect, &ctx).is_err());
    // Campo da reflexão faltando: erro.
    let mut ctx = r.ctx(r.t.symbol("drag"), 5, cave_at, vec![]);
    ctx.fields.remove("StackSize");
    assert!(caves::build(CaveBody::Drag, &ctx).is_err());
    // Brain Power com `ResearchPoints` diferente do padrão: padrão incoerente.
    let mut ctx = r.ctx(r.t.symbol("level_point"), 7, cave_at, captures);
    ctx.fields
        .insert("ResearchPoints", RESEARCH_POINTS as u32 + 4);
    assert!(caves::build(CaveBody::Brain, &ctx).is_err());
    // Analisador num ponto que não é `movss xmm,[rcx+C]`: erro.
    let ctx = r.ctx(r.t.symbol("drag"), 5, cave_at, vec![]);
    assert!(caves::build(CaveBody::Analyzer, &ctx).is_err());
}
