//! Mapa de teclas (spec 003, 6.6).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// Ação do jogador, já independente da tecla usada.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Mover a seleção para cima.
    Up,
    /// Mover a seleção para baixo.
    Down,
    /// Ligar/desligar o cheat selecionado.
    Toggle,
    /// Diminuir o valor em um `step`.
    Dec,
    /// Aumentar o valor em um `step`.
    Inc,
    /// Tentar de novo (`r`).
    Retry,
    /// Sair.
    Quit,
}

/// Traduz uma tecla em ação. Só `Press` conta; Ctrl+Z e as demais teclas dão `None`.
pub fn action(ev: &KeyEvent) -> Option<Action> {
    if ev.kind != KeyEventKind::Press {
        return None;
    }
    let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl {
        return match ev.code {
            KeyCode::Char('c') => Some(Action::Quit),
            _ => None,
        };
    }
    if ev.modifiers.contains(KeyModifiers::ALT) {
        return None;
    }
    match ev.code {
        KeyCode::Up | KeyCode::Char('k') => Some(Action::Up),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::Down),
        KeyCode::Char(' ') | KeyCode::Enter => Some(Action::Toggle),
        KeyCode::Left | KeyCode::Char('h') => Some(Action::Dec),
        KeyCode::Right | KeyCode::Char('l') => Some(Action::Inc),
        KeyCode::Char('r') => Some(Action::Retry),
        KeyCode::Char('q') | KeyCode::Esc => Some(Action::Quit),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn plain(code: KeyCode) -> Option<Action> {
        action(&press(code, KeyModifiers::NONE))
    }

    #[test]
    fn keys_map() {
        assert_eq!(plain(KeyCode::Up), Some(Action::Up));
        assert_eq!(plain(KeyCode::Char('k')), Some(Action::Up));
        assert_eq!(plain(KeyCode::Down), Some(Action::Down));
        assert_eq!(plain(KeyCode::Char('j')), Some(Action::Down));
        assert_eq!(plain(KeyCode::Char(' ')), Some(Action::Toggle));
        assert_eq!(plain(KeyCode::Enter), Some(Action::Toggle));
        assert_eq!(plain(KeyCode::Left), Some(Action::Dec));
        assert_eq!(plain(KeyCode::Char('h')), Some(Action::Dec));
        assert_eq!(plain(KeyCode::Right), Some(Action::Inc));
        assert_eq!(plain(KeyCode::Char('l')), Some(Action::Inc));
        assert_eq!(plain(KeyCode::Char('r')), Some(Action::Retry));
        assert_eq!(plain(KeyCode::Char('q')), Some(Action::Quit));
        assert_eq!(plain(KeyCode::Esc), Some(Action::Quit));
        assert_eq!(
            action(&press(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Action::Quit)
        );
        // Ctrl+Z e o resto são ignorados.
        assert_eq!(
            action(&press(KeyCode::Char('z'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(plain(KeyCode::Char('x')), None);
        assert_eq!(plain(KeyCode::Tab), None);
        assert_eq!(
            action(&press(KeyCode::Char('q'), KeyModifiers::CONTROL)),
            None
        );
    }

    #[test]
    fn only_press_counts() {
        for kind in [KeyEventKind::Release, KeyEventKind::Repeat] {
            let ev = KeyEvent {
                code: KeyCode::Char('q'),
                modifiers: KeyModifiers::NONE,
                kind,
                state: KeyEventState::NONE,
            };
            assert_eq!(action(&ev), None);
        }
    }
}
