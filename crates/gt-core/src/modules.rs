//! Módulos (imagens PE) carregados no processo e política de módulos confiáveis.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};
use crate::maps::{MapPath, MapRegion};
use crate::mem::MemReader;
use crate::pe::{PeHeaders, read_file_headers};

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
    ///
    /// Diretórios vazios, relativos ou iguais a `/` (depois de normalizados) são descartados:
    /// confiariam em tudo. Fica do lado restritivo.
    pub fn new(dirs: Vec<PathBuf>) -> TrustPolicy {
        TrustPolicy {
            dirs: dirs
                .iter()
                .filter(|d| d.is_absolute())
                .map(|d| normalize(d))
                .filter(|d| d.components().count() > 1)
                .collect(),
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
    /// Extensão usada pela guarda e pelas saídas: `min(memory_size, disk_size)` para módulo
    /// confiável com o disco lido, senão `memory_size` (spec 002, 6.12).
    pub size: u64,
    /// `SizeOfImage` do cabeçalho PE na memória.
    pub memory_size: u64,
    /// `SizeOfImage` do arquivo em disco; `None` em módulo de terceiros (o disco não é aberto) ou
    /// quando o arquivo não pôde ser lido.
    pub disk_size: Option<u64>,
    /// `true` se o caminho está dentro de um diretório da [`TrustPolicy`].
    pub trusted: bool,
}

impl Module {
    /// `true` se o módulo é confiável e a extensão foi conferida com o `SizeOfImage` do disco.
    pub fn extent_checked(&self) -> bool {
        self.trusted && self.disk_size.is_some()
    }

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

/// Lê só o cabeçalho PE do arquivo de um módulo.
///
/// Tenta `root` + caminho (no uso real, `/proc/<pid>/root`, que enxerga o sistema de arquivos do
/// container da Steam) e, se falhar, o caminho direto do host.
pub fn open_in_root(root: &Path, path: &Path) -> Result<PeHeaders> {
    // `join` com caminho absoluto descartaria `root`.
    let in_root = root.join(path.strip_prefix("/").unwrap_or(path));
    read_file_headers(&in_root).or_else(|first| {
        tracing::debug!(path = %in_root.display(), error = %first, "imagem não encontrada no root do processo");
        read_file_headers(path)
    })
}

/// Extensão de um módulo confiável: `(size, disk_size)` a partir do `SizeOfImage` da memória e do
/// que o disco informa. Diferença entre os dois sai como aviso (spec 002, 6.12).
fn trusted_extent(name: &str, path: &Path, memory: u64, root: &Path) -> (u64, Option<u64>) {
    match open_in_root(root, path) {
        Ok(disk) => {
            let disk = u64::from(disk.size_of_image);
            if disk != memory {
                tracing::warn!(
                    module = name,
                    memory,
                    disk,
                    "SizeOfImage difere entre a memória e o disco"
                );
            }
            (memory.min(disk), Some(disk))
        }
        Err(err) => {
            tracing::warn!(module = name, error = %err, "extensão do módulo não conferida com o disco");
            (memory, None)
        }
    }
}

/// Descobre as imagens PE carregadas: lê o cabeçalho em cada base candidata.
///
/// Só entram as que passam em [`PeHeaders::parse`]. Uma candidata não mapeada é ignorada;
/// `PermissionDenied` e `ProcessGone` são propagados.
///
/// Para módulo **confiável**, a extensão é limitada pelo `SizeOfImage` do arquivo em disco, lido
/// por `root` (ver [`open_in_root`]); módulo de terceiros nunca abre o disco.
pub fn discover(
    maps: &[MapRegion],
    reader: &MemReader,
    policy: &TrustPolicy,
    root: &Path,
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
        let memory_size = u64::from(pe.size_of_image);
        let (size, disk_size) = if trusted {
            trusted_extent(&name, &path, memory_size, root)
        } else {
            (memory_size, None)
        };
        modules.push(Module {
            name,
            path,
            base,
            size,
            memory_size,
            disk_size,
            trusted,
        });
    }
    Ok(modules)
}

/// Caminhos de regiões executáveis com arquivo que foram verificados como **não** PE.
///
/// Para cada caminho único de região executável com arquivo (`File`, não `deleted`), lê os dois
/// primeiros bytes de `root` + caminho e o inclui só se a leitura der certo e os bytes forem
/// diferentes de `MZ`. Qualquer erro de leitura deixa o caminho fora do conjunto (não verificado).
/// No uso real, `root` é `/proc/<pid>/root`, que enxerga o mesmo sistema de arquivos do processo.
pub fn verified_non_pe(maps: &[MapRegion], root: &Path) -> HashSet<PathBuf> {
    let paths: HashSet<&Path> = maps
        .iter()
        .filter(|region| region.perms.exec)
        .filter_map(|region| match &region.path {
            MapPath::File {
                path,
                deleted: false,
            } => Some(path.as_path()),
            _ => None,
        })
        .collect();
    paths
        .into_iter()
        .filter(|path| starts_without_mz(root, path))
        .map(Path::to_path_buf)
        .collect()
}

/// `true` se o arquivo é regular, legível e seus dois primeiros bytes não são `MZ`.
fn starts_without_mz(root: &Path, path: &Path) -> bool {
    // `join` com caminho absoluto descartaria `root`.
    let real = root.join(path.strip_prefix("/").unwrap_or(path));
    // Só arquivos regulares: abrir um FIFO ou dispositivo poderia bloquear.
    if !std::fs::metadata(&real).is_ok_and(|m| m.is_file()) {
        return false;
    }
    let mut magic = [0u8; 2];
    let read = File::open(&real).and_then(|mut f| f.read_exact(&mut magic));
    read.is_ok() && &magic != b"MZ"
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
    use crate::pe::tests::fake_pe;

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
    fn policy_drops_empty_relative_and_root_dirs() {
        let p = TrustPolicy::new(vec![
            PathBuf::new(),
            ".".into(),
            "relativo/dir".into(),
            "/".into(),
            "/..".into(),
            "/a/../..".into(),
        ]);
        assert!(!p.is_trusted(Path::new("/qualquer/coisa.dll")));
        assert!(!p.is_trusted(Path::new("relativo/dir/a.dll")));
        assert!(!p.is_trusted(Path::new("a.dll")));
        // Diretórios válidos na mesma lista continuam valendo.
        let p = TrustPolicy::new(vec!["".into(), "/opt/ok".into()]);
        assert!(p.is_trusted(Path::new("/opt/ok/a.dll")));
    }

    #[test]
    fn verified_non_pe_keeps_only_readable_non_mz_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::write(root.join("lib/libx.so"), b"\x7fELF....").unwrap();
        std::fs::write(root.join("lib/a.dll"), b"MZ\x90\x00").unwrap();
        std::fs::write(root.join("lib/short.bin"), b"M").unwrap();
        std::fs::write(root.join("lib/gone.so"), b"\x7fELF").unwrap();
        let maps = parse_maps(
            "1000-2000 r-xp 00000000 00:00 1 /lib/libx.so\n\
             2000-3000 r-xp 00000000 00:00 1 /lib/libx.so\n\
             3000-4000 r-xp 00000000 00:00 1 /lib/a.dll\n\
             4000-5000 r-xp 00000000 00:00 1 /lib/short.bin\n\
             5000-6000 r-xp 00000000 00:00 1 /lib/ausente.so\n\
             6000-7000 r-xp 00000000 00:00 1 /lib/gone.so (deleted)\n\
             7000-8000 r--p 00000000 00:00 1 /lib/gone.so\n\
             8000-9000 r-xp 00000000 00:00 0\n",
        )
        .unwrap();
        let set = verified_non_pe(&maps, root);
        assert_eq!(set, HashSet::from([PathBuf::from("/lib/libx.so")]));
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

    /// Imagem PE mínima (sem seções) com o `SizeOfImage` dado, completada até `0x1000` bytes
    /// (o `discover` lê essa janela da memória).
    fn tiny_pe(size_of_image: u32) -> Vec<u8> {
        let mut f = fake_pe(&[], 0x1_4000_0000, 0, size_of_image, None);
        f.resize(HEADER_LEN, 0);
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
        // Nenhum dos arquivos existe em disco: o raiz vazio não muda o resultado.
        let root = tempfile::tempdir().unwrap();
        let mut modules = discover(&maps, &reader, &policy, root.path()).unwrap();
        modules.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(modules.len(), 2);
        assert_eq!(modules[0].name, "Game.EXE");
        assert_eq!(modules[0].size, 0x20_0000);
        assert_eq!(modules[0].memory_size, 0x20_0000);
        assert_eq!(modules[0].disk_size, None);
        assert_eq!(modules[0].base, pe_trusted.as_ptr() as u64);
        assert!(modules[0].trusted);
        assert_eq!(modules[1].name, "inject.dll");
        assert!(!modules[1].trusted);
    }

    /// Descobre um único módulo em `path`, com `memory` de `SizeOfImage` na memória e, se houver,
    /// `disk` no arquivo correspondente sob um raiz temporário. Só `/opt/trusted` é confiável.
    fn discover_one(path: &str, memory: u32, disk: Option<u32>) -> Module {
        let root = tempfile::tempdir().unwrap();
        if let Some(disk) = disk {
            let file = root.path().join(path.trim_start_matches('/'));
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, fake_pe(&[], 0x1_4000_0000, 0, disk, None)).unwrap();
        }
        let image = tiny_pe(memory);
        let maps = vec![file_region(image.as_ptr() as u64, path)];
        let reader = MemReader::new(std::process::id() as i32);
        let policy = TrustPolicy::new(vec!["/opt/trusted".into()]);
        let mut modules = discover(&maps, &reader, &policy, root.path()).unwrap();
        assert_eq!(modules.len(), 1);
        modules.remove(0)
    }

    #[test]
    fn trusted_extent_is_min_of_memory_and_disk() {
        let m = discover_one("/opt/trusted/a.dll", 0x3000, Some(0x2000));
        assert_eq!((m.memory_size, m.disk_size), (0x3000, Some(0x2000)));
        assert_eq!(m.size, 0x2000);
        assert!(m.extent_checked());
        // Disco maior que a memória: a memória continua limitando.
        let m = discover_one("/opt/trusted/b.dll", 0x2000, Some(0x3000));
        assert_eq!(m.size, 0x2000);
        // Iguais: sem diferença.
        let m = discover_one("/opt/trusted/c.dll", 0x2000, Some(0x2000));
        assert_eq!(m.size, 0x2000);
        assert!(m.extent_checked());
    }

    #[test]
    fn trusted_extent_without_disk_keeps_memory() {
        let m = discover_one("/opt/trusted/a.dll", 0x3000, None);
        assert_eq!(m.size, 0x3000);
        assert_eq!((m.memory_size, m.disk_size), (0x3000, None));
        assert!(!m.extent_checked());
    }

    #[test]
    fn third_party_extent_ignores_disk() {
        let m = discover_one("/home/u/evil/x.dll", 0x3000, Some(0x1000));
        assert!(!m.trusted);
        assert_eq!(m.size, 0x3000);
        assert_eq!(m.disk_size, None);
        assert!(!m.extent_checked());
    }

    #[test]
    fn open_in_root_prefers_root_and_falls_back_to_direct_path() {
        let root = tempfile::tempdir().unwrap();
        let direct = tempfile::tempdir().unwrap();
        let direct_file = direct.path().join("d.dll");
        std::fs::write(&direct_file, fake_pe(&[], 0x1_4000_0000, 0, 0x5000, None)).unwrap();
        // Só o caminho direto existe.
        let pe = open_in_root(root.path(), &direct_file).unwrap();
        assert_eq!(pe.size_of_image, 0x5000);
        // Com a cópia no raiz, ela vence.
        let in_root = root.path().join(direct_file.strip_prefix("/").unwrap());
        std::fs::create_dir_all(in_root.parent().unwrap()).unwrap();
        std::fs::write(&in_root, fake_pe(&[], 0x1_4000_0000, 0, 0x6000, None)).unwrap();
        assert_eq!(
            open_in_root(root.path(), &direct_file)
                .unwrap()
                .size_of_image,
            0x6000
        );
        // Nenhum dos dois: erro.
        assert!(open_in_root(root.path(), Path::new("/nao/existe.dll")).is_err());
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
