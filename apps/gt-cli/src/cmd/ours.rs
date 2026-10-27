//! O diário do `gt-patch` visto de fora (spec 008, 6.10): o `gt diff` e o `gt caves` leem o diário
//! **sem a trava** (`JournalStore::peek`, nada é movido nem escrito) e só o usam se ele é do mesmo
//! processo (boot, PID e instante de início iguais aos do alvo). Assim marcam o que é nosso.

use gt_core::journal::{Journal, JournalStore, Loaded};
use gt_core::procinfo::Identity;

/// O que o diário diz sobre o processo alvo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Diary {
    /// Diário do mesmo processo.
    Same(Journal),
    /// Há diário, mas é de outro processo (ou de outro boot): não vale para este alvo.
    OtherProcess,
    /// Não há diário: nada nosso no jogo.
    Missing,
    /// O arquivo existe e não pôde ser lido (corrompido, sem permissão, ...).
    Unreadable,
}

impl Diary {
    /// O diário, se ele vale para este processo.
    pub fn journal(&self) -> Option<&Journal> {
        match self {
            Diary::Same(journal) => Some(journal),
            _ => None,
        }
    }

    /// Frase curta para os relatórios.
    pub fn note(&self) -> &'static str {
        match self {
            Diary::Same(_) => "diário do processo atual",
            Diary::OtherProcess => "diário de outro processo (ignorado)",
            Diary::Missing => "sem diário (nada nosso no jogo)",
            Diary::Unreadable => "diário ilegível (ignorado)",
        }
    }
}

/// O diário é do processo `id`: mesmo boot, PID e instante de início.
pub fn same_process(journal: &Journal, id: &Identity) -> bool {
    journal.boot_id == id.boot_id && journal.pid == id.pid && journal.start_time == id.start_time
}

/// Lê o diário de `store` sem a trava e confere a identidade com `id`.
pub fn read(store: &JournalStore, id: &Identity) -> Diary {
    match store.peek() {
        Ok(Loaded::Ok(journal)) if same_process(&journal, id) => Diary::Same(journal),
        Ok(Loaded::Ok(_)) => Diary::OtherProcess,
        Ok(Loaded::Missing) => Diary::Missing,
        Ok(Loaded::Corrupt { .. }) => Diary::Unreadable,
        Err(err) => {
            tracing::warn!("não consegui ler o diário: {err}");
            Diary::Unreadable
        }
    }
}

/// Como [`read`], no diretório de estado padrão do usuário (`XDG_STATE_HOME`).
pub fn read_default(id: &Identity) -> Diary {
    match JournalStore::default_dir() {
        Some(dir) => read(&JournalStore::new(dir), id),
        None => Diary::Missing,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn journal(pid: i32, start_time: u64, boot: &str) -> Journal {
        Journal {
            boot_id: boot.into(),
            pid,
            start_time,
            version_key: "teste".into(),
            paused_by_us: false,
            entries: Vec::new(),
            data: Vec::new(),
            vars: Vec::new(),
        }
    }

    fn id() -> Identity {
        Identity {
            pid: 10,
            start_time: 99,
            boot_id: "boot-a".into(),
        }
    }

    #[test]
    fn only_the_same_process_counts() {
        let dir = tempfile::tempdir().unwrap();
        let store = JournalStore::new(dir.path().join("state"));
        assert_eq!(read(&store, &id()), Diary::Missing);

        store.save(&journal(10, 99, "boot-a")).unwrap();
        assert!(matches!(read(&store, &id()), Diary::Same(_)));
        // PID, início ou boot diferentes: é de outro processo.
        for other in [
            journal(11, 99, "boot-a"),
            journal(10, 98, "boot-a"),
            journal(10, 99, "boot-b"),
        ] {
            store.save(&other).unwrap();
            assert_eq!(read(&store, &id()), Diary::OtherProcess);
        }
        // Ler não escreve: o arquivo corrompido continua onde está.
        let path = dir.path().join("state/journal.json");
        std::fs::write(&path, "{ não é json").unwrap();
        assert_eq!(read(&store, &id()), Diary::Unreadable);
        assert!(path.exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ não é json");
    }
}
