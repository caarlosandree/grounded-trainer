//! Diário de patches: o que foi escrito no jogo, para desfazer depois de uma queda.
//!
//! O diário fica em `journal.json` dentro do diretório de estado do usuário.
//! A trava `journal.lock` (só com a feature `write`) garante um único `gt-patch`.
//! A decisão de recuperação é pura: quem lê o processo vivo passa os dados.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::patch::PatchKind;
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
        parse(&text).map_err(D::Error::custom)
    }

    /// Lê o texto hexadecimal; a mensagem do erro é a do diário.
    pub fn parse(text: &str) -> Result<Vec<u8>, String> {
        let bad = || format!("hex inválido: '{text}'");
        if !text.is_ascii() || !text.len().is_multiple_of(2) {
            return Err(bad());
        }
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(|_| bad()))
            .collect()
    }
}

/// Serialização de bytes opcionais como hexadecimal (`None` não é gravado: use junto de
/// `skip_serializing_if = "Option::is_none"` e `default`).
mod hex_opt {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(bytes: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match bytes {
            Some(bytes) => super::hex_bytes::serialize(bytes, s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        match Option::<String>::deserialize(d)? {
            Some(text) => super::hex_bytes::parse(&text)
                .map(Some)
                .map_err(D::Error::custom),
            None => Ok(None),
        }
    }
}

/// Bytes de uma entrada de dados: lê um número (formato da spec 005, um byte) ou texto
/// hexadecimal; grava sempre hexadecimal.
mod data_bytes {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Num(u8),
        Text(String),
    }

    impl Raw {
        fn into_bytes<E: Error>(self) -> Result<Vec<u8>, E> {
            match self {
                Raw::Num(n) => Ok(vec![n]),
                Raw::Text(text) => super::hex_bytes::parse(&text).map_err(E::custom),
            }
        }
    }

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        super::hex_bytes::serialize(bytes, s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        Raw::deserialize(d)?.into_bytes()
    }

    /// Versão opcional (`None` não é gravado: use com `skip_serializing_if` e `default`).
    pub mod opt {
        use serde::{Deserialize, Deserializer, Serializer};

        pub fn serialize<S: Serializer>(bytes: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
            match bytes {
                Some(bytes) => super::serialize(bytes, s),
                None => s.serialize_none(),
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
            match Option::<super::Raw>::deserialize(d)? {
                Some(raw) => raw.into_bytes().map(Some),
                None => Ok(None),
            }
        }
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
    /// Bytes que o patch tinha no jogo antes da troca em curso (fora de uma troca, os que
    /// ele escreveu). É sempre o valor que estava no jogo antes de uma troca de valor.
    #[serde(with = "hex_bytes")]
    pub replacement: Vec<u8>,
    /// Bytes novos, só durante uma troca de valor (spec 003 6.7); `None` fora dela.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "hex_opt")]
    pub next: Option<Vec<u8>>,
    /// Tipo do patch (spec 008). Diários antigos não têm o campo e leem como `Code`; `Code`
    /// não é gravado.
    #[serde(default, skip_serializing_if = "PatchKind::is_code")]
    pub kind: PatchKind,
}

impl JournalEntry {
    /// Intervalo de endereços coberto pela entrada.
    pub fn range(&self) -> Range<u64> {
        let len = u64::try_from(self.original.len()).unwrap_or(u64::MAX);
        self.addr..self.addr.saturating_add(len)
    }
}

/// Trecho de um objeto do jogo conferido imediatamente antes de uma escrita em dados (vtable,
/// `InternalIndex`, `ClassPrivate`): se os bytes mudaram, o objeto não é mais o mesmo.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct GuardBytes {
    #[serde(with = "hex_addr")]
    pub addr: u64,
    #[serde(with = "hex_bytes")]
    pub bytes: Vec<u8>,
}

/// Como uma entrada de dados foi escrita.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DataMode {
    /// Bits limpos num byte (spec 005): só os bits da máscara são nossos.
    #[default]
    Bits,
    /// Valor inteiro de 1 a 8 bytes (spec 006).
    Whole,
}

/// Escrita em dados do jogo (spec 005, 6.5.2 e 6.6; spec 006, 6.8): uma entrada por cheat e
/// endereço. `original`, `written` e `next` aceitam número (diário da 005) ou hexadecimal na
/// leitura e são gravados em hexadecimal.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DataEntry {
    pub id: String,
    /// Endereço do primeiro byte.
    #[serde(with = "hex_addr")]
    pub addr: u64,
    #[serde(default)]
    pub mode: DataMode,
    /// Bytes antes da primeira escrita neste objeto (`Bits`: 1 byte; `Whole`: 1 a 8).
    #[serde(with = "data_bytes")]
    pub original: Vec<u8>,
    /// Bytes escritos (`Bits`: o original com os bits da máscara limpos).
    #[serde(with = "data_bytes")]
    pub written: Vec<u8>,
    /// Bits nossos (só `Bits`; no `Whole` vale 0).
    #[serde(default)]
    pub mask: u8,
    /// Bytes novos, só durante uma troca de valor; `None` fora dela.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "data_bytes::opt"
    )]
    pub next: Option<Vec<u8>>,
    /// Conferência de identidade do objeto dono dos bytes.
    pub guard: Vec<GuardBytes>,
}

impl DataEntry {
    /// Entrada de bits limpos num byte.
    pub fn bits(
        id: impl Into<String>,
        addr: u64,
        original: u8,
        written: u8,
        mask: u8,
        guard: Vec<GuardBytes>,
    ) -> Self {
        Self {
            id: id.into(),
            addr,
            mode: DataMode::Bits,
            original: vec![original],
            written: vec![written],
            mask,
            next: None,
            guard,
        }
    }

    /// Entrada de valor inteiro (1 a 8 bytes).
    pub fn whole(
        id: impl Into<String>,
        addr: u64,
        original: Vec<u8>,
        written: Vec<u8>,
        guard: Vec<GuardBytes>,
    ) -> Self {
        Self {
            id: id.into(),
            addr,
            mode: DataMode::Whole,
            original,
            written,
            mask: 0,
            next: None,
            guard,
        }
    }
}

/// Identidade de uma entrada de dados no motor: `id` e endereço.
pub fn data_key(e: &DataEntry) -> (&str, u64) {
    (e.id.as_str(), e.addr)
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
    /// Bits limpos em dados (Fase 5). Diários antigos não têm a chave.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data: Vec<DataEntry>,
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

/// O que fazer com uma entrada de dados ao restaurar (desligar, sair ou recuperar).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DataAction {
    /// Já está no original (ou o jogo mudou o campo): só esquecer a entrada.
    Drop,
    /// Escrever estes bytes no endereço da entrada. Em `Bits`, 1 byte: os bits da máscara
    /// voltam ao original e os outros ficam como estão. Em `Whole`, o original inteiro.
    Restore(Vec<u8>),
    /// O objeto não é mais o mesmo, os bytes são ilegíveis ou não são graváveis, ou (em
    /// `Bits`) algum bit da máscara não é nem o nosso nem o original: esquecer sem escrever.
    Discard,
}

/// Decide a restauração de uma entrada de dados (spec 005, 6.6; spec 006, 6.4 e 6.8).
/// `current` são os bytes lidos agora (`None` se ilegíveis), `guard_ok` diz se a conferência de
/// identidade bateu e `writable` se a região é `rw` no `maps` atual.
///
/// `Bits`: só os bits da máscara contam; um bit é "de outro" quando o original e o escrito eram
/// iguais nele e o atual difere. `Whole`: o valor atual igual ao escrito (ou ao `next`) volta ao
/// original; igual ao original, só esquece; qualquer outro valor (inclusive igual em parte) é
/// do jogo: esquecer sem escrever.
pub fn decide_data(
    e: &DataEntry,
    current: Option<&[u8]>,
    guard_ok: bool,
    writable: bool,
) -> DataAction {
    if !well_formed(e) {
        return DataAction::Discard;
    }
    let Some(current) = current else {
        return DataAction::Discard;
    };
    if !guard_ok || !writable {
        return DataAction::Discard;
    }
    match e.mode {
        DataMode::Bits => decide_bits(e, current),
        DataMode::Whole => {
            if current == e.written.as_slice() || e.next.as_deref() == Some(current) {
                DataAction::Restore(e.original.clone())
            } else {
                DataAction::Drop
            }
        }
    }
}

/// Tamanhos coerentes: o serde do diário não valida, e uma entrada malformada nunca pode
/// virar escrita. `Whole`: `original` e `written` com o mesmo tamanho, de 1 a 8 bytes, e o
/// `next` (se houver) também. `Bits`: `original` e `written` com 1 byte.
fn well_formed(e: &DataEntry) -> bool {
    match e.mode {
        DataMode::Bits => e.original.len() == 1 && e.written.len() == 1,
        DataMode::Whole => {
            let len = e.original.len();
            (1..=8).contains(&len)
                && e.written.len() == len
                && e.next.as_ref().is_none_or(|n| n.len() == len)
        }
    }
}

fn decide_bits(e: &DataEntry, current: &[u8]) -> DataAction {
    let (Some(&current), Some(&original), Some(&written)) =
        (current.first(), e.original.first(), e.written.first())
    else {
        return DataAction::Discard;
    };
    let mask = e.mask;
    let same = !(original ^ written);
    if mask & same & (current ^ original) != 0 {
        return DataAction::Discard;
    }
    if current & mask == original & mask {
        DataAction::Drop
    } else {
        DataAction::Restore(vec![(current & !mask) | (original & mask)])
    }
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

/// Classifica os bytes atuais de uma entrada: cada byte precisa ser o original, o de
/// `replacement` ou, numa troca de valor em curso, o de `next` naquela posição.
pub fn decide_entry(e: &JournalEntry, current: &[u8]) -> EntryAction {
    if current == e.original {
        return EntryAction::Drop;
    }
    let next = e.next.as_deref();
    let comparable = current.len() == e.original.len()
        && current.len() == e.replacement.len()
        && next.is_none_or(|n| n.len() == current.len());
    if comparable
        && current.iter().enumerate().all(|(i, c)| {
            *c == e.original[i] || *c == e.replacement[i] || next.is_some_and(|n| *c == n[i])
        })
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
            next: None,
            kind: PatchKind::Code,
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
            data: Vec::new(),
        }
    }

    fn guard() -> Vec<GuardBytes> {
        vec![GuardBytes {
            addr: 0x7fff_0000_0000,
            bytes: vec![0xaa, 0xbb],
        }]
    }

    fn data_entry(original: u8, written: u8, mask: u8) -> DataEntry {
        DataEntry::bits(
            "nao-morrer",
            0x7fff_0000_011d,
            original,
            written,
            mask,
            guard(),
        )
    }

    fn whole_entry(len: usize) -> DataEntry {
        let original: Vec<u8> = (1..=len as u8).collect();
        let written: Vec<u8> = (0x10..0x10 + len as u8).collect();
        DataEntry::whole("carga", 0x7fff_0000_0200, original, written, guard())
    }

    #[test]
    fn decide_data_cases() {
        // Dois bits nossos (0x06) e um alheio (0x01) no mesmo byte.
        let e = data_entry(0x07, 0x01, 0x06);
        // Ainda como escrevemos: os bits voltam, o alheio fica.
        assert_eq!(
            decide_data(&e, Some(&[0x01]), true, true),
            DataAction::Restore(vec![0x07])
        );
        // O jogo mexeu no bit alheio enquanto os nossos estavam limpos: ele é preservado.
        assert_eq!(
            decide_data(&e, Some(&[0x00]), true, true),
            DataAction::Restore(vec![0x06])
        );
        // Um dos nossos já voltou sozinho: só o outro é restaurado.
        assert_eq!(
            decide_data(&e, Some(&[0x03]), true, true),
            DataAction::Restore(vec![0x07])
        );
        // Os dois já originais: só esquecer.
        assert_eq!(decide_data(&e, Some(&[0x06]), true, true), DataAction::Drop);
        assert_eq!(decide_data(&e, Some(&[0x07]), true, true), DataAction::Drop);
        // Objeto trocado, byte ilegível ou fora de região gravável: descartar sem escrever.
        assert_eq!(
            decide_data(&e, Some(&[0x01]), false, true),
            DataAction::Discard
        );
        assert_eq!(decide_data(&e, None, true, true), DataAction::Discard);
        assert_eq!(
            decide_data(&e, Some(&[0x01]), true, false),
            DataAction::Discard
        );
        // Bit da máscara que já era 0 no original e foi ligado por outro: não é nosso.
        let partial = data_entry(0x04, 0x00, 0x06);
        assert_eq!(
            decide_data(&partial, Some(&[0x02]), true, true),
            DataAction::Discard
        );
        assert_eq!(
            decide_data(&partial, Some(&[0x00]), true, true),
            DataAction::Restore(vec![0x04])
        );
        // `bool` nativo: máscara inteira.
        let native = data_entry(0x01, 0x00, 0xff);
        assert_eq!(
            decide_data(&native, Some(&[0x00]), true, true),
            DataAction::Restore(vec![0x01])
        );
        assert_eq!(
            decide_data(&native, Some(&[0x01]), true, true),
            DataAction::Drop
        );
        assert_eq!(
            decide_data(&native, Some(&[0x02]), true, true),
            DataAction::Discard
        );
    }

    #[test]
    fn journal_without_data_still_loads() {
        let j = journal(vec![entry(&[0x48], &[0x90])]);
        let text = serde_json::to_string(&j).unwrap();
        assert!(!text.contains("\"data\""), "{text}");
        // Diário antigo (sem a chave) carrega com `data` vazio.
        let back: Journal = serde_json::from_str(&text).unwrap();
        assert!(back.data.is_empty());
        // Com dados, a chave aparece e o endereço é hexadecimal.
        let with = Journal {
            data: vec![data_entry(0x07, 0x01, 0x06)],
            ..j
        };
        let text = serde_json::to_string(&with).unwrap();
        assert!(text.contains("\"data\""), "{text}");
        assert!(text.contains("\"0x7fff0000011d\""), "{text}");
        assert_eq!(serde_json::from_str::<Journal>(&text).unwrap(), with);
    }

    #[test]
    fn data_entries_other_process_discarded() {
        let j = Journal {
            data: vec![data_entry(0x07, 0x01, 0x06)],
            ..journal(Vec::new())
        };
        let never = |_: &JournalEntry| -> Option<Vec<u8>> { panic!("nada deve ser lido") };
        assert_eq!(
            decide(&j, "boot-a", stat('S', 999), never),
            Recovery::Discard(DiscardReason::OtherProcess)
        );
        assert_eq!(
            decide(&j, "boot-b", stat('S', 1000), never),
            Recovery::Discard(DiscardReason::OtherBoot)
        );
        assert_eq!(
            decide(&j, "boot-a", None, never),
            Recovery::Discard(DiscardReason::ProcessGone)
        );
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
    fn journal_entry_without_kind_reads_as_code() {
        let old = r#"{"id":"x","addr":"0x10","original":"488bf9","replacement":"909090"}"#;
        let e: JournalEntry = serde_json::from_str(old).unwrap();
        assert_eq!(e.kind, PatchKind::Code);
        // `Code` continua sem o campo no arquivo; `Cave` grava `kind`.
        let text = serde_json::to_string(&e).unwrap();
        assert!(!text.contains("kind"), "{text}");
        let cave = JournalEntry {
            kind: PatchKind::Cave,
            ..e
        };
        let text = serde_json::to_string(&cave).unwrap();
        assert!(text.contains(r#""kind":"cave""#), "{text}");
        let back: JournalEntry = serde_json::from_str(&text).unwrap();
        assert_eq!(back, cave);
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
    fn decide_entry_with_next() {
        let mut e = entry(&[1, 2], &[3, 4]);
        e.next = Some(vec![5, 6]);
        assert_eq!(decide_entry(&e, &[5, 6]), EntryAction::Restore);
        assert_eq!(decide_entry(&e, &[3, 6]), EntryAction::Restore);
        assert_eq!(decide_entry(&e, &[1, 6]), EntryAction::Restore);
        assert_eq!(decide_entry(&e, &[7, 6]), EntryAction::Foreign);
        assert_eq!(decide_entry(&e, &[1, 2]), EntryAction::Drop);
        // Sem `next`, o byte do valor novo não vale.
        e.next = None;
        assert_eq!(decide_entry(&e, &[5, 6]), EntryAction::Foreign);
    }

    #[test]
    fn phase2_journal_still_loads() {
        // Formato gravado pela Fase 2: sem a chave `next`.
        let text = r#"{
  "boot_id": "boot-a",
  "pid": 4242,
  "start_time": 1000,
  "version_key": "c935e799-0a1b2c",
  "paused_by_us": false,
  "entries": [
    { "id": "god_mode", "addr": "0x140123456", "original": "488bf9", "replacement": "909090" }
  ]
}"#;
        let j: Journal = serde_json::from_str(text).unwrap();
        assert_eq!(j.entries[0].next, None);
        assert_eq!(j.entries[0].replacement, vec![0x90; 3]);
        // Sem `next`, salvar não escreve a chave.
        let saved = serde_json::to_string(&j).unwrap();
        assert!(!saved.contains("next"), "{saved}");
        // Com `next`, vai em hexadecimal e volta igual.
        let mut with_next = j.clone();
        with_next.entries[0].next = Some(vec![0x66, 0x90, 0x90]);
        let saved = serde_json::to_string(&with_next).unwrap();
        assert!(saved.contains("\"next\":\"669090\""), "{saved}");
        assert_eq!(serde_json::from_str::<Journal>(&saved).unwrap(), with_next);
        // Hexadecimal inválido em `next` é recusado.
        let bad = saved.replace("669090", "66909");
        assert!(serde_json::from_str::<Journal>(&bad).is_err());
    }

    #[test]
    fn entry_range_covers_original() {
        let e = entry(&[1, 2, 3], &[4, 5, 6]);
        assert_eq!(e.range(), 0x1_4012_3456..0x1_4012_3459);
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

    #[test]
    fn journal_005_bits_entry_restores() {
        let json = r#"{"id":"nao-morrer","addr":"0x7fff0000011d","original":6,"written":0,
            "mask":6,"guard":[{"addr":"0x7fff00000000","bytes":"aabb"}]}"#;
        let e: DataEntry = serde_json::from_str(json).unwrap();
        assert_eq!(e.mode, DataMode::Bits);
        assert_eq!(e.original, vec![0x06]);
        assert_eq!(e.written, vec![0x00]);
        assert_eq!(e.mask, 0x06);
        assert_eq!(e.next, None);
        assert_eq!(
            decide_data(&e, Some(&[0x00]), true, true),
            DataAction::Restore(vec![0x06])
        );
    }

    #[test]
    fn whole_entry_roundtrip_hex() {
        let mut e = whole_entry(4);
        e.next = Some(vec![0xde, 0xad, 0xbe, 0xef]);
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""original":"01020304""#), "{json}");
        assert!(json.contains(r#""written":"10111213""#), "{json}");
        assert!(json.contains(r#""next":"deadbeef""#), "{json}");
        assert!(json.contains(r#""mode":"whole""#), "{json}");
        assert!(json.contains(r#""mask":0"#), "{json}");
        let back: DataEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back, e);
        // Sem `next`, a chave não é gravada.
        let plain = serde_json::to_string(&whole_entry(2)).unwrap();
        assert!(!plain.contains("next"), "{plain}");
    }

    #[test]
    fn decide_whole_cases() {
        let mut e = whole_entry(4);
        e.next = Some(vec![0xaa, 0xaa, 0xaa, 0xaa]);
        let orig = vec![1, 2, 3, 4];
        // Igual ao escrito: volta ao original.
        assert_eq!(
            decide_data(&e, Some(&[0x10, 0x11, 0x12, 0x13]), true, true),
            DataAction::Restore(orig.clone())
        );
        // Igual ao `next`: volta ao original.
        assert_eq!(
            decide_data(&e, Some(&[0xaa; 4]), true, true),
            DataAction::Restore(orig)
        );
        // Igual ao original: só esquece.
        assert_eq!(
            decide_data(&e, Some(&[1, 2, 3, 4]), true, true),
            DataAction::Drop
        );
        // Três dos quatro bytes iguais ao escrito: o jogo mexeu, esquece sem escrever.
        assert_eq!(
            decide_data(&e, Some(&[0x10, 0x11, 0x12, 0x99]), true, true),
            DataAction::Drop
        );
        // Guarda ruim, região ruim e leitura ilegível.
        assert_eq!(
            decide_data(&e, Some(&[0x10, 0x11, 0x12, 0x13]), false, true),
            DataAction::Discard
        );
        assert_eq!(
            decide_data(&e, Some(&[0x10, 0x11, 0x12, 0x13]), true, false),
            DataAction::Discard
        );
        assert_eq!(decide_data(&e, None, true, true), DataAction::Discard);
    }

    #[test]
    fn whole_entry_sizes_1_4_8() {
        for len in [1usize, 4, 8] {
            let e = whole_entry(len);
            assert_eq!(
                decide_data(&e, Some(&e.written), true, true),
                DataAction::Restore(e.original.clone()),
                "tamanho {len}"
            );
            assert_eq!(
                decide_data(&e, Some(&e.original), true, true),
                DataAction::Drop,
                "tamanho {len}"
            );
            let mut other = e.written.clone();
            other[0] ^= 0xff;
            assert_eq!(
                decide_data(&e, Some(&other), true, true),
                DataAction::Drop,
                "tamanho {len}"
            );
            let json = serde_json::to_string(&e).unwrap();
            assert_eq!(serde_json::from_str::<DataEntry>(&json).unwrap(), e);
        }
    }

    #[test]
    fn decide_malformed_entry_discards() {
        let cur4 = [0x10, 0x11, 0x12, 0x13];
        // Whole: original e written de tamanhos diferentes.
        let mut e = whole_entry(4);
        e.written.pop();
        assert_eq!(
            decide_data(&e, Some(&cur4), true, true),
            DataAction::Discard
        );
        // Whole: tamanho 0 e maior que 8.
        for len in [0usize, 9] {
            let mut e = whole_entry(4);
            e.original = vec![1; len];
            e.written = vec![2; len];
            assert_eq!(
                decide_data(&e, Some(&vec![2; len]), true, true),
                DataAction::Discard,
                "tamanho {len}"
            );
        }
        // Whole: `next` de outro tamanho.
        let mut e = whole_entry(4);
        e.next = Some(vec![0xaa; 2]);
        assert_eq!(
            decide_data(&e, Some(&[0xaa; 2]), true, true),
            DataAction::Discard
        );
        assert_eq!(
            decide_data(&e, Some(&cur4), true, true),
            DataAction::Discard
        );
        // Bits: original ou written com tamanho diferente de 1.
        let mut e = data_entry(0x07, 0x01, 0x06);
        e.original = vec![0x07, 0x00];
        assert_eq!(
            decide_data(&e, Some(&[0x01]), true, true),
            DataAction::Discard
        );
        let mut e = data_entry(0x07, 0x01, 0x06);
        e.written = Vec::new();
        assert_eq!(
            decide_data(&e, Some(&[0x01]), true, true),
            DataAction::Discard
        );
        // Entrada sã continua restaurando.
        let e = whole_entry(4);
        assert!(matches!(
            decide_data(&e, Some(&cur4), true, true),
            DataAction::Restore(_)
        ));
    }

    #[test]
    fn data_key_is_id_and_addr() {
        let e = data_entry(0x06, 0x00, 0x06);
        assert_eq!(data_key(&e), ("nao-morrer", 0x7fff_0000_011d));
    }
}
