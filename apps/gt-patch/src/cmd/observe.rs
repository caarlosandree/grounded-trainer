//! `gt-patch observe <padrão> <offset> <registrador> [--secs N]`: a cave de observação (spec 008,
//! 6.7.3), ferramenta de pesquisa.
//!
//! Instala uma cave que só **conta** as passagens por um ponto e guarda, num anel de variáveis, o
//! endereço de retorno (`[rsp]`) e o valor de um registrador. O ponto tem de ser aceito por uma das
//! três regras da spec (6.7.3), todas provadas pela `.pdata` e pelos bytes do disco: começo de
//! função da `.pdata`, começo de função folha, ou prefixo sem pilha. Só ali `[rsp]` é o retorno. O trainer lê o anel de
//! fora a cada 100 ms, mostra a cada segundo os chamadores distintos com a contagem e a classe do
//! valor do registrador (o nome pela reflexão, quando houver), e devolve tudo ao sair (normal,
//! Ctrl+C, SIGTERM): desvio, cave e variáveis. Uma observação por vez: a trava do diário já
//! impede dois `gt-patch` ao mesmo tempo.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::ops::Range;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use gt_core::asm::check_stack_free_prefix;
use gt_core::cave::{CAVE_MARGIN, hook_bytes};
use gt_core::engine::Engine;
use gt_core::guard;
use gt_core::mem::{MemReader, ReadMem};
use gt_core::memscan::{Verdict, scan_memory};
use gt_core::modules::{Module, require_trusted};
use gt_core::patch::{CAVE_FILL, PatchKind, PatchSpec, validate};
use gt_core::pe::{DiskImage, RuntimeFunction};
use gt_core::scan::Pattern;
use gt_core::session::Shutdown;
use gt_grounded2::GAME_EXE;
use gt_grounded2::catalog;
use gt_grounded2::caves::{self, CaveCtx, Register, var};
use gt_grounded2::policy::{cave_allocator, exec_ranges, var_region};
use gt_grounded2::resolve::Target;
use gt_grounded2::signatures::{Signatures, hook_insn};
use gt_grounded2::ue::UE_ID;
use gt_ue::read::{is_user_addr, read_u64, read_vec, u32_at};
use gt_ue::{Names, UeLayout};

use super::{begin_and_show, location, report_undone};

/// `id` da cave no diário.
pub const CAVE_ID: &str = "observe-cave";
/// `id` do desvio no diário.
pub const HOOK_ID: &str = "observe-hook";

/// De quanto em quanto tempo o anel é lido (o anel guarda só as últimas 8 passagens).
const POLL: Duration = Duration::from_millis(100);
/// De quanto em quanto tempo a tabela é mostrada.
const PRINT: Duration = Duration::from_secs(1);
/// Bytes lidos no ponto para decodificar a instrução trocada (o tamanho máximo de uma instrução x86).
const MAX_INSN: usize = 15;

// ---------------------------------------------------------------------------
// Achar e conferir o ponto
// ---------------------------------------------------------------------------

/// Acha a ocorrência **única** de `pattern` nas seções executáveis do módulo (as do disco) e devolve
/// o endereço do ponto (`ocorrência + offset`).
pub fn locate(
    target: &Target,
    module: &Module,
    disk: &DiskImage,
    pattern: &Pattern,
    offset: usize,
) -> anyhow::Result<u64> {
    let ranges = exec_ranges(module, &disk.headers);
    for range in &ranges {
        guard::check(
            &target.maps,
            &target.modules,
            &target.non_pe,
            range.start,
            range.end - range.start,
        )?;
    }
    let found = scan_memory(&target.reader, &ranges, std::slice::from_ref(pattern))?;
    let Some(scan) = found.first() else {
        bail!("a busca não devolveu resultado");
    };
    match (scan.verdict, scan.hits.first()) {
        (Verdict::Unique, Some(&hit)) => Ok(hit + offset as u64),
        (verdict, _) => bail!(
            "o padrão não é único na memória do jogo: {}",
            verdict.text(scan.unreadable_bytes)
        ),
    }
}

/// Prefixo sem pilha: no máximo este tanto de bytes entre o começo da função e o ponto.
const MAX_PREFIX: u32 = 64;

/// Por qual das três regras o ponto foi aceito (spec 008, 6.7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointKind {
    /// Regra 1: o ponto é o `start` de uma entrada da `.pdata` (sem unwind encadeado).
    PdataStart,
    /// Regra 2: começo de função folha (fora da `.pdata`, depois de `CC` ou de um `ret`).
    LeafEntry,
    /// Regra 3: prefixo sem pilha de uma função da `.pdata`.
    StackFreePrefix,
}

/// Decide se `rva` serve de ponto da observação, ou diz qual regra falhou. A observação guarda
/// `[rsp]`, que só nos pontos aceitos é o endereço de retorno. `functions` é a `.pdata` do disco
/// (em ordem de início) e `text` a `.text` do disco, que começa em `text_rva`. Nunca aceita sem
/// prova pela desmontagem ou pelos bytes do disco.
pub fn accept_point(
    functions: &[RuntimeFunction],
    text: &[u8],
    text_rva: u32,
    rva: u32,
) -> Result<PointKind, String> {
    if functions.is_empty() {
        return Err(
            "o executável em disco não tem .pdata (diretório de exceção): não dá para \
                    provar que o ponto é o começo de uma função"
                .to_string(),
        );
    }
    let offset = rva
        .checked_sub(text_rva)
        .map(|d| d as usize)
        .filter(|d| *d < text.len())
        .ok_or_else(|| "o ponto não está na seção .text".to_string())?;
    // A entrada da `.pdata` que cobre o ponto (a de maior início que ainda o alcança).
    let covering = functions
        .iter()
        .rev()
        .find(|f| f.start <= rva)
        .filter(|f| rva < f.end);
    let Some(function) = covering else {
        return leaf_entry(text, offset);
    };
    if function.chained {
        return Err(format!(
            "regra 1: a função da .pdata que cobre o ponto (começa no RVA {:#x}) tem unwind \
             encadeado (UNW_FLAG_CHAININFO): é um pedaço de outra função, e o [rsp] dela não é o \
             endereço de retorno",
            function.start
        ));
    }
    if function.start == rva {
        return Ok(PointKind::PdataStart);
    }
    stack_free_prefix(function, text, text_rva, rva)
}

/// Regra 2: o ponto fica fora de toda entrada da `.pdata` e o byte antes dele é `CC` ou o fim de
/// um `ret`.
fn leaf_entry(text: &[u8], offset: usize) -> Result<PointKind, String> {
    let before = |n: usize| offset.checked_sub(n).and_then(|at| text.get(at)).copied();
    let after_padding = before(1) == Some(0xCC);
    // `ret` (`c3`, também em `repz ret`) ou `ret imm16` (`c2 lo hi`).
    let after_ret = before(1) == Some(0xC3) || before(3) == Some(0xC2);
    if after_padding || after_ret {
        Ok(PointKind::LeafEntry)
    } else {
        Err(
            "o ponto não é o começo de uma função: não começa uma entrada da .pdata, não fica \
             dentro de nenhuma (regra 3) e o byte antes dele não é CC nem o fim de um ret \
             (regra 2, função folha); a observação guarda [rsp], que só no começo da função é o \
             endereço de retorno"
                .to_string(),
        )
    }
}

/// Regra 3: o ponto fica dentro de `function`, depois do `start`, num prefixo sem pilha.
fn stack_free_prefix(
    function: &RuntimeFunction,
    text: &[u8],
    text_rva: u32,
    rva: u32,
) -> Result<PointKind, String> {
    let distance = rva - function.start;
    if distance > MAX_PREFIX {
        return Err(format!(
            "regra 3: o ponto fica a {distance} bytes do começo da função, mais que o limite de \
             {MAX_PREFIX} do prefixo sem pilha"
        ));
    }
    let slice = |from: u32, to: u32| text.get((from - text_rva) as usize..(to - text_rva) as usize);
    let Some(code) = slice(function.start, function.end) else {
        return Err(
            "regra 3: a função da .pdata passa do fim da .text; não dá para desmontá-la"
                .to_string(),
        );
    };
    check_stack_free_prefix(code, u64::from(function.start), distance as usize)
        .map(|()| PointKind::StackFreePrefix)
        .map_err(|why| {
            format!(
                "regra 3: o ponto não é um prefixo sem pilha da função que começa no RVA {:#x}: {why}",
                function.start
            )
        })
}

/// Tudo que a observação precisa, conferido antes de escrever qualquer byte no jogo.
#[derive(Debug, Clone)]
pub struct Plan {
    /// Endereço do começo da função, onde entra o desvio.
    pub hook_at: u64,
    /// Bytes da instrução trocada (5 ou mais).
    pub insn: Vec<u8>,
    /// Folga da `.data` (as variáveis).
    pub vars: Range<u64>,
    /// Onde a cave fica e os bytes dela.
    pub cave_at: u64,
    pub cave: Vec<u8>,
    /// Registrador observado.
    pub reg: Register,
}

impl Plan {
    /// Confere o ponto `hook_at` e monta a cave (o `layout` é exigido pelo contexto das caves; a
    /// observação não o usa). Recusa se nenhuma regra de `accept_point` aceita o ponto, se a
    /// instrução tem menos de 5 bytes ou difere do disco, ou se falta espaço (variáveis ou cave).
    pub fn new(
        target: &Target,
        module: &Module,
        disk: &DiskImage,
        hook_at: u64,
        reg: Register,
        layout: UeLayout,
    ) -> anyhow::Result<Plan> {
        let rva = hook_at
            .checked_sub(module.base)
            .and_then(|d| u32::try_from(d).ok())
            .context("o ponto fica fora do módulo")?;
        let text = disk
            .headers
            .section(".text")
            .context("o executável não tem a seção .text")?;
        if !(text.rva..text.rva.saturating_add(text.virtual_size)).contains(&rva) {
            bail!("o ponto não está na seção .text");
        }
        let text_bytes = disk
            .bytes(text.rva, text.virtual_size as usize)
            .context("a seção .text sai dos limites da imagem em disco")?;
        let kind = accept_point(&disk.runtime_functions()?, text_bytes, text.rva, rva)
            .map_err(anyhow::Error::msg)?;
        tracing::info!("ponto aceito pela regra {kind:?}");
        guard::check(
            &target.maps,
            &target.modules,
            &target.non_pe,
            hook_at,
            MAX_INSN as u64,
        )?;
        let mut head = [0u8; MAX_INSN];
        target.reader.read_exact(hook_at, &mut head)?;
        let insn = hook_insn(&head, 0)?;
        if disk.bytes(rva, insn.len()) != Some(&insn[..]) {
            bail!(
                "os bytes do ponto na memória diferem do disco (outro trainer mexeu aqui?); \
                 confira com `gt diff`"
            );
        }

        let vars = var_region(module, &disk.headers, &target.maps)
            .filter(|r| r.end - r.start >= var::TOTAL)
            .context("indisponível (sem espaço para variáveis)")?;

        let text_start = module.base + u64::from(text.rva);
        let mut alloc = cave_allocator(module, disk)
            .context("a seção .text sai dos limites da imagem em disco")?;
        let ctx = |cave_at: u64| CaveCtx {
            hook_at,
            cave_at,
            insn: insn.clone(),
            vars: vars.start,
            captures: Vec::new(),
            fields: std::collections::HashMap::new(),
            layout,
        };
        // O tamanho não depende do lugar: mede, aloca e monta de novo no endereço certo.
        let probe = caves::build_observe(&ctx(text_start), reg)?;
        let cave_at = alloc.alloc(probe.len()).with_context(|| {
            format!(
                "indisponível (sem espaço para a cave: {} bytes mais {CAVE_MARGIN} de margem numa \
                 sequência de CC)",
                probe.len()
            )
        })?;
        let cave = caves::build_observe(&ctx(cave_at), reg)?;
        Ok(Plan {
            hook_at,
            insn,
            vars,
            cave_at,
            cave,
            reg,
        })
    }
}

// ---------------------------------------------------------------------------
// Instalar, ler e contar
// ---------------------------------------------------------------------------

/// Uma observação instalada no jogo.
#[derive(Debug)]
pub struct Observation {
    /// Começo das variáveis (a folga da `.data`).
    vars: u64,
}

/// O que o anel tinha numa leitura.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Passagens contadas desde a instalação.
    pub count: u64,
    /// As entradas do anel, na ordem da memória: `(retorno, registrador)`. A passagem `k` está na
    /// entrada `k % 8`.
    pub ring: Vec<(u64, u64)>,
}

impl Observation {
    /// Reclama a folga e aplica a cave e o desvio num lote só (`[Cave, Code]`, spec 008, 6.3).
    pub fn start(engine: &mut Engine, plan: &Plan) -> anyhow::Result<Observation> {
        engine.claim_vars(plan.vars.clone())?;
        let cave = validate(PatchSpec {
            id: CAVE_ID.to_string(),
            addr: plan.cave_at,
            expected: vec![CAVE_FILL; plan.cave.len()],
            replacement: plan.cave.clone(),
            kind: PatchKind::Cave,
        })?;
        let hook = validate(PatchSpec {
            id: HOOK_ID.to_string(),
            addr: plan.hook_at,
            expected: plan.insn.clone(),
            replacement: hook_bytes(plan.hook_at, plan.insn.len(), plan.cave_at)?,
            kind: PatchKind::Code,
        })?;
        engine.apply(vec![cave, hook])?;
        Ok(Observation {
            vars: plan.vars.start,
        })
    }

    /// Lê o contador e o anel (só leitura, sem pausa).
    pub fn poll(&self, engine: &Engine) -> anyhow::Result<Snapshot> {
        let word = |bytes: &[u8]| u64::from_le_bytes(bytes.try_into().unwrap_or([0; 8]));
        let count = engine.read_var(self.vars + var::OBSERVE_COUNT, 8)?;
        let ring = engine.read_var(
            self.vars + var::OBSERVE_RING,
            (var::OBSERVE_RING_LEN * var::OBSERVE_ENTRY) as usize,
        )?;
        // Cada entrada tem 16 bytes: o retorno e o registrador.
        let (entries, _) = ring.as_chunks::<16>();
        Ok(Snapshot {
            count: word(&count),
            ring: entries
                .iter()
                .map(|e| (word(&e[..8]), word(&e[8..])))
                .collect(),
        })
    }
}

/// As passagens vistas, agrupadas por (retorno, classe do registrador).
#[derive(Debug, Default)]
pub struct Tally {
    /// Passagens já contadas (as `0..consumed` do contador da cave).
    consumed: u64,
    /// Passagens que o anel já tinha sobrescrito quando foi lido.
    lost: u64,
    rows: BTreeMap<(u64, String), u64>,
}

impl Tally {
    /// Contabiliza as passagens novas de `snap`. O anel guarda as 8 últimas: as mais antigas que isso
    /// contam como perdidas. Fora da última leitura (`last`), a passagem mais recente fica para a
    /// próxima: a cave soma o contador **antes** de gravar a entrada, então ela pode estar pela
    /// metade. `class` dá o rótulo da classe do valor do registrador.
    pub fn absorb(&mut self, snap: &Snapshot, last: bool, class: &mut dyn FnMut(u64) -> String) {
        let upto = if last {
            snap.count
        } else {
            snap.count.saturating_sub(1)
        };
        if upto <= self.consumed {
            return;
        }
        let len = snap.ring.len().max(1) as u64;
        let first = self.consumed.max(upto.saturating_sub(len));
        self.lost += first - self.consumed;
        for k in first..upto {
            // `len` ≥ 1 e `k % len` < `len`: o índice existe se o anel veio inteiro.
            let Some(&(ret, value)) = snap.ring.get((k % len) as usize) else {
                self.lost += 1;
                continue;
            };
            *self.rows.entry((ret, class(value))).or_insert(0) += 1;
        }
        self.consumed = upto;
    }

    /// Passagens contabilizadas (as que o anel ainda tinha).
    pub fn seen(&self) -> u64 {
        self.rows.values().sum()
    }

    /// Passagens perdidas (anel sobrescrito entre duas leituras).
    pub fn lost(&self) -> u64 {
        self.lost
    }

    /// Linhas `(retorno, classe, contagem)`, da mais frequente para a menos.
    pub fn rows(&self) -> Vec<(u64, &str, u64)> {
        let mut rows: Vec<(u64, &str, u64)> = self
            .rows
            .iter()
            .map(|((ret, class), n)| (*ret, class.as_str(), *n))
            .collect();
        rows.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)).then(a.1.cmp(b.1)));
        rows
    }
}

/// O texto de cada segundo: o cabeçalho com o tempo e as passagens, e uma linha por (retorno, classe).
pub fn render(tally: &Tally, secs: u64, total: u64, ret_label: &dyn Fn(u64) -> String) -> String {
    let lost = if tally.lost() > 0 {
        format!(", {} perdida(s) (o anel guarda as 8 últimas)", tally.lost())
    } else {
        String::new()
    };
    let mut text = format!(
        "[{secs} s] {total} passagem(ns), {} vista(s){lost}\n",
        tally.seen()
    );
    for (ret, class, n) in tally.rows() {
        text.push_str(&format!("  {n:>6}  {}  {class}\n", ret_label(ret)));
    }
    text
}

// ---------------------------------------------------------------------------
// Classe do valor, pela reflexão
// ---------------------------------------------------------------------------

/// Resolve o nome da classe de um objeto: `ClassPrivate` do valor e o nome dela pelo `FNamePool`.
pub struct Reflection<'m, M: ReadMem + ?Sized> {
    mem: &'m M,
    layout: UeLayout,
    names: Option<Names<'m, M>>,
}

impl<'m, M: ReadMem + ?Sized> Reflection<'m, M> {
    pub fn new(mem: &'m M, layout: UeLayout, names: Option<Names<'m, M>>) -> Self {
        Self { mem, layout, names }
    }

    /// `Item`, `classe 0x…` (sem o pool de nomes) ou um motivo entre parênteses.
    pub fn class_label(&self, value: u64) -> String {
        if value == 0 {
            return "(nulo)".to_string();
        }
        if !is_user_addr(value) {
            return "(não é objeto)".to_string();
        }
        let class_at = value + u64::from(self.layout.uobject.class);
        let class = match read_u64(self.mem, class_at) {
            Ok(class) if is_user_addr(class) => class,
            Ok(_) => return "(sem classe)".to_string(),
            Err(_) => return "(ilegível)".to_string(),
        };
        self.class_name(class)
            .unwrap_or_else(|| format!("classe {class:#x}"))
    }

    fn class_name(&self, class: u64) -> Option<String> {
        let names = self.names.as_ref()?;
        let raw = read_vec(self.mem, class + u64::from(self.layout.uobject.name), 8).ok()?;
        names.fname(u32_at(&raw, 0), u32_at(&raw, 4)).ok()
    }
}

/// A reflexão do jogo, se as assinaturas e a conferência barata a reconhecem; sem ela as classes
/// saem como ponteiro. Nunca falha: é só para enfeitar a saída.
fn open_reflection<'m>(
    target: &'m Target,
    sigs: &Signatures,
    layout: UeLayout,
) -> Reflection<'m, MemReader> {
    let names = (|| {
        let scan = catalog::scan_subset(target, sigs, &[UE_ID.to_string()])
            .inspect_err(|err| tracing::warn!("sem a reflexão para nomear as classes: {err}"))
            .ok()?;
        let ue = scan.ue?;
        if let Err(detail) = &ue.cheap {
            tracing::warn!("reflexão não reconhecida ({detail}); as classes saem como ponteiro");
            return None;
        }
        Names::new(&target.reader, ue.names.addr()?)
            .inspect_err(|err| tracing::warn!("pool de nomes ilegível: {err}"))
            .ok()
    })();
    Reflection::new(&target.reader, layout, names)
}

// ---------------------------------------------------------------------------
// A sessão
// ---------------------------------------------------------------------------

/// Tempos da sessão (os testes usam valores curtos).
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// Intervalo entre as leituras do anel (e resolução da espera pelo fim do jogo).
    pub poll: Duration,
    /// Intervalo entre as tabelas mostradas.
    pub print: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            poll: POLL,
            print: PRINT,
        }
    }
}

/// Por que a observação terminou.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// Pedido de encerramento (sinal) ou fim do prazo de `--secs`.
    Requested,
    /// O jogo fechou.
    GameClosed,
}

/// Instala a observação, mostra a tabela a cada `timing.print` até um pedido de encerramento, o
/// prazo `limit` ou o fim do jogo, e **devolve tudo** (desvio, cave e variáveis), escrevendo o que
/// foi restaurado em `out`. O motor desfaz mesmo se a observação falhar no meio.
#[allow(clippy::too_many_arguments)]
pub fn session(
    engine: &mut Engine,
    plan: &Plan,
    shutdown: &Shutdown,
    limit: Option<Duration>,
    timing: Timing,
    out: &mut dyn Write,
    ret_label: &dyn Fn(u64) -> String,
    class_label: &dyn Fn(u64) -> String,
) -> anyhow::Result<()> {
    let observation = match Observation::start(engine, plan) {
        Ok(observation) => observation,
        Err(err) => {
            // Reclamou a folga e talvez escreveu a cave: o motor desfaz o que ficou.
            let _ = engine.undo_all();
            return Err(err);
        }
    };
    let _ = writeln!(
        out,
        "observando {} no ponto {} (cave em {:#x}, variáveis em {:#x}); Ctrl+C para sair",
        format!("{:?}", plan.reg).to_lowercase(),
        ret_label(plan.hook_at),
        plan.cave_at,
        plan.vars.start
    );
    let _ = out.flush();
    let watched = watch(
        engine,
        &observation,
        shutdown,
        limit,
        timing,
        out,
        ret_label,
        class_label,
    );
    let undone = engine.undo_all();
    // Se o jogo fechou, não há o que restaurar; o resto é o `report_undone` de sempre.
    report_undone(engine, undone, out)?;
    watched.map(|_| ())
}

/// O laço da observação: lê o anel, contabiliza e mostra a tabela. Não desfaz nada.
#[allow(clippy::too_many_arguments)]
fn watch(
    engine: &Engine,
    observation: &Observation,
    shutdown: &Shutdown,
    limit: Option<Duration>,
    timing: Timing,
    out: &mut dyn Write,
    ret_label: &dyn Fn(u64) -> String,
    class_label: &dyn Fn(u64) -> String,
) -> anyhow::Result<End> {
    let started = Instant::now();
    let mut next_print = started + timing.print;
    let mut tally = Tally::default();
    let show = |out: &mut dyn Write, tally: &Tally, total: u64| {
        let text = render(tally, started.elapsed().as_secs(), total, ret_label);
        let _ = write!(out, "{text}");
        let _ = out.flush();
    };
    loop {
        if shutdown.requested() || limit.is_some_and(|l| started.elapsed() >= l) {
            // A última leitura conta a passagem mais recente também.
            let snap = observation.poll(engine)?;
            tally.absorb(&snap, true, &mut |v| class_label(v));
            show(out, &tally, snap.count);
            return Ok(End::Requested);
        }
        // Um sinal no meio da espera interrompe o `poll` (`EINTR` vira `false`).
        if engine.pidfd().exited(timing.poll)? {
            return Ok(End::GameClosed);
        }
        let snap = observation.poll(engine)?;
        tally.absorb(&snap, false, &mut |v| class_label(v));
        if Instant::now() >= next_print {
            show(out, &tally, snap.count);
            next_print += timing.print;
        }
    }
}

// ---------------------------------------------------------------------------
// O comando
// ---------------------------------------------------------------------------

pub fn run(
    pid: Option<i32>,
    pattern: &str,
    offset: usize,
    register: &str,
    secs: Option<u64>,
) -> anyhow::Result<()> {
    // Os argumentos são conferidos antes de abrir o jogo ou a sessão.
    let pattern = Pattern::parse(pattern).context("padrão inválido")?;
    let reg = caves::parse_register(register)?;
    let session_state = begin_and_show()?;
    let target = Target::open(pid)?;
    tracing::info!(pid = target.pid, "jogo encontrado");
    let (module, mem_headers) = target.module(GAME_EXE)?;
    require_trusted(module)?;
    let disk = target.open_disk(module, &mem_headers)?;
    let sigs = Signatures::embedded()?;
    let layout = sigs
        .ue_for_version(&mem_headers.version_key())
        .context("as assinaturas não têm o bloco [ue]")?
        .layout;
    let hook_at = locate(&target, module, &disk, &pattern, offset)?;
    let plan = Plan::new(&target, module, &disk, hook_at, reg, layout)?;
    let reflection = open_reflection(&target, &sigs, layout);

    // Declarado depois da sessão: o motor cai antes de a trava ser solta.
    let mut engine = gt_grounded2::patching::open_engine(
        &target,
        session_state.store.clone(),
        &session_state.lock,
        module,
        disk.headers.clone(),
    )?;
    let ret_label = |addr: u64| location(&target, addr);
    let class_label = |value: u64| reflection.class_label(value);
    session(
        &mut engine,
        &plan,
        &session_state.shutdown,
        secs.map(Duration::from_secs),
        Timing::default(),
        &mut io::stdout().lock(),
        &ret_label,
        &class_label,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::Mutex;

    use gt_core::mem::MemWriter;
    use gt_testkit::module::{INVENTORY_COUNT, INVENTORY_TOTAL};
    use gt_testkit::rig::SynthRig;
    use gt_ue::synth::SynthUe;

    use super::*;

    /// Padrão do começo de `add_item`: `mov [rsp+8],rbx; mov rbx,rcx; mov eax,[rdx+StackSize]`.
    const ADD_ITEM: &str = "48 89 5c 24 08 48 89 cb 8b 82 b8 02 00 00";

    fn layout() -> UeLayout {
        UeLayout::observed()
    }

    fn plan_for(rig: &SynthRig, addr: u64, reg: Register) -> anyhow::Result<Plan> {
        Plan::new(&rig.target, &rig.module, &rig.disk, addr, reg, layout())
    }

    fn i32_at(rig: &SynthRig, addr: u64) -> i32 {
        let mut buf = [0u8; 4];
        rig.target.reader.read_exact(addr, &mut buf).unwrap();
        i32::from_le_bytes(buf)
    }

    /// `.text` da memória e do "disco" (a original), para conferir que a saída devolve tudo.
    fn text_pair(rig: &SynthRig) -> (Vec<u8>, Vec<u8>) {
        let text = rig.disk.headers.section(".text").unwrap();
        let disk = rig
            .disk
            .bytes(text.rva, text.virtual_size as usize)
            .unwrap()
            .to_vec();
        let mut mem = vec![0u8; disk.len()];
        rig.target
            .reader
            .read_exact(rig.module.base + u64::from(text.rva), &mut mem)
            .unwrap();
        (mem, disk)
    }

    // ---- partes puras ----

    fn ring(entries: &[(u64, u64)]) -> Vec<(u64, u64)> {
        let mut ring = entries.to_vec();
        ring.resize(8, (0, 0));
        ring
    }

    #[test]
    fn tally_takes_new_passes_keeps_the_latest_for_later_and_counts_lost() {
        let class = &mut |v: u64| format!("c{v}");
        let mut tally = Tally::default();
        // 3 passagens: a mais recente (a 3ª) espera a leitura seguinte.
        let snap = Snapshot {
            count: 3,
            ring: ring(&[(0xA, 1), (0xA, 1), (0xB, 2)]),
        };
        tally.absorb(&snap, false, class);
        assert_eq!(tally.rows(), vec![(0xA, "c1", 2)]);
        // Reler a mesma coisa não conta de novo; com `last`, entra a pendente.
        tally.absorb(&snap, false, class);
        assert_eq!(tally.seen(), 2);
        tally.absorb(&snap, true, class);
        assert_eq!(tally.rows(), vec![(0xA, "c1", 2), (0xB, "c2", 1)]);
        assert_eq!(tally.lost(), 0);
        // 20 passagens depois: o anel só guarda as 8 últimas (12..20); as 3..12 se perderam.
        let ring20: Vec<(u64, u64)> = (12..20u64).map(|k| (0xC, k % 2)).collect();
        let mut rotated = vec![(0, 0); 8];
        for (i, k) in (12..20u64).enumerate() {
            rotated[(k % 8) as usize] = ring20[i];
        }
        let snap = Snapshot {
            count: 20,
            ring: rotated,
        };
        tally.absorb(&snap, true, class);
        assert_eq!(tally.lost(), 9);
        assert_eq!(tally.seen(), 3 + 8);
        let text = render(&tally, 5, 20, &|a| format!("ret{a:x}"));
        assert!(
            text.starts_with("[5 s] 20 passagem(ns), 11 vista(s), 9 perdida(s)"),
            "{text}"
        );
        assert!(text.contains("ret"), "{text}");
    }

    #[test]
    fn render_sorts_by_count_and_labels_callers() {
        let mut tally = Tally::default();
        let snap = Snapshot {
            count: 6,
            ring: ring(&[
                (0x20, 7),
                (0x10, 7),
                (0x20, 8),
                (0x20, 7),
                (0x10, 7),
                (0x20, 7),
            ]),
        };
        tally.absorb(&snap, true, &mut |v| {
            if v == 7 {
                "Item".into()
            } else {
                "(nulo)".into()
            }
        });
        let text = render(&tally, 1, 6, &|a| format!("jogo+{a:x}"));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "[1 s] 6 passagem(ns), 6 vista(s)");
        assert_eq!(lines[1], "       3  jogo+20  Item");
        assert_eq!(lines[2], "       2  jogo+10  Item");
        assert_eq!(lines[3], "       1  jogo+20  (nulo)");
    }

    // ---- `accept_point`: as três regras do ponto aceito (spec 008, 6.7.3) ----

    /// Base da `.text` sintética dos testes puros.
    const TEXT_RVA: u32 = 0x1000;

    fn rf(start: u32, end: u32) -> RuntimeFunction {
        RuntimeFunction {
            start,
            end,
            chained: false,
        }
    }

    /// `.text` de 0x200 bytes todos `CC`, com `code` gravado nos RVAs dados.
    fn text_with(code: &[(u32, &[u8])]) -> Vec<u8> {
        let mut text = vec![0xCC; 0x200];
        for (rva, bytes) in code {
            let at = (rva - TEXT_RVA) as usize;
            text[at..at + bytes.len()].copy_from_slice(bytes);
        }
        text
    }

    fn accept(functions: &[RuntimeFunction], text: &[u8], rva: u32) -> Result<PointKind, String> {
        accept_point(functions, text, TEXT_RVA, rva)
    }

    /// A forma do `AddItem`: `test rdx,rdx; je far; mov [rsp+0x18],r8b`, com o ponto no `mov`
    /// (`0x1009`); o resto da função são `nop`s e um `ret`.
    fn add_item_like() -> Vec<u8> {
        let mut text = text_with(&[
            (0x1000, &[0x48, 0x85, 0xd2]),
            // je 0x1039 (rel32 = 0x30)
            (0x1003, &[0x0f, 0x84, 0x30, 0x00, 0x00, 0x00]),
            (0x1009, &[0x44, 0x88, 0x44, 0x24, 0x18]),
        ]);
        for b in &mut text[0xe..0x39] {
            *b = 0x90;
        }
        text[0x39] = 0xc3;
        text
    }

    #[test]
    fn accepts_pdata_start() {
        let text = text_with(&[(0x1000, &[0x53, 0xc3])]);
        let functions = [rf(0x1000, 0x1040)];
        assert_eq!(accept(&functions, &text, 0x1000), Ok(PointKind::PdataStart));
    }

    #[test]
    fn rejects_chained_unwind_start() {
        let text = text_with(&[(0x1000, &[0x53, 0xc3])]);
        let functions = [RuntimeFunction {
            chained: true,
            ..rf(0x1000, 0x1040)
        }];
        let err = accept(&functions, &text, 0x1000).unwrap_err();
        assert!(err.contains("encadeado"), "{err}");
        assert!(err.contains("regra 1"), "{err}");
    }

    #[test]
    fn accepts_leaf_after_cc() {
        // Folha em 0x1050, fora da `.pdata` e logo depois do preenchimento `CC`.
        let text = text_with(&[(0x1050, &[0x8b, 0x81, 0xb8, 0x02, 0x00, 0x00, 0xc3])]);
        let functions = [rf(0x1000, 0x1040)];
        assert_eq!(accept(&functions, &text, 0x1050), Ok(PointKind::LeafEntry));
    }

    #[test]
    fn accepts_leaf_after_ret() {
        let body = [0x8b, 0x81, 0xb8, 0x02, 0x00, 0x00, 0xc3];
        let functions = [rf(0x1000, 0x1040)];
        // O byte antes é o `ret` (`c3`) de outra função folha, sem preenchimento.
        let text = text_with(&[(0x104f, &[0xc3]), (0x1050, &body)]);
        assert_eq!(accept(&functions, &text, 0x1050), Ok(PointKind::LeafEntry));
        // `ret imm16` (`c2 08 00`) também fecha uma função.
        let text = text_with(&[(0x104d, &[0xc2, 0x08, 0x00]), (0x1050, &body)]);
        assert_eq!(accept(&functions, &text, 0x1050), Ok(PointKind::LeafEntry));
    }

    #[test]
    fn rejects_leaf_without_padding() {
        let functions = [rf(0x1000, 0x1040)];
        let text = text_with(&[(0x104f, &[0x90]), (0x1050, &[0x8b, 0x81, 0xb8, 0x02, 0, 0])]);
        let err = accept(&functions, &text, 0x1050).unwrap_err();
        assert!(err.contains("regra 2"), "{err}");
        assert!(err.contains("CC"), "{err}");
        // O primeiro byte da `.text` não tem byte antes.
        let err = accept(&[rf(0x1008, 0x1040)], &text, 0x1000).unwrap_err();
        assert!(err.contains("regra 2"), "{err}");
    }

    #[test]
    fn rejects_point_without_pdata_or_outside_text() {
        let text = add_item_like();
        let err = accept(&[], &text, 0x1000).unwrap_err();
        assert!(err.contains("não tem .pdata"), "{err}");
        let err = accept(&[rf(0x1000, 0x1040)], &text, 0x2000).unwrap_err();
        assert!(err.contains(".text"), "{err}");
    }

    #[test]
    fn accepts_stack_free_prefix_with_jcc() {
        let text = add_item_like();
        let functions = [rf(0x1000, 0x1040)];
        assert_eq!(
            accept(&functions, &text, 0x1009),
            Ok(PointKind::StackFreePrefix)
        );
        // O `je` do prefixo também é um ponto aceitável (3 bytes depois do `test`).
        assert_eq!(
            accept(&functions, &text, 0x1003),
            Ok(PointKind::StackFreePrefix)
        );
    }

    #[test]
    fn rejects_prefix_with_push() {
        // push rbx; mov rbx,rcx; ponto no `mov`: o `push` muda o `rsp`.
        let text = text_with(&[(0x1000, &[0x53, 0x48, 0x89, 0xcb, 0xc3])]);
        let functions = [rf(0x1000, 0x1040)];
        let err = accept(&functions, &text, 0x1001).unwrap_err();
        assert!(err.contains("regra 3"), "{err}");
        assert!(err.contains("rsp"), "{err}");
        assert!(err.contains("push"), "{err}");
        // `sub rsp,0x28` (destino `rsp`) e `call` também.
        for (name, code) in [
            ("sub", &[0x48u8, 0x83, 0xec, 0x28, 0x90][..]),
            ("call", &[0xe8, 0x00, 0x00, 0x00, 0x00, 0x90][..]),
            ("leave", &[0xc9, 0x90][..]),
        ] {
            let text = text_with(&[(0x1000, code)]);
            let at = 0x1000 + code.len() as u32 - 1;
            let err = accept(&functions, &text, at).unwrap_err();
            assert!(err.contains("rsp"), "{name}: {err}");
        }
    }

    #[test]
    fn rejects_prefix_not_on_boundary() {
        let text = add_item_like();
        let functions = [rf(0x1000, 0x1040)];
        // 0x100b cai no meio do `mov [rsp+0x18],r8b`.
        let err = accept(&functions, &text, 0x100b).unwrap_err();
        assert!(err.contains("regra 3"), "{err}");
        assert!(err.contains("fronteira"), "{err}");
    }

    #[test]
    fn rejects_prefix_over_64_bytes() {
        // 70 `nop`s: sem pilha, mas o ponto em +65 passa dos 64 bytes.
        let mut text = vec![0x90; 70];
        text.resize(0x200, 0xCC);
        let functions = [rf(0x1000, 0x1080)];
        assert_eq!(
            accept(&functions, &text, 0x1000 + 64),
            Ok(PointKind::StackFreePrefix)
        );
        let err = accept(&functions, &text, 0x1000 + 65).unwrap_err();
        assert!(err.contains("regra 3"), "{err}");
        assert!(err.contains("64"), "{err}");
    }

    #[test]
    fn rejects_point_targeted_by_inner_branch() {
        // nop x8; nop (ponto 0x1008)...; um `jmp rel8` em 0x1010 cai no ponto.
        let mut text = vec![0x90u8; 0x20];
        text.resize(0x200, 0xCC);
        // jmp 0x1008: eb f6 em 0x1010 (0x1010 + 2 - 10 = 0x1008)
        text[0x10] = 0xeb;
        text[0x11] = 0xf6;
        let functions = [rf(0x1000, 0x1020)];
        let err = accept(&functions, &text, 0x1008).unwrap_err();
        assert!(err.contains("regra 3"), "{err}");
        assert!(err.contains("desvio"), "{err}");
        // Um desvio para o `start` (laço) não conta; um para depois do ponto também não.
        text[0x11] = 0xee; // 0x1012 - 0x12 = 0x1000
        assert_eq!(
            accept(&functions, &text, 0x1008),
            Ok(PointKind::StackFreePrefix)
        );
        text[0x11] = 0x04; // 0x1016
        assert_eq!(
            accept(&functions, &text, 0x1008),
            Ok(PointKind::StackFreePrefix)
        );
        // Um `jcc` rel32 para o meio do prefixo também recusa.
        text[0x10..0x16].copy_from_slice(&[0x0f, 0x84, 0xf2, 0xff, 0xff, 0xff]); // 0x1016 - 14 = 0x1008
        let err = accept(&functions, &text, 0x1008).unwrap_err();
        assert!(err.contains("desvio"), "{err}");
    }

    #[test]
    fn rejects_function_it_cannot_disassemble_or_find() {
        // `06` é inválido em 64 bits: sem desmontar a função inteira não há prova.
        let text = text_with(&[(0x1000, &[0x90, 0x90, 0x06, 0x90])]);
        let err = accept(&[rf(0x1000, 0x1040)], &text, 0x1001).unwrap_err();
        assert!(err.contains("regra 3"), "{err}");
        assert!(err.contains("desmont"), "{err}");
        // Função que passa do fim da `.text`.
        let err = accept(&[rf(0x1000, 0x2000)], &text, 0x1001).unwrap_err();
        assert!(err.contains("regra 3"), "{err}");
        // Unwind encadeado dentro da função: o `start` não é entrada de função de verdade.
        let chained = [RuntimeFunction {
            chained: true,
            ..rf(0x1000, 0x1040)
        }];
        let err = accept(&chained, &add_item_like(), 0x1009).unwrap_err();
        assert!(err.contains("encadeado"), "{err}");
    }

    #[test]
    fn class_label_uses_the_reflection_names() {
        let mut s = SynthUe::new(layout(), 0x1000_0000, 64 << 20);
        let (object_c, engine) = (s.object_class, s.engine_package);
        let maine = s.add_package("/Script/Maine");
        let item_c = s.add_class(maine, "Item", Some(object_c), 0x40, &[]);
        let item = s.add_object(item_c, "Item_0", engine);
        let image = s.finish();
        let mem = image.into_mem();
        let names = Names::new(&mem, image.names).unwrap();
        let named = Reflection::new(&mem, layout(), Some(names));
        assert_eq!(named.class_label(item), "Item");
        assert_eq!(named.class_label(0), "(nulo)");
        assert_eq!(named.class_label(0x10), "(não é objeto)");
        assert_eq!(named.class_label(0x7000_0000_0000), "(ilegível)");
        // Sem o pool de nomes, a classe sai como ponteiro.
        let bare = Reflection::new(&mem, layout(), None);
        assert_eq!(bare.class_label(item), format!("classe {item_c:#x}"));
    }

    // ---- contra o módulo sintético ----

    #[test]
    fn observe_lists_callers_and_restores() {
        let mut rig = SynthRig::new();
        let pattern = Pattern::parse(ADD_ITEM).unwrap();
        let addr = locate(&rig.target, &rig.module, &rig.disk, &pattern, 0).unwrap();
        assert_eq!(addr, rig.t.symbol("add_item"));
        // O offset é somado à ocorrência.
        let shifted = Pattern::parse("48 89 cb 8b 82 b8 02 00 00").unwrap();
        assert_eq!(
            locate(&rig.target, &rig.module, &rig.disk, &shifted, 0).unwrap() - 5,
            addr
        );
        let plan = plan_for(&rig, addr, Register::RDX).unwrap();
        assert_eq!(plan.insn, [0x48, 0x89, 0x5c, 0x24, 0x08]);

        let (before_text, disk_text) = text_pair(&rig);
        assert_eq!(before_text, disk_text);
        let mut engine = rig.engine();
        let observation = Observation::start(&mut engine, &plan).unwrap();
        // O desvio e a cave entraram no mesmo lote e estão no diário.
        assert!(engine.active().iter().any(|p| p.id == CAVE_ID));
        assert!(engine.active().iter().any(|p| p.id == HOOK_ID));
        assert_eq!(engine.vars().len(), 1);

        let (inv_a, item_a) = (rig.t.symbol("inventory_a"), rig.t.symbol("item_a"));
        let (inv_b, item_b) = (rig.t.symbol("inventory_b"), rig.t.symbol("item_b"));
        for _ in 0..3 {
            rig.t.call("add_item", &[inv_a, item_a]);
        }
        for _ in 0..2 {
            rig.t.call("add_item", &[inv_b, item_b]);
        }
        // A cave só observa: as funções somam como sem ela (pilhas 1 e 5).
        assert_eq!(i32_at(&rig, inv_a + INVENTORY_TOTAL as u64), 3);
        assert_eq!(i32_at(&rig, inv_b + INVENTORY_TOTAL as u64), 10);
        assert_eq!(i32_at(&rig, inv_b + INVENTORY_COUNT as u64), 2);

        let snap = observation.poll(&engine).unwrap();
        assert_eq!(snap.count, 5);
        let reflection = Reflection::new(&rig.target.reader, layout(), None);
        let mut tally = Tally::default();
        tally.absorb(&snap, true, &mut |v| reflection.class_label(v));
        let class_item = rig.t.symbol("class_item");
        let class_sub = rig.t.symbol("class_item_sub");
        let rows = tally.rows();
        // Um só chamador (o laço de chamadas do alvo); duas classes de `rdx`: `Item` e a subclasse.
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0].2, 3);
        assert_eq!(rows[0].1, format!("classe {class_item:#x}"));
        assert_eq!(rows[1].2, 2);
        assert_eq!(rows[1].1, format!("classe {class_sub:#x}"));
        assert_eq!(rows[0].0, rows[1].0, "o mesmo ponto de chamada");
        assert!(
            !(rig.module.base..rig.module.end()).contains(&rows[0].0),
            "o retorno fica fora do módulo (no alvo de teste)"
        );
        let text = render(&tally, 1, snap.count, &|a| location(&rig.target, a));
        assert!(text.contains("5 passagem(ns), 5 vista(s)"), "{text}");

        // Sair devolve tudo: desvio, cave (a `.text` volta igual ao disco) e variáveis zeradas.
        let undone = engine.undo_all().unwrap();
        assert!(undone.restored.iter().any(|id| id == HOOK_ID));
        assert!(undone.restored.iter().any(|id| id == CAVE_ID));
        let (after_text, disk_text) = text_pair(&rig);
        assert_eq!(after_text, disk_text);
        let slack = rig.var_region();
        let mut buf = vec![0u8; (slack.end - slack.start) as usize];
        rig.target.reader.read_exact(slack.start, &mut buf).unwrap();
        assert!(buf.iter().all(|b| *b == 0));
        assert!(engine.vars().is_empty() && engine.active().is_empty());
        // Depois de sair, a função segue funcionando.
        assert_eq!(rig.t.call("add_item", &[inv_a, item_a]) as i32, 4);
    }

    #[test]
    fn observe_refuses_mid_function() {
        let rig = SynthRig::new();
        let reg = Register::RCX;
        // O ponto do Brain Power é a 36ª posição de uma função, depois de `push`es: nenhuma regra
        // o aceita.
        for name in ["level_point", "level_pattern"] {
            let err = plan_for(&rig, rig.t.symbol(name), reg)
                .unwrap_err()
                .to_string();
            assert!(err.contains("regra 3"), "{name}: {err}");
        }
        // No meio de uma instrução de `sum` (depois do `mov [rsp+8],rbx`): fora da fronteira.
        let err = plan_for(&rig, rig.t.symbol("sum") + 3, reg)
            .unwrap_err()
            .to_string();
        assert!(err.contains("fronteira"), "{err}");
        // Uma instrução depois do começo de `sum`: o prefixo (`mov [rsp+8],rbx`) não mexe no `rsp`,
        // então o ponto passa pelas regras; quem recusa é a instrução de 3 bytes (`lea`).
        let err = plan_for(&rig, rig.t.symbol("sum") + 5, reg)
            .unwrap_err()
            .to_string();
        assert!(!err.contains("regra"), "{err}");
        assert!(err.contains("precisa de 5"), "{err}");
        // Os começos de função (pela `.pdata` e folha) servem.
        for name in ["sum", "add_item", "stack_plus", "getter", "leaf"] {
            plan_for(&rig, rig.t.symbol(name), reg).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
        // Fora do módulo.
        let err = plan_for(&rig, 0x1000, reg).unwrap_err().to_string();
        assert!(err.contains("fora do módulo"), "{err}");
        // Nada foi escrito: o diário não existe e a `.text` está igual ao disco.
        let (mem, disk) = text_pair(&rig);
        assert_eq!(mem, disk);
        assert_eq!(rig.store.peek().unwrap(), gt_core::journal::Loaded::Missing);
    }

    #[test]
    fn observe_counts_on_leaf_entry() {
        // `leaf` não tem entrada na `.pdata` (função folha) e vem depois de um `CC`.
        let mut rig = SynthRig::new();
        let leaf = rig.t.symbol("leaf");
        let functions = rig.disk.functions().unwrap();
        let leaf_rva = (leaf - rig.module.base) as u32;
        assert!(
            !functions
                .iter()
                .any(|f| f.start <= leaf_rva && leaf_rva < f.end)
        );
        let plan = plan_for(&rig, leaf, Register::RCX).unwrap();
        let mut engine = rig.engine();
        let observation = Observation::start(&mut engine, &plan).unwrap();
        let item = rig.t.symbol("item_b");
        for _ in 0..3 {
            // `leaf(item)` devolve o `StackSize` do item (5): a cave não muda o resultado.
            assert_eq!(rig.t.call("leaf", &[item]) as i32, 5);
        }
        let snap = observation.poll(&engine).unwrap();
        assert_eq!(snap.count, 3);
        let reflection = Reflection::new(&rig.target.reader, layout(), None);
        let mut tally = Tally::default();
        tally.absorb(&snap, true, &mut |v| reflection.class_label(v));
        let rows = tally.rows();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].2, 3);
        assert!(
            !(rig.module.base..rig.module.end()).contains(&rows[0].0),
            "o retorno é o do chamador, fora do módulo"
        );
        engine.undo_all().unwrap();
        let (mem, disk) = text_pair(&rig);
        assert_eq!(mem, disk);
        assert_eq!(rig.t.call("leaf", &[item]) as i32, 5);
    }

    #[test]
    fn observe_refuses_register_pattern_and_bytes_it_cannot_trust() {
        let rig = SynthRig::new();
        // Registrador que não é de 64 bits.
        assert!(caves::parse_register("eax").is_err());
        // Padrão ausente e padrão ambíguo (`mov [rsp+8],rbx` começa várias funções).
        let absent = Pattern::parse("de ad be ef de ad be ef").unwrap();
        let err = locate(&rig.target, &rig.module, &rig.disk, &absent, 0).unwrap_err();
        assert!(err.to_string().contains("não é único"), "{err}");
        let many = Pattern::parse("48 89 5c 24 08").unwrap();
        let err = locate(&rig.target, &rig.module, &rig.disk, &many, 0).unwrap_err();
        assert!(err.to_string().contains("não é único"), "{err}");
        // Outro trainer trocou o começo de `sum`: os bytes diferem do disco.
        let sum = rig.t.symbol("sum");
        MemWriter::open(rig.t.pid)
            .unwrap()
            .write(sum + 4, &[0x09])
            .unwrap();
        let err = plan_for(&rig, sum, Register::RCX).unwrap_err().to_string();
        assert!(err.contains("diferem do disco"), "{err}");
    }

    /// `Write` que várias threads leem de fora.
    #[derive(Clone, Default)]
    struct Shared(std::sync::Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn observe_session_prints_every_tick_and_restores_on_shutdown() {
        let mut rig = SynthRig::new();
        let plan = plan_for(&rig, rig.t.symbol("add_item"), Register::RDX).unwrap();
        let mut engine = rig.engine();
        let shutdown = Shutdown::manual();
        let out = Shared::default();
        let (inv, item) = (rig.t.symbol("inventory_a"), rig.t.symbol("item_a"));
        let reflection = Reflection::new(&rig.target.reader, layout(), None);
        let timing = Timing {
            poll: Duration::from_millis(10),
            print: Duration::from_millis(50),
        };
        let result = std::thread::scope(|scope| {
            // O "jogo" chama a função enquanto a sessão observa e pede para sair depois.
            let caller = scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(80));
                for _ in 0..4 {
                    rig.t.call("add_item", &[inv, item]);
                }
                std::thread::sleep(Duration::from_millis(150));
                shutdown.request();
            });
            let mut sink = out.clone();
            let ret = |a: u64| format!("{a:#x}");
            let class = |v: u64| reflection.class_label(v);
            let result = session(
                &mut engine,
                &plan,
                &shutdown,
                None,
                timing,
                &mut sink,
                &ret,
                &class,
            );
            caller.join().unwrap();
            result
        });
        result.unwrap();
        let text = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
        assert!(text.contains("observando rdx no ponto"), "{text}");
        assert!(text.contains("4 passagem(ns), 4 vista(s)"), "{text}");
        assert!(
            text.matches("[").count() >= 2,
            "uma tabela por tick: {text}"
        );
        assert!(text.contains(&format!("restaurado: {HOOK_ID}")), "{text}");
        assert!(text.contains(&format!("restaurado: {CAVE_ID}")), "{text}");
        let (mem, disk) = text_pair(&rig);
        assert_eq!(mem, disk);
        assert!(engine.active().is_empty() && engine.vars().is_empty());
    }

    #[test]
    fn observe_session_stops_by_itself_after_the_limit() {
        let rig = SynthRig::new();
        let plan = plan_for(&rig, rig.t.symbol("stack_plus"), Register::RCX).unwrap();
        let mut engine = rig.engine();
        let mut out = Vec::new();
        let ret = |a: u64| format!("{a:#x}");
        let class = |v: u64| format!("{v:#x}");
        let started = Instant::now();
        session(
            &mut engine,
            &plan,
            &Shutdown::manual(),
            Some(Duration::from_millis(200)),
            Timing {
                poll: Duration::from_millis(10),
                print: Duration::from_millis(100),
            },
            &mut out,
            &ret,
            &class,
        )
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("0 passagem(ns), 0 vista(s)"), "{text}");
        assert!(text.contains("restaurado:"), "{text}");
        let (mem, disk) = text_pair(&rig);
        assert_eq!(mem, disk);
    }
}
