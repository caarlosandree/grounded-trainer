#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Execução dos cheats de valor contra o `gt-testtarget --ue` (spec 005, 6.4 a 6.6): o motor de
//! verdade, com o diário num `TempDir` e a política de dados do jogo (regiões `rw`).

use gt_core::Error;
use gt_core::engine::{Engine, EngineOptions, PolicyFn};
use gt_core::journal::{DataEntry, JournalLock, JournalStore, Loaded};
use gt_core::mem::{MemReader, MemWriter};
use gt_core::patch::WritePolicy;
use gt_core::suspend::PauseConfig;
use gt_grounded2::fields::{FieldRuntime, Note, PlayerState};
use gt_grounded2::player::{Absent, FieldCatalog, FieldValues, read_player};
use gt_grounded2::policy::data_policy;
use gt_testkit::ue::{DAMAGE_OFF, DIE_BYTE, HEALTH_OFF};
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
    engine.set_data_policy(Box::new(|maps| data_policy(&[], maps)));
    Scene {
        t,
        ue,
        reader,
        index,
        objects,
        runtime: FieldRuntime::new(catalog),
        engine,
        store,
        _lock: lock,
        _dir: dir,
    }
}

impl Scene {
    fn tick(&mut self, on: &[&str]) -> gt_grounded2::fields::TickReport {
        let on: Vec<String> = on.iter().map(|s| s.to_string()).collect();
        self.runtime.tick(
            &mut self.engine,
            &self.reader,
            &self.index,
            &self.objects,
            self.ue.controller,
            &on,
        )
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
    assert!(matches!(
        report.player,
        PlayerState::Absent(Absent::ComponentWrongClass(_) | Absent::Unreadable)
    ));
    assert_eq!(report.notes["vida-infinita"], Note::Waiting);
    assert_eq!(report.events, Vec::<String>::new());
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
