//! Log em arquivo (spec 003, 6.9).

use std::fs::{self, File};
use std::io;
use std::path::Path;

/// Nome do log atual.
const LOG_FILE: &str = "gt-tui.log";
/// Nome da geração anterior (só uma).
const OLD_LOG_FILE: &str = "gt-tui.log.1";

/// Abre um `gt-tui.log` novo em `dir`, antes renomeando o anterior para `gt-tui.log.1`
/// (substituindo a geração velha). Qualquer erro dá `None`: a TUI segue sem log em arquivo.
pub fn open_log(dir: &Path) -> Option<File> {
    fs::create_dir_all(dir).ok()?;
    let current = dir.join(LOG_FILE);
    // O `rename` substitui a geração velha. Sem log anterior, não há o que rotacionar.
    match fs::rename(&current, dir.join(OLD_LOG_FILE)) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return None,
    }
    File::create(current).ok()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn rotates_one_generation() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = open_log(dir.path()).unwrap();
        writeln!(first, "primeiro").unwrap();
        drop(first);
        let mut second = open_log(dir.path()).unwrap();
        writeln!(second, "segundo").unwrap();
        drop(second);
        let mut third = open_log(dir.path()).unwrap();
        writeln!(third, "terceiro").unwrap();
        drop(third);

        let read = |name: &str| std::fs::read_to_string(dir.path().join(name)).unwrap();
        assert_eq!(read("gt-tui.log"), "terceiro\n");
        assert_eq!(read("gt-tui.log.1"), "segundo\n");
        // Só uma geração: nada além desses dois arquivos.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn creates_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b");
        assert!(open_log(&nested).is_some());
        assert!(nested.join("gt-tui.log").exists());
    }

    #[test]
    fn unwritable_dir_gives_none() {
        let dir = tempfile::tempdir().unwrap();
        // Um arquivo no lugar do diretório: não dá para criar o log dentro dele.
        let blocker = dir.path().join("arquivo");
        std::fs::write(&blocker, b"x").unwrap();
        assert!(open_log(&blocker).is_none());
        assert!(open_log(&blocker.join("sub")).is_none());
    }
}
