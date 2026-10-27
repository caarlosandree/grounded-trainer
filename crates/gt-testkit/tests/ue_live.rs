#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Leitor de reflexão e caminho até o personagem contra o `gt-testtarget --ue` (spec 005, 8):
//! leituras de verdade por `process_vm_readv`, com a cena sintética na memória do alvo.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use gt_core::mem::{MemReader, MemWriter, ReadMem};
use gt_grounded2::player::{
    Absent, ControllerFind, FieldCatalog, find_controller, plan_tick, read_player,
};
use gt_testkit::{TargetOpts, TestTarget, UeAddrs, synth_signatures};
use gt_ue::{Names, ObjectArray, ReflectionIndex, ScanBudget, ScanEnd, UeLayout, cheap_check};

fn spawn() -> (TestTarget, UeAddrs) {
    let t = TestTarget::spawn_with(TargetOpts {
        ue: true,
        ..TargetOpts::default()
    });
    let ue = t.ue.expect("o alvo com --ue informa os endereços");
    (t, ue)
}

static PAUSES: AtomicUsize = AtomicUsize::new(0);

fn counted_sleep(_: Duration) {
    PAUSES.fetch_add(1, Ordering::Relaxed);
}

fn budget() -> ScanBudget {
    ScanBudget {
        pause: Duration::ZERO,
        ..ScanBudget::standard()
    }
}

/// Tabela de objetos e índice da reflexão do alvo.
fn indexed(reader: &MemReader, ue: &UeAddrs) -> (ObjectArray, ReflectionIndex) {
    let layout = UeLayout::observed();
    let names = Names::new(reader, ue.names).unwrap();
    let objects = ObjectArray::read(reader, &layout, ue.gobjects).unwrap();
    let index = ReflectionIndex::build(reader, &layout, &objects, &names, &budget()).unwrap();
    (objects, index)
}

#[test]
fn live_cheap_check_and_index() {
    let (t, ue) = spawn();
    let reader = MemReader::new(t.pid);
    let layout = UeLayout::observed();
    assert_eq!(cheap_check(&reader, &layout, ue.gobjects, ue.names), Ok(()));
    let (_, index) = indexed(&reader, &ue);
    assert_eq!(index.deep_check(), Ok(()));
    let class = index
        .find_class("/Script/Maine", "SurvivalPlayerController")
        .unwrap();
    let pawn = index.find_field(&reader, class, "Pawn").unwrap();
    assert_eq!(pawn.offset, gt_testkit::ue::PAWN_OFF);
}

#[test]
fn live_find_controller_and_read_player() {
    let (t, ue) = spawn();
    let reader = MemReader::new(t.pid);
    let (objects, index) = indexed(&reader, &ue);
    let (sigs, defs) = synth_signatures();
    let catalog = FieldCatalog::resolve(&index, &reader, &sigs.player, &defs);
    for def in &defs {
        assert_eq!(catalog.avail(&def.id), Ok(()), "{}", def.id);
    }
    assert_eq!(
        find_controller(&index, &reader, &objects, &catalog, &budget()).unwrap(),
        ControllerFind::Found(ue.controller)
    );
    let snap = read_player(&reader, &index, &catalog, &objects, ue.controller).unwrap();
    assert_eq!(snap.pawn, ue.pawn);
    // A soma (`add`) só é lida sob demanda, fora do ciclo; os cheats do mundo (raiz `game_state`)
    // não entram no retrato do personagem.
    let cycle = defs
        .iter()
        .filter(|d| !matches!(d.rule, gt_grounded2::signatures::FieldRule::Add { .. }))
        .filter(|d| d.root == gt_grounded2::signatures::FieldRoot::Pawn)
        .count();
    assert_eq!(snap.values.len(), cycle);
    // Tudo cheio na cena; só os bits ligados pedem escrita.
    let on: Vec<String> = defs.iter().map(|d| d.id.clone()).collect();
    let actions = plan_tick(&snap, &catalog, &on);
    assert_eq!(actions.len(), 2, "{actions:?}");
}

/// Leitor que conta as chamadas em lote.
struct Counting {
    inner: MemReader,
    batches: AtomicUsize,
}

impl ReadMem for Counting {
    fn read_exact(&self, addr: u64, buf: &mut [u8]) -> gt_core::Result<()> {
        self.inner.read_exact(addr, buf)
    }

    fn read_many(&self, reqs: &[(u64, usize)]) -> gt_core::Result<Vec<Option<Vec<u8>>>> {
        self.batches.fetch_add(1, Ordering::Relaxed);
        self.inner.read_many(reqs)
    }
}

#[test]
fn live_scan_batches_with_budget() {
    let (t, ue) = spawn();
    let mem = Counting {
        inner: MemReader::new(t.pid),
        batches: AtomicUsize::new(0),
    };
    let layout = UeLayout::observed();
    let objects = ObjectArray::read(&mem, &layout, ue.gobjects).unwrap();
    let small = ScanBudget {
        block_bytes: 16 * layout.objects.item_size as usize,
        batch: 8,
        pause: Duration::from_micros(1),
        deadline: Duration::from_secs(60),
        sleep: counted_sleep,
    };
    let mut visited = Vec::new();
    let end = objects
        .scan(&mem, &layout, &small, |h| visited.push(h.addr))
        .unwrap();
    assert_eq!(end, ScanEnd::Done);
    assert_eq!(visited.len(), objects.num_elements as usize);
    assert!(visited.contains(&ue.controller) && visited.contains(&ue.pawn));
    // Lotes de 8 cabeçalhos: várias chamadas em lote, com pausa entre elas.
    let batches = mem.batches.load(Ordering::Relaxed);
    assert!(batches >= visited.len() / 8, "{batches} lotes");
    assert!(PAUSES.load(Ordering::Relaxed) >= batches);
}

#[test]
fn live_pawn_swap_and_null() {
    let (t, ue) = spawn();
    let reader = MemReader::new(t.pid);
    let writer = MemWriter::open(t.pid).unwrap();
    let (objects, index) = indexed(&reader, &ue);
    let (sigs, defs) = synth_signatures();
    let catalog = FieldCatalog::resolve(&index, &reader, &sigs.player, &defs);
    let read = || read_player(&reader, &index, &catalog, &objects, ue.controller);

    // Outro personagem (morte ou recarga): a leitura acompanha o `Pawn`.
    writer
        .write(ue.pawn_field_addr, &ue.alt_pawn.to_le_bytes())
        .unwrap();
    assert_eq!(read().unwrap().pawn, ue.alt_pawn);
    // Montaria no `Pawn`: o campo alternativo ainda leva ao personagem; a criatura nunca é alvo.
    writer
        .write(ue.pawn_field_addr, &ue.creature.to_le_bytes())
        .unwrap();
    let snap = read().unwrap();
    assert_eq!(snap.pawn, ue.pawn);
    assert!(snap.objects.iter().all(|o| o.addr != ue.creature));
    // `Pawn` nulo (menu, carregamento).
    writer
        .write(ue.pawn_field_addr, &0u64.to_le_bytes())
        .unwrap();
    assert_eq!(read(), Err(Absent::PawnNull));
}

/// Lê um ponteiro de 8 bytes do alvo.
fn ptr(reader: &MemReader, addr: u64) -> u64 {
    let mut b = [0u8; 8];
    reader.read_exact(addr, &mut b).unwrap();
    u64::from_le_bytes(b)
}

fn read4(reader: &MemReader, addr: u64) -> [u8; 4] {
    let mut b = [0u8; 4];
    reader.read_exact(addr, &mut b).unwrap();
    b
}

#[test]
fn synthetic_scene_has_game_state_path() {
    use gt_testkit::ue as scene;

    let (t, ue) = spawn();
    let reader = MemReader::new(t.pid);
    let (_, index) = indexed(&reader, &ue);
    let layout = UeLayout::observed();
    let outer = u64::from(layout.uobject.outer);

    // O caminho `Outer → Level → World → GameState → PartyComponent`, pelos nomes da reflexão.
    assert_eq!(ptr(&reader, ue.controller + outer), ue.level);
    assert_eq!(ptr(&reader, ue.pawn + outer), ue.level);
    let level_c = index.find_class("/Script/Engine", "Level").unwrap();
    let world_f = index.find_field(&reader, level_c, "OwningWorld").unwrap();
    assert_eq!(ptr(&reader, ue.level + u64::from(world_f.offset)), ue.world);
    let world_c = index.find_class("/Script/Engine", "World").unwrap();
    let state_f = index.find_field(&reader, world_c, "GameState").unwrap();
    assert_eq!(
        ptr(&reader, ue.world + u64::from(state_f.offset)),
        ue.game_state
    );
    let state_c = index.find_class(scene::MAINE, "InGameGameState").unwrap();
    let party_f = index
        .find_field(&reader, state_c, "PartyComponent")
        .unwrap();
    assert_eq!(
        ptr(&reader, ue.game_state + u64::from(party_f.offset)),
        ue.party
    );
    let party_c = index.find_class(scene::MAINE, "PartyComponent").unwrap();
    let science = index.find_field(&reader, party_c, "ScienceFound").unwrap();
    assert_eq!(science.offset, scene::SCIENCE_OFF);
    assert_eq!(
        read4(&reader, ue.party + u64::from(science.offset)),
        scene::SCIENCE_START.to_le_bytes()
    );

    // Carga, movimento, pulos e dano, achados pelo nome e lidos pelo índice.
    let survival = index
        .find_class(scene::MAINE, "SurvivalPlayerCharacter")
        .unwrap();
    let hauling_f = index
        .find_field(&reader, survival, "HaulingComponent")
        .unwrap();
    assert_eq!(hauling_f.offset, scene::HAULING_OFF);
    assert_eq!(
        ptr(&reader, ue.pawn + u64::from(hauling_f.offset)),
        ue.hauling
    );
    let move_f = index
        .find_field(&reader, survival, "CharacterMovement")
        .unwrap();
    assert_eq!(move_f.offset, scene::MOVEMENT_OFF);
    assert_eq!(
        ptr(&reader, ue.pawn + u64::from(move_f.offset)),
        ue.movement
    );
    let jump_f = index.find_field(&reader, survival, "JumpMaxCount").unwrap();
    assert_eq!(
        read4(&reader, ue.pawn + u64::from(jump_f.offset)),
        scene::JUMP_START.to_le_bytes()
    );
    let damage_f = index
        .find_field(&reader, survival, "CustomDamageMultiplier")
        .unwrap();
    assert_eq!(
        read4(&reader, ue.pawn + u64::from(damage_f.offset)),
        scene::DAMAGE_MULT_START.to_le_bytes()
    );
    let hauling_c = index.find_class(scene::MAINE, "HaulingComponent").unwrap();
    let capacity = index.find_field(&reader, hauling_c, "Capacity").unwrap();
    assert_eq!(
        read4(&reader, ue.hauling + u64::from(capacity.offset)),
        scene::CAPACITY_START.to_le_bytes()
    );
    let move_c = index
        .find_class(scene::MAINE, "MaineCharMovementComponent")
        .unwrap();
    for (name, start) in [
        ("CustomGroundSpeedMultiplier", scene::SPEED_START),
        ("CustomSwimSpeedMultiplier", scene::SPEED_START),
        ("GravityScale", scene::GRAVITY_START),
    ] {
        let f = index.find_field(&reader, move_c, name).unwrap();
        assert_eq!(
            read4(&reader, ue.movement + u64::from(f.offset)),
            start.to_le_bytes(),
            "{name}"
        );
    }

    // A montaria tem os próprios componentes, distintos dos do personagem.
    assert_eq!(
        ptr(&reader, ue.creature + u64::from(hauling_f.offset)),
        ue.creature_hauling
    );
    assert_eq!(
        ptr(&reader, ue.creature + u64::from(move_f.offset)),
        ue.creature_movement
    );
    assert_ne!(ue.creature_hauling, ue.hauling);
    assert_ne!(ue.creature_movement, ue.movement);

    // As assinaturas sintéticas resolvem os seis cheats novos e o caminho do `GameState`.
    let (sigs, defs) = synth_signatures();
    assert!(sigs.player.game_state.is_some());
    let catalog = FieldCatalog::resolve(&index, &reader, &sigs.player, &defs);
    for id in [
        "carga",
        "dano",
        "somar-ciencia",
        "velocidade",
        "pulos-infinitos",
        "gravidade",
    ] {
        assert!(defs.iter().any(|d| d.id == id), "{id} ausente");
        assert_eq!(catalog.avail(id), Ok(()), "{id}");
    }
}
