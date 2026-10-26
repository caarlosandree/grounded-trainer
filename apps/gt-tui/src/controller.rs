//! Controlador da TUI (spec 003, 6.1, 6.2 e 6.10): conecta ao jogo, busca o catálogo numa
//! thread, recupera o diário, percebe o fechamento do jogo e refaz as buscas sozinho.
//!
//! O controlador não lê o relógio: todo instante vem do `now` que o laço passa (a duração da
//! busca já vem pronta no [`Search`]). Isso deixa os testes sem `sleep`.
//!
//! Os campos `session` e `backend` seguem esta ordem de propósito: a sessão (e o motor dela)
//! cai antes da trava do diário, que mora no backend.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};

use gt_core::procinfo::Identity;

use crate::backend::{Backend, CheatInfo, Find, Recovered, Search, SearchError, Session};
use crate::model::{Avail, CheatRow, Conn, Model};

/// Devolve a hora do registro de eventos como `HH:MM`.
pub type Clock = Box<dyn Fn() -> String>;

/// Quanto o laço da TUI dorme entre voltas (limite de resposta da spec 6.10: 250 ms).
pub const LOOP_INTERVAL: Duration = Duration::from_millis(100);

/// Intervalo da procura do processo do jogo sem conexão.
const FIND_INTERVAL: Duration = Duration::from_secs(2);
/// Menor intervalo entre duas buscas automáticas.
const AUTO_MIN_INTERVAL: Duration = Duration::from_secs(5);
/// Por quanto tempo, depois de cada conexão, as buscas automáticas continuam.
const AUTO_WINDOW: Duration = Duration::from_secs(120);

/// Mensagem do topo durante a recuperação do diário.
const RECOVERING: &str = "recuperando o diário…";
/// Motivos do catálogo que as buscas automáticas tentam resolver.
const NOT_FOUND: &str = "não encontrado";
const INCONCLUSIVE: &str = "inconclusivo";

type SearchResult<P> = Result<Search<P>, SearchError>;

/// Para que a busca em andamento foi pedida.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchKind {
    /// Primeira busca de uma conexão (catálogo inteiro; abre a sessão).
    Connect,
    /// Nova busca dos cheats desligados, com a sessão aberta.
    Refresh,
}

/// A busca em andamento na thread.
struct InFlight<P> {
    rx: Receiver<SearchResult<P>>,
    kind: SearchKind,
    /// Número da conexão quando a busca começou.
    conn_no: u64,
    /// Quando a busca começou.
    started: Instant,
    /// Contador de operações por cheat quando a busca começou.
    ops: HashMap<String, u64>,
}

/// O intervalo entre duas buscas automáticas, dado quanto a última levou.
fn auto_interval(took: Duration) -> Duration {
    AUTO_MIN_INTERVAL.max(took.saturating_mul(2))
}

/// Linha nova da lista para um cheat que a busca viu pela primeira vez.
fn row_of(c: CheatInfo) -> CheatRow {
    let k = c.value.as_ref().map_or(0, |v| v.default_k.min(v.steps()));
    CheatRow {
        id: c.id,
        name: c.name,
        category: c.category,
        value: c.value,
        k,
        applied_k: None,
        avail: c.avail,
        on: false,
        residue: false,
        desired: false,
    }
}

/// Se uma linha com `old` deve manter o estado quando a nova busca devolve `new`: um cheat
/// que já estava resolvido (disponível) nesta conexão e que a busca não acha (`não encontrado`,
/// `inconclusivo`) fica como estava (spec 6.2).
fn keeps_state(old: &Avail, new: &Avail) -> bool {
    let lost = matches!(new, Avail::Unavailable(t) if t == NOT_FOUND || t == INCONCLUSIVE);
    lost && matches!(old, Avail::Available { .. })
}

/// O controlador: o modelo da tela e tudo o que fala com o alvo.
pub struct Controller<B: Backend> {
    /// Modelo da tela.
    pub model: Model,
    /// Sessão com o alvo conectado. Antes do `backend`: cai antes da trava do diário.
    session: Option<B::Session>,
    backend: B,
    clock: Clock,
    pid_fixed: bool,
    /// Muda a cada conexão nova e a cada fechamento do jogo.
    conn_no: u64,
    /// Operações por cheat (a Tarefa 12 chama `note_op` em toda operação).
    op_counts: HashMap<String, u64>,
    search: Option<InFlight<B::Payload>>,
    search_panicked: bool,
    /// Identidade do processo da conexão atual.
    identity: Option<Identity>,
    last_find: Option<Instant>,
    /// Fim do prazo das buscas automáticas desta conexão.
    auto_until: Option<Instant>,
    /// Quando a próxima busca automática pode começar.
    next_auto: Option<Instant>,
    /// Quanto a última busca levou.
    last_took: Duration,
}

impl<B: Backend> Controller<B> {
    /// Controlador sem conexão. `diagnostic` mostra a categoria de diagnóstico; `pid_fixed`
    /// é o `--pid` (o processo que fecha não volta).
    pub fn new(backend: B, clock: Clock, diagnostic: bool, pid_fixed: bool) -> Controller<B> {
        Controller {
            model: Model::new(diagnostic),
            session: None,
            backend,
            clock,
            pid_fixed,
            conn_no: 0,
            op_counts: HashMap::new(),
            search: None,
            search_panicked: false,
            identity: None,
            last_find: None,
            auto_until: None,
            next_auto: None,
            last_took: Duration::ZERO,
        }
    }

    /// Recupera o diário antes de qualquer conexão (spec 6.1, passo 6).
    pub fn start(&mut self, draw: &mut dyn FnMut(&Model)) {
        self.recover_diary(draw);
    }

    /// Uma volta do laço: colhe a busca, percebe o fechamento do jogo, procura o jogo e
    /// dispara as novas buscas automáticas.
    pub fn tick(&mut self, now: Instant, draw: &mut dyn FnMut(&Model)) {
        self.poll_search(now);
        self.check_exit(now);
        if self.session.is_none() {
            self.poll_find(now, draw);
        } else {
            self.maybe_auto_search(now);
        }
    }

    /// Conta uma operação no cheat `id`: o resultado de uma busca que começou antes dela
    /// deixa de valer para esse cheat.
    pub fn note_op(&mut self, id: &str) {
        *self.op_counts.entry(id.to_string()).or_insert(0) += 1;
    }

    /// Bloqueia até o resultado da busca chegar (ou o prazo) e o processa como o `tick`, com o
    /// mesmo `now`. Devolve `true` se a busca terminou (com resultado ou sem, por pânico).
    #[doc(hidden)]
    pub fn wait_search(&mut self, now: Instant, timeout: Duration) -> bool {
        let Some(inflight) = self.search.take() else {
            return false;
        };
        match inflight.rx.recv_timeout(timeout) {
            Ok(res) => {
                self.finish_search(inflight, res, now);
                true
            }
            Err(RecvTimeoutError::Timeout) => {
                self.search = Some(inflight);
                false
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.search_panicked = true;
                true
            }
        }
    }

    /// A thread de busca terminou sem mandar resultado (pânico): o `main` sai pelo caminho
    /// de pânico.
    pub fn search_panicked(&self) -> bool {
        self.search_panicked
    }

    // ---- registro, seleção e banners ----

    fn event(&mut self, text: impl Into<String>) {
        let hhmm = (self.clock)();
        self.model.push_event(&hhmm, text.into());
    }

    /// Mantém a seleção dentro da lista visível.
    fn clamp_selected(&mut self) {
        let last = self.model.visible().len().saturating_sub(1);
        self.model.selected = self.model.selected.min(last);
    }

    /// O diário está resolvido (a recuperação não deixou nada).
    fn diary_resolved(&self) -> bool {
        self.model.banners.blocked.is_none()
    }

    /// Chama a sessão e, depois, liga ou desliga o aviso de jogo parado. `None` sem sessão.
    fn session_call<R>(&mut self, f: impl FnOnce(&mut B::Session) -> R) -> Option<R> {
        let session = self.session.as_mut()?;
        let out = f(session);
        self.sync_stuck();
        Some(out)
    }

    fn sync_stuck(&mut self) {
        let stuck = self.session.as_ref().is_some_and(|s| s.is_stuck());
        self.model.banners.stuck = match (&self.model.conn, stuck) {
            (Conn::Connected { pid, .. }, true) => {
                Some(format!("o jogo pode ter ficado parado: kill -CONT {pid}"))
            }
            _ => None,
        };
    }

    // ---- recuperação do diário ----

    /// Mostra `recuperando o diário…`, recupera e aplica o resultado (linhas, avisos fixos).
    fn recover_diary(&mut self, draw: &mut dyn FnMut(&Model)) {
        self.model.busy = Some(RECOVERING.to_string());
        draw(&self.model);
        let recovered = self.backend.recover();
        self.model.busy = None;
        match recovered {
            Recovered::Clean { lines, corrupt } => {
                for line in lines {
                    self.event(line);
                }
                if let Some(path) = corrupt {
                    self.model.banners.corrupt = Some(format!(
                        "diário corrompido movido para {}; o jogo pode ter patches sem \
                         registro, confira com gt diff",
                        path.display()
                    ));
                }
                self.model.banners.blocked = None;
            }
            Recovered::Blocked { lines, text } => {
                for line in lines {
                    self.event(line);
                }
                self.model.banners.blocked = Some(text);
            }
        }
    }

    // ---- procura e conexão ----

    /// Há uma busca em andamento que ainda vale (a conexão não mudou).
    fn search_live(&self) -> bool {
        self.search
            .as_ref()
            .is_some_and(|s| s.conn_no == self.conn_no)
    }

    fn poll_find(&mut self, now: Instant, draw: &mut dyn FnMut(&Model)) {
        if matches!(self.model.conn, Conn::PidGone) || self.search_live() {
            return;
        }
        if self
            .last_find
            .is_some_and(|last| now.saturating_duration_since(last) < FIND_INTERVAL)
        {
            return;
        }
        self.last_find = Some(now);
        match self.backend.find() {
            Find::NotRunning => self.model.conn = Conn::Waiting,
            Find::Many(pids) => self.model.conn = Conn::Many(pids),
            Find::PidGone => self.model.conn = Conn::PidGone,
            Find::Error(text) => self.model.conn = Conn::Error(text),
            Find::Found(pid) => self.begin_connection(pid, now, draw),
        }
    }

    /// Achou o jogo: recupera o diário de novo, passa a `Connecting` e inicia a busca.
    fn begin_connection(&mut self, pid: i32, now: Instant, draw: &mut dyn FnMut(&Model)) {
        self.recover_diary(draw);
        self.conn_no += 1;
        self.model.conn = Conn::Connecting;
        if let Err(e) = self.spawn_search(SearchKind::Connect, pid, None, None, now) {
            self.model.conn = Conn::Error(format!("não foi possível iniciar a busca: {e}"));
        }
    }

    /// Inicia a busca numa thread; o resultado volta por um canal.
    fn spawn_search(
        &mut self,
        kind: SearchKind,
        pid: i32,
        ids: Option<Vec<String>>,
        expected: Option<Identity>,
        now: Instant,
    ) -> std::io::Result<()> {
        let search_fn = self.backend.search_fn();
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("gt-search".to_string())
            .spawn(move || {
                // Um pânico em `search_fn` derruba o `tx` sem mandar nada: o `rx` percebe.
                let res = search_fn(pid, ids, expected);
                let _ = tx.send(res);
            })?;
        self.search = Some(InFlight {
            rx,
            kind,
            conn_no: self.conn_no,
            started: now,
            ops: self.op_counts.clone(),
        });
        Ok(())
    }

    fn poll_search(&mut self, now: Instant) {
        let Some(inflight) = self.search.take() else {
            return;
        };
        match inflight.rx.try_recv() {
            Ok(res) => self.finish_search(inflight, res, now),
            Err(TryRecvError::Empty) => self.search = Some(inflight),
            Err(TryRecvError::Disconnected) => self.search_panicked = true,
        }
    }

    fn finish_search(
        &mut self,
        inflight: InFlight<B::Payload>,
        res: SearchResult<B::Payload>,
        now: Instant,
    ) {
        if inflight.conn_no != self.conn_no {
            tracing::debug!("resultado de busca descartado: a conexão mudou");
            return;
        }
        match inflight.kind {
            SearchKind::Connect => self.finish_connect(res, &inflight, now),
            SearchKind::Refresh => self.finish_refresh(res, &inflight, now),
        }
    }

    /// Falha da busca de conexão: o estado mostra a causa e a procura recomeça em 2 s.
    fn connect_failed(&mut self, e: SearchError, now: Instant) {
        self.last_find = Some(now);
        self.model.conn = match e {
            SearchError::NoAccess(hint) => Conn::NoAccess(hint),
            SearchError::Gone => Conn::Waiting,
            SearchError::Other(text) => Conn::Error(text),
        };
    }

    fn finish_connect(
        &mut self,
        res: SearchResult<B::Payload>,
        inflight: &InFlight<B::Payload>,
        now: Instant,
    ) {
        let search = match res {
            Ok(search) => search,
            Err(e) => return self.connect_failed(e, now),
        };
        // O motor só abre com o diário resolvido e o cabeçalho do disco lido.
        let engine = self.diary_resolved() && search.disk.is_ok();
        let pid = search.pid;
        let identity = search.identity.clone();
        let version_key = search.version_key.clone();
        let has_version_file = search.has_version_file;
        let disk = search.disk.clone();
        let took = search.took;
        let cheats = search.cheats.clone();
        let session = match self.backend.open(search, engine) {
            Ok(session) => session,
            Err(e) => {
                self.last_find = Some(now);
                self.model.conn = Conn::Error(e.to_string());
                return;
            }
        };
        let has_engine = session.has_engine();
        self.session = Some(session);
        self.identity = Some(identity);
        self.model.conn = Conn::Connected {
            pid,
            version_key: version_key.clone(),
            has_version_file,
            disk,
            engine: has_engine,
        };
        self.sync_stuck();
        self.merge_connect(cheats);
        self.auto_until = Some(now + AUTO_WINDOW);
        self.schedule_auto(inflight.started, took, now);
        self.event(format!(
            "conectado ao jogo (PID {pid}, versão {version_key})"
        ));
    }

    /// Junta o catálogo da busca de conexão na lista: as linhas que já existiam (de uma conexão
    /// anterior) guardam o `k` mostrado; a disponibilidade é a da busca nova.
    fn merge_connect(&mut self, cheats: Vec<CheatInfo>) {
        for c in cheats {
            match self.model.cheats.iter_mut().find(|r| r.id == c.id) {
                Some(row) => {
                    row.name = c.name;
                    row.category = c.category;
                    row.avail = c.avail;
                    row.k = match &c.value {
                        Some(v) => row.k.min(v.steps()),
                        None => 0,
                    };
                    row.value = c.value;
                }
                None => self.model.cheats.push(row_of(c)),
            }
        }
        self.clamp_selected();
    }

    fn finish_refresh(
        &mut self,
        res: SearchResult<B::Payload>,
        inflight: &InFlight<B::Payload>,
        now: Instant,
    ) {
        let search = match res {
            Ok(search) => search,
            Err(SearchError::Gone) => return self.on_game_closed(now),
            Err(e) => {
                tracing::warn!(erro = %e, "a nova busca falhou");
                return self.schedule_auto(inflight.started, self.last_took, now);
            }
        };
        let took = search.took;
        let disk = search.disk.clone();
        let version_key = search.version_key.clone();
        let cheats = search.cheats.clone();
        if self.session_call(|s| s.merge_search(search)) != Some(true) {
            // Outro processo (ou nenhuma sessão): a conexão acabou.
            return self.on_game_closed(now);
        }
        for c in cheats {
            let ops_now = self.op_counts.get(&c.id).copied().unwrap_or(0);
            let ops_then = inflight.ops.get(&c.id).copied().unwrap_or(0);
            match self.model.cheats.iter_mut().find(|r| r.id == c.id) {
                Some(row) => {
                    if row.on || ops_now != ops_then || keeps_state(&row.avail, &c.avail) {
                        continue;
                    }
                    row.avail = c.avail;
                }
                None => self.model.cheats.push(row_of(c)),
            }
        }
        self.clamp_selected();
        let disk_was_unreadable = matches!(
            &self.model.conn,
            Conn::Connected { disk, .. } if disk.is_err()
        );
        let disk_now_ok = disk.is_ok();
        if let Conn::Connected {
            version_key: vk,
            disk: d,
            ..
        } = &mut self.model.conn
        {
            *vk = version_key;
            *d = disk;
        }
        if disk_was_unreadable && disk_now_ok {
            self.open_engine_if_possible();
        }
        self.schedule_auto(inflight.started, took, now);
    }

    /// Abre o motor quando o diário está resolvido e o disco foi lido.
    fn open_engine_if_possible(&mut self) {
        let Conn::Connected { disk, engine, .. } = &self.model.conn else {
            return;
        };
        if *engine || disk.is_err() || !self.diary_resolved() {
            return;
        }
        match self.session_call(|s| s.open_engine()) {
            Some(Ok(())) => {
                if let Conn::Connected { engine, .. } = &mut self.model.conn {
                    *engine = true;
                }
                self.event("motor aberto");
            }
            Some(Err(e)) => self.event(format!("não foi possível abrir o motor: {e}")),
            None => {}
        }
    }

    // ---- fechamento do jogo ----

    fn check_exit(&mut self, now: Instant) {
        if self.session_call(|s| s.exited()) == Some(true) {
            self.on_game_closed(now);
        }
    }

    /// O jogo fechou: a sessão descarta o diário sem escrever e cai; a lista perde o que estava
    /// ligado (os desejados da Tarefa 12 ficam) e a conexão volta a esperar.
    fn on_game_closed(&mut self, now: Instant) {
        self.session_call(|s| s.discard_gone());
        self.session = None;
        self.conn_no += 1;
        self.identity = None;
        self.auto_until = None;
        self.next_auto = None;
        self.last_find = Some(now);
        self.model.busy = None;
        self.model.banners.stuck = None;
        self.model.banners.residue = None;
        for row in &mut self.model.cheats {
            row.on = false;
            row.applied_k = None;
            row.residue = false;
        }
        self.model.conn = if self.pid_fixed {
            Conn::PidGone
        } else {
            Conn::Waiting
        };
        self.event("o jogo fechou");
        self.clamp_selected();
    }

    // ---- novas buscas ----

    /// Marca o início da próxima busca automática: `max(5 s, 2 × took)` depois do início
    /// desta, e nunca antes de agora.
    fn schedule_auto(&mut self, started: Instant, took: Duration, now: Instant) {
        self.last_took = took;
        self.next_auto = Some((started + auto_interval(took)).max(now));
    }

    /// Há cheat mostrado que as buscas automáticas ainda tentam resolver.
    fn wants_auto_search(&self) -> bool {
        self.model.visible().iter().any(|r| {
            !r.on
                && matches!(&r.avail, Avail::Unavailable(t) if t == NOT_FOUND || t == INCONCLUSIVE)
        })
    }

    fn maybe_auto_search(&mut self, now: Instant) {
        if self.search_live() {
            return;
        }
        let (Some(until), Some(next)) = (self.auto_until, self.next_auto) else {
            return;
        };
        if now >= until || now < next || !self.wants_auto_search() {
            return;
        }
        self.start_refresh(now);
    }

    /// Inicia a busca dos cheats desligados (só com a conexão aberta e o disco lido).
    fn start_refresh(&mut self, now: Instant) -> bool {
        let Conn::Connected { pid, disk, .. } = &self.model.conn else {
            return false;
        };
        if disk.is_err() {
            return false;
        }
        let pid = *pid;
        let ids: Vec<String> = self
            .model
            .cheats
            .iter()
            .filter(|r| !r.on)
            .map(|r| r.id.clone())
            .collect();
        let expected = self.identity.clone();
        match self.spawn_search(SearchKind::Refresh, pid, Some(ids), expected, now) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(erro = %e, "não foi possível iniciar a nova busca");
                false
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::backend::SearchError;
    use crate::fake::{Call, FakeBackend, FakeCtl, SearchSpec, available, cheat, unavailable};

    const WAIT: Duration = Duration::from_secs(10);

    struct H {
        c: Controller<FakeBackend>,
        ctl: FakeCtl,
        draw: Box<dyn FnMut(&Model)>,
        t0: Instant,
    }

    impl H {
        fn new() -> H {
            H::build(false)
        }

        fn pid_fixed() -> H {
            H::build(true)
        }

        fn build(pid_fixed: bool) -> H {
            let backend = FakeBackend::new();
            let ctl = backend.ctl();
            let draw = ctl.drawer();
            let c = Controller::new(backend, Box::new(|| "12:00".to_string()), false, pid_fixed);
            H {
                c,
                ctl,
                draw,
                t0: Instant::now(),
            }
        }

        fn at(&self, ms: u64) -> Instant {
            self.t0 + Duration::from_millis(ms)
        }

        fn tick(&mut self, ms: u64) {
            let now = self.at(ms);
            self.c.tick(now, &mut *self.draw);
        }

        fn wait(&mut self, ms: u64) -> bool {
            let now = self.at(ms);
            self.c.wait_search(now, WAIT)
        }

        /// Acha o jogo (PID 100) em `ms` e espera a busca de conexão.
        fn connect(&mut self, ms: u64) {
            self.ctl.set_find(Find::Found(100));
            self.tick(ms);
            assert!(self.wait(ms), "a busca de conexão não terminou");
        }

        /// Há uma busca em andamento (começou num `tick`).
        fn searching(&self) -> bool {
            self.c.search.is_some()
        }

        fn row(&self, id: &str) -> &CheatRow {
            self.c.model.cheats.iter().find(|r| r.id == id).unwrap()
        }

        fn events(&self) -> Vec<&str> {
            self.c.model.events.iter().map(String::as_str).collect()
        }
    }

    fn not_found() -> Avail {
        unavailable("não encontrado")
    }

    fn connected(c: &Controller<FakeBackend>) -> (i32, String, bool, bool) {
        match &c.model.conn {
            Conn::Connected {
                pid,
                version_key,
                disk,
                engine,
                ..
            } => (*pid, version_key.clone(), disk.is_ok(), *engine),
            other => panic!("esperava conectado, veio {other:?}"),
        }
    }

    fn blocked(text: &str) -> Recovered {
        Recovered::Blocked {
            lines: vec!["restaurado: a".to_string()],
            text: text.to_string(),
        }
    }

    #[test]
    fn start_blocked_recovery_blocks_toggles() {
        let mut h = H::new();
        h.ctl
            .push_recover(blocked("não foi possível restaurar b agora"));
        h.c.start(&mut *h.draw);
        assert_eq!(
            h.c.model.banners.blocked.as_deref(),
            Some("não foi possível restaurar b agora")
        );
        assert_eq!(h.events(), ["12:00 restaurado: a"]);
        // A tela mostrou `recuperando o diário…` antes de chamar `recover`.
        assert_eq!(
            h.ctl.calls(),
            [Call::Recover {
                busy: Some(RECOVERING.to_string())
            }]
        );
        assert_eq!(h.c.model.busy, None);

        // Conectando com o diário ainda bloqueado: conectado, sem motor.
        h.ctl.push_recover(blocked("ainda bloqueado"));
        h.ctl.set_catalog(vec![cheat("a", available())]);
        h.connect(0);
        assert_eq!(connected(&h.c), (100, "v1".to_string(), true, false));
        assert!(h.ctl.calls().contains(&Call::Open { engine: false }));
        assert_eq!(
            h.c.model.banners.blocked.as_deref(),
            Some("ainda bloqueado")
        );
    }

    #[test]
    fn start_blocked_then_resolved_on_connect_opens_engine() {
        let mut h = H::new();
        h.ctl.push_recover(blocked("bloqueado"));
        h.c.start(&mut *h.draw);
        assert!(h.c.model.banners.blocked.is_some());
        // A recuperação da conexão resolve: o aviso some e o motor abre.
        h.ctl.set_catalog(vec![cheat("a", available())]);
        h.connect(0);
        assert_eq!(h.c.model.banners.blocked, None);
        assert_eq!(connected(&h.c), (100, "v1".to_string(), true, true));
        assert!(h.ctl.calls().contains(&Call::Open { engine: true }));
    }

    #[test]
    fn start_corrupt_goes_to_banner() {
        let mut h = H::new();
        h.ctl.push_recover(Recovered::Clean {
            lines: vec!["diário descartado: o jogo fechou".to_string()],
            corrupt: Some(PathBuf::from("/x/journal.json.bad")),
        });
        h.c.start(&mut *h.draw);
        assert_eq!(
            h.c.model.banners.corrupt.as_deref(),
            Some(
                "diário corrompido movido para /x/journal.json.bad; o jogo pode ter patches \
                 sem registro, confira com gt diff"
            )
        );
        assert_eq!(h.c.model.banners.blocked, None);
        assert_eq!(h.events(), ["12:00 diário descartado: o jogo fechou"]);
    }

    #[test]
    fn connect_flow_waiting_connecting_connected() {
        let mut h = H::new();
        h.ctl
            .set_catalog(vec![cheat("a", available()), cheat("b", not_found())]);

        // Sem jogo: procura a cada 2 s.
        h.tick(0);
        assert_eq!(h.c.model.conn, Conn::Waiting);
        assert_eq!(h.ctl.find_calls(), 1);
        h.tick(1_000);
        assert_eq!(h.ctl.find_calls(), 1);
        h.tick(2_000);
        assert_eq!(h.ctl.find_calls(), 2);

        // O jogo aparece; a busca fica presa para ver `Connecting`.
        h.ctl.set_find(Find::Found(100));
        h.ctl.hold_search();
        h.tick(4_000);
        assert_eq!(h.c.model.conn, Conn::Connecting);
        h.tick(4_100);
        h.tick(6_500);
        assert_eq!(h.c.model.conn, Conn::Connecting);
        assert_eq!(h.ctl.find_calls(), 3, "não procura durante a busca");

        h.ctl.release_search();
        assert!(h.wait(6_600));
        assert_eq!(connected(&h.c), (100, "v1".to_string(), true, true));
        let ids: Vec<&str> = h.c.model.cheats.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(h.row("a").avail, available());
        assert_eq!(h.row("b").avail, not_found());
        assert!(h.events().iter().any(|e| e.contains("conectado ao jogo")));
        assert_eq!(h.c.model.busy, None);
    }

    #[test]
    fn recover_runs_before_each_search() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![cheat("a", available())]);
        h.connect(0);
        h.ctl.set_exited(true);
        h.tick(100);
        assert_eq!(h.c.model.conn, Conn::Waiting);
        h.ctl.set_exited(false);
        h.connect(5_000);

        let calls = h.ctl.calls();
        let recovers: Vec<usize> = calls
            .iter()
            .enumerate()
            .filter(|(_, c)| matches!(c, Call::Recover { .. }))
            .map(|(i, _)| i)
            .collect();
        let searches: Vec<usize> = calls
            .iter()
            .enumerate()
            .filter(|(_, c)| matches!(c, Call::Search { .. }))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(recovers.len(), 2, "um recover por conexão: {calls:?}");
        assert_eq!(searches.len(), 2);
        for (r, s) in recovers.iter().zip(&searches) {
            assert!(r < s, "recover antes da busca: {calls:?}");
            assert_eq!(
                calls[*r],
                Call::Recover {
                    busy: Some(RECOVERING.to_string())
                }
            );
        }
    }

    #[test]
    fn connect_without_desired_never_applies() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![
            cheat("a", available()),
            cheat("b", available()),
            cheat("c", not_found()),
        ]);
        h.connect(0);
        for t in 1..30 {
            h.tick(t * 1_000);
            h.wait(t * 1_000);
        }
        assert!(h.c.model.cheats.iter().all(|r| !r.desired && !r.on));
        assert_eq!(h.ctl.count(|c| matches!(c, Call::Apply { .. })), 0);
    }

    #[test]
    fn stale_search_result_is_dropped() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![cheat("a", not_found())]);
        h.connect(0);
        assert_eq!(h.ctl.search_calls(), 1);

        // Uma nova busca fica presa; o jogo fecha no meio.
        h.ctl
            .push_search(Ok(SearchSpec::new(vec![cheat("a", available())])));
        h.ctl.hold_search();
        h.tick(5_000);
        assert!(h.searching());
        h.ctl.set_exited(true);
        h.tick(5_100);
        assert_eq!(h.c.model.conn, Conn::Waiting);

        // O resultado chega depois do fechamento: nada é aplicado.
        h.ctl.release_search();
        assert!(h.wait(5_200));
        assert_eq!(h.c.model.conn, Conn::Waiting);
        assert_eq!(h.row("a").avail, not_found());
        assert_eq!(
            h.ctl.count(|c| matches!(c, Call::MergeSearch { .. })),
            0,
            "o resultado velho não chega à sessão"
        );
        assert!(!h.c.search_panicked());
        // O resultado velho também não refaz o fechamento (nem repete o evento).
        let closed = h
            .events()
            .iter()
            .filter(|e| e.ends_with("o jogo fechou"))
            .count();
        assert_eq!(closed, 1);
    }

    #[test]
    fn stale_result_after_reconnect_is_dropped() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![cheat("a", not_found())]);
        h.connect(0);
        h.ctl
            .push_search(Ok(SearchSpec::new(vec![cheat("a", available())])));
        h.ctl.hold_search();
        h.tick(5_000);
        h.ctl.set_exited(true);
        h.tick(5_100);
        h.ctl.set_exited(false);
        // O jogo reabre antes de a busca velha terminar: a conexão nova a substitui.
        h.ctl.set_catalog(vec![cheat("a", not_found())]);
        h.tick(8_000);
        assert_eq!(h.c.model.conn, Conn::Connecting);
        h.ctl.release_search();
        assert!(h.wait(8_100));
        // O resultado da busca nova vale (conexão 2); o da velha foi descartado.
        assert_eq!(connected(&h.c).0, 100);
        assert_eq!(h.ctl.count(|c| matches!(c, Call::MergeSearch { .. })), 0);
    }

    #[test]
    fn op_during_search_drops_that_cheat_result() {
        let mut h = H::new();
        h.ctl
            .set_catalog(vec![cheat("a", not_found()), cheat("b", not_found())]);
        h.connect(0);

        h.ctl.push_search(Ok(SearchSpec::new(vec![
            cheat("a", available()),
            cheat("b", available()),
        ])));
        h.ctl.hold_search();
        h.tick(5_000);
        // Uma operação em `a` enquanto a busca roda.
        h.c.note_op("a");
        h.ctl.release_search();
        assert!(h.wait(5_100));
        assert_eq!(
            h.row("a").avail,
            not_found(),
            "o resultado de `a` foi descartado"
        );
        assert_eq!(h.row("b").avail, available());
    }

    #[test]
    fn op_before_search_does_not_drop_result() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![cheat("a", not_found())]);
        h.connect(0);
        h.c.note_op("a");
        h.ctl
            .push_search(Ok(SearchSpec::new(vec![cheat("a", available())])));
        h.tick(5_000);
        assert!(h.wait(5_100));
        assert_eq!(h.row("a").avail, available());
    }

    #[test]
    fn unresolved_cheat_keeps_state_on_research() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![
            cheat("a", available()),
            cheat("b", not_found()),
            cheat("c", available()),
            cheat("d", not_found()),
            cheat("e", available()),
        ]);
        h.connect(0);
        h.ctl.push_search(Ok(SearchSpec::new(vec![
            cheat("a", not_found()),
            cheat("b", available()),
            cheat("c", unavailable("ambíguo")),
            cheat("d", unavailable("inconclusivo")),
            cheat("e", unavailable("inconclusivo")),
        ])));
        h.tick(5_000);
        assert!(h.wait(5_000));
        // Resolvido que a busca não acha: mantém.
        assert_eq!(h.row("a").avail, available());
        assert_eq!(h.row("e").avail, available());
        // Resultado novo ou estado estável diferente: troca.
        assert_eq!(h.row("b").avail, available());
        assert_eq!(h.row("c").avail, unavailable("ambíguo"));
        // Não resolvido que continua sem achar: troca de motivo.
        assert_eq!(h.row("d").avail, unavailable("inconclusivo"));
        assert_eq!(
            h.ctl
                .search_ids()
                .last()
                .cloned()
                .flatten()
                .map(|v| v.len()),
            Some(5)
        );
    }

    #[test]
    fn refresh_skips_ids_that_are_on() {
        let mut h = H::new();
        h.ctl
            .set_catalog(vec![cheat("a", available()), cheat("b", not_found())]);
        h.connect(0);
        h.c.model.cheats[0].on = true;
        h.tick(5_000);
        assert!(h.searching());
        assert!(h.wait(5_000));
        assert_eq!(
            h.ctl.search_ids().last().cloned().flatten(),
            Some(vec!["b".to_string()])
        );
        // A busca é só dos desligados: o ligado não muda mesmo que ela o listasse.
        assert_eq!(h.row("a").avail, available());
    }

    #[test]
    fn auto_search_cadence_and_120s_limit() {
        let mut h = H::new();
        let mut spec = SearchSpec::new(vec![cheat("a", not_found())]);
        spec.took = Duration::from_secs(4);
        h.ctl.set_default_search(spec);
        h.connect(0);

        // `took` de 4 s: intervalo de 8 s.
        h.tick(7_900);
        assert!(!h.searching());
        h.tick(8_000);
        assert!(h.searching());
        assert!(h.wait(8_000));
        h.tick(15_900);
        assert!(!h.searching());

        // Anda de 1 em 1 s até bem depois dos 120 s e anota quando cada busca começa.
        let mut starts = vec![8_000];
        for t in (9..300).map(|s| s * 1_000) {
            h.tick(t);
            if h.searching() {
                starts.push(t);
                assert!(h.wait(t));
            }
        }
        let expected: Vec<u64> = (1..=14).map(|n| n * 8_000).collect();
        assert_eq!(starts, expected, "de 8 em 8 s e nada depois dos 120 s");
        // Mais a busca da conexão.
        assert_eq!(h.ctl.search_calls(), 15);
    }

    #[test]
    fn auto_search_min_interval_is_5s() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![cheat("a", not_found())]);
        h.connect(0);
        h.tick(4_900);
        assert!(!h.searching());
        h.tick(5_000);
        assert!(h.searching());
    }

    #[test]
    fn auto_search_only_while_something_is_missing() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![
            cheat("a", available()),
            cheat("b", unavailable("ambíguo")),
        ]);
        h.connect(0);
        for t in 1..20 {
            h.tick(t * 1_000);
            assert!(!h.searching());
        }
        assert_eq!(h.ctl.search_calls(), 1);
    }

    #[test]
    fn auto_search_ignores_hidden_diagnostic() {
        let mut h = H::new();
        let mut diag = cheat("d", not_found());
        diag.category = gt_grounded2::signatures::Category::Diagnostico;
        h.ctl.set_catalog(vec![cheat("a", available()), diag]);
        h.connect(0);
        h.tick(10_000);
        assert!(!h.searching(), "diagnóstico escondido não conta");
    }

    #[test]
    fn disk_unreadable_connected_without_engine() {
        let mut h = H::new();
        let mut spec = SearchSpec::new(vec![
            cheat("a", unavailable("disco ilegível")),
            cheat("b", not_found()),
        ]);
        spec.disk = Err("cabeçalho ilegível".to_string());
        h.ctl.set_default_search(spec);
        h.connect(0);
        let Conn::Connected { disk, engine, .. } = &h.c.model.conn else {
            panic!("esperava conectado");
        };
        assert_eq!(disk, &Err("cabeçalho ilegível".to_string()));
        assert!(!engine);
        assert!(h.ctl.calls().contains(&Call::Open { engine: false }));
        for t in 1..60 {
            h.tick(t * 1_000);
            assert!(!h.searching(), "disco ilegível não dispara busca");
        }
        assert_eq!(h.ctl.search_calls(), 1);
    }

    #[test]
    fn refresh_opens_engine_when_disk_becomes_readable() {
        let mut h = H::new();
        let mut bad = SearchSpec::new(vec![cheat("a", unavailable("disco ilegível"))]);
        bad.disk = Err("sem cabeçalho".to_string());
        h.ctl.set_default_search(bad);
        h.connect(0);
        assert!(!connected(&h.c).3);

        h.ctl
            .push_search(Ok(SearchSpec::new(vec![cheat("a", available())])));
        // Disco `Err` não dispara a automática; a nova busca vem de `start_refresh`.
        h.c.model.conn = match h.c.model.conn.clone() {
            Conn::Connected {
                pid,
                version_key,
                has_version_file,
                engine,
                ..
            } => Conn::Connected {
                pid,
                version_key,
                has_version_file,
                disk: Ok(()),
                engine,
            },
            other => other,
        };
        assert!(h.c.start_refresh(h.at(1_000)));
        h.c.model.conn = match h.c.model.conn.clone() {
            Conn::Connected {
                pid,
                version_key,
                has_version_file,
                engine,
                ..
            } => Conn::Connected {
                pid,
                version_key,
                has_version_file,
                disk: Err("sem cabeçalho".to_string()),
                engine,
            },
            other => other,
        };
        assert!(h.wait(1_000));
        assert_eq!(connected(&h.c), (100, "v1".to_string(), true, true));
        assert!(h.ctl.calls().contains(&Call::OpenEngine));
        assert_eq!(h.row("a").avail, available());
    }

    #[test]
    fn no_access_many_error_states() {
        let mut h = H::new();

        h.ctl.set_find(Find::Many(vec![1, 2]));
        h.tick(0);
        assert_eq!(h.c.model.conn, Conn::Many(vec![1, 2]));
        h.tick(1_000);
        assert_eq!(h.ctl.find_calls(), 1);

        h.ctl.set_find(Find::Error("falha em /proc".to_string()));
        h.tick(2_000);
        assert_eq!(h.c.model.conn, Conn::Error("falha em /proc".to_string()));

        // Sem permissão: mostra a dica e tenta de novo a cada 2 s.
        h.ctl.set_find(Find::Found(100));
        h.ctl
            .push_search(Err(SearchError::NoAccess("ptrace_scope".to_string())));
        h.tick(4_000);
        assert!(h.wait(4_000));
        assert_eq!(h.c.model.conn, Conn::NoAccess("ptrace_scope".to_string()));
        let finds = h.ctl.find_calls();
        h.tick(5_000);
        assert_eq!(h.ctl.find_calls(), finds);
        h.tick(6_000);
        assert_eq!(h.ctl.find_calls(), finds + 1);
        assert!(h.wait(6_000));
        assert!(matches!(h.c.model.conn, Conn::Connected { .. }));

        // Outro erro da busca.
        h.ctl.set_exited(true);
        h.tick(6_100);
        h.ctl.set_exited(false);
        h.ctl
            .push_search(Err(SearchError::Other("sem módulo".to_string())));
        h.tick(9_000);
        assert!(h.wait(9_000));
        assert_eq!(h.c.model.conn, Conn::Error("sem módulo".to_string()));

        // O processo sumiu durante a busca.
        h.ctl.push_search(Err(SearchError::Gone));
        h.tick(11_000);
        assert!(h.wait(11_000));
        assert_eq!(h.c.model.conn, Conn::Waiting);
        assert!(h.c.model.cheats.is_empty());
    }

    #[test]
    fn open_failure_shows_error() {
        let mut h = H::new();
        h.ctl.set_open_error(Some("o processo terminou"));
        h.connect(0);
        assert_eq!(
            h.c.model.conn,
            Conn::Error("o processo terminou".to_string())
        );
        // Tenta de novo em 2 s.
        h.ctl.set_open_error(None);
        h.tick(2_000);
        assert!(h.wait(2_000));
        assert!(matches!(h.c.model.conn, Conn::Connected { .. }));
    }

    #[test]
    fn pid_fixed_gone_is_final() {
        let mut h = H::pid_fixed();
        h.ctl.set_catalog(vec![cheat("a", available())]);
        h.connect(0);
        h.ctl.set_exited(true);
        h.tick(100);
        assert_eq!(h.c.model.conn, Conn::PidGone);
        assert_eq!(h.events().last(), Some(&"12:00 o jogo fechou"));

        // Mesmo que o jogo apareça de novo, o estado é final: nem procura mais.
        h.ctl.set_exited(false);
        h.ctl.set_find(Find::Found(200));
        let finds = h.ctl.find_calls();
        for t in 1..10 {
            h.tick(t * 5_000);
        }
        assert_eq!(h.c.model.conn, Conn::PidGone);
        assert_eq!(h.ctl.find_calls(), finds);
    }

    #[test]
    fn pid_fixed_missing_at_start_is_final() {
        let mut h = H::pid_fixed();
        h.ctl.set_find(Find::PidGone);
        h.tick(0);
        assert_eq!(h.c.model.conn, Conn::PidGone);
        h.ctl.set_find(Find::Found(200));
        h.tick(10_000);
        assert_eq!(h.c.model.conn, Conn::PidGone);
        assert_eq!(h.ctl.find_calls(), 1);
    }

    #[test]
    fn game_close_detected_while_idle() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![cheat("a", available())]);
        h.connect(0);
        h.c.model.cheats[0].on = true;
        h.c.model.cheats[0].applied_k = Some(2);
        h.ctl.set_stuck(true);
        h.tick(100);
        assert!(h.c.model.banners.stuck.is_some());

        h.ctl.set_exited(true);
        h.tick(200);
        assert_eq!(h.c.model.conn, Conn::Waiting);
        assert_eq!(h.events().last(), Some(&"12:00 o jogo fechou"));
        let calls = h.ctl.calls();
        let discard = calls.iter().position(|c| *c == Call::DiscardGone).unwrap();
        let dropped = calls
            .iter()
            .position(|c| *c == Call::SessionDropped)
            .unwrap();
        assert!(discard < dropped, "descarta antes de soltar a sessão");
        assert!(!h.c.model.cheats[0].on);
        assert_eq!(h.c.model.cheats[0].applied_k, None);
        assert_eq!(h.c.model.banners.stuck, None);

        // Procura de novo depois de 2 s.
        h.ctl.set_exited(false);
        h.tick(1_000);
        assert_eq!(h.c.model.conn, Conn::Waiting);
        let finds = h.ctl.find_calls();
        h.tick(2_300);
        assert_eq!(h.ctl.find_calls(), finds + 1);
    }

    #[test]
    fn search_panic_is_reported() {
        let mut h = H::new();
        h.ctl.set_search_panics(true);
        h.ctl.set_find(Find::Found(100));
        h.tick(0);
        assert!(!h.c.search_panicked());
        assert!(h.wait(0));
        assert!(h.c.search_panicked());
        assert_eq!(h.c.model.conn, Conn::Connecting);
    }

    #[test]
    fn search_panic_is_reported_by_tick() {
        let mut h = H::new();
        h.ctl.set_search_panics(true);
        h.ctl.set_find(Find::Found(100));
        h.tick(0);
        let end = Instant::now() + WAIT;
        while !h.c.search_panicked() && Instant::now() < end {
            h.tick(0);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(h.c.search_panicked());
    }

    #[test]
    fn research_other_identity_dropped() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![cheat("a", not_found())]);
        h.connect(0);
        h.ctl.set_merge_result(false);
        h.ctl
            .push_search(Ok(SearchSpec::new(vec![cheat("a", available())])));
        h.tick(5_000);
        assert!(h.wait(5_000));
        assert_eq!(h.c.model.conn, Conn::Waiting);
        assert_eq!(h.row("a").avail, not_found());
        assert!(h.ctl.calls().contains(&Call::SessionDropped));
    }

    #[test]
    fn research_gone_goes_waiting() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![cheat("a", not_found())]);
        h.connect(0);
        // Um processo novo no mesmo PID: a busca com a identidade antiga volta `Gone`.
        h.ctl.set_start_time(2);
        h.tick(5_000);
        assert!(h.wait(5_000));
        assert_eq!(h.c.model.conn, Conn::Waiting);
        assert_eq!(h.events().last(), Some(&"12:00 o jogo fechou"));
    }

    #[test]
    fn stuck_banner_follows_is_stuck() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![cheat("a", available())]);
        h.connect(0);
        assert_eq!(h.c.model.banners.stuck, None);
        h.ctl.set_stuck(true);
        h.tick(100);
        assert_eq!(
            h.c.model.banners.stuck.as_deref(),
            Some("o jogo pode ter ficado parado: kill -CONT 100")
        );
        h.ctl.set_stuck(false);
        h.tick(200);
        assert_eq!(h.c.model.banners.stuck, None);
    }

    #[test]
    fn selection_clamped_when_cheats_change() {
        let mut h = H::new();
        h.c.model.selected = 9;
        h.ctl
            .set_catalog(vec![cheat("a", available()), cheat("b", available())]);
        h.connect(0);
        assert_eq!(h.c.model.selected, 1);

        h.c.model.selected = 7;
        h.c.clamp_selected();
        assert_eq!(h.c.model.selected, 1);

        // Lista vazia: satura em zero.
        let mut empty = H::new();
        empty.c.model.selected = 3;
        empty.connect(0);
        assert_eq!(empty.c.model.selected, 0);
    }

    #[test]
    fn reconnect_keeps_rows_and_shown_value() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![cheat("a", available())]);
        h.connect(0);
        h.c.model.cheats[0].k = 3;
        h.c.model.cheats[0].desired = true;
        h.ctl.set_exited(true);
        h.tick(100);
        h.ctl.set_exited(false);
        h.ctl.set_catalog(vec![cheat("a", not_found())]);
        h.connect(5_000);
        assert_eq!(h.c.model.cheats.len(), 1);
        assert!(h.row("a").desired);
        assert_eq!(h.row("a").avail, not_found(), "conexão nova: estado novo");
    }
}
