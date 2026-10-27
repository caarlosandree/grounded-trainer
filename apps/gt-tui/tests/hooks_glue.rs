#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `HookCheats` (spec 008, 6.11) contra o módulo sintético: o motor de verdade escreve as caves e
//! os desvios no `gt-testtarget --module`; a reflexão vem de uma cena sintética local. Cobre a
//! cola da TUI com o `HookRuntime` (disponibilidade, ligar, trocar valor, armar e desligar).
//! Antes: `cargo build --workspace`.

use std::sync::Arc;
use std::time::Instant;

use gt_core::mem::MemReader;
use gt_grounded2::catalog::{HookEntry, HookState};
use gt_grounded2::hooks::HookEvent;
use gt_grounded2::signatures::{HookDef, Signatures};
use gt_testkit::module::{ITEM_STACK, RESEARCH_POINTS};
use gt_testkit::rig::{MODULE_NAME, SynthRig};
use gt_tui::backend::{ArmReport, OpOutcome};
use gt_tui::hooks::HookCheats;
use gt_tui::model::Avail;
use gt_tui::values::{Globals, ReflectionGate};
use gt_ue::synth::{SparseMem, SynthField, SynthUe};
use gt_ue::{Names, ObjectArray, ReflectionIndex, ScanBudget, UeLayout};

const ARENA: u64 = 0x1000_0000;
const ARENA_LEN: usize = 64 << 20;
const MAINE: &str = "/Script/Maine";
const BRAIN: &str = "brain-power";
const ANALYZER: &str = "analisador-infinito";
const DRAG: &str = "quantidade-arrastar";
const MAX_POWER: u32 = 0x980;
const INVENTORY_OFF: u32 = 0x48;

fn field(name: &'static str, kind: &'static str, offset: u32, size: u32) -> SynthField {
    SynthField {
        name,
        kind,
        offset,
        size,
        bool_mask: None,
    }
}

/// A reflexão sintética: as classes de que o `HookRuntime` precisa.
fn refl() -> (SparseMem, ReflectionIndex) {
    let layout = UeLayout::observed();
    let mut s = SynthUe::new(layout, ARENA, ARENA_LEN);
    let (object_c, actor) = (s.object_class, s.actor_class);
    let maine = s.add_package(MAINE);
    let research = RESEARCH_POINTS as u32;
    s.add_class(
        maine,
        "PartyComponent",
        Some(object_c),
        research + 8,
        &[field("ResearchPoints", "IntProperty", research, 4)],
    );
    s.add_class(
        maine,
        "Item",
        Some(object_c),
        ITEM_STACK as u32 + 8,
        &[field("StackSize", "IntProperty", ITEM_STACK as u32, 4)],
    );
    s.add_class(
        maine,
        "ResourceAnalyzerBuilding",
        Some(object_c),
        MAX_POWER + 8,
        &[field("MaxPowerCapacity", "IntProperty", MAX_POWER, 4)],
    );
    s.add_class(
        maine,
        "SurvivalPlayerCharacter",
        Some(actor),
        INVENTORY_OFF + 0x10,
        &[field(
            "InventoryComponent",
            "ObjectProperty",
            INVENTORY_OFF,
            8,
        )],
    );
    let image = s.finish();
    let mem = image.into_mem();
    let names = Names::new(&mem, image.names).unwrap();
    let objects = ObjectArray::read(&mem, &layout, image.gobjects).unwrap();
    let index =
        ReflectionIndex::build(&mem, &layout, &objects, &names, &ScanBudget::standard()).unwrap();
    (mem, index)
}

fn hit_entry(rig: &SynthRig, def: &HookDef) -> HookEntry {
    let start = match def.id.as_str() {
        BRAIN => rig.t.symbol("level_pattern"),
        ANALYZER => rig.t.symbol("getter"),
        _ => rig.t.symbol("drag"),
    };
    let mut captures = vec![0u8; def.pattern.len()];
    MemReader::new(rig.t.pid)
        .read_exact(start, &mut captures)
        .unwrap();
    HookEntry {
        def: def.clone(),
        state: HookState::Found {
            addr: start + def.offset as u64,
            captures,
        },
    }
}

fn avail_of(c: &HookCheats, gate: &ReflectionGate, id: &str) -> Avail {
    c.avail(gate)
        .into_iter()
        .find(|(i, _)| i == id)
        .map(|(_, a)| a)
        .unwrap()
}

#[test]
fn resolve_enable_set_value_arm_and_disable() {
    let rig = SynthRig::new();
    let mut engine = rig.engine();
    let (mem, index) = refl();
    let (module, mem_headers) = rig.target.module(MODULE_NAME).unwrap();
    let disk = Arc::new(rig.target.open_disk(module, &mem_headers).unwrap());
    let sigs = Signatures::embedded().unwrap();
    let ue = sigs.ue_for_version("0000-0000").unwrap();
    let globals = Globals {
        sigs: ue,
        gobjects: 1,
        names: 2,
        verified: true,
    };
    let entries: Vec<HookEntry> = sigs
        .hooks()
        .iter()
        .filter(|d| [BRAIN, ANALYZER, DRAG].contains(&d.id.as_str()))
        .map(|d| hit_entry(&rig, d))
        .collect();
    let mut hooks = HookCheats::new(module.clone(), rig.target.maps.clone(), Ok(disk), entries);

    // Antes do índice: `verificando…`; depois de conferir, disponíveis e verificados.
    assert_eq!(
        avail_of(&hooks, &ReflectionGate::Checking, BRAIN),
        Avail::Checking
    );
    hooks.resolve(&mem, Some(&engine), &index, &globals);
    let ready = ReflectionGate::Ready { verified: true };
    for id in [BRAIN, ANALYZER, DRAG] {
        assert_eq!(
            avail_of(&hooks, &ready, id),
            Avail::Available { verified: true },
            "{id}"
        );
    }

    // Ligar: cave e desvio no motor; as entradas dele voltam ao `id` da linha.
    assert!(matches!(
        hooks.enable(&mut engine, BRAIN, 1),
        OpOutcome::Done(_)
    ));
    assert!(hooks.is_hooked(BRAIN));
    let engine_ids: Vec<String> = engine.active().iter().map(|s| s.id.clone()).collect();
    assert!(engine_ids.iter().any(|i| i == "brain-power-hook"));
    assert!(
        engine_ids
            .iter()
            .filter_map(|i| hooks.row_of_engine_id(i))
            .all(|row| row == BRAIN)
    );
    // Ligado mesmo sem o padrão na memória e mesmo sem o índice.
    assert_eq!(
        avail_of(&hooks, &ReflectionGate::Checking, BRAIN),
        Avail::Available { verified: true }
    );
    assert!(matches!(
        hooks.set_value(&mut engine, BRAIN, 2),
        OpOutcome::Done(_)
    ));
    // Um desvio desconhecido é recusado, sem restos.
    assert!(matches!(
        hooks.enable(&mut engine, "nao-existe", 0),
        OpOutcome::Refused(_)
    ));

    // Arrastar: arma com a quantidade do ponto (100 = k 2), desarma ao apertar de novo.
    assert_eq!(hooks.arm(&mut engine, 2), ArmReport::Armed(100));
    assert!(hooks.is_hooked(DRAG));
    assert_eq!(hooks.arm(&mut engine, 4), ArmReport::Disarmed(100));
    // Sem o jogo aplicar nada, a manutenção não tem o que dizer.
    let events: Vec<HookEvent> = hooks.tick(&mem, &mut engine, None, Instant::now());
    assert!(events.is_empty(), "{events:?}");

    // Desligar devolve só o desvio.
    assert!(matches!(
        hooks.disable(&mut engine, BRAIN),
        OpOutcome::Done(_)
    ));
    assert!(!hooks.is_hooked(BRAIN));
    let engine_ids: Vec<String> = engine.active().iter().map(|s| s.id.clone()).collect();
    assert!(!engine_ids.iter().any(|i| i == "brain-power-hook"));
    assert!(engine_ids.iter().any(|i| i == "brain-power-cave"));
}
