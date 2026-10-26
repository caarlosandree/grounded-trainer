#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Execução dos cheats de valor contra o `gt-testtarget --ue` (spec 005, 6.4 a 6.6): o motor de
//! verdade, com o diário num `TempDir` e a política de dados do jogo (regiões `rw`).

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gt_core::Error;
use gt_core::engine::{DataFault, Engine, EngineOptions, PolicyFn};
use gt_core::journal::{DataEntry, JournalLock, JournalStore, Loaded};
use gt_core::mem::{MemReader, MemWriter};
use gt_core::patch::WritePolicy;
use gt_core::suspend::PauseConfig;
use gt_grounded2::fields::{ActionOutcome, FieldRuntime, Note, PlayerState};
use gt_grounded2::player::{FieldCatalog, FieldValues, read_player};
use gt_grounded2::policy::data_policy;
use gt_testkit::ue::{
    CAPACITY_OFF, DAMAGE_MULT_OFF, DAMAGE_OFF, DIE_BYTE, GRAVITY_OFF, GROUND_MULT_OFF, HAULING_OFF,
    HEALTH_OFF, JUMP_OFF, PARTY_OFF, SCIENCE_OFF, SWIM_MULT_OFF, WORLD_OFF,
};
use gt_testkit::{TargetOpts, TestTarget, UeAddrs, synth_signatures};
use gt_ue::{Names, ObjectArray, ReflectionIndex, ScanBudget, UeLayout};

/// Alvo, leitor, índice, motor e o runtime de uma cena.
struct Scene {
    t: TestTarget,
    ue: UeAddrs,
    reader: MemReader,
    index: ReflectionIndex,
    objects: ObjectArray,
    runtime: FieldRuntime,
    engine: Engine,
    store: JournalStore,
    /// Relógio injetado nos ciclos.
    now: Instant,
    /// Quantas vezes a política de dados foi recalculada (o `maps` foi relido).
    policy_calls: Rc<Cell<u32>>,
    /// Intervalo `[início, fim)` onde a política de dados recusa escrever (`(0, 0)`: nenhum).
    forbidden: Rc<Cell<(u64, u64)>>,
    _lock: JournalLock,
    _dir: tempfile::TempDir,
}

fn scene() -> Scene {
    let t = TestTarget::spawn_with(TargetOpts {
        ue: true,
        ..TargetOpts::default()
    });
    let ue = t.ue.unwrap();
    let reader = MemReader::new(t.pid);
    let layout = UeLayout::observed();
    let names = Names::new(&reader, ue.names).unwrap();
    let objects = ObjectArray::read(&reader, &layout, ue.gobjects).unwrap();
    let index = ReflectionIndex::build(&reader, &layout, &objects, &names, &ScanBudget::standard())
        .unwrap();
    let (sigs, defs) = synth_signatures();
    let catalog = FieldCatalog::resolve(&index, &reader, &sigs.player, &defs);
    let dir = tempfile::tempdir().unwrap();
    let store = JournalStore::new(dir.path().join("state"));
    let lock = store.lock().unwrap();
    let policy: PolicyFn = Box::new(|_maps| WritePolicy::new(Vec::new()));
    let opts = EngineOptions {
        version_key: "teste".into(),
        pause: PauseConfig::default(),
    };
    let mut engine = Engine::open(t.pid, None, store.clone(), &lock, policy, opts).unwrap();
    let policy_calls = Rc::new(Cell::new(0u32));
    let forbidden = Rc::new(Cell::new((0u64, 0u64)));
    let (calls, hole) = (Rc::clone(&policy_calls), Rc::clone(&forbidden));
    engine.set_data_policy(Box::new(move |maps| {
        calls.set(calls.get() + 1);
        let (start, end) = hole.get();
        let images: Vec<_> = (start < end).then_some(start..end).into_iter().collect();
        data_policy(&images, maps)
    }));
    Scene {
        t,
        ue,
        reader,
        index,
        objects,
        runtime: FieldRuntime::new(catalog),
        engine,
        store,
        now: Instant::now(),
        policy_calls,
        forbidden,
        _lock: lock,
        _dir: dir,
    }
}

impl Scene {
    fn tick(&mut self, on: &[&str]) -> gt_grounded2::fields::TickReport {
        self.tick_after(on, Duration::from_millis(100))
    }

    /// Um ciclo `after` depois do anterior (relógio injetado).
    fn tick_after(&mut self, on: &[&str], after: Duration) -> gt_grounded2::fields::TickReport {
        self.now += after;
        let on: Vec<String> = on.iter().map(|s| s.to_string()).collect();
        self.runtime.tick(
            &mut self.engine,
            &self.reader,
            &self.index,
            &self.objects,
            self.ue.controller,
            &on,
            self.now,
        )
    }

    fn action(&mut self, controller: Option<u64>, amount: i32) -> ActionOutcome {
        self.runtime.run_action(
            &mut self.engine,
            &self.reader,
            &self.index,
            &self.objects,
            controller,
            "somar-ciencia",
            amount,
        )
    }

    fn i32_at(&self, addr: u64) -> i32 {
        self.f32_at(addr).to_bits() as i32
    }

    fn cap(&self) -> u64 {
        self.ue.hauling + u64::from(CAPACITY_OFF)
    }

    fn science(&self) -> u64 {
        self.ue.party + u64::from(SCIENCE_OFF)
    }

    /// Mexe no campo `Capacity` como o jogo faria.
    fn game_sets_capacity(&self, v: i32) {
        self.poke(self.cap(), &v.to_le_bytes());
    }

    /// Endereço do campo de trava `id` no personagem atual.
    fn lock_addr(&self, id: &str) -> (u64, f32) {
        let snap = read_player(
            &self.reader,
            &self.index,
            self.runtime.catalog(),
            &self.objects,
            self.ue.controller,
        )
        .unwrap();
        match snap.values[id] {
            FieldValues::Lock { addr, target, .. } => (addr, target),
            ref other => panic!("{id} não é trava: {other:?}"),
        }
    }

    fn f32_at(&self, addr: u64) -> f32 {
        let mut b = [0u8; 4];
        self.reader.read_exact(addr, &mut b).unwrap();
        f32::from_le_bytes(b)
    }

    fn u8_at(&self, addr: u64) -> u8 {
        let mut b = [0u8; 1];
        self.reader.read_exact(addr, &mut b).unwrap();
        b[0]
    }

    fn u64_at(&self, addr: u64) -> u64 {
        let mut b = [0u8; 8];
        self.reader.read_exact(addr, &mut b).unwrap();
        u64::from_le_bytes(b)
    }

    fn poke(&self, addr: u64, bytes: &[u8]) {
        MemWriter::open(self.t.pid)
            .unwrap()
            .write(addr, bytes)
            .unwrap();
    }

    /// `HealthComponent` de um personagem.
    fn health_of(&self, pawn: u64) -> u64 {
        self.u64_at(pawn + u64::from(HEALTH_OFF))
    }

    fn swap_pawn(&self, pawn: u64) {
        self.poke(self.ue.pawn_field_addr, &pawn.to_le_bytes());
    }
}

#[test]
fn locks_raise_values_in_target() {
    let mut s = scene();
    let ids = ["stamina-infinita", "comida-infinita", "agua-infinita"];
    let addrs: Vec<(u64, f32)> = ids.iter().map(|id| s.lock_addr(id)).collect();
    for (addr, _) in &addrs {
        s.poke(*addr, &12.5f32.to_le_bytes());
    }
    let report = s.tick(&ids);
    assert_eq!(report.player, PlayerState::Found);
    assert!(report.notes.is_empty(), "{:?}", report.notes);
    for (addr, target) in &addrs {
        assert_eq!(s.f32_at(*addr), *target);
    }
    // Acima do alvo, nunca baixa.
    s.poke(addrs[0].0, &130f32.to_le_bytes());
    s.tick(&ids);
    assert_eq!(s.f32_at(addrs[0].0), 130.0);
    // Barras não vão ao diário.
    assert_eq!(s.store.peek().unwrap(), Loaded::Missing);
}

#[test]
fn life_lower_to_zero() {
    let mut s = scene();
    let (addr, target) = s.lock_addr("vida-infinita");
    assert_eq!(target, 0.0);
    s.poke(addr, &25f32.to_le_bytes());
    s.tick(&["vida-infinita"]);
    assert_eq!(s.f32_at(addr), 0.0);
}

#[test]
fn bits_cleared_and_restored_on_turn_off() {
    let mut s = scene();
    let die = s.health_of(s.ue.pawn) + u64::from(DIE_BYTE);
    assert_eq!(s.u8_at(die), 0x03);
    let report = s.tick(&["nao-morrer", "sem-dano-queda"]);
    assert!(report.notes.is_empty(), "{:?}", report.notes);
    assert_eq!(s.u8_at(die), 0x00);
    match s.store.peek().unwrap() {
        Loaded::Ok(j) => assert_eq!(j.data.len(), 2),
        other => panic!("diário inesperado: {other:?}"),
    }
    // O jogo religa um bit: a manutenção limpa de novo.
    s.poke(die, &[0x02]);
    s.tick(&["nao-morrer", "sem-dano-queda"]);
    assert_eq!(s.u8_at(die), 0x00);
    let r = s.runtime.turn_off(&mut s.engine, "nao-morrer").unwrap();
    assert_eq!(r.restored, vec!["nao-morrer"]);
    assert_eq!(s.u8_at(die), 0x03);
    s.runtime.turn_off(&mut s.engine, "sem-dano-queda").unwrap();
    assert_eq!(s.store.peek().unwrap(), Loaded::Missing);
    // Uma trava desligada só para.
    let r = s.runtime.turn_off(&mut s.engine, "vida-infinita").unwrap();
    assert_eq!(r.restored, vec!["vida-infinita"]);
}

#[test]
fn creature_never_written() {
    let mut s = scene();
    let creature_health = s.health_of(s.ue.creature);
    let pawn_health = s.health_of(s.ue.pawn);
    for h in [creature_health, pawn_health] {
        s.poke(h + u64::from(DAMAGE_OFF), &50f32.to_le_bytes());
    }
    let all = [
        "vida-infinita",
        "stamina-infinita",
        "nao-morrer",
        "sem-dano-queda",
    ];
    for _ in 0..5 {
        s.tick(&all);
    }
    assert_eq!(s.f32_at(pawn_health + u64::from(DAMAGE_OFF)), 0.0);
    assert_eq!(s.f32_at(creature_health + u64::from(DAMAGE_OFF)), 50.0);
    assert_eq!(s.u8_at(creature_health + u64::from(DIE_BYTE)), 0x03);
    // A criatura no `Pawn` (montado): o personagem continua sendo o alvo.
    s.swap_pawn(s.ue.creature);
    s.poke(pawn_health + u64::from(DAMAGE_OFF), &10f32.to_le_bytes());
    s.tick(&all);
    assert_eq!(s.f32_at(pawn_health + u64::from(DAMAGE_OFF)), 0.0);
    assert_eq!(s.f32_at(creature_health + u64::from(DAMAGE_OFF)), 50.0);
}

#[test]
fn pawn_swap_resolves_old_entry_first() {
    let mut s = scene();
    let old_die = s.health_of(s.ue.pawn) + u64::from(DIE_BYTE);
    let new_die = s.health_of(s.ue.alt_pawn) + u64::from(DIE_BYTE);
    s.tick(&["nao-morrer"]);
    assert_eq!(s.u8_at(old_die), 0x00);
    // Outro personagem; o antigo continua vivo e passa na guarda: os bits dele voltam.
    s.swap_pawn(s.ue.alt_pawn);
    let report = s.tick(&["nao-morrer"]);
    assert!(report.notes.is_empty(), "{:?}", report.notes);
    assert_eq!(s.u8_at(old_die), 0x03);
    assert_eq!(s.u8_at(new_die), 0x00);
    let entries: Vec<&DataEntry> = s
        .engine
        .data_entries()
        .iter()
        .filter(|e| e.id == "nao-morrer")
        .collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].addr, new_die);
}

#[test]
fn pawn_destroyed_entry_discarded() {
    let mut s = scene();
    let old_health = s.health_of(s.ue.pawn);
    let old_die = old_health + u64::from(DIE_BYTE);
    let new_die = s.health_of(s.ue.alt_pawn) + u64::from(DIE_BYTE);
    s.tick(&["nao-morrer"]);
    // O componente antigo foi destruído (a guarda não bate mais) e o pawn mudou.
    s.poke(old_health, &[0xEE; 8]);
    s.swap_pawn(s.ue.alt_pawn);
    s.tick(&["nao-morrer"]);
    // A entrada antiga sai sem escrever; o objeto novo ganha a dele.
    assert_eq!(s.u8_at(old_die), 0x00);
    assert_eq!(s.u8_at(new_die), 0x00);
    let addrs: Vec<u64> = s.engine.data_entries().iter().map(|e| e.addr).collect();
    assert_eq!(addrs, vec![new_die]);
}

#[test]
fn invalid_target_note_and_resume() {
    let mut s = scene();
    let (stamina, _) = s.lock_addr("stamina-infinita");
    // `MaxStamina` fica logo depois de `CurrentStamina` na cena.
    let max = stamina + 4;
    s.poke(stamina, &10f32.to_le_bytes());
    s.poke(max, &f32::NAN.to_le_bytes());
    let first = s.tick(&["stamina-infinita"]);
    assert_eq!(first.notes["stamina-infinita"], Note::InvalidTarget);
    assert_eq!(
        first.events,
        vec!["personagem encontrado", "stamina-infinita: alvo inválido"]
    );
    // O evento só sai na transição; a nota continua.
    let second = s.tick(&["stamina-infinita"]);
    assert_eq!(second.notes["stamina-infinita"], Note::InvalidTarget);
    assert!(second.events.is_empty(), "{:?}", second.events);
    assert_eq!(s.f32_at(stamina), 10.0);
    // O alvo volta à faixa: retoma sozinho.
    s.poke(max, &100f32.to_le_bytes());
    let third = s.tick(&["stamina-infinita"]);
    assert!(third.notes.is_empty());
    assert_eq!(s.f32_at(stamina), 100.0);
}

#[test]
fn pending_id_skipped() {
    let mut s = scene();
    let die = s.health_of(s.ue.pawn) + u64::from(DIE_BYTE);
    s.engine.force_data_pending_for_test(DataEntry::bits(
        "nao-morrer",
        die,
        0x03,
        0x00,
        0x03,
        Vec::new(),
    ));
    let report = s.tick(&["nao-morrer"]);
    assert!(!report.notes.contains_key("nao-morrer"));
    assert_eq!(s.u8_at(die), 0x03);
}

#[test]
fn region_not_rw_refused() {
    let mut s = scene();
    let exec_before = s.u8_at(s.t.exec_page + u64::from(DAMAGE_OFF));
    // O componente aponta para código (`exec_page`): nada é escrito.
    s.poke(
        s.ue.pawn + u64::from(HEALTH_OFF),
        &s.t.exec_page.to_le_bytes(),
    );
    let report = s.tick(&["vida-infinita", "nao-morrer"]);
    // Só os cheats do componente esperam; o personagem continua achado (spec 006, 6.3).
    assert_eq!(report.player, PlayerState::Found);
    assert_eq!(report.notes["vida-infinita"], Note::Waiting);
    assert_eq!(report.notes["nao-morrer"], Note::Waiting);
    assert_eq!(report.events, vec!["personagem encontrado".to_string()]);
    assert_eq!(s.u8_at(s.t.exec_page + u64::from(DAMAGE_OFF)), exec_before);
    // E uma escrita direta fora da política é recusada pelo motor.
    let err = s.engine.write_data(s.t.exec_page, &[0], &[]).unwrap_err();
    assert!(matches!(err, Error::DataOutsidePolicy { .. }), "{err:?}");
}

#[test]
fn raise_target_learns_effective_max() {
    let mut s = scene();
    let (stamina, target) = s.lock_addr("stamina-infinita");
    assert_eq!(target, 100.0);
    s.tick(&["stamina-infinita"]);
    // O jogo regenera acima de `MaxStamina` (bônus): a trava aprende esse máximo.
    s.poke(stamina, &120f32.to_le_bytes());
    s.tick(&["stamina-infinita"]);
    s.poke(stamina, &40f32.to_le_bytes());
    s.tick(&["stamina-infinita"]);
    assert_eq!(s.f32_at(stamina), 120.0);
    // Valor absurdo (fora da faixa sã) não ensina.
    s.poke(stamina, &1.0e9f32.to_le_bytes());
    s.tick(&["stamina-infinita"]);
    s.poke(stamina, &40f32.to_le_bytes());
    s.tick(&["stamina-infinita"]);
    assert_eq!(s.f32_at(stamina), 120.0);
    // Desligar esquece o aprendido.
    s.runtime
        .turn_off(&mut s.engine, "stamina-infinita")
        .unwrap();
    s.poke(stamina, &40f32.to_le_bytes());
    s.tick(&["stamina-infinita"]);
    assert_eq!(s.f32_at(stamina), 100.0);
}

const SET_IDS: [&str; 5] = [
    "carga",
    "dano",
    "velocidade",
    "gravidade",
    "pulos-infinitos",
];

fn journal_data_len(s: &Scene) -> usize {
    match s.store.peek().unwrap() {
        Loaded::Ok(j) => j.data.len(),
        Loaded::Missing => 0,
        other => panic!("diário inesperado: {other:?}"),
    }
}

#[test]
fn set_writes_and_turn_off_restores() {
    let mut s = scene();
    let (ground, swim) = (
        s.ue.movement + u64::from(GROUND_MULT_OFF),
        s.ue.movement + u64::from(SWIM_MULT_OFF),
    );
    let gravity = s.ue.movement + u64::from(GRAVITY_OFF);
    let damage = s.ue.pawn + u64::from(DAMAGE_MULT_OFF);
    let jump = s.ue.pawn + u64::from(JUMP_OFF);
    let first = s.tick(&SET_IDS);
    assert_eq!(first.player, PlayerState::Found);
    assert!(first.notes.is_empty(), "{:?}", first.notes);
    // Escrever o valor desejado não gera evento: só o personagem achado.
    assert_eq!(first.events, vec!["personagem encontrado".to_string()]);
    assert_eq!(s.i32_at(s.cap()), 20);
    assert_eq!(s.f32_at(damage), 10.0);
    assert_eq!((s.f32_at(ground), s.f32_at(swim)), (2.0, 2.0));
    assert_eq!(s.f32_at(gravity), 0.5);
    assert_eq!(s.i32_at(jump), 1000);
    assert_eq!(journal_data_len(&s), 6);
    // Os ciclos seguintes não escrevem nem falam nada.
    let again = s.tick(&SET_IDS);
    assert!(again.notes.is_empty() && again.events.is_empty());
    assert_eq!(journal_data_len(&s), 6);
    for id in SET_IDS {
        let r = s.runtime.turn_off(&mut s.engine, id).unwrap();
        assert_eq!(r.restored, vec![id.to_string()]);
    }
    assert_eq!(s.i32_at(s.cap()), 10);
    assert_eq!(s.f32_at(damage), 1.0);
    assert_eq!((s.f32_at(ground), s.f32_at(swim)), (1.0, 1.0));
    assert_eq!(s.f32_at(gravity), 1.0);
    assert_eq!(s.i32_at(jump), 1);
    assert_eq!(s.store.peek().unwrap(), Loaded::Missing);
}

#[test]
fn set_value_change_goes_through_next() {
    let mut s = scene();
    let (ground, swim) = (
        s.ue.movement + u64::from(GROUND_MULT_OFF),
        s.ue.movement + u64::from(SWIM_MULT_OFF),
    );
    s.tick(&["velocidade"]);
    assert_eq!(s.f32_at(ground), 2.0);
    s.runtime.set_desired("velocidade", 3.0);
    let report = s.tick(&["velocidade"]);
    assert!(report.notes.is_empty() && report.events.is_empty());
    assert_eq!((s.f32_at(ground), s.f32_at(swim)), (3.0, 3.0));
    match s.store.peek().unwrap() {
        Loaded::Ok(j) => {
            assert_eq!(j.data.len(), 2);
            for e in &j.data {
                assert_eq!(e.written, 3.0f32.to_le_bytes().to_vec());
                assert_eq!(e.original, 1.0f32.to_le_bytes().to_vec());
                assert_eq!(e.next, None);
            }
        }
        other => panic!("diário inesperado: {other:?}"),
    }
    s.runtime.turn_off(&mut s.engine, "velocidade").unwrap();
    assert_eq!((s.f32_at(ground), s.f32_at(swim)), (1.0, 1.0));
    // Com `target`, o valor escrito é sempre o `target`: `set_desired` não o muda (spec 6.4).
    s.runtime.set_desired("pulos-infinitos", 7.0);
    s.tick(&["pulos-infinitos"]);
    assert_eq!(s.i32_at(s.ue.pawn + u64::from(JUMP_OFF)), 1000);
}

#[test]
fn set_game_change_becomes_new_original() {
    let mut s = scene();
    s.tick(&["carga"]);
    s.game_sets_capacity(15);
    let report = s.tick(&["carga"]);
    assert_eq!(
        report.events,
        vec!["carga: o jogo mudou o valor para 15; novo original".to_string()]
    );
    assert!(report.notes.is_empty());
    assert_eq!(s.i32_at(s.cap()), 20);
    // Sem mudança nova, nenhuma linha nova.
    assert!(s.tick(&["carga"]).events.is_empty());
    s.runtime.turn_off(&mut s.engine, "carga").unwrap();
    assert_eq!(s.i32_at(s.cap()), 15);
    assert_eq!(s.store.peek().unwrap(), Loaded::Missing);
}

#[test]
fn set_invalid_game_value_note_and_resume() {
    let mut s = scene();
    let gravity = s.ue.movement + u64::from(GRAVITY_OFF);
    s.poke(gravity, &f32::NAN.to_le_bytes());
    let first = s.tick(&["gravidade"]);
    assert_eq!(first.notes["gravidade"], Note::InvalidGameValue);
    assert!(
        first
            .events
            .contains(&"gravidade: valor do jogo inválido".to_string())
    );
    assert_eq!(journal_data_len(&s), 0);
    // A marca persiste, o evento não.
    let second = s.tick(&["gravidade"]);
    assert_eq!(second.notes["gravidade"], Note::InvalidGameValue);
    assert!(second.events.is_empty(), "{:?}", second.events);
    assert!(s.f32_at(gravity).is_nan());
    // O valor volta a ser são: o cheat escreve e a marca some.
    s.poke(gravity, &1.0f32.to_le_bytes());
    let third = s.tick(&["gravidade"]);
    assert!(third.notes.is_empty());
    // A volta ao normal gera uma linha, uma vez só.
    assert_eq!(
        third.events,
        vec!["gravidade: valor do jogo válido de novo".to_string()]
    );
    assert!(s.tick(&["gravidade"]).events.is_empty());
    assert_eq!(s.f32_at(gravity), 0.5);
    s.runtime.turn_off(&mut s.engine, "gravidade").unwrap();
    assert_eq!(s.f32_at(gravity), 1.0);
}

#[test]
fn set_reread_mismatch_next_tick_new_original() {
    let mut s = scene();
    s.engine
        .inject_data_fault_for_test(Some(DataFault::ThirdValue(15i32.to_le_bytes().to_vec())));
    // O jogo escreve logo depois de nós: a releitura vê um terceiro valor.
    let first = s.tick(&["carga"]);
    assert_eq!(first.notes["carga"], Note::Waiting);
    assert_eq!(s.i32_at(s.cap()), 15);
    assert_eq!(s.engine.data_entries().len(), 1);
    // O ciclo seguinte registra o novo original e escreve o desejado.
    let second = s.tick(&["carga"]);
    assert_eq!(
        second.events,
        vec!["carga: o jogo mudou o valor para 15; novo original".to_string()]
    );
    assert!(second.notes.is_empty());
    assert_eq!(s.i32_at(s.cap()), 20);
    s.runtime.turn_off(&mut s.engine, "carga").unwrap();
    assert_eq!(s.i32_at(s.cap()), 15);
}

/// O jogo reescreve a carga a cada ciclo até o cheat desistir; devolve o último relatório.
fn provoke_block(s: &mut Scene) -> gt_grounded2::fields::TickReport {
    s.tick(&["carga"]);
    let mut last = None;
    for i in 0..6 {
        s.game_sets_capacity(30 + i);
        let r = s.tick(&["carga"]);
        if i < 5 {
            assert!(r.notes.is_empty(), "ciclo {i}: {:?}", r.notes);
            assert_eq!(s.i32_at(s.cap()), 20, "ciclo {i}");
        }
        last = Some(r);
    }
    last.unwrap()
}

#[test]
fn set_game_fights_blocks_after_five() {
    let mut s = scene();
    let sixth = provoke_block(&mut s);
    assert_eq!(sixth.notes["carga"], Note::GameRefuses);
    assert_eq!(
        sixth.events,
        vec![
            "carga: o jogo mudou o valor para 35; novo original".to_string(),
            "carga: o jogo não deixa".to_string()
        ]
    );
    // A entrada foi resolvida: o valor do jogo ficou com o jogo e o diário está vazio.
    assert_eq!(s.i32_at(s.cap()), 35);
    assert_eq!(s.store.peek().unwrap(), Loaded::Missing);
    // Bloqueado: nada é escrito e nenhuma linha nova aparece.
    s.game_sets_capacity(36);
    let seventh = s.tick(&["carga"]);
    assert_eq!(seventh.notes["carga"], Note::GameRefuses);
    assert!(seventh.events.is_empty(), "{:?}", seventh.events);
    assert_eq!(s.i32_at(s.cap()), 36);
    assert_eq!(journal_data_len(&s), 0);
    // Outro pawn: a marca some e o cheat escreve no objeto novo.
    let alt_cap = s.u64_at(s.ue.alt_pawn + u64::from(HAULING_OFF)) + u64::from(CAPACITY_OFF);
    s.swap_pawn(s.ue.alt_pawn);
    let swapped = s.tick(&["carga"]);
    assert!(swapped.notes.is_empty(), "{:?}", swapped.notes);
    assert_eq!(s.i32_at(alt_cap), 20);
    assert_eq!(s.i32_at(s.cap()), 36);
}

#[test]
fn set_block_cleared_by_turn_off() {
    let mut s = scene();
    provoke_block(&mut s);
    s.runtime.turn_off(&mut s.engine, "carga").unwrap();
    let report = s.tick(&["carga"]);
    assert!(report.notes.is_empty(), "{:?}", report.notes);
    assert_eq!(s.i32_at(s.cap()), 20);
    // A janela também recomeçou: precisa de mais de cinco de novo.
    for i in 0..5 {
        s.game_sets_capacity(50 + i);
        assert!(s.tick(&["carga"]).notes.is_empty());
    }
}

#[test]
fn set_fight_window_slides() {
    let mut s = scene();
    s.tick(&["carga"]);
    // Um original novo a cada 3 s: a janela de 10 s nunca passa de quatro.
    for i in 0..10 {
        s.game_sets_capacity(30 + i);
        let r = s.tick_after(&["carga"], Duration::from_secs(3));
        assert!(r.notes.is_empty(), "ciclo {i}: {:?}", r.notes);
        assert_eq!(s.i32_at(s.cap()), 20);
    }
}

#[test]
fn set_partial_failure_completes_later() {
    let mut s = scene();
    let (ground, swim) = (
        s.ue.movement + u64::from(GROUND_MULT_OFF),
        s.ue.movement + u64::from(SWIM_MULT_OFF),
    );
    // O campo de natação fica fora da região escrevível neste ciclo.
    s.forbidden.set((swim, swim + 4));
    let first = s.tick(&["velocidade"]);
    assert_eq!(first.notes["velocidade"], Note::Waiting);
    assert_eq!((s.f32_at(ground), s.f32_at(swim)), (2.0, 1.0));
    assert_eq!(s.engine.data_entries().len(), 1);
    // De volta: a manutenção completa o campo que faltava.
    s.forbidden.set((0, 0));
    s.engine.refresh_data_maps().unwrap();
    let second = s.tick(&["velocidade"]);
    assert!(second.notes.is_empty(), "{:?}", second.notes);
    assert_eq!((s.f32_at(ground), s.f32_at(swim)), (2.0, 2.0));
    assert_eq!(s.engine.data_entries().len(), 2);
    s.runtime.turn_off(&mut s.engine, "velocidade").unwrap();
    assert_eq!((s.f32_at(ground), s.f32_at(swim)), (1.0, 1.0));
    assert_eq!(s.store.peek().unwrap(), Loaded::Missing);
}

#[test]
fn set_pawn_swap_resolves_old_entry() {
    let mut s = scene();
    let alt_cap = s.u64_at(s.ue.alt_pawn + u64::from(HAULING_OFF)) + u64::from(CAPACITY_OFF);
    let alt_before = s.i32_at(alt_cap);
    s.tick(&["carga"]);
    assert_eq!(s.i32_at(s.cap()), 20);
    s.swap_pawn(s.ue.alt_pawn);
    let report = s.tick(&["carga"]);
    assert!(report.notes.is_empty(), "{:?}", report.notes);
    // O antigo volta ao original; o novo ganha a entrada dele.
    assert_eq!(s.i32_at(s.cap()), 10);
    assert_eq!(s.i32_at(alt_cap), 20);
    let entries: Vec<u64> = s.engine.data_entries().iter().map(|e| e.addr).collect();
    assert_eq!(entries, vec![alt_cap]);
    s.runtime.turn_off(&mut s.engine, "carga").unwrap();
    assert_eq!(s.i32_at(alt_cap), alt_before);
}

#[test]
fn mount_never_written() {
    let mut s = scene();
    let c = s.ue.creature;
    let mounts = [
        s.ue.creature_hauling + u64::from(CAPACITY_OFF),
        s.ue.creature_movement + u64::from(GROUND_MULT_OFF),
        s.ue.creature_movement + u64::from(SWIM_MULT_OFF),
        s.ue.creature_movement + u64::from(GRAVITY_OFF),
        c + u64::from(JUMP_OFF),
        c + u64::from(DAMAGE_MULT_OFF),
    ];
    let before: Vec<u32> = mounts.iter().map(|a| s.f32_at(*a).to_bits()).collect();
    // A criatura no `Pawn` (montado): o personagem continua sendo o alvo.
    s.swap_pawn(c);
    for _ in 0..3 {
        let report = s.tick(&SET_IDS);
        assert!(report.notes.is_empty(), "{:?}", report.notes);
    }
    let after: Vec<u32> = mounts.iter().map(|a| s.f32_at(*a).to_bits()).collect();
    assert_eq!(before, after);
    assert!(
        s.engine
            .data_entries()
            .iter()
            .all(|e| !mounts.contains(&e.addr))
    );
    // E o personagem recebeu os valores.
    assert_eq!(s.i32_at(s.cap()), 20);
    assert_eq!(s.f32_at(s.ue.pawn + u64::from(DAMAGE_MULT_OFF)), 10.0);
}

#[test]
fn action_adds_and_rereads() {
    let mut s = scene();
    let c = Some(s.ue.controller);
    assert_eq!(
        s.action(c, 1000),
        ActionOutcome::Done {
            before: 500,
            after: 1500
        }
    );
    assert_eq!(s.i32_at(s.science()), 1500);
    assert_eq!(
        s.action(c, 100),
        ActionOutcome::Done {
            before: 1500,
            after: 1600
        }
    );
    // Sem diário e sem entrada no motor: a ação não é reversível.
    assert_eq!(s.store.peek().unwrap(), Loaded::Missing);
    assert!(s.engine.data_entries().is_empty());
    // Até o teto exato do resultado.
    s.poke(s.science(), &99_000i32.to_le_bytes());
    assert_eq!(
        s.action(c, 1000),
        ActionOutcome::Done {
            before: 99_000,
            after: 100_000
        }
    );
}

#[test]
fn action_raced_when_game_writes_during_the_write() {
    let mut s = scene();
    let c = Some(s.ue.controller);
    s.engine
        .inject_data_fault_for_test(Some(DataFault::ThirdValue(777i32.to_le_bytes().to_vec())));
    assert_eq!(s.action(c, 1000), ActionOutcome::Raced { read: 777 });
    // Não tenta de novo: o valor do jogo fica como está.
    assert_eq!(s.i32_at(s.science()), 777);
    assert!(s.engine.data_entries().is_empty());
    assert_eq!(s.store.peek().unwrap(), Loaded::Missing);
}

#[test]
fn action_refusals() {
    let mut s = scene();
    let c = Some(s.ue.controller);
    let refused = |text: &str| ActionOutcome::Refused(text.to_string());
    // Depois de ler o jogo (passos 3 a 5): a recusa diz que leu.
    let after_read = |text: &str| ActionOutcome::RefusedAfterRead(text.to_string());
    s.poke(s.science(), &(-5i32).to_le_bytes());
    assert_eq!(s.action(c, 1000), after_read("valor atual fora da faixa"));
    s.poke(s.science(), &100_001i32.to_le_bytes());
    assert_eq!(s.action(c, 1000), after_read("valor atual fora da faixa"));
    s.poke(s.science(), &99_500i32.to_le_bytes());
    assert_eq!(s.action(c, 1000), after_read("passaria do máximo"));
    assert_eq!(s.i32_at(s.science()), 99_500);
    // Sem controller, ou com um que não é mais válido: pede a procura.
    assert_eq!(s.action(None, 1000), ActionOutcome::NeedsController);
    assert_eq!(
        s.action(Some(s.ue.pawn), 1000),
        ActionOutcome::NeedsController
    );
    // Controller válido, mas o mundo sumiu do nível.
    s.poke(s.ue.level + u64::from(WORLD_OFF), &0u64.to_le_bytes());
    assert_eq!(s.action(c, 1000), refused("estado do jogo ausente"));
    assert_eq!(s.i32_at(s.science()), 99_500);
}

#[test]
fn action_refreshes_maps_on_new_object() {
    let mut s = scene();
    let c = Some(s.ue.controller);
    let calls = |s: &Scene| s.policy_calls.get();
    let start = calls(&s);
    s.action(c, 100);
    // A primeira execução relê o `maps`; a segunda, no mesmo objeto, não.
    assert_eq!(calls(&s), start + 1);
    s.action(c, 100);
    assert_eq!(calls(&s), start + 1);
    // Outro `PartyComponent` (cópia em outra região): o endereço muda e o `maps` é relido.
    let mut copy = vec![0u8; 0x40];
    s.reader.read_exact(s.ue.party, &mut copy).unwrap();
    let other = s.t.big_region;
    s.poke(other, &copy);
    s.poke(s.ue.game_state + u64::from(PARTY_OFF), &other.to_le_bytes());
    let out = s.action(c, 100);
    assert!(matches!(out, ActionOutcome::Done { .. }), "{out:?}");
    assert_eq!(calls(&s), start + 2);
    assert_eq!(s.i32_at(other + u64::from(SCIENCE_OFF)), 800);
}
