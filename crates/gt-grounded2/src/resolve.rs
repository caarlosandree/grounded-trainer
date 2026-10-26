//! Resolução do processo alvo: PID, mapa de memória, módulos e imagem em disco.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gt_core::addr::AddrSpec;
use gt_core::maps::{MapPath, MapRegion, read_maps};
use gt_core::mem::MemReader;
use gt_core::modules::{self, Module, TrustPolicy};
use gt_core::pe::{DiskImage, PeHeaders, check_same_version};
use gt_core::process::list_processes;
use gt_core::procinfo::{Identity, identity};

use crate::target::{Pick, pick};
use crate::{Error, GAME_EXE, Result};

/// Bytes lidos do início da imagem para interpretar o cabeçalho PE.
const HEADER_LEN: usize = 0x1000;

/// Processo alvo já resolvido.
pub struct Target {
    pub pid: i32,
    /// Identidade do processo lida na abertura; o motor confere que ainda é a mesma.
    pub identity: Identity,
    /// Processos que mencionam o jogo mas foram descartados, com o motivo.
    pub discarded: Vec<(i32, String)>,
    pub maps: Vec<MapRegion>,
    pub reader: MemReader,
    pub modules: Vec<Module>,
    /// Arquivos executáveis do `maps` verificados como não PE (entrada da guarda).
    pub non_pe: HashSet<PathBuf>,
}

impl Target {
    /// Abre o alvo. Sem `pid`, procura o jogo em `/proc` (spec 6.1), ignorando o próprio `gt` e o
    /// processo pai, cujos `cmdline` podem citar `Grounded2`.
    pub fn open(pid: Option<i32>) -> Result<Target> {
        let (pid, discarded) = match pid {
            Some(pid) => (pid, Vec::new()),
            None => find_game()?,
        };
        let identity = identity(pid)?;
        let maps = read_maps(pid)?;
        let reader = MemReader::new(pid);
        let policy = match game_exe_path(&maps) {
            Some(exe) => {
                let home = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_default();
                crate::trust::policy(exe, &home)
            }
            None => TrustPolicy::default(),
        };
        let root = PathBuf::from(format!("/proc/{pid}/root"));
        let modules = modules::discover(&maps, &reader, &policy, &root)?;
        let non_pe = modules::verified_non_pe(&maps, &root);
        Ok(Target {
            pid,
            identity,
            discarded,
            maps,
            reader,
            modules,
            non_pe,
        })
    }

    /// Módulo pelo nome, com o cabeçalho PE lido da memória.
    ///
    /// `ModuleNotFound` se nenhum arquivo com esse nome estiver em `maps`; `NotPe` se estiver mas
    /// não for uma imagem PE.
    pub fn module(&self, name: &str) -> Result<(&Module, PeHeaders)> {
        Ok(self.find_module(name)?)
    }

    fn find_module(&self, name: &str) -> gt_core::Result<(&Module, PeHeaders)> {
        let (base, _) = modules::candidate_bases(&self.maps)
            .into_iter()
            .find(|(_, path)| {
                path.file_name()
                    .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(name))
            })
            .ok_or_else(|| gt_core::Error::ModuleNotFound {
                name: name.to_string(),
            })?;
        let not_pe = || gt_core::Error::NotPe {
            module: name.to_string(),
        };
        let mut header = vec![0u8; HEADER_LEN];
        match self.reader.read_exact(base, &mut header) {
            Ok(()) => {}
            Err(gt_core::Error::Unmapped { .. }) => return Err(not_pe()),
            Err(err) => return Err(err),
        }
        let headers = PeHeaders::parse(&header).map_err(|_| not_pe())?;
        let module = self
            .modules
            .iter()
            .find(|m| m.base == base)
            .ok_or_else(not_pe)?;
        Ok((module, headers))
    }

    /// Endereço absoluto de `spec`; `default_module` serve ao atalho `+offset`.
    pub fn resolve(&self, spec: &AddrSpec, default_module: &str) -> Result<u64> {
        let base = |name: Option<&str>| {
            self.find_module(name.unwrap_or(default_module))
                .map(|(module, _)| module.base)
        };
        Ok(spec.resolve(base)?)
    }

    /// `0x1456fc11c (Grounded2Steam-Win64-Shipping.exe+56fc11c)`, ou só o absoluto.
    pub fn label(&self, addr: u64) -> String {
        match self.module_label(addr) {
            Some(label) => format!("{addr:#x} ({label})"),
            None => format!("{addr:#x}"),
        }
    }

    /// `Grounded2Steam-Win64-Shipping.exe+56fc11c`, se o endereço está num módulo conhecido.
    pub fn module_label(&self, addr: u64) -> Option<String> {
        modules::describe(&self.modules, addr).map(|(name, offset)| format!("{name}+{offset:x}"))
    }

    /// Abre a imagem em disco do módulo, confere a versão e a realoca para a base atual.
    ///
    /// Procura o arquivo em `/proc/<pid>/root` (o container da Steam) e, se falhar, no caminho
    /// direto do host.
    pub fn open_disk(&self, module: &Module, mem: &PeHeaders) -> Result<DiskImage> {
        let relative = module.path.strip_prefix("/").unwrap_or(&module.path);
        let in_root = Path::new(&format!("/proc/{}/root", self.pid)).join(relative);
        let mut image = match DiskImage::load(&in_root) {
            Ok(image) => image,
            Err(first) => {
                tracing::debug!(path = %in_root.display(), error = %first, "imagem não encontrada no root do processo");
                DiskImage::load(&module.path).map_err(|source| Error::DiskOpen {
                    path: module.path.clone(),
                    source,
                })?
            }
        };
        check_same_version(mem, &image.headers)?;
        if module.base != image.headers.image_base {
            image.relocate(module.base)?;
        }
        Ok(image)
    }
}

/// Procura o jogo em `/proc`. Erro se não houver nenhum candidato ou houver mais de um.
fn find_game() -> Result<(i32, Vec<(i32, String)>)> {
    let own = i32::try_from(std::process::id()).ok();
    let parent = i32::try_from(std::os::unix::process::parent_id()).ok();
    let procs: Vec<_> = list_processes()?
        .into_iter()
        .filter(|(pid, _)| Some(*pid) != own && Some(*pid) != parent)
        .collect();
    let (choice, discarded) = pick(&procs);
    match choice {
        Pick::One(pid) => Ok((pid, discarded)),
        Pick::None => Err(Error::GameNotFound),
        Pick::Many(pids) => Err(Error::ManyCandidates { pids }),
    }
}

/// Caminho do executável do jogo em `maps` (primeira região com esse arquivo, offset 0).
fn game_exe_path(maps: &[MapRegion]) -> Option<&Path> {
    maps.iter().find_map(|region| {
        let MapPath::File { path, .. } = &region.path else {
            return None;
        };
        let name = path.file_name()?.to_string_lossy();
        (region.offset == 0 && name.eq_ignore_ascii_case(GAME_EXE)).then_some(path.as_path())
    })
}

/// Texto de ajuda para o erro de permissão (spec 6.10). Não sugere `sudo` como primeira opção.
///
/// `program` é o nome do programa que mostra a ajuda (`gt`, `gt-patch`, `gt-tui`) e entra nas
/// frases no lugar de "o programa". `ptrace_scope` é o valor de `kernel.yama.ptrace_scope`, se
/// conhecido.
pub fn permission_hint(program: &str, ptrace_scope: Option<u8>) -> String {
    match ptrace_scope {
        Some(0) => "kernel.yama.ptrace_scope está em 0, que já permite a leitura; confira se o \
                    processo é de outro usuário ou se o SELinux/um container bloqueia o acesso."
            .to_string(),
        Some(3) => format!(
            "kernel.yama.ptrace_scope está em 3, que bloqueia qualquer leitura da memória de \
             outro processo e só pode ser baixado reiniciando o sistema (o {program} não altera \
             nada)."
        ),
        Some(value) => format!(
            "kernel.yama.ptrace_scope está em {value}, o que restringe a leitura da memória de \
             outro processo (com 1, só descendentes do processo que lê). Confira se o {program} e \
             o jogo são do mesmo usuário; se aceitar mudar a política, baixe o valor com `sudo \
             sysctl kernel.yama.ptrace_scope=0` (afeta o sistema todo; o {program} nunca altera \
             isso sozinho)."
        ),
        None => "o arquivo kernel.yama.ptrace_scope não existe (Yama ausente); a recusa vem de \
                 outra política, como SELinux, um container ou um processo de outro usuário."
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::permission_hint;
    use crate::Error;

    #[test]
    fn permission_hint_mentions_scope_and_value_without_sudo_first() {
        let hint = permission_hint("gt", Some(1));
        assert!(hint.contains("ptrace_scope"));
        assert!(hint.contains('1'));
        assert!(!hint.trim_start().starts_with("sudo"));
        let sudo = hint.find("sudo");
        let scope = hint.find("ptrace_scope").unwrap();
        assert!(sudo.is_none_or(|s| s > scope), "{hint}");
    }

    #[test]
    fn permission_hint_for_scope_3_says_only_a_reboot_lowers_it() {
        let hint = permission_hint("gt", Some(3));
        assert!(hint.contains("reiniciando"), "{hint}");
        assert!(!hint.contains("sysctl"), "{hint}");
    }

    #[test]
    fn permission_hint_names_program() {
        let hint = permission_hint("gt-tui", Some(3));
        assert!(hint.contains("o gt-tui não altera nada"), "{hint}");
        let hint = permission_hint("gt-tui", Some(1));
        assert!(hint.contains("o gt-tui e o jogo"), "{hint}");
        assert!(!hint.contains("o programa"), "{hint}");
    }

    #[test]
    fn permission_hint_without_yama_says_so() {
        assert!(permission_hint("gt", None).contains("Yama ausente"));
    }

    #[test]
    fn many_candidates_lists_pids_and_mentions_pid_flag() {
        let msg = Error::ManyCandidates { pids: vec![10, 20] }.to_string();
        assert!(msg.contains("10"));
        assert!(msg.contains("20"));
        assert!(msg.contains("--pid"));
    }
}
