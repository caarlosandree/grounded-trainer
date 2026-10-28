//! Política de escrita do jogo (spec 002, 6.6), calculada de forma pura.

use std::ops::Range;

use gt_core::cave::{CaveAllocator, cc_runs};
use gt_core::maps::MapRegion;
use gt_core::modules::Module;
use gt_core::patch::WritePolicy;
use gt_core::pe::{DiskImage, PeHeaders};

/// Seções executáveis do `disk` (cabeçalho do arquivo, não o da memória), em endereços do
/// processo e cortadas na extensão do módulo.
pub fn exec_ranges(module: &Module, disk: &PeHeaders) -> Vec<Range<u64>> {
    let limit = module.end();
    disk.sections
        .iter()
        .filter(|section| section.is_executable())
        .filter_map(|section| {
            let start = module.base.checked_add(u64::from(section.rva))?;
            let end = start
                .checked_add(u64::from(section.virtual_size))?
                .min(limit);
            (start < end).then_some(start..end)
        })
        .collect()
}

/// Intervalos onde o motor pode escrever: [`exec_ranges`] ∩ regiões executáveis do `maps`.
///
/// Módulo não confiável: política vazia (escrever é mais rígido que ler).
pub fn write_policy(module: &Module, disk: &PeHeaders, maps: &[MapRegion]) -> WritePolicy {
    if !module.trusted {
        return WritePolicy::new(Vec::new());
    }
    let sections = exec_ranges(module, disk);
    let allowed = maps
        .iter()
        .filter(|region| region.perms.exec)
        .flat_map(|region| {
            sections.iter().filter_map(move |section| {
                let start = section.start.max(region.start);
                let end = section.end.min(region.end);
                (start < end).then_some(start..end)
            })
        })
        .collect();
    WritePolicy::new(allowed)
}

/// Tamanho da página que fecha a folga da `.data` (a do carregador PE).
const PAGE: u64 = 0x1000;

/// Folga da `.data` para as variáveis das caves (spec 008, 6.4): do fim do `VirtualSize` da
/// `.data` (arredondado para cima a 8) até o fim da página. `None` se o módulo não é confiável,
/// não há `.data`, não sobra folga ou o trecho não está **inteiro** em regiões `rw-` privadas
/// do `maps` dentro de `base..base + SizeOfImage` (a parte mapeada do arquivo e a cauda
/// anônima valem; a cauda é uma VMA própria no jogo). Se o trecho tem bytes diferentes de
/// zero, quem reclama a região ([`gt_core::engine::Engine::claim_vars`]) recusa.
pub fn var_region(module: &Module, disk: &PeHeaders, maps: &[MapRegion]) -> Option<Range<u64>> {
    if !module.trusted {
        return None;
    }
    let data = disk.section(".data")?;
    let data_end = module
        .base
        .checked_add(u64::from(data.rva))?
        .checked_add(u64::from(data.virtual_size))?;
    let start = data_end.checked_add(7)? & !7;
    let end = data_end.checked_add(PAGE - 1)? & !(PAGE - 1);
    let image_end = module.base.checked_add(u64::from(disk.size_of_image))?;
    if start >= end || end > image_end {
        return None;
    }
    let rw = WritePolicy::new(
        maps.iter()
            .filter(|r| r.perms.read && r.perms.write && !r.perms.exec && !r.perms.shared)
            .map(|r| r.start..r.end)
            .collect(),
    );
    let region = start..end;
    rw.allows(&region).then_some(region)
}

/// Tamanho mínimo, em bytes, das sequências de `CC` que servem para caves (spec 008, 6.4): a
/// menor cave é a de observação (~70 bytes) e a margem de 16 fica por conta do alocador; sequências
/// menores só atrapalhariam as listagens.
pub const CAVE_MIN_RUN: usize = 64;

/// Sequências de `CC` de pelo menos `min_len` bytes na `.text` do **disco** (`disk`), em
/// endereços do processo. `None` se a imagem não tem `.text` ou ela sai dos limites do arquivo.
///
/// O disco é a fonte porque a memória pode já ter caves nossas ou de outros nas sequências: o que
/// está em uso se descobre lendo a memória por cima desta lista.
pub fn text_cc_runs(module: &Module, disk: &DiskImage, min_len: usize) -> Option<Vec<Range<u64>>> {
    let text = disk.headers.section(".text")?;
    let bytes = disk.bytes(text.rva, usize::try_from(text.virtual_size).ok()?)?;
    let start = module.base.checked_add(u64::from(text.rva))?;
    Some(cc_runs(bytes, start, min_len))
}

/// Alocador de caves sobre as sequências de [`CAVE_MIN_RUN`] bytes ou mais da `.text` do disco.
pub fn cave_allocator(module: &Module, disk: &DiskImage) -> Option<CaveAllocator> {
    text_cc_runs(module, disk, CAVE_MIN_RUN).map(CaveAllocator::new)
}

/// Onde o motor pode escrever em dados (spec 005, 6.4): regiões `rw-` do `maps` (sem execução),
/// menos as imagens de módulo (`images`, de `base` a `end` de cada módulo). Os objetos do jogo
/// moram no heap; a `.data` dos módulos nunca é escrita por um cheat de valor.
pub fn data_policy(images: &[Range<u64>], maps: &[MapRegion]) -> WritePolicy {
    let mut allowed: Vec<Range<u64>> = Vec::new();
    for region in maps
        .iter()
        .filter(|r| r.perms.read && r.perms.write && !r.perms.exec)
    {
        // A região menos cada imagem que a corta.
        let mut pieces: Vec<Range<u64>> = std::iter::once(region.start..region.end).collect();
        for image in images {
            pieces = pieces
                .into_iter()
                .flat_map(|p| {
                    let left = p.start..p.end.min(image.start);
                    let right = p.start.max(image.end)..p.end;
                    [left, right].into_iter().filter(|r| r.start < r.end)
                })
                .collect();
        }
        allowed.extend(pieces);
    }
    WritePolicy::new(allowed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gt_core::maps::{MapPath, Perms};
    use gt_core::pe::Section;

    const BASE: u64 = 0x1_4000_0000;

    fn section(name: &str, rva: u32, vsize: u32, characteristics: u32) -> Section {
        Section {
            name: name.into(),
            rva,
            virtual_size: vsize,
            raw_size: vsize,
            raw_offset: 0x400,
            characteristics,
        }
    }

    /// `.text` r-x em `0x1000..0x3000` e `.data` rw- em `0x3000..0x4000`.
    fn disk() -> PeHeaders {
        PeHeaders {
            image_base: BASE,
            size_of_image: 0x5000,
            time_date_stamp: 1,
            size_of_headers: 0x400,
            sections: vec![
                section(".text", 0x1000, 0x2000, 0x6000_0020),
                section(".data", 0x3000, 0x1000, 0xC000_0040),
            ],
        }
    }

    fn module(trusted: bool, size: u64) -> Module {
        Module {
            name: "game.exe".into(),
            path: "/game/game.exe".into(),
            base: BASE,
            size,
            memory_size: 0x5000,
            disk_size: Some(0x5000),
            trusted,
        }
    }

    fn region(start: u64, end: u64, perms: &str) -> MapRegion {
        MapRegion {
            start,
            end,
            perms: Perms {
                read: perms.contains('r'),
                write: perms.contains('w'),
                exec: perms.contains('x'),
                shared: false,
            },
            offset: 0,
            path: MapPath::Anonymous,
        }
    }

    fn maps() -> Vec<MapRegion> {
        vec![
            region(BASE, BASE + 0x1000, "r--"),
            region(BASE + 0x1000, BASE + 0x3000, "r-x"),
            region(BASE + 0x3000, BASE + 0x4000, "rw-"),
        ]
    }

    #[test]
    fn exec_section_allowed_data_refused() {
        let policy = write_policy(&module(true, 0x5000), &disk(), &maps());
        assert!(policy.allows(&(BASE + 0x1000..BASE + 0x1004)));
        assert!(policy.allows(&(BASE + 0x2ffc..BASE + 0x3000)));
        // `.data` (gravável, não executável) e os cabeçalhos ficam fora.
        assert!(!policy.allows(&(BASE + 0x3000..BASE + 0x3004)));
        assert!(!policy.allows(&(BASE..BASE + 4)));
    }

    #[test]
    fn range_crossing_section_end_refused() {
        let policy = write_policy(&module(true, 0x5000), &disk(), &maps());
        assert!(!policy.allows(&(BASE + 0x2ffe..BASE + 0x3002)));
        assert!(!policy.allows(&(BASE + 0xffe..BASE + 0x1002)));
    }

    #[test]
    fn exec_ranges_are_clipped_to_the_module() {
        // Módulo menor que a seção: a extensão corta a `.text`.
        let ranges = exec_ranges(&module(true, 0x2000), &disk());
        assert_eq!(ranges, vec![BASE + 0x1000..BASE + 0x2000]);
        // Módulo que acaba antes da seção: nada sobra.
        assert!(exec_ranges(&module(true, 0x800), &disk()).is_empty());
    }

    #[test]
    fn untrusted_module_has_empty_policy() {
        let policy = write_policy(&module(false, 0x5000), &disk(), &maps());
        assert!(!policy.allows(&(BASE + 0x1000..BASE + 0x1004)));
    }

    #[test]
    fn policy_intersects_maps() {
        // Seção executável no disco, mas a região no `maps` é `r--`: recusada.
        let no_exec = vec![region(BASE + 0x1000, BASE + 0x3000, "r--")];
        let policy = write_policy(&module(true, 0x5000), &disk(), &no_exec);
        assert!(!policy.allows(&(BASE + 0x1000..BASE + 0x1004)));

        // Só a primeira metade da seção é executável no `maps`.
        let half = vec![
            region(BASE + 0x1000, BASE + 0x2000, "r-x"),
            region(BASE + 0x2000, BASE + 0x3000, "r--"),
        ];
        let policy = write_policy(&module(true, 0x5000), &disk(), &half);
        assert!(policy.allows(&(BASE + 0x1ff0..BASE + 0x2000)));
        assert!(!policy.allows(&(BASE + 0x2000..BASE + 0x2004)));
        assert!(!policy.allows(&(BASE + 0x1ffe..BASE + 0x2002)));

        // Regiões executáveis vizinhas (VMAs partidas) não recusam o patch que cruza a emenda.
        let split = vec![
            region(BASE + 0x1000, BASE + 0x2000, "r-x"),
            region(BASE + 0x2000, BASE + 0x3000, "r-x"),
        ];
        let policy = write_policy(&module(true, 0x5000), &disk(), &split);
        assert!(policy.allows(&(BASE + 0x1ffe..BASE + 0x2002)));
    }

    /// `.data` em `0x3000` com `VirtualSize` `0xed3`: termina em `0x3ed3`, e a folga vai de
    /// `0x3ed8` (arredondado a 8) até o fim da página, `0x4000`.
    fn disk_with_gap() -> PeHeaders {
        PeHeaders {
            sections: vec![
                section(".text", 0x1000, 0x2000, 0x6000_0020),
                section(".data", 0x3000, 0xed3, 0xC000_0040),
            ],
            ..disk()
        }
    }

    #[test]
    fn var_region_aligns_and_accepts_anonymous_tail() {
        // Cabeça de arquivo `0x3000..0x3800` e cauda anônima `0x3800..0x4000`.
        let head = MapRegion {
            path: MapPath::File {
                path: "/game/game.exe".into(),
                deleted: false,
            },
            ..region(BASE + 0x3000, BASE + 0x3800, "rw-")
        };
        let maps = vec![
            region(BASE + 0x1000, BASE + 0x3000, "r-x"),
            head,
            region(BASE + 0x3800, BASE + 0x4000, "rw-"),
        ];
        let got = var_region(&module(true, 0x5000), &disk_with_gap(), &maps);
        assert_eq!(got, Some(BASE + 0x3ed8..BASE + 0x4000));
        // Tudo numa VMA só (anônima) também vale.
        let one = vec![region(BASE + 0x3000, BASE + 0x4000, "rw-")];
        assert_eq!(
            var_region(&module(true, 0x5000), &disk_with_gap(), &one),
            Some(BASE + 0x3ed8..BASE + 0x4000)
        );
        // `.data` que termina na fronteira da página: sem folga.
        assert_eq!(var_region(&module(true, 0x5000), &disk(), &maps), None);
    }

    #[test]
    fn var_region_none_when_not_rw() {
        let m = module(true, 0x5000);
        let d = disk_with_gap();
        let good = vec![region(BASE + 0x3000, BASE + 0x4000, "rw-")];
        assert_eq!(
            var_region(&m, &d, &good),
            Some(BASE + 0x3ed8..BASE + 0x4000)
        );
        // Só leitura, rwx e compartilhada não servem.
        for perms in ["r--", "rwx", "r-x"] {
            let maps = vec![region(BASE + 0x3000, BASE + 0x4000, perms)];
            assert_eq!(var_region(&m, &d, &maps), None, "{perms}");
        }
        let mut shared = region(BASE + 0x3000, BASE + 0x4000, "rw-");
        shared.perms.shared = true;
        assert_eq!(var_region(&m, &d, &[shared]), None);
        // Parte da folga sem região `rw-` (buraco no `maps`).
        let hole = vec![region(BASE + 0x3000, BASE + 0x3f00, "rw-")];
        assert_eq!(var_region(&m, &d, &hole), None);
        // Duas regiões `rw-` que não se tocam deixam um buraco.
        let gap = vec![
            region(BASE + 0x3000, BASE + 0x3f00, "rw-"),
            region(BASE + 0x3f80, BASE + 0x4000, "rw-"),
        ];
        assert_eq!(var_region(&m, &d, &gap), None);
        // Módulo de terceiros, sem `.data` ou com a página fora da imagem.
        let ok = vec![region(BASE + 0x3000, BASE + 0x4000, "rw-")];
        assert_eq!(var_region(&module(false, 0x5000), &d, &ok), None);
        let no_data = PeHeaders {
            sections: vec![section(".text", 0x1000, 0x2000, 0x6000_0020)],
            ..disk()
        };
        assert_eq!(var_region(&m, &no_data, &ok), None);
        let small = PeHeaders {
            size_of_image: 0x3f00,
            ..disk_with_gap()
        };
        assert_eq!(var_region(&m, &small, &ok), None);
    }

    #[test]
    fn data_policy_is_rw_heap_outside_images() {
        let maps = vec![
            region(0x1000, 0x2000, "rw-p"),
            region(0x2000, 0x3000, "r-xp"),
            region(0x3000, 0x6000, "rw-p"),
            region(0x6000, 0x7000, "r--p"),
            region(0x7000, 0x8000, "rwxp"),
        ];
        // Uma imagem de módulo no meio da segunda região rw.
        let image: Vec<Range<u64>> = std::iter::once(0x4000..0x5000).collect();
        let p = data_policy(&image, &maps);
        assert!(p.allows(&(0x1000..0x1004)));
        assert!(p.allows(&(0x3ffc..0x4000)));
        assert!(!p.allows(&(0x3ffe..0x4002)));
        assert!(!p.allows(&(0x4000..0x4001)));
        assert!(p.allows(&(0x5000..0x5001)));
        // Executável, só leitura e rwx ficam de fora.
        assert!(!p.allows(&(0x2000..0x2001)));
        assert!(!p.allows(&(0x6000..0x6001)));
        assert!(!p.allows(&(0x7000..0x7001)));
        // Sem imagens, a região inteira vale.
        assert!(data_policy(&[], &maps).allows(&(0x3000..0x6000)));
    }
}
