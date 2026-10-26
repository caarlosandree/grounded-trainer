//! Política de escrita do jogo (spec 002, 6.6), calculada de forma pura.

use std::ops::Range;

use gt_core::maps::MapRegion;
use gt_core::modules::Module;
use gt_core::patch::WritePolicy;
use gt_core::pe::PeHeaders;

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
