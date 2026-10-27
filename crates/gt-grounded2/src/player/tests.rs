//! Testes do caminho até o personagem sobre a reflexão sintética. Os offsets daqui são arbitrários
//! (de propósito, diferentes dos do jogo): os campos vêm da reflexão, por nome.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use gt_ue::synth::{SparseMem, SynthField, SynthUe};
use gt_ue::{Names, ObjectArray, ReflectionIndex, ScanBudget, UeLayout};

use super::*;
use crate::signatures::{Category, FieldKind, FieldRoot, GameStateCfg};

const ARENA: u64 = 0x1000_0000;
const ARENA_LEN: usize = 64 << 20;

const MAINE: &str = "/Script/Maine";

fn no_sleep(_: Duration) {}

fn budget() -> ScanBudget {
    ScanBudget {
        block_bytes: 1 << 20,
        batch: 64,
        pause: Duration::ZERO,
        deadline: Duration::from_secs(60),
        sleep: no_sleep,
    }
}

fn float(name: &'static str, offset: u32) -> SynthField {
    SynthField {
        name,
        kind: "FloatProperty",
        offset,
        size: 4,
        bool_mask: None,
    }
}

fn int(name: &'static str, offset: u32) -> SynthField {
    SynthField {
        name,
        kind: "IntProperty",
        offset,
        size: 4,
        bool_mask: None,
    }
}

fn object(name: &'static str, offset: u32) -> SynthField {
    SynthField {
        name,
        kind: "ObjectProperty",
        offset,
        size: 8,
        bool_mask: None,
    }
}

fn boolean(name: &'static str, offset: u32, mask: u8) -> SynthField {
    SynthField {
        name,
        kind: "BoolProperty",
        offset,
        size: 1,
        bool_mask: Some((0, mask)),
    }
}

// Offsets sintéticos dos campos (o cabeçalho de `UObject` ocupa até 0x28 e o `Actor` até 0x40).
const PAWN_OFF: u32 = 0x48;
const HEALTH_OFF: u32 = 0x48;
const STAMINA_OFF: u32 = 0x50;
const SURVIVAL_OFF: u32 = 0x58;
const TEMP_OFF: u32 = 0x60;
const FALL_BYTE: u32 = 0x68;
const FALL_MASK: u8 = 0x04;
const DAMAGE_OFF: u32 = 0x30;
const DIE_BYTE: u32 = 0x38;
const CUR_STAMINA: u32 = 0x30;
const MAX_STAMINA: u32 = 0x34;
const CUR_FOOD: u32 = 0x30;
/// `SurvivalComponent.HungerSettings`; o máximo é `MaxValue` (`+0x0`) da struct, como no jogo.
const HUNGER_OFF: u32 = 0x50;
const THIRST_OFF: u32 = 0x60;
const BREATH_OFF: u32 = 0x70;
/// `SurvivalPlayerController.CachedSurvivalPlayerCharacter` (aponta para o personagem montado).
const CACHED_OFF: u32 = 0x78;
const CUR_TEMP: u32 = 0x30;
const NEUTRAL_TEMP: f32 = 37.0;
/// `SurvivalCharacter.PetMasterComponent` e `PetMasterComponent.CurrentPetFull`.
const PET_MASTER_OFF: u32 = 0x70;
const CURRENT_PET_OFF: u32 = 0x30;
/// `SurvivalCharacter.HaulingComponent`, `.CharacterMovement` e `.CustomDamageMultiplier`.
const HAULING_OFF: u32 = 0x88;
const MOVEMENT_OFF: u32 = 0x90;
const DAMAGE_MULT_OFF: u32 = 0x98;
/// `HaulingComponent.Capacity`.
const CAPACITY_OFF: u32 = 0x34;
/// `MaineCharMovementComponent`: os dois multiplicadores e a gravidade.
const GROUND_MULT_OFF: u32 = 0x30;
const SWIM_MULT_OFF: u32 = 0x34;
const GRAVITY_OFF: u32 = 0x3c;
/// `Level.OwningWorld`, `World.GameState`, `InGameGameState.PartyComponent` e `PartyComponent.ScienceFound`.
const WORLD_OFF: u32 = 0x40;
const GAME_STATE_OFF: u32 = 0x48;
const PARTY_OFF: u32 = 0x38;
const SCIENCE_OFF: u32 = 0x30;

/// Objetos de um mundo sintético.
struct World {
    mem: SparseMem,
    index: ReflectionIndex,
    objects: ObjectArray,
    catalog: FieldCatalog,
    layout: UeLayout,
    controller: u64,
    second_controller: u64,
    pawn: u64,
    creature: u64,
    health: u64,
    stamina: u64,
    survival: u64,
    temperature: u64,
    health_class: u64,
    gobjects: u64,
    pet: u64,
    pet_master: u64,
    pet_health: u64,
    level: u64,
    world: u64,
    game_state: u64,
    party: u64,
    hauling: u64,
    movement: u64,
}

impl World {
    fn write_u64(&self, addr: u64, v: u64) {
        self.mem.write(addr, &v.to_le_bytes());
    }

    fn write_f32(&self, addr: u64, v: f32) {
        self.mem.write(addr, &v.to_le_bytes());
    }

    fn read(&self) -> Result<PlayerSnapshot, Absent> {
        read_player(
            &self.mem,
            &self.index,
            &self.catalog,
            &self.objects,
            self.controller,
        )
    }

    fn plan(&self, on: &[&str]) -> Vec<FieldAction> {
        let snap = self.read().unwrap();
        let on: Vec<String> = on.iter().map(|s| s.to_string()).collect();
        plan_tick(&snap, &self.catalog, &on)
    }

    fn find(&self) -> ControllerFind {
        find_controller(
            &self.index,
            &self.mem,
            &self.objects,
            &self.catalog,
            &budget(),
        )
        .unwrap()
    }
}

fn cfg() -> PlayerCfg {
    PlayerCfg {
        controller_class: format!("{MAINE}.SurvivalPlayerController"),
        pawn_field: "Pawn".into(),
        pawn_class: format!("{MAINE}.SurvivalPlayerCharacter"),
        alt_pawn_fields: vec!["CachedSurvivalPlayerCharacter".into()],
        game_state: None,
    }
}

/// O `[ue.player]` com o caminho do `GameState`.
fn cfg_gs() -> PlayerCfg {
    PlayerCfg {
        game_state: Some(GameStateCfg {
            level_class: "/Script/Engine.Level".into(),
            world_field: "OwningWorld".into(),
            world_class: "/Script/Engine.World".into(),
            game_state_field: "GameState".into(),
            game_state_class: format!("{MAINE}.InGameGameState"),
        }),
        ..cfg()
    }
}

fn lock(
    id: &str,
    object: &str,
    class: &str,
    field: &str,
    mode: LockMode,
    target: LockTarget,
) -> FieldDef {
    FieldDef {
        id: id.into(),
        name: id.into(),
        category: Category::Sobrevivencia,
        description: String::new(),
        object: object.into(),
        object_class: format!("{MAINE}.{class}"),
        root: FieldRoot::Pawn,
        value: None,
        rule: FieldRule::Lock {
            field: field.into(),
            mode,
            target,
            min: -100.0,
            max: 10_000.0,
        },
    }
}

fn bits(id: &str, object: &str, class: &str, fields: &[&str]) -> FieldDef {
    FieldDef {
        id: id.into(),
        name: id.into(),
        category: Category::Sobrevivencia,
        description: String::new(),
        object: object.into(),
        object_class: format!("{MAINE}.{class}"),
        root: FieldRoot::Pawn,
        value: None,
        rule: FieldRule::ClearBits {
            fields: fields.iter().map(|s| s.to_string()).collect(),
        },
    }
}

/// Os oito cheats da spec 6.5, com a temperatura neutra como constante.
fn defs() -> Vec<FieldDef> {
    let max = |f: &str| LockTarget::Field(f.into());
    vec![
        lock(
            "vida-infinita",
            "HealthComponent",
            "HealthComponent",
            "CurrentDamage",
            LockMode::Lower,
            LockTarget::Const(0.0),
        ),
        lock(
            "stamina-infinita",
            "StaminaComponent",
            "StaminaComponent",
            "CurrentStamina",
            LockMode::Raise,
            max("MaxStamina"),
        ),
        lock(
            "comida-infinita",
            "SurvivalComponent",
            "SurvivalComponent",
            "CurrentFood",
            LockMode::Raise,
            max("HungerSettings.MaxValue"),
        ),
        lock(
            "agua-infinita",
            "SurvivalComponent",
            "SurvivalComponent",
            "CurrentWater",
            LockMode::Raise,
            max("ThirstSettings.MaxValue"),
        ),
        lock(
            "oxigenio-infinito",
            "SurvivalComponent",
            "SurvivalComponent",
            "CurrentBreath",
            LockMode::Raise,
            max("BreathSettings.MaxValue"),
        ),
        lock(
            "temperatura-estavel",
            "BodyTemperatureComponent",
            "BodyTemperatureComponent",
            "CurrentBodyTemperature",
            LockMode::Exact,
            LockTarget::Const(NEUTRAL_TEMP),
        ),
        bits(
            "sem-dano-queda",
            "pawn",
            "SurvivalCharacter",
            &["bTakeFallDamage"],
        ),
        bits(
            "nao-morrer",
            "HealthComponent",
            "HealthComponent",
            &["bCanDieFromDamage", "bCanDieEver"],
        ),
    ]
}

/// Monta o mundo: controller com pawn `SurvivalPlayerCharacter`, os quatro componentes, uma
/// criatura `SurvivalCharacter` com componentes próprios e um segundo controller com `Pawn` nulo.
/// Os campos cujo nome está em `omit` ficam de fora das classes.
fn world(omit: &[&str]) -> World {
    let layout = UeLayout::observed();
    let mut s = SynthUe::new(layout, ARENA, ARENA_LEN);
    let keep = |fields: Vec<SynthField>| -> Vec<SynthField> {
        fields
            .into_iter()
            .filter(|f| !omit.contains(&f.name))
            .collect()
    };
    let (engine, actor, object_c) = (s.engine_package, s.actor_class, s.object_class);
    let maine = s.add_package(MAINE);

    let controller_c = s.add_class(
        engine,
        "Controller",
        Some(actor),
        0x60,
        &keep(vec![object("Pawn", PAWN_OFF), object("Character", 0x50)]),
    );
    let player_controller_c = s.add_class(
        engine,
        "PlayerController",
        Some(controller_c),
        0x70,
        &keep(vec![object("AcknowledgedPawn", 0x60)]),
    );
    let survival_controller_c = s.add_class(
        maine,
        "SurvivalPlayerController",
        Some(player_controller_c),
        0x80,
        &keep(vec![object("CachedSurvivalPlayerCharacter", CACHED_OFF)]),
    );
    let character_c = s.add_class(
        maine,
        "SurvivalCharacter",
        Some(actor),
        0xb0,
        &keep(vec![
            object("HaulingComponent", HAULING_OFF),
            object("CharacterMovement", MOVEMENT_OFF),
            float("CustomDamageMultiplier", DAMAGE_MULT_OFF),
            object("HealthComponent", HEALTH_OFF),
            object("StaminaComponent", STAMINA_OFF),
            object("SurvivalComponent", SURVIVAL_OFF),
            object("BodyTemperatureComponent", TEMP_OFF),
            boolean("bTakeFallDamage", FALL_BYTE, FALL_MASK),
            object("PetMasterComponent", PET_MASTER_OFF),
        ]),
    );
    let creature_c = s.add_class(maine, "SurvivalCreature", Some(character_c), 0xb0, &[]);
    let pet_master_c = s.add_class(
        maine,
        "PetMasterComponent",
        Some(object_c),
        0x40,
        &keep(vec![object("CurrentPetFull", CURRENT_PET_OFF)]),
    );
    if !omit.contains(&"CurrentPetFull") {
        s.link_struct(pet_master_c, "CurrentPetFull", creature_c);
    }
    if !omit.contains(&"PetMasterComponent") {
        s.link_struct(character_c, "PetMasterComponent", pet_master_c);
    }
    let player_character_c = s.add_class(
        maine,
        "SurvivalPlayerCharacter",
        Some(character_c),
        0xb0,
        &[],
    );
    let hauling_c = s.add_class(
        maine,
        "HaulingComponent",
        Some(object_c),
        0x40,
        &keep(vec![int("Capacity", CAPACITY_OFF)]),
    );
    let movement_c = s.add_class(
        maine,
        "MaineCharMovementComponent",
        Some(object_c),
        0x50,
        &keep(vec![
            float("CustomGroundSpeedMultiplier", GROUND_MULT_OFF),
            float("CustomSwimSpeedMultiplier", SWIM_MULT_OFF),
            float("GravityScale", GRAVITY_OFF),
        ]),
    );
    let level_c = s.add_class(
        engine,
        "Level",
        Some(object_c),
        0x60,
        &keep(vec![object("OwningWorld", WORLD_OFF)]),
    );
    let world_c = s.add_class(
        engine,
        "World",
        Some(object_c),
        0x70,
        &keep(vec![object("GameState", GAME_STATE_OFF)]),
    );
    let game_state_c = s.add_class(
        maine,
        "InGameGameState",
        Some(actor),
        0x60,
        &keep(vec![object("PartyComponent", PARTY_OFF)]),
    );
    let party_c = s.add_class(
        maine,
        "PartyComponent",
        Some(object_c),
        0x40,
        &keep(vec![int("ScienceFound", SCIENCE_OFF)]),
    );
    let health_c = s.add_class(
        maine,
        "HealthComponent",
        Some(object_c),
        0x40,
        &keep(vec![
            float("CurrentDamage", DAMAGE_OFF),
            float("MaxHealth", 0x34),
            boolean("bCanDieFromDamage", DIE_BYTE, 0x01),
            boolean("bCanDieEver", DIE_BYTE, 0x02),
        ]),
    );
    let stamina_c = s.add_class(
        maine,
        "StaminaComponent",
        Some(object_c),
        0x38,
        &keep(vec![
            float("CurrentStamina", CUR_STAMINA),
            float("MaxStamina", MAX_STAMINA),
        ]),
    );
    let trait_settings = s.add_struct(
        maine,
        "SurvivalTraitSettings",
        0x10,
        &keep(vec![float("MaxValue", 0x0), float("InitialValue", 0x4)]),
    );
    let breath_settings = s.add_struct(
        maine,
        "BreathSettings",
        0x8,
        &keep(vec![float("MaxValue", 0x0)]),
    );
    let settings = |name: &'static str, offset: u32| SynthField {
        name,
        kind: "StructProperty",
        offset,
        size: 0x10,
        bool_mask: None,
    };
    let survival_fields = keep(vec![
        float("CurrentFood", CUR_FOOD),
        float("CurrentWater", 0x38),
        float("CurrentBreath", 0x40),
        settings("HungerSettings", HUNGER_OFF),
        settings("ThirstSettings", THIRST_OFF),
        settings("BreathSettings", BREATH_OFF),
    ]);
    let survival_c = s.add_class(
        maine,
        "SurvivalComponent",
        Some(object_c),
        0x80,
        &survival_fields,
    );
    for (field, strukt) in [
        ("HungerSettings", trait_settings),
        ("ThirstSettings", trait_settings),
        ("BreathSettings", breath_settings),
    ] {
        if survival_fields.iter().any(|f| f.name == field) {
            s.link_struct(survival_c, field, strukt);
        }
    }
    let temp_c = s.add_class(
        maine,
        "BodyTemperatureComponent",
        Some(object_c),
        0x38,
        &keep(vec![float("CurrentBodyTemperature", CUR_TEMP)]),
    );

    // O controller e o pawn compartilham o `Outer` (o nível persistente), como no jogo.
    let level = s.add_object(level_c, "PersistentLevel", maine);
    let world = s.add_object(world_c, "World_0", maine);
    let game_state = s.add_object(game_state_c, "GameState_0", level);
    let party = s.add_object(party_c, "Party", game_state);
    s.set(level, WORLD_OFF, &world.to_le_bytes());
    s.set(world, GAME_STATE_OFF, &game_state.to_le_bytes());
    s.set(game_state, PARTY_OFF, &party.to_le_bytes());
    let controller = s.add_object(survival_controller_c, "Controller_0", level);
    let second_controller = s.add_object(survival_controller_c, "Controller_1", level);
    let pawn = s.add_object(player_character_c, "Player_0", maine);
    let creature = s.add_object(character_c, "Creature_0", maine);
    let pet = s.add_object(creature_c, "Pet_0", maine);
    let mut components = |owner: u64| {
        let parts = [
            (s.add_object(health_c, "Health", owner), HEALTH_OFF),
            (s.add_object(stamina_c, "Stamina", owner), STAMINA_OFF),
            (s.add_object(survival_c, "Survival", owner), SURVIVAL_OFF),
            (s.add_object(temp_c, "Temperature", owner), TEMP_OFF),
        ];
        for (obj, off) in parts {
            s.set(owner, off, &obj.to_le_bytes());
        }
        parts.map(|(obj, _)| obj)
    };
    let [health, stamina, survival, temperature] = components(pawn);
    components(creature);
    let [pet_health, ..] = components(pet);
    let hauling = s.add_object(hauling_c, "Hauling", pawn);
    let movement = s.add_object(movement_c, "Movement", pawn);
    s.set(pawn, HAULING_OFF, &hauling.to_le_bytes());
    s.set(pawn, MOVEMENT_OFF, &movement.to_le_bytes());
    let pet_master = s.add_object(pet_master_c, "PetMaster", pawn);
    s.set(pawn, PET_MASTER_OFF, &pet_master.to_le_bytes());
    s.set(pet_master, CURRENT_PET_OFF, &pet.to_le_bytes());
    s.set(controller, PAWN_OFF, &pawn.to_le_bytes());

    let image = s.finish();
    let mem = image.into_mem();
    let names = Names::new(&mem, image.names).unwrap();
    let objects = ObjectArray::read(&mem, &layout, image.gobjects).unwrap();
    let index = ReflectionIndex::build(&mem, &layout, &objects, &names, &budget()).unwrap();
    let catalog = FieldCatalog::resolve(&index, &mem, &cfg(), &defs());
    let w = World {
        mem,
        index,
        objects,
        catalog,
        layout,
        controller,
        second_controller,
        pawn,
        creature,
        health,
        stamina,
        survival,
        temperature,
        health_class: health_c,
        gobjects: image.gobjects,
        pet,
        pet_master,
        pet_health,
        level,
        world,
        game_state,
        party,
        hauling,
        movement,
    };
    // Valores iniciais: tudo cheio, sem dano, temperatura neutra e bits ligados como no jogo.
    w.write_f32(stamina + u64::from(CUR_STAMINA), 100.0);
    w.write_f32(stamina + u64::from(MAX_STAMINA), 100.0);
    for off in [CUR_FOOD, 0x38, 0x40, HUNGER_OFF, THIRST_OFF, BREATH_OFF] {
        w.write_f32(survival + u64::from(off), 100.0);
    }
    w.write_f32(temperature + u64::from(CUR_TEMP), NEUTRAL_TEMP);
    w.mem
        .write(hauling + u64::from(CAPACITY_OFF), &20i32.to_le_bytes());
    w.mem
        .write(party + u64::from(SCIENCE_OFF), &7400i32.to_le_bytes());
    w
}

#[test]
fn resolves_all_fields_by_name() {
    let w = world(&[]);
    for def in defs() {
        assert_eq!(w.catalog.avail(&def.id), Ok(()), "{}", def.id);
    }
    let Some(Ok(stamina)) = w.catalog.get("stamina-infinita") else {
        panic!("stamina não resolvida");
    };
    assert_eq!(stamina.object_path[0].field.offset, STAMINA_OFF);
    let ResolvedRule::Lock { field, target, .. } = &stamina.rule else {
        panic!("stamina não é trava");
    };
    assert_eq!(field.offset, CUR_STAMINA);
    assert!(matches!(target, ResolvedTarget::Field(f) if f.offset == MAX_STAMINA));
    // O próprio pawn não passa por campo.
    let Some(Ok(fall)) = w.catalog.get("sem-dano-queda") else {
        panic!("queda não resolvida");
    };
    assert!(fall.object_path.is_empty());
    assert!(w.catalog.avail("nao-existe").is_err());

    let snap = w.read().unwrap();
    assert_eq!(snap.pawn, w.pawn);
    assert_eq!(snap.values.len(), 8);
}

#[test]
fn missing_field_reason_text() {
    let w = world(&["CurrentFood"]);
    assert_eq!(
        w.catalog.avail("comida-infinita"),
        Err("campo CurrentFood não encontrado".to_string())
    );
    // Os outros continuam disponíveis, e a leitura segue sem o que falta.
    assert_eq!(w.catalog.avail("agua-infinita"), Ok(()));
    let snap = w.read().unwrap();
    assert!(!snap.values.contains_key("comida-infinita"));

    // Sem o campo do caminho, todos ficam indisponíveis com o mesmo motivo.
    let w = world(&["Pawn"]);
    assert_eq!(
        w.catalog.avail("vida-infinita"),
        Err("campo Pawn não encontrado".to_string())
    );
    assert_eq!(w.read(), Err(Absent::NoController));
    assert_eq!(w.find(), ControllerFind::Absent);
}

#[test]
fn missing_class_reason_text() {
    let w = world(&[]);
    let mut cfg = cfg();
    cfg.pawn_class = format!("{MAINE}.NaoExiste");
    let catalog = FieldCatalog::resolve(&w.index, &w.mem, &cfg, &defs());
    assert_eq!(
        catalog.avail("vida-infinita"),
        Err("classe NaoExiste não encontrada".to_string())
    );
}

#[test]
fn two_controllers_pick_non_null_pawn() {
    let w = world(&[]);
    assert_eq!(w.find(), ControllerFind::Found(w.controller));
}

#[test]
fn two_controllers_both_null_ambiguous() {
    let w = world(&[]);
    w.write_u64(w.controller + u64::from(PAWN_OFF), 0);
    assert_eq!(w.find(), ControllerFind::Ambiguous);
    // Dois com `Pawn` também é ambíguo.
    w.write_u64(w.controller + u64::from(PAWN_OFF), w.pawn);
    w.write_u64(w.second_controller + u64::from(PAWN_OFF), w.pawn);
    assert_eq!(w.find(), ControllerFind::Ambiguous);
}

#[test]
fn pawn_null_is_absent() {
    let w = world(&[]);
    w.write_u64(w.controller + u64::from(PAWN_OFF), 0);
    assert_eq!(w.read(), Err(Absent::PawnNull));
}

#[test]
fn controller_replaced_in_table_is_invalid() {
    let w = world(&[]);
    // Outro objeto ocupa o `InternalIndex` do controller: o item não aponta mais para ele.
    let index =
        gt_ue::read::read_u32(&w.mem, w.controller + u64::from(w.layout.uobject.index)).unwrap();
    let per = u64::from(w.layout.objects.items_per_chunk);
    let item = w.objects.chunks[(u64::from(index) / per) as usize]
        + (u64::from(index) % per) * u64::from(w.layout.objects.item_size);
    w.write_u64(item + u64::from(w.layout.objects.item_object), w.pawn);
    assert_eq!(w.read(), Err(Absent::ControllerInvalid));
}

#[test]
fn mount_in_pawn_never_targeted() {
    let w = world(&[]);
    w.write_u64(w.controller + u64::from(PAWN_OFF), w.creature);
    // Sem o campo alternativo apontando para o personagem, nada é lido.
    assert_eq!(w.read(), Err(Absent::PawnWrongClass));
    // Montado, como no jogo: o campo alternativo leva ao personagem, e a criatura nunca é alvo.
    w.write_u64(w.controller + u64::from(CACHED_OFF), w.pawn);
    let snap = w.read().unwrap();
    assert_eq!(snap.pawn, w.pawn);
    assert!(snap.objects.iter().all(|o| o.addr != w.creature));
    // O campo alternativo apontando para outra coisa que não o personagem também não serve.
    w.write_u64(w.controller + u64::from(CACHED_OFF), w.creature);
    assert_eq!(w.read(), Err(Absent::PawnWrongClass));
}

#[test]
fn food_target_comes_from_struct_member() {
    let w = world(&[]);
    let Some(Ok(food)) = w.catalog.get("comida-infinita") else {
        panic!("comida não resolvida");
    };
    let ResolvedRule::Lock { target, .. } = &food.rule else {
        panic!("comida não é trava");
    };
    assert!(matches!(target, ResolvedTarget::Field(f) if f.offset == HUNGER_OFF));
    // O máximo sobe (upgrade): a trava acompanha.
    w.write_f32(w.survival + u64::from(HUNGER_OFF), 150.0);
    assert!(matches!(
        w.plan(&["comida-infinita"]).as_slice(),
        [FieldAction::WriteF32 { value, .. }] if *value == 150.0
    ));
    // Sem o membro da struct, o motivo cita o caminho.
    let w = world(&["InitialValue", "MaxValue"]);
    assert_eq!(
        w.catalog.avail("comida-infinita"),
        Err("campo HungerSettings.MaxValue não encontrado".to_string())
    );
}

#[test]
fn component_wrong_class_absent() {
    let w = world(&[]);
    // Um componente de classe errada (ou nulo) deixa só os cheats dele esperando; o resto segue.
    for bad in [w.stamina, 0] {
        w.write_u64(w.pawn + u64::from(HEALTH_OFF), bad);
        let snap = w.read().unwrap();
        for id in ["vida-infinita", "nao-morrer"] {
            assert_eq!(
                snap.missing.get(id),
                Some(&Absent::ComponentWrongClass("HealthComponent".into())),
                "{id}"
            );
            assert!(!snap.values.contains_key(id), "{id}");
        }
        assert!(snap.values.contains_key("stamina-infinita"));
        assert!(snap.values.contains_key("sem-dano-queda"));
    }
    // O próprio pawn de classe errada para `object = "pawn"` continua derrubando a leitura.
    w.write_u64(w.controller + u64::from(PAWN_OFF), w.creature);
    assert_eq!(w.read(), Err(Absent::PawnWrongClass));
}

#[test]
fn props_size_exceeded_absent() {
    let w = world(&[]);
    // A classe do componente diz que o objeto acaba antes do byte dos bits: só os cheats dele esperam.
    let props = w.health_class + u64::from(w.layout.ustruct.properties_size);
    w.mem.write(props, &DIE_BYTE.to_le_bytes());
    let snap = w.read().unwrap();
    assert!(matches!(
        snap.missing.get("nao-morrer"),
        Some(Absent::FieldOutsideObject(_))
    ));
    assert!(snap.values.contains_key("stamina-infinita"));

    // O campo do próprio pawn (sem caminho) fora do objeto derruba a leitura.
    let w = world(&[]);
    let class = gt_ue::read::read_u64(&w.mem, w.pawn + u64::from(w.layout.uobject.class)).unwrap();
    w.mem.write(
        class + u64::from(w.layout.ustruct.properties_size),
        &FALL_BYTE.to_le_bytes(),
    );
    assert!(matches!(w.read(), Err(Absent::FieldOutsideObject(_))));
}

#[test]
fn raise_never_lowers() {
    let w = world(&[]);
    w.write_f32(w.stamina + u64::from(CUR_STAMINA), 120.0);
    assert!(w.plan(&["stamina-infinita"]).is_empty());
    // Abaixo do máximo, sobe até ele.
    w.write_f32(w.stamina + u64::from(CUR_STAMINA), 40.0);
    assert!(matches!(
        w.plan(&["stamina-infinita"]).as_slice(),
        [FieldAction::WriteF32 { value, addr, .. }]
            if *value == 100.0 && *addr == w.stamina + u64::from(CUR_STAMINA)
    ));
}

#[test]
fn lower_writes_zero_damage() {
    let w = world(&[]);
    w.write_f32(w.health + u64::from(DAMAGE_OFF), 25.0);
    let actions = w.plan(&["vida-infinita"]);
    let [
        FieldAction::WriteF32 {
            id,
            addr,
            value,
            guard,
        },
    ] = actions.as_slice()
    else {
        panic!("esperada uma escrita: {actions:?}");
    };
    assert_eq!(id, "vida-infinita");
    assert_eq!(*addr, w.health + u64::from(DAMAGE_OFF));
    assert_eq!(*value, 0.0);
    // Guarda: vtable, `InternalIndex` e `ClassPrivate` do componente.
    let u = w.layout.uobject;
    let addrs: Vec<u64> = guard.iter().map(|g| g.addr).collect();
    assert_eq!(
        addrs,
        vec![
            w.health,
            w.health + u64::from(u.index),
            w.health + u64::from(u.class)
        ]
    );
    assert_eq!(guard[2].bytes, w.health_class.to_le_bytes());
}

#[test]
fn exact_temperature() {
    let w = world(&[]);
    for current in [30.0, 40.0] {
        w.write_f32(w.temperature + u64::from(CUR_TEMP), current);
        assert!(matches!(
            w.plan(&["temperatura-estavel"]).as_slice(),
            [FieldAction::WriteF32 { value, .. }] if *value == NEUTRAL_TEMP
        ));
    }
}

#[test]
fn writes_only_when_changed() {
    let w = world(&[]);
    let all: Vec<String> = defs().into_iter().map(|d| d.id).collect();
    let all: Vec<&str> = all.iter().map(String::as_str).collect();
    // Tudo já no alvo e bits limpos: nada a fazer.
    assert!(w.plan(&all).is_empty());
    // Desligado não escreve, mesmo fora do alvo.
    w.write_f32(w.survival + u64::from(CUR_FOOD), 10.0);
    assert!(w.plan(&["agua-infinita"]).is_empty());
    assert_eq!(w.plan(&["comida-infinita"]).len(), 1);
}

#[test]
fn nan_or_out_of_range_target_is_invalid() {
    let w = world(&[]);
    w.write_f32(w.stamina + u64::from(CUR_STAMINA), 10.0);
    for max in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1.0e9, -1.0e9] {
        w.write_f32(w.stamina + u64::from(MAX_STAMINA), max);
        let actions = w.plan(&["stamina-infinita"]);
        assert!(
            matches!(
                actions.as_slice(),
                [FieldAction::InvalidTarget { id, .. }] if id == "stamina-infinita"
            ),
            "alvo {max}: {actions:?}"
        );
    }
}

#[test]
fn bits_same_byte_merged_mask() {
    let w = world(&[]);
    // Os dois bits ligados e um bit alheio no mesmo byte.
    w.mem.write(w.health + u64::from(DIE_BYTE), &[0x83]);
    let actions = w.plan(&["nao-morrer"]);
    let [FieldAction::ClearBits { addr, mask, .. }] = actions.as_slice() else {
        panic!("esperada uma limpeza: {actions:?}");
    };
    assert_eq!(*addr, w.health + u64::from(DIE_BYTE));
    assert_eq!(*mask, 0x03);
    // Bit do próprio pawn.
    w.mem.write(w.pawn + u64::from(FALL_BYTE), &[FALL_MASK]);
    assert!(matches!(
        w.plan(&["sem-dano-queda"]).as_slice(),
        [FieldAction::ClearBits { addr, mask, .. }]
            if *addr == w.pawn + u64::from(FALL_BYTE) && *mask == FALL_MASK
    ));
}

#[test]
fn bits_already_clear_no_action() {
    let w = world(&[]);
    w.mem.write(w.health + u64::from(DIE_BYTE), &[0x80]);
    assert!(w.plan(&["nao-morrer"]).is_empty());
}

#[test]
fn report_lists_pointers_values_and_bits() {
    let w = world(&["Character"]);
    w.mem.write(w.health + u64::from(DIE_BYTE), &[0x01]);
    let report = player_report(&w.mem, &w.index, &w.catalog, &w.objects, w.controller).unwrap();
    assert_eq!(report.pawn, Some(w.pawn));
    assert_eq!(report.character, None);
    assert_eq!(report.acknowledged_pawn, Some(0));
    let die = report.fields.iter().find(|f| f.id == "nao-morrer").unwrap();
    let FieldReportValue::Bits { bits } = &die.value else {
        panic!("nao-morrer não é bits");
    };
    let set: Vec<(&str, bool)> = bits.iter().map(|b| (b.field.as_str(), b.set)).collect();
    assert_eq!(
        set,
        vec![("bCanDieFromDamage", true), ("bCanDieEver", false)]
    );
    let food = report
        .fields
        .iter()
        .find(|f| f.id == "comida-infinita")
        .unwrap();
    assert!(matches!(
        food.value,
        FieldReportValue::Lock { current, target, .. } if current == 100.0 && target == 100.0
    ));
}

#[test]
fn controller_needs_search_after_30s_null() {
    let now = Instant::now();
    assert!(controller_needs_search(false, None, now));
    assert!(!controller_needs_search(true, None, now));
    let since = now - Duration::from_secs(10);
    assert!(!controller_needs_search(true, Some(since), now));
    let since = now - Duration::from_secs(31);
    assert!(controller_needs_search(true, Some(since), now));
}

/// Leitor que conta as leituras em lote: só a varredura da tabela usa esse caminho.
struct Counting<'a> {
    inner: &'a SparseMem,
    batches: AtomicUsize,
}

impl ReadMem for Counting<'_> {
    fn read_exact(&self, addr: u64, buf: &mut [u8]) -> gt_core::Result<()> {
        self.inner.read_exact(addr, buf)
    }

    fn read_many(&self, reqs: &[(u64, usize)]) -> gt_core::Result<Vec<Option<Vec<u8>>>> {
        self.batches.fetch_add(1, Ordering::Relaxed);
        self.inner.read_many(reqs)
    }
}

fn watch(
    w: &World,
    mem: &Counting<'_>,
    controller: Option<u64>,
    rounds: u32,
    mut between: impl FnMut(u32),
) -> Vec<Result<PlayerReport, Absent>> {
    let opts = WatchOpts {
        controller,
        every: Duration::from_secs(2),
        rounds: Some(rounds),
        budget: budget(),
    };
    let mut seen = Vec::new();
    let mut sleeps = 0u32;
    let mut sleep = |d: Duration| {
        assert_eq!(d, Duration::from_secs(2));
        sleeps += 1;
        between(sleeps);
    };
    watch_player(
        mem,
        &w.index,
        &w.catalog,
        w.gobjects,
        &opts,
        &mut sleep,
        &mut |r| {
            seen.push(r.cloned().map_err(Clone::clone));
        },
    )
    .unwrap();
    seen
}

#[test]
fn watch_player_rounds_without_rescan() {
    let w = world(&[]);
    let mem = Counting {
        inner: &w.mem,
        batches: AtomicUsize::new(0),
    };
    let mut after_first = None;
    let seen = watch(&w, &mem, None, 3, |_| {
        after_first.get_or_insert(mem.batches.load(Ordering::Relaxed));
    });
    assert_eq!(seen.len(), 3);
    assert!(
        seen.iter()
            .all(|r| r.as_ref().is_ok_and(|r| r.controller == w.controller))
    );
    // A varredura aconteceu na primeira rodada, e nada mais foi lido em lote depois dela.
    let first = after_first.unwrap();
    assert!(first > 0);
    assert_eq!(mem.batches.load(Ordering::Relaxed), first);

    // Com o controller dado, nenhuma varredura.
    let mem = Counting {
        inner: &w.mem,
        batches: AtomicUsize::new(0),
    };
    let seen = watch(&w, &mem, Some(w.controller), 2, |_| {});
    assert!(seen.iter().all(Result::is_ok));
    assert_eq!(mem.batches.load(Ordering::Relaxed), 0);
}

#[test]
fn watch_player_rescans_when_controller_invalid() {
    let w = world(&[]);
    let mem = Counting {
        inner: &w.mem,
        batches: AtomicUsize::new(0),
    };
    // Depois da primeira rodada, o pawn passa para o segundo controller e o primeiro some da
    // tabela (o item dele passa a apontar para o segundo).
    let seen = watch(&w, &mem, Some(w.controller), 3, |n| {
        if n == 1 {
            let u = w.layout.uobject;
            let index = gt_ue::read::read_u32(&w.mem, w.controller + u64::from(u.index)).unwrap();
            let per = u64::from(w.layout.objects.items_per_chunk);
            let item = w.objects.chunks[(u64::from(index) / per) as usize]
                + (u64::from(index) % per) * u64::from(w.layout.objects.item_size);
            w.write_u64(
                item + u64::from(w.layout.objects.item_object),
                w.second_controller,
            );
            w.write_u64(w.second_controller + u64::from(PAWN_OFF), w.pawn);
        }
    });
    let controllers: Vec<u64> = seen
        .iter()
        .map(|r| r.as_ref().unwrap().controller)
        .collect();
    assert_eq!(
        controllers,
        vec![w.controller, w.second_controller, w.second_controller]
    );
    assert!(mem.batches.load(Ordering::Relaxed) > 0);

    // Sem nenhum controller na tabela, a rodada diz por quê e segue.
    let w = world(&[]);
    w.write_u64(w.controller + u64::from(PAWN_OFF), 0);
    let mem = Counting {
        inner: &w.mem,
        batches: AtomicUsize::new(0),
    };
    let seen = watch(&w, &mem, None, 1, |_| {});
    assert_eq!(seen, vec![Err(Absent::ControllerAmbiguous)]);
}

// ---- pet (caminho com vários saltos) ----

fn pet_defs() -> Vec<FieldDef> {
    let path = |component: &str| format!("PetMasterComponent.CurrentPetFull.{component}");
    vec![
        lock(
            "pet-vida-infinita",
            &path("HealthComponent"),
            "HealthComponent",
            "CurrentDamage",
            LockMode::Lower,
            LockTarget::Const(0.0),
        ),
        bits(
            "pet-nao-morre",
            &path("HealthComponent"),
            "HealthComponent",
            &["bCanDieFromDamage", "bCanDieEver"],
        ),
    ]
}

/// Catálogo com os cheats do personagem e os do pet.
fn with_pet(w: &World) -> FieldCatalog {
    let mut defs = defs();
    defs.extend(pet_defs());
    FieldCatalog::resolve(&w.index, &w.mem, &cfg(), &defs)
}

fn read_with(w: &World, catalog: &FieldCatalog) -> Result<PlayerSnapshot, Absent> {
    read_player(&w.mem, &w.index, catalog, &w.objects, w.controller)
}

#[test]
fn pet_fields_resolve_through_property_class() {
    let w = world(&[]);
    let catalog = with_pet(&w);
    assert_eq!(catalog.avail("pet-vida-infinita"), Ok(()));
    let Some(Ok(r)) = catalog.get("pet-vida-infinita") else {
        panic!("pet não resolvido");
    };
    assert_eq!(r.object_path.len(), 3);
    assert!(r.is_secondary());
    let snap = read_with(&w, &catalog).unwrap();
    assert!(snap.missing.is_empty());
    match snap.values["pet-vida-infinita"] {
        FieldValues::Lock { obj, .. } => assert_eq!(obj, w.pet_health),
        ref other => panic!("{other:?}"),
    }
    // O pet é escrito; o personagem continua com o dele.
    w.write_f32(w.pet_health + u64::from(DAMAGE_OFF), 30.0);
    let on = vec!["pet-vida-infinita".to_string()];
    let actions = plan_tick(&snap_of(&w, &catalog), &catalog, &on);
    assert!(matches!(
        actions.as_slice(),
        [FieldAction::WriteF32 { addr, .. }] if *addr == w.pet_health + u64::from(DAMAGE_OFF)
    ));
}

fn snap_of(w: &World, catalog: &FieldCatalog) -> PlayerSnapshot {
    read_with(w, catalog).unwrap()
}

#[test]
fn pet_absent_only_affects_pet_fields() {
    let w = world(&[]);
    let catalog = with_pet(&w);
    // Sem pet chamado: o personagem continua valendo.
    w.write_u64(w.pet_master + u64::from(CURRENT_PET_OFF), 0);
    let snap = read_with(&w, &catalog).unwrap();
    assert_eq!(snap.values.len(), 8);
    assert_eq!(
        snap.missing.get("pet-vida-infinita"),
        Some(&Absent::ComponentWrongClass("CurrentPetFull".into()))
    );
    assert!(snap.missing.contains_key("pet-nao-morre"));
    // O pet chamado de novo: volta a valer.
    w.write_u64(w.pet_master + u64::from(CURRENT_PET_OFF), w.pet);
    let snap = read_with(&w, &catalog).unwrap();
    assert!(snap.missing.is_empty());
    assert_eq!(snap.values.len(), 10);
}

#[test]
fn pet_pointing_to_player_never_written() {
    let w = world(&[]);
    let catalog = with_pet(&w);
    // O ponteiro do pet apontando para o próprio personagem (não é SurvivalCreature).
    w.write_u64(w.pet_master + u64::from(CURRENT_PET_OFF), w.pawn);
    let snap = read_with(&w, &catalog).unwrap();
    assert!(snap.missing.contains_key("pet-vida-infinita"));
    assert!(!snap.values.contains_key("pet-vida-infinita"));
}

#[test]
fn pet_path_without_property_class_is_unavailable() {
    let w = world(&["CurrentPetFull"]);
    let catalog = with_pet(&w);
    assert_eq!(
        catalog.avail("pet-vida-infinita"),
        Err("campo CurrentPetFull não encontrado".to_string())
    );
    // O personagem não é afetado.
    assert_eq!(catalog.avail("vida-infinita"), Ok(()));
}

// ---- set, add e game_state (spec 006, 6.2 a 6.4 e 6.6) ----

fn set_def(
    id: &str,
    object: &str,
    class: &str,
    fields: &[&str],
    kind: FieldKind,
    target: f64,
) -> FieldDef {
    FieldDef {
        id: id.into(),
        name: id.into(),
        category: Category::Sobrevivencia,
        description: String::new(),
        object: object.into(),
        object_class: format!("{MAINE}.{class}"),
        root: FieldRoot::Pawn,
        value: None,
        rule: FieldRule::Set {
            fields: fields.iter().map(|s| s.to_string()).collect(),
            kind,
            target: Some(target),
            bools: None,
        },
    }
}

fn add_def() -> FieldDef {
    FieldDef {
        id: "somar-ciencia".into(),
        name: "Somar ciência".into(),
        category: Category::Sobrevivencia,
        description: String::new(),
        object: "PartyComponent".into(),
        object_class: format!("{MAINE}.PartyComponent"),
        root: FieldRoot::GameState,
        value: None,
        rule: FieldRule::Add {
            field: "ScienceFound".into(),
            result_max: 100_000,
        },
    }
}

/// Os cheats novos da spec 6.7.
fn new_defs() -> Vec<FieldDef> {
    vec![
        set_def(
            "carga",
            "HaulingComponent",
            "HaulingComponent",
            &["Capacity"],
            FieldKind::Int,
            20.0,
        ),
        set_def(
            "dano",
            "pawn",
            "SurvivalCharacter",
            &["CustomDamageMultiplier"],
            FieldKind::Float,
            10.0,
        ),
        set_def(
            "velocidade",
            "CharacterMovement",
            "MaineCharMovementComponent",
            &["CustomGroundSpeedMultiplier", "CustomSwimSpeedMultiplier"],
            FieldKind::Float,
            2.0,
        ),
        set_def(
            "gravidade",
            "CharacterMovement",
            "MaineCharMovementComponent",
            &["GravityScale"],
            FieldKind::Float,
            0.5,
        ),
        add_def(),
    ]
}

/// Catálogo com os cheats antigos, os novos e o caminho do `GameState`.
fn full_catalog(w: &World, cfg: &PlayerCfg) -> FieldCatalog {
    let mut all = defs();
    all.extend(new_defs());
    FieldCatalog::resolve(&w.index, &w.mem, cfg, &all)
}

fn read_gs(
    w: &World,
    catalog: &FieldCatalog,
) -> Result<(u64, u64, Vec<GuardBytes>), GameStateAbsent> {
    read_game_state(
        &w.mem,
        &w.index,
        catalog,
        &w.objects,
        w.controller,
        "somar-ciencia",
    )
}

#[test]
fn int_field_checks_type_and_size() {
    let w = world(&[]);
    // Um `FloatProperty` pedido como `int`, e um `IntProperty` pedido como `float`.
    let mut all = new_defs();
    all.push(set_def(
        "float-como-int",
        "pawn",
        "SurvivalCharacter",
        &["CustomDamageMultiplier"],
        FieldKind::Int,
        1.0,
    ));
    all.push(set_def(
        "int-como-float",
        "HaulingComponent",
        "HaulingComponent",
        &["Capacity"],
        FieldKind::Float,
        1.0,
    ));
    let catalog = FieldCatalog::resolve(&w.index, &w.mem, &cfg_gs(), &all);
    assert_eq!(
        catalog.avail("float-como-int"),
        Err("reflexão incoerente".to_string())
    );
    assert_eq!(
        catalog.avail("int-como-float"),
        Err("reflexão incoerente".to_string())
    );
    assert_eq!(catalog.avail("carga"), Ok(()));
}

#[test]
fn resolves_new_fields_by_name() {
    let w = world(&[]);
    let catalog = full_catalog(&w, &cfg_gs());
    for def in new_defs() {
        assert_eq!(catalog.avail(&def.id), Ok(()), "{}", def.id);
    }
    let Some(Ok(carga)) = catalog.get("carga") else {
        panic!("carga não resolvida");
    };
    assert_eq!(carga.root, FieldRoot::Pawn);
    assert_eq!(carga.object_path[0].field.offset, HAULING_OFF);
    let ResolvedRule::Set {
        fields,
        kind,
        target,
    } = &carga.rule
    else {
        panic!("carga não é set");
    };
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].offset, CAPACITY_OFF);
    assert_eq!((*kind, *target), (FieldKind::Int, Some(20.0)));

    let Some(Ok(vel)) = catalog.get("velocidade") else {
        panic!("velocidade não resolvida");
    };
    let ResolvedRule::Set { fields, .. } = &vel.rule else {
        panic!("velocidade não é set");
    };
    let offsets: Vec<u32> = fields.iter().map(|f| f.offset).collect();
    assert_eq!(offsets, [GROUND_MULT_OFF, SWIM_MULT_OFF]);

    let Some(Ok(dano)) = catalog.get("dano") else {
        panic!("dano não resolvido");
    };
    assert!(dano.object_path.is_empty());

    let Some(Ok(ciencia)) = catalog.get("somar-ciencia") else {
        panic!("ciência não resolvida");
    };
    assert_eq!(ciencia.root, FieldRoot::GameState);
    assert_eq!(ciencia.object_path[0].field.offset, PARTY_OFF);
    let ResolvedRule::Add { field, result_max } = &ciencia.rule else {
        panic!("ciência não é add");
    };
    assert_eq!((field.offset, *result_max), (SCIENCE_OFF, 100_000));
}

#[test]
fn game_state_class_missing_only_affects_action() {
    let w = world(&[]);
    let mut bad = cfg_gs();
    if let Some(gs) = bad.game_state.as_mut() {
        gs.game_state_class = format!("{MAINE}.NaoExiste");
    }
    let catalog = full_catalog(&w, &bad);
    assert_eq!(
        catalog.avail("somar-ciencia"),
        Err("classe NaoExiste não encontrada".to_string())
    );
    for id in ["vida-infinita", "carga", "dano", "velocidade", "gravidade"] {
        assert_eq!(catalog.avail(id), Ok(()), "{id}");
    }
    assert!(w.read_with_catalog(&catalog).is_ok());

    // Campo do caminho que falta: também só a ação.
    let w = world(&["OwningWorld"]);
    let catalog = full_catalog(&w, &cfg_gs());
    assert_eq!(
        catalog.avail("somar-ciencia"),
        Err("campo OwningWorld não encontrado".to_string())
    );
    assert_eq!(catalog.avail("carga"), Ok(()));

    // Sem o bloco no `[ue.player]`, a ação fica indisponível e o resto segue.
    let w = world(&[]);
    let catalog = full_catalog(&w, &cfg());
    assert!(catalog.avail("somar-ciencia").is_err());
    assert_eq!(catalog.avail("carga"), Ok(()));
}

#[test]
fn pawn_class_missing_only_affects_pawn_root() {
    let w = world(&[]);
    let mut bad = cfg_gs();
    bad.pawn_class = format!("{MAINE}.NaoExiste");
    let catalog = full_catalog(&w, &bad);
    let reason = Err("classe NaoExiste não encontrada".to_string());
    for id in ["vida-infinita", "carga", "dano", "velocidade"] {
        assert_eq!(catalog.avail(id), reason, "{id}");
    }
    assert_eq!(catalog.avail("somar-ciencia"), Ok(()));
    // A ação segue funcionando sem o pawn.
    assert!(read_gs(&w, &catalog).is_ok());
}

impl World {
    fn read_with_catalog(&self, catalog: &FieldCatalog) -> Result<PlayerSnapshot, Absent> {
        read_with(self, catalog)
    }
}

#[test]
fn hauling_null_only_waits_carga() {
    let w = world(&[]);
    let catalog = full_catalog(&w, &cfg_gs());
    let snap = w.read_with_catalog(&catalog).unwrap();
    // O `game_state` não é lido pelo snapshot do pawn.
    assert!(!snap.values.contains_key("somar-ciencia"));
    assert!(!snap.missing.contains_key("somar-ciencia"));
    match &snap.values["carga"] {
        FieldValues::Set { obj, fields } => {
            assert_eq!(*obj, w.hauling);
            assert_eq!(
                fields,
                &[(w.hauling + u64::from(CAPACITY_OFF), 20i32.to_le_bytes())]
            );
        }
        other => panic!("{other:?}"),
    }

    w.write_u64(w.pawn + u64::from(HAULING_OFF), 0);
    let snap = w.read_with_catalog(&catalog).unwrap();
    assert!(snap.values.contains_key("vida-infinita"));
    assert!(snap.values.contains_key("dano"));
    assert!(!snap.values.contains_key("carga"));
    assert_eq!(
        snap.missing.get("carga"),
        Some(&Absent::ComponentWrongClass("HaulingComponent".into()))
    );
    let Some(Ok(carga)) = catalog.get("carga") else {
        panic!("carga não resolvida");
    };
    assert_eq!(missing_kind(carga), MissingKind::Component);
}

#[test]
fn pet_missing_keeps_waiting_pet() {
    let w = world(&[]);
    let mut all = defs();
    all.extend(new_defs());
    all.extend(pet_defs());
    let catalog = FieldCatalog::resolve(&w.index, &w.mem, &cfg_gs(), &all);
    w.write_u64(w.pet_master + u64::from(CURRENT_PET_OFF), 0);
    let snap = w.read_with_catalog(&catalog).unwrap();
    assert!(snap.missing.contains_key("pet-vida-infinita"));
    assert!(snap.values.contains_key("carga"));
    let Some(Ok(pet)) = catalog.get("pet-vida-infinita") else {
        panic!("pet não resolvido");
    };
    assert_eq!(missing_kind(pet), MissingKind::Pet);
}

#[test]
fn movement_null_waits_speed_and_gravity() {
    let w = world(&[]);
    let catalog = full_catalog(&w, &cfg_gs());
    let snap = w.read_with_catalog(&catalog).unwrap();
    assert!(matches!(
        snap.values.get("gravidade"),
        Some(FieldValues::Set { obj, .. }) if *obj == w.movement
    ));
    w.write_u64(w.pawn + u64::from(MOVEMENT_OFF), 0);
    let snap = w.read_with_catalog(&catalog).unwrap();
    for id in ["velocidade", "gravidade"] {
        assert!(snap.missing.contains_key(id), "{id}");
        assert!(!snap.values.contains_key(id), "{id}");
    }
    for id in ["vida-infinita", "carga", "dano"] {
        assert!(snap.values.contains_key(id), "{id}");
    }
    // O objeto de movimento de classe errada (o de carga) também só espera.
    w.write_u64(w.pawn + u64::from(MOVEMENT_OFF), w.hauling);
    let snap = w.read_with_catalog(&catalog).unwrap();
    assert!(snap.missing.contains_key("gravidade"));
    assert!(snap.values.contains_key("carga"));
}

#[test]
fn plan_set_nan_and_negative_zero() {
    let zero = 0.0f32.to_le_bytes();
    let values = FieldValues::Set {
        obj: 0x100,
        fields: vec![
            (0x10, f32::NAN.to_le_bytes()),
            (0x20, (-0.0f32).to_le_bytes()),
            (0x30, 0.0f32.to_le_bytes()),
            (0x40, f32::INFINITY.to_le_bytes()),
            (0x50, 2.0e9f32.to_le_bytes()),
        ],
    };
    let steps = plan_set(&values, zero, FieldKind::Float);
    let sane: Vec<bool> = steps.iter().map(|s| s.sane).collect();
    assert_eq!(sane, [false, true, true, false, false]);
    // `-0.0` tem os mesmos números que `0.0`, mas bytes diferentes: conta como diferente.
    let same: Vec<bool> = steps.iter().map(|s| s.same).collect();
    assert_eq!(same, [false, false, true, false, false]);
    assert_eq!(steps[1].addr, 0x20);

    // `int`: fora de [-1e9, 1e9] não é são.
    let ints = FieldValues::Set {
        obj: 0x100,
        fields: vec![
            (0x10, 2_000_000_000i32.to_le_bytes()),
            (0x20, (-5i32).to_le_bytes()),
        ],
    };
    let sane: Vec<bool> = plan_set(&ints, 20i32.to_le_bytes(), FieldKind::Int)
        .iter()
        .map(|s| s.sane)
        .collect();
    assert_eq!(sane, [false, true]);
    // Valores de outra regra não geram passos.
    let lock = FieldValues::Bits {
        obj: 1,
        bytes: Vec::new(),
    };
    assert!(plan_set(&lock, zero, FieldKind::Float).is_empty());
}

#[test]
fn encode_value_is_little_endian() {
    assert_eq!(encode_value(FieldKind::Int, 1000.0), 1000i32.to_le_bytes());
    assert_eq!(encode_value(FieldKind::Float, 0.5), 0.5f32.to_le_bytes());
}

#[test]
fn bool_kind_is_inert_until_resolution_exists() {
    // Até a Tarefa 3 nenhum byte de `bool` é são, então nada é escrito.
    assert!(!value_is_sane(FieldKind::Bool, [0, 0, 0, 0]));
    assert!(!value_is_sane(FieldKind::Bool, [1, 0, 0, 0]));
    assert_eq!(encode_value(FieldKind::Bool, 1.0), [1, 0, 0, 0]);
    assert_eq!(encode_value(FieldKind::Bool, 0.0), [0, 0, 0, 0]);
    assert_eq!(decode_value(FieldKind::Bool, &[1]), 1.0);
    assert_eq!(format_value(None, FieldKind::Bool, 1.0), "true");
    assert_eq!(format_value(None, FieldKind::Bool, 0.0), "false");
}

#[test]
fn read_game_state_checks_each_hop() {
    let w = world(&[]);
    let catalog = full_catalog(&w, &cfg_gs());
    let (obj, addr, guard) = read_gs(&w, &catalog).unwrap();
    assert_eq!(obj, w.party);
    assert_eq!(addr, w.party + u64::from(SCIENCE_OFF));
    assert_eq!(guard.len(), 3);

    let outer = w.controller + u64::from(w.layout.uobject.outer);
    let cases: [(u64, u64, &str); 6] = [
        (outer, w.pawn, "Outer"),
        (outer, 0, "Outer"),
        (w.level + u64::from(WORLD_OFF), 0, "OwningWorld"),
        (w.level + u64::from(WORLD_OFF), w.pawn, "OwningWorld"),
        (w.world + u64::from(GAME_STATE_OFF), w.pawn, "GameState"),
        (w.game_state + u64::from(PARTY_OFF), 0, "PartyComponent"),
    ];
    for (at, bad, hop) in cases {
        let good = gt_ue::read::read_u64(&w.mem, at).unwrap();
        w.write_u64(at, bad);
        assert_eq!(
            read_gs(&w, &catalog).map(|_| ()),
            Err(GameStateAbsent::Missing(hop.to_string())),
            "{hop}"
        );
        w.write_u64(at, good);
        assert!(read_gs(&w, &catalog).is_ok(), "{hop} restaurado");
    }
    // O `GameState` de outra classe (um pawn) e a classe final errada.
    w.write_u64(w.game_state + u64::from(PARTY_OFF), w.hauling);
    assert_eq!(
        read_gs(&w, &catalog).map(|_| ()),
        Err(GameStateAbsent::Missing("PartyComponent".into()))
    );
    w.write_u64(w.game_state + u64::from(PARTY_OFF), w.party);
    // `PropertiesSize` do componente menor que o campo.
    let party_class =
        gt_ue::read::read_u64(&w.mem, w.party + u64::from(w.layout.uobject.class)).unwrap();
    let props = party_class + u64::from(w.layout.ustruct.properties_size);
    w.mem.write(props, &SCIENCE_OFF.to_le_bytes());
    assert!(matches!(
        read_gs(&w, &catalog),
        Err(GameStateAbsent::Missing(_))
    ));
}

#[test]
fn read_game_state_rejects_invalid_controller() {
    let w = world(&[]);
    let catalog = full_catalog(&w, &cfg_gs());
    let got = read_game_state(
        &w.mem,
        &w.index,
        &catalog,
        &w.objects,
        w.pawn,
        "somar-ciencia",
    );
    assert_eq!(got.map(|_| ()), Err(GameStateAbsent::ControllerInvalid));
}

#[test]
fn read_game_state_pawn_null_ok() {
    let w = world(&[]);
    let catalog = full_catalog(&w, &cfg_gs());
    w.write_u64(w.controller + u64::from(PAWN_OFF), 0);
    assert_eq!(w.read_with_catalog(&catalog), Err(Absent::PawnNull));
    let (obj, _, _) = read_gs(&w, &catalog).unwrap();
    assert_eq!(obj, w.party);
}

#[test]
fn read_game_state_refuses_add_with_pawn_root() {
    let w = world(&[]);
    // Um `add` de raiz `pawn` resolve, mas não é lido nesta spec: nem pelo `GameState`...
    let mut all = defs();
    all.extend(new_defs());
    all.push(FieldDef {
        id: "somar-carga".into(),
        name: "Somar carga".into(),
        object: "HaulingComponent".into(),
        object_class: format!("{MAINE}.HaulingComponent"),
        root: FieldRoot::Pawn,
        rule: FieldRule::Add {
            field: "Capacity".into(),
            result_max: 1000,
        },
        ..add_def()
    });
    let catalog = FieldCatalog::resolve(&w.index, &w.mem, &cfg_gs(), &all);
    assert!(catalog.avail("somar-carga").is_ok());
    let got = read_game_state(
        &w.mem,
        &w.index,
        &catalog,
        &w.objects,
        w.controller,
        "somar-carga",
    );
    assert_eq!(
        got.map(|_| ()),
        Err(GameStateAbsent::Missing("somar-carga".into()))
    );
    // ...nem pelo ciclo, que o ignora sem derrubar a leitura do personagem.
    let snap = w.read_with_catalog(&catalog).unwrap();
    assert!(!snap.values.contains_key("somar-carga"));
    assert!(!snap.missing.contains_key("somar-carga"));
}

#[test]
fn plan_add_cases() {
    assert_eq!(plan_add(7400, 1000, 100_000), Ok(8400));
    assert_eq!(plan_add(-1, 1000, 100_000), Err(AddRefusal::OutOfRange));
    assert_eq!(plan_add(100_001, 1, 100_000), Err(AddRefusal::OutOfRange));
    assert_eq!(
        plan_add(99_500, 1000, 100_000),
        Err(AddRefusal::WouldExceed)
    );
    assert_eq!(plan_add(99_000, 1000, 100_000), Ok(100_000));
    // Perto de `i32::MAX`: a soma é em 64 bits e não estoura.
    assert_eq!(
        plan_add(i32::MAX, i32::MAX, i64::MAX),
        Err(AddRefusal::WouldExceed)
    );
    assert_eq!(plan_add(i32::MAX - 1, 1, i64::from(i32::MAX)), Ok(i32::MAX));
    assert_eq!(
        plan_add(i32::MAX, 1, i64::from(i32::MAX)),
        Err(AddRefusal::WouldExceed)
    );
}

// ---- relatório com carga, dano, ciência e movimento (spec 006, 6.10) ----

fn report_with(w: &World, catalog: &FieldCatalog) -> PlayerReport {
    player_report(&w.mem, &w.index, catalog, &w.objects, w.controller).unwrap()
}

fn report_field<'a>(report: &'a PlayerReport, id: &str) -> &'a FieldReportValue {
    &report.fields.iter().find(|f| f.id == id).unwrap().value
}

#[test]
fn report_includes_set_and_add() {
    let w = world(&[]);
    w.mem
        .write(w.party + u64::from(SCIENCE_OFF), &1234i32.to_le_bytes());
    let catalog = full_catalog(&w, &cfg_gs());
    let report = report_with(&w, &catalog);

    let FieldReportValue::Set { fields } = report_field(&report, "velocidade") else {
        panic!("velocidade não é set");
    };
    let got: Vec<(&str, u64)> = fields.iter().map(|f| (f.name.as_str(), f.addr)).collect();
    assert_eq!(
        got,
        vec![
            (
                "CustomGroundSpeedMultiplier",
                w.movement + u64::from(GROUND_MULT_OFF)
            ),
            (
                "CustomSwimSpeedMultiplier",
                w.movement + u64::from(SWIM_MULT_OFF)
            ),
        ]
    );
    // Sem grade, o valor atual aparece formatado pelo tipo (1.0 no mundo sintético).
    assert!(fields.iter().all(|f| !f.value.is_empty()));

    let FieldReportValue::Set { fields } = report_field(&report, "carga") else {
        panic!("carga não é set");
    };
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].addr, w.hauling + u64::from(CAPACITY_OFF));

    assert_eq!(
        report_field(&report, "somar-ciencia"),
        &FieldReportValue::Add {
            addr: w.party + u64::from(SCIENCE_OFF),
            current: 1234,
        }
    );
}

#[test]
fn report_game_state_path() {
    let w = world(&[]);
    let catalog = full_catalog(&w, &cfg_gs());
    let report = report_with(&w, &catalog);
    assert_eq!(
        report.game_state,
        Some(GameStateReport {
            level: Some(w.level),
            world: Some(w.world),
            game_state: Some(w.game_state),
            object: Some(w.party),
            error: None,
        })
    );
    // Sem cheat de `game_state` no catálogo, não há caminho a mostrar.
    let plain = FieldCatalog::resolve(&w.index, &w.mem, &cfg_gs(), &defs());
    assert_eq!(report_with(&w, &plain).game_state, None);
}

#[test]
fn report_game_state_missing_hop() {
    let w = world(&[]);
    let catalog = full_catalog(&w, &cfg_gs());
    w.write_u64(w.world + u64::from(GAME_STATE_OFF), 0);
    let report = report_with(&w, &catalog);
    // Os saltos que deram certo continuam aparecendo; o que falhou vem como motivo.
    assert_eq!(
        report.game_state,
        Some(GameStateReport {
            level: Some(w.level),
            world: Some(w.world),
            game_state: None,
            object: None,
            error: Some("GameState ausente ou de classe inesperada".into()),
        })
    );
    match report_field(&report, "somar-ciencia") {
        FieldReportValue::Unavailable { reason } => {
            assert!(reason.contains("GameState"), "{reason}");
        }
        other => panic!("{other:?}"),
    }
    // O resto do relatório segue valendo.
    assert!(matches!(
        report_field(&report, "carga"),
        FieldReportValue::Set { .. }
    ));
}
