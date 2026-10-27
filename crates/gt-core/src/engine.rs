//! Motor de patches: aplica e desfaz lotes com diário, identidade e pausa conferida
//! (feature `write`).
//!
//! Os três caminhos (`apply`, `undo`/`undo_all` e `restore_entries`) seguem o mesmo
//! procedimento (spec 6.2): identidade → política → leitura prévia → intenção e
//! marcador no diário (fora da pausa) → **uma** pausa para o lote todo, com novas
//! tentativas pela regra de perigo → identidade → releitura sob pausa → conferência de
//! que tudo continua parado → escrita e releitura → SIGCONT → diário final.
//!
//! A troca de valor de um patch ativo ([`Engine::set_replacement`]) usa a mesma pausa
//! conferida, sem ponto perigoso (os dois valores têm as mesmas fronteiras de instrução).
//! Durante a troca o ativo fica fora de `active` e a entrada `{ original, replacement:
//! antigo, next: Some(novo) }` fica em `pending`, para o diário nunca perder o `next`.
//!
//! O diário em disco espelha o que o motor guarda em memória: os patches ativos e as
//! entradas ainda não resolvidas (intenções de um lote em curso, entradas vindas da
//! recuperação, restos de uma falha). O `Drop` desfaz os ativos e tenta resolver o resto.
//!
//! # Desfechos de uma operação
//!
//! Não há enum de desfecho: quem chama classifica pelo resultado e pelo estado do motor.
//!
//! - **Sucesso**: `Ok(report)` com o `id` em `report.restored`.
//! - **Bytes de outro**: `Ok(report)` com o `id` em `report.foreign`; o patch sai do
//!   registro e nada é escrito.
//! - **Recusa limpa**: qualquer outro `Err` com o `id` **fora** de [`Engine::pending_ids`]
//!   (`ThreadsInPatch`, `PauseTimeout`, `BytesMismatch`, `Journal`, `Io`, `WriteRefused`,
//!   `OutsidePolicy`, `Overlap`, `BadPatch` e, na troca de valor, `NotActive` e
//!   `BoundariesChanged`). Nada ficou escrito pela operação.
//! - **SIGCONT que falhou**: depois de **qualquer** operação, [`Engine::is_stuck`] diz se
//!   o jogo pode ter ficado parado por nós; isso pode acontecer sem restos.
//! - **Restos**: `Err(_)` com o `id` em [`Engine::pending_ids`]; a entrada fica no diário
//!   e bloqueia o trecho até [`Engine::restore_pending`] (ou o `Drop`, ou a próxima
//!   execução) resolvê-la.
//! - **Jogo fechou**: `Err(ProcessGone)`; o motor já descartou ativos, pendentes e o
//!   diário sem escrever. Sem rodar operação, [`Engine::discard_gone`] faz o mesmo.
//!
//! # Desvio e cave (spec 008, 6.3)
//!
//! Um patch é de um de dois tipos ([`PatchKind`]): `Code` (o desvio, de instruções inteiras) ou
//! `Cave` (código nosso sobre preenchimento `0xCC`). Para a cave, qualquer `pc` dentro do
//! trecho e qualquer thread de posição desconhecida são perigo. [`Engine::undo_all`] e a
//! recuperação ([`Engine::restore_entries`]) trabalham em **duas fases**: primeiro todos os
//! `Code`, depois as `Cave`, estas com [`CAVE_PAUSE_ATTEMPTS`] tentativas e
//! [`CAVE_PAUSE_WAIT`] entre elas. Uma `Cave` só é desfeita se nenhum `Code` ficou sem
//! resolver (senão o desvio ainda aponta para ela): ela fica no registro como pendente e no
//! diário. A cave que não pôde ser desfeita (`ThreadsInPatch`) também vira pendente.
//!
//! # Variáveis das caves (spec 008, 6.4)
//!
//! Um terceiro tipo de escrita, **sem pausa**: a folga entre o fim da `.data` e o fim da página,
//! que [`Engine::claim_vars`] reclama (região `rw-` no `maps`, toda zerada, gravada no diário
//! antes de qualquer escrita). [`Engine::write_var`] e [`Engine::read_var`] só tocam dentro
//! dela. O diário guarda a região, não os valores. No fim do [`Engine::undo_all`], no `Drop` e
//! na recuperação (e no [`Engine::restore_pending`] que resolve uma pendência) a região
//! inteira volta a zeros ([`Engine::clear_vars`]), **se** nenhum desvio (`Code`) nem cave
//! nossos estão ativos ou pendentes (uma cave que ficou com uma thread dentro ainda pode
//! gravar) e o `maps` ainda mostra a região `rw-`; senão ela fica no diário. Toda cave é
//! segura com as variáveis zeradas.
//!
//! # Escrita em dados (spec 005, 6.6)
//!
//! Um segundo tipo de escrita, **sem pausa**: campos de dados do jogo, numa região permitida
//! pela política de dados ([`Engine::set_data_policy`]), sempre depois de conferir a
//! identidade do objeto (bytes de guarda relidos imediatamente antes). [`Engine::write_data`]
//! não vai ao diário (barras: não há original a restaurar). [`Engine::set_bits`] limpa bits
//! e grava a entrada no diário **antes** de escrever; [`Engine::set_word`] faz o mesmo com um
//! valor inteiro (spec 006, 6.4). [`Engine::restore_field`], o `undo_all`, o `Drop` e a
//! recuperação devolvem os bits da máscara (ou o valor inteiro) ao original ([`decide_data`]).
//! Uma entrada viva por par (`id`, endereço) ([`data_key`]).

use std::ops::Range;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use nix::sys::signal::Signal;

use crate::journal::{
    DataAction, DataEntry, DataMode, EntryAction, GuardBytes, Journal, JournalEntry, JournalLock,
    JournalStore, Loaded, VarRegion, data_key, decide_data, decide_entry,
};
use crate::maps::{MapRegion, read_maps};
use crate::mem::{MemWriter, Pidfd};
use crate::patch::{
    Direction, PatchKind, PatchSpec, Validated, WritePolicy, is_dangerous, overlaps, validate,
};
use crate::procinfo::{Identity, ThreadPos, check_alive, identity};
use crate::suspend::{
    Fault, HookPoint, PauseConfig, PauseGuard, Thread, is_stopped_now, pause, resume_points,
};
use crate::{Error, Result};

/// Política de escrita calculada a partir do `maps` atual (relido antes de cada lote).
pub type PolicyFn = Box<dyn FnMut(&[MapRegion]) -> WritePolicy>;

/// Política das escritas em dados, calculada a partir do `maps`. Fica em cache: o `maps` só é
/// relido em [`Engine::refresh_data_maps`] e depois de uma escrita em dados que falhou.
pub type DataPolicyFn = Box<dyn FnMut(&[MapRegion]) -> WritePolicy>;

/// Maior escrita em dados aceita, em bytes.
const MAX_DATA_WRITE: usize = 8;

/// Tentativas da pausa ao desfazer caves (spec 008, 6.3).
pub const CAVE_PAUSE_ATTEMPTS: u32 = 5;

/// Espera entre as tentativas da pausa ao desfazer caves (spec 008, 6.3).
pub const CAVE_PAUSE_WAIT: Duration = Duration::from_millis(100);

/// Maior região de variáveis aceita, em bytes (uma página: a folga da `.data` é menor).
pub const MAX_VARS_LEN: u64 = 4096;

/// Parâmetros do motor.
#[derive(Debug, Clone)]
pub struct EngineOptions {
    /// Chave de versão do jogo, gravada no diário.
    pub version_key: String,
    /// Prazo, tentativas e espera da pausa (e o gancho de teste).
    pub pause: PauseConfig,
}

/// Resultado de um lote de `apply`, `undo` ou `undo_all`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BatchReport {
    /// Quanto tempo o alvo ficou parado na tentativa que escreveu (zero sem pausa).
    pub pause: Duration,
    /// Novas tentativas por thread em ponto perigoso.
    pub danger_retries: u32,
    /// Novas tentativas por prazo estourado (ou alvo retomado por outro na pausa).
    pub timeout_retries: u32,
    /// Patches aplicados (ou desfeitos) de fato.
    pub restored: Vec<String>,
    /// Patches não tocados porque os bytes eram de outro.
    pub foreign: Vec<String>,
}

/// Resultado de [`Engine::restore_entries`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoreReport {
    /// Entradas reescritas com os bytes originais.
    pub restored: Vec<String>,
    /// Entradas que já tinham os bytes originais (só esquecidas).
    pub dropped: Vec<String>,
    /// Entradas com bytes de outro (ou inválidas): não tocadas e tiradas do diário.
    pub foreign: Vec<String>,
    /// Entradas fora da política de escrita: não tocadas e mantidas no diário.
    pub outside_policy: Vec<String>,
    /// Caves não tentadas porque ainda restava um desvio sem resolver (o desvio aponta para
    /// elas): não tocadas e mantidas no diário.
    pub deferred: Vec<String>,
}

/// Quais fronteiras perdidas valem na regra de perigo de uma escrita.
#[derive(Debug, Clone, Copy)]
enum Danger {
    One(Direction),
    /// Restauração com bytes possivelmente misturados: união dos dois sentidos.
    Both,
    /// Troca de valor: os dois lados têm as mesmas fronteiras (conferido antes), então
    /// nenhum ponto de retomada é perigoso.
    None,
}

/// O que uma escrita do lote faz.
#[derive(Debug, Clone)]
enum Kind {
    /// Esperados → novos; bytes diferentes dos esperados recusam o lote.
    Apply,
    /// Novos → esperados; bytes diferentes dos novos são de outro.
    Undo,
    /// Entrada do diário: decisão por byte (`decide_entry`).
    Restore(JournalEntry),
    /// Troca de valor de um patch ativo: `from` → novos (`spec.replacement`); bytes
    /// diferentes de `from` são de outro.
    Swap { from: Vec<u8> },
}

/// Uma escrita do lote.
#[derive(Debug, Clone)]
struct Op {
    v: Validated,
    danger: Danger,
    kind: Kind,
}

/// Por que uma escrita do lote não acontece.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Skip {
    /// Os bytes já são os originais.
    Dropped,
    /// Os bytes são de outro (ou ilegíveis).
    Foreign,
}

/// Decisão para uma escrita, a partir dos bytes atuais.
#[derive(Debug, Clone)]
enum Decision {
    Write(Vec<u8>),
    Skip(Skip),
}

/// Escritas que precisam da pausa e as já resolvidas na leitura prévia.
type Presorted = (Vec<Op>, Vec<(Op, Skip)>);

/// Resultado da passagem com o alvo parado.
struct Pass {
    pause: Duration,
    danger_retries: u32,
    timeout_retries: u32,
    /// Para cada escrita do lote, na mesma ordem: `None` = escrita; `Some` = pulada.
    outcomes: Vec<Option<Skip>>,
}

/// Falha da passagem. `dirty` diz que pode ter sobrado byte escrito (o desfazer
/// ainda pausado também falhou, ou o SIGCONT falhou depois da escrita). `stuck` diz que
/// o SIGCONT falhou e o alvo pode ter ficado parado por nós: o diário mantém o marcador.
struct Failure {
    error: Error,
    dirty: bool,
    stuck: bool,
}

impl Failure {
    fn clean(error: Error) -> Failure {
        Failure {
            error,
            dirty: false,
            stuck: false,
        }
    }
}

/// O que aconteceu numa tentativa com o alvo parado.
#[derive(Debug)]
enum Step {
    /// Thread em ponto perigoso para a escrita deste `id`: nova tentativa.
    Danger(String),
    /// O alvo foi retomado por outro durante a conferência: nova tentativa.
    Resumed,
    /// Lote escrito e conferido (resultado de cada escrita, na ordem do lote).
    Wrote(Vec<Option<Skip>>),
}

/// O `SIGSTOP` da pausa foi nosso, mas o diário foi gravado sem o marcador (o alvo
/// estava parado na conferência e alguém o retomou antes da pausa).
fn needs_marker(sent_stop: bool, marked: bool) -> bool {
    sent_stop && !marked
}

/// Junta o resultado da tentativa pausada com o do SIGCONT que a encerra.
///
/// SIGCONT que falha nunca some: a falha volta com `stuck` (o diário fica com o
/// marcador) e, se o lote já tinha sido escrito, com `dirty`. Falha dentro da pausa
/// mantém o erro original. Alvo que terminou não fica parado: sem `stuck`.
fn settle_resume(
    step: std::result::Result<Step, Failure>,
    cont: Result<()>,
) -> std::result::Result<Step, Failure> {
    let error = match cont {
        Ok(()) => return step,
        Err(error) => error,
    };
    if matches!(error, Error::ProcessGone { .. }) {
        return Err(match step {
            Err(f) => f,
            Ok(_) => Failure::clean(error),
        });
    }
    tracing::error!(erro = %error, "falha ao enviar SIGCONT; o diário mantém o marcador");
    Err(match step {
        Err(f) => Failure { stuck: true, ..f },
        Ok(Step::Wrote(_)) => Failure {
            error,
            dirty: true,
            stuck: true,
        },
        Ok(Step::Danger(_) | Step::Resumed) => Failure {
            error,
            dirty: false,
            stuck: true,
        },
    })
}

/// Erro do SIGCONT simulado pela injeção de falha dos testes.
fn simulated_cont_error() -> Error {
    Error::Io(std::io::Error::other("falha de SIGCONT simulada"))
}

/// A injeção de falha deve trocar este SIGCONT por um erro? `ContFailsOnce` dispara uma
/// vez por motor (`fired`); só vale quando fomos nós que enviamos o `SIGSTOP`.
fn inject_cont_fault(fault: Option<Fault>, fired: &mut bool, sent_stop: bool) -> bool {
    if !sent_stop {
        return false;
    }
    match fault {
        Some(Fault::ContFailsAlways) => true,
        Some(Fault::ContFailsOnce) if !*fired => {
            *fired = true;
            true
        }
        _ => false,
    }
}

/// Retoma o alvo ao fim de uma tentativa pausada, com a falha de SIGCONT dos testes.
fn resume_guard(guard: PauseGuard<'_>, fault: Option<Fault>, fired: &mut bool) -> Result<()> {
    if inject_cont_fault(fault, fired, guard.sent_stop()) {
        guard.forget_resume();
        return Err(simulated_cont_error());
    }
    guard.resume()
}

/// Falhas injetadas nas escritas em dados (só para testes).
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DataFault {
    /// Toda escrita de restauração de dados falha (`WriteRefused`) sem tocar no jogo.
    RestoreFails,
    /// A próxima escrita de `set_word` ou `write_data` grava estes bytes (o jogo mexeu no campo) e a releitura
    /// diverge do pedido: `BytesMismatch` com um terceiro valor. Dispara uma vez.
    ThirdValue(Vec<u8>),
}

/// Resultado de [`Engine::set_word`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetOutcome {
    /// O campo já tem o valor pedido (e a entrada é nossa): nada foi escrito.
    Unchanged,
    /// O valor foi escrito (entrada nova ou troca de valor).
    Written,
    /// Uma troca interrompida já tinha escrito o valor: só a entrada foi finalizada.
    Finalized,
    /// O jogo mudou o campo: este valor virou o novo original e o valor pedido foi escrito.
    NewOriginal(Vec<u8>),
    /// O valor atual do jogo não passou em `accept`: nada foi escrito.
    Rejected(Vec<u8>),
}

/// Motor de patches de um processo.
///
/// Quem cria o motor segura a [`JournalLock`] enquanto ele existir.
pub struct Engine {
    pid: i32,
    identity: Identity,
    pidfd: Pidfd,
    writer: MemWriter,
    store: JournalStore,
    policy: PolicyFn,
    version_key: String,
    pause_cfg: PauseConfig,
    /// Patches ativos (escritos e confirmados).
    active: Vec<Validated>,
    /// Entradas do diário que não são patches ativos.
    pending: Vec<JournalEntry>,
    /// Pausas bem-sucedidas deste motor (o gancho e a falha de escrita injetados valem na
    /// `hook_on_pause`-ésima).
    pauses_ok: u32,
    after_apply_fired: bool,
    in_drop: bool,
    /// O SIGCONT falhou e o alvo pode ter ficado parado por nós: vale até um SIGCONT
    /// bem-sucedido (ou o jogo fechar). Enquanto valer, todo diário grava o marcador.
    stuck: bool,
    /// A falha injetada `ContFailsOnce` já disparou neste motor.
    cont_fault_fired: bool,
    /// Política das escritas em dados; sem ela, `write_data` e `set_bits` recusam.
    data_policy: Option<DataPolicyFn>,
    /// Política de dados calculada do último `maps` lido.
    data_maps: Option<WritePolicy>,
    /// Bits limpos e confirmados (uma entrada por `id` e byte).
    data: Vec<DataEntry>,
    /// Entradas de dados não resolvidas: restos de uma falha e as vindas da recuperação.
    data_pending: Vec<DataEntry>,
    /// Falha injetada nas escritas em dados (só para testes).
    data_fault: Option<DataFault>,
    /// Regiões de variáveis reclamadas (spec 008, 6.4), ainda sem zerar.
    vars: Vec<VarRegion>,
}

impl Engine {
    /// Abre o motor: horário de início → `pidfd` → `/proc/<pid>/mem` → horário de novo
    /// (spec 6.7). Se o horário mudou, o PID foi reaproveitado: [`Error::ProcessGone`].
    ///
    /// `expected` é a identidade que quem chama já conhece (a do diário, na recuperação,
    /// ou a lida ao resolver o alvo): se a lida agora for outra, o processo não é mais o
    /// mesmo e a abertura devolve [`Error::ProcessGone`] sem escrever nada.
    ///
    /// `_lock` é a testemunha de que quem chama segura a trava do diário.
    pub fn open(
        pid: i32,
        expected: Option<&Identity>,
        store: JournalStore,
        _lock: &JournalLock,
        policy: PolicyFn,
        opts: EngineOptions,
    ) -> Result<Engine> {
        let before = identity(pid)?;
        if expected.is_some_and(|e| *e != before) {
            return Err(Error::ProcessGone { pid });
        }
        let pidfd = Pidfd::open(pid)?;
        let writer = MemWriter::open(pid)?;
        let after = identity(pid)?;
        if before != after {
            return Err(Error::ProcessGone { pid });
        }
        let mut engine = Engine {
            pid,
            identity: before,
            pidfd,
            writer,
            store,
            policy,
            version_key: opts.version_key,
            pause_cfg: opts.pause,
            active: Vec::new(),
            pending: Vec::new(),
            pauses_ok: 0,
            after_apply_fired: false,
            in_drop: false,
            stuck: false,
            cont_fault_fired: false,
            data_policy: None,
            data_maps: None,
            data: Vec::new(),
            data_pending: Vec::new(),
            data_fault: None,
            vars: Vec::new(),
        };
        // Regiões de variáveis que um diário do mesmo processo ainda guarda (a recuperação as
        // manteve): sem isto o primeiro diário deste motor as apagaria e a limpeza nunca mais
        // aconteceria. Só regiões válidas; não grava nada.
        if let Ok(Loaded::Ok(j)) = engine.store.peek()
            && j.pid == engine.identity.pid
            && j.start_time == engine.identity.start_time
            && j.boot_id == engine.identity.boot_id
        {
            engine.adopt_vars(&j.vars);
        }
        Ok(engine)
    }

    /// Aplica um lote de patches (spec 6.2, passos 1–8). Tudo ou nada.
    pub fn apply(&mut self, batch: Vec<Validated>) -> Result<BatchReport> {
        self.retry_resume()?;
        let result = self.apply_inner(batch);
        self.finish(result)
    }

    /// Desfaz os patches ativos com estes `id` (os outros `id` são ignorados).
    pub fn undo(&mut self, ids: &[&str]) -> Result<BatchReport> {
        self.retry_resume()?;
        let result = self.undo_inner(|v| ids.contains(&v.spec.id.as_str()));
        self.finish(result)
    }

    /// Desfaz todos os patches ativos e restaura os bits de todas as entradas de dados (os
    /// restaurados e os já originais vão em `restored`; os descartados, em `foreign`). As
    /// partes são tentadas mesmo que a anterior falhe; o primeiro erro volta. Os patches
    /// saem em duas fases: os desvios (`Code`) e depois as caves (spec 008, 6.3). Por último
    /// as variáveis voltam a zeros ([`Engine::clear_vars`]), a não ser que ainda reste um desvio
    /// ou uma cave (pendentes): então elas ficam até o `restore_pending` (ou o `Drop`).
    pub fn undo_all(&mut self) -> Result<BatchReport> {
        self.retry_resume()?;
        let patches = self.undo_all_patches();
        if matches!(patches, Err(Error::ProcessGone { .. })) {
            return patches;
        }
        let data = if self.data.is_empty() && self.data_pending.is_empty() {
            Ok(RestoreReport::default())
        } else {
            let result = self.restore_data_inner(|_| true);
            self.finish(result)
        };
        // Por último, as variáveis (spec 008, 6.3): só se nenhum desvio nosso continua vivo.
        let vars = if self.vars.is_empty() {
            Ok(())
        } else {
            let result = self.clear_vars_inner();
            self.finish(result)
        };
        let mut report = patches?;
        let data = data?;
        vars?;
        report.restored.extend(data.restored);
        report.restored.extend(data.dropped);
        report.foreign.extend(data.foreign);
        Ok(report)
    }

    /// Os patches do `undo_all`, em duas fases (spec 008, 6.3): todos os `Code` e, só se
    /// nenhum ficou sem resolver, as `Cave`. A cave que não pôde ser desfeita vira pendente
    /// (e o erro, como `ThreadsInPatch`, volta).
    fn undo_all_patches(&mut self) -> Result<BatchReport> {
        let result = self.undo_inner(|v| v.spec.kind == PatchKind::Code);
        let mut report = self.finish(result)?;
        if !self.active.iter().any(|v| v.spec.kind == PatchKind::Cave) {
            return Ok(report);
        }
        if !caves_may_resolve(&self.journal_entries()) {
            // Um desvio pendente ainda aponta para as caves: não são tocadas, mas passam a
            // pendentes (aparecem em `pending_ids` e o `restore_pending` as resolve depois do
            // desvio, na ordem certa) em vez de ficarem ativas sem ninguém relatar.
            tracing::warn!("há desvio sem desfazer; as caves ficam pendentes sem serem tocadas");
            self.caves_to_pending("há desvio sem desfazer");
            return Ok(report);
        }
        let result =
            self.with_cave_pause(|engine| engine.undo_inner(|v| v.spec.kind == PatchKind::Cave));
        match self.finish(result) {
            Ok(caves) => {
                report.restored.extend(caves.restored);
                report.foreign.extend(caves.foreign);
                report.pause = report.pause.max(caves.pause);
                report.danger_retries += caves.danger_retries;
                report.timeout_retries += caves.timeout_retries;
                Ok(report)
            }
            Err(e @ Error::ProcessGone { .. }) => Err(e),
            Err(e) => {
                // O desvio já saiu, então a cave é inalcançável: fica no diário, como
                // pendente, para a próxima execução (ou o `Drop`) tentar de novo.
                self.caves_to_pending(&e.to_string());
                Err(e)
            }
        }
    }

    /// Passa as caves ativas para pendentes (sem tocar na memória) e regrava o diário.
    fn caves_to_pending(&mut self, why: &str) {
        let (caves, rest): (Vec<Validated>, Vec<Validated>) = std::mem::take(&mut self.active)
            .into_iter()
            .partition(|v| v.spec.kind == PatchKind::Cave);
        self.active = rest;
        for v in &caves {
            let entry = entry_of(v);
            if !self.pending.contains(&entry) {
                tracing::warn!(id = %v.spec.id, motivo = %why, "cave não desfeita; mantida no diário");
                self.pending.push(entry);
            }
        }
        self.save_logged(self.stuck);
    }

    /// Roda `f` com a pausa das caves ([`CAVE_PAUSE_ATTEMPTS`] tentativas, [`CAVE_PAUSE_WAIT`]
    /// entre elas) e devolve a configuração de antes.
    fn with_cave_pause<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let saved = (self.pause_cfg.max_attempts, self.pause_cfg.retry_wait);
        self.pause_cfg.max_attempts = CAVE_PAUSE_ATTEMPTS;
        self.pause_cfg.retry_wait = CAVE_PAUSE_WAIT;
        let out = f(self);
        (self.pause_cfg.max_attempts, self.pause_cfg.retry_wait) = saved;
        out
    }

    /// Define a política das escritas em dados (e esquece a calculada antes).
    pub fn set_data_policy(&mut self, policy: DataPolicyFn) {
        self.data_policy = Some(policy);
        self.data_maps = None;
    }

    /// Relê o `maps` e recalcula a política de dados. Sem política, não faz nada.
    pub fn refresh_data_maps(&mut self) -> Result<()> {
        self.data_maps = None;
        if self.data_policy.is_some() {
            self.current_data_policy()?;
        }
        Ok(())
    }

    /// Escreve `bytes` (1 a 8) em dados do jogo, sem pausa e sem diário: identidade do
    /// processo → política de dados ([`Error::DataOutsidePolicy`]) → guarda relida
    /// imediatamente antes ([`Error::GuardMismatch`]) → escrita e releitura.
    pub fn write_data(&mut self, addr: u64, bytes: &[u8], guard: &[GuardBytes]) -> Result<()> {
        self.retry_resume()?;
        let result = self.write_data_inner(addr, bytes, guard);
        self.finish(result)
    }

    /// Limpa os bits `mask` do byte em `addr` e registra o original no diário antes de
    /// escrever. Com a entrada (`id`, `addr`) já viva, é a manutenção: se algum bit da máscara
    /// voltou a ligar, limpa de novo (o original continua o do primeiro registro). Bits já
    /// limpos e sem entrada: nada é escrito nem registrado. Recusa se o `id` tem entrada de
    /// dados pendente ([`Error::DataPending`]).
    pub fn set_bits(&mut self, id: &str, addr: u64, mask: u8, guard: &[GuardBytes]) -> Result<()> {
        self.retry_resume()?;
        let result = self.set_bits_inner(id, addr, mask, guard);
        self.finish(result)
    }

    /// Escreve `value` (1 a 8 bytes) num campo de dados com o original no diário (spec 006,
    /// 6.4). Começa por `retry_resume`; recusa se o `id` tem entrada pendente
    /// ([`Error::DataPending`]); confere identidade, política de dados e guarda como o
    /// [`Engine::write_data`]. Depois decide pelo valor atual:
    ///
    /// - sem entrada (`id`, `addr`): `accept(atual)` falso é [`SetOutcome::Rejected`]; senão
    ///   guarda `original = atual` no diário **antes** de escrever;
    /// - entrada de outro objeto (guarda diferente): [`Error::DataEntryOtherObject`];
    /// - entrada com `next`: atual igual ao `next` finaliza ([`SetOutcome::Finalized`]); igual
    ///   ao `written` retira o `next` e segue;
    /// - atual igual ao `written`: igual ao valor pedido é [`SetOutcome::Unchanged`]; diferente
    ///   é uma troca (`next` no diário, escrita, `written` novo);
    /// - atual diferente do `written`: o jogo mudou o campo; `accept(atual)` falso é
    ///   `Rejected`, senão o atual vira o novo original ([`SetOutcome::NewOriginal`]).
    pub fn set_word(
        &mut self,
        id: &str,
        addr: u64,
        value: &[u8],
        guard: &[GuardBytes],
        accept: &dyn Fn(&[u8]) -> bool,
    ) -> Result<SetOutcome> {
        self.retry_resume()?;
        let result = self.set_word_inner(id, addr, value, guard, accept);
        self.finish(result)
    }

    /// Restaura todas as entradas de dados do `id` (vivas e pendentes, bits ou valor inteiro),
    /// pela regra de [`decide_data`]: restauradas e já originais em `restored`, descartadas
    /// em `foreign`. O valor inteiro volta de uma vez ou nada é escrito.
    pub fn restore_field(&mut self, id: &str) -> Result<BatchReport> {
        self.retry_resume()?;
        let result = self.restore_data_inner(|e| e.id == id);
        let data = self.finish(result)?;
        let mut restored = data.restored;
        restored.extend(data.dropped);
        Ok(BatchReport {
            restored,
            foreign: data.foreign,
            ..BatchReport::default()
        })
    }

    /// Resolve entradas de dados vindas de fora (a recuperação): entram como pendentes e são
    /// restauradas pela regra de [`decide_data`]. Uma escrita que falha mantém a entrada.
    pub fn restore_data(&mut self, entries: Vec<DataEntry>) -> Result<RestoreReport> {
        self.retry_resume()?;
        for e in &entries {
            let key = data_key(e);
            let known = self
                .data
                .iter()
                .chain(self.data_pending.iter())
                .any(|d| data_key(d) == key);
            if !known {
                self.data_pending.push(e.clone());
            }
        }
        let result =
            self.restore_data_inner(|e| entries.iter().any(|x| data_key(x) == data_key(e)));
        self.finish(result)
    }

    /// Entradas de dados vivas (bits limpos ou valor inteiro escrito, uma por par `id` e endereço).
    pub fn data_entries(&self) -> &[DataEntry] {
        &self.data
    }

    /// Reclama a região de variáveis das caves na folga da `.data` (spec 008, 6.4): confere a
    /// identidade do processo, que a região inteira é `rw-` (sem execução) no `maps` atual e que
    /// todos os bytes são zero, e grava a região no diário **antes** de qualquer escrita nela.
    /// Reclamar de novo a mesma região é um no-op; uma região que a cobre em parte é
    /// [`Error::BadPatch`]. Que a região é a folga da imagem do jogo quem a calcula garante
    /// (`gt_grounded2::policy::var_region`).
    pub fn claim_vars(&mut self, region: Range<u64>) -> Result<()> {
        self.retry_resume()?;
        let result = self.claim_vars_inner(region);
        self.finish(result)
    }

    /// Escreve `bytes` numa variável, **sem pausa** e sem guarda de objeto: identidade do
    /// processo → o trecho inteiro dentro de uma região reclamada ([`Error::VarOutsideRegion`])
    /// → escrita e releitura.
    pub fn write_var(&mut self, addr: u64, bytes: &[u8]) -> Result<()> {
        self.retry_resume()?;
        let result = self.write_var_inner(addr, bytes);
        self.finish(result)
    }

    /// Lê `len` bytes de uma variável, só dentro de uma região reclamada
    /// ([`Error::VarOutsideRegion`]).
    pub fn read_var(&self, addr: u64, len: usize) -> Result<Vec<u8>> {
        if !self.in_vars(addr, len) {
            return Err(Error::VarOutsideRegion { addr });
        }
        let mut buf = vec![0u8; len];
        self.writer.read_exact(addr, &mut buf)?;
        Ok(buf)
    }

    /// Regiões de variáveis reclamadas e ainda não zeradas.
    pub fn vars(&self) -> &[VarRegion] {
        &self.vars
    }

    /// Põe regiões vindas do diário entre as reclamadas, **sem** ler nem escrever nem gravar o
    /// diário: elas passam a constar do diário que o motor grava e [`Engine::clear_vars`] as
    /// zera quando nenhum desvio nosso continua vivo. O diário não é de confiança: cada região
    /// passa pelo mesmo filtro do [`Engine::claim_vars`] (1 a [`MAX_VARS_LEN`] bytes, início
    /// múltiplo de 8, sem sobrepor outra). Devolve as **recusadas** (com `warn`), que o motor
    /// não guarda e portanto saem do diário na próxima gravação.
    pub fn adopt_vars(&mut self, regions: &[VarRegion]) -> Vec<VarRegion> {
        let mut rejected = Vec::new();
        for r in regions {
            if self.vars.contains(r) {
                continue;
            }
            match self.check_var_region(r.addr, r.len) {
                Ok(()) => self.vars.push(*r),
                Err(reason) => {
                    tracing::warn!(addr = r.addr, len = r.len, %reason, "região de variáveis do diário recusada");
                    rejected.push(*r);
                }
            }
        }
        rejected
    }

    /// Regrava o diário com o estado em memória. Cuidado: entradas e dados só entram no registro
    /// do motor quando são restaurados; chamar antes disso tira-os do disco. A recuperação só
    /// usa quando não há nada a restaurar.
    pub fn save_journal_now(&self) {
        self.save_final();
    }

    /// Zera todas as regiões de variáveis (spec 008, 6.4), **se** nenhum desvio (`Code`) nem cave
    /// nossos estão ativos ou pendentes (uma cave que ficou com uma thread dentro ainda pode
    /// gravar nas variáveis), a identidade do processo bate e o `maps` ainda mostra a região
    /// `rw-`. Não confere os valores (ninguém mais usa a folga). Quando alguma condição falha, a
    /// região fica no registro e no diário e nada é escrito. Uma escrita que falha mantém a
    /// região e devolve o primeiro erro. Sem pausa.
    pub fn clear_vars(&mut self) -> Result<()> {
        self.retry_resume()?;
        let result = self.clear_vars_inner();
        self.finish(result)
    }

    /// Troca os bytes do patch ativo `id` por `new` numa pausa (spec 003 6.7).
    ///
    /// Os bytes originais continuam sendo os de antes do patch; `new` precisa ter o mesmo
    /// tamanho e as mesmas fronteiras de instrução dos bytes atuais do patch
    /// ([`Error::BoundariesChanged`] senão), então não há ponto perigoso. Durante a troca o
    /// diário guarda `{ original, replacement: antigo, next: novo }`. Desfechos: veja o
    /// topo do módulo (sucesso, bytes de outro em `foreign`, recusa limpa, restos, SIGCONT
    /// que falhou e jogo fechou). `id` sem patch ativo: [`Error::NotActive`].
    pub fn set_replacement(&mut self, id: &str, new: Vec<u8>) -> Result<BatchReport> {
        self.retry_resume()?;
        let result = self.swap_inner(id, new);
        self.finish(result)
    }

    /// Resolve entradas do diário: reescreve os originais onde cada byte é o original ou
    /// o nosso, esquece as que já estão originais e não toca nas de outro.
    ///
    /// Falha por perigo ou prazo mantém as entradas no diário. Entradas fora da política de
    /// escrita vão para `outside_policy`: não são tocadas e continuam no diário, porque os bytes
    /// podem ser nossos e a próxima execução precisa vê-las.
    pub fn restore_entries(&mut self, entries: Vec<JournalEntry>) -> Result<RestoreReport> {
        self.retry_resume()?;
        let result = self.restore_inner(entries);
        self.finish(result)
    }

    /// Restaura as entradas pendentes (restos de uma falha e intenções não resolvidas),
    /// como na recuperação: é [`Engine::restore_entries`] com todas elas. Sem pendentes,
    /// não escreve nada (o SIGCONT que falhou ainda é tentado antes).
    pub fn restore_pending(&mut self) -> Result<RestoreReport> {
        let entries = self.pending.clone();
        let had_pending = !entries.is_empty() || !self.data_pending.is_empty();
        let mut report = self.restore_entries(entries)?;
        let data = self.data_pending.clone();
        if !data.is_empty() {
            let r = self.restore_data(data)?;
            report.restored.extend(r.restored);
            report.dropped.extend(r.dropped);
            report.foreign.extend(r.foreign);
        }
        // Resolvida a pendência (uma cave que ficou com uma thread dentro, por exemplo), as
        // variáveis que ela segurava podem voltar a zeros. Só quando havia pendência: sem ela,
        // as variáveis são de patches em uso.
        if had_pending && !self.vars.is_empty() {
            match self.clear_vars() {
                Ok(()) => {}
                Err(e @ Error::ProcessGone { .. }) => return Err(e),
                Err(e) => tracing::warn!(erro = %e, "variáveis não zeradas agora"),
            }
        }
        Ok(report)
    }

    /// `id` das entradas pendentes: restos de uma falha e intenções não resolvidas, de
    /// patches e de dados.
    pub fn pending_ids(&self) -> Vec<String> {
        self.pending
            .iter()
            .map(|e| e.id.clone())
            .chain(self.data_pending.iter().map(|e| e.id.clone()))
            .collect()
    }

    /// Se o jogo já terminou, descarta o registro sem escrever (zera ativos, pendentes e
    /// `stuck`, limpa o diário), como o fim de uma operação com [`Error::ProcessGone`].
    /// Com o jogo vivo, ou se não deu para saber, não faz nada.
    pub fn discard_gone(&mut self) {
        if let Err(e @ Error::ProcessGone { .. }) = check_alive(&self.identity) {
            let _ = self.finish::<()>(Err(e));
        }
    }

    /// Patches ativos.
    pub fn active(&self) -> Vec<&PatchSpec> {
        self.active.iter().map(|v| &v.spec).collect()
    }

    pub fn pid(&self) -> i32 {
        self.pid
    }

    pub fn pidfd(&self) -> &Pidfd {
        &self.pidfd
    }

    pub fn store(&self) -> &JournalStore {
        &self.store
    }

    /// O último SIGCONT falhou e o alvo pode ter ficado parado por nós. Enquanto isso
    /// valer, o diário mantém o marcador "pausado por nós".
    pub fn is_stuck(&self) -> bool {
        self.stuck
    }

    /// Tenta de novo o SIGCONT que falhou. Sem `stuck`, não faz nada e não envia sinal.
    ///
    /// Com `stuck`, envia SIGCONT pelo `pidfd`: sucesso limpa o estado e regrava o diário
    /// sem o marcador (apagando o arquivo se não houver entradas); [`Error::ProcessGone`]
    /// descarta o registro e o diário; qualquer outra falha mantém o estado e o devolve.
    ///
    /// Toda operação do motor (e o `Drop`) chama isto antes de qualquer outra coisa,
    /// inclusive antes da conferência "o alvo já está parado?": um alvo que nós deixamos
    /// parado pareceria parado por outro.
    pub fn retry_resume(&mut self) -> Result<()> {
        if !self.stuck {
            return Ok(());
        }
        if let Some(counter) = &self.pause_cfg.resume_counter {
            counter.fetch_add(1, Ordering::SeqCst);
        }
        let sent = if self.pause_cfg.fault == Some(Fault::ContFailsAlways) {
            Err(simulated_cont_error())
        } else {
            self.pidfd.send(Signal::SIGCONT)
        };
        match sent {
            Ok(()) => {
                tracing::warn!(pid = self.pid, "SIGCONT enviado de novo; jogo retomado");
                self.stuck = false;
                self.save_final();
                Ok(())
            }
            Err(e) => {
                if !matches!(e, Error::ProcessGone { .. }) {
                    tracing::error!(erro = %e, "nova tentativa de SIGCONT falhou; o diário mantém o marcador");
                }
                self.finish(Err(e))
            }
        }
    }

    /// Marca o motor como `stuck` e grava o diário com o marcador (só para testes).
    #[doc(hidden)]
    pub fn force_stuck_for_test(&mut self) {
        self.stuck = true;
        self.save_logged(true);
    }

    /// Liga (ou desliga, com `None`) a falha injetada nas escritas em dados (só para testes).
    #[doc(hidden)]
    pub fn inject_data_fault_for_test(&mut self, fault: Option<DataFault>) {
        self.data_fault = fault;
    }

    /// Põe uma entrada de dados como viva e grava o diário (só para testes).
    #[doc(hidden)]
    pub fn force_data_for_test(&mut self, entry: DataEntry) {
        self.upsert_data(entry);
        self.save_final();
    }

    /// Põe uma entrada de dados como pendente e grava o diário (só para testes).
    #[doc(hidden)]
    pub fn force_data_pending_for_test(&mut self, entry: DataEntry) {
        self.data_pending.push(entry);
        self.save_final();
    }

    /// `ProcessGone` descarta o registro e limpa o diário (sem escrever nada).
    fn finish<T>(&mut self, result: Result<T>) -> Result<T> {
        if let Err(Error::ProcessGone { pid }) = &result {
            tracing::warn!(pid, "o jogo fechou; patches descartados sem escrever");
            self.active.clear();
            self.pending.clear();
            self.data.clear();
            self.data_pending.clear();
            self.vars.clear();
            self.stuck = false;
            if let Err(e) = self.store.clear() {
                tracing::error!(erro = %e, "falha ao limpar o diário");
            }
        }
        result
    }

    /// Política de dados em cache (calculada do `maps` na primeira vez).
    fn current_data_policy(&mut self) -> Result<&WritePolicy> {
        let Some(policy) = self.data_policy.as_mut() else {
            return Err(Error::NoDataPolicy);
        };
        if self.data_maps.is_none() {
            let maps = read_maps(self.pid)?;
            self.data_maps = Some(policy(&maps));
        }
        self.data_maps.as_ref().ok_or(Error::NoDataPolicy)
    }

    /// Confere a guarda do objeto: cada trecho relido tem de ser igual ao esperado. Trecho
    /// ilegível conta como objeto trocado.
    fn guard_ok(&self, guard: &[GuardBytes]) -> Result<bool> {
        for g in guard {
            let mut now = vec![0u8; g.bytes.len()];
            match self.writer.read_exact(g.addr, &mut now) {
                Ok(()) if now == g.bytes => {}
                Ok(()) | Err(Error::Unmapped { .. }) => return Ok(false),
                Err(e) => return Err(e),
            }
        }
        Ok(true)
    }

    /// Byte em `addr`; `None` se ilegível.
    fn read_byte(&self, addr: u64) -> Result<Option<u8>> {
        let mut b = [0u8; 1];
        match self.writer.read_exact(addr, &mut b) {
            Ok(()) => Ok(Some(b[0])),
            Err(Error::Unmapped { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Identidade do processo, política de dados e guarda, nesta ordem.
    fn check_data_write(&mut self, addr: u64, len: usize, guard: &[GuardBytes]) -> Result<()> {
        check_alive(&self.identity)?;
        let end = addr
            .checked_add(len as u64)
            .ok_or(Error::DataOutsidePolicy { addr })?;
        if !self.current_data_policy()?.allows(&(addr..end)) {
            return Err(Error::DataOutsidePolicy { addr });
        }
        if !self.guard_ok(guard)? {
            return Err(Error::GuardMismatch { addr });
        }
        Ok(())
    }

    /// Escrita em dados conferida; na falha, o `maps` é relido na próxima.
    fn write_data_verified(&mut self, id: &str, addr: u64, bytes: &[u8]) -> Result<()> {
        let result = self.write_verified(id, addr, bytes);
        if result.is_err() {
            self.data_maps = None;
        }
        result
    }

    fn write_data_inner(&mut self, addr: u64, bytes: &[u8], guard: &[GuardBytes]) -> Result<()> {
        if bytes.is_empty() || bytes.len() > MAX_DATA_WRITE {
            return Err(Error::BadPatch {
                id: "dados".to_owned(),
                reason: format!(
                    "escrita em dados de {} bytes (1 a {MAX_DATA_WRITE})",
                    bytes.len()
                ),
            });
        }
        self.check_data_write(addr, bytes.len(), guard)?;
        if let Some(DataFault::ThirdValue(third)) = self.data_fault.clone() {
            // O jogo escreve entre a nossa escrita e a releitura (só para testes).
            self.data_fault = None;
            self.writer.write(addr, &third)?;
            self.data_maps = None;
            return Err(Error::BytesMismatch {
                id: "dados".to_owned(),
                expected: hex(bytes),
                found: hex(&third),
            });
        }
        self.write_data_verified("dados", addr, bytes)
    }

    /// Entrada viva do par (`id`, `addr`).
    fn live_entry(&self, id: &str, addr: u64) -> Option<&DataEntry> {
        self.data.iter().find(|d| data_key(d) == (id, addr))
    }

    /// Põe a entrada entre as vivas, substituindo a do mesmo par.
    fn upsert_data(&mut self, entry: DataEntry) {
        match self
            .data
            .iter_mut()
            .find(|d| data_key(d) == data_key(&entry))
        {
            Some(slot) => *slot = entry,
            None => self.data.push(entry),
        }
    }

    /// Põe a entrada entre as pendentes, substituindo a do mesmo par.
    fn upsert_pending(&mut self, entry: DataEntry) {
        match self
            .data_pending
            .iter_mut()
            .find(|d| data_key(d) == data_key(&entry))
        {
            Some(slot) => *slot = entry,
            None => self.data_pending.push(entry),
        }
    }

    /// Tira dos vivos a entrada do mesmo par.
    fn remove_data(&mut self, entry: &DataEntry) {
        self.data.retain(|d| data_key(d) != data_key(entry));
    }

    /// `len` bytes em `addr`; `None` se ilegíveis.
    fn read_bytes(&self, addr: u64, len: usize) -> Result<Option<Vec<u8>>> {
        let mut buf = vec![0u8; len];
        match self.writer.read_exact(addr, &mut buf) {
            Ok(()) => Ok(Some(buf)),
            Err(Error::Unmapped { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Bytes atuais da entrada (o tamanho do `original`); entrada malformada não lê nada
    /// (`decide_data` a descarta).
    fn read_entry_bytes(&self, e: &DataEntry) -> Result<Option<Vec<u8>>> {
        if e.original.is_empty() || e.original.len() > MAX_DATA_WRITE {
            return Ok(None);
        }
        self.read_bytes(e.addr, e.original.len())
    }

    fn set_word_inner(
        &mut self,
        id: &str,
        addr: u64,
        value: &[u8],
        guard: &[GuardBytes],
        accept: &dyn Fn(&[u8]) -> bool,
    ) -> Result<SetOutcome> {
        if self.data_pending.iter().any(|e| e.id == id) {
            return Err(Error::DataPending { id: id.to_owned() });
        }
        if value.is_empty() || value.len() > MAX_DATA_WRITE {
            return Err(Error::BadPatch {
                id: id.to_owned(),
                reason: format!(
                    "escrita em dados de {} bytes (1 a {MAX_DATA_WRITE})",
                    value.len()
                ),
            });
        }
        self.check_data_write(addr, value.len(), guard)?;
        let len = value.len();
        let current = self
            .read_bytes(addr, len)?
            .ok_or(Error::Unmapped { addr, len })?;

        let Some(mut entry) = self.live_entry(id, addr).cloned() else {
            if !accept(&current) {
                return Ok(SetOutcome::Rejected(current));
            }
            let entry = DataEntry::whole(id, addr, current.clone(), value.to_vec(), guard.to_vec());
            self.journal_then_write(entry, None, &current)?;
            return Ok(SetOutcome::Written);
        };
        if entry.mode != DataMode::Whole
            || entry.guard != guard
            || entry.original.len() != len
            || entry.written.len() != len
        {
            return Err(Error::DataEntryOtherObject { id: id.to_owned() });
        }

        // Restos de uma troca interrompida.
        if let Some(next) = entry.next.clone() {
            if current == next {
                entry.written = next;
                entry.next = None;
                self.upsert_data(entry);
                self.save_journal(false)?;
                return Ok(SetOutcome::Finalized);
            }
            if current == entry.written {
                entry.next = None;
                self.upsert_data(entry.clone());
                self.save_journal(false)?;
            }
        }

        if current == entry.written {
            if entry.written == value {
                return Ok(SetOutcome::Unchanged);
            }
            // Troca de valor: `next` no diário, escrita, `written` novo.
            let previous = entry.clone();
            let mut swap = entry;
            swap.next = Some(value.to_vec());
            self.journal_then_write(swap, Some(previous), &current)?;
            return Ok(SetOutcome::Written);
        }

        // O jogo mudou o campo: o valor dele, se são, vira o novo original.
        if !accept(&current) {
            return Ok(SetOutcome::Rejected(current));
        }
        let previous = entry.clone();
        let fresh = DataEntry::whole(id, addr, current.clone(), value.to_vec(), guard.to_vec());
        self.journal_then_write(fresh, Some(previous), &current)?;
        Ok(SetOutcome::NewOriginal(current))
    }

    /// Grava `entry` no diário **antes** de escrever e escreve o valor dela (`next`, numa troca;
    /// senão `written`). `previous` é a entrada de antes (a que volta se nada chegou a ser
    /// escrito); `before` são os bytes lidos antes da escrita. Numa troca, termina gravando
    /// `written = next` sem `next`.
    fn journal_then_write(
        &mut self,
        entry: DataEntry,
        previous: Option<DataEntry>,
        before: &[u8],
    ) -> Result<()> {
        let swapping = entry.next.is_some();
        let value = entry.next.clone().unwrap_or_else(|| entry.written.clone());
        self.upsert_data(entry.clone());
        if let Err(e) = self.save_journal(false) {
            self.revert_entry(&entry, previous);
            return Err(e);
        }
        if swapping {
            self.fire_hook(HookPoint::AfterNext);
        }
        let written = if let Some(DataFault::ThirdValue(third)) = self.data_fault.clone() {
            self.data_fault = None;
            self.writer.write(entry.addr, &third).and_then(|()| {
                Err(Error::BytesMismatch {
                    id: entry.id.clone(),
                    expected: hex(&value),
                    found: hex(&third),
                })
            })
        } else {
            self.write_data_verified(&entry.id, entry.addr, &value)
        };
        if let Err(e) = written {
            if matches!(e, Error::ProcessGone { .. }) {
                return Err(e);
            }
            match self.read_bytes(entry.addr, before.len()) {
                // Nada foi escrito: volta a entrada de antes (ou nenhuma).
                Ok(Some(now)) if now == before => self.revert_entry(&entry, previous),
                // O jogo mexeu no campo depois da nossa escrita (releitura com um terceiro
                // valor): vale a regra do "jogo mudou o campo"; a entrada (com o `next`, se
                // houver) segue viva e o próximo `set_word` a resolve.
                Ok(Some(now)) if now != value => {}
                // O campo ficou com o nosso valor sem confirmação, ou não dá para saber:
                // pendente até a recuperação, que decide pelo `written`/`next` da entrada.
                _ => {
                    self.remove_data(&entry);
                    self.upsert_pending(entry);
                }
            }
            self.save_final();
            return Err(e);
        }
        if swapping {
            let mut done = entry;
            done.written = value;
            done.next = None;
            self.upsert_data(done);
            self.save_journal(false)?;
        }
        Ok(())
    }

    /// Desfaz a entrada em memória e no diário depois de uma escrita que não aconteceu.
    fn revert_entry(&mut self, entry: &DataEntry, previous: Option<DataEntry>) {
        match previous {
            Some(p) => self.upsert_data(p),
            None => self.remove_data(entry),
        }
        self.save_final();
    }

    fn set_bits_inner(
        &mut self,
        id: &str,
        addr: u64,
        mask: u8,
        guard: &[GuardBytes],
    ) -> Result<()> {
        if self.data_pending.iter().any(|e| e.id == id) {
            return Err(Error::DataPending { id: id.to_owned() });
        }
        if mask == 0 {
            return Err(Error::BadPatch {
                id: id.to_owned(),
                reason: "máscara vazia".to_owned(),
            });
        }
        self.check_data_write(addr, 1, guard)?;
        let unreadable = || Error::Unmapped { addr, len: 1 };

        // Manutenção: a entrada já existe; só limpa de novo o que o jogo religou.
        if self.data.iter().any(|e| e.id == id && e.addr == addr) {
            let now = self.read_byte(addr)?.ok_or_else(unreadable)?;
            if now & mask == 0 {
                return Ok(());
            }
            tracing::debug!(id, addr, "bits religados pelo jogo; limpando de novo");
            return self.write_data_verified(id, addr, &[now & !mask]);
        }

        let before = self.read_byte(addr)?.ok_or_else(unreadable)?;
        if before & mask == 0 {
            // Já limpos: nada a escrever, e nenhum original a guardar.
            return Ok(());
        }
        let entry = DataEntry::bits(id, addr, before, before & !mask, mask, guard.to_vec());
        // O diário recebe a entrada antes da escrita.
        self.upsert_data(entry.clone());
        if let Err(e) = self.save_journal(false) {
            self.remove_data(&entry);
            return Err(e);
        }
        // O byte é recalculado de uma leitura imediatamente anterior à escrita.
        let written = match self.read_byte(addr) {
            Ok(Some(now)) => self.write_data_verified(id, addr, &[now & !mask]),
            Ok(None) => Err(unreadable()),
            Err(e) => Err(e),
        };
        if let Err(e) = written {
            if matches!(e, Error::ProcessGone { .. }) {
                return Err(e);
            }
            self.remove_data(&entry);
            // Se os bits continuam os originais, nada ficou escrito; senão, sobra pendente.
            match self.read_byte(addr) {
                Ok(Some(now)) if now & mask == before & mask => {}
                _ => self.upsert_pending(entry),
            }
            self.save_final();
            return Err(e);
        }
        Ok(())
    }

    /// Restaura as entradas de dados escolhidas (vivas e pendentes) pela regra de
    /// [`decide_data`], com a guarda relida e o `maps` atual. Uma escrita que falha deixa a
    /// entrada pendente; as outras seguem, e o primeiro erro volta no fim.
    fn restore_data_inner(&mut self, select: impl Fn(&DataEntry) -> bool) -> Result<RestoreReport> {
        let chosen: Vec<DataEntry> = self
            .data
            .iter()
            .chain(self.data_pending.iter())
            .filter(|e| select(e))
            .cloned()
            .collect();
        if chosen.is_empty() {
            return Ok(RestoreReport::default());
        }
        check_alive(&self.identity)?;
        let maps = read_maps(self.pid)?;
        let mut report = RestoreReport::default();
        let mut failed: Option<Error> = None;
        for e in chosen {
            let current = self.read_entry_bytes(&e)?;
            let guard_ok = self.guard_ok(&e.guard)?;
            let end = e.addr.saturating_add(e.original.len() as u64);
            let writable = maps
                .iter()
                .any(|r| r.perms.read && r.perms.write && r.start <= e.addr && end <= r.end);
            match decide_data(&e, current.as_deref(), guard_ok, writable) {
                DataAction::Drop => report.dropped.push(e.id.clone()),
                DataAction::Discard => {
                    tracing::warn!(
                        id = %e.id,
                        addr = e.addr,
                        "objeto trocado ou bits de outro; entrada de dados descartada sem escrever"
                    );
                    report.foreign.push(e.id.clone());
                }
                DataAction::Restore(bytes) => {
                    let written = if self.data_fault == Some(DataFault::RestoreFails) {
                        Err(Error::WriteRefused { addr: e.addr })
                    } else {
                        self.write_verified(&e.id, e.addr, &bytes)
                    };
                    if let Err(err) = written {
                        if matches!(err, Error::ProcessGone { .. }) {
                            return Err(err);
                        }
                        tracing::error!(id = %e.id, erro = %err, "falha ao restaurar o campo");
                        self.remove_data(&e);
                        self.upsert_pending(e);
                        failed.get_or_insert(err);
                        continue;
                    }
                    report.restored.push(e.id.clone());
                }
            }
            self.remove_data(&e);
            self.data_pending.retain(|d| data_key(d) != data_key(&e));
        }
        self.save_final();
        match failed {
            Some(err) => Err(err),
            None => Ok(report),
        }
    }

    /// O trecho `[addr, addr + len)` cabe inteiro numa região de variáveis reclamada.
    fn in_vars(&self, addr: u64, len: usize) -> bool {
        let Some(end) = addr.checked_add(len as u64) else {
            return false;
        };
        len > 0
            && self
                .vars
                .iter()
                .any(|v| v.addr <= addr && end <= v.range().end)
    }

    /// Forma de uma região de variáveis: 1 a [`MAX_VARS_LEN`] bytes, início múltiplo de 8, sem
    /// estourar o espaço de endereços e sem sobrepor outra região já reclamada.
    fn check_var_region(&self, addr: u64, len: u64) -> std::result::Result<(), String> {
        let end = addr.checked_add(len);
        if len == 0 || len > MAX_VARS_LEN || !addr.is_multiple_of(8) || end.is_none() {
            return Err(format!(
                "região de variáveis {addr:#x} com {len} bytes (1 a {MAX_VARS_LEN} bytes, alinhada a 8)"
            ));
        }
        let end = end.unwrap_or(addr);
        if self
            .vars
            .iter()
            .any(|v| v.addr < end && addr < v.range().end)
        {
            return Err("a região se sobrepõe a outra já reclamada".to_owned());
        }
        Ok(())
    }

    fn claim_vars_inner(&mut self, region: Range<u64>) -> Result<()> {
        let bad = |reason: String| Error::BadPatch {
            id: "variáveis".to_owned(),
            reason,
        };
        check_alive(&self.identity)?;
        let len = region.end.saturating_sub(region.start);
        let wanted = VarRegion {
            addr: region.start,
            len,
        };
        if self.vars.contains(&wanted) {
            return Ok(());
        }
        self.check_var_region(region.start, len).map_err(bad)?;
        let maps = read_maps(self.pid)?;
        if !rw_policy(&maps).allows(&region) {
            return Err(Error::VarsNotWritable { addr: region.start });
        }
        let mut now = vec![0u8; len as usize];
        match self.writer.read_exact(region.start, &mut now) {
            Ok(()) => {}
            Err(Error::Unmapped { .. }) => {
                return Err(Error::VarsNotWritable { addr: region.start });
            }
            Err(e) => return Err(e),
        }
        if let Some(i) = now.iter().position(|b| *b != 0) {
            return Err(Error::VarsNotZero {
                addr: region.start + i as u64,
            });
        }
        // A região entra no diário antes de qualquer escrita nela.
        self.vars.push(wanted);
        if let Err(e) = self.save_journal(false) {
            self.vars.retain(|v| *v != wanted);
            return Err(e);
        }
        Ok(())
    }

    fn write_var_inner(&mut self, addr: u64, bytes: &[u8]) -> Result<()> {
        check_alive(&self.identity)?;
        if !self.in_vars(addr, bytes.len()) {
            return Err(Error::VarOutsideRegion { addr });
        }
        self.write_verified("variável", addr, bytes)
    }

    fn clear_vars_inner(&mut self) -> Result<()> {
        if self.vars.is_empty() {
            return Ok(());
        }
        // Um desvio vivo aponta para a cave; uma cave que ficou (thread dentro) ainda pode
        // gravar nas variáveis. Em qualquer dos casos a folga continua como está.
        if !self.journal_entries().is_empty() {
            tracing::warn!("há desvio ou cave ativo ou pendente; as variáveis ficam como estão");
            return Ok(());
        }
        check_alive(&self.identity)?;
        let maps = read_maps(self.pid)?;
        let rw = rw_policy(&maps);
        let mut failed: Option<Error> = None;
        for v in self.vars.clone() {
            if !rw.allows(&v.range()) {
                tracing::warn!(
                    addr = v.addr,
                    "a região de variáveis não é mais `rw-`; mantida no diário"
                );
                continue;
            }
            let zeros = vec![0u8; usize::try_from(v.len).unwrap_or(0)];
            match self.write_verified("variáveis", v.addr, &zeros) {
                Ok(()) => self.vars.retain(|x| *x != v),
                Err(e @ Error::ProcessGone { .. }) => return Err(e),
                Err(e) => {
                    tracing::error!(addr = v.addr, erro = %e, "falha ao zerar as variáveis; mantidas no diário");
                    failed.get_or_insert(e);
                }
            }
        }
        self.save_final();
        failed.map_or(Ok(()), Err)
    }

    fn apply_inner(&mut self, batch: Vec<Validated>) -> Result<BatchReport> {
        if batch.is_empty() {
            return Ok(BatchReport::default());
        }
        check_alive(&self.identity)?;
        let policy = self.current_policy()?;
        for (i, v) in batch.iter().enumerate() {
            let range = v.spec.range();
            if !policy.allows(&range) {
                return Err(Error::OutsidePolicy {
                    id: v.spec.id.clone(),
                    addr: v.spec.addr,
                });
            }
            let busy = self
                .active
                .iter()
                .map(|a| a.spec.range())
                .chain(self.pending.iter().map(JournalEntry::range))
                .chain(batch[..i].iter().map(|b| b.spec.range()))
                .any(|r| overlaps(&r, &range));
            if busy {
                return Err(Error::Overlap {
                    id: v.spec.id.clone(),
                });
            }
        }
        let ops: Vec<Op> = batch
            .into_iter()
            .map(|v| Op {
                v,
                danger: Danger::One(Direction::Apply),
                kind: Kind::Apply,
            })
            .collect();
        // Passo 2: leitura prévia, sem pausa. Bytes diferentes recusam o lote.
        for op in &ops {
            self.decide_now(op)?;
        }

        // Passo 3: intenção (e marcador, se vamos pausar) antes da pausa.
        let entries: Vec<JournalEntry> = ops.iter().map(|o| entry_of(&o.v)).collect();
        let will_stop = !is_stopped_now(self.pid)?;
        self.pending.extend(entries.iter().cloned());
        if let Err(e) = self.save_journal(will_stop) {
            self.forget_pending(&entries);
            return Err(e);
        }

        match self.locked_write(&ops, will_stop) {
            Ok(pass) => {
                self.forget_pending(&entries);
                let restored = ops.iter().map(|o| o.v.spec.id.clone()).collect();
                self.active.extend(ops.into_iter().map(|o| o.v));
                self.save_final();
                self.fire_after_apply();
                Ok(BatchReport {
                    pause: pass.pause,
                    danger_retries: pass.danger_retries,
                    timeout_retries: pass.timeout_retries,
                    restored,
                    foreign: Vec::new(),
                })
            }
            Err(f) => {
                if matches!(f.error, Error::ProcessGone { .. }) {
                    return Err(f.error);
                }
                // Sem sobra escrita, o diário volta ao estado anterior ao lote; com sobra,
                // as entradas ficam como pendentes para o `Drop` ou a próxima execução.
                if !f.dirty {
                    self.forget_pending(&entries);
                }
                self.stuck |= f.stuck;
                self.save_logged(f.stuck);
                Err(f.error)
            }
        }
    }

    fn swap_inner(&mut self, id: &str, new: Vec<u8>) -> Result<BatchReport> {
        let index = self
            .active
            .iter()
            .position(|v| v.spec.id == id)
            .ok_or_else(|| Error::NotActive { id: id.to_owned() })?;
        let old = self.active[index].clone();
        if old.spec.kind == PatchKind::Cave {
            return Err(Error::BadPatch {
                id: id.to_owned(),
                reason: "uma cave não troca de valor".to_owned(),
            });
        }
        check_alive(&self.identity)?;
        let policy = self.current_policy()?;
        if !policy.allows(&old.spec.range()) {
            return Err(Error::OutsidePolicy {
                id: id.to_owned(),
                addr: old.spec.addr,
            });
        }
        let fresh = validate(PatchSpec {
            id: id.to_owned(),
            addr: old.spec.addr,
            expected: old.spec.expected.clone(),
            replacement: new.clone(),
            kind: old.spec.kind,
        })?;
        if fresh.replacement_starts != old.replacement_starts {
            return Err(Error::BoundariesChanged { id: id.to_owned() });
        }
        let op = Op {
            v: fresh,
            danger: Danger::None,
            kind: Kind::Swap {
                from: old.spec.replacement.clone(),
            },
        };
        // Leitura prévia, sem pausa: bytes que não são os do valor atual são de outro.
        if let Decision::Skip(_) = self.decide_now(&op)?.0 {
            tracing::warn!(id = %id, "bytes do patch alterados por outro; tirado do registro sem escrever");
            self.active.remove(index);
            self.save_final();
            return Ok(BatchReport {
                foreign: vec![id.to_owned()],
                ..BatchReport::default()
            });
        }

        // Intenção antes da pausa (com o marcador, se vamos pausar). Em memória, o ativo
        // sai de `active` e a entrada com `next` entra em `pending`: qualquer regravação do
        // diário no meio da troca mantém o `next`.
        let entry = JournalEntry {
            next: Some(new),
            ..entry_of(&old)
        };
        let will_stop = !is_stopped_now(self.pid)?;
        self.active.remove(index);
        self.pending.push(entry.clone());
        if let Err(e) = self.save_journal(will_stop) {
            self.forget_pending(std::slice::from_ref(&entry));
            self.active.insert(index.min(self.active.len()), old);
            return Err(e);
        }

        match self.locked_write(std::slice::from_ref(&op), will_stop) {
            Ok(pass) => {
                self.forget_pending(std::slice::from_ref(&entry));
                let mut report = BatchReport {
                    pause: pass.pause,
                    danger_retries: pass.danger_retries,
                    timeout_retries: pass.timeout_retries,
                    ..BatchReport::default()
                };
                if pass.outcomes.first().is_some_and(Option::is_none) {
                    self.active.insert(index.min(self.active.len()), op.v);
                    report.restored.push(id.to_owned());
                } else {
                    // Os bytes mudaram na pausa: o patch deixou de ser nosso.
                    tracing::warn!(id = %id, "bytes do patch alterados por outro; tirado do registro sem escrever");
                    report.foreign.push(id.to_owned());
                }
                self.save_final();
                Ok(report)
            }
            Err(f) => {
                if matches!(f.error, Error::ProcessGone { .. }) {
                    return Err(f.error);
                }
                // Sem sobra escrita o ativo antigo volta; com sobra, a entrada com `next`
                // fica pendente para a restauração.
                if !f.dirty {
                    self.forget_pending(std::slice::from_ref(&entry));
                    self.active.insert(index.min(self.active.len()), old);
                }
                self.stuck |= f.stuck;
                self.save_logged(f.stuck);
                Err(f.error)
            }
        }
    }

    fn undo_inner(&mut self, select: impl Fn(&Validated) -> bool) -> Result<BatchReport> {
        let chosen: Vec<Validated> = self.active.iter().filter(|v| select(v)).cloned().collect();
        if chosen.is_empty() {
            return Ok(BatchReport::default());
        }
        check_alive(&self.identity)?;
        let policy = self.current_policy()?;
        if let Some(v) = chosen.iter().find(|v| !policy.allows(&v.spec.range())) {
            return Err(Error::OutsidePolicy {
                id: v.spec.id.clone(),
                addr: v.spec.addr,
            });
        }
        let ops: Vec<Op> = chosen
            .into_iter()
            .map(|v| Op {
                v,
                danger: Danger::One(Direction::Undo),
                kind: Kind::Undo,
            })
            .collect();
        let (to_write, skipped) = self.presort(ops)?;

        let mut report = BatchReport::default();
        let mut resolved: Vec<JournalEntry> = Vec::new();
        for (op, _) in &skipped {
            report.foreign.push(op.v.spec.id.clone());
            resolved.push(entry_of(&op.v));
        }
        if !to_write.is_empty() {
            let pass = self.run_pass(&to_write)?;
            report.pause = pass.pause;
            report.danger_retries = pass.danger_retries;
            report.timeout_retries = pass.timeout_retries;
            for (op, outcome) in to_write.iter().zip(&pass.outcomes) {
                match outcome {
                    None => report.restored.push(op.v.spec.id.clone()),
                    Some(_) => report.foreign.push(op.v.spec.id.clone()),
                }
                resolved.push(entry_of(&op.v));
            }
        }
        for id in &report.foreign {
            tracing::warn!(
                id = %id,
                "bytes do patch alterados por outro; tirado do registro sem escrever"
            );
        }
        self.forget(&resolved);
        self.save_final();
        Ok(report)
    }

    fn restore_inner(&mut self, entries: Vec<JournalEntry>) -> Result<RestoreReport> {
        if entries.is_empty() {
            return Ok(RestoreReport::default());
        }
        check_alive(&self.identity)?;
        for e in &entries {
            if !self.journal_entries().contains(e) {
                self.pending.push(e.clone());
            }
        }
        let policy = self.current_policy()?;
        let mut report = RestoreReport::default();
        let mut resolved: Vec<JournalEntry> = Vec::new();
        let (mut code_ops, mut cave_ops) = (Vec::new(), Vec::new());
        for e in entries {
            if !policy.allows(&e.range()) {
                // Não conta como resolvida: os bytes podem ser nossos, então a entrada
                // fica no diário para a próxima execução (spec 6.7).
                tracing::warn!(
                    id = %e.id,
                    addr = e.addr,
                    "entrada fora da política; não tocada e mantida no diário"
                );
                report.outside_policy.push(e.id.clone());
                continue;
            }
            let spec = PatchSpec {
                id: e.id.clone(),
                addr: e.addr,
                expected: e.original.clone(),
                replacement: e.replacement.clone(),
                kind: e.kind,
            };
            match validate(spec) {
                Ok(v) => {
                    let kind = v.spec.kind;
                    let op = Op {
                        v,
                        danger: Danger::Both,
                        kind: Kind::Restore(e),
                    };
                    match kind {
                        PatchKind::Code => code_ops.push(op),
                        PatchKind::Cave => cave_ops.push(op),
                    }
                }
                Err(err) => {
                    tracing::warn!(id = %e.id, erro = %err, "entrada inválida; não tocada");
                    report.foreign.push(e.id.clone());
                    resolved.push(e);
                }
            }
        }
        // Fase 1: os desvios. Fase 2: as caves, só se nenhum desvio ficou sem resolver
        // (senão ele ainda aponta para elas) e com a pausa mais paciente (spec 008, 6.3).
        self.restore_phase(code_ops, &mut report, &mut resolved)?;
        if !cave_ops.is_empty() {
            if caves_may_resolve(&self.journal_entries()) {
                self.with_cave_pause(|engine| {
                    engine.restore_phase(cave_ops, &mut report, &mut resolved)
                })?;
            } else {
                for op in cave_ops {
                    tracing::warn!(
                        id = %op.v.spec.id,
                        "há desvio sem resolver; cave não tocada e mantida no diário"
                    );
                    report.deferred.push(op.v.spec.id.clone());
                }
            }
        }
        self.forget(&resolved);
        self.save_final();
        Ok(report)
    }

    /// Uma fase da restauração: decide cada entrada, escreve as que precisam e fecha o
    /// diário (as resolvidas saem do registro). Falha por perigo ou prazo devolve o erro e
    /// mantém tudo o que esta fase tocava.
    fn restore_phase(
        &mut self,
        ops: Vec<Op>,
        report: &mut RestoreReport,
        resolved: &mut Vec<JournalEntry>,
    ) -> Result<()> {
        if !ops.is_empty() {
            let (to_write, skipped) = self.presort(ops)?;
            let mut outcomes: Vec<(Op, Option<Skip>)> =
                skipped.into_iter().map(|(op, s)| (op, Some(s))).collect();
            if !to_write.is_empty() {
                let pass = self.run_pass(&to_write)?;
                outcomes.extend(to_write.into_iter().zip(pass.outcomes));
            }
            for (op, outcome) in outcomes {
                let Kind::Restore(e) = op.kind else { continue };
                match outcome {
                    None => report.restored.push(e.id.clone()),
                    Some(Skip::Dropped) => report.dropped.push(e.id.clone()),
                    Some(Skip::Foreign) => {
                        tracing::warn!(id = %e.id, "bytes de outro na entrada do diário; não tocada");
                        report.foreign.push(e.id.clone());
                    }
                }
                resolved.push(e);
            }
        }
        // O que esta fase resolveu sai do registro já, para a conferência da fase seguinte.
        self.forget(resolved);
        self.save_final();
        Ok(())
    }

    /// Intenção no diário, passagem pausada e tratamento da falha (desfazer e restaurar).
    ///
    /// Na falha, as entradas continuam no diário (sem o marcador, salvo se o SIGCONT
    /// falhou); se sobrou byte escrito, os patches ativos envolvidos viram pendentes.
    fn run_pass(&mut self, ops: &[Op]) -> Result<Pass> {
        let will_stop = !is_stopped_now(self.pid)?;
        self.save_journal(will_stop)?;
        match self.locked_write(ops, will_stop) {
            Ok(pass) => Ok(pass),
            Err(f) => {
                if matches!(f.error, Error::ProcessGone { .. }) {
                    return Err(f.error);
                }
                if f.dirty {
                    let entries: Vec<JournalEntry> = ops.iter().map(|o| entry_of(&o.v)).collect();
                    self.active.retain(|v| !entries.contains(&entry_of(v)));
                    for e in entries {
                        if !self.pending.contains(&e) {
                            self.pending.push(e);
                        }
                    }
                }
                self.stuck |= f.stuck;
                self.save_logged(f.stuck);
                Err(f.error)
            }
        }
    }

    /// Lê os bytes atuais de cada escrita (sem pausa) e separa as que precisam escrever.
    fn presort(&self, ops: Vec<Op>) -> Result<Presorted> {
        let mut write = Vec::new();
        let mut skipped = Vec::new();
        for op in ops {
            match self.decide_now(&op)?.0 {
                Decision::Write(_) => write.push(op),
                Decision::Skip(s) => skipped.push((op, s)),
            }
        }
        Ok((write, skipped))
    }

    /// Lê os bytes atuais pelo descritor do `mem` e decide. Devolve também os bytes lidos.
    fn decide_now(&self, op: &Op) -> Result<(Decision, Vec<u8>)> {
        let mut current = vec![0u8; op.v.spec.expected.len()];
        match self.writer.read_exact(op.v.spec.addr, &mut current) {
            Ok(()) => {}
            // Entrada do diário em memória ilegível: não é nossa para mexer.
            Err(Error::Unmapped { .. }) if matches!(op.kind, Kind::Restore(_)) => {
                return Ok((Decision::Skip(Skip::Foreign), current));
            }
            Err(e) => return Err(e),
        }
        let spec = &op.v.spec;
        let decision = match &op.kind {
            Kind::Apply => {
                if current != spec.expected {
                    return Err(Error::BytesMismatch {
                        id: spec.id.clone(),
                        expected: hex(&spec.expected),
                        found: hex(&current),
                    });
                }
                Decision::Write(spec.replacement.clone())
            }
            Kind::Undo => {
                if current == spec.replacement {
                    Decision::Write(spec.expected.clone())
                } else {
                    Decision::Skip(Skip::Foreign)
                }
            }
            Kind::Swap { from } => {
                if current == *from {
                    Decision::Write(spec.replacement.clone())
                } else {
                    Decision::Skip(Skip::Foreign)
                }
            }
            Kind::Restore(e) => match decide_entry(e, &current) {
                EntryAction::Restore => Decision::Write(e.original.clone()),
                EntryAction::Drop => Decision::Skip(Skip::Dropped),
                EntryAction::Foreign => Decision::Skip(Skip::Foreign),
            },
        };
        Ok((decision, current))
    }

    /// Pausa uma vez para o lote todo, com novas tentativas (spec 6.5), e escreve.
    ///
    /// `marked` diz se o diário em disco já tem o marcador "pausado por nós". Se a pausa
    /// enviou o `SIGSTOP` sem ele, o alvo é retomado, o marcador é gravado fora da pausa
    /// e a tentativa conta como nova (por prazo).
    fn locked_write(&mut self, ops: &[Op], mut marked: bool) -> std::result::Result<Pass, Failure> {
        let need_positions = ops.iter().any(|o| has_lost(&o.v, o.danger));
        let max = self.pause_cfg.max_attempts.max(1);
        let (mut danger_retries, mut timeout_retries) = (0u32, 0u32);
        let mut last_danger: Option<String> = None;
        for attempt in 1..=max {
            if attempt > 1 {
                std::thread::sleep(self.pause_cfg.retry_wait);
            }
            check_alive(&self.identity).map_err(Failure::clean)?;
            let cfg = self.next_pause_config();
            let started = Instant::now();
            let guard = match pause(&self.pidfd, self.pid, &cfg, need_positions) {
                Ok(guard) => guard,
                Err(e) => {
                    // O gancho só dispara numa pausa que deu certo, e só ela conta.
                    match e {
                        Error::PauseTimeout { .. } => {
                            timeout_retries += 1;
                            last_danger = None;
                            continue;
                        }
                        e => return Err(Failure::clean(e)),
                    }
                }
            };
            self.pauses_ok += 1;
            if needs_marker(guard.sent_stop(), marked) {
                tracing::debug!(
                    attempt,
                    "alvo retomado antes da pausa e diário sem marcador; nova tentativa"
                );
                settle_resume(
                    Ok(Step::Resumed),
                    resume_guard(guard, self.pause_cfg.fault, &mut self.cont_fault_fired),
                )?;
                self.save_journal(true).map_err(Failure::clean)?;
                marked = true;
                timeout_retries += 1;
                last_danger = None;
                continue;
            }
            let step = self.paused_step(&guard, ops, attempt);
            let pause_len = started.elapsed();
            // O SIGCONT sai sempre por aqui (nunca pelo `Drop` do guarda), para que a falha
            // dele chegue a quem grava o diário.
            let cont = resume_guard(guard, self.pause_cfg.fault, &mut self.cont_fault_fired);
            match settle_resume(step, cont)? {
                Step::Danger(id) => {
                    danger_retries += 1;
                    last_danger = Some(id);
                }
                Step::Resumed => {
                    timeout_retries += 1;
                    last_danger = None;
                }
                Step::Wrote(outcomes) => {
                    tracing::debug!(
                        pausa_us = pause_len.as_micros() as u64,
                        danger_retries,
                        timeout_retries,
                        "lote escrito"
                    );
                    return Ok(Pass {
                        pause: pause_len,
                        danger_retries,
                        timeout_retries,
                        outcomes,
                    });
                }
            }
        }
        Err(Failure::clean(match last_danger {
            Some(id) => Error::ThreadsInPatch { id, attempts: max },
            None => Error::PauseTimeout { attempts: max },
        }))
    }

    /// Uma tentativa com o alvo parado: regra de perigo → identidade → releitura sob
    /// pausa → conferência de que tudo continua parado → escrita e releitura.
    fn paused_step(
        &self,
        guard: &PauseGuard<'_>,
        ops: &[Op],
        attempt: u32,
    ) -> std::result::Result<Step, Failure> {
        let threads = guard.threads();
        if let Some(op) = ops.iter().find(|op| {
            threads
                .iter()
                .any(|t| thread_dangerous(&op.v, op.danger, t))
        }) {
            tracing::debug!(id = %op.v.spec.id, attempt, "thread em ponto perigoso; nova tentativa");
            return Ok(Step::Danger(op.v.spec.id.clone()));
        }
        // Releitura sob pausa (passo 5), com a identidade conferida antes.
        check_alive(&self.identity).map_err(Failure::clean)?;
        let mut plan = Vec::with_capacity(ops.len());
        for op in ops {
            plan.push(self.decide_now(op).map_err(Failure::clean)?);
        }
        // Imediatamente antes de escrever: ninguém retomou o alvo durante a conferência.
        if !guard.still_stopped().map_err(Failure::clean)? {
            tracing::debug!(
                attempt,
                "alvo retomado por outro durante a pausa; nova tentativa"
            );
            return Ok(Step::Resumed);
        }
        self.write_plan(ops, plan).map(Step::Wrote)
    }

    /// Escreve e relê cada escrita do plano; na primeira falha desfaz as já escritas.
    fn write_plan(
        &self,
        ops: &[Op],
        plan: Vec<(Decision, Vec<u8>)>,
    ) -> std::result::Result<Vec<Option<Skip>>, Failure> {
        let mut outcomes = Vec::with_capacity(ops.len());
        let mut done: Vec<(u64, Vec<u8>)> = Vec::new();
        for (op, (decision, before)) in ops.iter().zip(plan) {
            let bytes = match decision {
                Decision::Skip(s) => {
                    outcomes.push(Some(s));
                    continue;
                }
                Decision::Write(bytes) => bytes,
            };
            let addr = op.v.spec.addr;
            if self.pause_cfg.fault == Some(Fault::WriteAndRollbackFail)
                && self.pauses_ok == self.pause_cfg.hook_on_pause
            {
                // Falha injetada (testes): os bytes novos ficam no jogo e o desfazer
                // "também falha", deixando restos.
                if let Err(error @ Error::ProcessGone { .. }) =
                    self.write_verified(&op.v.spec.id, addr, &bytes)
                {
                    return Err(Failure::clean(error));
                }
                return Err(Failure {
                    error: Error::Io(std::io::Error::other("falha de escrita simulada")),
                    dirty: true,
                    stuck: false,
                });
            }
            if let Err(error) = self.write_verified(&op.v.spec.id, addr, &bytes) {
                if matches!(error, Error::ProcessGone { .. }) {
                    return Err(Failure::clean(error));
                }
                tracing::warn!(id = %op.v.spec.id, erro = %error, "escrita falhou; desfazendo o lote");
                let clean = self.roll_back(&done, addr, &before);
                return Err(Failure {
                    error,
                    dirty: !clean,
                    stuck: false,
                });
            }
            done.push((addr, before));
            outcomes.push(None);
        }
        Ok(outcomes)
    }

    /// Escreve `bytes` e confere lendo de volta.
    fn write_verified(&self, id: &str, addr: u64, bytes: &[u8]) -> Result<()> {
        self.writer.write(addr, bytes).map_err(|e| match e {
            Error::PermissionDenied { .. } | Error::Unmapped { .. } => Error::WriteRefused { addr },
            e => e,
        })?;
        let mut back = vec![0u8; bytes.len()];
        self.writer.read_exact(addr, &mut back)?;
        if back != bytes {
            return Err(Error::BytesMismatch {
                id: id.to_owned(),
                expected: hex(bytes),
                found: hex(&back),
            });
        }
        Ok(())
    }

    /// Desfaz, ainda pausado, as escritas já feitas do lote e a que falhou. `true` se
    /// tudo voltou aos bytes de antes.
    fn roll_back(&self, done: &[(u64, Vec<u8>)], failed_addr: u64, failed_before: &[u8]) -> bool {
        let mut clean = true;
        let pieces = std::iter::once((failed_addr, failed_before)).chain(
            done.iter()
                .rev()
                .map(|(addr, before)| (*addr, before.as_slice())),
        );
        for (addr, before) in pieces {
            let mut current = vec![0u8; before.len()];
            if self.writer.read_exact(addr, &mut current).is_ok() && current == before {
                continue;
            }
            let restored = self.writer.write(addr, before).and_then(|()| {
                self.writer.read_exact(addr, &mut current)?;
                Ok(current == before)
            });
            if !matches!(restored, Ok(true)) {
                tracing::error!(addr, "não foi possível desfazer a escrita do lote");
                clean = false;
            }
        }
        clean
    }

    /// Cópia da configuração da pausa: o gancho só vai na `hook_on_pause`-ésima pausa
    /// bem-sucedida (nunca no `Drop`); a pausa que falha antes do gancho não conta.
    fn next_pause_config(&self) -> PauseConfig {
        let mut cfg = self.pause_cfg.clone();
        let armed = !self.in_drop && self.pauses_ok.saturating_add(1) == cfg.hook_on_pause;
        if !armed {
            cfg.hook = None;
        }
        cfg
    }

    /// Gancho de teste de um ponto sem estado próprio (fora do `Drop` e de pânico em curso).
    fn fire_hook(&self, point: HookPoint) {
        if let Some(hook) = self.pause_cfg.hook
            && !self.in_drop
            && !std::thread::panicking()
        {
            hook(point);
        }
    }

    fn fire_after_apply(&mut self) {
        if let Some(hook) = self.pause_cfg.hook
            && !self.after_apply_fired
            && !self.in_drop
            && !std::thread::panicking()
        {
            self.after_apply_fired = true;
            hook(HookPoint::AfterApply);
        }
    }

    fn current_policy(&mut self) -> Result<WritePolicy> {
        let maps = read_maps(self.pid)?;
        Ok((self.policy)(&maps))
    }

    /// Entradas do diário: os ativos e as pendentes.
    fn journal_entries(&self) -> Vec<JournalEntry> {
        self.active
            .iter()
            .map(entry_of)
            .chain(self.pending.iter().cloned())
            .collect()
    }

    /// Grava o diário com o estado em memória; sem entradas nem marcador, apaga.
    fn save_journal(&self, paused_by_us: bool) -> Result<()> {
        // `stuck` mantém o marcador em toda gravação, até um SIGCONT bem-sucedido.
        let paused_by_us = paused_by_us || self.stuck;
        let entries = self.journal_entries();
        let data: Vec<DataEntry> = self
            .data
            .iter()
            .chain(self.data_pending.iter())
            .cloned()
            .collect();
        if entries.is_empty() && data.is_empty() && self.vars.is_empty() && !paused_by_us {
            return self.store.clear();
        }
        self.store.save(&Journal {
            boot_id: self.identity.boot_id.clone(),
            pid: self.pid,
            start_time: self.identity.start_time,
            version_key: self.version_key.clone(),
            paused_by_us,
            entries,
            data,
            vars: self.vars.clone(),
        })
    }

    /// Diário final, sem marcador. A falha só vai para o log: o que está em disco
    /// continua cobrindo o que foi escrito.
    fn save_final(&self) {
        self.save_logged(false);
    }

    /// Grava o diário com o estado em memória e o marcador pedido (mantido quando o
    /// SIGCONT falhou). A falha só vai para o log.
    fn save_logged(&self, paused_by_us: bool) {
        if let Err(e) = self.save_journal(paused_by_us) {
            tracing::error!(erro = %e, "falha ao atualizar o diário");
        }
    }

    fn forget_pending(&mut self, entries: &[JournalEntry]) {
        self.pending.retain(|e| !entries.contains(e));
    }

    /// Tira entradas resolvidas do registro e das pendentes.
    fn forget(&mut self, resolved: &[JournalEntry]) {
        self.active.retain(|v| !resolved.contains(&entry_of(v)));
        self.forget_pending(resolved);
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.in_drop = true;
        // Primeiro o SIGCONT que falhou, mesmo sem nenhum patch ativo.
        match self.retry_resume() {
            Ok(()) => {}
            Err(Error::ProcessGone { .. }) => return,
            Err(e) => tracing::error!(erro = %e, "falha ao retomar o jogo na saída"),
        }
        if !self.active.is_empty() || !self.data.is_empty() || !self.vars.is_empty() {
            match self.undo_all() {
                Ok(_) => {}
                Err(Error::ProcessGone { .. }) => return,
                Err(e) => tracing::error!(erro = %e, "falha ao desfazer os patches ativos"),
            }
        }
        let rest = self.journal_entries();
        if !rest.is_empty() {
            match self.restore_entries(rest) {
                Ok(_) => {}
                Err(Error::ProcessGone { .. }) => return,
                Err(e) => tracing::error!(erro = %e, "falha ao restaurar as entradas do diário"),
            }
        }
        let data = self.data_pending.clone();
        if !data.is_empty() {
            match self.restore_data(data) {
                Ok(_) => {}
                Err(Error::ProcessGone { .. }) => return,
                Err(e) => tracing::error!(erro = %e, "falha ao restaurar os bits do diário"),
            }
        }
        // Depois de resolver o resto: se um desvio pendente saiu agora, as variáveis também.
        if !self.vars.is_empty() {
            match self.clear_vars() {
                Ok(()) => {}
                Err(Error::ProcessGone { .. }) => return,
                Err(e) => tracing::error!(erro = %e, "falha ao zerar as variáveis na saída"),
            }
        }
        if !self.journal_entries().is_empty()
            || !self.data_pending.is_empty()
            || !self.vars.is_empty()
        {
            tracing::error!(
                "alguns patches não puderam ser restaurados; a próxima execução do trainer vai restaurar"
            );
        }
    }
}

fn entry_of(v: &Validated) -> JournalEntry {
    JournalEntry {
        id: v.spec.id.clone(),
        addr: v.spec.addr,
        original: v.spec.expected.clone(),
        replacement: v.spec.replacement.clone(),
        next: None,
        kind: v.spec.kind,
    }
}

/// Decisão pura da segunda fase: `unresolved` são as entradas ainda no registro (ativas e
/// pendentes). As caves só podem ser desfeitas se nenhum desvio (`Code`) continua sem
/// resolver, porque um desvio vivo aponta para a cave.
fn caves_may_resolve(unresolved: &[JournalEntry]) -> bool {
    unresolved.iter().all(|e| e.kind != PatchKind::Code)
}

/// Regiões `rw-` privadas (sem execução, sem compartilhamento) do `maps`: onde as variáveis
/// das caves podem morar.
fn rw_policy(maps: &[MapRegion]) -> WritePolicy {
    WritePolicy::new(
        maps.iter()
            .filter(|r| r.perms.read && r.perms.write && !r.perms.exec && !r.perms.shared)
            .map(|r| r.start..r.end)
            .collect(),
    )
}

fn has_lost(v: &Validated, danger: Danger) -> bool {
    // Numa cave toda posição vale: as threads são sempre lidas.
    if v.spec.kind == PatchKind::Cave && !matches!(danger, Danger::None) {
        return true;
    }
    match danger {
        Danger::One(Direction::Apply) => !v.lost_on_apply().is_empty(),
        Danger::One(Direction::Undo) => !v.lost_on_undo().is_empty(),
        Danger::Both => !v.lost_on_apply().is_empty() || !v.lost_on_undo().is_empty(),
        Danger::None => false,
    }
}

fn point_dangerous(v: &Validated, danger: Danger, point: Option<u64>) -> bool {
    match danger {
        Danger::One(dir) => is_dangerous(v, dir, point),
        Danger::Both => {
            is_dangerous(v, Direction::Apply, point) || is_dangerous(v, Direction::Undo, point)
        }
        Danger::None => false,
    }
}

/// Algum ponto de retomada da thread é perigoso. Posição ilegível (ou `Running`, que a
/// pausa nunca devolve) segue a regra da posição desconhecida.
fn thread_dangerous(v: &Validated, danger: Danger, t: &Thread) -> bool {
    match t.pos {
        Some(pos @ ThreadPos::Stopped { .. }) => resume_points(&pos)
            .into_iter()
            .any(|p| point_dangerous(v, danger, Some(p))),
        Some(ThreadPos::Running) | None => point_dangerous(v, danger, None),
    }
}

/// Bytes em hexadecimal maiúsculo separados por espaço (ex. `"48 8B F9"`).
fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validated(expected: &[u8], replacement: &[u8]) -> Validated {
        validate(PatchSpec {
            id: "t".into(),
            addr: 0x1000,
            expected: expected.to_vec(),
            replacement: replacement.to_vec(),
            kind: PatchKind::Code,
        })
        .unwrap()
    }

    fn thread(pos: Option<ThreadPos>) -> Thread {
        Thread { tid: 1, pos }
    }

    fn io_error() -> Error {
        Error::Io(std::io::Error::other("sigcont"))
    }

    fn entry(id: &str, kind: PatchKind) -> JournalEntry {
        JournalEntry {
            id: id.into(),
            addr: 0x1000,
            original: vec![0x90],
            replacement: vec![0xCC],
            next: None,
            kind,
        }
    }

    #[test]
    fn rw_policy_takes_private_rw_regions_only() {
        use crate::maps::{MapPath, Perms};
        let region = |start: u64, end: u64, r, w, x, shared| MapRegion {
            start,
            end,
            perms: Perms {
                read: r,
                write: w,
                exec: x,
                shared,
            },
            offset: 0,
            path: MapPath::Anonymous,
        };
        let maps = [
            region(0x1000, 0x2000, true, true, false, false),
            region(0x2000, 0x3000, true, true, false, false),
            region(0x3000, 0x4000, true, false, false, false),
            region(0x4000, 0x5000, true, true, true, false),
            region(0x5000, 0x6000, true, true, false, true),
        ];
        let p = rw_policy(&maps);
        // Vizinhas `rw-` fundem; leitura, `rwx` e compartilhada ficam de fora.
        assert!(p.allows(&(0x1ff8..0x2008)));
        assert!(!p.allows(&(0x2ff8..0x3008)));
        for out in [0x3000..0x3008, 0x4000..0x4008, 0x5000..0x5008] {
            assert!(!p.allows(&out), "{out:?}");
        }
    }

    #[test]
    fn recovery_keeps_cave_while_code_unresolved() {
        // Um desvio que continua sem resolver (fora da política, por exemplo) segura a cave.
        let code = entry("desvio", PatchKind::Code);
        let cave = entry("cave", PatchKind::Cave);
        assert!(!caves_may_resolve(&[code.clone(), cave.clone()]));
        assert!(!caves_may_resolve(std::slice::from_ref(&code)));
        // Com o desvio resolvido (saiu do registro, inclusive o de bytes de outro), a cave segue.
        assert!(caves_may_resolve(std::slice::from_ref(&cave)));
        assert!(caves_may_resolve(&[]));
        assert!(caves_may_resolve(&[cave.clone(), cave]));
    }

    #[test]
    fn cave_threads_are_always_dangerous_in_both_directions() {
        let cave = validate(PatchSpec {
            id: "c".into(),
            addr: 0x1000,
            expected: vec![0xCC; 8],
            replacement: vec![0x90; 8],
            kind: PatchKind::Cave,
        })
        .unwrap();
        for danger in [
            Danger::One(Direction::Apply),
            Danger::One(Direction::Undo),
            Danger::Both,
        ] {
            assert!(has_lost(&cave, danger));
            let stopped = |pc| thread(Some(ThreadPos::Stopped { pc, syscall: None }));
            assert!(thread_dangerous(&cave, danger, &stopped(0x1000)));
            assert!(thread_dangerous(&cave, danger, &stopped(0x1007)));
            assert!(!thread_dangerous(&cave, danger, &stopped(0x1008)));
            assert!(thread_dangerous(&cave, danger, &thread(None)));
            assert!(thread_dangerous(
                &cave,
                danger,
                &thread(Some(ThreadPos::Running))
            ));
        }
        assert_eq!(entry_of(&cave).kind, PatchKind::Cave);
    }

    #[test]
    fn marker_is_needed_only_for_our_stop_without_marker() {
        assert!(needs_marker(true, false));
        assert!(!needs_marker(true, true));
        assert!(!needs_marker(false, false));
        assert!(!needs_marker(false, true));
    }

    #[test]
    fn cont_fault_injection_rules() {
        let mut fired = false;
        // Sem falha configurada, ou sem SIGSTOP nosso, nunca injeta.
        assert!(!inject_cont_fault(None, &mut fired, true));
        assert!(!inject_cont_fault(
            Some(Fault::ContFailsAlways),
            &mut fired,
            false
        ));
        assert!(!inject_cont_fault(
            Some(Fault::WriteAndRollbackFail),
            &mut fired,
            true
        ));
        // `ContFailsOnce` dispara uma vez só (e só conta quando de fato disparou).
        assert!(!inject_cont_fault(
            Some(Fault::ContFailsOnce),
            &mut fired,
            false
        ));
        assert!(!fired);
        assert!(inject_cont_fault(
            Some(Fault::ContFailsOnce),
            &mut fired,
            true
        ));
        assert!(!inject_cont_fault(
            Some(Fault::ContFailsOnce),
            &mut fired,
            true
        ));
        // `ContFailsAlways` sempre injeta.
        for _ in 0..3 {
            assert!(inject_cont_fault(
                Some(Fault::ContFailsAlways),
                &mut fired,
                true
            ));
        }
    }

    #[test]
    fn resumed_target_keeps_the_step() {
        assert!(matches!(
            settle_resume(Ok(Step::Wrote(vec![None])), Ok(())),
            Ok(Step::Wrote(_))
        ));
        let f = settle_resume(Err(Failure::clean(io_error())), Ok(()))
            .err()
            .unwrap();
        assert!(!f.stuck && !f.dirty);
    }

    #[test]
    fn failed_sigcont_keeps_the_marker() {
        // Escrito e conferido, mas o SIGCONT falhou: sobra possível e alvo parado por nós.
        let f = settle_resume(Ok(Step::Wrote(vec![None])), Err(io_error()))
            .err()
            .unwrap();
        assert!(f.stuck && f.dirty);
        // Nova tentativa (perigo ou retomado por outro) sem escrita: só parado por nós.
        for step in [Step::Danger("p".into()), Step::Resumed] {
            let f = settle_resume(Ok(step), Err(io_error())).err().unwrap();
            assert!(f.stuck && !f.dirty);
        }
        // Falha dentro da pausa: o erro original vale, e o marcador fica.
        let inner = Failure::clean(Error::WriteRefused { addr: 1 });
        let f = settle_resume(Err(inner), Err(io_error())).err().unwrap();
        assert!(f.stuck);
        assert!(matches!(f.error, Error::WriteRefused { addr: 1 }));
    }

    #[test]
    fn sigcont_to_gone_process_is_process_gone() {
        let gone = || Error::ProcessGone { pid: 7 };
        let f = settle_resume(Ok(Step::Wrote(vec![None])), Err(gone()))
            .err()
            .unwrap();
        assert!(matches!(f.error, Error::ProcessGone { pid: 7 }) && !f.stuck);
        let inner = Failure::clean(Error::WriteRefused { addr: 1 });
        let f = settle_resume(Err(inner), Err(gone())).err().unwrap();
        assert!(!f.stuck);
    }

    #[test]
    fn hex_is_upper_with_spaces() {
        assert_eq!(hex(&[0x48, 0x8b, 0x0f]), "48 8B 0F");
        assert_eq!(hex(&[]), "");
    }

    #[test]
    fn both_directions_union_of_lost_boundaries() {
        // 90 90 90 90 -> 66 90 66 90: perde +1 e +3 ao aplicar, nada ao desfazer.
        let v = validated(&[0x90; 4], &[0x66, 0x90, 0x66, 0x90]);
        assert!(has_lost(&v, Danger::Both));
        assert!(!has_lost(&v, Danger::One(Direction::Undo)));
        let at = |pc| thread(Some(ThreadPos::Stopped { syscall: None, pc }));
        assert!(thread_dangerous(&v, Danger::Both, &at(0x1001)));
        assert!(!thread_dangerous(
            &v,
            Danger::One(Direction::Undo),
            &at(0x1001)
        ));
        assert!(!thread_dangerous(&v, Danger::Both, &at(0x1002)));
        assert!(thread_dangerous(&v, Danger::Both, &thread(None)));
        assert!(!thread_dangerous(
            &v,
            Danger::One(Direction::Undo),
            &thread(None)
        ));
    }

    #[test]
    fn syscall_thread_checks_pc_minus_2() {
        let v = validated(
            &[0xBA, 0x01, 0x00, 0x00, 0x00, 0x0F, 0x05],
            &[0x0F, 0x1F, 0x80, 0x00, 0x00, 0x00, 0x00],
        );
        let in_syscall = thread(Some(ThreadPos::Stopped {
            syscall: Some(0),
            pc: 0x1007,
        }));
        let outside = thread(Some(ThreadPos::Stopped {
            syscall: None,
            pc: 0x1007,
        }));
        assert!(thread_dangerous(
            &v,
            Danger::One(Direction::Apply),
            &in_syscall
        ));
        assert!(!thread_dangerous(
            &v,
            Danger::One(Direction::Apply),
            &outside
        ));
        // `Running` nunca vem da pausa; se vier, vale como posição desconhecida.
        let running = thread(Some(ThreadPos::Running));
        assert!(thread_dangerous(
            &v,
            Danger::One(Direction::Apply),
            &running
        ));
    }
}
