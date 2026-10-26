//! Resumo e código de saída do `gt-tui` (spec 003, 6.8, passo 3).

use gt_core::journal::{JournalStore, Loaded};

/// Código de saída quando a TUI terminou por pânico (o mesmo que o Rust usa).
pub const PANIC_CODE: i32 = 101;

/// O que imprimir na saída e com que código sair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// Linhas do resumo.
    pub lines: Vec<String>,
    /// Código de saída.
    pub code: i32,
}

/// Monta o resumo. `restored` e `foreign` vêm do `undo_all`; o que ficou no jogo sai de uma
/// leitura nova do diário (`peek`), depois do `drop` do motor. `logged` diz se o log em arquivo
/// foi aberto (só então a mensagem de pânico manda olhar o log).
pub fn summarize(
    store: &JournalStore,
    restored: &[String],
    foreign: &[String],
    panicked: bool,
    logged: bool,
) -> Summary {
    let mut lines: Vec<String> = restored
        .iter()
        .map(|id| format!("restaurado: {id}"))
        .collect();
    lines.extend(
        foreign
            .iter()
            .map(|id| format!("não restaurado: {id} (bytes alterados por outro)")),
    );
    let mut left = false;
    match store.peek() {
        Ok(Loaded::Missing) => {}
        Ok(Loaded::Ok(journal)) => {
            for entry in &journal.entries {
                left = true;
                lines.push(format!(
                    "não foi possível restaurar {} agora; a próxima execução do gt-tui (ou gt-patch restore) vai restaurar",
                    entry.id
                ));
            }
            if journal.paused_by_us {
                left = true;
                lines.push(format!(
                    "o jogo pode ter ficado parado: kill -CONT {}",
                    journal.pid
                ));
            }
        }
        Ok(Loaded::Corrupt { moved_to }) => {
            left = true;
            lines.push(format!(
                "diário ilegível ({}); o jogo pode ter patches sem registro, confira com gt diff",
                moved_to.display()
            ));
        }
        Err(e) => {
            left = true;
            lines.push(format!("não foi possível ler o diário: {e}"));
        }
    }
    if panicked {
        let hint = if logged {
            "veja o log em arquivo"
        } else {
            "veja a mensagem acima"
        };
        lines.push(format!(
            "o gt-tui terminou por um erro interno (pânico); {hint}"
        ));
    }
    let code = if panicked {
        PANIC_CODE
    } else {
        i32::from(left)
    };
    Summary { lines, code }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use gt_core::journal::{Journal, JournalEntry};

    fn journal(entries: Vec<JournalEntry>, paused_by_us: bool) -> Journal {
        Journal {
            boot_id: "boot".into(),
            pid: 4242,
            start_time: 1,
            version_key: "v1".into(),
            paused_by_us,
            entries,
        }
    }

    fn entry(id: &str) -> JournalEntry {
        JournalEntry {
            id: id.into(),
            addr: 0x1000,
            original: vec![0x90],
            replacement: vec![0xcc],
            next: None,
        }
    }

    fn store_with(j: Option<Journal>) -> (tempfile::TempDir, JournalStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = JournalStore::new(dir.path().to_path_buf());
        if let Some(j) = j {
            store.save(&j).unwrap();
        }
        (dir, store)
    }

    #[test]
    fn summary_codes() {
        // Diário vazio: tudo restaurado, código 0.
        let (_d, store) = store_with(None);
        let s = summarize(&store, &["a".to_string()], &[], false, true);
        assert_eq!(s.code, 0);
        assert!(
            s.lines.iter().any(|l| l == "restaurado: a"),
            "{:?}",
            s.lines
        );
        assert!(!s.lines.iter().any(|l| l.contains("kill -CONT")));

        // Entrada que ficou: a linha da spec 002 6.7 e código 1.
        let (_d, store) = store_with(Some(journal(vec![entry("god")], false)));
        let s = summarize(&store, &[], &[], false, true);
        assert_eq!(s.code, 1);
        assert!(
            s.lines.iter().any(|l| l
                == "não foi possível restaurar god agora; a próxima execução do gt-tui (ou gt-patch restore) vai restaurar"),
            "{:?}",
            s.lines
        );

        // Marcador "pausado por nós": kill -CONT com o pid do diário relido.
        let (_d, store) = store_with(Some(journal(vec![], true)));
        let s = summarize(&store, &[], &[], false, true);
        assert_eq!(s.code, 1);
        assert!(
            s.lines
                .iter()
                .any(|l| l == "o jogo pode ter ficado parado: kill -CONT 4242"),
            "{:?}",
            s.lines
        );

        // Pânico: 101, mesmo com o diário limpo, e o resumo diz isso.
        let (_d, store) = store_with(None);
        let s = summarize(&store, &[], &[], true, true);
        assert_eq!(s.code, 101);
        assert!(
            s.lines.iter().any(|l| l.contains("pânico")),
            "{:?}",
            s.lines
        );

        // Pânico com restos: continua 101, e os restos também aparecem.
        let (_d, store) = store_with(Some(journal(vec![entry("god")], true)));
        let s = summarize(&store, &[], &[], true, true);
        assert_eq!(s.code, 101);
        assert!(s.lines.iter().any(|l| l.contains("restaurar god")));
        assert!(s.lines.iter().any(|l| l.contains("kill -CONT 4242")));
    }

    #[test]
    fn panic_hint_depends_on_log() {
        let (_d, store) = store_with(None);
        let with_log = summarize(&store, &[], &[], true, true);
        assert!(
            with_log
                .lines
                .iter()
                .any(|l| l.contains("veja o log em arquivo")),
            "{:?}",
            with_log.lines
        );
        let without = summarize(&store, &[], &[], true, false);
        assert!(
            without
                .lines
                .iter()
                .any(|l| l.contains("veja a mensagem acima")),
            "{:?}",
            without.lines
        );
        assert!(!without.lines.iter().any(|l| l.contains("log em arquivo")));
    }

    #[test]
    fn foreign_entries_are_listed_without_changing_code() {
        let (_d, store) = store_with(None);
        let s = summarize(&store, &[], &["x".to_string()], false, true);
        assert_eq!(s.code, 0);
        assert!(
            s.lines
                .iter()
                .any(|l| l == "não restaurado: x (bytes alterados por outro)"),
            "{:?}",
            s.lines
        );
    }

    #[test]
    fn unreadable_journal_gives_code_1() {
        let dir = tempfile::tempdir().unwrap();
        let store = JournalStore::new(dir.path().to_path_buf());
        std::fs::write(dir.path().join("journal.json"), b"{ isto nao e json").unwrap();
        let s = summarize(&store, &[], &[], false, true);
        assert_eq!(s.code, 1);
        assert!(
            s.lines.iter().any(|l| l.contains("diário")),
            "{:?}",
            s.lines
        );
        // `peek` não move o arquivo.
        assert!(dir.path().join("journal.json").exists());
    }
}
