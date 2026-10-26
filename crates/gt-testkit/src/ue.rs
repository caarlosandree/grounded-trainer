//! Cena de reflexão sintética do `gt-testtarget --ue` (spec 005, 8): um controller com o pawn
//! `SurvivalPlayerCharacter` e os quatro componentes, uma criatura `SurvivalCharacter` com
//! componentes próprios e um segundo personagem não referenciado, para simular a troca de pawn.
//!
//! Os nomes de classes e campos são os do jogo; os offsets são outros, de propósito: o caminho
//! tem de vir da reflexão, por nome. O layout das estruturas é o [`UeLayout::observed`].

use gt_core::global::GlobalPattern;
use gt_core::scan::Pattern;
use gt_grounded2::signatures::{
    Category, FieldDef, FieldRoot, FieldRule, GlobalDef, LockMode, LockTarget, PlayerCfg, UeSigs,
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
const SETTINGS_OFF: [u32; 3] = [0x50, 0x60, 0x70];
/// `SurvivalPlayerController.CachedSurvivalPlayerCharacter`.
pub const CACHED_OFF: u32 = 0x78;
const CUR_TEMP: u32 = 0x30;

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
    let character_c = s.add_class(
        maine,
        "SurvivalCharacter",
        Some(actor),
        0x70,
        &[
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
        0x80,
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
    let [hunger, thirst, breath] = SETTINGS_OFF;
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

    let controller = s.add_object(survival_controller_c, "Controller_0", maine);
    let pawn = s.add_object(player_character_c, "Player_0", maine);
    let creature = s.add_object(character_c, "Creature_0", maine);
    let alt_pawn = s.add_object(player_character_c, "Player_1", maine);
    // Componentes de um personagem, com os valores de um personagem saudável e os bits ligados
    // como no jogo. Devolve o `HealthComponent`.
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
        for off in [0x30, 0x38, 0x40].into_iter().chain(SETTINGS_OFF) {
            s.set(survival, off, &100f32.to_le_bytes());
        }
        s.set(temperature, CUR_TEMP, &NEUTRAL_TEMP.to_le_bytes());
        health
    };
    let health = components(pawn);
    components(creature);
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

/// `[ue]` e `[[field]]` da cena: o `[ue.player]` e os oito cheats da spec 6.5, com os nomes do
/// jogo, e o layout observado. As globais não têm `rva` (a cena entrega os endereços direto).
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
            game_state: None,
        },
    };
    let bar = (0.0, 10_000.0);
    let max = |f: &str| LockTarget::Field(f.into());
    let defs = vec![
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
    (sigs, defs)
}
