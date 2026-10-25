//! Regra de casamento do processo do jogo (spec 6.1).

use crate::GAME_EXE;

/// Escolha do processo alvo entre os candidatos.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// Exatamente um processo do jogo.
    One(i32),
    /// Nenhum processo do jogo.
    None,
    /// Mais de um processo do jogo, em ordem de varredura.
    Many(Vec<i32>),
}

/// `true` quando o primeiro argumento aponta para o executável do jogo.
///
/// O campo casa se contiver o nome do exe (sem diferenciar maiúsculas) precedido por `\`, `/` ou
/// pelo início do campo, e seguido pelo fim do campo ou por espaço.
pub fn is_game(args: &[String]) -> bool {
    let Some(first) = args.first() else {
        return false;
    };
    let field = first.to_ascii_lowercase();
    let exe = GAME_EXE.to_ascii_lowercase();
    let bytes = field.as_bytes();
    let mut from = 0;
    while let Some(pos) = field[from..].find(&exe) {
        let start = from + pos;
        let end = start + exe.len();
        let before_ok = start == 0 || matches!(bytes[start - 1], b'\\' | b'/');
        let after_ok = end == bytes.len() || bytes[end] == b' ';
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

/// `true` quando algum argumento menciona o Grounded 2 (sem diferenciar maiúsculas).
pub fn mentions_game(args: &[String]) -> bool {
    args.iter()
        .any(|a| a.to_ascii_lowercase().contains("grounded2"))
}

/// Escolhe o processo alvo.
///
/// Devolve a escolha entre os processos de [`is_game`] e os descartados: só os que
/// [`mentions_game`] mas não [`is_game`], cada um com o `args[0]` como motivo.
pub fn pick(procs: &[(i32, Vec<String>)]) -> (Pick, Vec<(i32, String)>) {
    let mut found = Vec::new();
    let mut discarded = Vec::new();
    for (pid, args) in procs {
        if is_game(args) {
            found.push(*pid);
        } else if mentions_game(args) {
            discarded.push((*pid, args.first().cloned().unwrap_or_default()));
        }
    }
    let choice = match found.len() {
        0 => Pick::None,
        1 => Pick::One(found[0]),
        _ => Pick::Many(found),
    };
    (choice, discarded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gt_core::process::parse_cmdline;

    const GAME: &[u8] = include_bytes!("../../gt-core/tests/fixtures/cmdline/game.bin");
    const LAUNCHER: &[u8] = include_bytes!("../../gt-core/tests/fixtures/cmdline/launcher.bin");
    const STEAM_EXE: &[u8] = include_bytes!("../../gt-core/tests/fixtures/cmdline/steam-exe.bin");
    const PROTON: &[u8] = include_bytes!("../../gt-core/tests/fixtures/cmdline/proton.bin");
    const REAPER: &[u8] = include_bytes!("../../gt-core/tests/fixtures/cmdline/reaper.bin");

    #[test]
    fn real_cmdlines() {
        assert!(is_game(&parse_cmdline(GAME)));
        for other in [LAUNCHER, STEAM_EXE, PROTON, REAPER] {
            let a = parse_cmdline(other);
            assert!(!is_game(&a));
            assert!(mentions_game(&a));
        }
    }

    #[test]
    fn joined_with_space_and_case() {
        assert!(is_game(&[
            r"S:\steamapps\common\Grounded2\Augusta\Binaries\Win64\grounded2steam-win64-shipping.EXE Augusta"
                .into()
        ]));
        assert!(is_game(&["Grounded2Steam-Win64-Shipping.exe".into()]));
        assert!(!is_game(&[
            r"C:\x\NotGrounded2Steam-Win64-Shipping.exe".into()
        ]));
        assert!(!is_game(&[
            r"C:\x\Grounded2Steam-Win64-Shipping.exe.bak".into()
        ]));
    }

    #[test]
    fn pick_one_none_many() {
        let g = |pid| {
            (
                pid,
                vec![r"S:\a\Grounded2Steam-Win64-Shipping.exe".to_string()],
            )
        };
        assert!(matches!(pick(&[g(10)]).0, Pick::One(10)));
        assert!(matches!(pick(&[]).0, Pick::None));
        assert!(matches!(pick(&[g(1), g(2)]).0, Pick::Many(ref v) if v == &vec![1, 2]));
    }
}
