//! Leitura de `/proc/<pid>/maps`.

use std::ffi::OsStr;
use std::io;
use std::path::PathBuf;

use nix::errno::Errno;

use crate::error::{Error, Result};

/// Permissões de uma região, como em `rwxp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Perms {
    pub read: bool,
    pub write: bool,
    pub exec: bool,
    /// `true` para `s` (compartilhada), `false` para `p` (privada).
    pub shared: bool,
}

/// Origem de uma região de memória.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapPath {
    /// Sem nome: memória anônima comum.
    Anonymous,
    /// Pseudo-caminho entre colchetes, como `[heap]`, `[stack]` ou `[anon:...]`.
    Pseudo(String),
    /// Arquivo no disco. `deleted` indica que o arquivo foi apagado depois de mapeado.
    File { path: PathBuf, deleted: bool },
}

/// Uma linha de `/proc/<pid>/maps`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapRegion {
    pub start: u64,
    /// Primeiro endereço fora da região (exclusivo).
    pub end: u64,
    pub perms: Perms,
    pub offset: u64,
    pub path: MapPath,
}

impl MapRegion {
    /// `true` para memória sem arquivo de origem: anônima, `[heap]`, `[stack]` e `[anon...]`.
    pub fn is_anonymous(&self) -> bool {
        match &self.path {
            MapPath::Anonymous => true,
            MapPath::Pseudo(name) => {
                name == "[heap]" || name == "[stack]" || name.starts_with("[anon")
            }
            MapPath::File { .. } => false,
        }
    }

    /// Nome do arquivo de origem, sem o diretório.
    pub fn file_name(&self) -> Option<&OsStr> {
        match &self.path {
            MapPath::File { path, .. } => path.file_name(),
            _ => None,
        }
    }
}

/// Interpreta o texto de `/proc/<pid>/maps`.
pub fn parse_maps(text: &str) -> Result<Vec<MapRegion>> {
    text.lines().map(parse_line).collect()
}

/// Lê e interpreta `/proc/<pid>/maps` de um processo.
pub fn read_maps(pid: i32) -> Result<Vec<MapRegion>> {
    let bytes = match std::fs::read(format!("/proc/{pid}/maps")) {
        Ok(bytes) => bytes,
        Err(err) => return Err(proc_error(pid, err)),
    };
    // Um processo zumbi devolve o arquivo vazio, não um erro.
    if bytes.is_empty() {
        return Err(Error::ProcessGone { pid });
    }
    parse_maps(&String::from_utf8_lossy(&bytes))
}

fn proc_error(pid: i32, err: io::Error) -> Error {
    if err.raw_os_error() == Some(Errno::ESRCH as i32) {
        return Error::ProcessGone { pid };
    }
    match err.kind() {
        io::ErrorKind::NotFound => Error::ProcessGone { pid },
        // `ptrace_scope` é preenchido pelo módulo de processos, quando necessário.
        io::ErrorKind::PermissionDenied => Error::PermissionDenied {
            pid,
            ptrace_scope: None,
        },
        _ => Error::Io(err),
    }
}

fn parse_line(line: &str) -> Result<MapRegion> {
    let bad = || Error::BadMaps {
        line: line.to_string(),
    };

    // Cinco campos separados por espaços; o restante é o caminho, com espaços internos.
    let mut rest = line;
    let mut fields = [""; 5];
    for field in &mut fields {
        rest = rest.trim_start();
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        if end == 0 {
            return Err(bad());
        }
        *field = &rest[..end];
        rest = &rest[end..];
    }
    let [range, perms, offset, _device, inode] = fields;

    let (start, end) = range.split_once('-').ok_or_else(bad)?;
    let start = u64::from_str_radix(start, 16).map_err(|_| bad())?;
    let end = u64::from_str_radix(end, 16).map_err(|_| bad())?;
    if start > end {
        return Err(bad());
    }
    let offset = u64::from_str_radix(offset, 16).map_err(|_| bad())?;
    inode.parse::<u64>().map_err(|_| bad())?;

    let flags = perms.as_bytes();
    if flags.len() != 4 {
        return Err(bad());
    }
    let perms = Perms {
        read: flag(flags[0], b'r').ok_or_else(bad)?,
        write: flag(flags[1], b'w').ok_or_else(bad)?,
        exec: flag(flags[2], b'x').ok_or_else(bad)?,
        shared: match flags[3] {
            b's' => true,
            b'p' => false,
            _ => return Err(bad()),
        },
    };

    let rest = rest.trim_start();
    let path = if rest.is_empty() {
        MapPath::Anonymous
    } else if let Some(file) = rest.strip_suffix(" (deleted)") {
        MapPath::File {
            path: PathBuf::from(file),
            deleted: true,
        }
    } else if rest.starts_with('[') {
        MapPath::Pseudo(rest.to_string())
    } else {
        MapPath::File {
            path: PathBuf::from(rest),
            deleted: false,
        }
    };

    Ok(MapRegion {
        start,
        end,
        perms,
        offset,
        path,
    })
}

/// `Some(true)` se o caractere é `expected`, `Some(false)` se é `-`, `None` se é outro.
fn flag(found: u8, expected: u8) -> Option<bool> {
    match found {
        b'-' => Some(false),
        c if c == expected => Some(true),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &str = include_str!("../tests/fixtures/game-maps.txt");

    #[test]
    fn parses_real_game_maps() {
        let regions = parse_maps(REAL).unwrap();
        assert!(regions.len() > 6000);
        let text = regions.iter().find(|r| r.start == 0x140001000).unwrap();
        assert_eq!(text.end, 0x147a90000);
        assert!(text.perms.exec && !text.perms.write);
        assert!(text.is_anonymous());
        let header = regions.iter().find(|r| r.start == 0x140000000).unwrap();
        assert_eq!(header.offset, 0);
        assert_eq!(
            header.file_name().unwrap(),
            "Grounded2Steam-Win64-Shipping.exe"
        );
    }

    #[test]
    fn keeps_spaces_in_paths() {
        let r = parse_maps("7f00-7f01 r--p 00000000 08:02 12 /home/user/.local/share/Steam/steamapps/common/Proton - Experimental/files/lib/wine/x86_64-windows/ntdll.dll\n").unwrap();
        assert!(
            matches!(&r[0].path, MapPath::File { path, deleted: false } if path.ends_with("x86_64-windows/ntdll.dll") && path.to_string_lossy().contains("Proton - Experimental"))
        );
    }

    #[test]
    fn classifies_deleted_and_pseudo_paths() {
        let r = parse_maps(concat!(
            "1000-2000 rw-s 00000000 00:01 5 /memfd:wine-mapping (deleted)\n",
            "2000-3000 rw-p 00000000 00:00 0 [heap]\n",
            "3000-4000 r-xp 00000000 00:00 0 [vdso]\n",
            "4000-5000 rwxp 00000000 00:00 0 [anon:jit]\n",
            "5000-6000 r-xp 00000000 00:00 0 \n",
        ))
        .unwrap();
        assert!(matches!(&r[0].path, MapPath::File { deleted: true, .. }));
        assert!(r[1].is_anonymous());
        assert!(!r[2].is_anonymous());
        assert!(r[3].is_anonymous() && r[3].perms.exec);
        assert!(matches!(r[4].path, MapPath::Anonymous));
    }

    #[test]
    fn rejects_garbage_line() {
        assert!(matches!(
            parse_maps("isso não é maps\n"),
            Err(Error::BadMaps { .. })
        ));
    }

    #[test]
    fn reads_own_process_maps() {
        let regions = read_maps(std::process::id() as i32).unwrap();
        assert!(
            regions
                .iter()
                .any(|r| r.perms.exec && r.file_name().is_some())
        );
    }

    #[test]
    fn maps_errno_to_error_kinds() {
        let esrch = io::Error::from_raw_os_error(Errno::ESRCH as i32);
        assert!(matches!(
            proc_error(7, esrch),
            Error::ProcessGone { pid: 7 }
        ));
        for errno in [Errno::EACCES, Errno::EPERM] {
            let err = io::Error::from_raw_os_error(errno as i32);
            assert!(matches!(
                proc_error(7, err),
                Error::PermissionDenied { pid: 7, .. }
            ));
        }
        let other = io::Error::from_raw_os_error(Errno::EIO as i32);
        assert!(matches!(proc_error(7, other), Error::Io(_)));
    }

    #[test]
    fn missing_process_is_gone() {
        // PIDs acima de pid_max não existem.
        assert!(matches!(
            read_maps(i32::MAX),
            Err(Error::ProcessGone { .. })
        ));
    }
}
