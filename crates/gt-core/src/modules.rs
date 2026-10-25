//! Módulos (imagens PE) carregados no processo e política de módulos confiáveis.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};
use crate::maps::{MapPath, MapRegion};
use crate::mem::MemReader;
use crate::pe::PeHeaders;

/// Quantos bytes do início da imagem são lidos para interpretar o cabeçalho PE.
const HEADER_LEN: usize = 0x1000;

/// Política de módulos confiáveis: uma lista de diretórios absolutos.
///
/// Um arquivo é confiável quando o caminho está dentro de um dos diretórios. A comparação é por
/// componentes de caminho, sobre caminhos normalizados lexicalmente (sem tocar o disco), nunca
/// por substring.
#[derive(Debug, Clone, Default)]
pub struct TrustPolicy {
    dirs: Vec<PathBuf>,
}

impl TrustPolicy {
    /// Cria a política a partir dos diretórios confiáveis.
    pub fn new(dirs: Vec<PathBuf>) -> TrustPolicy {
        TrustPolicy {
            dirs: dirs.iter().map(|d| normalize(d)).collect(),
        }
    }

    /// `true` se `path` estiver dentro de algum diretório confiável.
    pub fn is_trusted(&self, path: &Path) -> bool {
        let path = normalize(path);
        self.dirs.iter().any(|dir| path.starts_with(dir))
    }
}

/// Remove `.` e resolve `..` sem consultar o disco, para que `a/b/../../x` não escape de `a`.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Em `/`, `..` continua em `/`; em caminho relativo vazio, é preservado.
                if !out.pop() && !out.has_root() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Uma imagem PE carregada no processo.
#[derive(Debug, Clone, Default)]
pub struct Module {
    /// Nome do arquivo, sem o diretório.
    pub name: String,
    /// Caminho do arquivo em `/proc/<pid>/maps`.
    pub path: PathBuf,
    /// Endereço onde a imagem foi carregada.
    pub base: u64,
    /// `SizeOfImage` do cabeçalho PE na memória.
    pub size: u64,
    /// `true` se o caminho está dentro de um diretório da [`TrustPolicy`].
    pub trusted: bool,
}

impl Module {
    /// Primeiro endereço fora do módulo (exclusivo).
    pub fn end(&self) -> u64 {
        self.base.saturating_add(self.size)
    }

    /// `true` se `addr` está em `[base, end)`.
    pub fn contains(&self, addr: u64) -> bool {
        addr >= self.base && addr < self.end()
    }
}

/// Bases candidatas a imagem PE: regiões de arquivo (não apagado), com offset 0, legíveis e fora
/// de `/dev/`, uma por caminho (a de menor endereço), em ordem de endereço.
pub fn candidate_bases(maps: &[MapRegion]) -> Vec<(u64, PathBuf)> {
    let mut lowest: HashMap<&Path, u64> = HashMap::new();
    for region in maps {
        let MapPath::File {
            path,
            deleted: false,
        } = &region.path
        else {
            continue;
        };
        if region.offset != 0 || !region.perms.read || path.starts_with("/dev/") {
            continue;
        }
        lowest
            .entry(path.as_path())
            .and_modify(|start| *start = (*start).min(region.start))
            .or_insert(region.start);
    }
    let mut bases: Vec<(u64, PathBuf)> = lowest
        .into_iter()
        .map(|(path, start)| (start, path.to_path_buf()))
        .collect();
    bases.sort();
    bases
}

/// Descobre as imagens PE carregadas: lê o cabeçalho em cada base candidata.
///
/// Só entram as que passam em [`PeHeaders::parse`]. Uma candidata não mapeada é ignorada;
/// `PermissionDenied` e `ProcessGone` são propagados.
pub fn discover(
    maps: &[MapRegion],
    reader: &MemReader,
    policy: &TrustPolicy,
) -> Result<Vec<Module>> {
    let mut modules = Vec::new();
    for (base, path) in candidate_bases(maps) {
        let mut header = vec![0u8; HEADER_LEN];
        match reader.read_exact(base, &mut header) {
            Ok(()) => {}
            Err(Error::Unmapped { .. }) => continue,
            Err(err) => return Err(err),
        }
        let Ok(pe) = PeHeaders::parse(&header) else {
            continue;
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let trusted = policy.is_trusted(&path);
        modules.push(Module {
            name,
            path,
            base,
            size: u64::from(pe.size_of_image),
            trusted,
        });
    }
    Ok(modules)
}

/// Recusa módulos de terceiros (usada por `scan`, `diff` e `dump` de seção executável).
pub fn require_trusted(module: &Module) -> Result<()> {
    if module.trusted {
        Ok(())
    } else {
        Err(Error::ThirdPartyCode { addr: module.base })
    }
}

/// Procura um módulo pelo nome do arquivo, sem diferenciar maiúsculas.
pub fn find<'a>(modules: &'a [Module], name: &str) -> Option<&'a Module> {
    modules.iter().find(|m| m.name.eq_ignore_ascii_case(name))
}

/// `(nome, offset)` do módulo que contém `addr`, para exibir `módulo+offset`.
pub fn describe(modules: &[Module], addr: u64) -> Option<(String, u64)> {
    modules
        .iter()
        .find(|m| m.contains(addr))
        .map(|m| (m.name.clone(), addr - m.base))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::maps::parse_maps;

    const GAME_MAPS: &str = include_str!("../tests/fixtures/game-maps.txt");

    fn module(name: &str, base: u64, size: u64, trusted: bool) -> Module {
        Module {
            name: name.to_string(),
            base,
            size,
            trusted,
            ..Module::default()
        }
    }

    #[test]
    fn policy_matches_by_components() {
        let p = TrustPolicy::new(vec![
            "/home/user/.local/share/Steam/steamapps/common/Proton 11.0".into(),
        ]);
        assert!(p.is_trusted(Path::new(
            "/home/user/.local/share/Steam/steamapps/common/Proton 11.0/files/lib/wine/x86_64-windows/ntdll.dll"
        )));
        assert!(!p.is_trusted(Path::new("/tmp/steamapps/common/Proton x/evil.dll")));
        assert!(!p.is_trusted(Path::new(
            "/home/user/.local/share/Steam/steamapps/common/Proton 11.0-evil/a.dll"
        )));
    }

    #[test]
    fn policy_does_not_follow_dotdot_out_of_the_directory() {
        let p = TrustPolicy::new(vec!["/opt/trusted".into()]);
        assert!(p.is_trusted(Path::new("/opt/trusted/./a/../b.dll")));
        assert!(!p.is_trusted(Path::new("/opt/trusted/../evil/b.dll")));
        assert!(!p.is_trusted(Path::new("/opt/trusted/a/../../evil/b.dll")));
    }

    #[test]
    fn empty_policy_trusts_nothing() {
        assert!(!TrustPolicy::default().is_trusted(Path::new("/opt/trusted/a.dll")));
    }

    #[test]
    fn candidate_bases_from_real_maps() {
        let maps = parse_maps(GAME_MAPS).unwrap();
        let bases = candidate_bases(&maps);
        let game = bases
            .iter()
            .find(|(base, _)| *base == 0x1_4000_0000)
            .expect("base do exe do jogo");
        assert_eq!(
            game.1.file_name().and_then(|n| n.to_str()),
            Some("Grounded2Steam-Win64-Shipping.exe")
        );
        assert!(bases.iter().all(|(_, p)| !p.starts_with("/dev/")));
        // Uma base por caminho, em ordem de endereço.
        let mut paths: Vec<_> = bases.iter().map(|(_, p)| p).collect();
        paths.sort();
        paths.dedup();
        assert_eq!(paths.len(), bases.len());
        assert!(bases.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn candidate_bases_skips_deleted_nonzero_offset_and_unreadable() {
        let maps = parse_maps(
            "1000-2000 r--p 00000000 00:00 1 /a/deleted.dll (deleted)\n\
             3000-4000 r--p 00001000 00:00 1 /a/offset.dll\n\
             5000-6000 ---p 00000000 00:00 1 /a/noread.dll\n\
             7000-8000 r-xp 00000000 00:00 1 /a/ok.dll\n\
             9000-a000 r--p 00000000 00:00 1 /a/ok.dll\n",
        )
        .unwrap();
        assert_eq!(
            candidate_bases(&maps),
            vec![(0x7000, PathBuf::from("/a/ok.dll"))]
        );
    }

    /// Cabeçalho PE32+ mínimo (sem seções) com o `SizeOfImage` dado.
    fn tiny_pe(size_of_image: u32) -> Vec<u8> {
        let mut f = vec![0u8; 0x1000];
        f[0..2].copy_from_slice(b"MZ");
        f[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        f[0x40..0x44].copy_from_slice(b"PE\0\0");
        f[0x44..0x46].copy_from_slice(&0x8664u16.to_le_bytes());
        let opt_size = 112 + 16 * 8;
        f[0x54..0x56].copy_from_slice(&(opt_size as u16).to_le_bytes());
        let o = 0x58;
        f[o..o + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        f[o + 24..o + 32].copy_from_slice(&0x1_4000_0000u64.to_le_bytes());
        f[o + 32..o + 36].copy_from_slice(&0x1000u32.to_le_bytes());
        f[o + 36..o + 40].copy_from_slice(&0x200u32.to_le_bytes());
        f[o + 56..o + 60].copy_from_slice(&size_of_image.to_le_bytes());
        f[o + 60..o + 64].copy_from_slice(&0x400u32.to_le_bytes());
        f[o + 108..o + 112].copy_from_slice(&16u32.to_le_bytes());
        f
    }

    fn file_region(start: u64, path: &str) -> MapRegion {
        let text = format!(
            "{start:x}-{:x} r--p 00000000 00:00 1 {path}",
            start + 0x1000
        );
        parse_maps(&text).unwrap().remove(0)
    }

    #[test]
    fn discover_keeps_only_pe_images_and_applies_policy() {
        // Memória do próprio processo faz o papel do jogo: dois PEs e um arquivo que não é PE.
        let pe_trusted = tiny_pe(0x20_0000);
        let pe_foreign = tiny_pe(0x1000);
        let not_pe = vec![0x7fu8; 0x1000];
        let maps = vec![
            file_region(pe_trusted.as_ptr() as u64, "/opt/trusted/Game.EXE"),
            file_region(pe_foreign.as_ptr() as u64, "/home/u/evil/inject.dll"),
            file_region(not_pe.as_ptr() as u64, "/usr/lib64/libc.so.6"),
            // Candidata fora de qualquer região legível: é ignorada.
            file_region(0x10, "/nowhere/gone.dll"),
        ];
        let reader = MemReader::new(std::process::id() as i32);
        let policy = TrustPolicy::new(vec!["/opt/trusted".into()]);
        let mut modules = discover(&maps, &reader, &policy).unwrap();
        modules.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(modules.len(), 2);
        assert_eq!(modules[0].name, "Game.EXE");
        assert_eq!(modules[0].size, 0x20_0000);
        assert_eq!(modules[0].base, pe_trusted.as_ptr() as u64);
        assert!(modules[0].trusted);
        assert_eq!(modules[1].name, "inject.dll");
        assert!(!modules[1].trusted);
    }

    #[test]
    fn require_trusted_rejects_third_party() {
        let bad = module("evil.dll", 0x1800_0000, 0x1000, false);
        match require_trusted(&bad) {
            Err(Error::ThirdPartyCode { addr }) => assert_eq!(addr, 0x1800_0000),
            other => panic!("esperava ThirdPartyCode, veio {other:?}"),
        }
        assert!(require_trusted(&module("ok.dll", 0x1000, 0x1000, true)).is_ok());
    }

    #[test]
    fn find_ignores_case_and_describe_gives_offset() {
        let mods = vec![
            module("Game.exe", 0x1_4000_0000, 0xb27c000, true),
            module("ntdll.dll", 0x7000_0000, 0x20000, true),
        ];
        assert_eq!(find(&mods, "game.EXE").map(|m| m.base), Some(0x1_4000_0000));
        assert!(find(&mods, "outro.dll").is_none());
        assert_eq!(
            describe(&mods, 0x1_4000_1000),
            Some(("Game.exe".to_string(), 0x1000))
        );
        assert_eq!(describe(&mods, 0x7001_ffff).map(|d| d.1), Some(0x1ffff));
        assert_eq!(describe(&mods, 0x7002_0000), None);
        assert_eq!(describe(&mods, 0x1), None);
    }

    #[test]
    fn module_contains_is_half_open_and_saturates() {
        let m = module("a", 0x1000, 0x1000, true);
        assert!(m.contains(0x1000) && m.contains(0x1fff));
        assert!(!m.contains(0xfff) && !m.contains(0x2000));
        assert_eq!(m.end(), 0x2000);
        assert_eq!(module("b", u64::MAX - 1, 100, true).end(), u64::MAX);
    }
}
