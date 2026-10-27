//! Tela do `gt-tui` (spec 003, 6.4): desenha o [`Model`] com `ratatui`.
//!
//! Só desenho: nada aqui altera o modelo nem toca o terminal. As cores e os estilos são
//! enfeite; o que importa (e o que os testes conferem) são os textos.

use gt_core::value::ValueGrid;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph};

use crate::model::{Avail, CHECKING, CheatRow, CheatStatus, Conn, Model};
use gt_grounded2::fields::Note;

/// Largura mínima do terminal.
pub const MIN_WIDTH: u16 = 60;
/// Altura mínima do terminal.
pub const MIN_HEIGHT: u16 = 15;

/// Menor número de linhas do registro de eventos.
const MIN_EVENT_LINES: usize = 3;
/// Largura do marcador `> ` das linhas de cheat.
const MARKER_WIDTH: usize = 2;
/// Espaço entre o nome e o estado.
const STATE_GAP: usize = 2;
/// Teclas do rodapé.
const FOOTER: &str = "↑↓ mover  espaço liga/desliga  ◀▶ valor  r rebuscar  q sair";
/// Mensagem da lista vazia (spec 6.5).
const EMPTY_LIST: &str =
    "nenhum cheat disponível nesta versão do trainer (use --diagnostico para o patch de teste)";
/// Mensagem quando o terminal é menor que o mínimo.
const TOO_SMALL: &str = "aumente o terminal";

/// `true` quando o terminal é menor que o mínimo (60×15).
pub fn too_small(width: u16, height: u16) -> bool {
    width < MIN_WIDTH || height < MIN_HEIGHT
}

/// Deslocamento da janela de `height` linhas sobre a lista, sem guardar estado.
///
/// `is_header[i]` é `true` quando a linha `i` é um cabeçalho de categoria. Devolve o menor
/// deslocamento que deixa `selected_line` visível; se o cabeçalho mais próximo acima dela
/// couber junto na janela, ele também fica visível (o menor deslocamento já o inclui).
pub fn scroll_offset(_is_header: &[bool], selected_line: usize, height: usize) -> usize {
    if height == 0 {
        return 0;
    }
    // O cabeçalho mais próximo acima da seleção fica na janela sempre que cabe junto com ela:
    // a janela que termina na seleção começa em `selected_line + 1 - height`, que não passa
    // da linha do cabeçalho quando `selected_line - cabeçalho < height`.
    (selected_line + 1).saturating_sub(height)
}

/// Desenha a tela inteira.
pub fn render(model: &Model, frame: &mut Frame) {
    let area = frame.area();
    if too_small(area.width, area.height) {
        frame.render_widget(Paragraph::new(TOO_SMALL), area);
        return;
    }
    // A última linha é o rodapé; o resto é a caixa.
    let boxed = Rect::new(area.x, area.y, area.width, area.height - 1);
    let footer = Rect::new(area.x, area.y + area.height - 1, area.width, 1);
    frame.render_widget(
        Paragraph::new(FOOTER).style(Style::new().add_modifier(Modifier::DIM)),
        footer,
    );

    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title(title(&model.conn));
    let inner = block.inner(boxed);
    frame.render_widget(block, boxed);

    let width = usize::from(inner.width);
    let height = usize::from(inner.height);
    // Linhas fixas: dois separadores, no mínimo 3 de eventos e 1 de lista.
    let budget = height.saturating_sub(2 + MIN_EVENT_LINES + 1);
    let mut banner_lines = wrap_all(&model.banners.lines(), width);
    banner_lines.truncate(budget.saturating_sub(1));
    let mut status_lines = status_lines(model, width);
    status_lines.truncate(budget - banner_lines.len());

    let list = list_lines(model, width);
    let free = height.saturating_sub(status_lines.len() + banner_lines.len() + 2);
    // A lista leva o que precisa; o que sobra vai para o registro de eventos.
    let list_height = list
        .lines
        .len()
        .min(free.saturating_sub(MIN_EVENT_LINES))
        .max(1);
    let event_height = free.saturating_sub(list_height);

    let mut y = inner.y;
    let mut place = |lines: usize| {
        let rect = Rect::new(inner.x, y, inner.width, lines as u16);
        y += lines as u16;
        rect
    };
    let status_rect = place(status_lines.len());
    let banner_rect = place(banner_lines.len());
    let sep_top = place(1);
    let list_rect = place(list_height);
    let sep_bottom = place(1);
    let events_rect = place(event_height);

    frame.render_widget(Paragraph::new(status_lines), status_rect);
    let banner_style = Style::new()
        .fg(Color::Black)
        .bg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    frame.render_widget(
        Paragraph::new(banner_lines.into_iter().map(Line::from).collect::<Vec<_>>())
            .style(banner_style),
        banner_rect,
    );
    for sep in [sep_top, sep_bottom] {
        separator(frame, boxed, sep.y);
    }

    let offset = scroll_offset(&list.is_header, list.selected_line, list_height);
    let visible: Vec<Line> = list
        .lines
        .into_iter()
        .skip(offset)
        .take(list_height)
        .collect();
    frame.render_widget(Paragraph::new(visible), list_rect);

    let skip = model.events.len().saturating_sub(event_height);
    let events: Vec<Line> = model
        .events
        .iter()
        .skip(skip)
        .map(|e| Line::from(e.as_str()))
        .collect();
    frame.render_widget(Paragraph::new(events), events_rect);
}

/// Título da caixa: `grounded-trainer ─ <estado>` e, conectado, `(pid N)`.
fn title(conn: &Conn) -> String {
    match conn {
        Conn::Connected { pid, .. } => format!(" grounded-trainer ─ {} (pid {pid}) ", conn.label()),
        _ => format!(" grounded-trainer ─ {} ", conn.label()),
    }
}

/// Linha horizontal que cruza a caixa de borda a borda, com as junções `├` e `┤`.
fn separator(frame: &mut Frame, boxed: Rect, y: u16) {
    let right = boxed.x + boxed.width - 1;
    let buf = frame.buffer_mut();
    for x in boxed.x..=right {
        let symbol = if x == boxed.x {
            "├"
        } else if x == right {
            "┤"
        } else {
            "─"
        };
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_symbol(symbol);
        }
    }
}

/// Texto do topo: a versão (conectado) ou o que a TUI está fazendo/esperando.
fn status_lines(model: &Model, width: usize) -> Vec<Line<'static>> {
    let busy = model
        .busy
        .as_ref()
        .map(|b| Line::styled(b.clone(), Style::new().fg(Color::Cyan)));
    let detail: Vec<String> = match &model.conn {
        Conn::Connected {
            version_key,
            has_version_file,
            disk,
            ..
        } => {
            if let Some(busy) = busy {
                return vec![busy];
            }
            let signatures = match disk {
                Err(cause) => cause.as_str(),
                Ok(()) if *has_version_file => "com assinaturas",
                Ok(()) => "sem assinaturas",
            };
            let (ok, total) = model.counts();
            let mut lines = vec![Line::from(format!(
                "versão {version_key} ({signatures})  {ok}/{total}"
            ))];
            // Com cheats de valor na lista, a linha do personagem (spec 005, 6.7).
            if model.cheats.iter().any(|r| r.field) {
                lines.push(Line::from(model.player.label()));
            }
            return lines;
        }
        Conn::Waiting => wrap("procurando o processo do jogo…", width),
        Conn::NoAccess(reason) => wrap(reason, width),
        Conn::Connecting if busy.is_none() => wrap("abrindo o alvo…", width),
        Conn::Connecting => Vec::new(),
        Conn::Many(pids) => {
            let list = pids
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            wrap(
                &format!("mais de um processo do jogo (pid {list}); use --pid <pid>"),
                width,
            )
        }
        Conn::Error(reason) => wrap(reason, width),
        Conn::PidGone => wrap("reinicie o gt-tui sem --pid, ou com o PID novo", width),
    };
    busy.into_iter()
        .chain(detail.into_iter().map(Line::from))
        .collect()
}

/// Linhas da lista, com a posição da seleção e quais são cabeçalhos.
struct ListLines {
    lines: Vec<Line<'static>>,
    is_header: Vec<bool>,
    selected_line: usize,
}

fn list_lines(model: &Model, width: usize) -> ListLines {
    let rows = model.visible();
    if rows.is_empty() {
        let lines = if matches!(model.conn, Conn::Connected { .. }) {
            wrap(EMPTY_LIST, width)
                .into_iter()
                .map(Line::from)
                .collect()
        } else {
            Vec::new()
        };
        let is_header = vec![false; lines.len()];
        return ListLines {
            lines,
            is_header,
            selected_line: 0,
        };
    }
    let name_width = rows
        .iter()
        .map(|r| r.name.chars().count())
        .max()
        .unwrap_or(0);
    let mut out = ListLines {
        lines: Vec::new(),
        is_header: Vec::new(),
        selected_line: 0,
    };
    let mut current = None;
    for (index, row) in rows.iter().enumerate() {
        if current != Some(row.category) {
            current = Some(row.category);
            out.lines.push(Line::styled(
                row.category.label(),
                Style::new().add_modifier(Modifier::BOLD).fg(Color::Cyan),
            ));
            out.is_header.push(true);
        }
        if index == model.selected {
            out.selected_line = out.lines.len();
        }
        out.lines.push(cheat_line(
            row,
            &model.conn,
            index == model.selected,
            name_width,
            width,
        ));
        out.is_header.push(false);
    }
    out
}

/// `> nome  estado  [valor ◀ ▶]`.
fn cheat_line(
    row: &CheatRow,
    conn: &Conn,
    selected: bool,
    name_width: usize,
    width: usize,
) -> Line<'static> {
    let status = row.status(conn);
    let marker = if selected { "> " } else { "  " };
    let unverified = matches!(row.avail, Avail::Available { verified: false });
    let again = row.desired && status != CheatStatus::On;
    // Duas casas fixas para `?` e duas para `↻`, para o valor alinhar entre as linhas.
    let unverified_mark = if unverified { " ?" } else { "  " };
    let again_mark = if again { " ↻" } else { "  " };

    let note = match row.note {
        Some(Note::Waiting) => " aguardando o personagem",
        Some(Note::WaitingPet) => " aguardando o pet",
        Some(Note::WaitingWorld) => " aguardando o mundo",
        Some(Note::InvalidTarget) => " alvo inválido",
        Some(Note::InvalidGameValue) => " valor do jogo inválido",
        Some(Note::GameRefuses) => " o jogo não deixa",
        None => "",
    };
    let (state, style, has_value) = match &status {
        CheatStatus::On if !note.is_empty() => {
            (format!("[ON ]{note}"), Style::new().fg(Color::Yellow), true)
        }
        CheatStatus::On => (
            format!("[ON ]{unverified_mark}{again_mark}"),
            Style::new().fg(Color::Green),
            true,
        ),
        CheatStatus::Unavailable(reason) if reason == CHECKING => {
            (CHECKING.to_string(), Style::new().fg(Color::DarkGray), true)
        }
        // Uma ação não liga nem desliga: pede uma execução (spec 006, 6.6).
        CheatStatus::Off if row.action => (
            format!("[ação]{unverified_mark}"),
            Style::new().fg(Color::Cyan),
            true,
        ),
        CheatStatus::Off => (
            format!("[OFF]{unverified_mark}{again_mark}"),
            Style::new(),
            true,
        ),
        CheatStatus::Unavailable(reason) => {
            let again = again_mark.trim_end();
            // O valor vale quando o cheat voltar a ligar (e é o que o religar usa): sempre
            // aparece. Quando falta espaço, o motivo é que perde caracteres.
            let reason = match &row.value {
                Some(value) => {
                    let fixed = MARKER_WIDTH
                        + name_width
                        + STATE_GAP
                        + "indisponível ()".chars().count()
                        + again.chars().count()
                        + 1
                        + usize::from(row.action)
                        + value_text(value, row.k).chars().count();
                    truncate(reason, width.saturating_sub(fixed))
                }
                None => reason.clone(),
            };
            (
                format!("indisponível ({reason}){again}"),
                Style::new().fg(Color::DarkGray),
                true,
            )
        }
        CheatStatus::Residue => (
            format!("erro {unverified_mark}{again_mark}"),
            Style::new().fg(Color::Red),
            true,
        ),
    };
    let name_style = if selected {
        Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED)
    } else {
        Style::new()
    };
    let mut spans = vec![
        Span::raw(marker),
        Span::styled(format!("{:<name_width$}", row.name), name_style),
        Span::raw(" ".repeat(STATE_GAP)),
        Span::styled(state, style),
    ];
    if has_value && let Some(value) = &row.value {
        let text = if row.action {
            format!(" +{}", value_text(value, row.k))
        } else {
            format!(" {}", value_text(value, row.k))
        };
        spans.push(Span::raw(text));
    }
    Line::from(spans)
}

/// Corta `text` em `max` colunas, com `…` no fim quando perde caracteres.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = text.chars().take(keep).collect();
    if max > 0 {
        out.push('…');
    }
    out
}

/// `1.5x ◀ ▶`.
fn value_text(spec: &ValueGrid, k: u64) -> String {
    format!("{} ◀ ▶", spec.format(k))
}

/// Quebra `text` em linhas de até `width` colunas, nas palavras.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut used = 0;
    for word in text.split_whitespace() {
        let len = word.chars().count();
        if len > width {
            // Palavra maior que a linha (ex.: um caminho): divide em pedaços, sem perder o fim.
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            let chars: Vec<char> = word.chars().collect();
            let mut chunks = chars.chunks(width);
            let last = chunks.next_back();
            lines.extend(chunks.map(|c| c.iter().collect::<String>()));
            current = last.map(|c| c.iter().collect()).unwrap_or_default();
            used = current.chars().count();
            continue;
        }
        if used > 0 && used + 1 + len > width {
            lines.push(std::mem::take(&mut current));
            used = 0;
        }
        if used > 0 {
            current.push(' ');
            used += 1;
        }
        current.push_str(word);
        used += len;
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// [`wrap`] para vários textos, um após o outro.
fn wrap_all(texts: &[&str], width: usize) -> Vec<String> {
    texts.iter().flat_map(|t| wrap(t, width)).collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use gt_core::value::ValueType;
    use gt_grounded2::signatures::Category;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn connected() -> Conn {
        Conn::Connected {
            pid: 4123,
            version_key: "c935e799-0b27c000".into(),
            has_version_file: true,
            disk: Ok(()),
            engine: true,
        }
    }

    fn row(name: &str, category: Category, avail: Avail) -> CheatRow {
        CheatRow {
            id: name.to_lowercase().replace(' ', "-"),
            name: name.to_string(),
            category,
            value: None,
            k: 0,
            applied_k: None,
            avail,
            on: false,
            residue: false,
            desired: false,
            field: false,
            action: false,
            note: None,
        }
    }

    fn ok() -> Avail {
        Avail::Available { verified: true }
    }

    /// Desenha o modelo num `TestBackend` e devolve as linhas do buffer.
    fn draw(model: &Model, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render(model, f)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer.cell((x, y)).map_or(" ", |c| c.symbol()))
                    .collect::<String>()
            })
            .collect()
    }

    fn screen(model: &Model) -> Vec<String> {
        draw(model, 70, 20)
    }

    /// Texto da tela com as bordas e os espaços repetidos tirados: acha trechos que a quebra
    /// de linha dividiu.
    fn flat(lines: &[String]) -> String {
        lines
            .join(" ")
            .replace(['│', '├', '┤', '─', '┌', '┐', '└', '┘'], " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn line_with<'a>(lines: &'a [String], needle: &str) -> &'a str {
        lines
            .iter()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("sem {needle:?} em:\n{}", lines.join("\n")))
    }

    fn speed() -> ValueGrid {
        ValueGrid {
            ty: ValueType::F32,
            min: 0.5,
            max: 3.0,
            step: 0.5,
            default_k: 2,
            unit: Some("x".into()),
            decimals: 1,
        }
    }

    #[test]
    fn renders_waiting() {
        let lines = screen(&Model::new(false));
        assert!(line_with(&lines, "grounded-trainer").contains("aguardando o jogo"));
        assert!(flat(&lines).contains("procurando o processo do jogo"));
        assert!(flat(&lines).contains("↑↓ mover espaço liga/desliga ◀▶ valor r rebuscar q sair"));
    }

    #[test]
    fn renders_no_access_hint() {
        let mut m = Model::new(false);
        m.conn = Conn::NoAccess(
            "kernel.yama.ptrace_scope está em 1, o que restringe a leitura da memória".into(),
        );
        let lines = screen(&m);
        assert!(line_with(&lines, "grounded-trainer").contains("sem acesso"));
        assert!(flat(&lines).contains("kernel.yama.ptrace_scope está em 1"));
        assert!(flat(&lines).contains("restringe a leitura da memória"));
    }

    #[test]
    fn renders_many_processes() {
        let mut m = Model::new(false);
        m.conn = Conn::Many(vec![11, 22]);
        let lines = screen(&m);
        assert!(line_with(&lines, "grounded-trainer").contains("vários processos"));
        assert!(flat(&lines).contains("11, 22"));
        assert!(flat(&lines).contains("--pid"));
    }

    #[test]
    fn renders_pid_gone() {
        let mut m = Model::new(false);
        m.conn = Conn::PidGone;
        let lines = screen(&m);
        assert!(line_with(&lines, "grounded-trainer").contains("o processo terminou"));
        assert!(flat(&lines).contains("reinicie o gt-tui sem --pid, ou com o PID novo"));
    }

    #[test]
    fn renders_connected_rows() {
        let mut m = Model::new(false);
        m.conn = connected();
        let mut on = row("Cargas infinitas", Category::Itens, ok());
        on.on = true;
        let off = row("Duplicar ao arrastar", Category::Itens, ok());
        let unverified = row(
            "Sem conferir",
            Category::Itens,
            Avail::Available { verified: false },
        );
        let missing = row(
            "God mode",
            Category::Sobrevivencia,
            Avail::Unavailable("não encontrado".into()),
        );
        let mut residue = row("Quebrado", Category::Progressao, ok());
        residue.residue = true;
        let mut again = row("Religar", Category::Progressao, ok());
        again.desired = true;
        let mut speed_row = row("Velocidade", Category::Movimento, ok());
        speed_row.value = Some(speed());
        speed_row.k = 2;
        m.cheats = vec![on, off, unverified, missing, residue, again, speed_row];
        // Ordem visível: God mode, Cargas infinitas, Duplicar ao arrastar, ...
        m.selected = 2;
        let lines = screen(&m);

        let top = line_with(&lines, "grounded-trainer");
        assert!(top.contains("conectado (pid 4123)"), "{top}");
        let version = line_with(&lines, "versão");
        assert!(
            version.contains("c935e799-0b27c000 (com assinaturas)"),
            "{version}"
        );
        // 6 disponíveis ou ligados, de 7 (só o God mode não conta).
        assert!(version.contains("6/7"), "{version}");

        for header in ["SOBREVIVÊNCIA", "ITENS", "PROGRESSÃO", "MOVIMENTO"] {
            line_with(&lines, header);
        }
        assert!(line_with(&lines, "Cargas infinitas").contains("[ON ]"));
        let selected = line_with(&lines, "Duplicar ao arrastar");
        assert!(selected.contains("> Duplicar ao arrastar"), "{selected}");
        assert!(selected.contains("[OFF]"));
        assert!(!selected.contains('?'));
        assert!(line_with(&lines, "Sem conferir").contains("[OFF] ?"));
        assert!(line_with(&lines, "God mode").contains("indisponível (não encontrado)"));
        assert!(line_with(&lines, "Quebrado").contains("erro"));
        let again = line_with(&lines, "Religar");
        assert!(again.contains("[OFF]") && again.contains('↻'), "{again}");
        let value = line_with(&lines, "Velocidade");
        assert!(
            value.contains("[OFF]") && value.contains("1.5x ◀ ▶"),
            "{value}"
        );
    }

    #[test]
    fn renders_disk_cause_and_no_signatures() {
        let mut m = Model::new(false);
        m.conn = Conn::Connected {
            pid: 1,
            version_key: "v1".into(),
            has_version_file: false,
            disk: Ok(()),
            engine: true,
        };
        assert!(line_with(&screen(&m), "versão").contains("(sem assinaturas)"));
        m.conn = Conn::Connected {
            pid: 1,
            version_key: "v1".into(),
            has_version_file: true,
            disk: Err("disco ilegível".into()),
            engine: false,
        };
        assert!(line_with(&screen(&m), "versão").contains("(disco ilegível)"));
    }

    #[test]
    fn renders_busy_message() {
        let mut m = Model::new(false);
        m.conn = connected();
        m.busy = Some("ligando God mode…".into());
        let lines = screen(&m);
        line_with(&lines, "ligando God mode…");
        assert!(!lines.iter().any(|l| l.contains("versão c935")));
    }

    #[test]
    fn renders_empty_list_message() {
        let mut m = Model::new(false);
        m.conn = connected();
        assert!(flat(&screen(&m)).contains(
            "nenhum cheat disponível nesta versão do trainer (use --diagnostico para o patch de teste)"
        ));
        // Sem o jogo conectado a lista fica em branco, sem a mensagem.
        let waiting = Model::new(false);
        assert!(!flat(&screen(&waiting)).contains("nenhum cheat"));
    }

    #[test]
    fn renders_banner() {
        let mut m = Model::new(false);
        m.conn = connected();
        m.banners.corrupt = Some("diário corrompido movido para /x/journal.bad".into());
        m.banners.stuck = Some("o jogo pode ter ficado parado: kill -CONT 4123".into());
        let lines = screen(&m);
        line_with(&lines, "diário corrompido movido para /x/journal.bad");
        line_with(&lines, "kill -CONT 4123");
    }

    #[test]
    fn renders_banner_long_path() {
        let path = "/home/caarlosandree/.local/state/grounded-trainer/journal.json.bad";
        let mut m = Model::new(false);
        m.conn = connected();
        m.banners.corrupt = Some(format!(
            "diário corrompido movido para {path}; o jogo pode ter patches sem registro, confira com gt diff"
        ));
        let lines = draw(&m, 60, 20);
        // O caminho pode quebrar em várias linhas, mas aparece inteiro.
        assert!(
            flat(&lines).replace(' ', "").contains(path),
            "{}",
            lines.join("\n")
        );
        line_with(&lines, "confira com gt diff");
    }

    #[test]
    fn wrap_splits_words_longer_than_the_line() {
        assert_eq!(wrap("ab cdefghij k", 4), ["ab", "cdef", "ghij", "k"]);
        assert_eq!(wrap("abcdefgh", 4), ["abcd", "efgh"]);
        assert_eq!(wrap("a bb ccc", 5), ["a bb", "ccc"]);
        assert!(wrap("", 4).is_empty());
    }

    #[test]
    fn renders_value_on_unavailable_rows() {
        let mut m = Model::new(false);
        m.conn = connected();
        let mut missing = row(
            "Velocidade",
            Category::Movimento,
            Avail::Unavailable("não encontrado".into()),
        );
        missing.value = Some(speed());
        missing.k = 2;
        m.cheats = vec![missing];
        let lines = screen(&m);
        let line = line_with(&lines, "Velocidade");
        assert!(line.contains("indisponível (não encontrado)"), "{line}");
        assert!(line.contains("1.5x ◀ ▶"), "{line}");

        // Desejado e sem o jogo: valor e `↻`.
        let mut m = Model::new(false);
        let mut pending = row("Velocidade", Category::Movimento, ok());
        pending.value = Some(speed());
        pending.k = 2;
        pending.desired = true;
        m.cheats = vec![pending];
        let lines = screen(&m);
        let line = line_with(&lines, "Velocidade");
        assert!(line.contains("jogo não conectado"), "{line}");
        assert!(line.contains('↻') && line.contains("1.5x ◀ ▶"), "{line}");
    }

    #[test]
    fn narrow_terminal_truncates_reason_not_value() {
        let mut m = Model::new(false);
        m.conn = connected();
        let mut long = row(
            "Velocidade do personagem",
            Category::Movimento,
            Avail::Unavailable("bytes diferentes do esperado no endereço".into()),
        );
        long.value = Some(speed());
        long.k = 2;
        long.desired = true;
        m.cheats = vec![long];
        let lines = draw(&m, MIN_WIDTH, MIN_HEIGHT);
        let line = line_with(&lines, "Velocidade do personagem");
        assert!(line.contains("1.5x ◀ ▶"), "{line}");
        assert!(line.contains('↻'), "{line}");
        assert!(line.contains('…'), "{line}");
    }

    #[test]
    fn renders_events_last_lines() {
        let mut m = Model::new(false);
        m.conn = connected();
        for i in 0..50 {
            m.push_event("12:04", format!("evento {i}"));
        }
        let lines = screen(&m);
        line_with(&lines, "12:04 evento 49");
        line_with(&lines, "12:04 evento 48");
        line_with(&lines, "12:04 evento 47");
        assert!(!lines.iter().any(|l| l.contains("evento 10")));
    }

    #[test]
    fn renders_small_terminal() {
        let mut m = Model::new(false);
        m.conn = connected();
        m.cheats = vec![row("God mode", Category::Sobrevivencia, ok())];
        let lines = draw(&m, 50, 10);
        assert!(lines[0].starts_with("aumente o terminal"));
        assert_eq!(flat(&lines), "aumente o terminal");
        // Um lado só pequeno também conta.
        assert_eq!(flat(&draw(&m, 59, 20)), "aumente o terminal");
        assert_eq!(flat(&draw(&m, 70, 14)), "aumente o terminal");
        assert!(too_small(59, 15) && too_small(60, 14) && !too_small(60, 15));
    }

    #[test]
    fn renders_at_minimum_size() {
        let mut m = Model::new(false);
        m.conn = connected();
        m.cheats = vec![row("God mode", Category::Sobrevivencia, ok())];
        m.push_event("12:04", "ligado".into());
        let lines = draw(&m, MIN_WIDTH, MIN_HEIGHT);
        line_with(&lines, "God mode");
        line_with(&lines, "12:04 ligado");
        line_with(&lines, "q sair");
    }

    fn many_cheats() -> Model {
        let mut m = Model::new(false);
        m.conn = connected();
        m.cheats = (0..20)
            .map(|i| {
                let category = match i {
                    0..8 => Category::Itens,
                    8..16 => Category::Progressao,
                    _ => Category::Movimento,
                };
                row(&format!("Cheat numero {i:02}"), category, ok())
            })
            .collect();
        m
    }

    #[test]
    fn renders_scrolled_selection() {
        let mut m = many_cheats();
        m.selected = 19;
        let lines = screen(&m);
        assert!(line_with(&lines, "Cheat numero 19").contains("> Cheat numero 19"));
        line_with(&lines, "MOVIMENTO");
        assert!(!lines.iter().any(|l| l.contains("Cheat numero 00")));
        // Sem rolagem a primeira linha aparece.
        m.selected = 0;
        let lines = screen(&m);
        line_with(&lines, "Cheat numero 00");
        line_with(&lines, "ITENS");
    }

    #[test]
    fn scroll_keeps_selection_and_header_visible() {
        let mut is_header = Vec::new();
        for size in [8, 8, 4] {
            is_header.push(true);
            is_header.extend(std::iter::repeat_n(false, size));
        }
        assert_eq!(is_header.len(), 23);
        let headers: Vec<usize> = (0..is_header.len()).filter(|&i| is_header[i]).collect();
        assert_eq!(headers, [0, 9, 18]);
        let height = 6;
        let selected = is_header.len() - 1;
        let offset = scroll_offset(&is_header, selected, height);
        let window = offset..offset + height;
        assert!(window.contains(&selected), "seleção fora de {window:?}");
        assert!(window.contains(&18), "cabeçalho fora de {window:?}");
        // Para toda seleção possível, ela fica visível e, se couber, o cabeçalho também.
        for sel in (0..is_header.len()).filter(|&i| !is_header[i]) {
            let offset = scroll_offset(&is_header, sel, height);
            let window = offset..offset + height;
            assert!(window.contains(&sel), "sel {sel} fora de {window:?}");
            let header = headers.iter().rev().find(|&&h| h <= sel).copied().unwrap();
            if sel - header < height {
                assert!(
                    window.contains(&header),
                    "cabeçalho de {sel} fora de {window:?}"
                );
            }
            assert!(offset + height <= is_header.len().max(height));
        }
        // Sem necessidade, não rola.
        assert_eq!(scroll_offset(&is_header, 3, height), 0);
        assert_eq!(scroll_offset(&is_header, 3, 0), 0);
    }

    #[test]
    fn on_with_waiting_note() {
        let mut m = Model::new(false);
        m.conn = connected();
        let mut r = row("Vida infinita", Category::Sobrevivencia, ok());
        r.field = true;
        r.on = true;
        r.note = Some(Note::Waiting);
        m.cheats = vec![r];
        let lines = screen(&m);
        assert!(line_with(&lines, "Vida infinita").contains("[ON ] aguardando o personagem"));
    }

    #[test]
    fn on_with_invalid_target_note() {
        let mut m = Model::new(false);
        m.conn = connected();
        let mut r = row("Stamina infinita", Category::Sobrevivencia, ok());
        r.field = true;
        r.on = true;
        r.note = Some(Note::InvalidTarget);
        m.cheats = vec![r];
        let lines = screen(&m);
        assert!(line_with(&lines, "Stamina infinita").contains("[ON ] alvo inválido"));
    }

    #[test]
    fn checking_row_shows_only_verificando() {
        let mut m = Model::new(false);
        m.conn = connected();
        let mut r = row("Comida infinita", Category::Sobrevivencia, Avail::Checking);
        r.field = true;
        m.cheats = vec![r];
        let lines = screen(&m);
        let line = line_with(&lines, "Comida infinita");
        assert!(line.contains("verificando…"), "{line}");
        assert!(!line.contains("indisponível"), "{line}");
    }

    #[test]
    fn render_includes_player_line() {
        let mut m = Model::new(false);
        m.conn = connected();
        m.cheats = vec![row("Patch", Category::Itens, ok())];
        // Sem cheats de valor, sem a linha.
        assert!(!flat(&screen(&m)).contains("personagem:"));
        let mut f = row("Vida infinita", Category::Sobrevivencia, ok());
        f.field = true;
        m.cheats.push(f);
        assert!(flat(&screen(&m)).contains("personagem: —"));
        m.player = crate::model::PlayerLine::Found;
        assert!(flat(&screen(&m)).contains("personagem: encontrado"));
        m.player = crate::model::PlayerLine::NotFound;
        assert!(flat(&screen(&m)).contains("personagem: não encontrado"));
    }

    fn science() -> ValueGrid {
        ValueGrid {
            ty: ValueType::I32,
            min: 100.0,
            max: 5000.0,
            step: 100.0,
            default_k: 9,
            unit: None,
            decimals: 0,
        }
    }

    #[test]
    fn action_row_shows_acao_and_amount() {
        let mut m = Model::new(false);
        m.conn = connected();
        let mut r = row("Somar ciência", Category::Progressao, ok());
        r.field = true;
        r.action = true;
        r.value = Some(science());
        r.k = 9;
        m.cheats = vec![r];
        let lines = screen(&m);
        let line = line_with(&lines, "Somar ciência");
        assert!(line.contains("[ação]"), "{line}");
        assert!(line.contains("+1000"), "{line}");
        assert!(!line.contains("[ON ]") && !line.contains("[OFF]"), "{line}");
    }

    #[test]
    fn unavailable_action_keeps_plus_inside_width() {
        let mut m = Model::new(false);
        m.conn = connected();
        let mut r = row(
            "Somar ciência",
            Category::Progressao,
            Avail::Unavailable("bytes diferentes do esperado no endereço".into()),
        );
        r.field = true;
        r.action = true;
        r.value = Some(science());
        r.k = 9;
        m.cheats = vec![r];
        let lines = draw(&m, MIN_WIDTH, MIN_HEIGHT);
        let line = line_with(&lines, "Somar ciência");
        assert!(line.contains("+1000 ◀ ▶"), "{line}");
        assert!(line.contains('…'), "{line}");
        assert!(
            line.trim_end().chars().count() <= MIN_WIDTH as usize,
            "{line}"
        );
    }

    #[test]
    fn notes_new_texts() {
        let mut m = Model::new(false);
        m.conn = connected();
        let mut a = row("Velocidade", Category::Movimento, ok());
        a.field = true;
        a.on = true;
        a.note = Some(Note::InvalidGameValue);
        let mut b = row("Pulos", Category::Movimento, ok());
        b.field = true;
        b.on = true;
        b.note = Some(Note::GameRefuses);
        m.cheats = vec![a, b];
        let lines = screen(&m);
        assert!(line_with(&lines, "Velocidade").contains("[ON ] valor do jogo inválido"));
        assert!(line_with(&lines, "Pulos").contains("[ON ] o jogo não deixa"));
    }

    #[test]
    fn field_row_with_value_shows_value() {
        let mut m = Model::new(false);
        m.conn = connected();
        let mut r = row("Velocidade", Category::Movimento, ok());
        r.field = true;
        r.on = true;
        r.value = Some(ValueGrid {
            decimals: 2,
            ..speed()
        });
        r.k = 3;
        m.cheats = vec![r];
        let lines = screen(&m);
        let line = line_with(&lines, "Velocidade");
        assert!(line.contains("[ON ]") && line.contains("2.00x"), "{line}");
    }

    #[test]
    fn pulos_row_without_value() {
        let mut m = Model::new(false);
        m.conn = connected();
        let mut r = row("Pulos infinitos", Category::Movimento, ok());
        r.field = true;
        r.on = true;
        m.cheats = vec![r];
        let lines = screen(&m);
        let line = line_with(&lines, "Pulos infinitos");
        assert!(line.contains("[ON ]"), "{line}");
        assert!(!line.contains('◀') && !line.contains('x'), "{line}");
    }
}
