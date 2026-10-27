//! Cena de reflexão sintética do `gt-testtarget --ue` (spec 005, 8, e spec 006, 8): um controller
//! com o pawn `SurvivalPlayerCharacter` e os quatro componentes, a carga, o movimento, os pulos e
//! o multiplicador de dano, o caminho `Outer` → `Level` → `World` → `GameState` →
//! `PartyComponent`, uma criatura `SurvivalCharacter` com componentes próprios (inclusive carga e
//! movimento) e um segundo personagem não referenciado, para simular a troca de pawn.
//!
//! Os nomes de classes e campos são os do jogo; os offsets são outros, de propósito: o caminho
//! tem de vir da reflexão, por nome. O layout das estruturas é o [`UeLayout::observed`].

use gt_core::global::GlobalPattern;
use gt_core::scan::Pattern;
use gt_core::value::{ValueGrid, ValueType};
use gt_grounded2::signatures::{
    Category, FieldDef, FieldKind, FieldRoot, FieldRule, GameStateCfg, GlobalDef, LockMode,
    LockTarget, PlayerCfg, UeSigs,
};
use gt_ue::UeLayout;
use gt_ue::synth::{SynthField, SynthImage, SynthUe};

/// Pacote das classes do jogo.
pub const MAINE: &str = "/Script/Maine";
/// Tamanho da arena da cena (o bloco de itens do layout observado ocupa 1,5 MiB).
pub const ARENA_LEN: usize = 4 << 20;

/// `Controller.Pawn`.
pub const PAWN_OFF: u32 = 0x48;
/// `SurvivalCharacter.HealthComponent`.
pub const HEALTH_OFF: u32 = 0x48;
/// `HealthComponent.CurrentDamage`.
pub const DAMAGE_OFF: u32 = 0x30;
/// Byte de `bCanDieFromDamage` (máscara `0x01`) e `bCanDieEver` (`0x02`).
pub const DIE_BYTE: u32 = 0x38;
/// `StaminaComponent.CurrentStamina`.
pub const CUR_STAMINA: u32 = 0x30;
/// Valor neutro da temperatura nas definições sintéticas.
pub const NEUTRAL_TEMP: f32 = 37.0;

const STAMINA_OFF: u32 = 0x50;
const SURVIVAL_OFF: u32 = 0x58;
const TEMP_OFF: u32 = 0x60;
const FALL_BYTE: u32 = 0x68;
const FALL_MASK: u8 = 0x04;
const MAX_STAMINA: u32 = 0x34;
/// `SurvivalComponent.HungerSettings`, `ThirstSettings` e `BreathSettings` (o máximo é `MaxValue`).
const TRAIT_SETTINGS_OFF: [u32; 3] = [0x50, 0x60, 0x70];
/// `SurvivalPlayerController.CachedSurvivalPlayerCharacter`.
pub const CACHED_OFF: u32 = 0x78;
const CUR_TEMP: u32 = 0x30;

/// `SurvivalCharacter.HaulingComponent` e `.CustomDamageMultiplier`.
pub const HAULING_OFF: u32 = 0x80;
pub const DAMAGE_MULT_OFF: u32 = 0x88;
/// `Character.CharacterMovement` e `.JumpMaxCount` (a classe `Character` é a do motor).
pub const MOVEMENT_OFF: u32 = 0x70;
pub const JUMP_OFF: u32 = 0x78;
/// `HaulingComponent.Capacity`.
pub const CAPACITY_OFF: u32 = 0x34;
/// `MaineCharMovementComponent`: os dois multiplicadores de velocidade e a gravidade.
pub const GROUND_MULT_OFF: u32 = 0x30;
pub const SWIM_MULT_OFF: u32 = 0x34;
pub const GRAVITY_OFF: u32 = 0x3c;
/// `Level.OwningWorld`, `World.GameState`, `InGameGameState.PartyComponent` e
/// `PartyComponent.ScienceFound`.
pub const WORLD_OFF: u32 = 0x40;
pub const GAME_STATE_OFF: u32 = 0x48;
pub const PARTY_OFF: u32 = 0x38;
pub const SCIENCE_OFF: u32 = 0x30;

/// Configurações do modo de jogo (spec 007): `InGameGameState.SurvivalModeManagerComponent`,
/// `SurvivalModeManagerComponent.GameModeSettings` e os três campos de
/// `SurvivalGameModeSettings` (offsets diferentes dos do jogo, de propósito).
pub const MODE_MANAGER_OFF: u32 = 0x50;
pub const SETTINGS_OFF: u32 = 0x38;
pub const RECIPES_OFF: u32 = 0x31;
pub const AUTO_COMPLETE_OFF: u32 = 0x33;
pub const ENEMY_DAMAGE_OFF: u32 = 0x3c;
/// Dano inimigo inicial das configurações da cena.
pub const ENEMY_DAMAGE_START: f32 = 1.0;

/// Valores iniciais dos campos novos (o personagem sem nenhum cheat).
pub const CAPACITY_START: i32 = 10;
pub const SCIENCE_START: i32 = 500;
pub const JUMP_START: i32 = 1;
pub const DAMAGE_MULT_START: f32 = 1.0;
pub const SPEED_START: f32 = 1.0;
pub const GRAVITY_START: f32 = 1.0;

/// Endereços da cena, como o alvo os informa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UeAddrs {
    pub gobjects: u64,
    /// Lista de blocos do `FNamePool`.
    pub names: u64,
    pub controller: u64,
    pub pawn: u64,
    pub creature: u64,
    /// `controller + Controller.Pawn`.
    pub pawn_field_addr: u64,
    /// `HealthComponent` do pawn.
    pub health: u64,
    /// Outro `SurvivalPlayerCharacter`, com componentes próprios e fora do caminho.
    pub alt_pawn: u64,
    /// `HaulingComponent` do pawn.
    pub hauling: u64,
    /// Componente de movimento do pawn (`Character.CharacterMovement`).
    pub movement: u64,
    /// `HaulingComponent` e componente de movimento da criatura (montaria): próprios, fora do
    /// caminho do personagem.
    pub creature_hauling: u64,
    pub creature_movement: u64,
    /// `Outer` do controller e do pawn.
    pub level: u64,
    pub world: u64,
    pub game_state: u64,
    pub party: u64,
    /// `SurvivalModeManagerComponent` do `GameState`.
    pub mode_manager: u64,
    /// `GameModeSettings` em uso (instância da subclasse de Blueprint).
    pub settings: u64,
    /// Outra instância da subclasse, com o bit de CDO, fora do caminho.
    pub cdo_settings: u64,
}

fn field(name: &'static str, kind: &'static str, offset: u32, size: u32) -> SynthField {
    SynthField {
        name,
        kind,
        offset,
        size,
        bool_mask: None,
    }
}

fn float(name: &'static str, offset: u32) -> SynthField {
    field(name, "FloatProperty", offset, 4)
}

fn int(name: &'static str, offset: u32) -> SynthField {
    field(name, "IntProperty", offset, 4)
}

fn object(name: &'static str, offset: u32) -> SynthField {
    field(name, "ObjectProperty", offset, 8)
}

fn boolean(name: &'static str, offset: u32, mask: u8) -> SynthField {
    SynthField {
        bool_mask: Some((0, mask)),
        ..field(name, "BoolProperty", offset, 1)
    }
}

/// Monta a cena na arena `[arena, arena + len)`. As escritas da imagem ainda têm de ser
/// aplicadas na memória.
pub fn build(arena: u64, len: usize) -> (SynthImage, UeAddrs) {
    let layout = UeLayout::observed();
    let mut s = SynthUe::new(layout, arena, len);
    let (engine, actor, object_c) = (s.engine_package, s.actor_class, s.object_class);
    let maine = s.add_package(MAINE);

    let controller_c = s.add_class(
        engine,
        "Controller",
        Some(actor),
        0x60,
        &[object("Pawn", PAWN_OFF), object("Character", 0x50)],
    );
    let player_controller_c = s.add_class(
        engine,
        "PlayerController",
        Some(controller_c),
        0x70,
        &[object("AcknowledgedPawn", 0x60)],
    );
    let survival_controller_c = s.add_class(
        maine,
        "SurvivalPlayerController",
        Some(player_controller_c),
        0x80,
        &[object("CachedSurvivalPlayerCharacter", CACHED_OFF)],
    );
    // `Character` do motor: o movimento e os pulos moram nele, como no jogo.
    let engine_character_c = s.add_class(
        engine,
        "Character",
        Some(actor),
        0x80,
        &[
            object("CharacterMovement", MOVEMENT_OFF),
            int("JumpMaxCount", JUMP_OFF),
        ],
    );
    let character_c = s.add_class(
        maine,
        "SurvivalCharacter",
        Some(engine_character_c),
        0xa0,
        &[
            object("HaulingComponent", HAULING_OFF),
            float("CustomDamageMultiplier", DAMAGE_MULT_OFF),
            object("HealthComponent", HEALTH_OFF),
            object("StaminaComponent", STAMINA_OFF),
            object("SurvivalComponent", SURVIVAL_OFF),
            object("BodyTemperatureComponent", TEMP_OFF),
            boolean("bTakeFallDamage", FALL_BYTE, FALL_MASK),
        ],
    );
    let player_character_c = s.add_class(
        maine,
        "SurvivalPlayerCharacter",
        Some(character_c),
        0xa0,
        &[],
    );
    let health_c = s.add_class(
        maine,
        "HealthComponent",
        Some(object_c),
        0x40,
        &[
            float("CurrentDamage", DAMAGE_OFF),
            float("MaxHealth", 0x34),
            boolean("bCanDieFromDamage", DIE_BYTE, 0x01),
            boolean("bCanDieEver", DIE_BYTE, 0x02),
        ],
    );
    let stamina_c = s.add_class(
        maine,
        "StaminaComponent",
        Some(object_c),
        0x38,
        &[
            float("CurrentStamina", CUR_STAMINA),
            float("MaxStamina", MAX_STAMINA),
        ],
    );
    let trait_settings = s.add_struct(
        maine,
        "SurvivalTraitSettings",
        0x10,
        &[float("MaxValue", 0x0), float("InitialValue", 0x4)],
    );
    let breath_settings = s.add_struct(maine, "BreathSettings", 0x8, &[float("MaxValue", 0x0)]);
    let [hunger, thirst, breath] = TRAIT_SETTINGS_OFF;
    let survival_c = s.add_class(
        maine,
        "SurvivalComponent",
        Some(object_c),
        0x80,
        &[
            float("CurrentFood", 0x30),
            float("CurrentWater", 0x38),
            float("CurrentBreath", 0x40),
            field("HungerSettings", "StructProperty", hunger, 0x10),
            field("ThirstSettings", "StructProperty", thirst, 0x10),
            field("BreathSettings", "StructProperty", breath, 0x10),
        ],
    );
    s.link_struct(survival_c, "HungerSettings", trait_settings);
    s.link_struct(survival_c, "ThirstSettings", trait_settings);
    s.link_struct(survival_c, "BreathSettings", breath_settings);
    let temp_c = s.add_class(
        maine,
        "BodyTemperatureComponent",
        Some(object_c),
        0x38,
        &[float("CurrentBodyTemperature", CUR_TEMP)],
    );
    let hauling_c = s.add_class(
        maine,
        "HaulingComponent",
        Some(object_c),
        0x40,
        &[int("Capacity", CAPACITY_OFF)],
    );
    let movement_c = s.add_class(
        maine,
        "MaineCharMovementComponent",
        Some(object_c),
        0x50,
        &[
            float("CustomGroundSpeedMultiplier", GROUND_MULT_OFF),
            float("CustomSwimSpeedMultiplier", SWIM_MULT_OFF),
            float("GravityScale", GRAVITY_OFF),
        ],
    );
    let level_c = s.add_class(
        engine,
        "Level",
        Some(object_c),
        0x60,
        &[object("OwningWorld", WORLD_OFF)],
    );
    let world_c = s.add_class(
        engine,
        "World",
        Some(object_c),
        0x70,
        &[object("GameState", GAME_STATE_OFF)],
    );
    let game_state_c = s.add_class(
        maine,
        "InGameGameState",
        Some(actor),
        0x60,
        &[
            object("PartyComponent", PARTY_OFF),
            object("SurvivalModeManagerComponent", MODE_MANAGER_OFF),
        ],
    );
    let party_c = s.add_class(
        maine,
        "PartyComponent",
        Some(object_c),
        0x40,
        &[int("ScienceFound", SCIENCE_OFF)],
    );
    let mode_manager_c = s.add_class(
        maine,
        "SurvivalModeManagerComponent",
        Some(object_c),
        0x48,
        &[object("GameModeSettings", SETTINGS_OFF)],
    );
    let settings_c = s.add_class(
        maine,
        "SurvivalGameModeSettings",
        Some(object_c),
        0x48,
        &[
            boolean("bRecipesRequireIngredients", RECIPES_OFF, 0xff),
            boolean("bAutoCompleteBuildings", AUTO_COMPLETE_OFF, 0xff),
            float("EnemyDamageMultiplier", ENEMY_DAMAGE_OFF),
        ],
    );
    // Subclasse de Blueprint, como no jogo: os campos vêm da classe pai.
    let normal_settings_c = s.add_class(
        maine,
        "BP_NormalModeSettings_C",
        Some(settings_c),
        0x48,
        &[],
    );
    s.link_struct(game_state_c, "SurvivalModeManagerComponent", mode_manager_c);
    s.link_struct(mode_manager_c, "GameModeSettings", settings_c);

    // O controller e o pawn compartilham o `Outer` (o nível persistente), como no jogo.
    let level = s.add_object(level_c, "PersistentLevel", maine);
    let world = s.add_object(world_c, "World_0", maine);
    let game_state = s.add_object(game_state_c, "GameState_0", level);
    let party = s.add_object(party_c, "Party", game_state);
    s.set(level, WORLD_OFF, &world.to_le_bytes());
    s.set(world, GAME_STATE_OFF, &game_state.to_le_bytes());
    s.set(game_state, PARTY_OFF, &party.to_le_bytes());
    s.set(party, SCIENCE_OFF, &SCIENCE_START.to_le_bytes());
    let mode_manager = s.add_object(mode_manager_c, "ModeManager", game_state);
    let settings = s.add_object(normal_settings_c, "NormalModeSettings", world);
    let cdo_settings = s.add_object(normal_settings_c, "Default__BP_NormalModeSettings_C", maine);
    s.set(game_state, MODE_MANAGER_OFF, &mode_manager.to_le_bytes());
    s.set(mode_manager, SETTINGS_OFF, &settings.to_le_bytes());
    let uobject = UeLayout::observed().uobject;
    s.set(cdo_settings, uobject.flags, &uobject.cdo_flag.to_le_bytes());
    for obj in [settings, cdo_settings] {
        s.set(obj, RECIPES_OFF, &[1]);
        s.set(obj, AUTO_COMPLETE_OFF, &[0]);
        s.set(obj, ENEMY_DAMAGE_OFF, &ENEMY_DAMAGE_START.to_le_bytes());
    }
    let controller = s.add_object(survival_controller_c, "Controller_0", level);
    let pawn = s.add_object(player_character_c, "Player_0", level);
    let creature = s.add_object(character_c, "Creature_0", maine);
    let alt_pawn = s.add_object(player_character_c, "Player_1", maine);
    // Componentes de um personagem, com os valores de um personagem saudável e os bits ligados
    // como no jogo. Devolve o `HealthComponent`, a carga e o movimento.
    let mut components = |owner: u64| {
        let health = s.add_object(health_c, "Health", owner);
        let stamina = s.add_object(stamina_c, "Stamina", owner);
        let survival = s.add_object(survival_c, "Survival", owner);
        let temperature = s.add_object(temp_c, "Temperature", owner);
        for (obj, off) in [
            (health, HEALTH_OFF),
            (stamina, STAMINA_OFF),
            (survival, SURVIVAL_OFF),
            (temperature, TEMP_OFF),
        ] {
            s.set(owner, off, &obj.to_le_bytes());
        }
        s.set(health, DIE_BYTE, &[0x03]);
        s.set(owner, FALL_BYTE, &[FALL_MASK]);
        s.set(stamina, CUR_STAMINA, &100f32.to_le_bytes());
        s.set(stamina, MAX_STAMINA, &100f32.to_le_bytes());
        for off in [0x30, 0x38, 0x40].into_iter().chain(TRAIT_SETTINGS_OFF) {
            s.set(survival, off, &100f32.to_le_bytes());
        }
        s.set(temperature, CUR_TEMP, &NEUTRAL_TEMP.to_le_bytes());
        let hauling = s.add_object(hauling_c, "Hauling", owner);
        let movement = s.add_object(movement_c, "Movement", owner);
        s.set(owner, HAULING_OFF, &hauling.to_le_bytes());
        s.set(owner, MOVEMENT_OFF, &movement.to_le_bytes());
        s.set(hauling, CAPACITY_OFF, &CAPACITY_START.to_le_bytes());
        for off in [GROUND_MULT_OFF, SWIM_MULT_OFF] {
            s.set(movement, off, &SPEED_START.to_le_bytes());
        }
        s.set(movement, GRAVITY_OFF, &GRAVITY_START.to_le_bytes());
        s.set(owner, JUMP_OFF, &JUMP_START.to_le_bytes());
        s.set(owner, DAMAGE_MULT_OFF, &DAMAGE_MULT_START.to_le_bytes());
        (health, hauling, movement)
    };
    let (health, hauling, movement) = components(pawn);
    let (_, creature_hauling, creature_movement) = components(creature);
    components(alt_pawn);
    s.set(controller, PAWN_OFF, &pawn.to_le_bytes());
    s.set(controller, CACHED_OFF, &pawn.to_le_bytes());

    let image = s.finish();
    let addrs = UeAddrs {
        gobjects: image.gobjects,
        names: image.names,
        controller,
        pawn,
        creature,
        pawn_field_addr: controller + u64::from(PAWN_OFF),
        health,
        alt_pawn,
        hauling,
        movement,
        creature_hauling,
        creature_movement,
        level,
        world,
        game_state,
        party,
        mode_manager,
        settings,
        cdo_settings,
    };
    (image, addrs)
}

fn lock(
    id: &str,
    object: &str,
    field: &str,
    mode: LockMode,
    target: LockTarget,
    (min, max): (f32, f32),
) -> FieldDef {
    FieldDef {
        id: id.into(),
        name: id.into(),
        category: Category::Sobrevivencia,
        description: String::new(),
        object: object.into(),
        object_class: format!("{MAINE}.{object}"),
        root: FieldRoot::Pawn,
        value: None,
        rule: FieldRule::Lock {
            field: field.into(),
            mode,
            target,
            min,
            max,
        },
    }
}

fn clear_bits(id: &str, object: &str, class: &str, fields: &[&str]) -> FieldDef {
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
            fields: fields.iter().map(|f| f.to_string()).collect(),
        },
    }
}

/// Cheat `set` ou `add` da spec 006 com a grade do `generic.toml`.
fn progression(
    id: &str,
    category: Category,
    (object, class, root): (&str, &str, FieldRoot),
    rule: FieldRule,
    value: Option<ValueGrid>,
) -> FieldDef {
    FieldDef {
        id: id.into(),
        name: id.into(),
        category,
        description: String::new(),
        object: object.into(),
        object_class: class.into(),
        root,
        value,
        rule,
    }
}

fn grid(
    ty: ValueType,
    (min, max, step): (f64, f64, f64),
    default: f64,
    unit: Option<&str>,
) -> ValueGrid {
    ValueGrid {
        ty,
        min,
        max,
        step,
        default_k: ((default - min) / step).round() as u64,
        unit: unit.map(str::to_string),
        decimals: match ty {
            ValueType::F32 => 2,
            _ => 0,
        },
        points: None,
    }
}

fn set_rule(fields: &[&str], kind: FieldKind, target: Option<f64>) -> FieldRule {
    FieldRule::Set {
        fields: fields.iter().map(|f| f.to_string()).collect(),
        kind,
        target,
        bools: None,
    }
}

/// Os dois cheats do mundo da spec 007, com os ids e as grades do `generic.toml`.
fn world_defs() -> Vec<FieldDef> {
    let settings = (
        "SurvivalModeManagerComponent.GameModeSettings",
        format!("{MAINE}.SurvivalGameModeSettings"),
        FieldRoot::GameState,
    );
    let world = |id: &str, category, rule, value| {
        progression(
            id,
            category,
            (settings.0, &settings.1, settings.2),
            rule,
            value,
        )
    };
    vec![
        world(
            "construir-sem-ingredientes",
            Category::Itens,
            FieldRule::Set {
                fields: vec![
                    "bRecipesRequireIngredients".into(),
                    "bAutoCompleteBuildings".into(),
                ],
                kind: FieldKind::Bool,
                target: None,
                bools: Some(vec![false, true]),
            },
            None,
        ),
        world(
            "dano-recebido",
            Category::Sobrevivencia,
            set_rule(&["EnemyDamageMultiplier"], FieldKind::Float, None),
            Some(grid(ValueType::F32, (0.0, 1.0, 0.05), 0.25, Some("x"))),
        ),
    ]
}

/// Os seis cheats da spec 006, 6.7, com os nomes do jogo e as grades do `generic.toml`.
fn progression_defs() -> Vec<FieldDef> {
    let maine = |class: &str| format!("{MAINE}.{class}");
    let movement = || ("CharacterMovement", maine("MaineCharMovementComponent"));
    let (mv_obj, mv_class) = movement();
    let carga = progression(
        "carga",
        Category::Itens,
        (
            "HaulingComponent",
            &maine("HaulingComponent"),
            FieldRoot::Pawn,
        ),
        set_rule(&["Capacity"], FieldKind::Int, None),
        Some(grid(ValueType::I32, (1.0, 100.0, 1.0), 20.0, None)),
    );
    vec![
        carga,
        progression(
            "dano",
            Category::Progressao,
            ("pawn", &maine("SurvivalCharacter"), FieldRoot::Pawn),
            set_rule(&["CustomDamageMultiplier"], FieldKind::Float, None),
            Some(ValueGrid {
                decimals: 0,
                ..grid(ValueType::F32, (1.0, 100.0, 1.0), 10.0, Some("x"))
            }),
        ),
        progression(
            "somar-ciencia",
            Category::Progressao,
            (
                "PartyComponent",
                &maine("PartyComponent"),
                FieldRoot::GameState,
            ),
            FieldRule::Add {
                field: "ScienceFound".into(),
                result_max: 100_000,
            },
            Some(grid(ValueType::I32, (100.0, 10_000.0, 100.0), 1000.0, None)),
        ),
        progression(
            "velocidade",
            Category::Movimento,
            (mv_obj, &mv_class, FieldRoot::Pawn),
            set_rule(
                &["CustomGroundSpeedMultiplier", "CustomSwimSpeedMultiplier"],
                FieldKind::Float,
                None,
            ),
            Some(grid(ValueType::F32, (0.5, 4.0, 0.25), 2.0, Some("x"))),
        ),
        progression(
            "pulos-infinitos",
            Category::Movimento,
            ("pawn", "/Script/Engine.Character", FieldRoot::Pawn),
            set_rule(&["JumpMaxCount"], FieldKind::Int, Some(1000.0)),
            None,
        ),
        progression(
            "gravidade",
            Category::Movimento,
            (mv_obj, &mv_class, FieldRoot::Pawn),
            set_rule(&["GravityScale"], FieldKind::Float, None),
            Some(ValueGrid {
                decimals: 1,
                ..grid(ValueType::F32, (0.1, 2.0, 0.1), 0.5, Some("x"))
            }),
        ),
    ]
}

/// `[ue]` e `[[field]]` da cena: o `[ue.player]`, os oito cheats da spec 5 (6.5) e os seis da
/// spec 6 (6.7) e os dois da spec 7, com os nomes do jogo, e o layout observado. As globais não têm `rva` (a cena entrega os endereços direto).
pub fn synth_signatures() -> (UeSigs, Vec<FieldDef>) {
    let global = || GlobalDef {
        pattern: GlobalPattern {
            pattern: Pattern::parse("48 8b 05 ?? ?? ?? ??").expect("padrão fixo"),
            disp_offset: 3,
            insn_end: 7,
            adjust: 0,
        },
        rva: None,
    };
    let sigs = UeSigs {
        layout: UeLayout::observed(),
        gobjects: global(),
        names: global(),
        player: PlayerCfg {
            controller_class: format!("{MAINE}.SurvivalPlayerController"),
            pawn_field: "Pawn".into(),
            pawn_class: format!("{MAINE}.SurvivalPlayerCharacter"),
            alt_pawn_fields: vec!["CachedSurvivalPlayerCharacter".into()],
            game_state: Some(GameStateCfg {
                level_class: "/Script/Engine.Level".into(),
                world_field: "OwningWorld".into(),
                world_class: "/Script/Engine.World".into(),
                game_state_field: "GameState".into(),
                game_state_class: format!("{MAINE}.InGameGameState"),
            }),
            research: None,
        },
    };
    let bar = (0.0, 10_000.0);
    let max = |f: &str| LockTarget::Field(f.into());
    let mut defs = vec![
        lock(
            "vida-infinita",
            "HealthComponent",
            "CurrentDamage",
            LockMode::Lower,
            LockTarget::Const(0.0),
            bar,
        ),
        lock(
            "stamina-infinita",
            "StaminaComponent",
            "CurrentStamina",
            LockMode::Raise,
            max("MaxStamina"),
            bar,
        ),
        lock(
            "comida-infinita",
            "SurvivalComponent",
            "CurrentFood",
            LockMode::Raise,
            max("HungerSettings.MaxValue"),
            bar,
        ),
        lock(
            "agua-infinita",
            "SurvivalComponent",
            "CurrentWater",
            LockMode::Raise,
            max("ThirstSettings.MaxValue"),
            bar,
        ),
        lock(
            "oxigenio-infinito",
            "SurvivalComponent",
            "CurrentBreath",
            LockMode::Raise,
            max("BreathSettings.MaxValue"),
            bar,
        ),
        lock(
            "temperatura-estavel",
            "BodyTemperatureComponent",
            "CurrentBodyTemperature",
            LockMode::Exact,
            LockTarget::Const(NEUTRAL_TEMP),
            (-100.0, 200.0),
        ),
        clear_bits(
            "sem-dano-queda",
            "pawn",
            "SurvivalCharacter",
            &["bTakeFallDamage"],
        ),
        clear_bits(
            "nao-morrer",
            "HealthComponent",
            "HealthComponent",
            &["bCanDieFromDamage", "bCanDieEver"],
        ),
    ];
    defs.extend(progression_defs());
    defs.extend(world_defs());
    (sigs, defs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gt_core::mem::ReadMem;

    fn ptr(mem: &impl ReadMem, addr: u64) -> u64 {
        let mut b = [0u8; 8];
        mem.read_exact(addr, &mut b).unwrap();
        u64::from_le_bytes(b)
    }

    fn bytes<const N: usize>(mem: &impl ReadMem, addr: u64) -> [u8; N] {
        let mut b = [0u8; N];
        mem.read_exact(addr, &mut b).unwrap();
        b
    }

    #[test]
    fn scene_has_mode_settings() {
        let (image, a) = build(0x1000_0000, ARENA_LEN);
        let mem = image.into_mem();
        let layout = UeLayout::observed();

        assert_eq!(
            ptr(&mem, a.game_state + u64::from(MODE_MANAGER_OFF)),
            a.mode_manager
        );
        assert_eq!(
            ptr(&mem, a.mode_manager + u64::from(SETTINGS_OFF)),
            a.settings
        );
        assert_eq!(bytes::<1>(&mem, a.settings + u64::from(RECIPES_OFF)), [1]);
        assert_eq!(
            bytes::<1>(&mem, a.settings + u64::from(AUTO_COMPLETE_OFF)),
            [0]
        );
        assert_eq!(
            bytes::<4>(&mem, a.settings + u64::from(ENEMY_DAMAGE_OFF)),
            ENEMY_DAMAGE_START.to_le_bytes()
        );

        let flags =
            |obj: u64| u32::from_le_bytes(bytes(&mem, obj + u64::from(layout.uobject.flags)));
        assert_ne!(flags(a.cdo_settings) & layout.uobject.cdo_flag, 0);
        assert_eq!(flags(a.settings) & layout.uobject.cdo_flag, 0);
        assert_ne!(a.settings, a.cdo_settings);
    }

    #[test]
    fn synth_signatures_include_world_cheats() {
        let (_, defs) = synth_signatures();
        let ids: Vec<&str> = defs.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(
            &ids[ids.len() - 2..],
            ["construir-sem-ingredientes", "dano-recebido"]
        );
        let build_def = &defs[defs.len() - 2];
        assert_eq!(build_def.root, FieldRoot::GameState);
        assert!(matches!(
            &build_def.rule,
            FieldRule::Set { kind: FieldKind::Bool, bools: Some(b), .. } if b == &[false, true]
        ));
    }
}
