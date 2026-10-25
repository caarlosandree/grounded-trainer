//! Diário de patches: o que foi escrito no jogo, para desfazer depois de uma queda.
//!
//! O diário fica em `journal.json` dentro do diretório de estado do usuário.
//! A trava `journal.lock` (só com a feature `write`) garante um único `gt-patch`.
//! A decisão de recuperação é pura: quem lê o processo vivo passa os dados.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::procinfo::{ProcStat, is_dead, is_stopped};
use crate::{Error, Result};

const JOURNAL_FILE: &str = "journal.json";
const BAD_FILE: &str = "journal.json.bad";
const TMP_FILE: &str = "journal.json.tmp";
#[cfg(feature = "write")]
const LOCK_FILE: &str = "journal.lock";

/// Serialização de bytes como texto hexadecimal sem espaços (`"488bf9"`).
mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        let mut text = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            text.push_str(&format!("{b:02x}"));
        }
        s.serialize_str(&text)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        if !text.is_ascii() || text.len() % 2 != 0 {
            return Err(D::Error::custom(format!("hex inválido: '{text}'")));
        }
        (0..text.len())
            .step_by(2)
            .map(|i| {
                u8::from_str_radix(&text[i..i + 2], 16)
                    .map_err(|_| D::Error::custom(format!("hex inválido: '{text}'")))
            })
            .collect()
    }
}

/// Serialização de endereço como `"0x..."`.
mod hex_addr {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(addr: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("{addr:#x}"))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        let text = String::deserialize(d)?;
        text.strip_prefix("0x")
            .and_then(|h| u64::from_str_radix(h, 16).ok())
            .ok_or_else(|| D::Error::custom(format!("endereço inválido: '{text}'")))
    }
}

/// Um patch escrito no jogo e ainda não desfeito.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct JournalEntry {
    pub id: String,
    #[serde(with = "hex_addr")]
    pub addr: u64,
    /// Bytes antes do patch.
    #[serde(with = "hex_bytes")]
    pub original: Vec<u8>,
    /// Bytes escritos pelo patch.
    #[serde(with = "hex_bytes")]
    pub replacement: Vec<u8>,
}

/// Conteúdo do diário: identidade do processo e os patches ativos.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Journal {
    pub boot_id: String,
    pub pid: i32,
    pub start_time: u64,
    pub version_key: String,
    /// O `gt-patch` pausou o jogo e ainda não o retomou.
    pub paused_by_us: bool,
    pub entries: Vec<JournalEntry>,
}

/// Resultado da leitura do diário.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Loaded {
    Missing,
    Ok(Journal),
    /// Ilegível; o arquivo foi (ou, no `peek`, seria) movido para `moved_to`.
    Corrupt {
        moved_to: PathBuf,
    },
}

/// Diretório de estado padrão a partir do ambiente.
///
/// `$XDG_STATE_HOME` só vale se for absoluto (regra da especificação XDG).
pub fn default_dir_from(xdg: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let absolute = |v: Option<OsString>| v.map(PathBuf::from).filter(|p| p.is_absolute());
    if let Some(base) = absolute(xdg) {
        return Some(base.join("grounded-trainer"));
    }
    absolute(home).map(|h| h.join(".local/state/grounded-trainer"))
}

/// Acesso ao diretório do diário.
#[derive(Clone)]
pub struct JournalStore {
    dir: PathBuf,
}

impl JournalStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Diretório padrão (`$XDG_STATE_HOME/grounded-trainer` ou `~/.local/state/...`).
    pub fn default_dir() -> Option<PathBuf> {
        default_dir_from(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
    }

    fn journal_path(&self) -> PathBuf {
        self.dir.join(JOURNAL_FILE)
    }

    fn read(&self, quarantine: bool) -> Result<Loaded> {
        let path = self.journal_path();
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Loaded::Missing),
            // Bytes que não são UTF-8 também contam como diário corrompido.
            Err(e) if e.kind() == io::ErrorKind::InvalidData => String::new(),
            Err(e) => return Err(e.into()),
        };
        match serde_json::from_str::<Journal>(&text) {
            Ok(j) => Ok(Loaded::Ok(j)),
            Err(e) => {
                let moved_to = self.dir.join(BAD_FILE);
                tracing::warn!("diário ilegível ({e}); veja {}", moved_to.display());
                if quarantine {
                    fs::rename(&path, &moved_to)?;
                }
                Ok(Loaded::Corrupt { moved_to })
            }
        }
    }

    /// Lê o diário. Se estiver corrompido, move-o para `journal.json.bad`.
    pub fn load(&self) -> Result<Loaded> {
        self.read(true)
    }

    /// Como [`load`](Self::load), mas sem mover o arquivo corrompido (não escreve nada).
    pub fn peek(&self) -> Result<Loaded> {
        self.read(false)
    }

    /// Grava o diário de forma atômica: temporário, `fsync`, `rename`, `fsync` do diretório.
    ///
    /// Erros de E/S viram [`Error::Journal`] com o caminho do diário.
    pub fn save(&self, j: &Journal) -> Result<()> {
        let text = serde_json::to_string_pretty(j)
            .map_err(|e| Error::Journal(format!("serialização: {e}")))?;
        let tmp = self.dir.join(TMP_FILE);
        let write = || -> io::Result<()> {
            fs::create_dir_all(&self.dir)?;
            let mut file = File::create(&tmp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            drop(file);
            fs::rename(&tmp, self.journal_path())?;
            sync_dir(&self.dir)
        };
        write().map_err(|e| self.io_error("gravação", e))
    }

    /// Apaga o diário. Sem arquivo, não faz nada.
    ///
    /// Erros de E/S viram [`Error::Journal`] com o caminho do diário.
    pub fn clear(&self) -> Result<()> {
        match fs::remove_file(self.journal_path()) {
            Ok(()) => sync_dir(&self.dir).map_err(|e| self.io_error("remoção", e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(self.io_error("remoção", e)),
        }
    }

    fn io_error(&self, what: &str, e: io::Error) -> Error {
        Error::Journal(format!("{what} de {}: {e}", self.journal_path().display()))
    }
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Trava exclusiva do diário; solta ao cair.
#[cfg(feature = "write")]
pub struct JournalLock {
    _file: File,
}

#[cfg(feature = "write")]
fn whole_file_lock() -> libc::flock {
    libc::flock {
        l_type: libc::F_WRLCK as _,
        l_whence: libc::SEEK_SET as _,
        l_start: 0,
        l_len: 0,
        l_pid: 0,
    }
}

#[cfg(feature = "write")]
impl JournalStore {
    /// Pega a trava exclusiva (`F_OFD_SETLK`). Já ocupada: [`Error::AlreadyRunning`].
    pub fn lock(&self) -> Result<JournalLock> {
        use nix::errno::Errno;
        use nix::fcntl::{FcntlArg, fcntl};

        fs::create_dir_all(&self.dir)?;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.dir.join(LOCK_FILE))?;
        let request = whole_file_lock();
        match fcntl(&file, FcntlArg::F_OFD_SETLK(&request)) {
            Ok(_) => Ok(JournalLock { _file: file }),
            Err(Errno::EAGAIN | Errno::EACCES) => Err(Error::AlreadyRunning),
            Err(e) => Err(Error::Journal(format!("trava: {e}"))),
        }
    }

    /// Diz se alguém segura a trava, sem segurá-la (`F_OFD_GETLK`). Sem arquivo: `false`.
    pub fn is_locked(&self) -> Result<bool> {
        use nix::fcntl::{FcntlArg, fcntl};

        let file = match File::open(self.dir.join(LOCK_FILE)) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        let mut probe = whole_file_lock();
        fcntl(&file, FcntlArg::F_OFD_GETLK(&mut probe))
            .map_err(|e| Error::Journal(format!("consulta da trava: {e}")))?;
        Ok(i32::from(probe.l_type) != libc::F_UNLCK)
    }
}

/// O que fazer com uma entrada do diário na recuperação.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryAction {
    /// Os bytes já são os originais: só esquecer a entrada.
    Drop,
    /// Cada byte é o original ou o do patch: reescrever os originais.
    Restore,
    /// Há bytes que não são nossos (ou ilegíveis): não tocar.
    Foreign,
}

/// Por que o diário foi descartado sem escrever nada.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscardReason {
    OtherBoot,
    ProcessGone,
    OtherProcess,
}

/// Decisão de recuperação para um diário.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recovery {
    Nothing,
    Discard(DiscardReason),
    SameProcess {
        /// O jogo está parado por nós: retomar com SIGCONT.
        resume: bool,
        /// O jogo está parado, mas não fomos nós.
        stopped_by_other: bool,
        actions: Vec<(JournalEntry, EntryAction)>,
    },
}

/// Classifica os bytes atuais de uma entrada.
pub fn decide_entry(e: &JournalEntry, current: &[u8]) -> EntryAction {
    if current == e.original {
        return EntryAction::Drop;
    }
    let comparable = current.len() == e.original.len() && current.len() == e.replacement.len();
    if comparable
        && current
            .iter()
            .zip(e.original.iter().zip(&e.replacement))
            .all(|(c, (o, n))| c == o || c == n)
    {
        EntryAction::Restore
    } else {
        EntryAction::Foreign
    }
}

/// Decide a recuperação de `j`, dado o boot atual, o processo vivo e a leitura dos bytes.
///
/// A versão do jogo não muda dentro do mesmo processo; por isso basta comparar
/// `boot_id` e `start_time`.
pub fn decide(
    j: &Journal,
    live_boot: &str,
    live: Option<ProcStat>,
    current: impl Fn(&JournalEntry) -> Option<Vec<u8>>,
) -> Recovery {
    if j.boot_id != live_boot {
        return Recovery::Discard(DiscardReason::OtherBoot);
    }
    let Some(stat) = live.filter(|s| !is_dead(s.state)) else {
        return Recovery::Discard(DiscardReason::ProcessGone);
    };
    if stat.start_time != j.start_time {
        return Recovery::Discard(DiscardReason::OtherProcess);
    }
    let stopped = is_stopped(stat.state);
    let actions = j
        .entries
        .iter()
        .map(|e| {
            let action = match current(e) {
                Some(bytes) => decide_entry(e, &bytes),
                None => EntryAction::Foreign,
            };
            (e.clone(), action)
        })
        .collect();
    Recovery::SameProcess {
        resume: stopped && j.paused_by_us,
        stopped_by_other: stopped && !j.paused_by_us,
        actions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(original: &[u8], replacement: &[u8]) -> JournalEntry {
        JournalEntry {
            id: "god_mode".into(),
            addr: 0x1_4012_3456,
            original: original.to_vec(),
            replacement: replacement.to_vec(),
        }
    }

    fn journal(entries: Vec<JournalEntry>) -> Journal {
        Journal {
            boot_id: "boot-a".into(),
            pid: 4242,
            start_time: 1000,
            version_key: "c935e799-0a1b2c".into(),
            paused_by_us: false,
            entries,
        }
    }

    fn stat(state: char, start_time: u64) -> Option<ProcStat> {
        Some(ProcStat { state, start_time })
    }

    #[test]
    fn json_round_trip_uses_hex() {
        let j = journal(vec![entry(&[0x48, 0x8b, 0xf9], &[0x90, 0x90, 0x90])]);
        let text = serde_json::to_string(&j).unwrap();
        assert!(text.contains("\"488bf9\""), "{text}");
        assert!(text.contains("\"909090\""), "{text}");
        assert!(text.contains("\"0x140123456\""), "{text}");
        let back: Journal = serde_json::from_str(&text).unwrap();
        assert_eq!(back, j);
    }

    #[test]
    fn bad_hex_is_rejected() {
        let text = r#"{"id":"x","addr":"0x10","original":"4","replacement":"90"}"#;
        assert!(serde_json::from_str::<JournalEntry>(text).is_err());
        let text = r#"{"id":"x","addr":"0x10","original":"zz","replacement":"90"}"#;
        assert!(serde_json::from_str::<JournalEntry>(text).is_err());
        let text = r#"{"id":"x","addr":"10","original":"48","replacement":"90"}"#;
        assert!(serde_json::from_str::<JournalEntry>(text).is_err());
    }

    #[test]
    fn decide_entry_cases() {
        let e = entry(&[1, 2, 3, 4], &[9, 9, 9, 9]);
        assert_eq!(decide_entry(&e, &[1, 2, 3, 4]), EntryAction::Drop);
        assert_eq!(decide_entry(&e, &[9, 9, 9, 9]), EntryAction::Restore);
        assert_eq!(decide_entry(&e, &[1, 2, 9, 9]), EntryAction::Restore);
        assert_eq!(decide_entry(&e, &[1, 2, 9, 4]), EntryAction::Restore);
        assert_eq!(decide_entry(&e, &[1, 2, 7, 4]), EntryAction::Foreign);
        assert_eq!(decide_entry(&e, &[1, 2, 3]), EntryAction::Foreign);
    }

    #[test]
    fn decide_cases() {
        let e = entry(&[1, 2], &[9, 9]);
        let j = journal(vec![e.clone()]);
        let orig = |_: &JournalEntry| Some(vec![1, 2]);
        let patched = |_: &JournalEntry| Some(vec![9, 9]);

        assert_eq!(
            decide(&j, "boot-b", stat('S', 1000), orig),
            Recovery::Discard(DiscardReason::OtherBoot)
        );
        assert_eq!(
            decide(&j, "boot-a", None, orig),
            Recovery::Discard(DiscardReason::ProcessGone)
        );
        assert_eq!(
            decide(&j, "boot-a", stat('Z', 1000), orig),
            Recovery::Discard(DiscardReason::ProcessGone)
        );
        assert_eq!(
            decide(&j, "boot-a", stat('S', 2000), orig),
            Recovery::Discard(DiscardReason::OtherProcess)
        );
        assert_eq!(
            decide(&j, "boot-a", stat('S', 1000), patched),
            Recovery::SameProcess {
                resume: false,
                stopped_by_other: false,
                actions: vec![(e.clone(), EntryAction::Restore)],
            }
        );
        assert_eq!(
            decide(&j, "boot-a", stat('S', 1000), orig),
            Recovery::SameProcess {
                resume: false,
                stopped_by_other: false,
                actions: vec![(e.clone(), EntryAction::Drop)],
            }
        );

        // Parado: com marcador, nós retomamos; sem marcador, foi outro.
        let mut ours = j.clone();
        ours.paused_by_us = true;
        assert_eq!(
            decide(&ours, "boot-a", stat('T', 1000), patched),
            Recovery::SameProcess {
                resume: true,
                stopped_by_other: false,
                actions: vec![(e.clone(), EntryAction::Restore)],
            }
        );
        assert_eq!(
            decide(&j, "boot-a", stat('T', 1000), patched),
            Recovery::SameProcess {
                resume: false,
                stopped_by_other: true,
                actions: vec![(e.clone(), EntryAction::Restore)],
            }
        );
        // Marcador sem estar parado: nada a retomar.
        assert_eq!(
            decide(&ours, "boot-a", stat('S', 1000), patched),
            Recovery::SameProcess {
                resume: false,
                stopped_by_other: false,
                actions: vec![(e.clone(), EntryAction::Restore)],
            }
        );

        // Bytes ilegíveis viram Foreign.
        assert_eq!(
            decide(&j, "boot-a", stat('S', 1000), |_| None),
            Recovery::SameProcess {
                resume: false,
                stopped_by_other: false,
                actions: vec![(e, EntryAction::Foreign)],
            }
        );
    }

    #[test]
    fn default_dir_from_cases() {
        use std::ffi::OsString;
        let os = |s: &str| Some(OsString::from(s));
        assert_eq!(
            default_dir_from(os("/x/state"), os("/home/u")),
            Some(PathBuf::from("/x/state/grounded-trainer"))
        );
        // XDG relativo é ignorado.
        assert_eq!(
            default_dir_from(os("rel/state"), os("/home/u")),
            Some(PathBuf::from("/home/u/.local/state/grounded-trainer"))
        );
        assert_eq!(
            default_dir_from(os(""), os("/home/u")),
            Some(PathBuf::from("/home/u/.local/state/grounded-trainer"))
        );
        assert_eq!(
            default_dir_from(None, os("/home/u")),
            Some(PathBuf::from("/home/u/.local/state/grounded-trainer"))
        );
        assert_eq!(default_dir_from(None, None), None);
        assert_eq!(default_dir_from(None, os("rel")), None);
    }

    #[test]
    fn save_load_clear() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path().join("novo/sub"));
        assert_eq!(store.load().unwrap(), Loaded::Missing);
        let j = journal(vec![entry(&[1, 2], &[9, 9])]);
        store.save(&j).unwrap();
        assert_eq!(store.load().unwrap(), Loaded::Ok(j.clone()));
        assert_eq!(store.peek().unwrap(), Loaded::Ok(j));
        // Sem temporário esquecido.
        let names: Vec<_> = std::fs::read_dir(tmp.path().join("novo/sub"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("journal.json")]);
        store.clear().unwrap();
        assert_eq!(store.load().unwrap(), Loaded::Missing);
        store.clear().unwrap();
    }

    #[test]
    fn corrupt_is_moved_to_bad() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path().to_path_buf());
        std::fs::write(tmp.path().join("journal.json"), "{").unwrap();
        let bad = tmp.path().join("journal.json.bad");

        // `peek` não escreve nada.
        assert_eq!(
            store.peek().unwrap(),
            Loaded::Corrupt {
                moved_to: bad.clone()
            }
        );
        assert!(tmp.path().join("journal.json").exists());
        assert!(!bad.exists());

        assert_eq!(
            store.load().unwrap(),
            Loaded::Corrupt {
                moved_to: bad.clone()
            }
        );
        assert!(bad.exists());
        assert!(!tmp.path().join("journal.json").exists());
        assert_eq!(store.load().unwrap(), Loaded::Missing);
    }

    #[cfg(feature = "write")]
    #[test]
    fn lock_is_exclusive_and_query_does_not_hold() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path().join("d"));
        assert!(!store.is_locked().unwrap());
        let guard = store.lock().unwrap();
        assert!(matches!(store.lock(), Err(crate::Error::AlreadyRunning)));
        assert!(store.is_locked().unwrap());
        // Consultar não segura a trava.
        assert!(store.is_locked().unwrap());
        drop(guard);
        assert!(!store.is_locked().unwrap());
        let again = store.lock().unwrap();
        drop(again);
    }
}
