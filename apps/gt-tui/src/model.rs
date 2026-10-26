//! Modelo da tela (spec 003, 6.4): estado puro, sem terminal e sem relógio.

use std::collections::VecDeque;

use gt_core::value::ValueGrid;
use gt_grounded2::fields::Note;
use gt_grounded2::signatures::Category;

/// Quantas linhas do registro de eventos o modelo guarda.
const MAX_EVENTS: usize = 200;

/// Estado de um cheat de valor enquanto a reflexão do jogo ainda não foi conferida (spec 005,
/// 6.7). A tela mostra só este texto, sem `indisponível (…)`.
pub const CHECKING: &str = "verificando…";

/// Estado de conexão com o jogo (spec 6.2), mostrado no topo.
#[derive(Debug, Clone, PartialEq)]
pub enum Conn {
    /// Procurando o processo do jogo.
    Waiting,
    /// O processo existe, mas a leitura da memória foi negada (motivo).
    NoAccess(String),
    /// Abrindo o alvo e resolvendo o catálogo.
    Connecting,
    /// Alvo aberto e catálogo resolvido.
    Connected {
        /// PID do processo no Linux.
        pid: i32,
        /// Chave da versão do jogo.
        version_key: String,
        /// Se há arquivo de assinaturas para a versão.
        has_version_file: bool,
        /// Resultado da leitura do cabeçalho do disco (motivo se falhou).
        disk: Result<(), String>,
        /// Se o motor de escrita abriu.
        engine: bool,
    },
    /// Mais de um processo candidato (PIDs).
    Many(Vec<i32>),
    /// Falha ao conectar (motivo).
    Error(String),
    /// O processo que estava conectado terminou.
    PidGone,
}

impl Conn {
    /// Texto curto do estado.
    pub fn label(&self) -> &'static str {
        match self {
            Conn::Waiting => "aguardando o jogo",
            Conn::NoAccess(_) => "sem acesso",
            Conn::Connecting => "conectando",
            Conn::Connected { .. } => "conectado",
            Conn::Many(_) => "vários processos",
            Conn::Error(_) => "erro",
            Conn::PidGone => "o processo terminou",
        }
    }
}

/// Disponibilidade de um cheat segundo o catálogo.
#[derive(Debug, Clone, PartialEq)]
pub enum Avail {
    /// Pode ser ligado; `verified` é falso para o "não verificado".
    Available { verified: bool },
    /// Não pode ser ligado (motivo curto).
    Unavailable(String),
    /// Cheat de valor esperando a tarefa de reflexão (`verificando…`).
    Checking,
}

/// Estado mostrado na linha de um cheat.
#[derive(Debug, Clone, PartialEq)]
pub enum CheatStatus {
    /// Ligado por esta TUI.
    On,
    /// Disponível e desligado.
    Off,
    /// Indisponível (motivo).
    Unavailable(String),
    /// Restos de uma falha no jogo; só a recuperação resolve.
    Residue,
}

/// Uma linha da lista de cheats.
#[derive(Debug, Clone, PartialEq)]
pub struct CheatRow {
    /// Identificador estável do cheat.
    pub id: String,
    /// Nome mostrado.
    pub name: String,
    /// Categoria (cabeçalho sob o qual aparece).
    pub category: Category,
    /// Definição do valor ajustável, se o cheat tem valor.
    pub value: Option<ValueGrid>,
    /// Índice na grade do valor mostrado.
    pub k: u64,
    /// Índice na grade do valor que está no jogo (com o cheat ligado).
    pub applied_k: Option<u64>,
    /// Disponibilidade segundo o catálogo.
    pub avail: Avail,
    /// Ligado por esta TUI.
    pub on: bool,
    /// Ficou com restos no jogo depois de uma falha.
    pub residue: bool,
    /// Espelho para a tela do mapa de desejados do controlador (que é a fonte da verdade).
    pub desired: bool,
    /// Cheat de valor (`[[field]]`), não patch.
    pub field: bool,
    /// Ação de uma execução (`rule = "add"`, spec 006 6.6): nunca está ligada nem desejada.
    pub action: bool,
    /// Com o cheat ligado, por que ele não escreveu neste ciclo.
    pub note: Option<Note>,
}

impl CheatRow {
    /// Estado da linha dado o estado de conexão.
    pub fn status(&self, conn: &Conn) -> CheatStatus {
        if !matches!(conn, Conn::Connected { .. }) {
            return CheatStatus::Unavailable("jogo não conectado".to_string());
        }
        if self.residue {
            return CheatStatus::Residue;
        }
        if self.on {
            return CheatStatus::On;
        }
        match &self.avail {
            Avail::Available { .. } => CheatStatus::Off,
            Avail::Unavailable(reason) => CheatStatus::Unavailable(reason.clone()),
            Avail::Checking => CheatStatus::Unavailable(CHECKING.to_string()),
        }
    }
}

/// Linha `personagem:` do topo (spec 005, 6.7).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PlayerLine {
    /// Nenhum cheat de valor ligado ou desejado: ninguém procura.
    #[default]
    Idle,
    Found,
    NotFound,
}

impl PlayerLine {
    pub fn label(self) -> &'static str {
        match self {
            PlayerLine::Idle => "personagem: —",
            PlayerLine::Found => "personagem: encontrado",
            PlayerLine::NotFound => "personagem: não encontrado",
        }
    }
}

/// Avisos fixos acima da lista; cada origem liga e desliga o seu campo.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Banners {
    /// Diário corrompido movido de lugar.
    pub corrupt: Option<String>,
    /// Recuperação do diário bloqueada.
    pub blocked: Option<String>,
    /// Jogo que pode ter ficado parado (SIGCONT falhou).
    pub stuck: Option<String>,
    /// Restos no jogo depois de uma falha.
    pub residue: Option<String>,
}

impl Banners {
    /// Linhas ativas, na ordem corrompido, bloqueado, parado, restos.
    pub fn lines(&self) -> Vec<&str> {
        [&self.corrupt, &self.blocked, &self.stuck, &self.residue]
            .into_iter()
            .filter_map(|b| b.as_deref())
            .collect()
    }
}

/// Estado completo da tela.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    /// Estado de conexão.
    pub conn: Conn,
    /// Cheats, na ordem de chegada.
    pub cheats: Vec<CheatRow>,
    /// Índice da seleção em [`Model::visible`].
    pub selected: usize,
    /// O que está sendo feito numa operação demorada.
    pub busy: Option<String>,
    /// Avisos fixos.
    pub banners: Banners,
    /// Registro de eventos, do mais antigo ao mais novo.
    pub events: VecDeque<String>,
    /// Mostra a categoria de diagnóstico (`--diagnostico`).
    pub diagnostic: bool,
    /// O terminal está menor que o mínimo.
    pub too_small: bool,
    /// Linha `personagem:` (só aparece com cheats de valor na lista).
    pub player: PlayerLine,
    /// `id` do cheat da última linha e o valor de antes da primeira troca da sequência, se ela
    /// veio de [`Model::merge_value_event`].
    last_value_event: Option<(String, String)>,
}

impl Model {
    /// Modelo inicial, aguardando o jogo.
    pub fn new(diagnostic: bool) -> Model {
        Model {
            conn: Conn::Waiting,
            cheats: Vec::new(),
            selected: 0,
            busy: None,
            banners: Banners::default(),
            events: VecDeque::new(),
            diagnostic,
            too_small: false,
            player: PlayerLine::Idle,
            last_value_event: None,
        }
    }

    /// Cheats mostrados: sem `diagnostic`, esconde a categoria de diagnóstico; ordenados por
    /// categoria e, dentro dela, pela ordem de chegada.
    pub fn visible(&self) -> Vec<&CheatRow> {
        let mut rows: Vec<&CheatRow> = self
            .cheats
            .iter()
            .filter(|r| self.diagnostic || r.category != Category::Diagnostico)
            .collect();
        // `sort_by_key` é estável: preserva a ordem de chegada dentro da categoria.
        rows.sort_by_key(|r| r.category);
        rows
    }

    /// `id` do cheat selecionado.
    pub fn selected_id(&self) -> Option<&str> {
        self.visible().get(self.selected).map(|r| r.id.as_str())
    }

    /// Move a seleção sem dar a volta.
    pub fn move_selection(&mut self, delta: i32) {
        let len = self.visible().len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        let target = (self.selected as i64 + i64::from(delta)).clamp(0, len as i64 - 1);
        self.selected = target as usize;
    }

    /// (disponíveis ou ligados, total), só dos cheats mostrados.
    pub fn counts(&self) -> (usize, usize) {
        // Uma ação nunca está ligada: não entra na contagem (spec 006, 6.9).
        let visible: Vec<&CheatRow> = self.visible().into_iter().filter(|r| !r.action).collect();
        let ok = visible
            .iter()
            .filter(|r| r.on || matches!(r.avail, Avail::Available { .. }))
            .count();
        (ok, visible.len())
    }

    /// Acrescenta `"<hhmm> <text>"` ao registro (guarda as 200 últimas) e ao log.
    pub fn push_event(&mut self, hhmm: &str, text: String) {
        tracing::info!("{text}");
        self.last_value_event = None;
        self.append(format!("{hhmm} {text}"));
    }

    /// Como [`Model::push_event`] para trocas de valor: se a última linha veio de uma troca do
    /// mesmo `id`, ela é substituída em vez de somar uma linha por tecla, e mostra o valor de
    /// antes da primeira troca da sequência (`from`) até o atual (`to`).
    pub fn merge_value_event(&mut self, hhmm: &str, id: &str, name: &str, from: &str, to: &str) {
        let first = match &self.last_value_event {
            Some((last_id, first)) if last_id == id && !self.events.is_empty() => {
                Some(first.clone())
            }
            _ => None,
        };
        let merging = first.is_some();
        let first = first.unwrap_or_else(|| from.to_string());
        let text = format!("{name}: {first} → {to}");
        tracing::debug!("{text}");
        let line = format!("{hhmm} {text}");
        self.last_value_event = Some((id.to_string(), first));
        match self.events.back_mut() {
            Some(last) if merging => *last = line,
            _ => self.append(line),
        }
    }

    fn append(&mut self, line: String) {
        self.events.push_back(line);
        while self.events.len() > MAX_EVENTS {
            self.events.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, category: Category, avail: Avail) -> CheatRow {
        CheatRow {
            id: id.to_string(),
            name: id.to_string(),
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

    fn connected() -> Conn {
        Conn::Connected {
            pid: 1,
            version_key: "v".into(),
            has_version_file: true,
            disk: Ok(()),
            engine: true,
        }
    }

    fn avail() -> Avail {
        Avail::Available { verified: true }
    }

    #[test]
    fn conn_labels() {
        assert_eq!(Conn::Waiting.label(), "aguardando o jogo");
        assert_eq!(Conn::NoAccess("x".into()).label(), "sem acesso");
        assert_eq!(Conn::Connecting.label(), "conectando");
        assert_eq!(connected().label(), "conectado");
        assert_eq!(Conn::Many(vec![1, 2]).label(), "vários processos");
        assert_eq!(Conn::Error("x".into()).label(), "erro");
        assert_eq!(Conn::PidGone.label(), "o processo terminou");
    }

    #[test]
    fn selection_moves_over_visible_only() {
        let mut m = Model::new(false);
        m.cheats = vec![
            row("d", Category::Diagnostico, avail()),
            row("m", Category::Movimento, avail()),
            row("i", Category::Itens, avail()),
        ];
        // Ordem visível: categoria (Itens < Movimento); o diagnóstico fica de fora.
        let ids: Vec<&str> = m.visible().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["i", "m"]);
        assert_eq!(m.selected_id(), Some("i"));
        m.move_selection(1);
        assert_eq!(m.selected_id(), Some("m"));
        m.move_selection(1);
        assert_eq!(m.selected_id(), Some("m"), "sem dar a volta no fim");
        m.move_selection(-5);
        assert_eq!(m.selected_id(), Some("i"), "sem dar a volta no início");
    }

    #[test]
    fn selection_on_empty_list() {
        let mut m = Model::new(false);
        assert_eq!(m.selected_id(), None);
        m.move_selection(1);
        m.move_selection(-1);
        assert_eq!(m.selected, 0);
        assert_eq!(m.selected_id(), None);
    }

    #[test]
    fn visible_keeps_arrival_order_within_category() {
        let mut m = Model::new(false);
        m.cheats = vec![
            row("b", Category::Itens, avail()),
            row("a", Category::Itens, avail()),
        ];
        let ids: Vec<&str> = m.visible().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["b", "a"]);
    }

    #[test]
    fn status_mapping() {
        let c = connected();
        let mut r = row("x", Category::Itens, avail());
        assert_eq!(r.status(&c), CheatStatus::Off);
        r.on = true;
        assert_eq!(r.status(&c), CheatStatus::On);
        r.residue = true;
        assert_eq!(r.status(&c), CheatStatus::Residue, "restos vencem o ligado");
        let u = row(
            "y",
            Category::Itens,
            Avail::Unavailable("não encontrado".into()),
        );
        assert_eq!(
            u.status(&c),
            CheatStatus::Unavailable("não encontrado".into())
        );
        // Sem conexão, qualquer linha fica indisponível.
        for conn in [Conn::Waiting, Conn::PidGone, Conn::Connecting] {
            assert_eq!(
                r.status(&conn),
                CheatStatus::Unavailable("jogo não conectado".into())
            );
        }
    }

    #[test]
    fn diagnostic_hidden_and_not_counted() {
        let mut m = Model::new(false);
        let mut on = row("a", Category::Itens, Avail::Unavailable("x".into()));
        on.on = true;
        m.cheats = vec![
            on,
            row("b", Category::Itens, avail()),
            row("c", Category::Movimento, Avail::Unavailable("x".into())),
            row("d", Category::Diagnostico, avail()),
        ];
        assert_eq!(m.visible().len(), 3);
        assert_eq!(m.counts(), (2, 3));
        m.diagnostic = true;
        assert_eq!(m.visible().len(), 4);
        assert_eq!(m.counts(), (3, 4));
    }

    #[test]
    fn banners_lines_in_order() {
        let mut b = Banners::default();
        assert!(b.lines().is_empty());
        b.residue = Some("r".into());
        b.corrupt = Some("c".into());
        b.stuck = Some("s".into());
        b.blocked = Some("b".into());
        assert_eq!(b.lines(), ["c", "b", "s", "r"]);
        b.blocked = None;
        assert_eq!(b.lines(), ["c", "s", "r"]);
    }

    #[test]
    fn push_event_prefixes_time() {
        let mut m = Model::new(false);
        m.push_event("12:04", "ligado".into());
        assert_eq!(m.events.back().map(String::as_str), Some("12:04 ligado"));
    }

    #[test]
    fn value_events_merge() {
        let mut m = Model::new(false);
        m.merge_value_event("12:00", "speed", "v", "1.0", "1.5");
        m.merge_value_event("12:01", "speed", "v", "1.5", "2.0");
        assert_eq!(m.events.len(), 1);
        // A linha fundida mostra o valor de antes da primeira troca.
        assert_eq!(m.events[0], "12:01 v: 1.0 → 2.0");
        // Uma troca de outro id no meio separa.
        m.merge_value_event("12:02", "other", "o", "2", "3");
        m.merge_value_event("12:03", "speed", "v", "2.0", "2.5");
        assert_eq!(m.events.len(), 3);
        assert_eq!(m.events[2], "12:03 v: 2.0 → 2.5");
        // Um evento comum também separa.
        m.push_event("12:04", "ligado".into());
        m.merge_value_event("12:05", "speed", "v", "2.5", "3.0");
        assert_eq!(m.events.len(), 5);
        assert_eq!(m.events[4], "12:05 v: 2.5 → 3.0");
    }

    #[test]
    fn events_capped_at_200() {
        let mut m = Model::new(false);
        for i in 0..250 {
            m.push_event("00:00", format!("e{i}"));
        }
        assert_eq!(m.events.len(), 200);
        assert_eq!(m.events.front().map(String::as_str), Some("00:00 e50"));
        assert_eq!(m.events.back().map(String::as_str), Some("00:00 e249"));
        for i in 0..250 {
            m.merge_value_event("00:00", &format!("id{i}"), "n", "a", &format!("v{i}"));
        }
        assert_eq!(m.events.len(), 200);
    }

    #[test]
    fn checking_shows_verificando() {
        let r = row("f", Category::Sobrevivencia, Avail::Checking);
        assert_eq!(
            r.status(&connected()),
            CheatStatus::Unavailable(CHECKING.to_string())
        );
        // Não conta como disponível.
        let mut m = Model::new(false);
        m.cheats = vec![r];
        assert_eq!(m.counts(), (0, 1));
    }

    #[test]
    fn player_line_labels() {
        assert_eq!(PlayerLine::Idle.label(), "personagem: —");
        assert_eq!(PlayerLine::Found.label(), "personagem: encontrado");
        assert_eq!(PlayerLine::NotFound.label(), "personagem: não encontrado");
        assert_eq!(Model::new(false).player, PlayerLine::Idle);
    }

    #[test]
    fn top_count_ignores_action() {
        let mut model = Model::new(false);
        let mut on = row("a", Category::Itens, avail());
        on.on = true;
        let mut act = row("b", Category::Progressao, avail());
        act.field = true;
        act.action = true;
        model.cheats = vec![on, act, row("c", Category::Itens, avail())];
        assert_eq!(model.counts(), (2, 2));
    }
}
