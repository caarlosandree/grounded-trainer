#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `HookRuntime` (spec 008, 6.3, 6.5, 6.6.1, 6.7.4 e 6.11) contra o módulo sintético: o
//! motor de verdade escreve as caves e os desvios no `gt-testtarget --module`, e a reflexão vem de
//! uma cena sintética local (`SynthUe`), só para o `resolve` e para o inventário do personagem.

use std::collections::HashMap;
use std::sync::Arc;

use gt_core::cave::CaveAllocator;
use gt_core::engine::Engine;
use gt_core::mem::{MemReader, MemWriter};
use gt_core::patch::{CAVE_FILL, PatchKind};
use gt_core::suspend::{Fault, PauseConfig};
use gt_grounded2::catalog::HookHit;
use gt_grounded2::caves::var;
use gt_grounded2::hooks::{HookAvail, HookEnv, HookEvent, HookRuntime, PawnView};
use gt_grounded2::signatures::{HookDef, Signatures};
use gt_testkit::module::{INVENTORY_TOTAL, ITEM_STACK, RESEARCH_POINTS};
use gt_testkit::rig::{MODULE_NAME, SynthRig};
use gt_ue::synth::{SparseMem, SynthField, SynthUe};
use gt_ue::{Names, ObjectArray, ReflectionIndex, ScanBudget, UeLayout};

const ARENA: u64 = 0x1000_0000;
const ARENA_LEN: usize = 64 << 20;
const MAINE: &str = "/Script/Maine";
const BRAIN: &str = "brain-power";
const ANALYZER: &str = "analisador-infinito";
const COLLECT: &str = "coleta-multiplicada";

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
    /// Configuração de pausa do motor (falhas injetadas); `None`: a padrão.
    pause: Option<PauseConfig>,
}

fn default_opts() -> Opts {
    Opts {
        refl: refl(RESEARCH_POINTS as u32, &[]),
        var_len: None,
        no_runs: false,
        pause: None,
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
    let engine = match opts.pause {
        Some(pause) => rig.engine_with_pause(pause),
        None => rig.engine(),
    };
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
            COLLECT => self.rig.t.symbol("add_item_impl"),
            _ => self.rig.t.symbol("getter"),
        };
        HookHit {
            addr: start + def.offset as u64,
            captures: self.bytes(start, def.pattern.len()),
            def,
        }
    }

    fn hits(&self) -> Vec<HookHit> {
        [BRAIN, ANALYZER, COLLECT].map(|id| self.hit(id)).to_vec()
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

    fn tick(&mut self, pawn: Option<u64>) -> Vec<HookEvent> {
        self.rt
            .tick(&mut self.engine, &self.refl.mem, pawn.map(PawnView::from))
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
    for id in [BRAIN, ANALYZER, COLLECT] {
        assert_eq!(got[id], HookAvail::Available, "{id}");
    }
    // O inventário do personagem vem da reflexão.
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
    assert_eq!(got[COLLECT], HookAvail::Available);

    // Sem `StackSize` na reflexão: só a coleta fica indisponível.
    let mut no_stack = scene_with(Opts {
        refl: refl(RESEARCH_POINTS as u32, &["StackSize"]),
        ..default_opts()
    });
    let got = no_stack.resolve();
    assert_eq!(
        got[COLLECT],
        HookAvail::Unavailable("campo StackSize não encontrado".into())
    );
    assert_eq!(got[BRAIN], HookAvail::Available);
    assert_eq!(got[ANALYZER], HookAvail::Available);

    // Sem o campo do inventário no personagem: a coleta não tem como saber qual é o do jogador.
    let mut no_inv = scene_with(Opts {
        refl: refl(RESEARCH_POINTS as u32, &["InventoryComponent"]),
        ..default_opts()
    });
    assert_eq!(
        no_inv.resolve()[COLLECT],
        HookAvail::Unavailable("campo InventoryComponent não encontrado".into())
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
    // Outra classe no `object_class` do analisador: a da reflexão não é a pedida.
    let mut hit = s.hit(ANALYZER);
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
    // 100 bytes: o Brain Power (24) e o analisador (56) cabem; a coleta, com a classe `Item`
    // até 144, não.
    let mut small = scene_with(Opts {
        var_len: Some(100),
        ..default_opts()
    });
    let got = small.resolve();
    assert_eq!(got[BRAIN], HookAvail::Available);
    assert_eq!(got[ANALYZER], HookAvail::Available);
    assert_eq!(
        got[COLLECT],
        HookAvail::Unavailable("sem espaço para variáveis".into())
    );
    // 144 bytes: a coleta cabe.
    let mut exact = scene_with(Opts {
        var_len: Some(144),
        ..default_opts()
    });
    assert_eq!(exact.resolve()[COLLECT], HookAvail::Available);
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
fn caves_of_the_hooks_fit_together_without_overlap() {
    let mut s = scene();
    s.resolve();
    for id in [BRAIN, ANALYZER, COLLECT] {
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
    // O analisador não tem valor de variável: recusa.
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
fn disable_failure_leaving_the_hook_pending_reinstalls_after_the_restore() {
    // A 2ª pausa do motor (a do `disable`; a 1ª foi a do `enable`) escreve e não consegue
    // desfazer: o desvio fica pendente.
    let mut s = scene_with(Opts {
        pause: Some(PauseConfig {
            fault: Some(Fault::WriteAndRollbackFail),
            hook_on_pause: 2,
            ..PauseConfig::default()
        }),
        ..default_opts()
    });
    s.resolve();
    let original = s.level_point_bytes();
    s.rt.enable(&mut s.engine, BRAIN, None).unwrap();
    let jumped = s.level_point_bytes();
    assert_ne!(jumped, original);
    assert!(s.rt.disable(&mut s.engine, BRAIN).is_err());
    assert_eq!(s.engine.pending_ids(), ["brain-power-hook".to_string()]);
    // O desvio saiu do motor: `hooked` bate com ele.
    assert!(!s.rt.is_hooked(BRAIN));
    s.engine.restore_pending().unwrap();
    assert!(s.engine.pending_ids().is_empty());
    assert_eq!(s.level_point_bytes(), original);
    // Religar instala o `jmp` de novo e a cave multiplica.
    s.rt.enable(&mut s.engine, BRAIN, Some(1)).unwrap();
    assert!(s.rt.is_hooked(BRAIN));
    assert_eq!(s.level_point_bytes(), jumped, "o jmp voltou ao ponto");
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
    s.engine.undo_all().unwrap();
    assert_eq!(s.level_point_bytes(), original);
}

#[test]
fn recognize_installed_drops_a_hooked_flag_the_engine_does_not_have() {
    let mut s = scene();
    s.resolve();
    s.rt.enable(&mut s.engine, BRAIN, None).unwrap();
    assert!(s.rt.is_hooked(BRAIN));
    // O desvio sai do motor por fora do runtime (um desfazer que o deixou fora dos ativos).
    s.engine.undo(&["brain-power-hook"]).unwrap();
    let defs = s.defs.clone();
    let found = s.rt.recognize_installed(&s.engine, &defs);
    assert!(found.is_empty(), "{found:?}");
    assert!(!s.rt.is_hooked(BRAIN));
    // Religar escreve o desvio de novo.
    s.rt.enable(&mut s.engine, BRAIN, None).unwrap();
    assert!(s.rt.is_hooked(BRAIN));
    assert!(s.engine_ids().contains(&"brain-power-hook".to_string()));
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
    // então só o analisador vem como ocorrência.
    s.rt = HookRuntime::new(make_env(&s.rig));
    assert_eq!(s.rt.recognize_installed(&s.engine, &s.defs), [BRAIN]);
    let hits = [s.hit(ANALYZER)];
    let got = s.rt.resolve(&hits, &s.refl.mem, &s.refl.index);
    assert_eq!(
        got,
        vec![
            (ANALYZER.to_string(), HookAvail::Available),
            (BRAIN.to_string(), HookAvail::Available),
        ]
    );
    s.rt.enable(&mut s.engine, ANALYZER, None).unwrap();
    let spans = cave_spans(&s);
    assert_eq!(spans.len(), 2);
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
    // Religar o Brain Power reaproveita a cave: entra só o desvio, no mesmo lugar.
    s.rt.enable(&mut s.engine, BRAIN, Some(2)).unwrap();
    let spans = cave_spans(&s);
    assert_eq!(spans.len(), 2, "{spans:x?}");
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
    s.set_points(0);
    // Nada aconteceu: nenhum evento.
    assert!(s.tick(None).is_empty());
    // Um ganho: um evento, sem ganhos escondidos (×2: +4 vira +8; o próximo nível é 100).
    s.gain(4);
    assert_eq!(
        s.tick(None),
        [HookEvent::BrainGain {
            gain: 4,
            applied: 8,
            l1: 100,
            skipped: 0
        }]
    );
    // Sem novo ganho, sem evento.
    assert!(s.tick(None).is_empty());
    // Três ganhos entre dois ciclos: o último e `(+2 ganhos não mostrados)`.
    s.gain(1);
    s.gain(2);
    s.gain(3);
    assert_eq!(
        s.tick(None),
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
    assert!(s.tick(None).is_empty());
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
    assert!(s.tick(None).is_empty());
    s.gain(5);
    assert_eq!(s.tick(None).len(), 1);
}

#[test]
fn tick_before_any_enable_does_nothing() {
    let mut s = scene();
    s.resolve();
    let pawn = s.refl.pawn_a;
    assert!(s.tick(Some(pawn)).is_empty());
    assert!(s.engine.vars().is_empty());
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
    let (a, b) = (s.refl.pawn_a, s.refl.pawn_b);
    assert_eq!(s.var_u64(var::COLLECT_INVENTORY), 0);
    s.tick(Some(a));
    assert_eq!(s.var_u64(var::COLLECT_INVENTORY), INV_A);
    // O personagem some (morte, menu, outro mundo): o ponteiro vira 0 no ciclo seguinte.
    s.tick(None);
    assert_eq!(s.var_u64(var::COLLECT_INVENTORY), 0);
    // Outro personagem: o inventário dele.
    s.tick(Some(b));
    assert_eq!(s.var_u64(var::COLLECT_INVENTORY), INV_B);
    // Personagem sem inventário (ponteiro nulo): 0.
    s.refl
        .mem
        .write(b + u64::from(INVENTORY_OFF), &0u64.to_le_bytes());
    s.tick(Some(b));
    assert_eq!(s.var_u64(var::COLLECT_INVENTORY), 0);
    // Ponteiro que não é de usuário: 0.
    s.refl.mem.write(
        a + u64::from(INVENTORY_OFF),
        &0xffff_ffff_ffff_ffffu64.to_le_bytes(),
    );
    s.tick(Some(a));
    assert_eq!(s.var_u64(var::COLLECT_INVENTORY), 0);
    // Pawn ilegível: 0 (nada acima do que a leitura falhada mostra).
    s.refl
        .mem
        .write(a + u64::from(INVENTORY_OFF), &INV_A.to_le_bytes());
    s.tick(Some(a));
    assert_eq!(s.var_u64(var::COLLECT_INVENTORY), INV_A);
    s.tick(Some(0x10));
    assert_eq!(s.var_u64(var::COLLECT_INVENTORY), 0);
    // A saída zera tudo.
    s.engine.undo_all().unwrap();
    let region = s.rig.var_region();
    let slack = s.bytes(region.start, (region.end - region.start) as usize);
    assert!(slack.iter().all(|b| *b == 0));
}

impl Scene {
    /// `add_item_impl(inv, item, 1)` no alvo; devolve o total do inventário.
    fn add_item(&mut self, inv: u64, item: u64) -> i32 {
        self.rig.t.call("add_item_impl", &[inv, item, 1]) as i32
    }

    fn stack(&self, item: u64) -> i32 {
        i32::from_le_bytes(self.bytes(item + ITEM_STACK as u64, 4).try_into().unwrap())
    }
}

#[test]
fn collect_enable_uses_player_inventory() {
    let mut s = scene();
    s.resolve();
    // Ligar escreve a classe `Item` da reflexão (136) e o N da grade (k = 1: ×3) antes do lote.
    s.rt.enable(&mut s.engine, COLLECT, Some(1)).unwrap();
    assert!(s.rt.is_hooked(COLLECT));
    assert_eq!(
        s.engine_ids(),
        [
            "coleta-multiplicada-cave".to_string(),
            "coleta-multiplicada-hook".to_string()
        ]
    );
    let item_class = s.refl.index.find_class(MAINE, "Item").unwrap();
    assert_eq!(s.var_u64(var::ITEM_CLASS), item_class);
    assert_eq!(s.var_i32(var::COLLECT_N), 3);
    // A lista de retornos (72..136) não é usada.
    assert!(
        s.bytes(s.var_at(var::COLLECT_RETURNS), 64)
            .iter()
            .all(|b| *b == 0)
    );

    // A cena da reflexão é local; no alvo, o personagem aponta para o `inventory_a` dele e a
    // classe é a `class_item` do módulo sintético.
    let (inv_a, inv_b) = (s.rig.t.symbol("inventory_a"), s.rig.t.symbol("inventory_b"));
    let item = s.rig.t.symbol("item_a");
    let pawn = s.refl.pawn_a;
    s.refl
        .mem
        .write(pawn + u64::from(INVENTORY_OFF), &inv_a.to_le_bytes());
    s.poke(
        s.var_at(var::ITEM_CLASS),
        &s.rig.t.symbol("class_item").to_le_bytes(),
    );
    // Sem personagem no ciclo: o inventário é 0 e nada multiplica.
    s.tick(None);
    assert_eq!(s.add_item(inv_a, item), 1);
    // Com o personagem: o inventário dele multiplica (1 × 3), o outro não.
    s.tick(Some(pawn));
    assert_eq!(s.var_u64(var::COLLECT_INVENTORY), inv_a);
    assert_eq!(s.add_item(inv_a, item), 4);
    assert_eq!(s.stack(item), 3);
    assert_eq!(s.add_item(inv_b, item), 3);
    assert_eq!(s.stack(item), 3);
    // Trocar o valor é só a variável (k = 3: ×10).
    s.rt.set_value(&mut s.engine, COLLECT, 3).unwrap();
    assert_eq!(s.var_i32(var::COLLECT_N), 10);
    assert_eq!(s.add_item(inv_a, item), 34);
    // Desligar devolve o desvio; a cave fica e o item extra também (sem desfazer).
    s.rt.disable(&mut s.engine, COLLECT).unwrap();
    assert_eq!(s.engine_ids(), ["coleta-multiplicada-cave".to_string()]);
    assert_eq!(s.add_item(inv_a, item), 64);
    assert_eq!(s.stack(item), 30);
    let total = i32::from_le_bytes(
        s.bytes(inv_a + INVENTORY_TOTAL as u64, 4)
            .try_into()
            .unwrap(),
    );
    assert_eq!(total, 64);
    // A saída zera as variáveis.
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
    s.tick(Some(pawn));
    assert_eq!(s.var_u64(var::COLLECT_INVENTORY), 0);
}

#[test]
fn hit_without_enough_bytes_for_the_instruction_is_reported() {
    let mut s = scene();
    let mut hit = s.hit(ANALYZER);
    hit.captures.truncate(3);
    let got = s.rt.resolve(&[hit], &s.refl.mem, &s.refl.index);
    assert!(matches!(&got[0].1, HookAvail::Unavailable(_)));
}
