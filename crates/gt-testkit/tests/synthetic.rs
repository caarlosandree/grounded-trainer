#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Módulo sintético do `gt-testtarget --module`: a mesma forma do jogo (cabeça mapeada do arquivo,
//! cauda da `.data` em VMA anônima), achado pela descoberta de módulos como o jogo.

use std::path::Path;
use std::time::Duration;

use gt_core::cave::cc_runs;
use gt_core::maps::{MapPath, read_maps};
use gt_core::mem::{MemReader, MemWriter};
use gt_core::modules::{self, TrustPolicy};
use gt_core::pe::{DiskImage, PeHeaders};
use gt_core::suspend::{PauseConfig, open_checked, sample_pcs};
use gt_testkit::{TargetOpts, TestTarget};

fn spawn() -> TestTarget {
    TestTarget::spawn_with(TargetOpts {
        module: true,
        ..TargetOpts::default()
    })
}

/// Descobre os módulos do alvo com o diretório do módulo sintético como confiável.
fn discover(t: &TestTarget) -> Vec<modules::Module> {
    let maps = read_maps(t.pid).unwrap();
    let policy = TrustPolicy::new(vec![t.module_dir().to_path_buf()]);
    modules::discover(
        &maps,
        &MemReader::new(t.pid),
        &policy,
        Path::new(&format!("/proc/{}/root", t.pid)),
    )
    .unwrap()
}

#[test]
fn synthetic_module_is_found() {
    let mut t = spawn();
    let found = discover(&t);
    let module = modules::find(&found, "gt-synthetic.exe").expect("módulo sintético");
    assert_eq!(module.base, t.module_base());
    assert!(module.trusted);
    assert_eq!(module.disk_size, Some(0x5000));
    assert_eq!(module.size, 0x5000);

    // As duas seções, pelo cabeçalho na memória.
    let mut header = vec![0u8; 0x1000];
    MemReader::new(t.pid)
        .read_exact(module.base, &mut header)
        .unwrap();
    let pe = PeHeaders::parse(&header).unwrap();
    let text = pe.section(".text").unwrap();
    let data = pe.section(".data").unwrap();
    assert_eq!(text.perms_string(), "r-x");
    assert_eq!(data.perms_string(), "rw-");

    // O `VirtualSize` da `.data` termina no meio da página, e a cauda é uma VMA anônima `rw-p`
    // até o fim da página, depois da parte mapeada do arquivo.
    let data_end = module.base + u64::from(data.rva) + u64::from(data.virtual_size);
    assert_ne!(data_end % 0x1000, 0);
    let page_end = data_end.next_multiple_of(0x1000);
    let maps = read_maps(t.pid).unwrap();
    let tail = maps
        .iter()
        .find(|r| r.start <= data_end && data_end < r.end)
        .unwrap();
    assert!(tail.is_anonymous(), "{:?}", tail.path);
    assert!(tail.perms.read && tail.perms.write && !tail.perms.exec);
    assert_eq!(tail.end, page_end);
    let head = maps
        .iter()
        .find(|r| r.end == tail.start)
        .expect("parte da .data mapeada do arquivo");
    assert!(matches!(head.path, MapPath::File { .. }));
    assert!(head.perms.read && head.perms.write);
    // A `.text` vem do arquivo, `r-x`.
    let text_region = maps
        .iter()
        .find(|r| r.start == module.base + u64::from(text.rva))
        .unwrap();
    assert!(text_region.perms.exec && !text_region.perms.write);
    assert!(matches!(text_region.path, MapPath::File { .. }));

    assert_eq!(t.call("sum", &[2, 3]) as i32, 5);
}

#[test]
fn synthetic_text_has_cc_runs_and_pointers() {
    let t = spawn();
    let reader = MemReader::new(t.pid);
    let mut text = vec![0u8; 0x1000];
    reader
        .read_exact(t.module_base() + 0x1000, &mut text)
        .unwrap();
    let runs = cc_runs(&text, t.module_base() + 0x1000, 64);
    let lens: Vec<u64> = runs.iter().map(|r| r.end - r.start).collect();
    assert_eq!(lens, vec![300, 70]);
    assert_eq!(runs[0].start, t.symbol("cc300"));
    assert_eq!(runs[1].start, t.symbol("cc70"));
    // Cada função-alvo é seguida de `CC`.
    for name in ["sum", "level_gain", "getter", "drag", "add_item"] {
        assert!(t.symbol(name) >= t.module_base() + 0x1000, "{name}");
    }
}

fn read_i32(t: &TestTarget, addr: u64) -> i32 {
    let mut b = [0u8; 4];
    MemReader::new(t.pid).read_exact(addr, &mut b).unwrap();
    i32::from_le_bytes(b)
}

fn write(t: &TestTarget, addr: u64, bytes: &[u8]) {
    MemWriter::open(t.pid).unwrap().write(addr, bytes).unwrap();
}

#[test]
fn synthetic_functions_compute() {
    let mut t = spawn();
    // sum
    assert_eq!(t.call("sum", &[40, 2]) as i32, 42);
    assert_eq!(t.call("sum", &[(-7i64) as u64, 3]) as i32, -4);
    // level_gain: pontos e nível do objeto de pesquisa (limites 100, 300, 700, ...).
    let obj = t.symbol("research_obj");
    let points = obj + 0x514;
    let level = obj + 0x518;
    assert_eq!(t.call("level_gain", &[obj, 150]) as i32, 1);
    assert_eq!(read_i32(&t, points), 150);
    assert_eq!(t.call("level_gain", &[obj, 100]) as i32, 1); // 250: não cruza 300
    assert_eq!(t.call("level_gain", &[obj, 100]) as i32, 2); // 350: cruza 300
    assert_eq!(read_i32(&t, points), 350);
    assert_eq!(read_i32(&t, level), 2);
    // Sobe no máximo um nível por chamada, mesmo atravessando vários limites.
    assert_eq!(t.call("level_gain", &[obj, 5000]) as i32, 3);
    // getter: cargas em `float`.
    let analyzer = t.symbol("analyzer_obj");
    write(&t, analyzer + 0x974, &7.0f32.to_le_bytes());
    assert_eq!(t.call("getter", &[analyzer]) as i32, 7);
    // drag: StackSize + n; item nulo devolve -1.
    assert_eq!(t.call("drag", &[t.symbol("item_b"), 0]) as i32, 5);
    assert_eq!(t.call("drag", &[t.symbol("item_a"), 10]) as i32, 11);
    assert_eq!(t.call("drag", &[0, 3]) as i32, -1);
    // add_item: soma ao total do inventário.
    let inv = t.symbol("inventory_a");
    assert_eq!(t.call("add_item", &[inv, t.symbol("item_b")]) as i32, 5);
    assert_eq!(t.call("add_item", &[inv, t.symbol("item_b")]) as i32, 10);
    assert_eq!(read_i32(&t, inv + 0x44), 2);
    // Por endereço também.
    let sum = t.symbol("sum");
    assert_eq!(t.call_addr(sum, &[1, 1]) as i32, 2);
}

#[test]
fn synthetic_text_equals_disk() {
    let t = spawn();
    let disk = DiskImage::load(t.module_path()).unwrap();
    let text = disk.headers.section(".text").unwrap().clone();
    let from_disk = disk.bytes(text.rva, text.virtual_size as usize).unwrap();
    let mut from_memory = vec![0u8; text.virtual_size as usize];
    MemReader::new(t.pid)
        .read_exact(t.module_base() + u64::from(text.rva), &mut from_memory)
        .unwrap();
    assert_eq!(from_memory, from_disk);
}

#[test]
fn synthetic_data_slack_is_zero_and_writable() {
    let t = spawn();
    let slack = t.symbol("slack");
    let end = t.symbol("slack_end");
    assert_eq!(end - slack, 0x130);
    let mut buf = vec![0u8; (end - slack) as usize];
    MemReader::new(t.pid).read_exact(slack, &mut buf).unwrap();
    assert!(buf.iter().all(|b| *b == 0));
    write(&t, slack, &[1, 2, 3, 4, 5, 6, 7, 8]);
    MemReader::new(t.pid)
        .read_exact(slack, &mut buf[..8])
        .unwrap();
    assert_eq!(&buf[..8], &[1, 2, 3, 4, 5, 6, 7, 8]);
}

#[test]
fn synthetic_loop_calls_until_stop() {
    let mut t = spawn();
    t.start_loop("sum", &[2, 3]);
    t.start_loop("sum", &[2, 3]);
    std::thread::sleep(Duration::from_millis(50));
    let stats = t.stop_loops();
    assert!(stats.calls > 0);
    assert_eq!(stats.results, vec![5]);
    // Depois do `stop` o alvo continua respondendo e aceita um novo laço.
    assert_eq!(t.call("sum", &[1, 2]) as i32, 3);
    t.start_loop("sum", &[10, 10]);
    assert_eq!(t.stop_loops().results, vec![20]);
}

#[test]
fn synthetic_park_holds_a_thread_at_the_address() {
    let mut t = spawn();
    let spin = t.symbol("spin");
    let tid = t.park(spin);
    assert!(gt_core::procinfo::list_tids(t.pid).unwrap().contains(&tid));
    let (pidfd, _id) = open_checked(t.pid).unwrap();
    let pcs = sample_pcs(
        &pidfd,
        t.pid,
        &PauseConfig::default(),
        20,
        Duration::from_millis(2),
        |_| Ok(()),
        || Ok(()),
        || false,
    )
    .unwrap();
    assert!(pcs.contains(&spin), "nenhum pc em spin: {pcs:x?}");
}

#[test]
fn synthetic_rejects_bad_commands() {
    let mut t = spawn();
    assert!(t.command("call nada").starts_with("err "));
    assert!(t.command("call sum 1 2 3 4 5").starts_with("err "));
    assert!(t.command("voar").starts_with("err "));
    assert_eq!(t.call("sum", &[1, 1]) as i32, 2);
}
