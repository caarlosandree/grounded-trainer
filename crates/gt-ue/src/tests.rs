//! Testes do leitor sobre a reflexão sintética.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::read::{read_u32, read_u64};
use crate::synth::{SparseMem, SynthField, SynthImage, SynthUe};
use crate::{
    BoolBits, ClassQuery, Names, ObjHeader, ObjectArray, ReflectionIndex, ScanBudget, ScanEnd,
    UeError, UeLayout, cheap_check, decode_entry, describe_class, is_object_family,
};
use gt_core::mem::ReadMem;

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

// ---------------------------------------------------------------------------------------------
// Índice de classes e campos.
// ---------------------------------------------------------------------------------------------

fn fld(name: &'static str, kind: &'static str, offset: u32, size: u32) -> SynthField {
    SynthField {
        name,
        kind,
        offset,
        size,
        bool_mask: None,
    }
}

/// Monta a imagem e o índice. O `mem` é devolvido para os testes mexerem na memória depois.
fn indexed(s: SynthUe) -> (SparseMem, SynthImage, ReflectionIndex) {
    let layout = UeLayout::observed();
    let image = s.finish();
    let mem = image.into_mem();
    let array = ObjectArray::read(&mem, &layout, image.gobjects).unwrap();
    let names = Names::new(&mem, image.names).unwrap();
    let index = ReflectionIndex::build(&mem, &layout, &array, &names, &budget()).unwrap();
    (mem, image, index)
}

fn synth() -> SynthUe {
    SynthUe::new(UeLayout::observed(), ARENA, ARENA_LEN)
}

#[test]
fn index_finds_anchor_and_classes() {
    let s = synth();
    let (anchor, object, actor) = (s.class_class, s.object_class, s.actor_class);
    let (mem, _, index) = indexed(s);
    assert_eq!(index.anchor(), anchor);
    assert_eq!(
        index.find_class("/Script/CoreUObject", "Class").unwrap(),
        anchor
    );
    assert_eq!(
        index.find_class("/Script/CoreUObject", "Object").unwrap(),
        object
    );
    assert_eq!(index.find_class("/Script/Engine", "Actor").unwrap(), actor);
    // Objetos que não são classes (os pacotes) ficam fora.
    assert!(matches!(
        index.find_class("", "/Script/Engine"),
        Err(UeError::NotFound(_))
    ));
    index.deep_check().unwrap();
    assert_eq!(index.class_of(&mem, actor).unwrap(), anchor);
    assert!(index.is_a(&mem, actor, object).unwrap());
    assert!(index.is_a(&mem, actor, actor).unwrap());
    assert!(!index.is_a(&mem, object, actor).unwrap());
    assert_eq!(
        index.props_size(&mem, actor).unwrap(),
        crate::synth::ACTOR_PROPS_SIZE
    );
}

#[test]
fn index_works_through_dyn_read_mem() {
    let s = synth();
    let actor = s.actor_class;
    let layout = UeLayout::observed();
    let image = s.finish();
    let mem = image.into_mem();
    let dyn_mem: &dyn ReadMem = &mem;
    let array = ObjectArray::read(dyn_mem, &layout, image.gobjects).unwrap();
    let names = Names::new(dyn_mem, image.names).unwrap();
    let index = ReflectionIndex::build(dyn_mem, &layout, &array, &names, &budget()).unwrap();
    assert_eq!(index.find_class("/Script/Engine", "Actor").unwrap(), actor);
    cheap_check(dyn_mem, &layout, image.gobjects, image.names).unwrap();
}

#[test]
fn build_without_anchor_is_not_found() {
    let s = synth();
    let layout = UeLayout::observed();
    let class_class = s.class_class;
    let image = s.finish();
    let mem = image.into_mem();
    // `Class` deixa de ser classe de si mesma.
    mem.write(
        class_class + u64::from(layout.uobject.class),
        &0x1234_0000u64.to_le_bytes(),
    );
    let array = ObjectArray::read(&mem, &layout, image.gobjects).unwrap();
    let names = Names::new(&mem, image.names).unwrap();
    assert!(matches!(
        ReflectionIndex::build(&mem, &layout, &array, &names, &budget()),
        Err(UeError::NotFound(_))
    ));
}

#[test]
fn build_deadline() {
    let s = synth();
    let layout = UeLayout::observed();
    let image = s.finish();
    let mem = image.into_mem();
    let array = ObjectArray::read(&mem, &layout, image.gobjects).unwrap();
    let names = Names::new(&mem, image.names).unwrap();
    let mut b = budget();
    b.deadline = Duration::ZERO;
    assert!(matches!(
        ReflectionIndex::build(&mem, &layout, &array, &names, &b),
        Err(UeError::Deadline)
    ));
}

#[test]
fn find_class_wrong_package_not_found() {
    let (_, _, index) = indexed(synth());
    assert!(matches!(
        index.find_class("/Script/Wrong", "Actor"),
        Err(UeError::NotFound(_))
    ));
    assert!(matches!(
        index.find_class("/Script/Engine", "Nope"),
        Err(UeError::NotFound(_))
    ));
}

#[test]
fn find_class_blueprint_child_of_class() {
    let mut s = synth();
    let (engine, class_class, actor) = (s.engine_package, s.class_class, s.actor_class);
    // `BlueprintGeneratedClass` é filha de `Class`; a classe do Blueprint é um objeto dela.
    let bgc = s.add_class(
        engine,
        "BlueprintGeneratedClass",
        Some(class_class),
        0x40,
        &[],
    );
    let game = s.add_package("/Game/Blueprints/BP_Hero");
    let hero = s.add_class_of(bgc, game, "BP_Hero_C", Some(actor), 0x80, &[]);
    let (mem, _, index) = indexed(s);
    assert_eq!(
        index
            .find_class("/Game/Blueprints/BP_Hero", "BP_Hero_C")
            .unwrap(),
        hero
    );
    assert_eq!(index.class_of(&mem, hero).unwrap(), bgc);
    assert!(index.is_a(&mem, bgc, index.anchor()).unwrap());
    assert!(index.is_a(&mem, hero, actor).unwrap());
}

#[test]
fn find_class_two_same_name_ambiguous() {
    let mut s = synth();
    let engine = s.engine_package;
    s.add_class(engine, "Twin", None, 0x40, &[]);
    s.add_class(engine, "Twin", None, 0x40, &[]);
    let (_, _, index) = indexed(s);
    assert!(matches!(
        index.find_class("/Script/Engine", "Twin"),
        Err(UeError::Ambiguous(_))
    ));
    // As classes de outros nomes seguem achadas.
    index.find_class("/Script/Engine", "Actor").unwrap();
}

#[test]
fn find_field_walks_super() {
    let mut s = synth();
    let (engine, actor) = (s.engine_package, s.actor_class);
    let pawn = s.add_class(
        engine,
        "Pawn",
        Some(actor),
        0x60,
        &[fld("Health", "FloatProperty", 0x48, 4)],
    );
    let (mem, _, index) = indexed(s);
    let health = index.find_field(&mem, pawn, "Health").unwrap();
    assert_eq!(
        health,
        crate::Field {
            owner: "Pawn".to_string(),
            name: "Health".to_string(),
            kind: "FloatProperty".to_string(),
            offset: 0x48,
            size: 4,
            bits: None,
        }
    );
    // Campo da superclasse.
    let root = index.find_field(&mem, pawn, "RootComponent").unwrap();
    assert_eq!(root.owner, "Actor");
    assert_eq!(root.offset, crate::synth::ACTOR_ROOT_COMPONENT_OFFSET);
    assert!(is_object_family(&root.kind));
    // O campo da filha não aparece na mãe.
    assert!(matches!(
        index.find_field(
            &mem,
            index.find_class("/Script/Engine", "Actor").unwrap(),
            "Health"
        ),
        Err(UeError::NotFound(_))
    ));
    assert!(matches!(
        index.find_field(&mem, pawn, "Nope"),
        Err(UeError::NotFound(_))
    ));
}

#[test]
fn find_field_bool_bits() {
    let mut s = synth();
    let (engine, actor) = (s.engine_package, s.actor_class);
    let flagged = s.add_class(
        engine,
        "Flagged",
        Some(actor),
        0x60,
        &[SynthField {
            name: "bFlag",
            kind: "BoolProperty",
            offset: 0x50,
            size: 1,
            bool_mask: Some((2, 0x08)),
        }],
    );
    let (mem, _, index) = indexed(s);
    let f = index.find_field(&mem, flagged, "bFlag").unwrap();
    assert_eq!(f.kind, "BoolProperty");
    assert_eq!(f.offset, 0x50);
    assert_eq!(
        f.bits,
        Some(BoolBits {
            byte: 0x52,
            mask: 0x08
        })
    );
}

#[test]
fn super_chain_cycle_is_malformed() {
    let mut s = synth();
    let layout = UeLayout::observed();
    let (engine, actor) = (s.engine_package, s.actor_class);
    let a = s.add_class(engine, "CycleA", Some(actor), 0x40, &[]);
    let b = s.add_class(engine, "CycleB", Some(a), 0x40, &[]);
    let (mem, _, index) = indexed(s);
    // Fecha o ciclo A -> B -> A depois do índice montado.
    mem.write(a + u64::from(layout.ustruct.super_struct), &b.to_le_bytes());
    assert!(matches!(
        index.is_a(&mem, b, 0x4242_0000),
        Err(UeError::Malformed(_))
    ));
    assert!(matches!(
        index.find_field(&mem, b, "Nope"),
        Err(UeError::Malformed(_))
    ));
    // Achar o ancestral antes de fechar o ciclo ainda funciona.
    assert!(index.is_a(&mem, b, a).unwrap());
}

#[test]
fn super_chain_cap_is_64() {
    let mut s = synth();
    let engine = s.engine_package;
    let mut prev = s.object_class;
    let mut chain = Vec::new();
    for i in 0..70 {
        prev = s.add_class(engine, &format!("Deep{i}"), Some(prev), 0x40, &[]);
        chain.push(prev);
    }
    let (mem, _, index) = indexed(s);
    // Com 1 (Object) + 63 filhas a cadeia tem 64 e ainda passa; a seguinte estoura o teto.
    assert!(!index.is_a(&mem, chain[62], 0x4242_0000).unwrap());
    assert!(matches!(
        index.is_a(&mem, chain[63], 0x4242_0000),
        Err(UeError::Malformed(_))
    ));
}

#[test]
fn field_list_cap() {
    let fill = |n: usize, last: &'static str| -> Vec<SynthField> {
        (0..n)
            .map(|i| {
                fld(
                    if i + 1 == n { last } else { "Filler" },
                    "IntProperty",
                    0x40,
                    4,
                )
            })
            .collect()
    };
    let mut s = synth();
    let (engine, actor) = (s.engine_package, s.actor_class);
    let exact = s.add_class(engine, "Exact", Some(actor), 0x40, &fill(4096, "Target"));
    let over = s.add_class(engine, "Over", Some(actor), 0x40, &fill(4097, "Target"));
    let (mem, _, index) = indexed(s);
    // 4096 campos é o limite e o último é achado.
    assert_eq!(
        index.find_field(&mem, exact, "Target").unwrap().name,
        "Target"
    );
    assert_eq!(index.fields_of(&mem, exact).unwrap().len(), 4096 + 1);
    // 4097 campos estoura o teto, até para um campo que nem existe.
    assert!(matches!(
        index.find_field(&mem, over, "Target"),
        Err(UeError::Malformed(_))
    ));
    assert!(matches!(
        index.fields_of(&mem, over),
        Err(UeError::Malformed(_))
    ));
}

#[test]
fn field_list_cycle_is_malformed() {
    let mut s = synth();
    let layout = UeLayout::observed();
    let (engine, actor) = (s.engine_package, s.actor_class);
    let c = s.add_class(
        engine,
        "Loop",
        Some(actor),
        0x40,
        &[fld("A", "IntProperty", 0x40, 4)],
    );
    let (mem, _, index) = indexed(s);
    let first = read_u64(&mem, c + u64::from(layout.ustruct.child_properties)).unwrap();
    mem.write(first + u64::from(layout.ffield.next), &first.to_le_bytes());
    assert!(matches!(
        index.find_field(&mem, c, "Nope"),
        Err(UeError::Malformed(_))
    ));
}

#[test]
fn object_ptr_property_is_object_family() {
    assert!(is_object_family("ObjectProperty"));
    assert!(is_object_family("ObjectPtrProperty"));
    assert!(!is_object_family("FloatProperty"));
    assert!(!is_object_family("SoftObjectProperty"));

    // Um `RootComponent` do tipo `ObjectPtrProperty` passa na conferência profunda.
    let layout = UeLayout::observed();
    let mut s = synth();
    let engine = s.engine_package;
    s.add_class(
        engine,
        "PtrHolder",
        None,
        0x40,
        &[fld("Ptr", "ObjectPtrProperty", 0x10, 8)],
    );
    let image = s.finish();
    let mem = image.into_mem();
    let array = ObjectArray::read(&mem, &layout, image.gobjects).unwrap();
    let names = Names::new(&mem, image.names).unwrap();
    let index = ReflectionIndex::build(&mem, &layout, &array, &names, &budget()).unwrap();
    let holder = index.find_class("/Script/Engine", "PtrHolder").unwrap();
    let f = index.find_field(&mem, holder, "Ptr").unwrap();
    assert_eq!(f.kind, "ObjectPtrProperty");
    assert!(is_object_family(&f.kind));
    // Aponta o `FFieldClass` do `RootComponent` para o de `Ptr`: segue coerente.
    let actor = index.find_class("/Script/Engine", "Actor").unwrap();
    let root_first = read_u64(&mem, actor + u64::from(layout.ustruct.child_properties)).unwrap();
    let ptr_first = read_u64(&mem, holder + u64::from(layout.ustruct.child_properties)).unwrap();
    let ptr_class = read_u64(&mem, ptr_first + u64::from(layout.ffield.class_private)).unwrap();
    mem.write(
        root_first + u64::from(layout.ffield.class_private),
        &ptr_class.to_le_bytes(),
    );
    let rebuilt = ReflectionIndex::build(&mem, &layout, &array, &names, &budget()).unwrap();
    rebuilt.deep_check().unwrap();
    assert_eq!(
        rebuilt
            .find_field(&mem, actor, "RootComponent")
            .unwrap()
            .kind,
        "ObjectPtrProperty"
    );
}

/// Reconstrói o índice sobre a memória (já alterada) de `image`.
fn rebuild(mem: &SparseMem, image: &SynthImage) -> ReflectionIndex {
    let layout = UeLayout::observed();
    let array = ObjectArray::read(mem, &layout, image.gobjects).unwrap();
    let names = Names::new(mem, image.names).unwrap();
    ReflectionIndex::build(mem, &layout, &array, &names, &budget()).unwrap()
}

#[test]
fn deep_check_rejects_missing_root_component() {
    let layout = UeLayout::observed();
    let mut s = synth();
    let (engine, actor) = (s.engine_package, s.actor_class);
    let floaty = s.add_class(
        engine,
        "Floaty",
        None,
        0x40,
        &[fld("F", "FloatProperty", 0x10, 4)],
    );
    let image = s.finish();
    let child_props = |c: u64| c + u64::from(layout.ustruct.child_properties);

    // Intacta: coerente.
    let mem = image.into_mem();
    rebuild(&mem, &image).deep_check().unwrap();

    // `Actor` sem campos.
    let mem = image.into_mem();
    mem.write(child_props(actor), &0u64.to_le_bytes());
    let err = rebuild(&mem, &image).deep_check().unwrap_err();
    assert!(err.contains("RootComponent"), "{err}");

    // `RootComponent` de um tipo que não é de objeto: usa o `FFieldClass` de uma `FloatProperty`.
    let mem = image.into_mem();
    let root = read_u64(&mem, child_props(actor)).unwrap();
    let float_field = read_u64(&mem, child_props(floaty)).unwrap();
    let float_class = read_u64(&mem, float_field + u64::from(layout.ffield.class_private)).unwrap();
    mem.write(
        root + u64::from(layout.ffield.class_private),
        &float_class.to_le_bytes(),
    );
    let err = rebuild(&mem, &image).deep_check().unwrap_err();
    assert!(err.contains("FloatProperty"), "{err}");

    // `Actor` com outro nome (o índice 0 é "None"): a classe `Actor` não é achada.
    let mem = image.into_mem();
    mem.write(actor + u64::from(layout.uobject.name), &0u32.to_le_bytes());
    let err = rebuild(&mem, &image).deep_check().unwrap_err();
    assert!(err.contains("Actor"), "{err}");
}

#[test]
fn instances_skip_cdo_and_include_children() {
    let mut s = synth();
    let layout = UeLayout::observed();
    let (engine, actor, object) = (s.engine_package, s.actor_class, s.object_class);
    let pawn = s.add_class(engine, "Pawn", Some(actor), 0x60, &[]);
    let other = s.add_class(engine, "Other", Some(object), 0x60, &[]);
    let a1 = s.add_object(actor, "A1", engine);
    let p1 = s.add_object(pawn, "P1", engine);
    let cdo = s.add_object(pawn, "Default__Pawn", engine);
    s.set(
        cdo,
        layout.uobject.flags,
        &layout.uobject.cdo_flag.to_le_bytes(),
    );
    let o1 = s.add_object(other, "O1", engine);
    let (mem, image, index) = indexed(s);
    let array = ObjectArray::read(&mem, &layout, image.gobjects).unwrap();

    let mut of_actor = index.instances_of(&mem, &array, actor, &budget()).unwrap();
    of_actor.sort_unstable();
    assert_eq!(of_actor, vec![a1, p1]);
    let of_pawn = index.instances_of(&mem, &array, pawn, &budget()).unwrap();
    assert_eq!(of_pawn, vec![p1]);
    assert!(!of_actor.contains(&cdo) && !of_actor.contains(&o1));
    // Teto de tempo.
    let mut b = budget();
    b.deadline = Duration::ZERO;
    assert!(matches!(
        index.instances_of(&mem, &array, actor, &b),
        Err(UeError::Deadline)
    ));
}

#[test]
fn describe_class_lists_fields_with_super() {
    let mut s = synth();
    let (engine, actor) = (s.engine_package, s.actor_class);
    s.add_class(
        engine,
        "Pawn",
        Some(actor),
        0x60,
        &[
            fld("Health", "FloatProperty", 0x48, 4),
            SynthField {
                name: "bDead",
                kind: "BoolProperty",
                offset: 0x4c,
                size: 1,
                bool_mask: Some((0, 0x01)),
            },
        ],
    );
    let (mem, _, index) = indexed(s);
    for query in ["/Script/Engine.Pawn", "Pawn"] {
        let ClassQuery::Found(dump) = describe_class(&index, &mem, query).unwrap() else {
            panic!("esperava Found para {query}");
        };
        assert_eq!(dump.path, "/Script/Engine.Pawn");
        let names: Vec<&str> = dump.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["Health", "bDead", "RootComponent"]);
        assert_eq!(dump.fields[1].bits.unwrap().byte, 0x4c);
    }
    assert!(matches!(
        describe_class(&index, &mem, "/Script/Wrong.Pawn"),
        Err(UeError::NotFound(_))
    ));
    assert!(matches!(
        describe_class(&index, &mem, "NoSuchClass"),
        Err(UeError::NotFound(_))
    ));
}

#[test]
fn describe_class_ambiguous_lists_paths() {
    let mut s = synth();
    let engine = s.engine_package;
    let game = s.add_package("/Script/Maine");
    s.add_class(engine, "Twin", None, 0x40, &[]);
    s.add_class(game, "Twin", None, 0x40, &[]);
    let (mem, _, index) = indexed(s);
    let ClassQuery::Ambiguous(paths) = describe_class(&index, &mem, "Twin").unwrap() else {
        panic!("esperava Ambiguous");
    };
    assert_eq!(paths, ["/Script/Engine.Twin", "/Script/Maine.Twin"]);
    // Com o pacote, não há ambiguidade.
    assert!(matches!(
        describe_class(&index, &mem, "/Script/Maine.Twin").unwrap(),
        ClassQuery::Found(_)
    ));
    assert_eq!(index.classes_named("Twin").len(), 2);
}

#[test]
fn field_path_through_struct() {
    let mut s = synth();
    let (engine, object) = (s.engine_package, s.object_class);
    let settings = s.add_struct(
        engine,
        "TraitSettings",
        0x10,
        &[
            SynthField {
                name: "MaxValue",
                kind: "FloatProperty",
                offset: 0x0,
                size: 4,
                bool_mask: None,
            },
            SynthField {
                name: "Enabled",
                kind: "BoolProperty",
                offset: 0x8,
                size: 1,
                bool_mask: Some((0, 0x02)),
            },
        ],
    );
    let comp = s.add_class(
        engine,
        "Comp",
        Some(object),
        0x80,
        &[
            SynthField {
                name: "Hunger",
                kind: "StructProperty",
                offset: 0x40,
                size: 0x10,
                bool_mask: None,
            },
            SynthField {
                name: "Current",
                kind: "FloatProperty",
                offset: 0x50,
                size: 4,
                bool_mask: None,
            },
        ],
    );
    s.link_struct(comp, "Hunger", settings);
    let (mem, _image, index) = indexed(s);

    let max = index
        .find_field_path(&mem, comp, "Hunger.MaxValue")
        .unwrap();
    assert_eq!((max.offset, max.kind.as_str()), (0x40, "FloatProperty"));
    assert_eq!(max.name, "Hunger.MaxValue");
    let flag = index.find_field_path(&mem, comp, "Hunger.Enabled").unwrap();
    assert_eq!(
        flag.bits,
        Some(BoolBits {
            byte: 0x48,
            mask: 0x02
        })
    );
    // Sem ponto, o mesmo que `find_field`.
    assert_eq!(
        index.find_field_path(&mem, comp, "Current").unwrap(),
        index.find_field(&mem, comp, "Current").unwrap()
    );
    assert!(matches!(
        index.find_field_path(&mem, comp, "Hunger.Nope"),
        Err(UeError::NotFound(_))
    ));
    // Parte do meio que não é struct.
    assert!(matches!(
        index.find_field_path(&mem, comp, "Current.MaxValue"),
        Err(UeError::Malformed(_))
    ));
    // A struct não é uma classe: não entra no índice.
    assert!(index.find_class("/Script/Engine", "TraitSettings").is_err());
}

#[test]
fn object_field_class_follows_property_class() {
    let mut s = synth();
    let (engine, object, actor) = (s.engine_package, s.object_class, s.actor_class);
    let pet_master = s.add_class(
        engine,
        "PetMaster",
        Some(object),
        0x40,
        &[SynthField {
            name: "CurrentPet",
            kind: "ObjectProperty",
            offset: 0x30,
            size: 8,
            bool_mask: None,
        }],
    );
    let creature = s.add_class(engine, "Creature", Some(actor), 0x60, &[]);
    s.link_struct(pet_master, "CurrentPet", creature);
    let (mem, _image, index) = indexed(s);
    let (field, class) = index
        .object_field_class(&mem, pet_master, "CurrentPet")
        .unwrap();
    assert_eq!((field.offset, class), (0x30, creature));
    // Campo que não é de objeto.
    assert!(matches!(
        index.object_field_class(&mem, actor, "RootComponent"),
        Err(UeError::Malformed(_))
    ));
}
