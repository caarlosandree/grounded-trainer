//! Guarda contra código de terceiros (spec 6.12).
//!
//! Garantia do projeto: o `gt` nunca exibe nem desmonta código injetado por outros trainers.
//! A guarda avalia intervalos de bytes, não regiões inteiras do `maps`, porque o kernel funde
//! regiões vizinhas com as mesmas permissões.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::maps::{MapPath, MapRegion};
use crate::modules::Module;

/// Verifica se `[addr, addr + len)` toca código de terceiros.
///
/// Código de terceiros são bytes de região executável (`x`) que:
/// - estão dentro de um módulo não confiável; ou
/// - não estão dentro de nenhum módulo confiável e a região é anônima, tem pseudo-caminho que
///   não seja `[vdso]`/`[vsyscall]`, ou tem arquivo que não foi verificado como não PE (arquivo
///   `(deleted)`, PE ou ilegível). O arquivo só passa se estiver em `non_pe`
///   (veja [`crate::modules::verified_non_pe`]); na dúvida a regra é restritiva.
///
/// Regiões não executáveis e `[vdso]`/`[vsyscall]` passam. Um intervalo que toca memória não
/// mapeada não é problema da guarda: a leitura trata. O cálculo é por intervalos, sem iterar
/// byte a byte.
///
/// Devolve [`Error::ThirdPartyCode`] com o primeiro byte bloqueado. Um intervalo que passa de
/// `u64::MAX` devolve [`Error::Unmapped`].
pub fn check(
    maps: &[MapRegion],
    modules: &[Module],
    non_pe: &HashSet<PathBuf>,
    addr: u64,
    len: u64,
) -> Result<()> {
    let end = addr.checked_add(len).ok_or(Error::Unmapped {
        addr,
        len: usize::try_from(len).unwrap_or(usize::MAX),
    })?;
    let first_blocked = maps
        .iter()
        .filter(|region| region.perms.exec)
        .filter_map(|region| {
            let start = addr.max(region.start);
            let stop = end.min(region.end);
            (start < stop).then(|| first_blocked_in(region, modules, non_pe, start, stop))?
        })
        .min();
    match first_blocked {
        Some(addr) => Err(Error::ThirdPartyCode { addr }),
        None => Ok(()),
    }
}

/// Primeiro byte bloqueado de `[start, stop)`, dentro de uma região executável.
fn first_blocked_in(
    region: &MapRegion,
    modules: &[Module],
    non_pe: &HashSet<PathBuf>,
    start: u64,
    stop: u64,
) -> Option<u64> {
    // Só `[vdso]` e `[vsyscall]` são do kernel; qualquer outro pseudo-caminho conta como anônimo.
    if let MapPath::Pseudo(name) = &region.path
        && (name == "[vdso]" || name == "[vsyscall]")
    {
        return None;
    }
    let in_untrusted = modules
        .iter()
        .filter(|m| !m.trusted)
        .filter_map(|m| {
            let from = start.max(m.base);
            (from < stop.min(m.end())).then_some(from)
        })
        .min();
    let verified_non_pe = matches!(
        &region.path,
        MapPath::File { path, deleted: false } if non_pe.contains(path)
    );
    let outside_trusted = if verified_non_pe {
        None
    } else {
        first_outside_trusted(modules, start, stop)
    };
    match (in_untrusted, outside_trusted) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Primeiro endereço de `[start, stop)` que não está em nenhum módulo confiável.
fn first_outside_trusted(modules: &[Module], start: u64, stop: u64) -> Option<u64> {
    let mut at = start;
    while at < stop {
        match modules.iter().find(|m| m.trusted && m.contains(at)) {
            Some(m) => at = m.end(),
            None => return Some(at),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::maps::parse_maps;

    const M_BASE: u64 = 0x1_4000_0000;
    const M_END: u64 = 0x1_4b27_c000;
    const T_BASE: u64 = 0x1_8000_0000;
    const T_END: u64 = 0x1_8010_0000;

    fn modules() -> Vec<Module> {
        vec![
            Module {
                name: "game.exe".into(),
                base: M_BASE,
                size: M_END - M_BASE,
                trusted: true,
                ..Module::default()
            },
            Module {
                name: "evil.dll".into(),
                base: T_BASE,
                size: T_END - T_BASE,
                trusted: false,
                ..Module::default()
            },
        ]
    }

    fn no_non_pe() -> HashSet<PathBuf> {
        HashSet::new()
    }

    fn non_pe(paths: &[&str]) -> HashSet<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    fn maps(text: &str) -> Vec<MapRegion> {
        parse_maps(text).unwrap()
    }

    fn blocked_at(result: Result<()>) -> u64 {
        match result {
            Err(Error::ThirdPartyCode { addr }) => addr,
            other => panic!("esperava ThirdPartyCode, veio {other:?}"),
        }
    }

    #[test]
    fn anon_exec_outside_modules_blocked() {
        let m = maps("7700000-7710000 rwxp 00000000 00:00 0\n");
        let addr = blocked_at(check(&m, &modules(), &no_non_pe(), 0x770_0000, 16));
        assert_eq!(addr, 0x770_0000);
        // O primeiro byte bloqueado é o do intervalo pedido, não o início da região.
        assert_eq!(
            blocked_at(check(&m, &modules(), &no_non_pe(), 0x770_8000, 16)),
            0x770_8000
        );
    }

    #[test]
    fn anon_text_inside_trusted_module_allowed() {
        let m = maps(
            "140000000-140001000 r--p 00000000 00:00 1 /opt/game.exe\n\
             140001000-147a90000 r-xp 00000000 00:00 0\n",
        );
        assert!(check(&m, &modules(), &no_non_pe(), 0x1_456f_c11c, 32).is_ok());
    }

    #[test]
    fn third_party_pe_exec_blocked() {
        let m = maps("180001000-180002000 r-xp 00001000 00:00 1 /home/u/evil.dll\n");
        assert_eq!(
            blocked_at(check(&m, &modules(), &no_non_pe(), 0x1_8000_1000, 8)),
            0x1_8000_1000
        );
    }

    #[test]
    fn third_party_pe_data_allowed() {
        let m = maps("180000000-180001000 r--p 00000000 00:00 1 /home/u/evil.dll\n");
        assert!(check(&m, &modules(), &no_non_pe(), 0x1_8000_0000, 0x1000).is_ok());
    }

    #[test]
    fn elf_file_exec_allowed() {
        let m = maps("7f0000000000-7f0000100000 r-xp 00000000 00:00 1 /usr/lib64/libc.so.6\n");
        let verified = non_pe(&["/usr/lib64/libc.so.6"]);
        assert!(check(&m, &modules(), &verified, 0x7f00_0000_0000, 0x1000).is_ok());
    }

    #[test]
    fn unverified_exec_file_blocked() {
        let m = maps("7f0000000000-7f0000100000 r-xp 00000000 00:00 1 /usr/lib64/libc.so.6\n");
        assert_eq!(
            blocked_at(check(
                &m,
                &modules(),
                &no_non_pe(),
                0x7f00_0000_0000,
                0x1000
            )),
            0x7f00_0000_0000
        );
        // Outro caminho verificado não ajuda.
        let verified = non_pe(&["/usr/lib64/outro.so"]);
        assert!(check(&m, &modules(), &verified, 0x7f00_0000_0000, 1).is_err());
    }

    #[test]
    fn pe_file_exec_outside_modules_blocked() {
        // PE fora dos módulos (cabeçalho apagado ou mapeado de novo): não está em `non_pe`.
        let m = maps("190001000-190002000 r-xp 00001000 00:00 1 /tmp/e2.dll\n");
        assert_eq!(
            blocked_at(check(&m, &modules(), &no_non_pe(), 0x1_9000_1000, 8)),
            0x1_9000_1000
        );
    }

    #[test]
    fn deleted_exec_file_blocked_even_if_path_is_non_pe() {
        let m = maps(
            "180000000-180001000 r--p 00000000 00:00 1 /tmp/e.dll (deleted)\n\
             180001000-180002000 r-xp 00001000 00:00 1 /tmp/e.dll (deleted)\n",
        );
        // Sem módulo conhecido (o `discover` descarta `(deleted)`).
        let only_trusted = vec![modules().remove(0)];
        assert_eq!(
            blocked_at(check(&m, &only_trusted, &no_non_pe(), 0x1_8000_1000, 8)),
            0x1_8000_1000
        );
        // Mesmo que o caminho esteja em `non_pe`, `(deleted)` não conta como verificado.
        let verified = non_pe(&["/tmp/e.dll"]);
        assert!(check(&m, &only_trusted, &verified, 0x1_8000_1000, 8).is_err());
    }

    #[test]
    fn uprobes_pseudo_path_exec_blocked() {
        let m = maps("7000-8000 r-xp 00000000 00:00 0 [uprobes]\n");
        assert_eq!(
            blocked_at(check(&m, &modules(), &no_non_pe(), 0x7000, 1)),
            0x7000
        );
    }

    #[test]
    fn exec_file_inside_trusted_module_allowed() {
        // Região com arquivo cobrindo parte de um módulo confiável (ex.: exe mapeado do disco).
        let m = maps("140001000-140002000 r-xp 00001000 00:00 1 /opt/game.exe\n");
        assert!(check(&m, &modules(), &no_non_pe(), 0x1_4000_1000, 0x1000).is_ok());
    }

    #[test]
    fn anon_data_allowed() {
        let m = maps("7700000-7710000 rw-p 00000000 00:00 0\n");
        assert!(check(&m, &modules(), &no_non_pe(), 0x770_0000, 0x1000).is_ok());
    }

    #[test]
    fn merged_region_crossing_module_end() {
        let m = maps("14b270000-14b290000 r-xp 00000000 00:00 0\n");
        assert!(check(&m, &modules(), &no_non_pe(), 0x1_4b27_0000, 0x1000).is_ok());
        // Até o último byte do módulo ainda passa.
        assert!(check(&m, &modules(), &no_non_pe(), 0x1_4b27_b000, 0x1000).is_ok());
        let addr = blocked_at(check(&m, &modules(), &no_non_pe(), 0x1_4b27_b000, 0x2000));
        assert_eq!(addr, 0x1_4b27_c000);
        // Só a parte fora do módulo.
        assert_eq!(
            blocked_at(check(&m, &modules(), &no_non_pe(), 0x1_4b27_c000, 1)),
            0x1_4b27_c000
        );
    }

    #[test]
    fn vdso_allowed() {
        let m = maps(
            "7ffd00000000-7ffd00002000 r-xp 00000000 00:00 0 [vdso]\n\
             ffffffffff600000-ffffffffff601000 --xp 00000000 00:00 0 [vsyscall]\n",
        );
        assert!(check(&m, &modules(), &no_non_pe(), 0x7ffd_0000_0000, 0x2000).is_ok());
        assert!(check(&m, &modules(), &no_non_pe(), 0xffff_ffff_ff60_0000, 0x1000).is_ok());
    }

    #[test]
    fn heap_and_anon_pseudo_exec_blocked() {
        let m = maps(
            "5000-6000 rwxp 00000000 00:00 0 [heap]\n7000-8000 r-xp 00000000 00:00 0 [anon:x]\n",
        );
        assert_eq!(
            blocked_at(check(&m, &modules(), &no_non_pe(), 0x5000, 1)),
            0x5000
        );
        assert_eq!(
            blocked_at(check(&m, &modules(), &no_non_pe(), 0x7000, 1)),
            0x7000
        );
    }

    #[test]
    fn range_past_u64_max_is_rejected() {
        let m = maps("7700000-7710000 rwxp 00000000 00:00 0\n");
        match check(&m, &modules(), &no_non_pe(), u64::MAX - 15, 32) {
            Err(Error::Unmapped { addr, len }) => {
                assert_eq!(addr, u64::MAX - 15);
                assert_eq!(len, 32);
            }
            other => panic!("esperava Unmapped, veio {other:?}"),
        }
        assert!(matches!(
            check(&m, &modules(), &no_non_pe(), 1, u64::MAX),
            Err(Error::Unmapped { .. })
        ));
    }

    #[test]
    fn empty_range_and_unmapped_range_pass() {
        let m = maps("7700000-7710000 rwxp 00000000 00:00 0\n");
        assert!(check(&m, &modules(), &no_non_pe(), 0x770_0000, 0).is_ok());
        assert!(check(&m, &modules(), &no_non_pe(), 0x1000, 0x10).is_ok());
    }

    #[test]
    fn range_spanning_regions_reports_lowest_blocked_address() {
        let m = maps(
            "140001000-147a90000 r-xp 00000000 00:00 0\n\
             7700000-7710000 rwxp 00000000 00:00 0\n",
        );
        // Maior endereço primeiro nos mapas; o menor bloqueado vence.
        assert_eq!(
            blocked_at(check(
                &m,
                &modules(),
                &no_non_pe(),
                0x770_f000,
                0x1_4000_1000
            )),
            0x770_f000
        );
    }

    #[test]
    fn forged_path_pe_is_third_party() {
        // PE num caminho forjado parecido com o do Proton: o módulo veio como não confiável.
        let m = maps(
            "180000000-180002000 r-xp 00000000 00:00 1 /tmp/steamapps/common/Proton x/evil.dll\n",
        );
        assert_eq!(
            blocked_at(check(&m, &modules(), &no_non_pe(), 0x1_8000_0000, 4)),
            0x1_8000_0000
        );
    }
}
