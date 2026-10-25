//! Lógica comum de `diff` e `fling`: trechos de diferença entre a memória e o disco, destino de
//! desvios e a formatação das desmontagens.
//!
//! Tudo que não precisa do processo vivo é função pura e fica testado aqui.

use std::ops::Range;

use gt_core::asm::{Branch, Insn, branch_at};
use gt_core::diff::group;
use gt_core::guard;
use gt_core::mem::LossyRead;
use gt_core::modules::Module;
use serde::Serialize;

use crate::context::Target;

/// Distância máxima entre duas diferenças do mesmo trecho (spec 6.7).
pub const GROUP_GAP: usize = 16;

/// Bytes que `branch_at` examina no início do trecho (maior instrução x86).
const BRANCH_LOOKAHEAD: usize = 15;

/// Um trecho de diferença entre a memória e o disco.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// Endereço absoluto do início do trecho.
    pub addr: u64,
    /// Bytes do disco (já realocados) no trecho. O tamanho do trecho é `original.len()`.
    pub original: Vec<u8>,
    /// Bytes da memória no trecho; vazio num trecho que é só lacuna ilegível.
    pub current: Vec<u8>,
    /// O trecho toca uma lacuna ilegível da memória: os bytes atuais não são confiáveis.
    pub unreadable: bool,
    /// Desvio na primeira instrução do trecho atual.
    pub branch: Option<Branch>,
}

impl Chunk {
    /// Tamanho do trecho em bytes.
    pub fn len(&self) -> usize {
        self.original.len()
    }

    /// Fim exclusivo do trecho.
    pub fn end(&self) -> u64 {
        self.addr + self.original.len() as u64
    }

    /// Trecho que é só lacuna: nada foi lido da memória.
    fn is_pure_hole(&self) -> bool {
        self.unreadable && self.current.is_empty()
    }
}

/// Trecho em construção: intervalo relativo a `start`, com a origem dele.
struct Span {
    range: Range<usize>,
    /// Toca alguma lacuna.
    hole: bool,
    /// É só lacuna (nenhuma diferença legível dentro).
    pure_hole: bool,
}

/// Agrupa as diferenças entre `mem` e `disk` (os dois começam em `start`).
///
/// - Diferenças a menos de 16 bytes uma da outra viram um trecho (`diff::group`).
/// - `holes` (intervalos absolutos ilegíveis da memória) não geram diferença por si: os zeros
///   que a leitura pôs no lugar não são memória do jogo. Cada lacuna vira um trecho próprio,
///   `unreadable = true`, com `original` do disco e `current` vazio, mesmo que o disco também
///   seja zero ali.
/// - Um trecho de diferença que se sobrepõe a uma lacuna funde-se com ela e fica `unreadable`;
///   `current` traz os bytes lidos (zerados na lacuna).
/// - `branch` vem de `branch_at` no início do trecho atual, sem passar de uma lacuna.
///
/// Se `mem` e `disk` tiverem tamanhos diferentes, só o prefixo comum é comparado.
pub fn build_chunks(mem: &[u8], holes: &[Range<u64>], disk: &[u8], start: u64) -> Vec<Chunk> {
    let n = mem.len().min(disk.len());
    let (mem, disk) = (&mem[..n], &disk[..n]);

    // Lacunas em índices relativos, cortadas ao bloco, ordenadas e sem sobreposição.
    let mut rel_holes: Vec<Range<usize>> = holes
        .iter()
        .filter_map(|hole| {
            let from = hole.start.max(start).saturating_sub(start);
            let to = hole
                .end
                .min(start.saturating_add(n as u64))
                .saturating_sub(start);
            // Cabe em `usize`: `to <= n`.
            (from < to).then_some(from as usize..to as usize)
        })
        .collect();
    rel_holes.sort_by_key(|hole| hole.start);
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(rel_holes.len());
    for hole in rel_holes {
        match merged.last_mut() {
            Some(last) if hole.start <= last.end => last.end = last.end.max(hole.end),
            _ => merged.push(hole),
        }
    }
    let rel_holes = merged;

    let diffs = if rel_holes.is_empty() {
        group(mem, disk, GROUP_GAP)
    } else {
        let mut masked = mem.to_vec();
        for hole in &rel_holes {
            masked[hole.clone()].copy_from_slice(&disk[hole.clone()]);
        }
        group(&masked, disk, GROUP_GAP)
    };

    let mut spans: Vec<Span> = diffs
        .into_iter()
        .map(|range| Span {
            range,
            hole: false,
            pure_hole: false,
        })
        .chain(rel_holes.iter().map(|hole| Span {
            range: hole.clone(),
            hole: true,
            pure_hole: true,
        }))
        .collect();
    spans.sort_by_key(|span| (span.range.start, span.range.end));

    // Funde o que se sobrepõe (diferença que cruza lacuna).
    let mut fused: Vec<Span> = Vec::with_capacity(spans.len());
    for span in spans {
        match fused.last_mut() {
            Some(last) if span.range.start < last.range.end => {
                last.range.end = last.range.end.max(span.range.end);
                last.hole |= span.hole;
                last.pure_hole &= span.pure_hole;
            }
            _ => fused.push(span),
        }
    }

    fused
        .into_iter()
        .map(|span| {
            let Range {
                start: from,
                end: to,
            } = span.range;
            let current = if span.pure_hole {
                Vec::new()
            } else {
                mem[from..to].to_vec()
            };
            let branch = if span.pure_hole {
                None
            } else {
                branch_in_memory(mem, &rel_holes, from, start)
            };
            Chunk {
                addr: start + from as u64,
                original: disk[from..to].to_vec(),
                current,
                unreadable: span.hole,
                branch,
            }
        })
        .collect()
}

/// `branch_at` no índice `from` da memória, sem ler além do fim do bloco nem de uma lacuna.
fn branch_in_memory(mem: &[u8], holes: &[Range<usize>], from: usize, start: u64) -> Option<Branch> {
    let mut limit = from.saturating_add(BRANCH_LOOKAHEAD).min(mem.len());
    if let Some(hole) = holes.iter().find(|hole| hole.end > from) {
        if hole.start <= from {
            return None;
        }
        limit = limit.min(hole.start);
    }
    branch_at(&mem[from..limit], start + from as u64)
}

/// Compara a seção inteira em blocos de `block` bytes, sem carregar a memória toda.
///
/// `disk` é a seção em disco e `start` o endereço dela. `read(addr, len)` lê `len` bytes da
/// memória, com lacunas. O resultado é o mesmo de `build_chunks` sobre a seção inteira: um trecho
/// que toca o fim de um bloco (a menos de [`GROUP_GAP`] bytes) é adiado e relido a partir do
/// início dele no bloco seguinte, para que a fronteira nunca o parta em dois; lacunas contíguas
/// entre blocos viram uma só.
pub fn compare_section<F>(
    start: u64,
    disk: &[u8],
    block: usize,
    mut read: F,
) -> gt_core::Result<Vec<Chunk>>
where
    F: FnMut(u64, usize) -> gt_core::Result<LossyRead>,
{
    let block = block.max(GROUP_GAP * 2);
    let total = disk.len();
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut pos = 0usize;
    let mut len = block;
    while pos < total {
        let len_now = len.min(total - pos);
        let read_back = read(start + pos as u64, len_now)?;
        let mut part = build_chunks(
            &read_back.bytes,
            &read_back.holes,
            &disk[pos..pos + len_now],
            start + pos as u64,
        );
        let mut next = pos + len_now;
        if next < total {
            let open = part.last().is_some_and(|last| {
                let end_rel = (last.end() - (start + pos as u64)) as usize;
                !last.is_pure_hole() && end_rel + GROUP_GAP > len_now
            });
            if open {
                let last_rel = part
                    .last()
                    .map_or(0, |last| (last.addr - start) as usize - pos);
                if last_rel == 0 {
                    // O trecho ocupa o bloco todo: sem progresso possível, aumenta o bloco.
                    len = len_now.saturating_mul(2);
                    continue;
                }
                part.pop();
                next = pos + last_rel;
            }
        }
        len = block;
        for chunk in part {
            match chunks.last_mut() {
                Some(prev)
                    if prev.is_pure_hole() && chunk.is_pure_hole() && prev.end() == chunk.addr =>
                {
                    prev.original.extend_from_slice(&chunk.original);
                }
                _ => chunks.push(chunk),
            }
        }
        pos = next;
    }
    Ok(chunks)
}

/// Onde cai o destino de um desvio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    /// Dentro de um módulo confiável (nome do módulo).
    Trusted(String),
    /// Dentro de um módulo de terceiros (nome do módulo).
    ThirdPartyModule(String),
    /// Fora de todos os módulos: provável código injetado.
    Outside,
    /// O destino não pôde ser lido (`jmp [rip+x]` com o ponteiro fora de módulo confiável).
    Unresolved,
}

/// Classifica o endereço `target` pelo módulo que o contém.
pub fn destination_for(modules: &[Module], target: u64) -> Destination {
    match modules.iter().find(|module| module.contains(target)) {
        Some(module) if module.trusted => Destination::Trusted(module.name.clone()),
        Some(module) => Destination::ThirdPartyModule(module.name.clone()),
        None => Destination::Outside,
    }
}

/// Resolve o destino de `branch` e o classifica. Nunca lê bytes no destino.
///
/// `RipIndirect` lê o ponteiro (8 bytes) só se o slot estiver inteiro num módulo `trusted` e a
/// guarda deixar; senão (ou se a leitura falhar) o destino é `Unresolved` e o endereço, `None`.
pub fn classify_branch(ctx: &Target, branch: Branch) -> (Option<u64>, Destination) {
    match branch {
        Branch::Direct(target) => (Some(target), destination_for(&ctx.modules, target)),
        Branch::RipIndirect { slot } => {
            let slot_trusted =
                matches!(destination_for(&ctx.modules, slot), Destination::Trusted(_))
                    && matches!(
                        destination_for(&ctx.modules, slot.saturating_add(7)),
                        Destination::Trusted(_)
                    );
            if !slot_trusted || guard::check(&ctx.maps, &ctx.modules, &ctx.non_pe, slot, 8).is_err()
            {
                return (None, Destination::Unresolved);
            }
            let mut pointer = [0u8; 8];
            match ctx.reader.read_exact(slot, &mut pointer) {
                Ok(()) => {
                    let target = u64::from_le_bytes(pointer);
                    (Some(target), destination_for(&ctx.modules, target))
                }
                Err(_) => (None, Destination::Unresolved),
            }
        }
    }
}

/// Texto do destino para a saída legível.
pub fn destination_text(destination: &Destination) -> String {
    match destination {
        Destination::Trusted(name) => format!("módulo confiável {name}"),
        Destination::ThirdPartyModule(name) => format!("módulo de terceiros {name}"),
        Destination::Outside => "fora de todos os módulos (provável código injetado)".to_string(),
        Destination::Unresolved => "não resolvido".to_string(),
    }
}

/// Desvio na saída `--json`: `target` é `null` se não resolvido; `destination` é `trusted`,
/// `third_party_module`, `outside` ou `unresolved`; `module` só existe nos dois primeiros.
#[derive(Debug, Serialize)]
pub struct BranchOutput {
    pub target: Option<String>,
    pub destination: &'static str,
    pub module: Option<String>,
}

/// Monta o desvio classificado de `branch`.
pub fn branch_output(ctx: &Target, branch: Branch) -> (BranchOutput, String) {
    let (target, destination) = classify_branch(ctx, branch);
    let (kind, module) = match &destination {
        Destination::Trusted(name) => ("trusted", Some(name.clone())),
        Destination::ThirdPartyModule(name) => ("third_party_module", Some(name.clone())),
        Destination::Outside => ("outside", None),
        Destination::Unresolved => ("unresolved", None),
    };
    let text = match target {
        Some(addr) => format!("{} → {}", ctx.label(addr), destination_text(&destination)),
        None => destination_text(&destination),
    };
    (
        BranchOutput {
            target: target.map(|t| format!("{t:#x}")),
            destination: kind,
            module,
        },
        text,
    )
}

/// Uma instrução desmontada na saída `--json`.
#[derive(Debug, Serialize)]
pub struct AsmLine {
    pub address: String,
    pub bytes_hex: String,
    pub text: String,
}

/// Converte instruções para a saída `--json`.
pub fn asm_lines(insns: &[Insn]) -> Vec<AsmLine> {
    insns
        .iter()
        .map(|insn| AsmLine {
            address: format!("{:#x}", insn.addr),
            bytes_hex: hex(&insn.bytes),
            text: insn.text.clone(),
        })
        .collect()
}

/// Hexadecimal minúsculo sem separadores.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Hexadecimal minúsculo com espaço entre os bytes.
pub fn hex_spaced(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    fn game_module() -> Module {
        Module {
            name: "Grounded2Steam-Win64-Shipping.exe".into(),
            base: 0x140000000,
            size: 0xb27c000,
            trusted: true,
            ..Default::default()
        }
    }

    fn hex_in(text: &str) -> Vec<u8> {
        text.split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect()
    }

    #[test]
    fn fling_charges_patch_is_one_chunk_outside() {
        // RESEARCH.md, +57BD6D0: disco f3 0f 10 89 74 09 00 00 × memória e9 4b 29 ac 86 0f 1f 00
        let (disk, mem) = (
            hex_in("f3 0f 10 89 74 09 00 00"),
            hex_in("e9 4b 29 ac 86 0f 1f 00"),
        );
        let chunks = build_chunks(&mem, &[], &disk, 0x1457BD6D0);
        assert_eq!(chunks.len(), 1);
        // 7, não 8: o último byte do `nop` do FLiNG (00) coincide com o original
        assert_eq!((chunks[0].addr, chunks[0].current.len()), (0x1457BD6D0, 7));
        assert!(matches!(chunks[0].branch, Some(Branch::Direct(0xCC280020))));
        assert!(!chunks[0].unreadable);
        assert!(matches!(
            destination_for(&[game_module()], 0xCC280020),
            Destination::Outside
        ));
    }

    #[test]
    fn two_far_patches_are_two_chunks() {
        let disk = vec![0x90u8; 0x100];
        let mut mem = disk.clone();
        mem[0x10] = 0xcc;
        mem[0x50] = 0xcc;
        let chunks = build_chunks(&mem, &[], &disk, 0x1000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].addr, 0x1010);
        assert_eq!(chunks[1].addr, 0x1050);
    }

    #[test]
    fn identical_is_empty() {
        let a = vec![0x90u8; 64];
        assert!(build_chunks(&a, &[], &a, 0x1000).is_empty());
    }

    #[test]
    fn hole_marks_unreadable() {
        let disk = vec![0u8; 0x100];
        let mut mem = disk.clone();
        mem[0x20..0x28].fill(0xe9);
        // A lacuna cobre a diferença: os zeros lidos não contam como memória do jogo.
        let chunks = build_chunks(&mem, &[0x1018..0x1030], &disk, 0x1000);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].unreadable);
        assert_eq!(chunks[0].addr, 0x1018);
        assert_eq!(chunks[0].original.len(), 0x18);
        assert!(chunks[0].current.is_empty());
        assert!(chunks[0].branch.is_none());
    }

    #[test]
    fn hole_is_a_chunk_even_if_disk_is_zero_too() {
        let zeros = vec![0u8; 0x40];
        let chunks = build_chunks(&zeros, &[0x2010..0x2020], &zeros, 0x2000);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].unreadable);
        assert_eq!((chunks[0].addr, chunks[0].end()), (0x2010, 0x2020));
    }

    #[test]
    fn hole_outside_the_block_is_clipped() {
        let a = vec![0u8; 0x40];
        let chunks = build_chunks(&a, &[0x1ff0..0x2008, 0x2038..0x3000], &a, 0x2000);
        let ranges: Vec<_> = chunks.iter().map(|c| (c.addr, c.end())).collect();
        assert_eq!(ranges, vec![(0x2000, 0x2008), (0x2038, 0x2040)]);
    }

    #[test]
    fn diff_crossing_a_hole_is_fused_and_keeps_branch() {
        let disk = vec![0x90u8; 0x40];
        let mut mem = disk.clone();
        mem[0x10..0x15].copy_from_slice(&hex_in("e9 4b 29 ac 86"));
        mem[0x1a] = 0xcc;
        // A lacuna 0x16..0x18 fica entre as duas diferenças (a menos de 16 bytes uma da outra):
        // o trecho cruza a lacuna, funde-se com ela e fica marcado.
        let chunks = build_chunks(&mem, &[0x1016..0x1018], &disk, 0x1000);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].unreadable);
        assert_eq!((chunks[0].addr, chunks[0].end()), (0x1010, 0x101b));
        assert_eq!(chunks[0].current.len(), 0x0b);
        assert!(matches!(chunks[0].branch, Some(Branch::Direct(_))));
    }

    #[test]
    fn branch_does_not_read_past_a_hole() {
        let disk = vec![0x90u8; 0x40];
        let mut mem = disk.clone();
        mem[0x10..0x15].copy_from_slice(&hex_in("e9 4b 29 ac 86"));
        mem[0x1a] = 0xcc;
        // A lacuna começa em 0x14: o `jmp rel32` teria o último byte zerado pela leitura.
        let chunks = build_chunks(&mem, &[0x1014..0x1018], &disk, 0x1000);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].unreadable);
        assert!(chunks[0].branch.is_none());
    }

    #[test]
    fn destination_kinds() {
        let third = Module {
            name: "t.dll".into(),
            base: 0x7ff000000000,
            size: 0x1000,
            trusted: false,
            ..Default::default()
        };
        let modules = [game_module(), third];
        assert!(matches!(
            destination_for(&modules, 0x140001000),
            Destination::Trusted(ref n) if n == "Grounded2Steam-Win64-Shipping.exe"
        ));
        assert!(matches!(
            destination_for(&modules, 0x7ff000000010),
            Destination::ThirdPartyModule(ref n) if n == "t.dll"
        ));
        assert!(matches!(
            destination_for(&modules, 0x1),
            Destination::Outside
        ));
    }

    /// Leitor falso: devolve `mem` com as lacunas zeradas.
    fn fake_reader<'a>(
        start: u64,
        mem: &'a [u8],
        holes: &'a [Range<u64>],
    ) -> impl FnMut(u64, usize) -> gt_core::Result<LossyRead> + 'a {
        move |addr, len| {
            let from = (addr - start) as usize;
            let mut bytes = mem[from..from + len].to_vec();
            let mut found = Vec::new();
            for hole in holes {
                let lo = hole.start.max(addr);
                let hi = hole.end.min(addr + len as u64);
                if lo < hi {
                    bytes[(lo - addr) as usize..(hi - addr) as usize].fill(0);
                    found.push(lo..hi);
                }
            }
            Ok(LossyRead {
                bytes,
                holes: found,
            })
        }
    }

    #[test]
    fn blocks_give_the_same_chunks_as_the_whole_section() {
        let start = 0x10000u64;
        let disk: Vec<u8> = (0..0x400u32).map(|i| (i % 251) as u8).collect();
        let mut mem = disk.clone();
        // Trechos em posições que caem em cima de várias fronteiras de bloco (64 bytes).
        for i in [
            0x3c, 0x3d, 0x45, 0x7e, 0x80, 0x90, 0xbf, 0xc0, 0x13f, 0x300, 0x3ff,
        ] {
            mem[i] ^= 0xff;
        }
        let holes = [0x10100u64..0x10120, 0x10200..0x10300];
        let whole = {
            let mut read = fake_reader(start, &mem, &holes);
            let r = read(start, mem.len()).unwrap();
            build_chunks(&r.bytes, &r.holes, &disk, start)
        };
        for block in [32usize, 33, 64, 100, 0x400, 0x1000] {
            let blocks =
                compare_section(start, &disk, block, fake_reader(start, &mem, &holes)).unwrap();
            assert_eq!(blocks, whole, "bloco de {block} bytes");
        }
    }

    #[test]
    fn a_chunk_larger_than_the_block_is_not_split() {
        let start = 0x2000u64;
        let disk = vec![0u8; 0x200];
        let mut mem = disk.clone();
        // Uma diferença a cada 8 bytes: um único trecho de 0x1f9 bytes.
        for i in (1..0x200).step_by(8) {
            mem[i] = 1;
        }
        let chunks = compare_section(start, &disk, 64, fake_reader(start, &mem, &[])).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 0x1f9);
    }

    #[test]
    fn identical_section_in_blocks_is_empty() {
        let disk = vec![7u8; 0x300];
        let chunks = compare_section(0x1000, &disk, 64, fake_reader(0x1000, &disk, &[])).unwrap();
        assert!(chunks.is_empty());
    }
}
