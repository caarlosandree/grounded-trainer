#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `HookRuntime` (spec 008, 6.3, 6.5, 6.6.1, 6.7.2, 6.7.4 e 6.11) contra o módulo sintético: o
//! motor de verdade escreve as caves e os desvios no `gt-testtarget --module`, e a reflexão vem de
//! uma cena sintética local (`SynthUe`), só para o `resolve` e para o inventário do personagem.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gt_core::cave::CaveAllocator;
use gt_core::engine::Engine;
use gt_core::mem::{MemReader, MemWriter};
use gt_core::patch::{CAVE_FILL, PatchKind};
use gt_grounded2::catalog::HookHit;
use gt_grounded2::caves::var;
use gt_grounded2::hooks::{
    ArmOutcome, DRAG_EXPIRY, HookAvail, HookEnv, HookEvent, HookRuntime, PawnView,
};
use gt_grounded2::signatures::{CaveBody, HookDef, Signatures};
use gt_testkit::module::{ITEM_STACK, RESEARCH_POINTS};
use gt_testkit::rig::{MODULE_NAME, SynthRig};
use gt_ue::synth::{SparseMem, SynthField, SynthUe};
use gt_ue::{Names, ObjectArray, ReflectionIndex, ScanBudget, UeLayout};

const ARENA: u64 = 0x1000_0000;
const ARENA_LEN: usize = 64 << 20;
const MAINE: &str = "/Script/Maine";
const BRAIN: &str = "brain-power";
const ANALYZER: &str = "analisador-infinito";
const DRAG: &str = "quantidade-arrastar";

/// `PartyComponent.ResearchPoints` no módulo sintético e os outros campos da cena.
const MAX_POWER: u32 = 0x980;
const INVENTORY_OFF: u32 = 0x48;
/// Inventários dos dois personagens da cena (endereços de usuário quaisquer).
const INV_A: u64 = 0x7000_1000;
const INV_B: u64 = 0x7000_2000;

fn field(name: &'static str, kind: &'static str, offset: u32, size: u32) -> SynthField {
    SynthField {
        name,
        kind,
        offset,
        size,
        bool_mask: None,
    }
}

/// A reflexão sintética: as quatro classes de que o `HookRuntime` precisa e dois personagens.
struct Refl {
    mem: SparseMem,
    index: ReflectionIndex,
    pawn_a: u64,
    pawn_b: u64,
}

/// `research_off` é o `disp` de `ResearchPoints` da reflexão; `omit` tira campos pelo nome.
fn refl(research_off: u32, omit: &[&str]) -> Refl {
    let layout = UeLayout::observed();
    let mut s = SynthUe::new(layout, ARENA, ARENA_LEN);
    let keep = |fields: Vec<SynthField>| -> Vec<SynthField> {
        fields
            .into_iter()
            .filter(|f| !omit.contains(&f.name))
            .collect()
    };
    let (object_c, actor) = (s.object_class, s.actor_class);
    let maine = s.add_package(MAINE);
    s.add_class(
        maine,
        "PartyComponent",
        Some(object_c),
        research_off + 8,
        &keep(vec![field(
            "ResearchPoints",
            "IntProperty",
            research_off,
            4,
        )]),
    );
    s.add_class(
        maine,
        "Item",
        Some(object_c),
        ITEM_STACK as u32 + 8,
        &keep(vec![field(
            "StackSize",
            "IntProperty",
            ITEM_STACK as u32,
            4,
        )]),
    );
    s.add_class(
        maine,
        "ResourceAnalyzerBuilding",
        Some(object_c),
        MAX_POWER + 8,
        &keep(vec![field("MaxPowerCapacity", "IntProperty", MAX_POWER, 4)]),
    );
    let character = s.add_class(
        maine,
        "SurvivalPlayerCharacter",
        Some(actor),
        INVENTORY_OFF + 0x10,
        &keep(vec![field(
            "InventoryComponent",
            "ObjectProperty",
            INVENTORY_OFF,
            8,
        )]),
    );
    let pawn_a = s.add_object(character, "Pawn_A", maine);
    let pawn_b = s.add_object(character, "Pawn_B", maine);
    s.set(pawn_a, INVENTORY_OFF, &INV_A.to_le_bytes());
    s.set(pawn_b, INVENTORY_OFF, &INV_B.to_le_bytes());
    let image = s.finish();
    let mem = image.into_mem();
    let names = Names::new(&mem, image.names).unwrap();
    let objects = ObjectArray::read(&mem, &layout, image.gobjects).unwrap();
    let index =
        ReflectionIndex::build(&mem, &layout, &objects, &names, &ScanBudget::standard()).unwrap();
    Refl {
        mem,
        index,
        pawn_a,
        pawn_b,
    }
}

/// Alvo, motor, runtime e reflexão de uma cena. O motor vem antes do alvo: cai primeiro.
struct Scene {
    engine: Engine,
    rt: HookRuntime,
    refl: Refl,
    defs: Vec<HookDef>,
    rig: SynthRig,
}

/// Como montar a cena.
struct Opts {
    refl: Refl,
    /// Corta a folga (bytes) ou a tira (0).
    var_len: Option<u64>,
    /// Sem sequência de `CC` livre.
    no_runs: bool,
}

fn default_opts() -> Opts {
    Opts {
        refl: refl(RESEARCH_POINTS as u32, &[]),
        var_len: None,
        no_runs: false,
    }
}

/// O ambiente de um runtime novo sobre o módulo sintético, com o `[ue.player]` do genérico.
fn make_env(rig: &SynthRig) -> HookEnv {
    let (module, mem_headers) = rig.target.module(MODULE_NAME).unwrap();
    let disk = rig.target.open_disk(module, &mem_headers).unwrap();
    let player = Signatures::embedded()
        .unwrap()
        .ue_for_version("0000-0000")
        .unwrap()
        .player;
    HookEnv::new(
        module.clone(),
        Arc::new(disk),
        &rig.target.maps,
        UeLayout::observed(),
        player,
    )
}

fn scene() -> Scene {
    scene_with(default_opts())
}

fn scene_with(opts: Opts) -> Scene {
    let rig = SynthRig::new();
    let mut env = make_env(&rig);
    if let Some(len) = opts.var_len {
        env.var_region = env
            .var_region
            .take()
            .filter(|_| len > 0)
            .map(|r| r.start..r.start + len);
    }
    if opts.no_runs {
        env.allocator = CaveAllocator::new(Vec::new());
    }
    let engine = rig.engine();
    let defs = Signatures::embedded().unwrap().hooks().to_vec();
    Scene {
        engine,
        rt: HookRuntime::new(env),
        refl: opts.refl,
        defs,
        rig,
    }
}

impl Scene {
    fn def(&self, id: &str) -> HookDef {
        self.defs.iter().find(|d| d.id == id).unwrap().clone()
    }

    fn bytes(&self, addr: u64, len: usize) -> Vec<u8> {
        let mut buf = vec![0u8; len];
        MemReader::new(self.rig.t.pid)
            .read_exact(addr, &mut buf)
            .unwrap();
        buf
    }

    fn poke(&self, addr: u64, bytes: &[u8]) {
        MemWriter::open(self.rig.t.pid)
            .unwrap()
            .write(addr, bytes)
            .unwrap();
    }

    /// A ocorrência do padrão de `id` no módulo sintético.
    fn hit(&self, id: &str) -> HookHit {
        let def = self.def(id);
        let start = match id {
            BRAIN => self.rig.t.symbol("level_pattern"),
            ANALYZER => self.rig.t.symbol("getter"),
            _ => self.rig.t.symbol("drag"),
        };
        HookHit {
            addr: start + def.offset as u64,
            captures: self.bytes(start, def.pattern.len()),
            def,
        }
    }

    fn hits(&self) -> Vec<HookHit> {
        [BRAIN, ANALYZER, DRAG].map(|id| self.hit(id)).to_vec()
    }

    fn resolve(&mut self) -> HashMap<String, HookAvail> {
        let hits = self.hits();
        self.rt
            .resolve(&hits, &self.refl.mem, &self.refl.index)
            .into_iter()
            .collect()
    }

    fn var_at(&self, off: u64) -> u64 {
        self.rig.var_region().start + off
    }

    fn var_i32(&self, off: u64) -> i32 {
        i32::from_le_bytes(self.bytes(self.var_at(off), 4).try_into().unwrap())
    }

    fn var_u64(&self, off: u64) -> u64 {
        u64::from_le_bytes(self.bytes(self.var_at(off), 8).try_into().unwrap())
    }

    fn tick(&mut self, pawn: Option<u64>, now: Instant) -> Vec<HookEvent> {
        self.rt.tick(
            &mut self.engine,
            &self.refl.mem,
            pawn.map(PawnView::from),
            now,
        )
    }

    fn points(&self) -> i32 {
        let at = self.rig.t.symbol("research_obj") + RESEARCH_POINTS as u64;
        i32::from_le_bytes(self.bytes(at, 4).try_into().unwrap())
    }

    fn set_points(&self, p: i32) {
        let at = self.rig.t.symbol("research_obj") + RESEARCH_POINTS as u64;
        self.poke(at, &p.to_le_bytes());
    }

    /// Um ganho de pesquisa pela função do módulo.
    fn gain(&mut self, gain: i32) {
        let obj = self.rig.t.symbol("research_obj");
        self.rig
            .t
            .call("level_gain", &[obj, u64::from(gain as u32)]);
    }

    fn engine_ids(&self) -> Vec<String> {
        self.engine
            .active()
            .iter()
            .map(|s| s.id.clone())
            .collect::<Vec<_>>()
    }

    /// Bytes do ponto do Brain Power na memória.
    fn level_point_bytes(&self) -> Vec<u8> {
        self.bytes(self.rig.t.symbol("level_point"), 7)
    }
}

// ---------------------------------------------------------------------------
// resolve
// ---------------------------------------------------------------------------

#[test]
fn resolve_makes_the_three_hooks_available() {
    let mut s = scene();
    let got = s.resolve();
    assert_eq!(got.len(), 3);
    for id in [BRAIN, ANALYZER, DRAG] {
        assert_eq!(got[id], HookAvail::Available, "{id}");
    }
    // O inventário do personagem vem da reflexão, sem hook de coleta.
    assert_eq!(s.rt.inventory_offset(), Some(INVENTORY_OFF));
    // Nada foi escrito só por resolver.
    assert!(s.engine.active().is_empty());
    assert!(s.engine.vars().is_empty());
}

#[test]
fn unavailable_reasons() {
    // Bytes da instrução diferentes do disco.
    let mut s = scene();
    let mut hit = s.hit(BRAIN);
    hit.captures[hit.def.offset + 3] ^= 0x01;
    let got = s.rt.resolve(&[hit], &s.refl.mem, &s.refl.index);
    assert_eq!(got[0].1, HookAvail::Unavailable("bytes diferentes".into()));

    // `disp` de ResearchPoints do padrão diferentes entre si.
    let mut hit = s.hit(BRAIN);
    hit.captures[2] ^= 0x01;
    let got = s.rt.resolve(&[hit], &s.refl.mem, &s.refl.index);
    assert_eq!(got[0].1, HookAvail::Unavailable("padrão incoerente".into()));

    // `disp` do padrão iguais, mas diferentes do da reflexão.
    let mut other = scene_with(Opts {
        refl: refl(RESEARCH_POINTS as u32 + 8, &[]),
        ..default_opts()
    });
    assert_eq!(
        other.resolve()[BRAIN],
        HookAvail::Unavailable("padrão incoerente".into())
    );

    // Campo da reflexão que falta: só o desvio que o usa fica indisponível.
    let mut no_max = scene_with(Opts {
        refl: refl(RESEARCH_POINTS as u32, &["MaxPowerCapacity"]),
        ..default_opts()
    });
    let got = no_max.resolve();
    assert_eq!(
        got[ANALYZER],
        HookAvail::Unavailable("campo MaxPowerCapacity não encontrado".into())
    );
    assert_eq!(got[BRAIN], HookAvail::Available);
    assert_eq!(got[DRAG], HookAvail::Available);
    let mut no_stack = scene_with(Opts {
        refl: refl(RESEARCH_POINTS as u32, &["StackSize"]),
        ..default_opts()
    });
    assert_eq!(
        no_stack.resolve()[DRAG],
        HookAvail::Unavailable("campo StackSize não encontrado".into())
    );

    // Sem sequência de CC livre.
    let mut no_runs = scene_with(Opts {
        no_runs: true,
        ..default_opts()
    });
    for (id, avail) in no_runs.resolve() {
        assert_eq!(
            avail,
            HookAvail::Unavailable("sem espaço para a cave".into()),
            "{id}"
        );
    }
}

/// As classes e campos vêm das assinaturas (`[ue.player]` e `object_class` do `[[hook]]`).
#[test]
fn classes_come_from_the_signatures() {
    let mut s = scene();
    // Sem o `ResearchPoints` no `[ue.player]` o Brain Power não tem de onde conferir o `disp`.
    s.rt = HookRuntime::new({
        let mut env = make_env(&s.rig);
        env.player.research = None;
        env
    });
    let got = s.resolve();
    assert_eq!(
        got[BRAIN],
        HookAvail::Unavailable("campo ResearchPoints não encontrado".into())
    );
    // Outra classe no `object_class` do arrastar: a da reflexão não é a pedida.
    let mut hit = s.hit(DRAG);
    hit.def.object_class = Some("/Script/Maine.Outra".into());
    let got = s.rt.resolve(&[hit], &s.refl.mem, &s.refl.index);
    assert_eq!(
        got[0].1,
        HookAvail::Unavailable("classe Outra não encontrada".into())
    );
    // Sem o campo do inventário no `[ue.player]`, o inventário não é mantido.
    s.rt = HookRuntime::new({
        let mut env = make_env(&s.rig);
        env.player.inventory_field = None;
        env
    });
    s.resolve();
    assert_eq!(s.rt.inventory_offset(), None);
}

#[test]
fn unavailable_without_room_for_vars_cuts_by_cheat_with_dependencies() {
    // Sem folga nenhuma: tudo fica indisponível.
    let mut none = scene_with(Opts {
        var_len: Some(0),
        ..default_opts()
    });
    for (id, avail) in none.resolve() {
        assert_eq!(
            avail,
            HookAvail::Unavailable("sem espaço para variáveis".into()),
            "{id}"
        );
    }
    // 100 bytes: o Brain Power (24) e o analisador (56) cabem; o arrastar leva a classe `Item`
    // (até 144) e não cabe.
    let mut small = scene_with(Opts {
        var_len: Some(100),
        ..default_opts()
    });
    let got = small.resolve();
    assert_eq!(got[BRAIN], HookAvail::Available);
    assert_eq!(got[ANALYZER], HookAvail::Available);
    assert_eq!(
        got[DRAG],
        HookAvail::Unavailable("sem espaço para variáveis".into())
    );
    // 20 bytes: nem o Brain Power.
    let mut tiny = scene_with(Opts {
        var_len: Some(16),
        ..default_opts()
    });
    assert!(
        tiny.resolve()
            .values()
            .all(|a| matches!(a, HookAvail::Unavailable(r) if r == "sem espaço para variáveis"))
    );
}

#[test]
fn caves_of_the_three_hooks_fit_together_without_overlap() {
    let mut s = scene();
    s.resolve();
    for id in [BRAIN, ANALYZER, DRAG] {
        s.rt.enable(&mut s.engine, id, None).unwrap();
    }
    let mut spans: Vec<(u64, u64)> = s
        .engine
        .active()
        .iter()
        .filter(|p| p.kind == PatchKind::Cave)
        .map(|p| (p.addr, p.addr + p.replacement.len() as u64))
        .collect();
    spans.sort();
    assert_eq!(spans.len(), 3);
    assert!(spans.windows(2).all(|w| w[0].1 <= w[1].0), "{spans:x?}");
}

// ---------------------------------------------------------------------------
// enable, set_value, disable
// ---------------------------------------------------------------------------

#[test]
fn enable_writes_vars_then_patches() {
    let mut s = scene();
    s.resolve();
    let original = s.level_point_bytes();
    // Com o ponto alterado por outro, o lote falha inteiro; as variáveis já foram escritas
    // (e a folga já está reclamada), mas nem a cave nem o desvio.
    let mut other = original.clone();
    other[6] ^= 0xff;
    s.poke(s.rig.t.symbol("level_point"), &other);
    let err = s.rt.enable(&mut s.engine, BRAIN, Some(2)).unwrap_err();
    assert!(err.to_string().contains("bytes diferentes"), "{err}");
    assert_eq!(s.var_i32(var::BRAIN_N), 5, "a variável veio antes do lote");
    assert!(s.engine.active().is_empty());
    assert_eq!(s.engine.vars().len(), 1);
    assert!(!s.rt.is_hooked(BRAIN));
    s.poke(s.rig.t.symbol("level_point"), &original);

    // Agora liga: cave e desvio no mesmo lote, ids do diário conhecidos.
    s.rt.enable(&mut s.engine, BRAIN, Some(2)).unwrap();
    assert!(s.rt.is_hooked(BRAIN));
    assert_eq!(
        s.engine_ids(),
        [
            "brain-power-cave".to_string(),
            "brain-power-hook".to_string()
        ]
    );
    assert_eq!(s.level_point_bytes()[0], 0xE9, "o desvio é um jmp rel32");
    // ×5 de verdade: ganho 10 → +50.
    s.set_points(0);
    s.gain(10);
    assert_eq!(s.points(), 50);
}

#[test]
fn set_value_only_writes_var() {
    let mut s = scene();
    s.resolve();
    s.rt.enable(&mut s.engine, BRAIN, Some(0)).unwrap();
    let before = s.engine.active().len();
    let code = s.level_point_bytes();
    let cave = s.bytes(s.rig.t.symbol("level_point"), 7);
    s.rt.set_value(&mut s.engine, BRAIN, 3).unwrap();
    // Só a variável mudou: o código e as entradas do motor são as mesmas.
    assert_eq!(s.var_i32(var::BRAIN_N), 10);
    assert_eq!(s.level_point_bytes(), code);
    assert_eq!(s.bytes(s.rig.t.symbol("level_point"), 7), cave);
    assert_eq!(s.engine.active().len(), before);
    s.set_points(0);
    s.gain(4);
    assert_eq!(s.points(), 40);
    // Desligado, o valor também pode mudar; religar usa o valor do arme.
    s.rt.disable(&mut s.engine, BRAIN).unwrap();
    s.rt.set_value(&mut s.engine, BRAIN, 1).unwrap();
    assert_eq!(s.var_i32(var::BRAIN_N), 3);
    // O arrastar e o analisador não têm valor de variável: recusa.
    s.rt.enable(&mut s.engine, ANALYZER, None).unwrap();
    assert!(s.rt.set_value(&mut s.engine, ANALYZER, 0).is_err());
    // Desconhecido.
    assert!(s.rt.set_value(&mut s.engine, "nao-existe", 0).is_err());
}

#[test]
fn disable_keeps_cave() {
    let mut s = scene();
    s.resolve();
    let original = s.level_point_bytes();
    s.rt.enable(&mut s.engine, BRAIN, None).unwrap();
    let jumped = s.level_point_bytes();
    assert_ne!(jumped, original);
    let cave_at = s
        .engine
        .active()
        .iter()
        .find(|p| p.kind == PatchKind::Cave)
        .map(|p| (p.addr, p.replacement.clone()))
        .unwrap();
    s.rt.disable(&mut s.engine, BRAIN).unwrap();
    assert!(!s.rt.is_hooked(BRAIN));
    assert_eq!(s.level_point_bytes(), original, "o desvio voltou");
    // A cave e as variáveis ficam; o diário guarda só a cave.
    assert_eq!(s.bytes(cave_at.0, cave_at.1.len()), cave_at.1);
    assert_ne!(
        s.bytes(cave_at.0, cave_at.1.len()),
        vec![CAVE_FILL; cave_at.1.len()]
    );
    assert_eq!(s.engine_ids(), ["brain-power-cave".to_string()]);
    assert_eq!(s.engine.vars().len(), 1);
    // Sem o desvio, ganhar pontos não multiplica.
    s.set_points(0);
    s.gain(10);
    assert_eq!(s.points(), 10);
    // Desligar de novo é um no-op.
    s.rt.disable(&mut s.engine, BRAIN).unwrap();
    // Religar reaproveita a cave: só o desvio entra e o valor passa a valer.
    s.rt.enable(&mut s.engine, BRAIN, Some(1)).unwrap();
    assert_eq!(s.level_point_bytes(), jumped);
    assert_eq!(
        s.engine_ids(),
        [
            "brain-power-cave".to_string(),
            "brain-power-hook".to_string()
        ]
    );
    s.set_points(0);
    s.gain(10);
    assert_eq!(s.points(), 30);
    // Sair: tudo devolvido.
    s.engine.undo_all().unwrap();
    assert_eq!(s.level_point_bytes(), original);
}

#[test]
fn enable_refuses_unavailable_and_unknown() {
    let mut s = scene_with(Opts {
        refl: refl(RESEARCH_POINTS as u32, &["MaxPowerCapacity"]),
        ..default_opts()
    });
    s.resolve();
    let err = s.rt.enable(&mut s.engine, ANALYZER, None).unwrap_err();
    assert!(err.to_string().contains("MaxPowerCapacity"), "{err}");
    let err = s.rt.enable(&mut s.engine, "nao-existe", None).unwrap_err();
    assert!(err.to_string().contains("nao-existe"), "{err}");
    assert!(s.engine.active().is_empty());
    // Nada claimed: o motor segue sem região reclamada.
    assert!(s.engine.vars().is_empty());
}

#[test]
fn analyzer_enable_sets_class_and_on_before_the_hook() {
    let mut s = scene();
    s.resolve();
    s.rt.enable(&mut s.engine, ANALYZER, None).unwrap();
    assert_eq!(s.var_u64(var::ANALYZER_ON), 1);
    assert_ne!(s.var_u64(var::ANALYZER_CLASS), 0);
    // Desligar não mexe nas variáveis (6.5).
    s.rt.disable(&mut s.engine, ANALYZER).unwrap();
    assert_eq!(s.var_u64(var::ANALYZER_ON), 1);
}

#[test]
fn installed_hook_stays_available_without_the_pattern() {
    let mut s = scene();
    s.resolve();
    s.rt.enable(&mut s.engine, BRAIN, Some(1)).unwrap();
    // Nova varredura: o nosso desvio tirou o padrão (sem ocorrência). O resolve o mantém.
    let got = s.rt.resolve(&[], &s.refl.mem, &s.refl.index);
    assert_eq!(got, vec![(BRAIN.to_string(), HookAvail::Available)]);

    // Um runtime novo sobre o mesmo motor reconhece o desvio pelo diário e segue controlando-o.
    let mut fresh = HookRuntime::new(make_env(&s.rig));
    assert_eq!(fresh.recognize_installed(&s.engine, &s.defs), [BRAIN]);
    assert!(fresh.is_hooked(BRAIN));
    fresh.set_value(&mut s.engine, BRAIN, 3).unwrap();
    assert_eq!(s.var_i32(var::BRAIN_N), 10);
    let original_after_off = {
        fresh.disable(&mut s.engine, BRAIN).unwrap();
        s.level_point_bytes()
    };
    assert_ne!(original_after_off[0], 0xE9);
    // E religa só com o desvio (a cave continua).
    fresh.enable(&mut s.engine, BRAIN, Some(0)).unwrap();
    assert_eq!(s.level_point_bytes()[0], 0xE9);
    s.engine.undo_all().unwrap();
}

/// Os trechos das caves ativas do motor, ordenados.
fn cave_spans(s: &Scene) -> Vec<(u64, u64)> {
    let mut spans: Vec<(u64, u64)> = s
        .engine
        .active()
        .iter()
        .filter(|p| p.kind == PatchKind::Cave)
        .map(|p| (p.addr, p.addr + p.replacement.len() as u64))
        .collect();
    spans.sort();
    spans
}

#[test]
fn recognized_caves_are_not_overlapped_by_a_new_runtime() {
    let mut s = scene();
    s.resolve();
    s.rt.enable(&mut s.engine, BRAIN, None).unwrap();
    let brain_cave = cave_spans(&s)[0];
    // Runtime novo sobre o mesmo motor: o padrão do Brain Power sumiu (o nosso desvio o tirou),
    // então só o analisador e o arrastar vêm como ocorrência.
    s.rt = HookRuntime::new(make_env(&s.rig));
    assert_eq!(s.rt.recognize_installed(&s.engine, &s.defs), [BRAIN]);
    let hits = [s.hit(ANALYZER), s.hit(DRAG)];
    let got = s.rt.resolve(&hits, &s.refl.mem, &s.refl.index);
    assert_eq!(
        got,
        vec![
            (ANALYZER.to_string(), HookAvail::Available),
            (DRAG.to_string(), HookAvail::Available),
            (BRAIN.to_string(), HookAvail::Available),
        ]
    );
    s.rt.enable(&mut s.engine, ANALYZER, None).unwrap();
    s.rt.enable(&mut s.engine, DRAG, None).unwrap();
    let spans = cave_spans(&s);
    assert_eq!(spans.len(), 3);
    assert!(spans.contains(&brain_cave));
    assert!(spans.windows(2).all(|w| w[0].1 <= w[1].0), "{spans:x?}");
    // O Brain Power segue sob controle do runtime novo.
    s.rt.set_value(&mut s.engine, BRAIN, 2).unwrap();
    s.set_points(0);
    s.gain(10);
    assert_eq!(s.points(), 50);
}

#[test]
fn a_cave_without_its_hook_is_recognized_and_reused() {
    let mut s = scene();
    s.resolve();
    s.rt.enable(&mut s.engine, BRAIN, None).unwrap();
    s.rt.disable(&mut s.engine, BRAIN).unwrap();
    let brain_cave = cave_spans(&s)[0];
    // Reconectou com o cheat desligado: a cave continua no jogo, sem desvio.
    s.rt = HookRuntime::new(make_env(&s.rig));
    assert!(s.rt.recognize_installed(&s.engine, &s.defs).is_empty());
    let got = s.resolve();
    assert!(got.values().all(|a| *a == HookAvail::Available), "{got:?}");
    // Os outros desvios não caem em cima dela.
    s.rt.enable(&mut s.engine, ANALYZER, None).unwrap();
    s.rt.enable(&mut s.engine, DRAG, None).unwrap();
    // Religar o Brain Power reaproveita a cave: entra só o desvio, no mesmo lugar.
    s.rt.enable(&mut s.engine, BRAIN, Some(2)).unwrap();
    let spans = cave_spans(&s);
    assert_eq!(spans.len(), 3, "{spans:x?}");
    assert!(spans.contains(&brain_cave));
    assert!(spans.windows(2).all(|w| w[0].1 <= w[1].0), "{spans:x?}");
    s.set_points(0);
    s.gain(10);
    assert_eq!(s.points(), 50);
    s.engine.undo_all().unwrap();
}

// ---------------------------------------------------------------------------
// Brain Power: registro
// ---------------------------------------------------------------------------

#[test]
fn brain_log_skips_counted() {
    let mut s = scene();
    s.resolve();
    s.rt.enable(&mut s.engine, BRAIN, Some(0)).unwrap();
    let now = Instant::now();
    s.set_points(0);
    // Nada aconteceu: nenhum evento.
    assert!(s.tick(None, now).is_empty());
    // Um ganho: um evento, sem ganhos escondidos (×2: +4 vira +8; o próximo nível é 100).
    s.gain(4);
    assert_eq!(
        s.tick(None, now),
        [HookEvent::BrainGain {
            gain: 4,
            applied: 8,
            l1: 100,
            skipped: 0
        }]
    );
    // Sem novo ganho, sem evento.
    assert!(s.tick(None, now).is_empty());
    // Três ganhos entre dois ciclos: o último e `(+2 ganhos não mostrados)`.
    s.gain(1);
    s.gain(2);
    s.gain(3);
    assert_eq!(
        s.tick(None, now),
        [HookEvent::BrainGain {
            gain: 3,
            applied: 6,
            l1: 100,
            skipped: 2
        }]
    );
    // Desligado, o registro não é lido.
    s.rt.disable(&mut s.engine, BRAIN).unwrap();
    s.poke(s.var_at(var::BRAIN_SEQ), &100u32.to_le_bytes());
    assert!(s.tick(None, now).is_empty());
}

#[test]
fn brain_log_starts_from_the_sequence_at_enable() {
    let mut s = scene();
    s.resolve();
    s.rt.enable(&mut s.engine, BRAIN, None).unwrap();
    s.gain(5);
    s.gain(5);
    s.rt.disable(&mut s.engine, BRAIN).unwrap();
    // Religar: o que aconteceu antes não é mostrado de novo.
    s.rt.enable(&mut s.engine, BRAIN, None).unwrap();
    assert!(s.tick(None, Instant::now()).is_empty());
    s.gain(5);
    assert_eq!(s.tick(None, Instant::now()).len(), 1);
}

#[test]
fn tick_before_any_enable_does_nothing() {
    let mut s = scene();
    s.resolve();
    let pawn = s.refl.pawn_a;
    assert!(s.tick(Some(pawn), Instant::now()).is_empty());
    assert!(s.engine.vars().is_empty());
}

// ---------------------------------------------------------------------------
// Arrastar
// ---------------------------------------------------------------------------

/// O jogo "aplicou" o arme: variável a 0 e o contador de aplicadas a mais.
fn game_applies(s: &Scene) {
    let counter = s.var_u64(var::DRAG_APPLIED) + 1;
    s.poke(s.var_at(var::DRAG_N), &0i32.to_le_bytes());
    s.poke(s.var_at(var::DRAG_APPLIED), &counter.to_le_bytes());
}

fn drag_scene() -> Scene {
    let mut s = scene();
    s.resolve();
    s
}

#[test]
fn drag_arm_installs_and_a_second_press_disarms() {
    let mut s = drag_scene();
    assert!(!s.rt.is_hooked(DRAG));
    // Arma com 250 (k = 3 da grade 10, 50, 100, 250, 500, 999).
    assert_eq!(s.rt.arm(&mut s.engine, 3).unwrap(), ArmOutcome::Armed);
    assert!(s.rt.is_hooked(DRAG), "armar instala o desvio");
    assert_eq!(s.var_i32(var::DRAG_N), 250);
    assert_eq!(s.rt.armed().map(|(n, _)| n), Some(250));
    // Apertar de novo, sem o jogo ter aplicado: desarma (a variável volta a 0).
    assert_eq!(s.rt.arm(&mut s.engine, 3).unwrap(), ArmOutcome::Disarmed);
    assert_eq!(s.var_i32(var::DRAG_N), 0);
    assert_eq!(s.rt.armed(), None);
    // O desvio continua (só sai com a saída).
    assert!(s.rt.is_hooked(DRAG));
    // Armar de novo com 10 e o jogo aplica antes do desarme: `Applied`.
    assert_eq!(s.rt.arm(&mut s.engine, 0).unwrap(), ArmOutcome::Armed);
    assert_eq!(s.var_i32(var::DRAG_N), 10);
    game_applies(&s);
    assert_eq!(s.rt.arm(&mut s.engine, 0).unwrap(), ArmOutcome::Applied);
    assert_eq!(s.rt.armed(), None);
    s.engine.undo_all().unwrap();
}

#[test]
fn drag_disarm_by_counter() {
    let mut s = drag_scene();
    s.rt.arm(&mut s.engine, 2).unwrap();
    // O contador sobe enquanto o trainer espera os 50 ms do desarme: o jogo aplicou. A cave zera
    // a variável antes de contar; aqui a thread do teste faz o mesmo dentro da janela.
    let (pid, n_at, counter_at) = (
        s.rig.t.pid,
        s.var_at(var::DRAG_N),
        s.var_at(var::DRAG_APPLIED),
    );
    let helper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(15));
        let w = MemWriter::open(pid).unwrap();
        w.write(counter_at, &1u64.to_le_bytes()).unwrap();
        w.write(n_at, &0i32.to_le_bytes()).unwrap();
    });
    assert_eq!(s.rt.arm(&mut s.engine, 2).unwrap(), ArmOutcome::Applied);
    helper.join().unwrap();
    assert_eq!(s.var_i32(var::DRAG_N), 0);
    // Sem o contador subir, é `Disarmed`.
    s.rt.arm(&mut s.engine, 2).unwrap();
    assert_eq!(s.rt.arm(&mut s.engine, 2).unwrap(), ArmOutcome::Disarmed);
}

#[test]
fn drag_tick_reports_applied_and_clears_the_arm() {
    let mut s = drag_scene();
    let now = Instant::now();
    s.rt.arm_at(&mut s.engine, 4, now).unwrap();
    assert!(s.tick(None, now).is_empty());
    game_applies(&s);
    assert_eq!(s.tick(None, now), [HookEvent::DragApplied(500)]);
    assert_eq!(s.rt.armed(), None);
    assert!(s.tick(None, now).is_empty());
}

#[test]
fn drag_real_cave_applies_and_the_tick_reports() {
    let mut s = drag_scene();
    let now = Instant::now();
    s.rt.arm_at(&mut s.engine, 3, now).unwrap();
    // O ponteiro da classe `Item` da reflexão sintética não é o do módulo: aponta a variável para
    // a classe `Item` do módulo, como o jogo teria.
    let item_class = s.rig.t.symbol("class_item");
    s.poke(s.var_at(var::ITEM_CLASS), &item_class.to_le_bytes());
    let item = s.rig.t.symbol("item_a");
    s.poke(item + ITEM_STACK as u64, &1i32.to_le_bytes());
    let got = s.rig.t.call("drag", &[item, 0]) as i32;
    assert_eq!(got, 250);
    assert_eq!(s.var_i32(var::DRAG_N), 0);
    assert_eq!(s.var_u64(var::DRAG_APPLIED), 1);
    assert_eq!(s.tick(None, now), [HookEvent::DragApplied(250)]);
}

/// Com a cave viva e uma thread do jogo chamando a função em laço, a cave leva o arme (e zera a
/// variável) antes da releitura do `write_var`: isso não é falha, e o contador diz `aplicada`.
#[test]
fn drag_arm_tolerates_the_cave_zeroing_the_variable() {
    let mut s = drag_scene();
    s.rt.enable(&mut s.engine, DRAG, None).unwrap();
    let item_class = s.rig.t.symbol("class_item");
    s.poke(s.var_at(var::ITEM_CLASS), &item_class.to_le_bytes());
    let item = s.rig.t.symbol("item_a");
    s.rig.t.start_loop("drag", &[item, 0]);
    let now = Instant::now();
    for round in 0..20 {
        assert_eq!(
            s.rt.arm_at(&mut s.engine, 2, now).unwrap(),
            ArmOutcome::Armed,
            "rodada {round}"
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let events = s.tick(None, now);
            if !events.is_empty() {
                assert_eq!(events, [HookEvent::DragApplied(100)], "rodada {round}");
                break;
            }
            assert!(Instant::now() < deadline, "rodada {round}: ninguém aplicou");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    s.rig.t.stop_loops();
    assert_eq!(s.var_u64(var::DRAG_APPLIED), 20);
}

#[test]
fn drag_expires_after_60s() {
    let mut s = drag_scene();
    let t0 = Instant::now();
    s.rt.arm_at(&mut s.engine, 2, t0).unwrap();
    assert!(
        s.tick(None, t0 + DRAG_EXPIRY - Duration::from_secs(1))
            .is_empty()
    );
    assert_eq!(s.var_i32(var::DRAG_N), 100, "ainda armado");
    // Passou de 60 s sem uso: desarma e registra `expirou`.
    assert_eq!(
        s.tick(None, t0 + DRAG_EXPIRY + Duration::from_secs(1)),
        [HookEvent::DragExpired]
    );
    assert_eq!(s.var_i32(var::DRAG_N), 0);
    assert_eq!(s.rt.armed(), None);
    // Nada mais a dizer.
    assert!(s.tick(None, t0 + 2 * DRAG_EXPIRY).is_empty());
}

#[test]
fn drag_expiry_sees_an_application_that_won_the_race() {
    let mut s = drag_scene();
    let t0 = Instant::now();
    s.rt.arm_at(&mut s.engine, 5, t0).unwrap();
    game_applies(&s);
    // O jogo aplicou, e o ciclo só roda depois dos 60 s: é `aplicada`, não `expirou`.
    assert_eq!(
        s.tick(None, t0 + 2 * DRAG_EXPIRY),
        [HookEvent::DragApplied(999)]
    );
}

#[test]
fn drag_cleared_from_outside_is_reported_after_two_cycles() {
    let mut s = drag_scene();
    let now = Instant::now();
    s.rt.arm_at(&mut s.engine, 2, now).unwrap();
    s.poke(s.var_at(var::DRAG_N), &0i32.to_le_bytes());
    // Um ciclo só pode ser a cave entre o `xchg` e o contador.
    assert!(s.tick(None, now).is_empty());
    assert_eq!(s.tick(None, now), [HookEvent::DragDisarmed]);
    assert_eq!(s.rt.armed(), None);
}

#[test]
fn disabling_the_drag_hook_disarms_first() {
    let mut s = drag_scene();
    s.rt.arm(&mut s.engine, 2).unwrap();
    s.rt.disable(&mut s.engine, DRAG).unwrap();
    assert!(!s.rt.is_hooked(DRAG));
    assert_eq!(s.rt.armed(), None);
    assert_eq!(s.var_i32(var::DRAG_N), 0);
}

#[test]
fn arm_without_a_resolved_drag_hook_is_refused() {
    let mut s = scene();
    // Sem `resolve`, não há desvio do arrastar.
    assert!(s.rt.arm(&mut s.engine, 0).is_err());
    assert_eq!(s.rt.armed(), None);
    assert!(s.engine.active().is_empty());
    // Indisponível (sem o campo): recusa sem escrever.
    let mut s = scene_with(Opts {
        refl: refl(RESEARCH_POINTS as u32, &["StackSize"]),
        ..default_opts()
    });
    s.resolve();
    let err = s.rt.arm(&mut s.engine, 0).unwrap_err();
    assert!(err.to_string().contains("StackSize"), "{err}");
    assert!(s.engine.active().is_empty());
}

// ---------------------------------------------------------------------------
// Coleta: inventário do jogador
// ---------------------------------------------------------------------------

#[test]
fn collect_inventory_cleared_on_pawn_loss() {
    let mut s = scene();
    s.resolve();
    // O Brain Power só reclama a folga; a variável do inventário é da coleta, mantida a cada
    // ciclo com o pawn do ciclo.
    s.rt.enable(&mut s.engine, BRAIN, None).unwrap();
    let now = Instant::now();
    let (a, b) = (s.refl.pawn_a, s.refl.pawn_b);
    assert_eq!(s.var_u64(var::COLLECT_INVENTORY), 0);
    s.tick(Some(a), now);
    assert_eq!(s.var_u64(var::COLLECT_INVENTORY), INV_A);
    // O personagem some (morte, menu, outro mundo): o ponteiro vira 0 no ciclo seguinte.
    s.tick(None, now);
    assert_eq!(s.var_u64(var::COLLECT_INVENTORY), 0);
    // Outro personagem: o inventário dele.
    s.tick(Some(b), now);
    assert_eq!(s.var_u64(var::COLLECT_INVENTORY), INV_B);
    // Personagem sem inventário (ponteiro nulo): 0.
    s.refl
        .mem
        .write(b + u64::from(INVENTORY_OFF), &0u64.to_le_bytes());
    s.tick(Some(b), now);
    assert_eq!(s.var_u64(var::COLLECT_INVENTORY), 0);
    // Ponteiro que não é de usuário: 0.
    s.refl.mem.write(
        a + u64::from(INVENTORY_OFF),
        &0xffff_ffff_ffff_ffffu64.to_le_bytes(),
    );
    s.tick(Some(a), now);
    assert_eq!(s.var_u64(var::COLLECT_INVENTORY), 0);
    // Pawn ilegível: 0 (nada acima do que a leitura falhada mostra).
    s.refl
        .mem
        .write(a + u64::from(INVENTORY_OFF), &INV_A.to_le_bytes());
    s.tick(Some(a), now);
    assert_eq!(s.var_u64(var::COLLECT_INVENTORY), INV_A);
    s.tick(Some(0x10), now);
    assert_eq!(s.var_u64(var::COLLECT_INVENTORY), 0);
    // A saída zera tudo.
    s.engine.undo_all().unwrap();
    let region = s.rig.var_region();
    let slack = s.bytes(region.start, (region.end - region.start) as usize);
    assert!(slack.iter().all(|b| *b == 0));
}

#[test]
fn inventory_var_is_untouched_when_the_reflection_has_no_inventory_field() {
    let mut s = scene_with(Opts {
        refl: refl(RESEARCH_POINTS as u32, &["InventoryComponent"]),
        ..default_opts()
    });
    s.resolve();
    assert_eq!(s.rt.inventory_offset(), None);
    s.rt.enable(&mut s.engine, BRAIN, None).unwrap();
    let pawn = s.refl.pawn_a;
    s.tick(Some(pawn), Instant::now());
    assert_eq!(s.var_u64(var::COLLECT_INVENTORY), 0);
}

#[test]
fn hit_without_enough_bytes_for_the_instruction_is_reported() {
    let mut s = scene();
    let mut hit = s.hit(DRAG);
    hit.captures.truncate(3);
    let got = s.rt.resolve(&[hit], &s.refl.mem, &s.refl.index);
    assert!(matches!(&got[0].1, HookAvail::Unavailable(_)));
    assert_eq!(s.def(DRAG).body, CaveBody::Drag);
}
