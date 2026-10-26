//! Testes do leitor sobre a reflexão sintética.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::read::{read_u32, read_u64};
use crate::synth::{SparseMem, SynthImage, SynthUe};
use crate::{
    Names, ObjHeader, ObjectArray, ScanBudget, ScanEnd, UeError, UeLayout, cheap_check,
    decode_entry,
};

const ARENA: u64 = 0x1000_0000;
const ARENA_LEN: usize = 256 << 20;

/// Layout de blocos pequenos, para a tabela ter vários blocos com poucos objetos.
fn small_layout() -> UeLayout {
    let mut l = UeLayout::observed();
    l.objects.items_per_chunk = 4;
    l
}

/// Imagem com os objetos fundamentais mais `extra` instâncias de `Actor`.
fn build(layout: UeLayout, extra: usize) -> (SynthImage, Vec<u64>) {
    let mut s = SynthUe::new(layout, ARENA, ARENA_LEN);
    let (actor, engine) = (s.actor_class, s.engine_package);
    for i in 0..extra {
        s.add_object(actor, &format!("Actor_{i}"), engine);
    }
    let objects = s.objects().to_vec();
    (s.finish(), objects)
}

fn no_sleep(_: Duration) {}

fn budget() -> ScanBudget {
    ScanBudget {
        block_bytes: 3 * 24,
        batch: 2,
        pause: Duration::ZERO,
        deadline: Duration::from_secs(60),
        sleep: no_sleep,
    }
}

fn names_block0(mem: &SparseMem, image: &SynthImage) -> u64 {
    read_u64(mem, image.names).unwrap()
}

/// Endereço do item `index` na tabela.
fn item_addr(mem: &SparseMem, layout: &UeLayout, image: &SynthImage, index: u64) -> u64 {
    let array = ObjectArray::read(mem, layout, image.gobjects).unwrap();
    let per = u64::from(layout.objects.items_per_chunk);
    array.chunks[(index / per) as usize] + (index % per) * u64::from(layout.objects.item_size)
}

/// Classe do objeto de índice 6 (primeira instância extra de `Actor`).
fn actor_class(mem: &SparseMem, layout: &UeLayout, objects: &[u64]) -> u64 {
    read_u64(mem, objects[6] + u64::from(layout.uobject.class)).unwrap()
}

fn write_u32(mem: &SparseMem, addr: u64, v: u32) {
    mem.write(addr, &v.to_le_bytes());
}

#[test]
fn decode_ansi_and_utf16() {
    assert_eq!(decode_entry(4 << 6, b"None").unwrap(), "None");
    // latin-1: 0xE7 é "ç".
    assert_eq!(decode_entry(1 << 6, &[0xe7]).unwrap(), "ç");
    let wide: Vec<u8> = "Ação"
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    assert_eq!(decode_entry((4 << 6) | 1, &wide).unwrap(), "Ação");
}

#[test]
fn decode_rejects_oversized() {
    // 600 caracteres UTF-16 = 1200 bytes, acima do teto de 1024.
    let raw = vec![b'a'; 1200];
    assert!(matches!(
        decode_entry((600 << 6) | 1, &raw),
        Err(UeError::Malformed(_))
    ));
    // Texto menor do que o cabeçalho promete.
    assert!(matches!(
        decode_entry(8 << 6, b"abc"),
        Err(UeError::Malformed(_))
    ));
    // No teto exato: 512 caracteres UTF-16 = 1024 bytes.
    let raw = [0x61u8, 0x00].repeat(512);
    assert_eq!(decode_entry((512 << 6) | 1, &raw).unwrap().len(), 512);
}

#[test]
fn fname_with_number() {
    let mut s = SynthUe::new(UeLayout::observed(), ARENA, ARENA_LEN);
    let foo = s.intern_name("Foo");
    let image = s.finish();
    let mem = image.into_mem();
    let names = Names::new(&mem, image.names).unwrap();
    assert_eq!(names.fname(foo, 0).unwrap(), "Foo");
    assert_eq!(names.fname(foo, 1).unwrap(), "Foo_0");
    assert_eq!(names.fname(foo, 3).unwrap(), "Foo_2");
    // Segunda consulta vem do cache.
    assert_eq!(names.name(foo).unwrap(), "Foo");
}

#[test]
fn name_zero_is_none() {
    let (image, _) = build(UeLayout::observed(), 0);
    let mem = image.into_mem();
    let names = Names::new(&mem, image.names).unwrap();
    assert_eq!(names.name(0).unwrap(), "None");
    assert!(names.block_count() >= 1);
    // Bloco que não existe.
    assert!(matches!(names.name(5 << 16), Err(UeError::Malformed(_))));
}

#[test]
fn names_read_non_ascii_and_reflection_names() {
    let mut s = SynthUe::new(UeLayout::observed(), ARENA, ARENA_LEN);
    let id = s.intern_name("Ação");
    let image = s.finish();
    let mem = image.into_mem();
    let names = Names::new(&mem, image.names).unwrap();
    assert_eq!(names.name(id).unwrap(), "Ação");
}

#[test]
fn object_at_reads_item_offset() {
    let layout = small_layout();
    let (image, objects) = build(layout, 6);
    let mem = image.into_mem();
    let array = ObjectArray::read(&mem, &layout, image.gobjects).unwrap();
    assert_eq!(array.num_elements as usize, objects.len());
    assert!(array.num_chunks >= 3, "vários blocos de 4 itens");
    for (i, &obj) in objects.iter().enumerate() {
        assert_eq!(array.object_at(&mem, &layout, i as u32).unwrap(), Some(obj));
    }
    // Fora de NumElements.
    assert_eq!(
        array
            .object_at(&mem, &layout, objects.len() as u32)
            .unwrap(),
        None
    );
    // Item zerado.
    let item = item_addr(&mem, &layout, &image, 2);
    mem.write(item + u64::from(layout.objects.item_object), &[0; 8]);
    assert_eq!(array.object_at(&mem, &layout, 2).unwrap(), None);
}

#[test]
fn scan_visits_all_skipping_null() {
    static SLEEPS: AtomicUsize = AtomicUsize::new(0);
    fn count_sleep(_: Duration) {
        SLEEPS.fetch_add(1, Ordering::SeqCst);
    }
    let layout = small_layout();
    let (image, objects) = build(layout, 9);
    let mem = image.into_mem();
    let array = ObjectArray::read(&mem, &layout, image.gobjects).unwrap();
    let names = Names::new(&mem, image.names).unwrap();
    // Item 5 vazio.
    let item = item_addr(&mem, &layout, &image, 5);
    mem.write(item + u64::from(layout.objects.item_object), &[0; 8]);

    let mut b = budget();
    b.sleep = count_sleep;
    let mut seen: Vec<ObjHeader> = Vec::new();
    let end = array.scan(&mem, &layout, &b, |h| seen.push(h)).unwrap();
    assert_eq!(end, ScanEnd::Done);
    assert_eq!(seen.len(), objects.len() - 1);
    assert!(seen.iter().all(|h| h.index != 5));
    assert!(
        SLEEPS.load(Ordering::SeqCst) >= 2,
        "pausas entre blocos e lotes"
    );
    for h in &seen {
        assert_eq!(h.addr, objects[h.index as usize]);
        assert_eq!(h.internal_index, h.index);
        assert_eq!(h.flags, 0);
    }
    // Os cabeçalhos trazem classe, nome e outer reais.
    // Os objetos fundamentais são 6; o de índice 6 é a primeira instância de Actor.
    let first_actor = seen.iter().find(|h| h.index == 6).unwrap();
    assert_eq!(first_actor.class, actor_class(&mem, &layout, &objects));
    assert_eq!(
        names.fname(first_actor.name.0, first_actor.name.1).unwrap(),
        "Actor_0"
    );
    assert_ne!(first_actor.outer, 0);
    // Ordem crescente de índice.
    assert!(seen.windows(2).all(|w| w[0].index < w[1].index));
}

#[test]
fn scan_respects_deadline() {
    let layout = small_layout();
    let (image, _) = build(layout, 5);
    let mem = image.into_mem();
    let array = ObjectArray::read(&mem, &layout, image.gobjects).unwrap();
    let mut b = budget();
    b.deadline = Duration::ZERO;
    let mut visited = 0;
    let end = array.scan(&mem, &layout, &b, |_| visited += 1).unwrap();
    assert_eq!(end, ScanEnd::DeadlineExceeded);
    assert_eq!(visited, 0);
}

#[test]
fn cheap_check_ok_on_synth() {
    let (image, _) = build(UeLayout::observed(), 3);
    let mem = image.into_mem();
    cheap_check(&mem, &UeLayout::observed(), image.gobjects, image.names).unwrap();
    let layout = small_layout();
    let (image, _) = build(layout, 12);
    let mem = image.into_mem();
    cheap_check(&mem, &layout, image.gobjects, image.names).unwrap();
}

#[test]
fn cheap_check_rejects() {
    let layout = UeLayout::observed();
    let o = layout.objects;
    let fresh = || {
        let (image, _) = build(layout, 20);
        (image.into_mem(), image)
    };

    // Nome 0 diferente de None.
    let (mem, image) = fresh();
    let block0 = names_block0(&mem, &image);
    mem.write(block0 + 2, b"Nope");
    let err = cheap_check(&mem, &layout, image.gobjects, image.names).unwrap_err();
    assert!(err.contains("None"), "{err}");

    // NumElements > MaxElements.
    let (mem, image) = fresh();
    let num = read_u32(&mem, image.gobjects + u64::from(o.num_elements)).unwrap();
    write_u32(&mem, image.gobjects + u64::from(o.max_elements), num - 1);
    let err = cheap_check(&mem, &layout, image.gobjects, image.names).unwrap_err();
    assert!(err.contains("MaxElements"), "{err}");

    // NumElements = 0.
    let (mem, image) = fresh();
    write_u32(&mem, image.gobjects + u64::from(o.num_elements), 0);
    assert!(cheap_check(&mem, &layout, image.gobjects, image.names).is_err());

    // NumChunks > MaxChunks.
    let (mem, image) = fresh();
    write_u32(&mem, image.gobjects + u64::from(o.max_chunks), 0);
    let err = cheap_check(&mem, &layout, image.gobjects, image.names).unwrap_err();
    assert!(err.contains("MaxChunks"), "{err}");

    // NumChunks × 65536 < NumElements.
    let (mem, image) = fresh();
    write_u32(&mem, image.gobjects + u64::from(o.max_elements), 0x25_0000);
    write_u32(&mem, image.gobjects + u64::from(o.num_elements), 70_000);
    let err = cheap_check(&mem, &layout, image.gobjects, image.names).unwrap_err();
    assert!(err.contains("NumChunks"), "{err}");

    // Amostra com InternalIndex trocado.
    let (mem, image) = fresh();
    let array = ObjectArray::read(&mem, &layout, image.gobjects).unwrap();
    let obj = array.object_at(&mem, &layout, 3).unwrap().unwrap();
    write_u32(&mem, obj + u64::from(layout.uobject.index), 999);
    let err = cheap_check(&mem, &layout, image.gobjects, image.names).unwrap_err();
    assert!(err.contains("InternalIndex"), "{err}");

    // Item da amostra aponta para memória não mapeada.
    let (mem, image) = fresh();
    let item = item_addr(&mem, &layout, &image, 4);
    mem.write(
        item + u64::from(o.item_object),
        &0x7000_0000_0000u64.to_le_bytes(),
    );
    assert!(cheap_check(&mem, &layout, image.gobjects, image.names).is_err());
}

#[test]
fn cheap_check_accepts_extra_chunks() {
    // 10 blocos para ~506 mil elementos: o jogo aloca blocos adiantados, então vale "≥", não "=".
    let layout = UeLayout::observed();
    let o = layout.objects;
    let (image, _) = build(layout, 20);
    let mem = image.into_mem();
    let chunk_bytes = u64::from(o.items_per_chunk) * u64::from(o.item_size);
    let array = ObjectArray::read(&mem, &layout, image.gobjects).unwrap();
    let mut chunks = array.chunks.clone();
    mem.map_zero(chunks[0], chunk_bytes);
    let extra_base = 0x6000_0000_0000u64;
    let mut list = vec![0u8; 0];
    for i in 0..10u64 {
        if i > 0 {
            let base = extra_base + i * chunk_bytes;
            mem.map_zero(base, chunk_bytes);
            chunks.push(base);
        }
    }
    chunks.truncate(10);
    for c in &chunks {
        list.extend_from_slice(&c.to_le_bytes());
    }
    let list_addr = 0x5000_0000_0000u64;
    mem.write(list_addr, &list);
    mem.write(
        image.gobjects + u64::from(o.chunks),
        &list_addr.to_le_bytes(),
    );
    write_u32(&mem, image.gobjects + u64::from(o.max_elements), 0x25_0000);
    write_u32(&mem, image.gobjects + u64::from(o.num_elements), 506_000);
    write_u32(&mem, image.gobjects + u64::from(o.max_chunks), 0x25);
    write_u32(&mem, image.gobjects + u64::from(o.num_chunks), 10);
    cheap_check(&mem, &layout, image.gobjects, image.names).unwrap();
}
