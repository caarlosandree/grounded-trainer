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
    assert_eq!(snap.values.len(), defs.len());
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
