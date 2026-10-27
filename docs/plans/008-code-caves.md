# Plano 008 — Code caves: Brain Power, hora do dia, analisador, arrastar e coleta

> **Para agentes executores:** SUB-SKILL OBRIGATÓRIA: use superpowers:subagent-driven-development (recomendado) ou superpowers:executing-plans para executar este plano tarefa a tarefa. Os passos usam checkbox (`- [ ]`).

**Objetivo:** motor de desvios com code cave e variáveis na folga da `.data`, a cave de observação para pesquisa, e
seis cheats: `brain-power`, `analisador-infinito`, `quantidade-arrastar`, `coleta-multiplicada` (tentativa),
`pular-hora` e `congelar-hora`, fechando a Fase 6.

**Arquitetura:** o desvio e a cave são **patches** do motor da Fase 2, de dois tipos: `Code` (o de hoje, a instrução
trocada por `jmp rel32` + `NOP`) e `Cave` (bytes escritos sobre `CC`, em que **qualquer** `pc` dentro do trecho é
perigoso). Assim os dois herdam a pausa com conferência de `pc`, o diário, o desfazer e a recuperação. As variáveis
são o único tipo novo de escrita: uma região na folga da `.data`, guardada no diário só como região e zerada na
limpeza. O `gt-grounded2` carrega os `[[hook]]`, monta o corpo de cada cave em Rust (`iced-x86`) e mantém um
`HookRuntime` (instalar, valor, armar, ler o registro). Os dois cheats de hora são `[[field]]` (regras `set` e `add`).

**Tecnologias:** Rust stable (edition 2024), `iced-x86` (com `code_asm`), `serde`/`toml`/`serde_json`, `thiserror`,
`tracing`, `ratatui`.

**Spec:** `docs/specs/008-code-caves.md` (citada como "spec 6.x"). Specs 006 e 007 citadas pelo número. Quem executa
lê a spec e este plano.

## Restrições globais

- Regras do `AGENTS.md`: identificadores em inglês; comentários, docs e mensagens em pt-BR; `unsafe` só em
  `crates/gt-core/src/mem/` com `// SAFETY:`; `thiserror` nas libs e `anyhow` nos binários; sem `unwrap()`/`expect()`
  fora de testes; `tracing`, nunca `println!` em libs.
- **Offsets**: só de curinga do padrão, da reflexão, do `[ue.layout]` ou de byte fixo do padrão (spec 4). Os
  endereços de `RESEARCH.md` só aparecem em testes `#[ignore]` e na conferência manual.
- O `gt-cli` continua sem `write` (`cargo tree -p gt-cli -e normal,build,features -i gt-core` sem `write`). O
  `gt-patch observe` mora no `gt-patch`.
- Validação de toda tarefa: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`. Antes de testes do `gt-testkit`/`gt-tui`, `cargo build --workspace`. Testes que tocam o
  diário usam `tempfile::TempDir`. Testes de integração novos começam com
  `#![allow(clippy::unwrap_used, clippy::expect_used)]`.
- **Valores fixados pela spec e por este plano:**
  - cave: margem de **16** bytes `CC` intactos no começo da sequência; sequência serve se `len ≥ cave + 16`;
    `Cave` com no máximo **4096** bytes, `expected` todo `0xCC`;
  - limpeza da cave: **5** tentativas, **100 ms** entre elas;
  - variáveis: começo da folga = `align_up(fim do VirtualSize da .data, 8)`, fim = fim da página; layout fixo
    (deslocamentos a partir do começo da folga): Brain Power `0..24` (N `i32` em 0, ganho original `i32` em 4,
    aplicado `i32` em 8, `L1` `i32` em 12, `L2` `i32` em 16, sequência `u32` em 20); arrastar `24..40` (N `i32` em 24,
    aplicadas `u64` em 32); analisador `40..56` (ligado `u64` em 40, classe `u64` em 48); coleta `56..136` (N `i32` em
    56, inventário `u64` em 64, 8 retornos `u64` em 72); classe `Item` `u64` em `136`; observação `144..280` (contador
    `u64` em 144, anel de 8 × 16 bytes em 152: retorno e registrador). Total 280; corte por cheat com dependências
    (spec 6.4);
  - Brain Power: grade `points = [2, 3, 5, 10]`, padrão 2, `unit = "x"`; tabela com no máximo **256** entradas;
  - arrastar: `points = [10, 50, 100, 250, 500, 999]`, padrão 100; desarme espera **50 ms**; expira em **60 s**;
  - coleta: `points = [2, 3, 5, 10]`, padrão 2, `unit = "x"`;
  - `pular-hora`: `points = [3600.0]`, `result_max = 1000000000`, releitura `≥ v + 3600`, nota acima de
    `v + 3600 + 300`;
  - subida do `ClassPrivate` → `SuperStruct`: no máximo **16** níveis.
- **Textos exatos** (os testes procuram estes trechos): `indisponível (bytes diferentes)`,
  `indisponível (padrão incoerente)`, `indisponível (sem espaço para variáveis)`,
  `indisponível (sem espaço para a cave)`, `Brain Power: +<g> → +<a> (próximo nível em <L1>)`,
  `(+<k> ganhos não mostrados)`, `Quantidade ao arrastar: aplicada (<N>)`, `Quantidade ao arrastar: desarmado`,
  `Quantidade ao arrastar: expirou`, `armado: <N>`, `Pular 1 hora: não pegou, aperte de novo`,
  `(o relógio andou mais que o esperado)`, `+1h`.
- **Git:** branch `spec-008-code-caves` no worktree `../grounded-trainer-f6`. Commit por tarefa, depois da validação
  verde, em Conventional Commits pt-BR. O branch está rebaseado sobre o `spec-007-construcao`; antes de mesclar na
  `main`, a 007 entra primeiro.
- **Tarefas com o jogo** (10 e 17) rodam na sessão principal, com o dono, FLiNG fechado e **backup do save** pedido
  antes. Nunca `sudo` nem mudar o `ptrace_scope`.
- **Despacho** (`CLAUDE.md` global): Tarefas 1, 9 e 16 → `haiku`/`sonnet`; 2, 3, 5, 6, 7, 11, 13, 14 e 15 →
  `sonnet`; 4, 8 e 12 (montagem de x86 e caves) → `opus`. Cada subagente recebe esta seção inteira, a tarefa e as
  seções da spec citadas nela.

## Foco de revisão

1. **Atualização do jogo muda o código em volta do desvio** (padrão ainda único, outro registrador). Esperado:
   o padrão estendido não acha, ou os `disp` não batem com a reflexão → indisponível, nunca uma cave com offset velho.
   Teste na Tarefa 7 (`brain_pattern_incoherent_disp`).
2. **Thread do jogo dentro da cave na saída.** Esperado: a cave não é apagada até a thread sair; depois de 5
   tentativas fica, sem desvio, e o diário a guarda. Teste na Tarefa 6 (`cave_kept_while_thread_inside`).
3. **SIGKILL do trainer entre escrever a cave e o desvio** (ou no meio do lote). Esperado: a próxima execução devolve
   o desvio (se escrito) e depois a cave e as variáveis. Teste na Tarefa 6 (`sigkill_between_cave_and_hook`).
4. **Personagem morre ou troca de mundo com a coleta ligada.** Esperado: o ponteiro do inventário vira 0 no ciclo
   seguinte e a cave não multiplica nada até o personagem novo. Teste na Tarefa 11 (`collect_inventory_cleared_on_pawn_loss`).
5. **Arrastar armado e o jogo reinicia (ou a TUI reconecta).** Esperado: o arme não volta; a linha mostra desarmada;
   nenhum desvio é reinstalado só por reconectar. Teste na Tarefa 15 (`drag_not_rearmed_on_reconnect`).

## Mapa de arquivos

| Arquivo | Responsabilidade |
|---|---|
| `crates/gt-core/src/value.rs` | `ValueGrid` com lista de pontos e `ValueType::F64` |
| `crates/gt-core/src/patch.rs` | `PatchKind` (`Code`/`Cave`), validação e perigo por tipo |
| `crates/gt-core/src/journal.rs` | `kind` na `JournalEntry`, `VarRegion` no `Journal` |
| `crates/gt-core/src/engine.rs` | desfazer em duas fases (desvios, depois caves), variáveis, releitura "maior ou igual" |
| `crates/gt-core/src/recovery.rs` | ordem desvios → caves → variáveis na recuperação |
| `crates/gt-core/src/cave.rs` (novo) | sequências de `CC`, alocador, folga da `.data`, `jmp rel32`, relocação, conferência do código da cave |
| `crates/gt-testkit/src/bin/gt-testtarget.rs` e `src/module.rs` (novo) | módulo sintético (cabeça de arquivo e cauda anônima) com funções-alvo |
| `crates/gt-testkit/tests/hooks.rs` (novo) | testes do motor de desvios contra o módulo sintético |
| `crates/gt-grounded2/src/signatures.rs` e `signatures/generic.toml` | `[[hook]]`, `kind = "double"`, `points`, `at_least`, classe dona no salto |
| `crates/gt-grounded2/src/caves/` (novo: `mod.rs`, `observe.rs`, `analyzer.rs`, `brain.rs`, `drag.rs`, `collect.rs`) | corpo de cada cave |
| `crates/gt-grounded2/src/hooks.rs` (novo) | `HookRuntime`: resolver, instalar, valor, armar, registro, ponteiros da reflexão |
| `crates/gt-grounded2/src/fields.rs`, `player.rs` | `add` de `double`, salto com classe dona, `ResearchPoints` no `gt ue player` |
| `apps/gt-patch/src/cmd/observe.rs` (novo) | `gt-patch observe` |
| `apps/gt-cli/src/cmd/caves.rs` (novo), `diff.rs`, `ue.rs` | `gt caves`, marcação no `gt diff`, `ResearchPoints` |
| `apps/gt-tui/src/` (`model.rs`, `backend.rs`, `game.rs`, `fake.rs`, `controller.rs`, `ui.rs`) | linhas de desvio e de arme, registro |

---

### Tarefa 1: Grade por lista de pontos e `f64`

**Spec:** 6.9.

**Files:** Modify `crates/gt-core/src/value.rs`; usos de `ValueGrid` no `gt-grounded2` e na TUI (só se o tipo mudar).

**Interfaces — Produces:**
- `ValueType::F64` (`size() == 8`); `ValueType::size()` deixa de ser sempre 4.
- `ValueGrid { …, points: Option<Vec<f64>> }`: com `points`, `steps() == len − 1`, `value(k) == points[k]`,
  `nearest_k` pela lista; `min`/`max`/`step` ficam com o primeiro, o último e `0.0` (sem uso).
- `ValueGrid::encode_bytes(&self, k: u64) -> Vec<u8>` (4 ou 8 bytes); `encode` continua para os patches com `slot`.
- `ValueGrid::check()` recusa lista vazia, não estritamente crescente, não finita, `default` fora, não inteira em
  `i32`; aceita um ponto só.

- [ ] **Step 1: testes** em `value.rs`: `points_grid_walks_list` (`[2,3,5,10]`: `steps()==3`, `value(2)==5.0`,
  `nearest_k(4.0)==1`), `points_single_has_no_steps` (`[3600.0]`, `steps()==0`), `points_rejects_unsorted`
  (`[2,2,5]` e `[5,2]`), `points_rejects_non_integer_i32`, `f64_encodes_8_bytes` (`3600.0` → `3600f64.to_le_bytes()`),
  `format_unit_suffix` (`5x`). Os testes existentes não mudam.
- [ ] **Step 2:** rodar `cargo test -p gt-core value` → falha (campo `points` inexistente).
- [ ] **Step 3:** implementar.
- [ ] **Step 4:** `cargo test --workspace` → passa.
- [ ] **Step 5:** commit `feat(core): grade de valores por lista de pontos e f64`.

### Tarefa 2: Patch do tipo `Cave` no motor e no diário

**Spec:** 6.2, 6.3, 6.4 (cave).

**Files:** Modify `crates/gt-core/src/patch.rs`, `journal.rs`, `engine.rs`, `recovery.rs`; testes nos mesmos arquivos.

**Interfaces — Produces:**
- `pub enum PatchKind { Code, Cave }` (`#[default] Code`); `PatchSpec { …, kind: PatchKind }`.
- `validate`: `Code` como hoje (1–64 bytes); `Cave`: 1–`MAX_CAVE_LEN` (`4096`), `expected` todo `0xCC`,
  `replacement` decodificável inteiro.
- `is_dangerous` para `Cave`: qualquer ponto em `range.start ..= range.end − 1` é perigoso (inclusive o início), e
  `None` (thread rodando ou ilegível) é perigoso.
- `JournalEntry { …, #[serde(default)] kind: PatchKind }` (diários antigos leem como `Code`).
- `Engine::undo_all` em **duas fases**: primeiro todos os `Code`, depois os `Cave`, com
  `PauseConfig { max_attempts: 5, retry_wait: 100 ms }` na fase das caves; cave que não pôde ser desfeita fica em
  `pending` com o motivo `ThreadsInPatch`.
- Recuperação (`recovery.rs`, `Engine::restore_entries`): mesma ordem e mesma pausa (5 tentativas, 100 ms) para
  as caves; uma entrada `Cave` só é resolvida se não restar nenhuma `Code` não resolvida (senão fica no diário).

- [ ] **Step 1: testes puros**: `cave_validate_requires_cc`, `cave_validate_max_len`, `cave_any_pc_inside_is_dangerous`
  (início, meio e `None`), `code_rules_unchanged` (casos atuais), `journal_entry_without_kind_reads_as_code`
  (JSON antigo), `recovery_keeps_cave_while_code_unresolved` (decisão pura com uma `Code` estrangeira e uma `Cave`).
- [ ] **Step 2:** rodar → falha.
- [ ] **Step 3:** implementar.
- [ ] **Step 4:** `cargo test --workspace` → passa.
- [ ] **Step 5:** commit `feat(core): patch do tipo cave, desfazer em duas fases`.

### Tarefa 3: Variáveis na folga da `.data`

**Spec:** 6.4 (variável, limpeza), 6.3 (sair, recuperação).

**Files:** Modify `crates/gt-core/src/journal.rs`, `engine.rs`, `recovery.rs`; `crates/gt-grounded2/src/policy.rs`.

**Interfaces — Produces:**
- `pub struct VarRegion { pub addr: u64, pub len: u64 }`; `Journal { …, #[serde(default)] vars: Vec<VarRegion> }`.
- `gt_grounded2::policy::var_region(module: &Module, disk: &PeHeaders, maps: &[MapRegion]) -> Option<Range<u64>>`:
  começo `align_up(data.va + data.virtual_size, 8)`, fim o fim da página; exige que o trecho todo esteja em VMAs
  `rw-` dentro de `base..base+SizeOfImage` (de arquivo ou anônimas).
- `Engine::claim_vars(&mut self, region: Range<u64>) -> Result<()>`: lê, exige zeros, grava `VarRegion` no diário.
- `Engine::write_var(&mut self, addr: u64, bytes: &[u8]) -> Result<()>` e `Engine::read_var(&self, addr: u64,
  len: usize) -> Result<Vec<u8>>`: sem pausa, só dentro da região reclamada.
- Limpeza (fim do `undo_all`, `Drop`, recuperação): zera a região inteira **se** nenhum `Code` nosso estiver ativo ou
  pendente e a identidade do processo e o `maps` baterem; senão mantém a `VarRegion` no diário.

- [ ] **Step 1: testes**: `var_region_aligns_and_accepts_anonymous_tail` (maps sintético com cabeça de arquivo e
  cauda anônima), `var_region_none_when_not_rw`, `claim_vars_requires_zero`, `write_var_outside_region_refused`,
  `old_journal_without_vars_reads`, `vars_not_zeroed_while_code_active`.
- [ ] **Step 2:** rodar → falha.
- [ ] **Step 3:** implementar.
- [ ] **Step 4:** `cargo test --workspace` → passa.
- [ ] **Step 5:** commit `feat(core): variáveis de cave na folga da .data`.

### Tarefa 4: Ferramentas de cave (`cave.rs`)

**Spec:** 6.2, 6.3 (alocador).

**Files:** Create `crates/gt-core/src/cave.rs`; Modify `crates/gt-core/src/lib.rs`, `Cargo.toml` (workspace:
`iced-x86` com a feature `code_asm`).

**Interfaces — Produces:**
- `cc_runs(bytes: &[u8], base: u64, min_len: usize) -> Vec<Range<u64>>`.
- `pub struct CaveAllocator` com `new(runs: Vec<Range<u64>>) -> Self` e `alloc(&mut self, len: usize) -> Option<u64>`
  (margem de 16 no começo de cada sequência, sem sobreposição, várias caves por sequência).
- `hook_bytes(at: u64, insn_len: usize, target: u64) -> Result<Vec<u8>>`: `E9 rel32` + `NOP` até `insn_len`; erro se
  `insn_len < 5` ou distância fora de `i32`.
- `relocate(insn: &[u8], from: u64, to: u64) -> Result<Vec<u8>>`: uma instrução, RIP relativo recalculado
  (`BlockEncoder`).
- `check_cave_code(code: &[u8], at: u64, back_to: u64) -> Result<()>`: sem `call`, `ret`, `int3`/`CC` como
  instrução, e termina em `jmp back_to`.

- [ ] **Step 1: testes**: `cc_runs_finds_min_len`, `alloc_respects_margin_and_packs` (sequência de 1871: três caves
  de 150/50/60 cabem; uma de 1856 não), `hook_bytes_jmp_and_nops` (7 bytes → `E9 xx xx xx xx 90 90`),
  `hook_bytes_rejects_short`, `relocate_rip_relative` (`movss xmm1,[rip+X]` movida mantém o alvo),
  `relocate_plain_unchanged` (`mov [rsp+8],rbx`), `check_rejects_call_ret_int3`.
- [ ] **Step 2:** rodar → falha.
- [ ] **Step 3:** implementar.
- [ ] **Step 4:** `cargo test -p gt-core cave` → passa.
- [ ] **Step 5:** commit `feat(core): sequências de CC, alocador, desvio e relocação`.

### Tarefa 5: Módulo sintético no `gt-testtarget`

**Spec:** 5 (módulo sintético), 8.

**Files:** Create `crates/gt-testkit/src/module.rs`; Modify `src/bin/gt-testtarget.rs`, `src/lib.rs`.

**Interfaces — Produces:**
- `TargetOpts { …, module: bool }`: o alvo grava em `TempDir` um arquivo com cabeçalho PE mínimo (seções `.text` e
  `.data`), mapeia a cabeça dele (`r-x` para a `.text`, `rw-` para o começo da `.data`) e uma cauda **anônima**
  `rw-p` até o fim da página da `.data`, com `VirtualSize` terminando no meio da página. O arquivo é o "disco" do
  `gt diff`. O nome do módulo é `gt-synthetic.exe`.
- Na `.text`: uma sequência de 300 bytes `CC` e uma de 70; funções-alvo com prólogo de 5+ bytes, cada uma seguida de
  `CC`: `sum(a, b) -> i32` (começo `mov [rsp+8],rbx`), `level_gain(obj, gain)` (cópia da forma do `+5970e80` com a
  tabela sintética: `mov esi,[rcx+R]`, `lea`, `mov [rcx+R],eax`, ponto `mov r14,[rax+T]`), `getter(obj) -> i32`
  (`movss xmm1,[rcx+C]`…), `drag(item, n)` e `add_item(inv, item)`.
- Comandos novos do protocolo do alvo: `call <função> <args…>` (devolve o resultado), `loop <função>` (thread
  chamando em laço até `stop`), `park <addr>` (thread presa num laço num endereço, para o teste de `pc`).
- `TestTarget::module_base()`, `TestTarget::symbol(name) -> u64`.

- [ ] **Step 1: teste** `synthetic_module_is_found` (`gt_core::modules` acha `gt-synthetic.exe`, as duas seções, a
  cauda anônima no `maps`, e `call sum 2 3` devolve 5).
- [ ] **Step 2:** rodar → falha.
- [ ] **Step 3:** implementar (o código das funções-alvo é montado com `iced-x86` no alvo, ao iniciar).
- [ ] **Step 4:** `cargo build --workspace && cargo test -p gt-testkit synthetic` → passa.
- [ ] **Step 5:** commit `test(testkit): módulo sintético com .text, folga anônima e funções-alvo`.

### Tarefa 6: Testes do motor de desvios

**Spec:** 6.2, 6.3, 6.4; 8 (linhas do motor).

**Files:** Create `crates/gt-testkit/tests/hooks.rs`.

**Interfaces — Consumes:** Tarefas 2–5. Os testes montam uma cave de teste (multiplica o resultado de `sum` por uma
variável) com as ferramentas da Tarefa 4.

- [ ] **Step 1: testes**: `hook_multiplies_and_restores` (liga: `sum 2 3` = 5×N; troca N pela variável sem pausa;
  desliga: 5; sai: `.text` igual ao arquivo e folga zerada), `toggle_1000_with_thread` (thread em laço, resultado
  sempre 5 ou 5×N, sem crash), `cave_kept_while_thread_inside` (thread `park` dentro da cave: `undo_all` remove o
  desvio, mantém a cave em `pending` e a folga sem zerar; depois de soltar a thread, `restore_pending` limpa as duas),
  `sigkill_between_cave_and_hook` (hook de pausa da Tarefa 2 mata o `gt-patch-harness` depois de escrever a cave;
  a próxima execução e o `gt-patch restore` devolvem tudo), `panic_and_sigterm_restore`, `refuses_foreign_bytes`
  (instrução diferente, `CC` sobrescrito, folga não zero), `caves_safe_with_zero_vars`.
- [ ] **Step 2:** rodar → falha onde faltar ligação.
- [ ] **Step 3:** ajustar o motor para o que os testes pedirem (sem mudar as interfaces das Tarefas 2–4).
- [ ] **Step 4:** `cargo build --workspace && cargo test -p gt-testkit --test hooks` → passa.
- [ ] **Step 5:** commit `test(testkit): desvios contra o módulo sintético`.

### Tarefa 7: `[[hook]]` nas assinaturas

**Spec:** 6.2, 6.6.1 (padrão estendido), 6.7.x, 12.

**Files:** Modify `crates/gt-grounded2/src/signatures.rs`, `signatures/generic.toml`, `catalog.rs`.

**Interfaces — Produces:**
- `pub struct HookDef { id, name, category, description, module, pattern: Pattern, offset: usize,
  body: CaveBody, value: Option<ValueGrid>, action: bool }` e
  `pub enum CaveBody { Brain, Analyzer, Drag, Collect }`; `Signatures::hooks() -> &[HookDef]`.
- `PatchDef`/`HookDef` não dividem `id`.
- `catalog`: busca os `[[hook]]` com os patches; um `HookHit { def, addr, captures: Vec<u8> }` com os bytes da
  ocorrência inteira (para os curingas).
- **Sem `expected` no TOML**: a instrução do desvio é decodificada na ocorrência, em `offset`; os bytes dela na
  ocorrência são o `expected` (o padrão único já fixa os bytes que importam) e o motor os relê na pausa. Erro se a
  instrução tiver menos de 5 bytes. Regras de carga: `offset` dentro do padrão; `action` só com `Drag`; `value` com
  `points`.
- `generic.toml`: os `[[hook]]` `brain-power` (padrão da spec 6.6.1, `offset = 36`), `analisador-infinito`
  (spec 6.7.1, `offset = 0`) e `quantidade-arrastar` (padrão da `RESEARCH.md`, `offset = 0`, `action = true`).
  A coleta entra na Tarefa 12.

- [ ] **Step 1: testes**: `hooks_load` (os três), `hook_insn_too_short` (ocorrência sintética com instrução de 3 bytes no `offset`), `hook_action_only_drag`,
  `hook_id_collides_with_patch`, `brain_captures_from_hit` (ocorrência sintética → `R`, `T0`, `T1`, `L` lidos dos
  curingas), `brain_pattern_incoherent_disp` (os três `disp` de `R` diferentes entre si → erro
  `padrão incoerente`).
- [ ] **Step 2:** rodar → falha.
- [ ] **Step 3:** implementar.
- [ ] **Step 4:** `cargo test -p gt-grounded2` → passa.
- [ ] **Step 5:** commit `feat(grounded2): [[hook]] nas assinaturas`.

### Tarefa 8: Corpos das caves (observação, analisador, Brain Power, arrastar)

**Spec:** 6.6.1, 6.7.1, 6.7.2, 6.7.3, 4 (falha de página, sem `call`).

**Files:** Create `crates/gt-grounded2/src/caves/{mod,observe,analyzer,brain,drag}.rs`; teste em
`crates/gt-testkit/tests/cave_bodies.rs`.

**Interfaces — Produces:**
- `pub struct CaveCtx { pub hook_at: u64, pub cave_at: u64, pub insn: Vec<u8>, pub vars: u64,
  pub captures: Vec<u8>, pub fields: HashMap<&'static str, u32>, pub layout: UeLayout }` (`vars` = começo da folga;
  `fields`: offsets da reflexão, ex. `"ResearchPoints"`, `"StackSize"`, `"MaxPowerCapacity"`).
- `pub fn build(body: CaveBody, ctx: &CaveCtx) -> Result<Vec<u8>>` e
  `pub fn build_observe(ctx: &CaveCtx, reg: Register) -> Result<Vec<u8>>`; todo corpo passa por
  `cave::check_cave_code` antes de sair.
- Comportamento de cada corpo: spec 6.6.1 (passos 1–4, `L1`/`L2` distintos, teto de 256 entradas, sequência par),
  6.7.1 (variável "ligado", classe por `ClassPrivate`/`SuperStruct` até 16 níveis), 6.7.2 (`xchg`, contador de
  aplicadas, classe `Item`), 6.7.3 (contador e anel, só `[rsp]` desreferenciado). Variáveis zeradas = passa direto.

- [ ] **Step 1: testes** (contra as funções-alvo do módulo sintético da Tarefa 5): `brain_multiplies`,
  `brain_caps_at_l2_minus_one`, `brain_never_below_original_gain`, `brain_unsorted_table`, `brain_no_l2_caps_int32`,
  `brain_nonpositive_gain_untouched`, `brain_log_vars_and_even_seq`; `analyzer_fills_only_right_class`,
  `analyzer_off_passes_through`; `drag_applies_once_and_counts`, `drag_two_threads_apply_once`,
  `drag_wrong_class_or_null_untouched`; `observe_counts_and_rings_without_side_effects`;
  `all_bodies_pass_check` (sem `call`/`ret`/`int3`).
- [ ] **Step 2:** rodar → falha.
- [ ] **Step 3:** implementar com `iced_x86::code_asm`.
- [ ] **Step 4:** `cargo build --workspace && cargo test -p gt-testkit --test cave_bodies` → passa.
- [ ] **Step 5:** commit `feat(grounded2): caves de observação, analisador, Brain Power e arrastar`.

### Tarefa 9: `gt-patch observe`, `gt caves`, `gt diff` e `ResearchPoints`

**Spec:** 6.7.3, 6.10.

**Files:** Create `apps/gt-patch/src/cmd/observe.rs`, `apps/gt-cli/src/cmd/caves.rs`; Modify `apps/gt-cli/src/cmd/diff.rs`,
`ue.rs`, `crates/gt-grounded2/src/player.rs`, `signatures/generic.toml` (`[ue.player]`: campo `ResearchPoints`).

**Interfaces:**
- `gt-patch observe <padrão> <offset> <registrador> [--secs N]`: recusa se o ponto não for começo de função (pela
  `.pdata` do disco); instala a cave de observação, imprime a cada segundo os retornos distintos com contagem e o
  `ClassPrivate` (nome da classe pela reflexão) do registrador, e devolve tudo ao sair.
- `gt caves`: sequências de `CC` (endereço, tamanho, em uso), a folga e, pelo diário (`JournalStore::peek`, identidade
  conferida), os desvios, caves e variáveis nossos.
- `gt diff`: diferenças cobertas por entradas do diário (mesmo processo) saem marcadas `nosso (<id>)`.
- `gt ue player`: linha `ResearchPoints` pelo caminho do `game_state`.

- [ ] **Step 1: testes** (contra o módulo sintético): `observe_lists_callers_and_restores`,
  `observe_refuses_mid_function`, `caves_lists_runs_and_ours`, `diff_marks_ours`; `ue_player_shows_research_points`
  (reflexão sintética).
- [ ] **Step 2:** rodar → falha. **Step 3:** implementar. **Step 4:** `cargo test --workspace` → passa.
- [ ] **Step 5:** commit `feat(cli): observe, gt caves e desvios no gt diff`.

### Tarefa 10: Pesquisa no jogo (sessão principal, com o dono)

**Spec:** 6.7.1, 6.7.2, 6.7.4; anti-objetivo da falha de página.

Com backup do save e o FLiNG fechado, usando o `gt-patch observe`:

- [ ] **Arrastar:** observar `+5bbfe70` com `rcx`. O dono arrasta um item, move entre slots, abre o baú. Anotar
  chamadores e classes. Critério: `rcx` é sempre `Item` (ou subclasse) **e** o arrastado. Se não, procurar outro
  ponto (instrução 5+ bytes, começo de função) e registrar; sem ponto, o cheat sai e volta ao dono.
- [ ] **Analisador:** observar `+57bd6d0` com `rcx`: chamadores e classes; ler cargas e `MaxPowerCapacity` com um
  analisador cheio e um gasto. Registrar se cheio é `float(MaxPowerCapacity)`.
- [ ] **Coleta:** observar `+5941660` com `rcx` e depois com `rdx`. O dono pega do chão, colhe, pega loot, cria na
  bancada, tira do baú, solta e pega de volta. Anotar, por ação, os retornos e as classes. Achar o campo do
  inventário do personagem (`SurvivalPlayerCharacter.InventoryComponent`) e de onde vem a pilha máxima de um item.
  Critério: as três fontes separáveis de baú e de item solto por retornos (até 8) ou por pontos próprios.
- [ ] Registrar tudo no `RESEARCH.md` (versão, data, endereços, padrões dos chamadores) e as hipóteses descartadas.
- [ ] **Portão:** se a coleta não for separável, tirar a Tarefa 12 e o cheat da spec (nota na spec, seção 11) e
  avisar o dono. Mesmo para o arrastar.
- [ ] Commit `docs(research): chamadores do arrastar, analisador e AddItem`.

### Tarefa 11: `HookRuntime`

**Spec:** 6.3, 6.5, 6.6.1 (registro), 6.7.2 (armar, desarmar, expirar), 6.7.4 (inventário), 6.11.

**Files:** Create `crates/gt-grounded2/src/hooks.rs`; Modify `lib.rs`, `fields.rs` (expor o pawn resolvido do ciclo).

**Interfaces — Produces:**
- `pub struct HookRuntime` com: `resolve(&mut self, hits: &[HookHit], refl: &Reflection) -> Vec<(String, Avail)>`
  (motivos da spec: `bytes diferentes`, `padrão incoerente`, `sem espaço…`, `campo X não encontrado`);
  `enable(&mut self, engine: &mut Engine, id: &str, value_k: Option<u64>) -> Result<()>` (reclama a folga na primeira
  vez, escreve classes e valor nas variáveis, aplica `[Cave, Code]` num lote); `disable(&mut self, engine, id)`
  (só o `Code`); `set_value(&mut self, engine, id, k)` (só `write_var`); `arm(&mut self, engine, k) -> ArmOutcome`
  (`Armed`, `Disarmed`, `Applied`); `tick(&mut self, engine, pawn: Option<PawnView>, now: Instant) -> Vec<HookEvent>`.
- `pub enum HookEvent { BrainGain { gain: i32, applied: i32, l1: i32, skipped: u32 }, DragApplied(i32),
  DragExpired, DragDisarmed }`.
- O `tick`: lê o registro do Brain Power com a sequência (par e igual; salto > 2 → `skipped`); acompanha o arrastar
  (contador de aplicadas, expiração de 60 s com o desarme de 50 ms); grava o inventário do jogador na variável da
  coleta (0 sem pawn).

- [ ] **Step 1: testes** (backend de memória falso da `gt-grounded2`): `enable_writes_vars_then_patches`,
  `set_value_only_writes_var`, `disable_keeps_cave`, `brain_log_skips_counted`, `brain_log_torn_read_retries`,
  `drag_disarm_by_counter` (contador subiu → `Applied`), `drag_expires_after_60s`,
  `collect_inventory_cleared_on_pawn_loss`, `unavailable_reasons`.
- [ ] **Step 2:** rodar → falha. **Step 3:** implementar. **Step 4:** `cargo test -p gt-grounded2 hooks` → passa.
- [ ] **Step 5:** commit `feat(grounded2): HookRuntime`.

### Tarefa 12: Coleta multiplicada (só se a Tarefa 10 liberou)

**Spec:** 6.7.4.

**Files:** Create `crates/gt-grounded2/src/caves/collect.rs`; Modify `signatures/generic.toml`, `caves/mod.rs`.

- [ ] **Step 1: testes** (módulo sintético, `add_item` com chamadores sintéticos): `collect_only_player_inventory`,
  `collect_caps_at_max_stack`, `collect_max_stack_one_untouched`, `collect_zero_n_or_null_inventory_noop`,
  `collect_other_caller_untouched` (baú e item solto).
- [ ] **Step 2:** rodar → falha.
- [ ] **Step 3:** implementar com o mecanismo da Tarefa 10 (lista de retornos por padrão, ou um `[[hook]]` por
  fonte) e o `[[hook]] coleta-multiplicada` (`points = [2,3,5,10]`).
- [ ] **Step 4:** `cargo test --workspace` → passa.
- [ ] **Step 5:** commit `feat(grounded2): coleta multiplicada`.

### Tarefa 13: `add` de `double` e `pular-hora`

**Spec:** 6.8 (salto com classe dona), 6.8.2.

**Files:** Modify `crates/gt-core/src/engine.rs`, `crates/gt-grounded2/src/signatures.rs`, `fields.rs`, `player.rs`,
`generic.toml`; TUI: `backend.rs`, `game.rs`, `fake.rs`, `controller.rs` (tipos da ação).

**Interfaces — Produces:**
- `Engine::write_data_at_least(&mut self, addr: u64, bytes: &[u8], guard: &[GuardBytes], min: f64, ty: ValueType)
  -> Result<f64>` (devolve o valor relido; erro `BytesMismatch` só se `< min`).
- `FieldKind::Double`; `FieldRule::Add { field, result_max: i64, at_least: bool }`.
- `ActionOutcome::Done { before: f64, after: f64, note: Option<String> }`, `Raced { read: f64 }`; `plan_add`,
  `Backend::field_action` e a formatação com `f64` (a ciência continua igual na tela).
- Salto com classe dona: `[[field]]` aceita `owner_class` no primeiro salto da raiz `game_state`; o trainer confere
  `GameState is_a owner_class` antes de ler o campo.
- `generic.toml`: `[[field]] pular-hora` (PROGRESSÃO, `root = "game_state"`, `owner_class =
  "/Script/Maine.SurvivalGameState"`, `object = "CalendarComponent"`, `object_class =
  "/Script/Maine.CalendarComponent"`, `rule = "add"`, `kind = "double"`, `field = "CurrentTime"`, `at_least = true`,
  `result_max = 1000000000`, `points = [3600.0]`, `label = "+1h"`).

- [ ] **Step 1: testes**: `add_double_applied_at_least`, `add_double_note_above_300`, `add_double_below_not_taken`,
  `add_double_refuses_negative_and_over_max`, `owner_class_mismatch_refused` (reflexão sintética), `science_unchanged`
  (o `add` de `int` continua com releitura exata).
- [ ] **Step 2:** rodar → falha. **Step 3:** implementar. **Step 4:** `cargo test --workspace` → passa.
- [ ] **Step 5:** commit `feat: add de double e pular 1 hora`.

### Tarefa 14: `congelar-hora` (depois da 007 na `main`)

**Spec:** 6.8.1.

- [ ] **Step 0:** o branch já está sobre o `spec-007-construcao` (`5a91236`). Se a 007 tiver commits novos (ou já
  estiver na `main`), `git rebase spec-007-construcao` (ou `main`); resolver conflitos; validação verde.
- [ ] **Step 1: teste** `freeze_time_set_and_restore` (calendário sintético: `GameToRealTimeRatio` 30 → 0, desligar
  → 30; jogo mudando o campo vira original novo, regra da 006).
- [ ] **Step 2–4:** `[[field]] congelar-hora` (`set`, `kind = "float"`, `target = 0.0`, mesmo salto da Tarefa 13);
  `cargo test --workspace` → passa.
- [ ] **Step 5:** commit `feat(grounded2): congelar hora do dia`.

### Tarefa 15: TUI

**Spec:** 6.11, 6.7.2, 6.6.1 (registro), 6.8.2.

**Files:** Modify `apps/gt-tui/src/model.rs`, `backend.rs`, `game.rs`, `fake.rs`, `controller.rs`, `ui.rs`.

**Interfaces:** linhas novas no `CheatRow`: desvio (`[ON ]`/`[OFF]`, valor com `←→` pela grade) e arme (`[ação]`,
`armado: N` quando o mostrado difere). `Backend` ganha `hook_enable`, `hook_disable`, `hook_set_value`, `hook_arm`,
`hook_tick`, implementados no `game.rs` com o `HookRuntime` e no `fake.rs` em memória. O controlador põe os
`HookEvent` no registro com os textos exatos e chama `hook_tick` no ciclo de 100 ms.

- [ ] **Step 1: testes** (backend falso): `hook_row_toggle_and_value`, `hook_row_verifying_until_reflection`,
  `hook_rearmed_after_reconnect` (desvio ligado volta), `drag_not_rearmed_on_reconnect`, `drag_arm_disarm_expire_texts`,
  `drag_arm_refused_when_stuck`, `brain_log_line_and_skipped`, `pular_hora_not_taken_text`, `action_window_1s` para o
  arme.
- [ ] **Step 2:** rodar → falha. **Step 3:** implementar. **Step 4:** `cargo build --workspace && cargo test
  --workspace` → passa.
- [ ] **Step 5:** commit `feat(tui): desvios, arme e registro do Brain Power`.

### Tarefa 16: Documentos

**Spec:** 10.

- [ ] `ARCHITECTURE.md` 5.5 (patch `Cave`, desfazer em duas fases, variáveis, alocador, observação), 5.3 (`[[hook]]`,
  `points`, `add` de `double`, `at_least`, `owner_class`), 5.4 (`kind`, `vars` no diário), registro de decisões
  (incluindo: desvio e cave como patches do motor da Fase 2; cave escrita sob pausa, mais estrito que a spec).
- [ ] `AGENTS.md` 3: ressalva dos desvios (spec 7).
- [ ] Notas nas specs 005 e 006; `ROADMAP.md` (Fase 6 com os cheats; god mode por desvio fora).
- [ ] Commit `docs: Fase 6 na arquitetura, regras e roadmap`.

### Tarefa 17: Sessão com o jogo e critério de pronto (sessão principal, com o dono)

- [ ] Testes `#[ignore]`: `cargo test --workspace -- --ignored` com o jogo aberto (`brain-power` liga e desliga
  parado; depois do `undo_all`, `gt diff` sem diferença e `gt caves` sem nada nosso).
- [ ] Spec seção 9, itens 1 a 11, com o dono pela TUI; registrar no `RESEARCH.md` (efeito de cada cheat, sono com a
  hora congelada, pilha acima do máximo, save).
- [ ] Atualizar `ROADMAP.md` (Fase 6 fechada com o item 2) e a spec (estado).
- [ ] Commit `docs(research): critério de pronto da spec 008 no jogo e Fase 6 fechada`.
