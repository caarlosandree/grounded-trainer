mod support;
use gt_core::mem::{MemReader, MemWriter};

#[test]
fn reads_marker() {
    let t = support::TestTarget::spawn();
    let mut buf = [0u8; 32];
    MemReader::new(t.pid)
        .read_exact(t.marker, &mut buf)
        .unwrap();
    assert_eq!(buf, support::marker_bytes());
}
#[test]
fn scan_finds_marker_once_in_its_region() {
    let t = support::TestTarget::spawn();
    let maps = gt_core::maps::read_maps(t.pid).unwrap();
    let region = maps
        .iter()
        .find(|r| r.start <= t.marker && t.marker < r.end)
        .unwrap();
    let pat =
        gt_core::scan::Pattern::parse(&support::hex_with_spaces(&support::marker_bytes())).unwrap();
    let reader = MemReader::new(t.pid);
    let hits = gt_core::scan::scan_chunked(region.end - region.start, 1 << 20, &pat, |off, buf| {
        reader.read_exact(region.start + off, buf)
    })
    .unwrap();
    assert_eq!(hits, vec![t.marker - region.start]);
}
#[test]
fn raw_writer_changes_marker() {
    let t = support::TestTarget::spawn();
    MemWriter::open(t.pid)
        .unwrap()
        .write(t.marker, &[0x11; 4])
        .unwrap();
    let mut buf = [0u8; 4];
    MemReader::new(t.pid)
        .read_exact(t.marker, &mut buf)
        .unwrap();
    assert_eq!(buf, [0x11; 4]);
}
#[test]
fn lossy_read_over_hole_marks_only_the_hole() {
    let t = support::TestTarget::spawn();
    let r = MemReader::new(t.pid)
        .read_lossy(t.region_start, 3 * 4096)
        .unwrap();
    assert_eq!(r.holes, vec![t.region_start + 4096..t.region_start + 8192]);
    assert_eq!(r.bytes.len(), 3 * 4096);
}
#[test]
fn fully_unmapped_is_error() {
    let t = support::TestTarget::spawn();
    assert!(matches!(
        MemReader::new(t.pid).read_lossy(t.region_start + 4096, 4096),
        Err(gt_core::Error::Unmapped { .. })
    ));
}
#[test]
fn read_past_u64_max_is_unmapped() {
    let t = support::TestTarget::spawn();
    assert!(matches!(
        MemReader::new(t.pid).read_lossy(u64::MAX - 15, 32),
        Err(gt_core::Error::Unmapped { .. })
    ));
}
#[test]
fn dead_process_is_gone_for_lossy_too() {
    let mut t = support::TestTarget::spawn();
    let pid = t.pid;
    t.child.kill().unwrap();
    t.child.wait().unwrap();
    assert!(matches!(
        MemReader::new(pid).read_lossy(t.marker, 32),
        Err(gt_core::Error::ProcessGone { .. })
    ));
}
#[test]
fn dead_process_is_gone() {
    let mut t = support::TestTarget::spawn();
    let pid = t.pid;
    t.child.kill().unwrap();
    t.child.wait().unwrap();
    let mut buf = [0u8; 1];
    assert!(matches!(
        MemReader::new(pid).read_exact(0x1000, &mut buf),
        Err(gt_core::Error::ProcessGone { .. })
    ));
}
