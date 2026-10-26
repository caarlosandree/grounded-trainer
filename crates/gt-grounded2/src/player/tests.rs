//! Testes do caminho até o personagem sobre a reflexão sintética. Os offsets daqui são arbitrários
//! (de propósito, diferentes dos do jogo): os campos vêm da reflexão, por nome.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use gt_ue::synth::{SparseMem, SynthField, SynthUe};
use gt_ue::{Names, ObjectArray, ReflectionIndex, ScanBudget, UeLayout};

use super::*;
use crate::signatures::Category;

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
        0x78,
        &keep(vec![
            object("HealthComponent", HEALTH_OFF),
            object("StaminaComponent", STAMINA_OFF),
            object("SurvivalComponent", SURVIVAL_OFF),
            object("BodyTemperatureComponent", TEMP_OFF),
            boolean("bTakeFallDamage", FALL_BYTE, FALL_MASK),
            object("PetMasterComponent", PET_MASTER_OFF),
        ]),
    );
    let creature_c = s.add_class(maine, "SurvivalCreature", Some(character_c), 0x80, &[]);
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
        0x80,
        &[],
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

    let controller = s.add_object(survival_controller_c, "Controller_0", maine);
    let second_controller = s.add_object(survival_controller_c, "Controller_1", maine);
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
    };
    // Valores iniciais: tudo cheio, sem dano, temperatura neutra e bits ligados como no jogo.
    w.write_f32(stamina + u64::from(CUR_STAMINA), 100.0);
    w.write_f32(stamina + u64::from(MAX_STAMINA), 100.0);
    for off in [CUR_FOOD, 0x38, 0x40, HUNGER_OFF, THIRST_OFF, BREATH_OFF] {
        w.write_f32(survival + u64::from(off), 100.0);
    }
    w.write_f32(temperature + u64::from(CUR_TEMP), NEUTRAL_TEMP);
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
    w.write_u64(w.pawn + u64::from(HEALTH_OFF), w.stamina);
    assert_eq!(
        w.read(),
        Err(Absent::ComponentWrongClass("HealthComponent".into()))
    );
    // Componente nulo também não serve.
    w.write_u64(w.pawn + u64::from(HEALTH_OFF), 0);
    assert_eq!(
        w.read(),
        Err(Absent::ComponentWrongClass("HealthComponent".into()))
    );
}

#[test]
fn props_size_exceeded_absent() {
    let w = world(&[]);
    // A classe do componente diz que o objeto acaba antes do byte dos bits.
    let props = w.health_class + u64::from(w.layout.ustruct.properties_size);
    w.mem.write(props, &DIE_BYTE.to_le_bytes());
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
