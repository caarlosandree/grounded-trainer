//! Controlador da TUI (spec 003, 6.1, 6.2 e 6.10): conecta ao jogo, busca o catálogo numa
//! thread, recupera o diário, percebe o fechamento do jogo e refaz as buscas sozinho.
//!
//! O controlador não lê o relógio: todo instante vem do `now` que o laço passa (a duração da
//! busca já vem pronta no [`Search`]). Isso deixa os testes sem `sleep`.
//!
//! Os campos `session` e `backend` seguem esta ordem de propósito: a sessão (e o motor dela)
//! cai antes da trava do diário, que mora no backend.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};

use gt_core::procinfo::Identity;
use gt_grounded2::fields::{ActionOutcome, Note as FieldNote, PlayerState};
use gt_grounded2::ue::UE_ID;

use crate::backend::{
    Backend, CheatInfo, Find, JobKind, OpOutcome, Recovered, ReflectionOutcome, Search,
    SearchError, Session,
};
use crate::keys::Action;
use crate::model::{Avail, CheatRow, Conn, Model, PlayerLine};
use crate::values::{REFLECTION_UNRECOGNIZED, STUCK_REFUSAL};

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

/// Menor intervalo entre o início de duas trocas de valor (cada troca pausa o jogo).
const SWAP_INTERVAL: Duration = Duration::from_millis(150);

/// Mensagem do topo durante a recuperação do diário.
const RECOVERING: &str = "recuperando o diário…";
/// Mensagem do topo durante a restauração dos restos e o novo SIGCONT.
const RESTORING: &str = "restaurando…";
/// Motivos do catálogo que as buscas automáticas tentam resolver.
const NOT_FOUND: &str = "não encontrado";
const INCONCLUSIVE: &str = "inconclusivo";

/// Menor intervalo entre dois trabalhos da tarefa de reflexão (spec 005, 6.3).
const REFLECTION_MIN_INTERVAL: Duration = Duration::from_secs(10);
/// Por quanto tempo, depois da conexão (ou do `r`), o índice é tentado de novo.
const INDEX_WINDOW: Duration = Duration::from_secs(120);
/// Eventos de personagem que o controlador dá sozinho, nas transições.
const PLAYER_FOUND: &str = "personagem encontrado";
const PLAYER_LOST: &str = "personagem perdido";
/// Eventos da procura do controller pedida pela ação (spec 006, 6.6).
const ACTION_PLAYER_FOUND: &str = "personagem encontrado: aperte de novo para somar";
const ACTION_PLAYER_MISSING: &str = "personagem não encontrado";
/// Recusas da ação que dependem da procura do controller.
const SEARCHING_PLAYER: &str = "procurando o personagem";
const WAIT_SEARCH: &str = "aguarde a procura";
/// Janela depois de uma execução da ação que escreveu, e intervalo mínimo entre duas linhas
/// de registro com o mesmo motivo (spec 006, 6.6).
const ACTION_WINDOW: Duration = Duration::from_secs(1);

type SearchResult<P> = Result<Search<P>, SearchError>;

/// O que o dono quer ligado depois de reconectar (spec 6.2): o valor escolhido e a chave de
/// versão da conexão em que ele ligou o cheat. O mapa `id → Desired` é a fonte da verdade;
/// `CheatRow::desired` é só o espelho para a tela.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Desired {
    /// Índice na grade do valor com que o dono ligou.
    pub k: u64,
    /// Chave de versão do jogo da conexão em que o dono ligou.
    pub version_key: String,
}

/// Avisos do registro de eventos que valem uma vez por cheat em cada conexão.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Note {
    /// Desejado que a religação não conseguiu religar.
    NotReapplied,
    /// Desejado de outra versão do jogo.
    NewVersion,
}

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

/// O trabalho da tarefa de reflexão em andamento.
struct JobInFlight {
    rx: Receiver<ReflectionOutcome>,
    kind: JobKind,
    /// Número da conexão quando o trabalho começou.
    conn_no: u64,
    started: Instant,
}

/// O intervalo da tarefa de reflexão, dado quanto o último trabalho levou.
fn reflection_interval(took: Duration) -> Duration {
    REFLECTION_MIN_INTERVAL.max(took.saturating_mul(2))
}

/// Motivo que a nova busca tenta resolver (e que não derruba um cheat já disponível).
fn retryable(reason: &str) -> bool {
    reason == NOT_FOUND || reason == INCONCLUSIVE || reason == REFLECTION_UNRECOGNIZED
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
        field: c.field,
        action: c.action,
        note: None,
    }
}

/// Se uma linha com `old` deve manter o estado quando a nova busca devolve `new`: um cheat
/// que já estava resolvido (disponível) nesta conexão e que a busca não acha (`não encontrado`,
/// `inconclusivo`) fica como estava (spec 6.2).
fn keeps_state(old: &Avail, new: &Avail) -> bool {
    let lost = matches!(new, Avail::Unavailable(t) if retryable(t));
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
    /// Operações por cheat (`note_op` é chamado em toda operação num cheat).
    op_counts: HashMap<String, u64>,
    /// Cheats que o dono quer ligados (religar depois de reconectar).
    desired: HashMap<String, Desired>,
    /// Há religação a fazer no próximo `tick` (o `tick` recebe o `draw`; o `wait_search`, não).
    reapply_pending: bool,
    /// Quando a última troca de valor começou (limite de ritmo; zerado quando o jogo fecha).
    last_swap: Option<Instant>,
    /// Avisos de religação já dados nesta conexão.
    notes: HashSet<(String, Note)>,
    /// Cheats não verificados já avisados nesta conexão.
    warned_unverified: HashSet<String>,
    /// O dono pediu para sair.
    quit: bool,
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
    /// Trabalho da tarefa de reflexão em andamento (um de cada vez).
    job: Option<JobInFlight>,
    job_panicked: bool,
    /// A sessão tem um índice da reflexão bom.
    index_ok: bool,
    /// Fim do prazo das novas tentativas do índice desta conexão (o `r` reinicia).
    index_until: Option<Instant>,
    /// Quando o próximo índice pode começar.
    next_index: Option<Instant>,
    /// Quando a próxima procura do controller pode começar.
    next_controller: Option<Instant>,
    /// O personagem estava achado no último ciclo de manutenção (`None`: ninguém procurou).
    player_found: Option<bool>,
    /// Quando cada ação escreveu pela última vez (a janela de 1 s; zera ao reconectar).
    action_last: HashMap<String, Instant>,
    /// Quando cada motivo de recusa da ação foi ao registro pela última vez (por ação).
    refusal_last: HashMap<(String, String), Instant>,
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
            desired: HashMap::new(),
            reapply_pending: false,
            last_swap: None,
            notes: HashSet::new(),
            warned_unverified: HashSet::new(),
            quit: false,
            search: None,
            search_panicked: false,
            identity: None,
            last_find: None,
            auto_until: None,
            next_auto: None,
            last_took: Duration::ZERO,
            job: None,
            job_panicked: false,
            index_ok: false,
            index_until: None,
            next_index: None,
            next_controller: None,
            player_found: None,
            action_last: HashMap::new(),
            refusal_last: HashMap::new(),
        }
    }

    /// Recupera o diário antes de qualquer conexão (spec 6.1, passo 6).
    pub fn start(&mut self, draw: &mut dyn FnMut(&Model)) {
        self.recover_diary(draw);
    }

    /// Uma volta do laço: colhe a busca, percebe o fechamento do jogo, procura o jogo, religa os
    /// desejados, dispara as novas buscas automáticas e manda a troca de valor que esperava o
    /// intervalo. Devolve `true` se chamou a sessão numa operação (religar ou trocar o valor): o
    /// laço então descarta as teclas que chegaram durante a chamada, como depois do `handle`.
    ///
    /// A manutenção dos cheats de valor (spec 005, 6.7) também roda aqui, mas não conta como
    /// operação: não muda o valor devolvido.
    pub fn tick(&mut self, now: Instant, draw: &mut dyn FnMut(&Model)) -> bool {
        self.poll_search(now);
        self.poll_job(now);
        self.check_exit(now);
        if self.session.is_none() {
            self.poll_find(now, draw);
            return false;
        }
        self.poll_controller_news();
        self.maybe_spawn_job(now);
        let mut called = self.run_reapply(now, draw);
        self.maybe_auto_search(now);
        // Só uma operação por volta: a troca que sobrou espera a próxima.
        if !called {
            called = self.run_pending_swap(now, draw);
        }
        self.maintain_fields(now);
        called
    }

    /// Das teclas lidas logo depois de uma operação do motor (spec 6.6), as que ainda valem:
    /// sair e as setas de valor (que seguem a regra da 6.7). O resto (espaço, `Enter`, `r` e
    /// movimento) é descartado, para que toques repetidos com o jogo pausado não virem uma
    /// sequência de liga-desliga depois.
    pub fn after_op_keys(&mut self, keys: Vec<Action>) -> Vec<Action> {
        keys.into_iter()
            .filter(|k| matches!(k, Action::Quit | Action::Dec | Action::Inc))
            .collect()
    }

    /// Saída (spec 6.8, passo 1): desenha `restaurando…`, abandona a busca em andamento sem
    /// esperá-la (solta só o `Receiver`; a thread termina sozinha e o resultado dela se perde) e
    /// desfaz todos os patches. Devolve (restaurados, alterados por outro). A sessão continua
    /// aberta: quem chama faz `drop` do controlador depois, e o `Drop` do motor restaura o que
    /// sobrar.
    pub fn shutdown(&mut self, draw: &mut dyn FnMut(&Model)) -> (Vec<String>, Vec<String>) {
        self.search = None;
        // A tarefa de reflexão também é abandonada, e a manutenção para.
        self.job = None;
        self.model.busy = Some(RESTORING.to_string());
        draw(&self.model);
        let out = self.session_call(|s| s.undo_all());
        self.model.busy = None;
        out.unwrap_or_default()
    }

    /// O dono pediu para sair (`q`, `Esc` ou Ctrl+C): o laço encerra.
    pub fn quit_requested(&self) -> bool {
        self.quit
    }

    /// Os cheats desejados (fonte da verdade; `CheatRow::desired` é o espelho).
    pub fn desired(&self) -> &HashMap<String, Desired> {
        &self.desired
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

    /// Como [`Controller::wait_search`], para o trabalho da tarefa de reflexão.
    #[doc(hidden)]
    pub fn wait_job(&mut self, now: Instant, timeout: Duration) -> bool {
        let Some(job) = self.job.take() else {
            return false;
        };
        match job.rx.recv_timeout(timeout) {
            Ok(out) => {
                self.finish_job(job, out, now);
                true
            }
            Err(RecvTimeoutError::Timeout) => {
                self.job = Some(job);
                false
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.job_panicked = true;
                true
            }
        }
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

    /// Chama a sessão e, depois, acerta o aviso de jogo parado e os restos (spec 6.8): vale para
    /// toda chamada. `None` sem sessão.
    fn session_call<R>(&mut self, f: impl FnOnce(&mut B::Session) -> R) -> Option<R> {
        let session = self.session.as_mut()?;
        let out = f(session);
        self.sync_stuck();
        self.sync_pending();
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

    /// O motor é a fonte dos restos: cada linha pendente vira `erro` e as que ele já resolveu
    /// voltam ao normal. O aviso fixo acompanha.
    fn sync_pending(&mut self) {
        let Some(pending) = self.session.as_ref().map(|s| s.pending_ids()) else {
            return;
        };
        let new: Vec<String> = pending
            .iter()
            .filter(|id| !self.model.cheats.iter().any(|r| &r.id == *id && r.residue))
            .cloned()
            .collect();
        for row in &mut self.model.cheats {
            if row.residue && !pending.contains(&row.id) {
                row.residue = false;
            }
        }
        self.mark_residue(&new);
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
        self.notes.clear();
        self.warned_unverified.clear();
        self.action_last.clear();
        self.refusal_last.clear();
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
        self.sync_pending();
        self.auto_until = Some(now + AUTO_WINDOW);
        self.schedule_auto(inflight.started, took, now);
        self.index_ok = false;
        self.index_until = Some(now + INDEX_WINDOW);
        self.next_index = Some(now);
        self.next_controller = None;
        self.player_found = None;
        self.sync_field_avail();
        self.event(format!(
            "conectado ao jogo (PID {pid}, versão {version_key})"
        ));
        self.schedule_reapply();
    }

    /// Junta o catálogo da busca de conexão na lista: as linhas que já existiam (de uma conexão
    /// anterior) guardam o valor mostrado; a disponibilidade é a da busca nova. Se o bloco
    /// `value` do cheat mudou (jogo atualizado), o valor mostrado, que é também o guardado, vai
    /// ao ponto mais próximo da grade nova (spec 6.2) e o registro de eventos diz o valor usado.
    fn merge_connect(&mut self, cheats: Vec<CheatInfo>) {
        for c in cheats {
            let Some(row) = self.model.cheats.iter_mut().find(|r| r.id == c.id) else {
                self.model.cheats.push(row_of(c));
                continue;
            };
            let old_spec = row.value.take();
            let (k, regridded) = match (&old_spec, &c.value) {
                (Some(old), Some(new)) if old != new => {
                    let k = new.nearest_k(old.value(row.k));
                    (k, Some(new.format(k)))
                }
                (Some(_), Some(new)) => (row.k.min(new.steps()), None),
                (None, Some(new)) => (new.default_k.min(new.steps()), None),
                (_, None) => (0, None),
            };
            row.name = c.name;
            row.category = c.category;
            row.avail = c.avail;
            row.value = c.value;
            row.k = k;
            let (id, name) = (row.id.clone(), row.name.clone());
            // O desejado guarda sempre o valor mostrado.
            if let Some(d) = self.desired.get_mut(&id) {
                d.k = k;
            }
            if let Some(used) = regridded {
                self.event(format!(
                    "{name}: a grade do valor mudou na versão nova; usando {used}"
                ));
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
        let mut search = match res {
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
        let has_version_file = search.has_version_file;
        // O resultado só vale para os cheats que continuam desligados e sem operação desde o
        // início da busca (spec 6.2). A sessão e o modelo usam o mesmo conjunto.
        let accepted: Vec<String> = search
            .cheats
            .iter()
            .filter(|c| search.ids.as_ref().is_none_or(|ids| ids.contains(&c.id)))
            .filter(|c| self.refresh_applies(&c.id, inflight))
            .map(|c| c.id.clone())
            .collect();
        search.ids = Some(accepted.clone());
        let cheats: Vec<CheatInfo> = search
            .cheats
            .iter()
            .filter(|c| accepted.contains(&c.id))
            .cloned()
            .collect();
        if self.session_call(|s| s.merge_search(search)) != Some(true) {
            // Outro processo (ou nenhuma sessão): a conexão acabou.
            return self.on_game_closed(now);
        }
        for c in cheats {
            match self.model.cheats.iter_mut().find(|r| r.id == c.id) {
                // Os cheats de valor seguem a sessão (globais e índice), logo abaixo.
                Some(row) if row.field => {}
                Some(row) => {
                    if keeps_state(&row.avail, &c.avail) {
                        continue;
                    }
                    row.avail = c.avail;
                }
                None => self.model.cheats.push(row_of(c)),
            }
        }
        self.sync_field_avail();
        // Globais que ficaram prontas: o índice pode tentar já.
        if !self.index_ok && self.job.is_none() {
            self.next_index = Some(now);
        }
        self.clamp_selected();
        let disk_was_unreadable = matches!(
            &self.model.conn,
            Conn::Connected { disk, .. } if disk.is_err()
        );
        let disk_now_ok = disk.is_ok();
        if let Conn::Connected {
            version_key: vk,
            has_version_file: hvf,
            disk: d,
            ..
        } = &mut self.model.conn
        {
            *vk = version_key;
            *hvf = has_version_file;
            *d = disk;
        }
        if disk_was_unreadable && disk_now_ok {
            self.open_engine_if_possible();
        }
        self.schedule_auto(inflight.started, took, now);
        self.schedule_reapply();
    }

    /// Se o resultado da nova busca vale para `id`: a linha (se houver) continua desligada e o
    /// contador de operações é o mesmo de quando a busca começou.
    fn refresh_applies(&self, id: &str, inflight: &InFlight<B::Payload>) -> bool {
        let ops_now = self.op_counts.get(id).copied().unwrap_or(0);
        let ops_then = inflight.ops.get(id).copied().unwrap_or(0);
        let on = self.model.cheats.iter().any(|r| r.id == id && r.on);
        !on && ops_now == ops_then
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
    /// ligado (os desejados ficam) e a conexão volta a esperar.
    fn on_game_closed(&mut self, now: Instant) {
        self.session_call(|s| s.discard_gone());
        self.session = None;
        self.conn_no += 1;
        self.identity = None;
        self.auto_until = None;
        self.next_auto = None;
        self.reapply_pending = false;
        self.last_swap = None;
        self.notes.clear();
        self.warned_unverified.clear();
        self.last_find = Some(now);
        // O trabalho da tarefa de reflexão é abandonado: nenhum endereço do processo antigo vale.
        self.job = None;
        self.index_ok = false;
        self.index_until = None;
        self.next_index = None;
        self.next_controller = None;
        self.player_found = None;
        self.action_last.clear();
        self.refusal_last.clear();
        self.model.player = PlayerLine::Idle;
        self.model.busy = None;
        self.model.banners.stuck = None;
        self.model.banners.residue = None;
        for row in &mut self.model.cheats {
            row.on = false;
            row.applied_k = None;
            row.residue = false;
            row.note = None;
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

    /// Há cheat que as buscas automáticas ainda tentam resolver: um mostrado como
    /// `não encontrado` ou `inconclusivo`, ou um desejado indisponível (spec 6.2).
    fn wants_auto_search(&self) -> bool {
        let missing = self
            .model
            .visible()
            .iter()
            .any(|r| !r.on && matches!(&r.avail, Avail::Unavailable(t) if retryable(t)));
        missing
            || self.model.cheats.iter().any(|r| {
                !r.on
                    && self.desired.contains_key(&r.id)
                    && matches!(r.avail, Avail::Unavailable(_))
            })
    }

    fn maybe_auto_search(&mut self, now: Instant) {
        if self.search_live() {
            return;
        }
        let (Some(until), Some(next)) = (self.auto_until, self.next_auto) else {
            return;
        };
        // `disco ilegível` não muda sozinho: só o `r` refaz a busca (spec 6.2).
        if matches!(&self.model.conn, Conn::Connected { disk, .. } if disk.is_err()) {
            return;
        }
        if now >= until || now < next || !self.wants_auto_search() {
            return;
        }
        self.start_refresh(now);
    }

    /// Inicia a busca dos cheats desligados (só com a conexão aberta).
    fn start_refresh(&mut self, now: Instant) -> bool {
        let Conn::Connected { pid, .. } = &self.model.conn else {
            return false;
        };
        let pid = *pid;
        let mut ids: Vec<String> = self
            .model
            .cheats
            .iter()
            .filter(|r| !r.on)
            .map(|r| r.id.clone())
            .collect();
        // As globais do Unreal entram na nova busca enquanto não há índice bom (spec 005, 6.7).
        if !self.index_ok && self.model.cheats.iter().any(|r| r.field) {
            ids.push(UE_ID.to_string());
        }
        let expected = self.identity.clone();
        match self.spawn_search(SearchKind::Refresh, pid, Some(ids), expected, now) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(erro = %e, "não foi possível iniciar a nova busca");
                // Sem isso, a próxima volta tentaria (e avisaria) de novo.
                self.next_auto = Some(now + auto_interval(self.last_took));
                false
            }
        }
    }

    // ---- tarefa de reflexão e manutenção dos cheats de valor (spec 005, 6.3 e 6.7) ----

    /// Há algum cheat de valor ligado ou desejado.
    fn fields_wanted(&self) -> bool {
        self.model
            .cheats
            .iter()
            .any(|r| r.field && (r.on || self.desired.contains_key(&r.id)))
    }

    /// Acerta a disponibilidade das linhas de valor com a da sessão.
    fn sync_field_avail(&mut self) {
        let Some(avail) = self.session.as_ref().map(|s| s.field_avail()) else {
            return;
        };
        for (id, a) in avail {
            if let Some(row) = self.model.cheats.iter_mut().find(|r| r.field && r.id == id) {
                row.avail = a;
            }
        }
    }

    fn poll_job(&mut self, now: Instant) {
        let Some(job) = self.job.take() else {
            return;
        };
        match job.rx.try_recv() {
            Ok(out) => self.finish_job(job, out, now),
            Err(TryRecvError::Empty) => self.job = Some(job),
            Err(TryRecvError::Disconnected) => {
                // Um pânico no trabalho não derruba a TUI: a próxima tentativa segue o intervalo.
                tracing::error!("o trabalho da tarefa de reflexão terminou sem resultado");
                self.job_panicked = true;
                let next = Some(now + REFLECTION_MIN_INTERVAL);
                match job.kind {
                    JobKind::Index => self.next_index = next,
                    JobKind::Controller => self.next_controller = next,
                }
            }
        }
    }

    /// Junta o resultado de um trabalho. O de outra conexão é descartado (spec 005, Foco 2).
    fn finish_job(&mut self, job: JobInFlight, out: ReflectionOutcome, now: Instant) {
        if job.conn_no != self.conn_no || self.session.is_none() {
            tracing::debug!("resultado da tarefa de reflexão descartado: a conexão mudou");
            return;
        }
        let took = out.took;
        let next = (job.started + reflection_interval(took)).max(now);
        let Some(news) = self.session_call(|s| s.merge_reflection(out)) else {
            return;
        };
        match job.kind {
            JobKind::Index => {
                let had = self.index_ok;
                self.index_ok = news.index_ok;
                self.next_index = news.retry.then_some(next);
                self.sync_field_avail();
                if self.index_ok && !had {
                    // Os cheats de valor desejados esperavam o índice para religar.
                    self.schedule_reapply();
                }
            }
            JobKind::Controller => self.next_controller = Some(next),
        }
    }

    /// Dispara o próximo trabalho, um de cada vez: o índice enquanto ele não está bom e dentro
    /// do prazo; a procura do controller só com cheat de valor desejado (ou ligado).
    fn maybe_spawn_job(&mut self, now: Instant) {
        if self.job.is_some() || !self.model.cheats.iter().any(|r| r.field) {
            return;
        }
        let index_due = !self.index_ok
            && self.next_index.is_some_and(|t| now >= t)
            && self.index_until.is_some_and(|u| now < u);
        let action_wants = self
            .session
            .as_ref()
            .is_some_and(|s| s.action_wants_controller());
        let controller_due = self.index_ok
            && (self.fields_wanted() || action_wants)
            && self.next_controller.is_none_or(|t| now >= t)
            && self
                .session
                .as_ref()
                .is_some_and(|s| s.needs_controller(now));
        let kind = if index_due {
            JobKind::Index
        } else if controller_due {
            JobKind::Controller
        } else {
            return;
        };
        let Some(job) = self.session.as_ref().and_then(|s| s.reflection_job(kind)) else {
            // Nada a fazer agora (globais indisponíveis): o próximo resultado de busca reabre.
            match kind {
                JobKind::Index => self.next_index = None,
                JobKind::Controller => self.next_controller = Some(now + REFLECTION_MIN_INTERVAL),
            }
            return;
        };
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("gt-reflexao".to_string())
            .spawn(move || {
                let _ = tx.send((job.run)());
            });
        match spawned {
            Ok(_) => {
                self.job = Some(JobInFlight {
                    rx,
                    kind,
                    conn_no: self.conn_no,
                    started: now,
                });
            }
            Err(e) => {
                tracing::warn!(erro = %e, "não foi possível iniciar a tarefa de reflexão");
                let next = Some(now + REFLECTION_MIN_INTERVAL);
                match kind {
                    JobKind::Index => self.next_index = next,
                    JobKind::Controller => self.next_controller = next,
                }
            }
        }
    }

    /// Colhe o resultado da procura do controller pedida pela ação (spec 006, 6.6): o evento e a
    /// linha `personagem:`, que passa a seguir o controller guardado mesmo sem cheat ligado.
    fn poll_controller_news(&mut self) {
        let Some(found) = self.session_call(|s| s.take_controller_news()).flatten() else {
            return;
        };
        self.player_found = Some(found);
        self.model.player = if found {
            PlayerLine::Found
        } else {
            PlayerLine::NotFound
        };
        self.event(if found {
            ACTION_PLAYER_FOUND
        } else {
            ACTION_PLAYER_MISSING
        });
    }

    /// Manutenção de ~10 Hz dos cheats de valor ligados. Não é operação (spec 005, 6.7): não
    /// mexe nas teclas nem nas buscas. Fica suspensa com o diário não resolvido, sem motor ou
    /// com o jogo possivelmente parado por nós.
    fn maintain_fields(&mut self, now: Instant) {
        if !self.fields_wanted() {
            // A linha `personagem:` segue o controller guardado: nada a zerar sem cheat ligado.
            for row in self.model.cheats.iter_mut().filter(|r| r.field) {
                row.note = None;
            }
            return;
        }
        let engine = matches!(self.model.conn, Conn::Connected { engine: true, .. });
        let stuck = self.session.as_ref().is_some_and(|s| s.is_stuck());
        if !engine || stuck || !self.diary_resolved() {
            return;
        }
        let on: Vec<String> = self
            .model
            .cheats
            .iter()
            .filter(|r| r.field && r.on)
            .map(|r| r.id.clone())
            .collect();
        let Some(report) = self.session_call(|s| s.field_tick(&on, now)) else {
            return;
        };
        for row in self.model.cheats.iter_mut().filter(|r| r.field) {
            row.note = if row.on {
                report.notes.get(&row.id).copied()
            } else {
                None
            };
        }
        // Com nada ligado, a sessão não procura o personagem: a linha fica como estava.
        if on.is_empty() {
            return;
        }
        let found = report.player == PlayerState::Found;
        match (self.player_found, found) {
            (Some(false) | None, true) => self.event(PLAYER_FOUND),
            (Some(true), false) => self.event(PLAYER_LOST),
            _ => {}
        }
        self.player_found = Some(found);
        self.model.player = if found {
            PlayerLine::Found
        } else {
            PlayerLine::NotFound
        };
        for e in report.events {
            if e != PLAYER_FOUND && e != PLAYER_LOST {
                self.event(e);
            }
        }
    }
}

// ---- teclas, operações e religação ----

/// Formata o inteiro `v` do jogo como a grade formata os valores (unidade incluída).
fn fmt_int(grid: &gt_core::value::ValueGrid, v: i32) -> String {
    let point = gt_core::value::ValueGrid {
        min: f64::from(v),
        max: f64::from(v),
        ..grid.clone()
    };
    point.format(0)
}

/// Milissegundos da pausa, para o registro de eventos.
fn ms(pause: Duration) -> u128 {
    pause.as_millis()
}

impl<B: Backend> Controller<B> {
    /// Atende uma ação do dono. Devolve `true` se chamou a sessão (o laço então descarta as
    /// teclas que chegaram durante a chamada). Com o terminal pequeno só `Quit` e `Retry` valem.
    pub fn handle(&mut self, action: Action, now: Instant, draw: &mut dyn FnMut(&Model)) -> bool {
        if action == Action::Quit {
            self.quit = true;
            return false;
        }
        if self.model.too_small && action != Action::Retry {
            return false;
        }
        match action {
            Action::Up => {
                self.model.move_selection(-1);
                false
            }
            Action::Down => {
                self.model.move_selection(1);
                false
            }
            Action::Toggle => self.toggle(now, draw),
            Action::Dec => self.change_value(-1, now, draw),
            Action::Inc => self.change_value(1, now, draw),
            Action::Retry => self.retry(now, draw),
            Action::Quit => false,
        }
    }

    /// `r` (spec 6.6), só o primeiro degrau que se aplica: SIGCONT que falhou; restos de uma
    /// falha; diário não resolvido (e, resolvido com o jogo conectado, abrir o motor e religar);
    /// conectado, nova busca dos desligados (e reinício do prazo das automáticas); sem conexão,
    /// procurar o jogo na hora. Em `conectando`, `o processo terminou` ou durante uma operação,
    /// não faz nada. Devolve `true` se o `r` chamou a sessão ou a recuperação.
    fn retry(&mut self, now: Instant, draw: &mut dyn FnMut(&Model)) -> bool {
        if self.model.busy.is_some() {
            return false;
        }
        if matches!(self.model.conn, Conn::Connecting | Conn::PidGone) {
            return false;
        }
        if self.session.as_ref().is_some_and(|s| s.is_stuck()) {
            return self.resume(now, draw);
        }
        if self
            .session
            .as_ref()
            .is_some_and(|s| !s.pending_ids().is_empty())
        {
            return self.restore_residue(now, draw);
        }
        if !self.diary_resolved() {
            return self.retry_recovery(draw);
        }
        if matches!(self.model.conn, Conn::Connected { .. }) {
            self.auto_until = Some(now + AUTO_WINDOW);
            // O `r` reinicia o prazo do índice (spec 005, 6.3).
            self.index_until = Some(now + INDEX_WINDOW);
            if !self.index_ok {
                self.next_index = Some(now);
            }
            if !self.search_live() {
                self.start_refresh(now);
            }
        } else {
            self.last_find = None;
            self.poll_find(now, draw);
        }
        false
    }

    /// Tenta de novo o SIGCONT que falhou (o aviso some quando dá certo).
    fn resume(&mut self, now: Instant, draw: &mut dyn FnMut(&Model)) -> bool {
        let Some(outcome) = self.run_op(RESTORING.to_string(), draw, |s| s.retry_resume()) else {
            return false;
        };
        match outcome {
            OpOutcome::Done(_) => self.event("o jogo foi retomado (SIGCONT enviado)"),
            OpOutcome::Gone => self.on_game_closed(now),
            OpOutcome::Refused(reason) | OpOutcome::Residue(reason) => {
                self.event(format!("não foi possível retomar o jogo: {reason}"));
            }
            OpOutcome::Foreign => self.event("não foi possível retomar o jogo"),
        }
        true
    }

    /// Pede ao motor para restaurar os restos (`r` ou espaço num cheat em `erro`). Resolvidos,
    /// o cheat volta a `[OFF]` (quem acerta as linhas é o `sync_pending`).
    fn restore_residue(&mut self, now: Instant, draw: &mut dyn FnMut(&Model)) -> bool {
        let residue: Vec<String> = self
            .model
            .cheats
            .iter()
            .filter(|r| r.residue)
            .map(|r| r.id.clone())
            .collect();
        let Some(outcome) = self.run_op(RESTORING.to_string(), draw, |s| s.restore_pending())
        else {
            return false;
        };
        match outcome {
            OpOutcome::Done(pause) => {
                self.event(format!("restos restaurados (pausa {} ms)", ms(pause)));
            }
            OpOutcome::Gone => self.on_game_closed(now),
            OpOutcome::Refused(reason) => {
                self.event(format!("não foi possível restaurar os restos: {reason}"));
            }
            OpOutcome::Residue(cause) => {
                self.event(format!("restos continuam no jogo ({cause})"));
            }
            // Bytes de outro: o motor não escreveu nada e as entradas saíram do registro.
            OpOutcome::Foreign => self.mark_residue_foreign(&residue),
        }
        true
    }

    /// Tenta a recuperação do diário de novo. Resolvida com o jogo conectado, refaz a conexão a
    /// partir da abertura do motor e religa os desejados no próximo `tick`.
    fn retry_recovery(&mut self, draw: &mut dyn FnMut(&Model)) -> bool {
        self.recover_diary(draw);
        if self.diary_resolved() && matches!(self.model.conn, Conn::Connected { .. }) {
            self.open_engine_if_possible();
            self.schedule_reapply();
        }
        true
    }

    // ---- desejados ----

    /// Põe (ou tira, com `None`) o cheat dos desejados e atualiza o espelho da linha.
    fn set_desired(&mut self, id: &str, desired: Option<Desired>) {
        match desired {
            Some(d) => {
                self.desired.insert(id.to_string(), d);
            }
            None => {
                self.desired.remove(id);
            }
        }
        let wanted = self.desired.contains_key(id);
        if let Some(row) = self.model.cheats.iter_mut().find(|r| r.id == id) {
            row.desired = wanted;
        }
    }

    /// Pede a religação no próximo `tick`, se houver desejados.
    fn schedule_reapply(&mut self) {
        if !self.desired.is_empty() {
            self.reapply_pending = true;
        }
    }

    /// Chave de versão da conexão atual.
    fn current_version(&self) -> Option<String> {
        match &self.model.conn {
            Conn::Connected { version_key, .. } => Some(version_key.clone()),
            _ => None,
        }
    }

    /// Por que não se pode escrever no jogo agora, se não se pode.
    fn write_block(&self) -> Option<&'static str> {
        if !self.diary_resolved() {
            return Some("diário com entradas não restauradas; aperte r");
        }
        match &self.model.conn {
            Conn::Connected { engine: true, .. } => None,
            _ => Some("o motor não está aberto"),
        }
    }

    // ---- ligar e desligar pela tecla ----

    /// Espaço/Enter no cheat selecionado.
    fn toggle(&mut self, now: Instant, draw: &mut dyn FnMut(&Model)) -> bool {
        let Some(id) = self.model.selected_id().map(str::to_string) else {
            return false;
        };
        let Some(row) = self.model.cheats.iter().find(|r| r.id == id).cloned() else {
            return false;
        };
        match &self.model.conn {
            Conn::Connecting => {
                self.event("aguarde: conectando ao jogo");
                return false;
            }
            Conn::Connected { .. } => {}
            _ => {
                self.toggle_unavailable(&row, "jogo não conectado");
                return false;
            }
        }
        if row.action {
            return self.action_key(&row, now);
        }
        if row.residue {
            return self.restore_residue(now, draw);
        }
        if row.on {
            return self.turn_off(&row, now, draw);
        }
        match &row.avail {
            Avail::Unavailable(reason) => {
                self.toggle_unavailable(&row, reason);
                false
            }
            Avail::Checking => {
                self.event(format!(
                    "{}: verificando a reflexão do jogo; tente de novo em instantes",
                    row.name
                ));
                false
            }
            Avail::Available { .. } => self.turn_on(&row, now, draw),
        }
    }

    /// Espaço num cheat indisponível: escreve o motivo e, com a marca de religar pendente, tira
    /// o cheat dos desejados.
    fn toggle_unavailable(&mut self, row: &CheatRow, reason: &str) {
        self.event(format!("{}: indisponível ({reason})", row.name));
        if self.desired.contains_key(&row.id) {
            self.set_desired(&row.id, None);
            self.event(format!("{}: não será mais religado", row.name));
        }
    }

    // ---- ação de somar (spec 006, 6.6) ----

    /// Escreve a linha de registro da ação, no máximo uma por segundo para o mesmo motivo
    /// (`key`), para uma tecla segurada não encher o registro.
    fn throttled_event(&mut self, id: &str, key: &str, text: String, now: Instant) {
        let slot = (id.to_string(), key.to_string());
        let recent = self
            .refusal_last
            .get(&slot)
            .is_some_and(|t| now.saturating_duration_since(*t) < ACTION_WINDOW);
        if !recent {
            self.refusal_last.insert(slot, now);
            self.event(text);
        }
    }

    /// Espaço/Enter numa linha `[ação]` com a conexão aberta. Devolve `true` se a execução
    /// chegou a ler o jogo (só então é operação e descarta as teclas que chegaram durante ela).
    fn action_key(&mut self, row: &CheatRow, now: Instant) -> bool {
        match &row.avail {
            Avail::Unavailable(reason) => {
                let text = format!("{}: indisponível ({reason})", row.name);
                self.throttled_event(&row.id, reason, text, now);
                false
            }
            Avail::Checking => {
                let text = format!(
                    "{}: verificando a reflexão do jogo; tente de novo em instantes",
                    row.name
                );
                self.throttled_event(&row.id, crate::model::CHECKING, text, now);
                false
            }
            Avail::Available { .. } => self.run_action(row, now),
        }
    }

    /// Executa a ação (passos 1 a 6 da spec 6.6; a sessão faz os passos 2 a 5).
    fn run_action(&mut self, row: &CheatRow, now: Instant) -> bool {
        let (id, name) = (row.id.as_str(), row.name.as_str());
        // Uma execução por segundo: a tecla segurada é descartada.
        let in_window = self
            .action_last
            .get(id)
            .is_some_and(|t| now.saturating_duration_since(*t) < ACTION_WINDOW);
        if in_window {
            self.throttled_event(id, "aguarde", format!("{name}: aguarde"), now);
            return false;
        }
        // Passo 1: a mesma mensagem que ligar um cheat receberia.
        let stuck = self.session.as_ref().is_some_and(|s| s.is_stuck());
        let block = self
            .write_block()
            .or_else(|| stuck.then_some(STUCK_REFUSAL));
        if let Some(why) = block {
            let text = format!("{name}: não foi possível ligar ({why})");
            self.throttled_event(id, why, text, now);
            return false;
        }
        let k = Self::shown_k(row);
        let target = id.to_string();
        let Some(outcome) = self.session_call(|s| s.field_action(&target, k)) else {
            return false;
        };
        let spec = row.value.clone();
        let show = |v: i32| {
            spec.as_ref()
                .map_or_else(|| v.to_string(), |g| fmt_int(g, v))
        };
        match outcome {
            ActionOutcome::Done { before, after } => {
                self.action_wrote(id, now);
                self.event(format!("{name}: {} → {}", show(before), show(after)));
                true
            }
            ActionOutcome::Raced { read } => {
                self.action_wrote(id, now);
                self.event(format!(
                    "{name}: o jogo mudou o valor durante a escrita (lido {})",
                    show(read)
                ));
                true
            }
            ActionOutcome::Refused(reason) => {
                self.throttled_event(id, &reason, format!("{name}: {reason}"), now);
                false
            }
            // Leu o jogo: é operação (descarta teclas), mas não escreveu e não abre a janela.
            ActionOutcome::RefusedAfterRead(reason) => {
                self.note_op(id);
                self.throttled_event(id, &reason, format!("{name}: {reason}"), now);
                true
            }
            ActionOutcome::NeedsController => {
                // A procura segue o intervalo da tarefa de reflexão: dispara agora se já pode.
                self.maybe_spawn_job(now);
                let searching = matches!(&self.job, Some(j) if j.kind == JobKind::Controller);
                let why = if searching {
                    SEARCHING_PLAYER
                } else {
                    WAIT_SEARCH
                };
                // O controller guardado caiu: a linha `personagem:` deixa de dizer `encontrado`.
                if self.player_found.is_some() {
                    self.player_found = Some(false);
                    self.model.player = PlayerLine::NotFound;
                }
                self.throttled_event(id, why, format!("{name}: {why}"), now);
                false
            }
        }
    }

    /// Uma execução que escreveu: abre a janela e conta como operação.
    fn action_wrote(&mut self, id: &str, now: Instant) {
        self.action_last.insert(id.to_string(), now);
        self.note_op(id);
    }

    /// O `k` com que um cheat é ligado: o valor mostrado, dentro da grade.
    fn shown_k(row: &CheatRow) -> u64 {
        row.value.as_ref().map_or(0, |v| row.k.min(v.steps()))
    }

    /// Mostra `busy`, chama a sessão e limpa o `busy` (a mensagem fica na tela durante toda a
    /// operação). `None` sem sessão.
    fn run_op(
        &mut self,
        busy: String,
        draw: &mut dyn FnMut(&Model),
        f: impl FnOnce(&mut B::Session) -> OpOutcome,
    ) -> Option<OpOutcome> {
        self.session.as_ref()?;
        self.model.busy = Some(busy);
        draw(&self.model);
        let out = self.session_call(f);
        self.model.busy = None;
        out
    }

    fn turn_on(&mut self, row: &CheatRow, now: Instant, draw: &mut dyn FnMut(&Model)) -> bool {
        if let Some(why) = self.write_block() {
            self.event(format!("{}: não foi possível ligar ({why})", row.name));
            return false;
        }
        let Some(version_key) = self.current_version() else {
            return false;
        };
        let k = Self::shown_k(row);
        self.note_op(&row.id);
        let busy = format!("ligando {}…", row.name);
        let id = row.id.clone();
        let field = row.field;
        let Some(outcome) = self.run_op(busy, draw, |s| {
            if field {
                s.field_on(&id, k)
            } else {
                s.apply(&[(id.clone(), k)])
            }
        }) else {
            return false;
        };
        match outcome {
            OpOutcome::Done(pause) => {
                self.mark_on(&row.id, k);
                self.set_desired(&row.id, Some(Desired { k, version_key }));
                if field {
                    self.event(format!("{} ligado", row.name));
                } else {
                    self.event(format!("{} ligado (pausa {} ms)", row.name, ms(pause)));
                }
                self.warn_unverified(&row.id);
            }
            OpOutcome::Refused(reason) => self.event(format!("{}: {reason}", row.name)),
            OpOutcome::Foreign => self.mark_foreign(&row.id),
            OpOutcome::Residue(cause) => {
                self.mark_residue(std::slice::from_ref(&row.id));
                self.event(format!(
                    "{}: falhou deixando restos no jogo ({cause})",
                    row.name
                ));
            }
            OpOutcome::Gone => self.on_game_closed(now),
        }
        true
    }

    fn turn_off(&mut self, row: &CheatRow, now: Instant, draw: &mut dyn FnMut(&Model)) -> bool {
        self.note_op(&row.id);
        let busy = format!("desligando {}…", row.name);
        let id = row.id.clone();
        let field = row.field;
        let Some(outcome) = self.run_op(busy, draw, |s| {
            if field { s.field_off(&id) } else { s.undo(&id) }
        }) else {
            return false;
        };
        match outcome {
            OpOutcome::Done(pause) => {
                self.mark_off(&row.id);
                self.set_desired(&row.id, None);
                if field {
                    self.event(format!("{} desligado", row.name));
                } else {
                    self.event(format!("{} desligado (pausa {} ms)", row.name, ms(pause)));
                }
            }
            // Recusa limpa: continua ligado e desejado; o dono pode tentar de novo.
            OpOutcome::Refused(reason) => self.event(format!("{}: {reason}", row.name)),
            OpOutcome::Foreign => self.mark_foreign(&row.id),
            OpOutcome::Residue(cause) => {
                self.mark_residue(std::slice::from_ref(&row.id));
                self.event(format!(
                    "{}: falhou deixando restos no jogo ({cause})",
                    row.name
                ));
            }
            OpOutcome::Gone => {
                // Desligar interrompido: a intenção do dono era desligar.
                self.set_desired(&row.id, None);
                self.on_game_closed(now);
            }
        }
        true
    }

    // ---- troca de valor (spec 6.2 e 6.7) ----

    /// Muda o valor mostrado do `id` e o guardado no desejado (o valor guardado é sempre o
    /// mostrado).
    fn set_shown_k(&mut self, id: &str, k: u64) {
        if let Some(row) = self.model.cheats.iter_mut().find(|r| r.id == id) {
            row.k = k;
        }
        if let Some(d) = self.desired.get_mut(id) {
            d.k = k;
        }
    }

    /// `←`/`→` no cheat selecionado: muda o valor mostrado em um passo, dentro da grade, também
    /// com o cheat desligado, indisponível ou com a marca de religar pendente. Com o cheat
    /// ligado, a troca vai para o jogo se o intervalo de 150 ms desde o início da anterior já
    /// passou; senão o valor fica como alvo e o `tick` o manda depois. Devolve `true` se chamou
    /// a sessão.
    fn change_value(&mut self, delta: i64, now: Instant, draw: &mut dyn FnMut(&Model)) -> bool {
        let Some(id) = self.model.selected_id().map(str::to_string) else {
            return false;
        };
        let Some(row) = self.model.cheats.iter().find(|r| r.id == id) else {
            return false;
        };
        let Some(spec) = &row.value else {
            return false;
        };
        let last = i64::try_from(spec.steps()).unwrap_or(i64::MAX);
        let current = i64::try_from(row.k).unwrap_or(i64::MAX);
        let k = u64::try_from(current.saturating_add(delta).clamp(0, last)).unwrap_or(0);
        if k == row.k {
            return false;
        }
        // Com `o jogo não deixa`, as setas só mudam o valor mostrado (spec 006, 6.4).
        let in_game = row.on && row.applied_k.is_some() && row.note != Some(FieldNote::GameRefuses);
        self.set_shown_k(&id, k);
        in_game && self.swap_due(now) && self.do_swap(&id, now, draw)
    }

    /// Já passaram 150 ms do início da última troca.
    fn swap_due(&self, now: Instant) -> bool {
        self.last_swap
            .is_none_or(|t| now.saturating_duration_since(t) >= SWAP_INTERVAL)
    }

    /// Manda ao jogo a troca que esperava o intervalo: o primeiro cheat ligado cujo valor
    /// mostrado difere do que está no jogo. Devolve `true` se chamou a sessão.
    fn run_pending_swap(&mut self, now: Instant, draw: &mut dyn FnMut(&Model)) -> bool {
        if !self.swap_due(now) {
            return false;
        }
        let pending = self
            .model
            .cheats
            .iter()
            .find(|r| {
                r.on && r.note != Some(FieldNote::GameRefuses)
                    && r.applied_k.is_some_and(|a| a != r.k)
            })
            .map(|r| r.id.clone());
        match pending {
            Some(id) => self.do_swap(&id, now, draw),
            None => false,
        }
    }

    /// Troca o valor do `id` no jogo para o valor mostrado (spec 6.7). Devolve `true` se chamou
    /// a sessão.
    fn do_swap(&mut self, id: &str, now: Instant, draw: &mut dyn FnMut(&Model)) -> bool {
        let Some(row) = self.model.cheats.iter().find(|r| r.id == id).cloned() else {
            return false;
        };
        let (Some(spec), Some(before)) = (row.value.clone(), row.applied_k) else {
            return false;
        };
        let k = Self::shown_k(&row);
        if k == before {
            return false;
        }
        self.last_swap = Some(now);
        self.note_op(id);
        let busy = format!("trocando o valor de {}…", row.name);
        let target = id.to_string();
        let Some(outcome) = self.run_op(busy, draw, |s| s.set_value(&target, k)) else {
            return false;
        };
        match outcome {
            OpOutcome::Done(_) => {
                if let Some(r) = self.model.cheats.iter_mut().find(|r| r.id == id) {
                    r.applied_k = Some(k);
                }
                let hhmm = (self.clock)();
                self.model.merge_value_event(
                    &hhmm,
                    id,
                    &row.name,
                    &spec.format(before),
                    &spec.format(k),
                );
            }
            // Cheat de valor antes do índice da reflexão (`verificando…`): o valor mostrado é
            // guardado e vale ao ligar ou religar; sem linha no registro nem volta do valor
            // (spec 006, 6.5). `applied_k` acompanha para a troca não se repetir a cada volta.
            OpOutcome::Refused(reason) if row.field && reason == crate::model::CHECKING => {
                if let Some(r) = self.model.cheats.iter_mut().find(|r| r.id == id) {
                    r.applied_k = Some(k);
                }
            }
            // Recusa limpa: o jogo continua com o valor de antes e a tela volta a mostrá-lo.
            OpOutcome::Refused(reason) => {
                self.set_shown_k(id, before);
                self.event(format!("{}: {reason}", row.name));
            }
            OpOutcome::Foreign => self.mark_foreign(id),
            OpOutcome::Residue(cause) => {
                self.mark_residue(&[id.to_string()]);
                self.event(format!(
                    "{}: falhou deixando restos no jogo ({cause})",
                    row.name
                ));
            }
            // O desejado já guarda o valor novo (é o mostrado).
            OpOutcome::Gone => self.on_game_closed(now),
        }
        true
    }

    // ---- efeitos dos desfechos na linha ----

    fn mark_on(&mut self, id: &str, k: u64) {
        if let Some(row) = self.model.cheats.iter_mut().find(|r| r.id == id) {
            row.on = true;
            row.residue = false;
            row.applied_k = row.value.is_some().then_some(k);
        }
    }

    fn mark_off(&mut self, id: &str) {
        if let Some(row) = self.model.cheats.iter_mut().find(|r| r.id == id) {
            row.on = false;
            row.applied_k = None;
            row.note = None;
        }
    }

    /// Os bytes eram de outro: o patch saiu do registro sem escrita (spec 6.7).
    fn mark_foreign(&mut self, id: &str) {
        let mut name = id.to_string();
        if let Some(row) = self.model.cheats.iter_mut().find(|r| r.id == id) {
            row.on = false;
            row.applied_k = None;
            row.avail = Avail::Unavailable("alterado por outro".to_string());
            name = row.name.clone();
        }
        self.set_desired(id, None);
        self.event(format!(
            "{name}: alterado por outro; o patch saiu do registro, nada foi escrito"
        ));
    }

    /// Os restos eram de bytes alterados por outro: nada foi restaurado e as entradas saíram do
    /// registro. As linhas deixam de estar em `erro` e ficam indisponíveis, como em
    /// [`Self::mark_foreign`].
    fn mark_residue_foreign(&mut self, ids: &[String]) {
        let mut names = Vec::new();
        for id in ids {
            if let Some(row) = self.model.cheats.iter_mut().find(|r| &r.id == id) {
                row.on = false;
                row.applied_k = None;
                row.residue = false;
                row.avail = Avail::Unavailable("alterado por outro".to_string());
                names.push(row.name.clone());
            }
            self.set_desired(id, None);
        }
        let who = if names.is_empty() {
            "restos".to_string()
        } else {
            names.join(", ")
        };
        self.event(format!(
            "{who}: restos não restaurados (bytes alterados por outro)"
        ));
    }

    /// Restos no jogo: a linha vira `erro`, sai dos desejados e o aviso fixo aparece.
    fn mark_residue(&mut self, ids: &[String]) {
        for id in ids {
            if let Some(row) = self.model.cheats.iter_mut().find(|r| &r.id == id) {
                row.on = false;
                row.applied_k = None;
                row.residue = true;
            }
            self.set_desired(id, None);
        }
        let names: Vec<&str> = self
            .model
            .cheats
            .iter()
            .filter(|r| r.residue)
            .map(|r| r.name.as_str())
            .collect();
        self.model.banners.residue = (!names.is_empty()).then(|| {
            format!(
                "restos de uma falha no jogo ({}); aperte r para restaurar",
                names.join(", ")
            )
        });
    }

    /// Uma vez por cheat e por conexão, avisa que um cheat não verificado foi ligado.
    fn warn_unverified(&mut self, id: &str) {
        let Some(row) = self.model.cheats.iter().find(|r| r.id == id) else {
            return;
        };
        if row.avail != (Avail::Available { verified: false }) {
            return;
        }
        let name = row.name.clone();
        if self.warned_unverified.insert(id.to_string()) {
            self.event(format!(
                "aviso: a versão do jogo não tem assinaturas conferidas; {name} foi ligado como \
                 não verificado"
            ));
        }
    }

    // ---- religar depois de reconectar ----

    /// Dá o aviso `text` uma vez por cheat e por conexão.
    fn note_once(&mut self, id: &str, note: Note, text: String) {
        if self.notes.insert((id.to_string(), note)) {
            self.event(text);
        }
    }

    /// Religa os desejados disponíveis da mesma versão (spec 6.2). Roda no `tick` seguinte ao
    /// fim de uma busca, porque só o `tick` recebe o `draw` que mostra `religando…` antes da
    /// chamada à sessão.
    /// Devolve `true` se chamou a sessão.
    fn run_reapply(&mut self, now: Instant, draw: &mut dyn FnMut(&Model)) -> bool {
        if !std::mem::take(&mut self.reapply_pending) || self.desired.is_empty() {
            return false;
        }
        let Conn::Connected { version_key, .. } = &self.model.conn else {
            return false;
        };
        let version_key = version_key.clone();
        let blocked = self.write_block();

        let rows: Vec<CheatRow> = self
            .model
            .cheats
            .iter()
            .filter(|r| self.desired.contains_key(&r.id))
            .cloned()
            .collect();
        let mut batch: Vec<(String, u64)> = Vec::new();
        let mut fields: Vec<(String, u64)> = Vec::new();
        for row in rows {
            let Some(wanted) = self.desired.get(&row.id).cloned() else {
                continue;
            };
            if row.on || row.residue {
                continue;
            }
            match &row.avail {
                // Cheat de valor esperando a reflexão: religado quando ela chegar.
                Avail::Checking => {}
                Avail::Unavailable(reason) => self.note_once(
                    &row.id,
                    Note::NotReapplied,
                    format!("{}: não foi religado (indisponível: {reason})", row.name),
                ),
                Avail::Available { .. } if wanted.version_key != version_key => self.note_once(
                    &row.id,
                    Note::NewVersion,
                    format!("{}: versão nova; aperte espaço para ligar", row.name),
                ),
                Avail::Available { .. } => match blocked {
                    Some(why) => self.note_once(
                        &row.id,
                        Note::NotReapplied,
                        format!("{}: não foi religado ({why})", row.name),
                    ),
                    None if row.field => fields.push((row.id.clone(), Self::shown_k(&row))),
                    None => batch.push((row.id.clone(), Self::shown_k(&row))),
                },
            }
        }
        // Os cheats de valor, um a um (não pausam o jogo).
        let mut called = false;
        for (id, k) in fields {
            self.note_op(&id);
            let Some(outcome) =
                self.run_op("religando…".to_string(), draw, |s| s.field_on(&id, k))
            else {
                return called;
            };
            called = true;
            if !self.finish_reapply_one(&id, k, outcome, &version_key, now) {
                return true;
            }
        }
        if batch.is_empty() {
            return called;
        }
        for (id, _) in &batch {
            self.note_op(id);
        }
        let items = batch.clone();
        let Some(outcome) = self.run_op("religando…".to_string(), draw, |s| s.apply(&items))
        else {
            return false;
        };
        match outcome {
            OpOutcome::Done(pause) => {
                for (id, k) in &batch {
                    self.reapplied(id, *k, &version_key, pause);
                }
            }
            // Religar interrompido não muda nada.
            OpOutcome::Gone => self.on_game_closed(now),
            // O motor deixa o lote inteiro pendente: não tenta um a um.
            OpOutcome::Residue(cause) => {
                let pending = self.session_call(|s| s.pending_ids()).unwrap_or_default();
                self.mark_residue(&pending);
                self.event(format!(
                    "religar falhou deixando restos no jogo: {} ({cause})",
                    pending.join(", ")
                ));
            }
            outcome @ (OpOutcome::Refused(_) | OpOutcome::Foreign) => {
                if let [(id, k)] = batch.as_slice() {
                    self.finish_reapply_one(id, *k, outcome, &version_key, now);
                } else {
                    self.reapply_one_by_one(&batch, &version_key, now, draw);
                }
            }
        }
        true
    }

    /// O lote foi recusado: cada desejado é tentado sozinho, para que um patch com problema
    /// não impeça os outros.
    fn reapply_one_by_one(
        &mut self,
        batch: &[(String, u64)],
        version_key: &str,
        now: Instant,
        draw: &mut dyn FnMut(&Model),
    ) {
        for (id, k) in batch {
            let item = [(id.clone(), *k)];
            let Some(outcome) = self.run_op("religando…".to_string(), draw, |s| s.apply(&item))
            else {
                return;
            };
            if !self.finish_reapply_one(id, *k, outcome, version_key, now) {
                return;
            }
        }
    }

    /// Efeito do desfecho do religar de um cheat sozinho. `false` se o jogo fechou (a
    /// religação para).
    fn finish_reapply_one(
        &mut self,
        id: &str,
        k: u64,
        outcome: OpOutcome,
        version_key: &str,
        now: Instant,
    ) -> bool {
        let name = self.name_of(id);
        match outcome {
            OpOutcome::Done(pause) => self.reapplied(id, k, version_key, pause),
            OpOutcome::Refused(reason) => {
                self.event(format!("{name}: não foi religado ({reason})"));
            }
            OpOutcome::Foreign => self.mark_foreign(id),
            OpOutcome::Residue(cause) => {
                self.mark_residue(&[id.to_string()]);
                self.event(format!("{name}: falhou deixando restos no jogo ({cause})"));
            }
            OpOutcome::Gone => {
                self.on_game_closed(now);
                return false;
            }
        }
        true
    }

    fn reapplied(&mut self, id: &str, k: u64, version_key: &str, pause: Duration) {
        let name = self.name_of(id);
        self.mark_on(id, k);
        self.set_desired(
            id,
            Some(Desired {
                k,
                version_key: version_key.to_string(),
            }),
        );
        self.event(format!("{name} religado (pausa {} ms)", ms(pause)));
        self.warn_unverified(id);
    }

    fn name_of(&self, id: &str) -> String {
        self.model
            .cheats
            .iter()
            .find(|r| r.id == id)
            .map_or_else(|| id.to_string(), |r| r.name.clone())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::path::PathBuf;

    use gt_core::value::{ValueGrid, ValueType};

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

        /// Uma volta do laço em `ms`; devolve se o `tick` chamou a sessão.
        fn tick(&mut self, ms: u64) -> bool {
            let now = self.at(ms);
            self.c.tick(now, &mut *self.draw)
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

        /// Aperta `action` em `ms`; devolve se a sessão foi chamada.
        fn key(&mut self, action: Action, ms: u64) -> bool {
            let now = self.at(ms);
            self.c.handle(action, now, &mut *self.draw)
        }

        /// Põe a seleção no cheat `id`.
        fn sel(&mut self, id: &str) {
            let index = self
                .c
                .model
                .visible()
                .iter()
                .position(|r| r.id == id)
                .unwrap();
            self.c.model.selected = index;
        }

        /// Seleciona `id` e aperta espaço.
        fn toggle(&mut self, id: &str, ms: u64) -> bool {
            self.sel(id);
            self.key(Action::Toggle, ms)
        }

        /// O jogo fecha em `close_ms` e reabre em `open_ms` (a busca de conexão termina; a
        /// religação fica para o `tick` seguinte).
        fn reopen(&mut self, close_ms: u64, open_ms: u64) {
            self.ctl.set_exited(true);
            self.tick(close_ms);
            self.ctl.set_exited(false);
            self.connect(open_ms);
        }

        fn applies(&self) -> Vec<Call> {
            self.ctl
                .calls()
                .into_iter()
                .filter(|c| matches!(c, Call::Apply { .. }))
                .collect()
        }

        fn row(&self, id: &str) -> &CheatRow {
            self.c.model.cheats.iter().find(|r| r.id == id).unwrap()
        }

        fn events(&self) -> Vec<&str> {
            self.c.model.events.iter().map(String::as_str).collect()
        }
    }

    fn grid() -> ValueGrid {
        ValueGrid {
            ty: ValueType::U32,
            min: 0.0,
            max: 10.0,
            step: 1.0,
            default_k: 2,
            unit: None,
            decimals: 0,
        }
    }

    fn apply_call(items: &[(&str, u64)], busy: &str) -> Call {
        Call::Apply {
            items: items.iter().map(|(id, k)| (id.to_string(), *k)).collect(),
            busy: Some(busy.to_string()),
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
            .set_catalog(vec![cheat("a", available()), cheat("b", not_found())]);
        h.connect(0);

        h.ctl.push_search(Ok(SearchSpec::new(vec![
            cheat("a", unavailable("bytes diferentes")),
            cheat("b", available()),
        ])));
        h.ctl.hold_search();
        h.tick(5_000);
        // Ligar e desligar `a` enquanto a busca roda: o cheat termina como começou, mas passou
        // por operações, e o resultado dele deixa de valer.
        assert!(h.toggle("a", 5_010));
        assert!(h.toggle("a", 5_020));
        assert!(!h.row("a").on);
        h.ctl.release_search();
        assert!(h.wait(5_100));
        assert_eq!(
            h.row("a").avail,
            available(),
            "o resultado de `a` foi descartado"
        );
        assert_eq!(h.row("b").avail, available());
        // A sessão também recebe só `b`.
        assert_eq!(h.ctl.merge_ids(), [Some(vec!["b".to_string()])]);
    }

    #[test]
    fn on_during_search_drops_that_cheat_result_in_session() {
        let mut h = H::new();
        h.ctl
            .set_catalog(vec![cheat("a", available()), cheat("b", not_found())]);
        h.connect(0);
        h.ctl.push_search(Ok(SearchSpec::new(vec![
            cheat("a", unavailable("bytes diferentes")),
            cheat("b", available()),
        ])));
        h.ctl.hold_search();
        h.tick(5_000);
        // `a` foi ligado durante a busca e continua ligado.
        h.c.note_op("a");
        h.c.model.cheats[0].on = true;
        h.ctl.release_search();
        assert!(h.wait(5_100));
        assert_eq!(h.row("a").avail, available());
        assert_eq!(h.row("b").avail, available());
        assert_eq!(h.ctl.merge_ids(), [Some(vec!["b".to_string()])]);
    }

    #[test]
    fn on_without_op_count_is_also_dropped() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![cheat("a", available())]);
        h.connect(0);
        h.c.model.cheats[0].on = true;
        h.ctl
            .push_search(Ok(SearchSpec::new(vec![cheat("a", not_found())])));
        // `a` está ligado: nem entra na busca; o `FakeSession` não recebe nada dele.
        assert!(h.c.start_refresh(h.at(5_000)));
        assert!(h.wait(5_000));
        assert_eq!(h.ctl.merge_ids(), [Some(Vec::<String>::new())]);
    }

    #[test]
    fn refresh_updates_has_version_file() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![cheat("a", not_found())]);
        h.connect(0);
        let mut spec = SearchSpec::new(vec![cheat("a", available())]);
        spec.has_version_file = false;
        h.ctl.push_search(Ok(spec));
        h.tick(5_000);
        assert!(h.wait(5_000));
        let Conn::Connected {
            has_version_file, ..
        } = &h.c.model.conn
        else {
            panic!("esperava conectado");
        };
        assert!(!has_version_file);
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
        h.c.set_desired(
            "a",
            Some(Desired {
                k: 3,
                version_key: "v1".to_string(),
            }),
        );
        h.ctl.set_exited(true);
        h.tick(100);
        h.ctl.set_exited(false);
        h.ctl.set_catalog(vec![cheat("a", not_found())]);
        h.connect(5_000);
        assert_eq!(h.c.model.cheats.len(), 1);
        assert!(h.row("a").desired);
        assert_eq!(h.row("a").avail, not_found(), "conexão nova: estado novo");
    }

    // ---- Tarefa 12: desejados, ligar/desligar e religação ----

    fn desired_of(k: u64, version: &str) -> Desired {
        Desired {
            k,
            version_key: version.to_string(),
        }
    }

    fn event_count(h: &H, needle: &str) -> usize {
        h.events().iter().filter(|e| e.contains(needle)).count()
    }

    #[test]
    fn toggle_on_adds_desired_and_off_removes() {
        let mut h = H::new();
        let mut v = cheat("v", available());
        v.value = Some(grid());
        h.ctl.set_catalog(vec![cheat("a", available()), v]);
        h.connect(0);
        h.c.model.cheats[1].k = 3;

        assert!(h.toggle("a", 100));
        assert!(h.row("a").on);
        assert!(h.row("a").desired);
        assert_eq!(h.c.desired().get("a"), Some(&desired_of(0, "v1")));
        assert_eq!(h.events().last(), Some(&"12:00 a ligado (pausa 1 ms)"));

        // Cheat com valor: liga com o valor mostrado, que vai para os desejados.
        assert!(h.toggle("v", 200));
        assert_eq!(h.row("v").applied_k, Some(3));
        assert_eq!(h.c.desired().get("v"), Some(&desired_of(3, "v1")));
        assert!(h.applies().contains(&apply_call(&[("v", 3)], "ligando v…")));

        assert!(h.toggle("a", 300));
        assert!(!h.row("a").on);
        assert!(!h.row("a").desired);
        assert_eq!(h.c.desired().get("a"), None);
        assert_eq!(h.events().last(), Some(&"12:00 a desligado (pausa 1 ms)"));
        assert!(h.row("v").on, "o outro cheat não muda");
    }

    #[test]
    fn failed_toggle_not_desired() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![cheat("a", available())]);
        h.connect(0);
        h.ctl
            .push_outcome(OpOutcome::Refused("threads no trecho".to_string()));
        assert!(h.toggle("a", 100));
        assert!(!h.row("a").on);
        assert!(!h.row("a").desired);
        assert!(h.c.desired().is_empty());
        assert_eq!(h.events().last(), Some(&"12:00 a: threads no trecho"));
        assert_eq!(h.c.model.busy, None);
    }

    #[test]
    fn busy_drawn_before_session_call() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![cheat("a", available())]);
        h.connect(0);
        h.toggle("a", 100);
        h.toggle("a", 200);
        // O alvo falso registra o `busy` do último `Model` desenhado quando recebe a chamada.
        let calls = h.ctl.calls();
        assert!(calls.contains(&apply_call(&[("a", 0)], "ligando a…")));
        assert!(calls.contains(&Call::Undo {
            id: "a".to_string(),
            busy: Some("desligando a…".to_string())
        }));
        // Depois da operação a mensagem some do modelo.
        assert_eq!(h.c.model.busy, None);
    }

    #[test]
    fn toggles_blocked_while_connecting() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![cheat("a", available())]);
        h.connect(0);
        h.ctl.set_exited(true);
        h.tick(100);
        h.ctl.set_exited(false);
        h.ctl.hold_search();
        h.tick(3_000);
        assert_eq!(h.c.model.conn, Conn::Connecting);
        assert!(!h.toggle("a", 3_100));
        assert!(h.applies().is_empty());
        assert!(!h.row("a").on);
        assert!(h.events().last().is_some_and(|e| e.contains("aguarde")));
        h.ctl.release_search();
        assert!(h.wait(3_200));

        // Diário não resolvido: conectado, mas sem escrever.
        let mut b = H::new();
        b.ctl.push_recover(blocked("não foi possível restaurar x"));
        b.ctl.set_catalog(vec![cheat("a", available())]);
        b.connect(0);
        assert!(!b.toggle("a", 100));
        assert!(b.applies().is_empty());
        assert!(!b.row("a").on);
        assert!(
            b.events()
                .last()
                .is_some_and(|e| e.contains("diário com entradas não restauradas"))
        );
    }

    #[test]
    fn game_close_keeps_desired_and_reconnect_reapplies() {
        let mut h = H::new();
        h.ctl
            .set_catalog(vec![cheat("a", available()), cheat("b", available())]);
        h.connect(0);
        assert!(h.toggle("a", 100));
        h.ctl.clear_calls();

        h.reopen(1_000, 4_000);
        // O jogo fechou: sai o `ligado`, ficam os desejados.
        assert!(!h.row("a").on);
        assert!(h.row("a").desired);
        // `wait_search` não religa (não tem `draw`): a religação é do `tick` seguinte.
        assert!(h.applies().is_empty());
        h.tick(4_100);
        assert_eq!(h.applies(), [apply_call(&[("a", 0)], "religando…")]);
        assert!(h.row("a").on);
        assert!(!h.row("b").on, "só o desejado é religado");
        assert!(event_count(&h, "a religado (pausa 1 ms)") == 1);
        assert_eq!(h.c.model.busy, None);

        // Nada mais a religar: outra volta não chama a sessão.
        h.tick(4_200);
        assert_eq!(h.applies().len(), 1);
    }

    #[test]
    fn reapply_batch_then_one_by_one() {
        fn three() -> H {
            let mut h = H::new();
            h.ctl.set_catalog(vec![
                cheat("a", available()),
                cheat("b", available()),
                cheat("c", available()),
            ]);
            h.connect(0);
            for id in ["a", "b", "c"] {
                assert!(h.toggle(id, 100));
            }
            h.ctl.clear_calls();
            h
        }
        let batch = apply_call(&[("a", 0), ("b", 0), ("c", 0)], "religando…");

        // Lote recusado: cada um sozinho; um recusado não impede os outros.
        let mut h = three();
        h.ctl.push_outcome(OpOutcome::Refused("lote".to_string()));
        h.ctl.push_outcome(OpOutcome::Done(Duration::ZERO));
        h.ctl
            .push_outcome(OpOutcome::Refused("não deu".to_string()));
        h.ctl.push_outcome(OpOutcome::Done(Duration::ZERO));
        h.reopen(1_000, 4_000);
        h.tick(4_100);
        assert_eq!(
            h.applies(),
            [
                batch.clone(),
                apply_call(&[("a", 0)], "religando…"),
                apply_call(&[("b", 0)], "religando…"),
                apply_call(&[("c", 0)], "religando…"),
            ]
        );
        assert!(h.row("a").on && !h.row("b").on && h.row("c").on);
        assert!(h.row("b").desired, "continua desejado");
        assert!(event_count(&h, "b: não foi religado (não deu)") == 1);

        // Lote com `Gone`: para.
        let mut h = three();
        h.ctl.push_outcome(OpOutcome::Gone);
        h.reopen(1_000, 4_000);
        h.tick(4_100);
        assert_eq!(h.applies(), std::slice::from_ref(&batch));
        assert_eq!(h.c.model.conn, Conn::Waiting);
        assert_eq!(h.c.desired().len(), 3, "religar interrompido não muda nada");
        assert!(h.row("a").desired && !h.row("a").on);

        // Lote com restos: todos os pendentes em `erro`, sem tentar um a um.
        let mut h = three();
        h.ctl.push_residue("escrita", &["a", "b", "c"]);
        h.reopen(1_000, 4_000);
        h.tick(4_100);
        assert_eq!(h.applies(), std::slice::from_ref(&batch));
        for id in ["a", "b", "c"] {
            let row = h.row(id);
            assert!(row.residue && !row.on && !row.desired, "{id}");
            assert_eq!(
                row.status(&h.c.model.conn),
                crate::model::CheatStatus::Residue
            );
        }
        assert!(h.c.desired().is_empty());
        assert!(h.c.model.banners.residue.is_some());

        // Restos num dos um a um: marca aquele e segue com os demais.
        let mut h = three();
        h.ctl.push_outcome(OpOutcome::Refused("lote".to_string()));
        h.ctl.push_residue("escrita", &["a"]);
        h.ctl.push_outcome(OpOutcome::Done(Duration::ZERO));
        h.ctl.push_outcome(OpOutcome::Done(Duration::ZERO));
        h.reopen(1_000, 4_000);
        h.tick(4_100);
        assert_eq!(h.applies().len(), 4);
        assert!(h.row("a").residue && !h.row("a").on);
        assert!(h.row("b").on && h.row("c").on);
        assert!(!h.c.desired().contains_key("a"));
    }

    #[test]
    fn reapply_only_same_version() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![cheat("a", available())]);
        h.connect(0);
        assert!(h.toggle("a", 100));

        // O jogo atualizou: a chave de versão mudou.
        let mut spec = SearchSpec::new(vec![cheat("a", available())]);
        spec.version_key = "v2".to_string();
        h.ctl.set_default_search(spec);
        h.ctl.clear_calls();
        h.reopen(1_000, 4_000);
        h.tick(4_100);
        h.tick(4_200);
        assert!(h.applies().is_empty(), "outra versão não religa sozinho");
        assert!(h.row("a").desired && !h.row("a").on);
        assert_eq!(
            event_count(&h, "a: versão nova; aperte espaço para ligar"),
            1,
            "avisa uma vez por conexão"
        );

        // O espaço liga e atualiza a versão guardada.
        assert!(h.toggle("a", 4_300));
        assert!(h.row("a").on);
        assert_eq!(h.c.desired().get("a"), Some(&desired_of(0, "v2")));

        // Na próxima reconexão (ainda v2) religa sozinho.
        h.ctl.clear_calls();
        h.reopen(5_000, 8_000);
        h.tick(8_100);
        assert_eq!(h.applies().len(), 1);
        assert!(h.row("a").on);
    }

    #[test]
    fn close_during_off_removes_desired() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![cheat("a", available())]);
        h.connect(0);
        assert!(h.toggle("a", 100));
        h.ctl.push_outcome(OpOutcome::Gone);
        assert!(h.toggle("a", 200));
        assert_eq!(h.c.model.conn, Conn::Waiting);
        assert!(h.c.desired().is_empty());
        assert!(!h.row("a").desired);
        assert!(!h.row("a").on);
        assert_eq!(h.events().last(), Some(&"12:00 o jogo fechou"));
        assert_eq!(h.c.model.busy, None);
    }

    #[test]
    fn close_during_on_does_not_add() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![cheat("a", available())]);
        h.connect(0);
        h.ctl.push_outcome(OpOutcome::Gone);
        assert!(h.toggle("a", 100));
        assert_eq!(h.c.model.conn, Conn::Waiting);
        assert!(h.c.desired().is_empty());
        assert!(!h.row("a").desired && !h.row("a").on);
        assert_eq!(h.events().last(), Some(&"12:00 o jogo fechou"));
    }

    #[test]
    fn undo_failures_follow_table() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![
            cheat("a", available()),
            cheat("b", available()),
            cheat("c", available()),
        ]);
        h.connect(0);
        for id in ["a", "b", "c"] {
            assert!(h.toggle(id, 100));
        }

        // Recusa limpa: continua ligado e desejado.
        h.ctl
            .push_outcome(OpOutcome::Refused("threads no trecho".to_string()));
        assert!(h.toggle("a", 200));
        assert!(h.row("a").on && h.row("a").desired);
        assert!(h.c.desired().contains_key("a"));
        assert_eq!(h.events().last(), Some(&"12:00 a: threads no trecho"));

        // Bytes de outro: indisponível e fora dos desejados.
        h.ctl.push_outcome(OpOutcome::Foreign);
        assert!(h.toggle("b", 300));
        let b = h.row("b");
        assert!(!b.on && !b.desired);
        assert_eq!(
            b.status(&h.c.model.conn),
            crate::model::CheatStatus::Unavailable("alterado por outro".to_string())
        );
        assert!(!h.c.desired().contains_key("b"));

        // Restos: `erro`, aviso fixo e fora dos desejados.
        h.ctl.push_residue("escrita", &["c"]);
        assert!(h.toggle("c", 400));
        let c = h.row("c");
        assert!(c.residue && !c.on && !c.desired);
        assert_eq!(
            c.status(&h.c.model.conn),
            crate::model::CheatStatus::Residue
        );
        assert!(!h.c.desired().contains_key("c"));
        assert!(
            h.c.model
                .banners
                .residue
                .as_deref()
                .is_some_and(|t| t.contains('c'))
        );
    }

    #[test]
    fn space_on_marked_unavailable_clears_desired() {
        let mut h = H::new();
        h.ctl
            .set_catalog(vec![cheat("a", available()), cheat("b", available())]);
        h.connect(0);
        assert!(h.toggle("a", 100));
        h.ctl
            .set_catalog(vec![cheat("a", not_found()), cheat("b", not_found())]);
        h.ctl.clear_calls();
        h.reopen(1_000, 4_000);
        h.tick(4_100);
        assert!(h.applies().is_empty());
        assert!(h.row("a").desired);
        assert!(event_count(&h, "a: não foi religado (indisponível: não encontrado)") == 1);

        // Sem a marca: só escreve o motivo.
        assert!(!h.toggle("b", 4_200));
        assert_eq!(
            h.events().last(),
            Some(&"12:00 b: indisponível (não encontrado)")
        );
        assert_eq!(event_count(&h, "não será mais religado"), 0);

        // Com a marca: tira dos desejados e confirma.
        assert!(!h.toggle("a", 4_300));
        assert!(!h.row("a").desired);
        assert!(h.c.desired().is_empty());
        assert_eq!(h.events().last(), Some(&"12:00 a: não será mais religado"));
        assert!(h.applies().is_empty());
    }

    #[test]
    fn auto_search_stops_without_pending_desired() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![cheat("a", available())]);
        h.connect(0);
        assert!(h.toggle("a", 100));
        // `ambíguo` não dispara busca sozinho; o desejado indisponível dispara.
        h.ctl.set_catalog(vec![cheat("a", unavailable("ambíguo"))]);
        h.reopen(1_000, 4_000);
        h.tick(4_100);
        h.tick(8_900);
        assert!(!h.searching());
        h.tick(9_000);
        assert!(h.searching());
        assert!(h.wait(9_000));

        // O dono tira o desejado: acabam as buscas automáticas.
        assert!(!h.toggle("a", 9_100));
        assert!(h.c.desired().is_empty());
        let searches = h.ctl.search_calls();
        for t in 3..30 {
            h.tick(t * 5_000);
            assert!(!h.searching());
        }
        assert_eq!(h.ctl.search_calls(), searches);
    }

    #[test]
    fn auto_search_reapplies_found_desired() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![cheat("a", available())]);
        h.connect(0);
        assert!(h.toggle("a", 100));
        // O jogo reabre ainda carregando: o cheat não aparece.
        h.ctl.set_catalog(vec![cheat("a", not_found())]);
        h.ctl.clear_calls();
        h.reopen(1_000, 4_000);
        h.tick(4_100);
        assert!(h.applies().is_empty());

        // A busca automática o acha; a religação vem no `tick` seguinte.
        h.ctl
            .push_search(Ok(SearchSpec::new(vec![cheat("a", available())])));
        h.tick(9_000);
        assert!(h.searching());
        assert!(h.wait(9_000));
        assert_eq!(h.row("a").avail, available());
        assert!(h.applies().is_empty());
        h.tick(9_100);
        assert_eq!(h.applies(), [apply_call(&[("a", 0)], "religando…")]);
        assert!(h.row("a").on);
        assert_eq!(event_count(&h, "a religado"), 1);
        // Já religado e sem nada faltando: o desejado não mantém a busca.
        h.tick(20_000);
        assert!(!h.searching());
    }

    #[test]
    fn unverified_warning_once_per_connection() {
        let warning = "aviso: a versão do jogo não tem assinaturas conferidas; a foi ligado como \
                       não verificado";
        let mut h = H::new();
        h.ctl
            .set_catalog(vec![cheat("a", Avail::Available { verified: false })]);
        h.connect(0);
        assert!(h.toggle("a", 100));
        assert!(h.toggle("a", 200));
        assert!(h.toggle("a", 300));
        assert_eq!(event_count(&h, warning), 1);

        // Na conexão nova, ao religar, avisa de novo.
        h.reopen(1_000, 4_000);
        h.tick(4_100);
        assert!(h.row("a").on);
        assert_eq!(event_count(&h, warning), 2);
    }

    #[test]
    fn too_small_only_quit_and_retry() {
        let mut h = H::new();
        h.ctl
            .set_catalog(vec![cheat("a", available()), cheat("b", available())]);
        h.connect(0);
        h.c.model.too_small = true;
        for action in [
            Action::Toggle,
            Action::Down,
            Action::Up,
            Action::Dec,
            Action::Inc,
        ] {
            assert!(!h.key(action, 100));
        }
        assert_eq!(h.c.model.selected, 0);
        assert!(h.applies().is_empty());
        assert!(!h.c.quit_requested());
        assert!(!h.key(Action::Quit, 200));
        assert!(h.c.quit_requested());

        // `r` funciona: sem conexão, procura na hora, sem esperar os 2 s.
        let mut w = H::new();
        w.c.model.too_small = true;
        w.key(Action::Retry, 0);
        w.key(Action::Retry, 10);
        assert_eq!(w.ctl.find_calls(), 2);
    }

    #[test]
    fn selection_keys_move_over_visible() {
        let mut h = H::new();
        h.ctl
            .set_catalog(vec![cheat("a", available()), cheat("b", available())]);
        h.connect(0);
        assert!(!h.key(Action::Down, 100));
        assert_eq!(h.c.model.selected_id(), Some("b"));
        h.key(Action::Down, 110);
        assert_eq!(h.c.model.selected_id(), Some("b"));
        h.key(Action::Up, 120);
        assert_eq!(h.c.model.selected_id(), Some("a"));
    }

    #[test]
    fn retry_noop_states() {
        // Conectando: nada.
        let mut h = H::new();
        h.ctl.set_catalog(vec![cheat("a", available())]);
        h.ctl.set_find(Find::Found(100));
        h.ctl.hold_search();
        h.tick(0);
        assert_eq!(h.c.model.conn, Conn::Connecting);
        let (finds, searches) = (h.ctl.find_calls(), h.ctl.search_calls());
        assert!(!h.key(Action::Retry, 3_000));
        assert_eq!(h.c.model.conn, Conn::Connecting);
        assert_eq!(
            (h.ctl.find_calls(), h.ctl.search_calls()),
            (finds, searches)
        );
        h.ctl.release_search();
        assert!(h.wait(3_100));

        // O processo terminou (com `--pid`): nada.
        let mut p = H::pid_fixed();
        p.ctl.set_catalog(vec![cheat("a", available())]);
        p.connect(0);
        p.ctl.set_exited(true);
        p.tick(100);
        assert_eq!(p.c.model.conn, Conn::PidGone);
        let finds = p.ctl.find_calls();
        p.key(Action::Retry, 10_000);
        assert_eq!(p.c.model.conn, Conn::PidGone);
        assert_eq!(p.ctl.find_calls(), finds);

        // Durante uma operação: nada.
        let mut w = H::new();
        w.c.model.busy = Some("ligando a…".to_string());
        w.key(Action::Retry, 0);
        assert_eq!(w.ctl.find_calls(), 0);
    }

    #[test]
    fn retry_waiting_looks_now_and_connected_searches_again() {
        // Sem conexão: procura na hora, mesmo dentro do intervalo de 2 s.
        let mut h = H::new();
        h.ctl.set_catalog(vec![cheat("a", not_found())]);
        h.tick(0);
        assert_eq!(h.ctl.find_calls(), 1);
        h.ctl.set_find(Find::Found(100));
        h.key(Action::Retry, 100);
        assert_eq!(h.c.model.conn, Conn::Connecting);
        assert!(h.wait(100));

        // Conectado e com o prazo de 120 s esgotado: o `r` refaz a busca e reinicia o prazo.
        h.tick(130_000);
        assert!(!h.searching());
        assert!(!h.key(Action::Retry, 130_000));
        assert!(h.searching());
        assert!(h.wait(130_000));
        h.tick(135_000);
        assert!(h.searching(), "o prazo recomeçou");
    }

    #[test]
    fn retry_connected_refreshes_even_with_unreadable_disk() {
        let mut h = H::new();
        let mut bad = SearchSpec::new(vec![cheat("a", unavailable("disco ilegível"))]);
        bad.disk = Err("sem cabeçalho".to_string());
        h.ctl.set_default_search(bad);
        h.connect(0);
        let searches = h.ctl.search_calls();
        // Só o `r` refaz a busca com o disco ilegível.
        h.ctl
            .push_search(Ok(SearchSpec::new(vec![cheat("a", available())])));
        h.key(Action::Retry, 1_000);
        assert!(h.wait(1_000));
        assert_eq!(h.ctl.search_calls(), searches + 1);
        assert_eq!(connected(&h.c), (100, "v1".to_string(), true, true));
        assert_eq!(h.row("a").avail, available());
    }

    // ---- Tarefa 13: restos, SIGCONT e a tecla `r` completa ----

    fn count_calls(h: &H, f: impl Fn(&Call) -> bool) -> usize {
        h.ctl.count(f)
    }

    #[test]
    fn retry_order() {
        // Tudo ao mesmo tempo: SIGCONT que falhou, restos, diário bloqueado, jogo conectado.
        let mut h = H::new();
        h.ctl.push_recover(blocked("não foi possível restaurar x"));
        h.ctl
            .set_catalog(vec![cheat("a", available()), cheat("b", not_found())]);
        h.connect(0);
        h.ctl.set_pending(&["a"]);
        h.ctl.set_stuck(true);
        h.tick(100);
        h.ctl.clear_calls();

        // 1. SIGCONT: só ele, com `restaurando…` no topo; recusado, o aviso continua.
        h.ctl.push_outcome(OpOutcome::Refused("EPERM".to_string()));
        assert!(h.key(Action::Retry, 200));
        assert_eq!(
            h.ctl.calls(),
            [Call::RetryResume {
                busy: Some("restaurando…".to_string())
            }]
        );
        assert!(h.c.model.banners.stuck.is_some());
        assert!(
            h.events()
                .last()
                .is_some_and(|e| e.contains("não foi possível retomar o jogo: EPERM"))
        );
        assert_eq!(h.c.model.busy, None);

        // 2. Restos: o SIGCONT deu certo, o aviso some e a próxima tecla restaura.
        h.ctl.clear_calls();
        h.ctl.push_outcome(OpOutcome::Done(Duration::ZERO));
        assert!(h.key(Action::Retry, 300));
        assert_eq!(h.c.model.banners.stuck, None);
        assert_eq!(
            h.ctl.calls(),
            [Call::RetryResume {
                busy: Some("restaurando…".to_string())
            }]
        );
        h.ctl.clear_calls();
        assert!(h.key(Action::Retry, 400));
        assert_eq!(
            h.ctl.calls(),
            [Call::RestorePending {
                busy: Some("restaurando…".to_string())
            }]
        );
        assert!(!h.row("a").residue);
        assert_eq!(h.c.model.banners.residue, None);

        // 3. Diário bloqueado: recupera de novo (ainda bloqueado), sem busca.
        h.ctl.clear_calls();
        h.ctl.push_recover(blocked("ainda x"));
        assert!(h.key(Action::Retry, 500));
        assert_eq!(
            h.ctl.calls(),
            [Call::Recover {
                busy: Some("recuperando o diário…".to_string())
            }]
        );
        assert_eq!(h.c.model.banners.blocked.as_deref(), Some("ainda x"));
        assert!(!h.searching());

        // 4. Diário resolvido e jogo conectado: o motor abre; depois, nova busca dos desligados.
        h.ctl.clear_calls();
        assert!(h.key(Action::Retry, 600));
        assert_eq!(h.c.model.banners.blocked, None);
        assert!(h.ctl.calls().contains(&Call::OpenEngine));
        assert!(!h.searching());
        h.ctl.clear_calls();
        assert!(!h.key(Action::Retry, 700));
        assert!(h.searching());
        assert!(h.wait(700));
        assert_eq!(
            h.ctl.search_ids(),
            [Some(vec!["a".to_string(), "b".to_string()])]
        );
        assert_eq!(count_calls(&h, |c| matches!(c, Call::Recover { .. })), 0);

        // 5. Sem conexão: procura o jogo na hora.
        let mut w = H::new();
        w.tick(0);
        w.ctl.clear_calls();
        assert!(!w.key(Action::Retry, 100));
        assert_eq!(w.ctl.calls(), [Call::Find]);
    }

    #[test]
    fn retry_stuck_without_pending_or_diary_trouble() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![cheat("a", available())]);
        h.connect(0);
        h.ctl.set_stuck(true);
        // Mesmo sem nenhuma tick depois do `set_stuck`, o `r` já consulta a sessão.
        h.ctl.clear_calls();
        assert!(h.key(Action::Retry, 100));
        assert_eq!(h.ctl.calls().len(), 1);
        assert_eq!(h.c.model.banners.stuck, None);
        assert_eq!(
            h.events().last(),
            Some(&"12:00 o jogo foi retomado (SIGCONT enviado)")
        );
    }

    #[test]
    fn restore_pending_clears_residue() {
        // Pelo `r`.
        let mut h = H::new();
        h.ctl.set_catalog(vec![
            cheat("a", available()),
            cheat("b", available()),
            cheat("c", available()),
        ]);
        h.connect(0);
        h.ctl.push_residue("escrita", &["a"]);
        assert!(h.toggle("a", 100));
        let a = h.row("a");
        assert!(a.residue && !a.on);
        assert_eq!(
            a.status(&h.c.model.conn),
            crate::model::CheatStatus::Residue
        );
        assert!(h.c.model.banners.residue.is_some());

        // Resto não bloqueia ligar os outros cheats (spec 6.8).
        assert!(h.toggle("b", 200));
        assert!(h.row("b").on);
        assert!(h.row("a").residue);

        h.ctl.clear_calls();
        assert!(h.key(Action::Retry, 300));
        assert_eq!(
            h.ctl.calls(),
            [Call::RestorePending {
                busy: Some("restaurando…".to_string())
            }]
        );
        let a = h.row("a");
        assert!(!a.residue && !a.on && !a.desired);
        assert_eq!(a.status(&h.c.model.conn), crate::model::CheatStatus::Off);
        assert_eq!(h.c.model.banners.residue, None);
        assert!(h.row("b").on, "o outro cheat não muda");

        // Pelo espaço no cheat em `erro`.
        let mut s = H::new();
        s.ctl
            .set_catalog(vec![cheat("a", available()), cheat("c", available())]);
        s.connect(0);
        s.ctl.push_residue("escrita", &["a", "c"]);
        assert!(s.toggle("a", 100));
        assert!(s.row("a").residue);
        assert!(s.row("c").residue, "o motor também deixou `c` pendente");
        assert!(
            s.c.model
                .banners
                .residue
                .as_deref()
                .is_some_and(|t| t.contains('a') && t.contains('c'))
        );
        s.ctl.clear_calls();
        assert!(s.toggle("a", 200));
        assert_eq!(
            s.ctl.calls(),
            [Call::RestorePending {
                busy: Some("restaurando…".to_string())
            }]
        );
        assert!(!s.row("a").residue && !s.row("c").residue);
        assert_eq!(s.c.model.banners.residue, None);
        assert!(!s.row("a").on);
        // Voltou a `[OFF]`: o próximo espaço liga.
        assert!(s.toggle("a", 300));
        assert!(s.row("a").on);
    }

    #[test]
    fn restore_pending_failures_keep_residue() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![cheat("a", available())]);
        h.connect(0);
        h.ctl.push_residue("escrita", &["a"]);
        assert!(h.toggle("a", 100));

        // Restauração que não resolve: os restos continuam e o aviso também.
        h.ctl.push_residue("sem permissão", &["a"]);
        assert!(h.key(Action::Retry, 200));
        assert!(h.row("a").residue);
        assert!(h.c.model.banners.residue.is_some());
        assert!(
            h.events()
                .last()
                .is_some_and(|e| e.contains("restos continuam no jogo (sem permissão)"))
        );

        // O jogo fechou durante a restauração.
        h.ctl.push_outcome(OpOutcome::Gone);
        assert!(h.key(Action::Retry, 300));
        assert_eq!(h.c.model.conn, Conn::Waiting);
        assert!(!h.row("a").residue);
        assert_eq!(h.c.model.banners.residue, None);
        assert_eq!(h.c.model.busy, None);
    }

    #[test]
    fn restore_pending_foreign_is_not_reported_as_restored() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![cheat("a", available())]);
        h.connect(0);
        h.ctl.push_residue("escrita", &["a"]);
        assert!(h.toggle("a", 100));
        assert!(h.row("a").residue);

        // Bytes de outro: o motor não escreveu nada e tirou a entrada do diário.
        h.ctl.push_outcome(OpOutcome::Foreign);
        assert!(h.key(Action::Retry, 200));
        let last = h.events().last().copied().unwrap_or_default();
        assert!(
            last.contains("a: restos não restaurados (bytes alterados por outro)"),
            "{last}"
        );
        assert!(
            !h.events().iter().any(|e| e.contains("restos restaurados")),
            "{:?}",
            h.events()
        );
        let a = h.row("a");
        assert!(!a.residue && !a.on && !a.desired);
        assert_eq!(
            a.avail,
            Avail::Unavailable("alterado por outro".to_string())
        );
        assert_eq!(h.c.model.banners.residue, None);
    }

    #[test]
    fn stuck_banner_appears_and_clears() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![cheat("a", available())]);
        h.connect(0);
        assert_eq!(h.c.model.banners.stuck, None);

        // Uma operação que deixa o jogo parado: o aviso aparece logo depois da chamada.
        h.ctl.set_stuck(true);
        assert!(h.toggle("a", 100));
        assert_eq!(
            h.c.model.banners.stuck.as_deref(),
            Some("o jogo pode ter ficado parado: kill -CONT 100")
        );

        // O SIGCONT que falha de novo mantém o aviso.
        h.ctl.push_outcome(OpOutcome::Refused("EPERM".to_string()));
        assert!(h.key(Action::Retry, 200));
        assert!(h.c.model.banners.stuck.is_some());

        // Deu certo: o aviso some.
        assert!(h.key(Action::Retry, 300));
        assert_eq!(h.c.model.banners.stuck, None);
        // E o jogo que fecha também apaga o aviso.
        h.ctl.set_stuck(true);
        h.tick(400);
        assert!(h.c.model.banners.stuck.is_some());
        h.ctl.set_exited(true);
        h.tick(500);
        assert_eq!(h.c.model.banners.stuck, None);
    }

    #[test]
    fn retry_opens_engine_when_diary_resolves() {
        let mut h = H::new();
        h.ctl.push_recover(blocked("não foi possível restaurar x"));
        h.c.start(&mut *h.draw);
        h.ctl.push_recover(blocked("ainda bloqueado"));
        h.ctl
            .set_catalog(vec![cheat("a", available()), cheat("b", available())]);
        h.connect(0);
        assert_eq!(connected(&h.c), (100, "v1".to_string(), true, false));
        // Desejados de antes: `a` da mesma versão, `b` de outra.
        h.c.set_desired("a", Some(desired_of(0, "v1")));
        h.c.set_desired("b", Some(desired_of(0, "v0")));
        h.tick(100);
        assert!(h.applies().is_empty(), "diário bloqueado: não escreve");

        h.ctl.clear_calls();
        assert!(h.key(Action::Retry, 1_000));
        assert_eq!(
            h.ctl.calls(),
            [
                Call::Recover {
                    busy: Some("recuperando o diário…".to_string())
                },
                Call::OpenEngine
            ]
        );
        assert_eq!(h.c.model.banners.blocked, None);
        assert_eq!(connected(&h.c), (100, "v1".to_string(), true, true));
        // A religação roda no `tick` seguinte, só para a mesma versão.
        assert!(h.applies().is_empty());
        h.tick(1_100);
        assert_eq!(h.applies(), [apply_call(&[("a", 0)], "religando…")]);
        assert!(h.row("a").on);
        assert!(!h.row("b").on);
        assert_eq!(
            event_count(&h, "b: versão nova; aperte espaço para ligar"),
            1
        );
    }

    #[test]
    fn retry_opens_engine_when_disk_becomes_readable() {
        let mut h = H::new();
        let mut bad = SearchSpec::new(vec![cheat("a", unavailable("disco ilegível"))]);
        bad.disk = Err("sem cabeçalho".to_string());
        h.ctl.set_default_search(bad);
        h.connect(0);
        assert_eq!(connected(&h.c), (100, "v1".to_string(), false, false));

        // O disco volta a ser legível; só o `r` refaz a busca.
        h.ctl
            .push_search(Ok(SearchSpec::new(vec![cheat("a", available())])));
        h.ctl.clear_calls();
        assert!(!h.key(Action::Retry, 1_000));
        assert!(h.wait(1_000));
        assert_eq!(connected(&h.c), (100, "v1".to_string(), true, true));
        assert!(h.ctl.calls().contains(&Call::OpenEngine));
        assert_eq!(h.row("a").avail, available());
        // O disco novo ficou na sessão (o `merge_search` do alvo falso o guarda).
        assert_eq!(h.ctl.merge_ids().len(), 1);
    }

    // ---- Tarefa 14: valores, teclas depois de uma operação e saída ----

    /// Cheat com valor na grade `grid()` (`0..=10`, padrão 2).
    fn valued(id: &str) -> CheatInfo {
        let mut c = cheat(id, available());
        c.value = Some(grid());
        c
    }

    fn set_call(id: &str, k: u64) -> Call {
        Call::SetValue {
            id: id.to_string(),
            k,
            busy: Some(format!("trocando o valor de {id}…")),
        }
    }

    fn swaps(h: &H) -> Vec<Call> {
        h.ctl
            .calls()
            .into_iter()
            .filter(|c| matches!(c, Call::SetValue { .. }))
            .collect()
    }

    #[test]
    fn value_queue_rate_limited_and_ends_on_last() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![valued("v")]);
        h.connect(0);
        assert!(h.toggle("v", 1_000));
        assert_eq!(h.row("v").applied_k, Some(2));

        // Uma tecla isolada vai na hora.
        assert!(h.key(Action::Inc, 2_000));
        assert_eq!(swaps(&h), [set_call("v", 3)]);
        assert_eq!(h.row("v").applied_k, Some(3));

        // Duas teclas dentro dos 150 ms só mudam o valor mostrado.
        assert!(!h.key(Action::Inc, 2_025));
        assert!(!h.key(Action::Inc, 2_050));
        assert_eq!(h.row("v").k, 5);
        assert_eq!(h.row("v").applied_k, Some(3));
        assert_eq!(h.c.desired().get("v").map(|d| d.k), Some(5));
        assert!(!h.tick(2_100));
        assert!(!h.tick(2_149));
        assert_eq!(swaps(&h), [set_call("v", 3)]);

        // Passado o intervalo, o `tick` manda direto para o último alvo (o 4 nunca passa).
        assert!(h.tick(2_150));
        assert_eq!(swaps(&h), [set_call("v", 3), set_call("v", 5)]);
        assert_eq!(h.row("v").applied_k, Some(5));
        assert_eq!(h.c.model.busy, None);
        // Trocas seguidas somam uma linha só.
        let arrows: Vec<&str> = h.events().into_iter().filter(|e| e.contains('→')).collect();
        // A linha mostra o valor de antes da primeira troca (2), não só do último par.
        assert_eq!(arrows, ["12:00 v: 2 → 5"]);

        // Nada mais a mandar.
        assert!(!h.tick(2_400));
        assert_eq!(swaps(&h).len(), 2);
        // Com o intervalo já passado, a tecla seguinte vai na hora.
        assert!(h.key(Action::Dec, 2_500));
        assert_eq!(swaps(&h).last(), Some(&set_call("v", 4)));
    }

    #[test]
    fn value_clamped_to_grid() {
        let mut h = H::new();
        h.ctl
            .set_catalog(vec![valued("v"), cheat("a", available())]);
        h.connect(0);
        h.sel("v");
        for _ in 0..15 {
            assert!(!h.key(Action::Inc, 100));
        }
        assert_eq!(h.row("v").k, 10);
        for _ in 0..15 {
            assert!(!h.key(Action::Dec, 200));
        }
        assert_eq!(h.row("v").k, 0);
        // Cheat sem valor: as setas não fazem nada.
        h.sel("a");
        assert!(!h.key(Action::Inc, 300));
        assert_eq!(h.row("a").k, 0);
        assert!(swaps(&h).is_empty());
    }

    #[test]
    fn value_clamped_to_grid_when_on() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![valued("v")]);
        h.connect(0);
        h.c.model.cheats[0].k = 10;
        assert!(h.toggle("v", 100));
        // No fim da grade a tecla não muda nada, então não há troca.
        assert!(!h.key(Action::Inc, 1_000));
        assert!(swaps(&h).is_empty());
        assert_eq!(h.row("v").k, 10);
    }

    #[test]
    fn value_change_while_off_only_updates_display() {
        let mut h = H::new();
        let mut gone = valued("u");
        gone.avail = not_found();
        h.ctl.set_catalog(vec![valued("v"), gone]);
        h.connect(0);
        let events = h.events().len();

        // Desligado e disponível.
        h.sel("v");
        assert!(!h.key(Action::Inc, 100));
        assert_eq!(h.row("v").k, 3);
        assert_eq!(h.row("v").applied_k, None);

        // Indisponível com a marca de religar pendente: o desejado acompanha o valor mostrado.
        h.c.set_desired("u", Some(desired_of(2, "v1")));
        h.sel("u");
        assert!(!h.key(Action::Dec, 200));
        assert_eq!(h.row("u").k, 1);
        assert_eq!(h.c.desired().get("u"), Some(&desired_of(1, "v1")));

        assert!(swaps(&h).is_empty());
        assert_eq!(h.events().len(), events, "nada vai para o registro");

        // O jogo fechou: as setas ainda mudam o valor mostrado, e o desejado o guarda.
        h.ctl.set_exited(true);
        h.tick(300);
        assert_eq!(h.c.model.conn, Conn::Waiting);
        assert!(!h.key(Action::Inc, 400));
        assert_eq!(h.row("u").k, 2);
        assert_eq!(h.c.desired().get("u").map(|d| d.k), Some(2));
    }

    #[test]
    fn value_failures_follow_table() {
        let mut h = H::new();
        h.ctl
            .set_catalog(vec![valued("a"), valued("b"), valued("c"), valued("d")]);
        h.connect(0);
        for id in ["a", "b", "c", "d"] {
            assert!(h.toggle(id, 100));
        }

        // Recusa limpa: o valor mostrado volta ao que está no jogo; continua ligado e desejado.
        h.sel("a");
        h.ctl
            .push_outcome(OpOutcome::Refused("threads no trecho".to_string()));
        assert!(h.key(Action::Inc, 1_000));
        let a = h.row("a");
        assert!(a.on);
        assert_eq!((a.k, a.applied_k), (2, Some(2)));
        assert_eq!(h.c.desired().get("a"), Some(&desired_of(2, "v1")));
        assert_eq!(h.events().last(), Some(&"12:00 a: threads no trecho"));
        assert_eq!(h.c.model.busy, None);
        // O alvo foi descartado: o `tick` não tenta de novo.
        assert!(!h.tick(1_500));
        assert_eq!(swaps(&h).len(), 1);

        // Bytes de outro: indisponível e fora dos desejados.
        h.sel("b");
        h.ctl.push_outcome(OpOutcome::Foreign);
        assert!(h.key(Action::Inc, 2_000));
        let b = h.row("b");
        assert!(!b.on && !b.desired);
        assert_eq!(
            b.status(&h.c.model.conn),
            crate::model::CheatStatus::Unavailable("alterado por outro".to_string())
        );
        assert!(!h.c.desired().contains_key("b"));

        // Restos: `erro`, aviso fixo e fora dos desejados.
        h.sel("c");
        h.ctl.push_residue("escrita", &["c"]);
        assert!(h.key(Action::Inc, 3_000));
        let c = h.row("c");
        assert!(c.residue && !c.on && !c.desired);
        assert_eq!(
            c.status(&h.c.model.conn),
            crate::model::CheatStatus::Residue
        );
        assert!(h.c.model.banners.residue.is_some());

        // O jogo fechou no meio da troca: o valor novo fica guardado no desejado.
        h.sel("d");
        h.ctl.push_outcome(OpOutcome::Gone);
        assert!(h.key(Action::Inc, 4_000));
        assert_eq!(h.c.model.conn, Conn::Waiting);
        let d = h.row("d");
        assert!(!d.on && d.desired);
        assert_eq!(d.k, 3);
        assert_eq!(h.c.desired().get("d"), Some(&desired_of(3, "v1")));
        assert_eq!(h.events().last(), Some(&"12:00 o jogo fechou"));
        assert_eq!(h.c.model.busy, None);
        // As ligadas que não falharam continuam desejadas.
        assert!(h.c.desired().contains_key("a"));
    }

    #[test]
    fn off_with_pending_value_skips_swap() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![valued("v")]);
        h.connect(0);
        assert!(h.toggle("v", 1_000));
        assert!(h.key(Action::Inc, 2_000));
        assert!(!h.key(Action::Inc, 2_050));
        assert_eq!(h.row("v").k, 4);
        assert_eq!(swaps(&h), [set_call("v", 3)]);

        // O espaço desliga direto: a troca pendente não vai para o jogo.
        h.ctl.clear_calls();
        assert!(h.toggle("v", 2_060));
        assert_eq!(
            h.ctl.calls(),
            [Call::Undo {
                id: "v".to_string(),
                busy: Some("desligando v…".to_string())
            }]
        );
        let v = h.row("v");
        assert!(!v.on && !v.desired);
        assert_eq!(
            (v.k, v.applied_k),
            (4, None),
            "o valor mostrado fica guardado"
        );
        assert!(!h.tick(2_500));
        assert!(swaps(&h).is_empty());

        // Ligar de novo usa o valor mostrado.
        h.ctl.clear_calls();
        assert!(h.toggle("v", 3_000));
        assert_eq!(h.applies(), [apply_call(&[("v", 4)], "ligando v…")]);
    }

    #[test]
    fn value_queue_survives_game_close() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![valued("v")]);
        h.connect(0);
        assert!(h.toggle("v", 1_000));
        assert!(h.key(Action::Inc, 2_000));
        assert!(!h.key(Action::Inc, 2_050));
        assert_eq!(h.row("v").k, 4);

        // O jogo fecha com a troca ainda na fila: o valor mostrado fica guardado.
        h.ctl.clear_calls();
        h.reopen(2_100, 5_000);
        assert!(!h.row("v").on);
        assert_eq!(h.row("v").k, 4);
        assert_eq!(h.c.desired().get("v"), Some(&desired_of(4, "v1")));
        assert!(
            swaps(&h).is_empty(),
            "a troca da fila não vai para o jogo novo"
        );

        // A religação usa o valor guardado.
        assert!(h.tick(5_100));
        assert_eq!(h.applies(), [apply_call(&[("v", 4)], "religando…")]);
        assert_eq!(h.row("v").applied_k, Some(4));

        // Uma troca interrompida pelo fechamento guarda o valor novo.
        h.ctl.push_outcome(OpOutcome::Gone);
        assert!(h.key(Action::Inc, 6_000));
        assert_eq!(h.c.model.conn, Conn::Waiting);
        assert_eq!(h.c.desired().get("v"), Some(&desired_of(5, "v1")));
        assert_eq!(h.row("v").k, 5);
    }

    #[test]
    fn value_regridded_on_new_version() {
        let mut h = H::new();
        let mut shrunk = grid();
        shrunk.max = 5.0;
        h.ctl
            .set_catalog(vec![valued("v"), valued("w"), valued("s"), {
                let mut x = cheat("x", available());
                x.value = Some(grid());
                x
            }]);
        h.connect(0);
        assert!(h.toggle("v", 1_000));
        for i in 0..5 {
            assert!(h.key(Action::Inc, 2_000 + 1_000 * i));
        }
        assert_eq!(h.row("v").k, 7);
        h.c.model.cheats[1].k = 4; // w
        h.c.model.cheats[2].k = 6; // s
        h.c.model.cheats[3].k = 10; // x

        // O jogo atualizou: `v` e `w` ganham outra grade; `x` uma grade menor; `s` fica igual.
        let mut wide = grid();
        wide.max = 100.0;
        wide.step = 10.0;
        wide.default_k = 1;
        let mut v2 = cheat("v", available());
        v2.value = Some(wide.clone());
        let mut w2 = cheat("w", available());
        w2.value = Some(wide);
        let mut x2 = cheat("x", available());
        x2.value = Some(shrunk);
        let mut spec = SearchSpec::new(vec![v2, w2, valued("s"), x2]);
        spec.version_key = "v2".to_string();
        h.ctl.set_default_search(spec);
        h.ctl.clear_calls();
        h.reopen(9_000, 12_000);

        // O valor antigo (7) vai ao ponto mais próximo da grade nova (10), e o desejado o guarda.
        assert_eq!(h.row("v").k, 1);
        assert_eq!(h.c.desired().get("v"), Some(&desired_of(1, "v1")));
        assert!(
            h.events()
                .iter()
                .any(|e| e.contains("v:") && e.contains("usando 10"))
        );
        // Sem religar sozinho: a versão é outra.
        h.tick(12_100);
        assert!(h.applies().is_empty());
        assert_eq!(
            event_count(&h, "v: versão nova; aperte espaço para ligar"),
            1
        );
        // Quem não era desejado também leva o valor para a grade nova.
        assert_eq!(h.row("w").k, 0);
        assert!(
            h.events()
                .iter()
                .any(|e| e.contains("w:") && e.contains("usando 0"))
        );
        // Grade menor: limitado ao fim.
        assert_eq!(h.row("x").k, 5);
        assert!(
            h.events()
                .iter()
                .any(|e| e.contains("x:") && e.contains("usando 5"))
        );
        // Grade igual: nada muda e nada é dito.
        assert_eq!(h.row("s").k, 6);
        assert_eq!(event_count(&h, "s:"), 0);

        // O espaço liga com o valor guardado.
        assert!(h.toggle("v", 12_200));
        assert_eq!(h.applies(), [apply_call(&[("v", 1)], "ligando v…")]);
    }

    #[test]
    fn after_op_keys_keeps_only_quit_and_arrows() {
        let mut h = H::new();
        let keys = vec![
            Action::Up,
            Action::Down,
            Action::Toggle,
            Action::Dec,
            Action::Retry,
            Action::Quit,
            Action::Inc,
            Action::Toggle,
        ];
        assert_eq!(
            h.c.after_op_keys(keys),
            [Action::Dec, Action::Quit, Action::Inc]
        );
        assert!(h.c.after_op_keys(Vec::new()).is_empty());
        assert!(
            h.c.after_op_keys(vec![Action::Retry, Action::Up])
                .is_empty()
        );
    }

    #[test]
    fn tick_reports_session_calls() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![cheat("a", available())]);
        h.connect(0);
        assert!(h.toggle("a", 100));
        assert!(!h.tick(200), "ocioso: nenhuma chamada");
        h.reopen(1_000, 4_000);
        assert!(h.tick(4_100), "a religação chamou a sessão");
        assert!(!h.tick(4_200));
        // Sem sessão: nunca.
        h.ctl.set_exited(true);
        assert!(!h.tick(4_300));
        assert!(!h.tick(9_000));
    }

    #[test]
    fn shutdown_does_not_wait_for_search() {
        let mut h = H::new();
        h.ctl
            .set_catalog(vec![cheat("a", available()), cheat("b", not_found())]);
        h.connect(0);
        assert!(h.toggle("a", 100));
        h.ctl.set_undo_all(vec!["a".to_string()], Vec::new());

        // Uma nova busca que dorme 10 s.
        h.ctl.set_search_delay(Duration::from_secs(10));
        assert!(h.c.start_refresh(h.at(5_000)));
        assert!(h.searching());

        h.ctl.clear_calls();
        let started = Instant::now();
        let out = h.c.shutdown(&mut *h.draw);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "o shutdown esperou a busca: {:?}",
            started.elapsed()
        );
        assert_eq!(out, (vec!["a".to_string()], Vec::new()));
        assert!(!h.searching(), "a busca foi abandonada");
        // `restaurando…` foi desenhado antes do desfazer, e a sessão continua (o `main` a solta).
        assert_eq!(
            h.ctl.calls(),
            [Call::UndoAll {
                busy: Some("restaurando…".to_string())
            }]
        );
        assert_eq!(
            h.ctl.drawn().and_then(|m| m.busy),
            Some("restaurando…".to_string())
        );
    }

    #[test]
    fn shutdown_without_session_returns_nothing() {
        let mut h = H::new();
        h.ctl.set_find(Find::Found(100));
        h.ctl.hold_search();
        h.tick(0);
        assert_eq!(h.c.model.conn, Conn::Connecting);
        let started = Instant::now();
        assert_eq!(h.c.shutdown(&mut *h.draw), (Vec::new(), Vec::new()));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(!h.searching());
        assert_eq!(h.ctl.count(|c| matches!(c, Call::UndoAll { .. })), 0);
        h.ctl.release_search();
    }

    // ---- cheats de valor (spec 005, plano 005 tarefa 16) ----

    use crate::backend::ReflectionNews;
    use crate::fake::field;
    use gt_grounded2::fields::{Note as FieldNote, TickReport};
    use gt_grounded2::player::Absent;

    impl H {
        fn wait_job(&mut self, ms: u64) -> bool {
            let now = self.at(ms);
            self.c.wait_job(now, WAIT)
        }
    }

    fn report(player: PlayerState, notes: &[(&str, FieldNote)]) -> TickReport {
        TickReport {
            player,
            notes: notes.iter().map(|(id, n)| (id.to_string(), *n)).collect(),
            events: Vec::new(),
            pawn_null_since: None,
            world: None,
        }
    }

    fn retry_news() -> ReflectionNews {
        ReflectionNews {
            index_ok: false,
            retry: true,
            player_changed: None,
        }
    }

    /// Conectado em 0 com um patch e o cheat de valor `f` (disponível pela sessão).
    fn with_field() -> H {
        let mut h = H::new();
        h.ctl
            .set_catalog(vec![cheat("p", available()), field("f", Avail::Checking)]);
        h.ctl.set_field_avail(&[("f", available())]);
        h.connect(0);
        h
    }

    #[test]
    fn stale_reflection_outcome_dropped() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![field("f", Avail::Checking)]);
        h.ctl.set_jobs(&[JobKind::Index]);
        h.ctl.hold_jobs();
        h.connect(0);
        h.tick(0);
        assert!(h.c.job.is_some());
        // O jogo fecha e reabre com o índice da conexão anterior ainda rodando.
        h.reopen(100, 3_000);
        h.ctl.release_jobs();
        h.tick(3_000);
        assert!(h.wait_job(3_000));
        // Só o resultado da sessão nova entrou; o da antiga foi abandonado.
        let merges: Vec<u64> = h
            .ctl
            .calls()
            .iter()
            .filter_map(|c| match c {
                Call::MergeReflection { session, .. } => Some(*session),
                _ => None,
            })
            .collect();
        assert_eq!(merges, vec![2]);
    }

    #[test]
    fn index_retry_interval_and_window() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![field("f", Avail::Checking)]);
        h.ctl.set_jobs(&[JobKind::Index]);
        h.ctl.set_job_took(Duration::from_secs(10));
        for _ in 0..20 {
            h.ctl.push_news(retry_news());
        }
        h.connect(0);
        // Intervalo: max(10 s, 2 × 10 s) = 20 s, por 120 s depois da conexão.
        let mut runs_at = Vec::new();
        for t in (0..=130_000).step_by(1_000) {
            h.tick(t);
            if h.c.job.is_some() {
                runs_at.push(t);
                assert!(h.wait_job(t));
            }
        }
        assert_eq!(runs_at, vec![0, 20_000, 40_000, 60_000, 80_000, 100_000]);
        // O `r` reinicia o prazo e tenta já.
        h.key(Action::Retry, 131_000);
        h.tick(131_000);
        assert!(h.c.job.is_some());
    }

    #[test]
    fn controller_job_only_with_field_desired() {
        let mut h = with_field();
        h.ctl.set_jobs(&[JobKind::Index, JobKind::Controller]);
        h.ctl.set_needs_controller(true);
        h.tick(0);
        assert!(h.wait_job(0));
        assert_eq!(h.ctl.merges(JobKind::Index), 1);
        assert_eq!(h.row("f").avail, available());
        // Sem cheat de valor desejado, ninguém procura o controller.
        h.tick(100);
        assert!(h.c.job.is_none());
        assert!(h.toggle("f", 200));
        h.tick(300);
        assert!(h.wait_job(300));
        assert_eq!(h.ctl.job_runs(JobKind::Controller), 1);
    }

    #[test]
    fn field_on_while_checking_refused() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![field("f", Avail::Checking)]);
        h.ctl.set_field_avail(&[("f", Avail::Checking)]);
        h.connect(0);
        assert!(!h.toggle("f", 100));
        assert!(
            h.events()
                .contains(&"12:00 f: verificando a reflexão do jogo; tente de novo em instantes")
        );
        assert_eq!(h.ctl.count(|c| matches!(c, Call::FieldOn { .. })), 0);
        assert!(!h.row("f").on);
    }

    #[test]
    fn maintenance_not_an_operation() {
        let mut h = with_field();
        assert!(h.toggle("f", 100));
        let ops = h.c.op_counts.clone();
        h.ctl.push_tick(report(PlayerState::Found, &[]));
        // A manutenção chama a sessão, mas o `tick` não conta como operação.
        assert!(!h.tick(200));
        assert_eq!(h.ctl.field_ticks(), 1);
        assert_eq!(h.c.op_counts, ops);
        assert!(h.ctl.calls().contains(&Call::FieldTick {
            on: vec!["f".to_string()]
        }));
    }

    #[test]
    fn maintenance_suspended_when_stuck_or_no_engine() {
        let mut h = with_field();
        assert!(h.toggle("f", 100));
        h.ctl.set_stuck(true);
        h.tick(200);
        assert_eq!(h.ctl.field_ticks(), 0);
        h.ctl.set_stuck(false);
        h.tick(300);
        assert_eq!(h.ctl.field_ticks(), 1);

        // Sem motor (disco ilegível), nada.
        let mut h = H::new();
        let mut spec = SearchSpec::new(vec![field("f", Avail::Checking)]);
        spec.disk = Err("disco ilegível".to_string());
        h.ctl.set_default_search(spec);
        h.ctl.set_field_avail(&[("f", available())]);
        h.connect(0);
        h.c.model.cheats[0].on = true;
        h.tick(100);
        assert_eq!(h.ctl.field_ticks(), 0);
    }

    #[test]
    fn player_events_only_on_transition() {
        let mut h = with_field();
        assert!(h.toggle("f", 100));
        let absent = || PlayerState::Absent(Absent::PawnNull);
        for (i, player) in [
            PlayerState::Found,
            PlayerState::Found,
            absent(),
            absent(),
            PlayerState::Found,
        ]
        .into_iter()
        .enumerate()
        {
            h.ctl.push_tick(report(player, &[]));
            h.tick(200 + i as u64 * 100);
        }
        assert_eq!(event_count(&h, "personagem encontrado"), 2);
        assert_eq!(event_count(&h, "personagem perdido"), 1);
        assert_eq!(h.c.model.player, PlayerLine::Found);
        h.ctl.push_tick(report(absent(), &[]));
        h.tick(800);
        assert_eq!(h.c.model.player, PlayerLine::NotFound);
    }

    #[test]
    fn reflexao_nao_reconhecida_keeps_state_and_auto_searches() {
        let unrecognized = || unavailable(REFLECTION_UNRECOGNIZED);
        assert!(keeps_state(&available(), &unrecognized()));
        assert!(!keeps_state(&unrecognized(), &unrecognized()));

        let mut h = H::new();
        h.ctl.set_catalog(vec![field("f", unrecognized())]);
        h.ctl.set_field_avail(&[("f", unrecognized())]);
        h.connect(0);
        // Sem nada desejado, a nova busca automática acontece, com as globais (`@ue`).
        h.tick(5_000);
        assert!(h.wait(5_000));
        let ids = h.ctl.search_ids();
        assert_eq!(ids.len(), 2);
        assert!(ids[1].as_ref().unwrap().contains(&UE_ID.to_string()));
    }

    #[test]
    fn field_relinked_after_reconnect_same_version() {
        let mut h = with_field();
        assert!(h.toggle("f", 100));
        assert!(h.c.desired().contains_key("f"));
        h.reopen(200, 3_000);
        h.tick(3_100);
        let ons: Vec<Call> = h
            .ctl
            .calls()
            .into_iter()
            .filter(|c| matches!(c, Call::FieldOn { .. }))
            .collect();
        assert_eq!(ons.len(), 2);
        assert_eq!(
            ons[1],
            Call::FieldOn {
                id: "f".to_string(),
                k: 0,
                busy: Some("religando…".to_string())
            }
        );
        assert!(h.row("f").on);
    }

    #[test]
    fn field_on_absent_player_waits() {
        let mut h = with_field();
        assert!(h.toggle("f", 100));
        assert!(h.row("f").on);
        h.ctl.push_tick(report(
            PlayerState::Absent(Absent::NoController),
            &[("f", FieldNote::Waiting)],
        ));
        h.tick(200);
        assert_eq!(h.row("f").note, Some(FieldNote::Waiting));
        assert_eq!(h.c.model.player, PlayerLine::NotFound);
        // Achado, a marca some.
        h.ctl.push_tick(report(PlayerState::Found, &[]));
        h.tick(300);
        assert_eq!(h.row("f").note, None);
    }

    #[test]
    fn world_note_only_on_world_rows() {
        let mut h = H::new();
        h.ctl.set_catalog(vec![
            cheat("p", available()),
            field("a", Avail::Checking),
            field("b", Avail::Checking),
            field("c", Avail::Checking),
        ]);
        h.ctl
            .set_field_avail(&[("a", available()), ("b", available()), ("c", available())]);
        h.connect(0);
        assert!(h.toggle("a", 100));
        assert!(h.toggle("b", 150));
        assert!(h.toggle("c", 200));
        // Personagem achado, mas o mundo some em duas linhas: a marca só vai a elas.
        h.ctl.push_tick(report(
            PlayerState::Found,
            &[
                ("a", FieldNote::WaitingWorld),
                ("b", FieldNote::WaitingWorld),
            ],
        ));
        h.tick(300);
        assert_eq!(h.row("a").note, Some(FieldNote::WaitingWorld));
        assert_eq!(h.row("b").note, Some(FieldNote::WaitingWorld));
        assert_eq!(h.row("c").note, None);
        assert_eq!(h.c.model.player, PlayerLine::Found);
        // Corrida de um ciclo: personagem achado e o mundo com o controller inválido.
        let mut r = report(PlayerState::Found, &[("a", FieldNote::WaitingWorld)]);
        r.world = Some(gt_grounded2::fields::WorldState::Absent(
            gt_grounded2::player::GameStateAbsent::ControllerInvalid,
        ));
        h.ctl.push_tick(r);
        h.tick(400);
        assert_eq!(h.row("a").note, Some(FieldNote::WaitingWorld));
        assert_eq!(h.row("b").note, None);
        assert_eq!(h.c.model.player, PlayerLine::Found);
    }

    #[test]
    fn field_off_absent_player_succeeds() {
        let mut h = with_field();
        assert!(h.toggle("f", 100));
        h.ctl.push_tick(report(
            PlayerState::Absent(Absent::PawnNull),
            &[("f", FieldNote::Waiting)],
        ));
        h.tick(200);
        assert!(h.toggle("f", 300));
        assert!(h.ctl.calls().contains(&Call::FieldOff {
            id: "f".to_string(),
            busy: Some("desligando f…".to_string())
        }));
        assert!(!h.row("f").on);
        assert_eq!(h.row("f").note, None);
        assert!(!h.c.desired().contains_key("f"));
        // Nada ligado: a linha segue o controller guardado (o último ciclo não o achou).
        h.tick(400);
        assert_eq!(h.c.model.player, PlayerLine::NotFound);
    }

    // ---- Tarefa 10 do plano 006: valor ajustável nos cheats de valor ----

    /// Cheat de valor com a grade `grid()` (`0..=10`, padrão 2).
    fn vfield(id: &str) -> CheatInfo {
        let mut c = field(id, Avail::Checking);
        c.value = Some(grid());
        c
    }

    /// Conectado em 0 com o cheat de valor `f` (grade `grid()`), disponível pela sessão.
    fn with_value_field() -> H {
        let mut h = H::new();
        h.ctl.set_catalog(vec![vfield("f")]);
        h.ctl.set_field_avail(&[("f", available())]);
        h.connect(0);
        h
    }

    fn field_ons(h: &H) -> Vec<Call> {
        h.ctl
            .calls()
            .into_iter()
            .filter(|c| matches!(c, Call::FieldOn { .. }))
            .collect()
    }

    fn field_on_call(k: u64, busy: &str) -> Call {
        Call::FieldOn {
            id: "f".to_string(),
            k,
            busy: Some(busy.to_string()),
        }
    }

    #[test]
    fn field_value_arrows_off_and_on() {
        let mut h = with_value_field();
        assert_eq!(h.row("f").k, 2);
        // Desligado, as setas só mudam o valor mostrado.
        assert!(!h.key(Action::Inc, 100));
        assert_eq!(h.row("f").k, 3);
        assert!(swaps(&h).is_empty());
        // Ligar usa o valor mostrado.
        assert!(h.toggle("f", 200));
        assert_eq!(field_ons(&h), [field_on_call(3, "ligando f…")]);
        assert_eq!(h.row("f").applied_k, Some(3));
        assert_eq!(h.c.desired().get("f").map(|d| d.k), Some(3));

        // Ligado: uma tecla isolada vai na hora.
        assert!(h.key(Action::Inc, 2_000));
        assert_eq!(swaps(&h), [set_call("f", 4)]);
        // Dentro dos 150 ms só muda o mostrado; passado o intervalo, vai o último valor.
        assert!(!h.key(Action::Inc, 2_025));
        assert!(!h.key(Action::Inc, 2_050));
        assert_eq!(h.row("f").k, 6);
        assert!(!h.tick(2_149));
        assert!(h.tick(2_150));
        assert_eq!(swaps(&h), [set_call("f", 4), set_call("f", 6)]);
        assert_eq!(h.row("f").applied_k, Some(6));
        assert_eq!(h.c.desired().get("f").map(|d| d.k), Some(6));
        assert!(!h.tick(2_400));
        assert_eq!(swaps(&h).len(), 2);
    }

    #[test]
    fn field_relink_uses_shown_value() {
        let mut h = with_value_field();
        assert!(h.toggle("f", 100));
        assert!(h.key(Action::Inc, 2_000));
        assert_eq!(h.row("f").k, 3);
        // O jogo fecha; as setas seguem mudando o mostrado (e o desejado).
        h.ctl.set_exited(true);
        h.tick(2_100);
        h.ctl.set_exited(false);
        assert!(!h.key(Action::Inc, 2_200));
        assert_eq!(h.row("f").k, 4);
        h.connect(4_200);
        h.tick(4_300);
        let ons = field_ons(&h);
        assert_eq!(ons.len(), 2);
        assert_eq!(ons[1], field_on_call(4, "religando…"));
        assert!(h.row("f").on);
        assert_eq!(h.row("f").applied_k, Some(4));
    }

    #[test]
    fn field_swap_during_checking_keeps_shown_value() {
        let mut h = with_value_field();
        assert!(h.toggle("f", 100));
        let before = h.c.model.events.len();
        // A sessão ainda não tem o índice: recusa a troca com `verificando…`.
        h.ctl
            .push_outcome(OpOutcome::Refused(crate::model::CHECKING.to_string()));
        assert!(h.key(Action::Inc, 2_000));
        // O valor mostrado e o desejado ficam; nada vai ao registro.
        assert_eq!(h.row("f").k, 3);
        assert_eq!(h.c.desired().get("f").map(|d| d.k), Some(3));
        assert_eq!(h.c.model.events.len(), before);
        assert!(!h.events().iter().any(|e| e.contains("verificando")));
        // Sem repetir a troca a cada volta.
        assert!(!h.tick(2_200));
        assert!(!h.tick(2_400));
        assert_eq!(swaps(&h).len(), 1);
    }

    #[test]
    fn set_maintenance_with_new_original_keeps_keys() {
        let mut h = with_value_field();
        assert!(h.toggle("f", 100));
        let ops = h.c.op_counts.clone();
        let mut r = report(PlayerState::Found, &[]);
        r.events = vec!["f: o jogo mudou o valor para 7; novo original".to_string()];
        h.ctl.push_tick(r);
        // Não é operação: o `tick` não chama o laço de teclas a descartar e as buscas valem.
        assert!(!h.tick(200));
        assert_eq!(h.c.op_counts, ops);
        assert_eq!(
            event_count(&h, "f: o jogo mudou o valor para 7; novo original"),
            1
        );
        assert_eq!(
            h.c.after_op_keys(vec![Action::Inc, Action::Dec, Action::Quit]),
            [Action::Inc, Action::Dec, Action::Quit]
        );
        // Uma busca iniciada antes ainda vale para este cheat.
        assert!(h.row("f").on);
    }

    #[test]
    fn game_refuses_arrows_only_change_shown() {
        let mut h = with_value_field();
        assert!(h.toggle("f", 100));
        h.ctl
            .push_tick(report(PlayerState::Found, &[("f", FieldNote::GameRefuses)]));
        h.tick(200);
        assert_eq!(h.row("f").note, Some(FieldNote::GameRefuses));
        // Com a marca, as setas só mudam o valor mostrado, sem trocar nada no jogo.
        assert!(!h.key(Action::Inc, 5_000));
        assert!(!h.key(Action::Inc, 5_300));
        assert_eq!(h.row("f").k, 4);
        assert_eq!(h.c.desired().get("f").map(|d| d.k), Some(4));
        h.ctl
            .push_tick(report(PlayerState::Found, &[("f", FieldNote::GameRefuses)]));
        assert!(!h.tick(5_600));
        assert!(swaps(&h).is_empty());
        // A marca some: o valor mostrado vai ao jogo.
        h.ctl.push_tick(report(PlayerState::Found, &[]));
        assert!(!h.tick(5_700));
        assert_eq!(h.row("f").note, None);
        assert!(h.tick(5_800));
        assert_eq!(swaps(&h), [set_call("f", 4)]);
    }

    #[test]
    fn game_refuses_cleared_on_reconnect() {
        let mut h = with_value_field();
        assert!(h.toggle("f", 100));
        h.ctl
            .push_tick(report(PlayerState::Found, &[("f", FieldNote::GameRefuses)]));
        h.tick(200);
        assert_eq!(h.row("f").note, Some(FieldNote::GameRefuses));
        h.reopen(300, 3_000);
        assert_eq!(h.row("f").note, None);
        h.tick(3_100);
        assert!(h.row("f").on);
        assert_eq!(h.row("f").note, None);
        // De novo as setas trocam o valor no jogo.
        assert!(h.key(Action::Inc, 4_000));
        assert_eq!(swaps(&h), [set_call("f", 3)]);
    }

    #[test]
    fn set_maintenance_suspended_stuck_or_pending() {
        let mut h = with_value_field();
        assert!(h.toggle("f", 100));
        h.ctl.set_stuck(true);
        h.tick(200);
        assert_eq!(h.ctl.field_ticks(), 0);
        h.ctl.set_stuck(false);
        h.tick(300);
        assert_eq!(h.ctl.field_ticks(), 1);

        // Resto pendente do `f`: a linha vira erro, sai da manutenção e nada é criado para ele.
        h.ctl.set_pending(&["f"]);
        h.tick(400);
        assert!(h.row("f").residue);
        assert!(!h.row("f").on);
        let ticks = h.ctl.field_ticks();
        h.tick(500);
        h.tick(600);
        assert_eq!(h.ctl.field_ticks(), ticks);
        assert!(h.ctl.calls().iter().all(|c| match c {
            Call::FieldTick { on } => !on.is_empty(),
            _ => true,
        }));
    }

    #[test]
    fn set_turn_off_failure_marks_error_blocks_id() {
        let mut h = with_value_field();
        assert!(h.toggle("f", 100));
        h.ctl.push_residue("escrita recusada", &["f"]);
        assert!(h.toggle("f", 200));
        assert!(h.row("f").residue);
        assert!(!h.row("f").on);
        assert!(!h.c.desired().contains_key("f"));
        assert!(h.c.model.banners.residue.is_some());
        assert_eq!(
            event_count(&h, "f: falhou deixando restos no jogo (escrita recusada)"),
            1
        );
        // Com o resto pendente, nada novo é criado para o id: espaço restaura, não liga.
        assert!(h.toggle("f", 300));
        assert_eq!(field_ons(&h).len(), 1);
        assert!(
            h.ctl
                .calls()
                .iter()
                .any(|c| matches!(c, Call::RestorePending { .. }))
        );
        // A manutenção não roda sem cheat de valor ligado.
        let ticks = h.ctl.field_ticks();
        h.tick(400);
        assert_eq!(h.ctl.field_ticks(), ticks);
    }
    // ---- Tarefa 11 do plano 006: a ação de somar ----

    /// Ação `ciencia` (grade `grid()`), no catálogo como `[[field]]` com `rule = "add"`.
    fn action_info(avail: Avail) -> CheatInfo {
        let mut c = field("ciencia", avail);
        c.name = "Somar ciência".to_string();
        c.value = Some(grid());
        c.action = true;
        c
    }

    /// Conectado em 0 com a ação disponível; o índice e a procura do controller são do fake.
    fn with_action() -> H {
        let mut h = H::new();
        h.ctl.set_catalog(vec![action_info(Avail::Checking)]);
        h.ctl.set_field_avail(&[("ciencia", available())]);
        h.connect(0);
        h
    }

    fn done(before: i32, after: i32) -> ActionOutcome {
        ActionOutcome::Done { before, after }
    }

    fn press(h: &mut H, ms: u64) -> bool {
        h.toggle("ciencia", ms)
    }

    #[test]
    fn action_executes_once_and_logs() {
        let mut h = with_action();
        h.ctl.push_action(done(7400, 8400));
        assert!(press(&mut h, 100));
        assert_eq!(h.ctl.actions(), 1);
        assert_eq!(event_count(&h, "Somar ciência: 7400 → 8400"), 1);
        assert_eq!(h.c.op_counts.get("ciencia"), Some(&1));
        assert!(!h.row("ciencia").on);
        assert!(h.c.desired().is_empty());
        // Uma corrida na releitura também escreveu: conta e abre a janela.
        h.ctl.push_action(ActionOutcome::Raced { read: 123 });
        assert!(press(&mut h, 1_200));
        assert_eq!(
            event_count(
                &h,
                "Somar ciência: o jogo mudou o valor durante a escrita (lido 123)"
            ),
            1
        );
        assert!(!press(&mut h, 1_300));
        assert_eq!(h.ctl.actions(), 2);
    }

    #[test]
    fn action_key_repeat_one_per_second() {
        let mut h = with_action();
        let mut called = 0;
        for i in 1..=20u64 {
            if press(&mut h, i * 100) {
                called += 1;
            }
        }
        assert_eq!(called, 2);
        assert_eq!(h.ctl.actions(), 2);
        assert_eq!(event_count(&h, "Somar ciência: aguarde"), 2);
    }

    #[test]
    fn action_refusal_does_not_open_window() {
        let mut h = with_action();
        h.ctl.push_action(ActionOutcome::RefusedAfterRead(
            "passaria do máximo".to_string(),
        ));
        // Leu o jogo: é operação (descarta teclas), mas não abre a janela.
        assert!(press(&mut h, 100));
        assert_eq!(event_count(&h, "Somar ciência: passaria do máximo"), 1);
        assert_eq!(h.c.op_counts.get("ciencia"), Some(&1));
        assert!(press(&mut h, 200));
        assert_eq!(h.ctl.actions(), 2);
        assert_eq!(h.c.op_counts.get("ciencia"), Some(&2));
        assert_eq!(event_count(&h, "Somar ciência: aguarde"), 0);

        // Recusa dos passos 1 e 2: não é operação.
        let mut h = with_action();
        h.ctl
            .push_action(ActionOutcome::Refused("estado do jogo ausente".to_string()));
        assert!(!press(&mut h, 100));
        assert_eq!(event_count(&h, "Somar ciência: estado do jogo ausente"), 1);
        assert_eq!(h.c.op_counts.get("ciencia"), None);
    }

    #[test]
    fn action_refusal_lines_throttled() {
        let mut h = with_action();
        let refuse = |h: &H, reason: &str| {
            h.ctl
                .push_action(ActionOutcome::Refused(reason.to_string()))
        };
        for ms in [100, 200, 300] {
            refuse(&h, "passaria do máximo");
            press(&mut h, ms);
        }
        refuse(&h, "valor atual fora da faixa");
        press(&mut h, 400);
        assert_eq!(event_count(&h, "passaria do máximo"), 1);
        assert_eq!(event_count(&h, "valor atual fora da faixa"), 1);
        // Passado um segundo, o mesmo motivo volta ao registro.
        refuse(&h, "passaria do máximo");
        press(&mut h, 1_200);
        assert_eq!(event_count(&h, "passaria do máximo"), 2);
    }

    #[test]
    fn action_refusal_not_an_op() {
        let mut h = with_action();
        h.ctl
            .push_action(ActionOutcome::Refused("estado do jogo ausente".to_string()));
        assert!(!press(&mut h, 100));
        h.ctl.push_action(ActionOutcome::NeedsController);
        assert!(!press(&mut h, 200));
        // Descarte por `aguarde` também não conta.
        h.ctl.push_action(done(1, 2));
        assert!(press(&mut h, 300));
        assert!(!press(&mut h, 400));
        assert_eq!(h.c.op_counts.get("ciencia"), Some(&1));
    }

    #[test]
    fn action_not_desired_not_relinked() {
        let mut h = with_action();
        assert!(press(&mut h, 100));
        assert!(h.c.desired().is_empty());
        assert!(!h.row("ciencia").desired);
        h.reopen(200, 3_000);
        h.tick(3_100);
        h.tick(3_200);
        assert!(h.c.desired().is_empty());
        assert!(!h.row("ciencia").on);
        assert_eq!(h.ctl.count(|c| matches!(c, Call::FieldOn { .. })), 0);
        assert_eq!(h.ctl.field_ticks(), 0);
        assert_eq!(h.ctl.actions(), 1);
    }

    #[test]
    fn action_untouched_on_exit_and_recovery() {
        let mut h = with_action();
        assert!(press(&mut h, 100));
        // Saída: desfaz o que há para desfazer e não toca na ação.
        let (restored, foreign) = h.c.shutdown(&mut *h.draw);
        assert!(restored.is_empty() && foreign.is_empty());
        assert_eq!(h.ctl.actions(), 1);
        assert_eq!(
            h.ctl
                .count(|c| matches!(c, Call::FieldOn { .. } | Call::FieldOff { .. })),
            0
        );
        // Recuperação (reconexão): a ação segue sem chamadas novas.
        let mut h = with_action();
        assert!(press(&mut h, 100));
        h.reopen(200, 3_000);
        h.tick(3_100);
        assert_eq!(h.ctl.actions(), 1);
        assert_eq!(
            h.ctl
                .count(|c| matches!(c, Call::FieldOn { .. } | Call::FieldOff { .. })),
            0
        );
    }

    /// Conectado com a ação, índice montado e a procura do controller disponível no fake.
    fn with_action_search() -> H {
        let mut h = with_action();
        h.ctl.set_jobs(&[JobKind::Index, JobKind::Controller]);
        h.ctl.set_needs_controller(true);
        h.tick(0);
        assert!(h.wait_job(0));
        h
    }

    #[test]
    fn action_triggers_controller_search() {
        let mut h = with_action_search();
        // Sem cheat de valor desejado nem ação pedida, ninguém procura o controller.
        h.tick(50);
        assert!(h.c.job.is_none());
        h.ctl.push_action(ActionOutcome::NeedsController);
        assert!(!press(&mut h, 100));
        assert_eq!(event_count(&h, "Somar ciência: procurando o personagem"), 1);
        assert!(matches!(&h.c.job, Some(j) if j.kind == JobKind::Controller));
        assert!(h.wait_job(100));
        assert_eq!(h.ctl.job_runs(JobKind::Controller), 1);
        // Procura terminada: nenhuma nova roda sem nova tecla, mesmo passado o intervalo.
        h.tick(5_000);
        h.tick(10_200);
        h.tick(20_000);
        assert!(h.c.job.is_none());
        assert_eq!(h.ctl.job_runs(JobKind::Controller), 1);
        // Uma nova tecla, já fora do intervalo, procura de novo.
        h.ctl.push_action(ActionOutcome::NeedsController);
        assert!(!press(&mut h, 20_100));
        assert!(h.wait_job(20_100));
        assert_eq!(h.ctl.job_runs(JobKind::Controller), 2);
    }

    #[test]
    fn action_within_interval_says_wait() {
        let mut h = with_action_search();
        h.ctl.push_action(ActionOutcome::NeedsController);
        press(&mut h, 100);
        assert!(h.wait_job(100));
        h.ctl.push_action(ActionOutcome::NeedsController);
        assert!(!press(&mut h, 200));
        assert_eq!(event_count(&h, "Somar ciência: aguarde a procura"), 1);
        assert!(h.c.job.is_none());
        assert_eq!(h.ctl.job_runs(JobKind::Controller), 1);
    }

    #[test]
    fn player_line_follows_stored_controller() {
        let mut h = with_action_search();
        assert_eq!(h.c.model.player, PlayerLine::Idle);
        h.ctl.push_action(ActionOutcome::NeedsController);
        press(&mut h, 100);
        assert!(h.wait_job(100));
        h.ctl.set_controller_news(Some(true));
        h.tick(200);
        assert_eq!(h.c.model.player, PlayerLine::Found);
        // O controller guardado caiu: a ação pede a procura e a linha passa a `não encontrado`.
        h.ctl.push_action(ActionOutcome::NeedsController);
        press(&mut h, 300);
        assert_eq!(h.c.model.player, PlayerLine::NotFound);
        h.tick(400);
        assert_eq!(h.c.model.player, PlayerLine::NotFound);
    }

    #[test]
    fn player_line_after_last_cheat_off_follows_stored_controller() {
        let mut h = with_field();
        assert!(h.toggle("f", 100));
        h.ctl.push_tick(report(PlayerState::Found, &[]));
        h.tick(200);
        assert_eq!(h.c.model.player, PlayerLine::Found);
        assert!(h.toggle("f", 300));
        h.tick(400);
        h.tick(500);
        assert_eq!(h.c.model.player, PlayerLine::Found);
    }

    #[test]
    fn controller_news_event_and_player_line() {
        let mut h = with_action_search();
        assert_eq!(h.c.model.player, PlayerLine::Idle);
        h.ctl.push_action(ActionOutcome::NeedsController);
        press(&mut h, 100);
        assert!(h.wait_job(100));
        h.ctl.set_controller_news(Some(true));
        h.tick(200);
        assert_eq!(
            event_count(&h, "personagem encontrado: aperte de novo para somar"),
            1
        );
        // A linha segue o controller guardado mesmo sem cheat ligado, e o evento sai uma vez.
        assert_eq!(h.c.model.player, PlayerLine::Found);
        h.tick(300);
        h.tick(400);
        assert_eq!(h.c.model.player, PlayerLine::Found);
        assert_eq!(event_count(&h, "personagem encontrado"), 1);
        // Aperta de novo: soma.
        h.ctl.push_action(done(10, 20));
        assert!(press(&mut h, 500));
        assert_eq!(event_count(&h, "Somar ciência: 10 → 20"), 1);
    }

    #[test]
    fn needs_controller_then_not_found() {
        let mut h = with_action_search();
        h.ctl.push_action(ActionOutcome::NeedsController);
        press(&mut h, 100);
        assert!(h.wait_job(100));
        h.ctl.set_controller_news(Some(false));
        h.tick(200);
        assert_eq!(event_count(&h, "personagem não encontrado"), 1);
        assert_eq!(h.c.model.player, PlayerLine::NotFound);
        h.tick(300);
        assert_eq!(h.c.model.player, PlayerLine::NotFound);
        assert_eq!(event_count(&h, "personagem não encontrado"), 1);
        assert_eq!(event_count(&h, "personagem encontrado"), 0);
    }

    #[test]
    fn action_window_resets_on_reconnect() {
        let mut h = with_action();
        assert!(press(&mut h, 100));
        assert!(!press(&mut h, 150));
        assert!(!h.c.action_last.is_empty() && !h.c.refusal_last.is_empty());
        // O jogo fecha e reabre (a procura do jogo leva 2 s): a janela e os limites recomeçam.
        h.reopen(200, 2_300);
        assert!(h.c.action_last.is_empty() && h.c.refusal_last.is_empty());
        assert!(press(&mut h, 2_400));
        assert_eq!(h.ctl.actions(), 2);
        assert_eq!(event_count(&h, "Somar ciência: aguarde"), 1);
    }

    #[test]
    fn action_checking_and_unavailable_states() {
        // Verificando a reflexão.
        let mut h = H::new();
        h.ctl.set_catalog(vec![action_info(Avail::Checking)]);
        h.ctl.set_field_avail(&[("ciencia", Avail::Checking)]);
        h.connect(0);
        assert!(!press(&mut h, 100));
        assert!(!press(&mut h, 200));
        assert_eq!(
            event_count(
                &h,
                "Somar ciência: verificando a reflexão do jogo; tente de novo em instantes"
            ),
            1
        );
        assert_eq!(h.ctl.actions(), 0);

        // Indisponível.
        let mut h = H::new();
        h.ctl.set_catalog(vec![action_info(Avail::Checking)]);
        h.ctl
            .set_field_avail(&[("ciencia", unavailable("campo X não encontrado"))]);
        h.connect(0);
        assert!(!press(&mut h, 100));
        assert_eq!(
            event_count(&h, "Somar ciência: indisponível (campo X não encontrado)"),
            1
        );
        assert_eq!(h.ctl.actions(), 0);
    }

    #[test]
    fn action_stuck_or_no_engine_uses_turn_on_message() {
        let mut h = with_action();
        h.ctl.set_stuck(true);
        assert!(!press(&mut h, 100));
        assert!(!press(&mut h, 200));
        assert_eq!(
            event_count(
                &h,
                "Somar ciência: não foi possível ligar (o jogo pode ter ficado parado; aperte r)"
            ),
            1
        );
        assert_eq!(h.ctl.actions(), 0);
        assert_eq!(h.c.op_counts.get("ciencia"), None);

        // Sem motor (disco ilegível): a mesma mensagem do ligar.
        let mut h = H::new();
        let mut spec = SearchSpec::new(vec![action_info(Avail::Checking)]);
        spec.disk = Err("disco ilegível".to_string());
        h.ctl.set_default_search(spec);
        h.ctl.set_field_avail(&[("ciencia", available())]);
        h.connect(0);
        assert!(!press(&mut h, 100));
        assert_eq!(
            event_count(
                &h,
                "Somar ciência: não foi possível ligar (o motor não está aberto)"
            ),
            1
        );
        assert_eq!(h.ctl.actions(), 0);
    }
}
