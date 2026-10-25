//! Política de módulos confiáveis do Grounded 2 (spec 6.1).

use std::path::{Component, Path, PathBuf};

use gt_core::modules::TrustPolicy;

use crate::APP_ID;

/// Monta a política de confiança para o Grounded 2.
///
/// `game_exe` é o caminho do executável do jogo em `/proc/<pid>/maps` e `home` é o diretório do
/// usuário. Qualquer PE fora destes diretórios é módulo de terceiros. Na dúvida, a lista fica do
/// lado restritivo.
pub fn policy(game_exe: &Path, home: &Path) -> TrustPolicy {
    let steam = home.join(".local/share/Steam");
    let game_library = steamapps_prefix(game_exe);

    let mut libraries: Vec<PathBuf> = Vec::new();
    if let Some(library) = &game_library {
        libraries.push(library.clone());
    }
    let default_library = steam.join("steamapps");
    if !libraries.contains(&default_library) {
        libraries.push(default_library);
    }

    let mut dirs: Vec<PathBuf> = Vec::new();
    for library in &libraries {
        let common = library.join("common");
        dirs.push(common.join("Grounded2"));
        dirs.extend(runtime_dirs(&common));
    }
    dirs.push(steam.join("compatibilitytools.d"));

    if let Some(library) = &game_library {
        let drive_c = library
            .join("compatdata")
            .join(APP_ID.to_string())
            .join("pfx/drive_c");
        dirs.push(drive_c.join("windows"));
        dirs.push(drive_c.join("Program Files (x86)/Steam"));
        dirs.push(drive_c.join("Program Files/Steam"));
    }
    TrustPolicy::new(dirs)
}

/// Prefixo de `path` até o componente `steamapps` (inclusive), a biblioteca Steam do jogo.
fn steamapps_prefix(path: &Path) -> Option<PathBuf> {
    let components: Vec<Component> = path.components().collect();
    let last = components
        .iter()
        .rposition(|c| c.as_os_str() == "steamapps")?;
    Some(components[..=last].iter().collect())
}

/// Entradas de `common/` que começam com `Proton` ou `SteamLinuxRuntime`. Erros de leitura são
/// ignorados: sem a lista, o resultado só fica mais restritivo.
fn runtime_dirs(common: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(common) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.starts_with("Proton") || name.starts_with("SteamLinuxRuntime")
        })
        .map(|entry| entry.path())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Cria a árvore de uma instalação do Grounded 2 em um diretório temporário.
    struct Fixture {
        _tmp: tempfile::TempDir,
        game_exe: PathBuf,
        home: PathBuf,
        library: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let home = root.join("home/user");
        let library = root.join("lib/SteamLibrary/steamapps");
        let game_dir = library.join("common/Grounded2/Augusta/Binaries/Win64");
        fs::create_dir_all(&game_dir).unwrap();
        let game_exe = game_dir.join(crate::GAME_EXE);
        fs::write(&game_exe, b"").unwrap();
        for dir in [
            "common/Proton 11.0/files",
            "common/Proton - Experimental",
            "common/SteamLinuxRuntime_sniper",
            "common/OutroJogo",
            "compatdata/2661300/pfx/drive_c/windows/system32",
            "compatdata/2661300/pfx/drive_c/Program Files (x86)/Steam",
            "compatdata/2661300/pfx/drive_c/users/steamuser/AppData/Local/FLiNGTrainer",
        ] {
            fs::create_dir_all(library.join(dir)).unwrap();
        }
        fs::create_dir_all(home.join(".local/share/Steam/steamapps/common/Proton 9.0")).unwrap();
        fs::create_dir_all(home.join(".local/share/Steam/compatibilitytools.d/GE-Proton")).unwrap();
        Fixture {
            _tmp: tmp,
            game_exe,
            home,
            library,
        }
    }

    #[test]
    fn fling_dll_is_third_party() {
        let f = fixture();
        let p = policy(&f.game_exe, &f.home);
        let drive_c = f.library.join("compatdata/2661300/pfx/drive_c");
        let fling = drive_c.join("users/steamuser/AppData/Local/FLiNGTrainer/TrSpeedHack_x64.dll");
        assert!(!p.is_trusted(&fling));
        assert!(p.is_trusted(&drive_c.join("windows/system32/kernel32.dll")));
        assert!(p.is_trusted(&drive_c.join("Program Files (x86)/Steam/steamclient64.dll")));
        assert!(p.is_trusted(&drive_c.join("Program Files/Steam/GameOverlayRenderer64.dll")));
    }

    #[test]
    fn game_runtime_and_compat_tools_are_trusted() {
        let f = fixture();
        let p = policy(&f.game_exe, &f.home);
        assert!(p.is_trusted(&f.game_exe));
        let common = f.library.join("common");
        assert!(p.is_trusted(&common.join("Proton 11.0/files/lib/wine/ntdll.dll")));
        assert!(p.is_trusted(&common.join("Proton - Experimental/files/x.dll")));
        assert!(p.is_trusted(&common.join("SteamLinuxRuntime_sniper/x.so")));
        assert!(!p.is_trusted(&common.join("OutroJogo/x.dll")));
        // Biblioteca padrão do usuário e Protons instalados à parte.
        let steam = f.home.join(".local/share/Steam");
        assert!(p.is_trusted(&steam.join("steamapps/common/Proton 9.0/files/x.dll")));
        assert!(p.is_trusted(&steam.join("compatibilitytools.d/GE-Proton/files/x.dll")));
        assert!(!p.is_trusted(&steam.join("steamapps/common/OutroJogo/x.dll")));
        // Fora de tudo.
        assert!(!p.is_trusted(&f.home.join("Downloads/evil.dll")));
    }

    #[test]
    fn exe_without_steamapps_falls_back_to_default_library_only() {
        let f = fixture();
        let p = policy(
            Path::new("/opt/jogo/Grounded2Steam-Win64-Shipping.exe"),
            &f.home,
        );
        assert!(
            !p.is_trusted(
                &f.library
                    .join("compatdata/2661300/pfx/drive_c/windows/a.dll")
            )
        );
        assert!(!p.is_trusted(Path::new("/opt/jogo/Grounded2Steam-Win64-Shipping.exe")));
        let steam = f.home.join(".local/share/Steam");
        assert!(p.is_trusted(&steam.join("steamapps/common/Grounded2/a.dll")));
    }

    #[test]
    fn missing_directories_are_ignored() {
        let p = policy(
            Path::new("/nao/existe/steamapps/common/Grounded2/Win64/g.exe"),
            Path::new("/nao/existe/home"),
        );
        assert!(p.is_trusted(Path::new(
            "/nao/existe/steamapps/common/Grounded2/Win64/g.exe"
        )));
        assert!(!p.is_trusted(Path::new("/nao/existe/steamapps/common/Proton 1/a.dll")));
    }
}
