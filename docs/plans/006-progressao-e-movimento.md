# Plano 006 — Carga, dano, ciência e movimento (reflexão do Unreal)

> **Para agentes executores:** SUB-SKILL OBRIGATÓRIA: use superpowers:subagent-driven-development (recomendado) ou superpowers:executing-plans para executar este plano tarefa a tarefa. Os passos usam checkbox (`- [ ]`).

**Objetivo:** seis cheats de valor novos (carga, dano, somar ciência, velocidade, pulos infinitos e gravidade), com
duas regras novas (`set`, ajuste com o original devolvido, e `add`, ação única), valor ajustável nos `[[field]]`, tipo
`int` e a raiz `game_state`, fechando a Fase 5.

**Arquitetura:** o `gt-core` separa a grade de valores do `slot` dos patches e generaliza a entrada de dados do diário
(modo bits de 1 byte, como hoje, e modo inteiro de 1 a 8 bytes com `next`), com a escrita `set` no motor e na
recuperação. O `gt-grounded2` carrega as regras novas, resolve o tipo `int` e a raiz `game_state`, isola a falta de um
componente por cheat, planeja o `set` e o `add` em funções puras e os executa no `FieldRuntime`. A TUI ganha valor
ajustável nas linhas de valor e a linha `[ação]`, com o limite de uma execução por segundo e a procura do controller
pedida pela ação.

**Tecnologias:** Rust stable (edition 2024), `serde`/`toml`/`serde_json`, `thiserror`, `tracing`, `ratatui`.

**Spec:** `docs/specs/006-progressao-e-movimento.md` (citada como "spec 6.x"). Spec 005 citada como "spec 005 6.x" e
spec 003 como "spec 003 6.x". O plano 005 (`docs/plans/005-sobrevivencia-por-reflexao.md`) descreve o código que este
estende. Quem executa lê a spec e este plano.

## Restrições globais

- Regras do `AGENTS.md`: identificadores em inglês; comentários, docs e mensagens em pt-BR; `unsafe` só em
  `crates/gt-core/src/mem/` com `// SAFETY:`; `thiserror` nas libs e `anyhow` nos binários; sem `unwrap()`/`expect()`
  fora de testes; `tracing`, nunca `println!` em libs.
- **Nenhum offset de campo do jogo em código de produção nem nas assinaturas.** Campos por nome, da reflexão. Os
  offsets de `RESEARCH.md` só aparecem em testes `#[ignore]` e na conferência manual.
- Regra de dependência inalterada; o `gt-cli` continua sem `write`
  (`cargo tree -p gt-cli -e normal,build,features -i gt-core` sem `write` nem `test-support`).
- Validação de toda tarefa: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`. Antes de testes do `gt-testkit`/`gt-tui`, `cargo build --workspace` (o `bin_path` precisa
  do `gt-testtarget`). Testes que tocam o diário usam `tempfile::TempDir`.
- **Valores fixados por este plano:**
  - sanidade do valor do jogo: finito e em `[-1e9, 1e9]` (também para `int`);
  - "jogo brigando": mais de **5** originais novos em **10 s**, janela deslizante, por cheat e objeto;
  - ação: janela de **1 s** depois de uma execução que escreveu; recusas no registro no máximo **1 por segundo por
    motivo**; soma em `i64`;
  - troca de valor: o intervalo de **150 ms** já existente (`SWAP_INTERVAL`); manutenção no ciclo de 100 ms;
  - os seis `[[field]]` com os valores da spec 6.7 (tabela na Tarefa 4).
- **Textos exatos** (os testes procuram estes trechos):
  - marcas de linha: `valor do jogo inválido`, `o jogo não deixa`, `aguardando o personagem`, `aguardando o pet`;
  - linha de ação: `[ação]` seguido da quantidade formatada pela grade (`+1000`);
  - recusas da ação: `estado do jogo ausente`, `procurando o personagem`, `aguarde a procura`, `valor atual fora da
    faixa`, `passaria do máximo`; descarte: `Somar ciência: aguarde`;
  - eventos: `<nome>: o jogo mudou o valor para <X>; novo original`, `<nome>: o jogo não deixa`, `<nome>: valor do
    jogo inválido`, `<nome>: <antes> → <depois>`, `<nome>: o jogo mudou o valor durante a escrita (lido <X>)`,
    `personagem encontrado: aperte de novo para somar`, `personagem não encontrado`.
  - O `<nome>` é o `name` do `[[field]]`; a quantidade e os valores usam `ValueGrid::format`.
- **Git:** branch `fase-5-progressao` (já criado, com a spec em `0c140b7`). Commit por tarefa, depois da validação
  verde, em Conventional Commits pt-BR, sem linhas de atribuição.
- **Tarefa com o jogo** (14) roda na sessão principal, com o dono. Antes dela, pedir o **backup do save**. Nunca
  `sudo` nem mudar o `ptrace_scope`. FLiNG fechado.
- **Despacho:** cada subagente recebe esta seção inteira, a tarefa e as seções da spec citadas no cabeçalho dela.
- Testes de integração novos começam com `#![allow(clippy::unwrap_used, clippy::expect_used)]`.

## Foco de revisão

1. **O jogo mexe no campo entre a leitura do `set` e a escrita** (um upgrade no mesmo ciclo). Esperado: a releitura
   depois da escrita não bate, e o ciclo seguinte trata o valor como "jogo mudou o campo", sem escrever um original
   velho. Teste `set_reread_mismatch_next_tick_new_original` (Tarefa 7).
2. **Diário de antes desta fase com uma entrada de bits pendente** (SIGKILL na sessão da 005, recuperado agora).
   Esperado: lido como modo bits de 1 byte e restaurado como antes. Teste `journal_005_bits_entry_restores` (Tarefa 2).
3. **Valor do jogo `NaN` ou com bits de `-0.0` no campo de um `set`.** Esperado: comparação por bytes; `NaN` vira
   `valor do jogo inválido`, sem escrever. Teste `plan_set_nan_and_negative_zero` (Tarefa 5).
4. **Segurar `Enter` na ação com o terminal em repetição automática.** Esperado: no máximo uma soma por segundo e no
   máximo uma linha `aguarde` por segundo. Teste `action_key_repeat_one_per_second` (Tarefa 11).
5. **Pawn novo no mesmo endereço do antigo** (morte e recriação). Esperado: guarda diferente resolve a entrada antiga e
   cria uma nova com o original lido do objeto novo. Teste `set_same_address_new_guard` (Tarefa 3).

---

## Mapa de arquivos

```
crates/gt-core/
  src/value.rs              ValueGrid (grade: passos, valor, formatação, codificação) e ValueSpec { grid, slot }
  src/journal.rs            DataEntry com modo, bytes, next; decide_data por modo; serde compatível com a 005
  src/engine.rs             set_word, restore_field (antes restore_bits), identidade por (id, endereço)
  src/recovery.rs           entradas inteiras pela mesma decisão
crates/gt-testkit/
  src/bin/gt-patch-harness.rs   subcomando set-word
  src/ue.rs                 cena sintética com Outer/Level/World/GameState, carga, movimento e campos int
  tests/engine_live.rs, harness.rs, fields_live.rs
crates/gt-grounded2/
  src/signatures.rs         kind int, rule set/add, [field.value], root, campos do game_state no [ue.player]
  signatures/generic.toml   os seis [[field]] e os campos novos do [ue.player]
  src/player.rs             int_field, resolução de set/add/game_state, isolamento por cheat, plan_set, plan_add,
                            read_game_state, relatório
  src/fields.rs             execução do set, briga com o jogo, desired k, run_action
apps/gt-tui/src/
  model.rs, ui.rs           ValueGrid nas linhas, [ação], marcas novas, contagem sem a ação
  backend.rs, values.rs, game.rs, fake.rs, raw.rs   field_on com k, set_value de valor, field_action, pedido de procura
  controller.rs             tecla da ação, janela de 1 s, recusas no registro, linha personagem:
apps/gt-cli/src/            gt ue player com os campos novos
docs/                       ARCHITECTURE, AGENTS, specs 002/003/005 (notas), ROADMAP, RESEARCH
```

---

### Tarefa 1: Grade de valores separada do `slot`

Spec 6.5.

**Arquivos:** modificar `crates/gt-core/src/value.rs`, `crates/gt-grounded2/src/signatures.rs` e, na TUI,
`backend.rs`, `model.rs`, `ui.rs`, `controller.rs`, `raw.rs` e `tests/core_raw.rs`.

**Interfaces (produz):**
- `pub struct ValueGrid` com os mesmos campos e tipos que o `ValueSpec` tem hoje, menos o `slot`, e os métodos que hoje são do `ValueSpec` e não dependem do `slot`: `steps`, `value`, `nearest_k`, `encode`,
  `format` e `check`.
- `pub struct ValueSpec { pub grid: ValueGrid, pub slot: usize }`, com `bytes(&self, replacement, k)` e
  `check_slot(&self, replacement)`. Sem `Deref`: quem usa a grade escreve `spec.grid`.
- `CheatInfo.value` e `CheatRow.value` passam a `Option<ValueGrid>` (a TUI só usa a grade).

- [ ] **Passo 1:** mover os métodos e ajustar os usos. Nenhum comportamento muda.
- [ ] **Passo 2:** os testes existentes de `value.rs`, das assinaturas e da TUI passam sem mudar o que conferem
  (só o caminho `spec.grid.x`). Acrescentar `grid_check_without_slot` (uma grade `i32` de 100 a 10000, passo 100,
  padrão 1000 passa no `check`; `format(9)` dá `1000`).
- [ ] **Passo 3:** validação verde e commit `refactor(core): grade de valores separada do slot dos patches`.

---

### Tarefa 2: Entrada de dados com modo, N bytes e `next` (diário e decisão)

Spec 6.8.

**Arquivos:** modificar `crates/gt-core/src/journal.rs`; testes em `mod tests`.

**Interfaces (produz):**
- `#[derive(Default)] pub enum DataMode { #[default] Bits, Whole }` (serde `snake_case`).
- `DataEntry { pub id, pub addr, #[serde(default)] pub mode: DataMode, pub original: DataBytes, pub written: DataBytes, #[serde(default)] pub mask: u8, #[serde(default, skip_serializing_if = "Option::is_none")] pub next: Option<DataBytes>, pub guard }`.
  `DataBytes` é `Vec<u8>` com serde próprio: **lê** um número (formato da 005, um byte) ou uma string hex; **grava**
  sempre hex. No modo `Bits`, `original`/`written` têm 1 byte e `mask` vale; no `Whole`, 1 a 8 bytes e `mask` é
  ignorada (gravada como 0). Construtores `DataEntry::bits(id, addr, original: u8, written: u8, mask, guard)` e
  `DataEntry::whole(id, addr, original: Vec<u8>, written: Vec<u8>, guard)`.
- `pub enum DataAction { Drop, Restore(Vec<u8>), Discard }` (o `Restore` passa a levar os bytes a escrever no
  endereço da entrada).
- `pub fn decide_data(e: &DataEntry, current: Option<&[u8]>, guard_ok: bool, writable: bool) -> DataAction`:
  - `Bits`: a regra de hoje, sobre `current[0]`;
  - `Whole`: ilegível, guarda ou região ruim → `Discard`; `current == written` ou `Some(current) == next` →
    `Restore(original)`; `current == original` → `Drop`; qualquer outro valor (inclusive igual em parte) → `Drop` (o
    jogo mudou o campo: esquecer sem escrever, spec 6.4).
- `pub fn data_key(e: &DataEntry) -> (&str, u64)`: a identidade (`id`, endereço) usada pelo motor (Tarefa 3).

- [ ] **Passo 1: testes que falham:**
  - `journal_005_bits_entry_restores` (Foco 2): JSON da 005 (`"original": 6, "written": 0, "mask": 6`, sem `mode`)
    carrega como `Bits` de 1 byte, e `decide_data` com o byte `0x00` dá `Restore(vec![0x06])`;
  - `whole_entry_roundtrip_hex` (grava `original`/`written`/`next` em hex, lê igual);
  - `decide_whole_cases`: 4 bytes `written` → `Restore(original)`; igual ao `next` → `Restore(original)`; igual ao
    `original` → `Drop`; 3 dos 4 bytes iguais ao `written` → `Drop`, sem escrever; guarda ruim → `Discard`;
  - `whole_entry_sizes_1_4_8` (os três tamanhos com a mesma regra);
  - os testes de `decide_data` de hoje continuam, adaptados à assinatura nova.
- [ ] **Passo 2:** falham. **Passo 3:** implementar. **Passo 4:** passam.
- [ ] **Passo 5:** commit `feat(core): entradas de dados do diário com modo inteiro e next`.

---

### Tarefa 3: Motor e recuperação — `set_word`, `restore_field` e identidade por par

Spec 6.4 (ligar, manutenção, troca, desligar), 6.8.

**Arquivos:** modificar `crates/gt-core/src/engine.rs`, `recovery.rs` e
`crates/gt-testkit/src/bin/gt-patch-harness.rs`; testes em `crates/gt-testkit/tests/engine_live.rs` e `harness.rs`.

**Interfaces:**
- Consome: `DataEntry`, `DataMode`, `decide_data`, `data_key` (Tarefa 2).
- Produz:
  - `pub enum SetOutcome { Unchanged, Written, Finalized, NewOriginal(Vec<u8>), Rejected(Vec<u8>) }`.
  - `Engine::set_word(&mut self, id: &str, addr: u64, value: &[u8], guard: &[GuardBytes], accept: &dyn Fn(&[u8]) -> bool) -> Result<SetOutcome>`.
    Começa por `retry_resume`; recusa se o `id` tem pendente; confere identidade do processo, política de dados e a
    guarda, como o `write_data`. Lê o valor atual e decide pela spec 6.4:
    - sem entrada (`id`, `addr`): `accept(atual)` falso → `Rejected(atual)`; senão cria `DataEntry::whole` com
      `original = atual` e `written = value`, grava o diário, escreve, relê → `Written`;
    - entrada com guarda diferente: erro novo `Error::DataEntryOtherObject { id }` (quem chama resolve antes);
    - entrada com `next`: atual == `next` → finaliza (`written = next`, sem `next`, grava o diário) → `Finalized`;
      atual == `written` → retira o `next` e segue;
    - atual == `written` e `written == value` → `Unchanged`;
    - atual == `written` e `written != value` (troca): grava `next = value`, escreve, relê, grava `written = value`
      sem `next` → `Written`. Recusa antes de escrever retira o `next` do diário;
    - atual diferente do `written`: `accept(atual)` falso → `Rejected(atual)`; senão `original = atual`, grava o
      diário, escreve `value`, relê → `NewOriginal(atual)`.
  - `restore_bits` é renomeado para `Engine::restore_field(&mut self, id: &str) -> Result<BatchReport>`, para os dois
    modos (`decide_data` por modo; escrita do `Restore(bytes)` numa chamada só). Os dois usos em `fields.rs` e os
    testes passam ao nome novo.
  - Todo `retain`/`contains` de entradas de dados (`data`, `data_pending`) passa a comparar por `data_key`; atualizar
    uma entrada substitui a do mesmo par. `undo_all`, `Drop`, `restore_pending`, `pending_ids` e `recover` tratam os
    dois modos.
  - `gt-patch-harness set-word --id ID --addr A --value HEX --guard A:HEX [--guard …]` (com `accept` sempre
    verdadeiro), e o gancho `--hook kill-after-next` que encerra o processo com `SIGKILL` depois de gravar o `next` e
    antes de escrever o valor novo (para isso, `set-word` aceita um segundo `--value` que vira a troca), e o gancho
    `--hook panic-after-set`.

- [ ] **Passo 1: testes que falham** (`engine_live.rs`, palavra de 4 bytes em `big_region` como campo e guarda nos
  bytes vizinhos):
  - `set_word_creates_entry_before_write` (o diário tem a entrada com `original` = valor anterior logo depois);
  - `set_word_equal_value_creates_entry` (valor atual já igual ao nosso: entrada com `original` = atual);
  - `set_word_rejects_insane_current` (`accept` falso: nada escrito, sem entrada);
  - `set_word_game_change_new_original`;
  - `set_word_swap_uses_next` (troca: o diário passa por `next` e termina sem ele);
  - `set_same_address_new_guard` (Foco 5: guarda diferente → `DataEntryOtherObject`, e depois do `restore_field` a
    entrada nova tem o original lido de novo);
  - `restore_field_whole_ours_restores`, `restore_field_whole_game_value_forgets`;
  - `entry_update_then_failed_restore_no_stale_copy` (atualizar o original e falhar a restauração deixa só uma entrada
    por par em `data_pending` e no diário);
  - `harness.rs`: `set_word_sigkill_then_recover`, `set_word_sigkill_between_next_and_write` (o `restore` devolve o
    original), `set_word_sigterm_restores`, `set_word_panic_restores` (gancho novo `--hook panic-after-set`: pânico
    logo depois do `set-word`; o `Drop` do motor devolve o original);
  - os testes de bits de hoje continuam verdes (com `restore_field`).
- [ ] **Passo 2:** falham. **Passo 3:** implementar. **Passo 4:** passam.
- [ ] **Passo 5:** commit `feat(core): escrita set com original no diário e recuperação por campo inteiro`.

---

### Tarefa 4: Assinaturas — `int`, `set`, `add`, `[field.value]`, `root` e o `[ue.player]` do estado do jogo

Spec 6.2, 6.3 (combinações), 6.4 (regras de carga), 6.5, 6.6 (carga), 6.7.

**Arquivos:** modificar `crates/gt-grounded2/src/signatures.rs` e `signatures/generic.toml`; testes em `mod tests`.

**Interfaces:**
- Consome: `ValueGrid` (Tarefa 1).
- Produz:
  - `FieldKind` ganha `Int` (TOML `"int"`).
  - `pub enum FieldRoot { Pawn, GameState }` (TOML `root`, padrão `pawn`); `FieldDef { …, pub root: FieldRoot, pub value: Option<ValueGrid> }`.
  - `FieldRule::Set { fields: Vec<String>, kind: FieldKind, target: Option<f64> }` e
    `FieldRule::Add { field: String, result_max: i64 }`. Com `value`, o `target` é `None`; com `target`, `value` é
    `None`.
  - `PlayerCfg { …, pub game_state: Option<GameStateCfg> }` com
    `pub struct GameStateCfg { pub level_class: String, pub world_field: String, pub world_class: String, pub game_state_field: String, pub game_state_class: String }`
    (os cinco campos juntos no `[ue.player]`; obrigatórios se algum `[[field]]` tem `root = "game_state"`).
- Os seis blocos no `generic.toml`, na ordem e com os valores da spec 6.7 (categorias `itens`, `progressao`,
  `movimento`):

  | `id` | `object` / `object_class` | regra | campos | valor |
  |---|---|---|---|---|
  | `carga` | `HaulingComponent` / `/Script/Maine.HaulingComponent` | `set`, `int` | `Capacity` | `i32` 1–100, passo 1, padrão 20 |
  | `dano` | `pawn` / `/Script/Maine.SurvivalCharacter` | `set`, `float` | `CustomDamageMultiplier` | `f32` 1–100, passo 1, padrão 10, `x`, 0 casas |
  | `somar-ciencia` | `PartyComponent` / `/Script/Maine.PartyComponent`, `root = "game_state"` | `add`, `int` | `ScienceFound` | `i32` 100–10000, passo 100, padrão 1000; `result_max = 100000` |
  | `velocidade` | `CharacterMovement` / `/Script/Maine.MaineCharMovementComponent` | `set`, `float` | `CustomGroundSpeedMultiplier`, `CustomSwimSpeedMultiplier` | `f32` 0,5–4, passo 0,25, padrão 2, `x`, 2 casas |
  | `pulos-infinitos` | `pawn` / `/Script/Engine.Character` | `set`, `int` | `JumpMaxCount` | `target = 1000`, `min = 1`, `max = 1000` |
  | `gravidade` | `CharacterMovement` / `/Script/Maine.MaineCharMovementComponent` | `set`, `float` | `GravityScale` | `f32` 0,1–2, passo 0,1, padrão 0,5, `x`, 1 casa |

  E no `[ue.player]`: `level_class = "/Script/Engine.Level"`, `world_field = "OwningWorld"`, `world_class =
  "/Script/Engine.World"`, `game_state_field = "GameState"`, `game_state_class = "/Script/Maine.InGameGameState"`.

- [ ] **Passo 1: testes que falham** (um por erro de carga, com o texto do erro citando arquivo e campo):
  `int_kind_loads`; `set_with_value_or_target_only` (os dois, ou nenhum, recusam); `set_target_needs_min_max`;
  `set_target_out_of_range`; `set_value_with_loose_min_max_refused`; `set_bool_refused`;
  `set_fields_empty_or_repeated`; `set_int_target_not_integer`; `value_in_lock_or_clear_bits_refused`;
  `lock_int_refused`; `value_type_must_match_kind` (`u32` sempre recusado); `value_grid_rules` (as da spec 003 sem
  `slot`); `add_rules` (sem `value`, com `target`, `float`, `min ≤ 0`, `result_max ≤ max`, `min`/`max` soltos);
  `root_unknown_refused`; `game_state_only_with_add`; `game_state_with_object_pawn_refused`;
  `game_state_needs_player_cfg`; `add_with_pawn_root_accepted`; `two_sets_same_field_refused`;
  `real_generic_toml_loads_six_new_fields` (os seis, na ordem, com os valores da tabela).
- [ ] **Passo 2:** falham. **Passo 3:** implementar. **Passo 4:** passam.
- [ ] **Passo 5:** commit `feat(grounded2): regras set e add, tipo int e raiz game_state nas assinaturas`.

---

### Tarefa 5: Resolução, isolamento por cheat e planos puros do `set` e do `add`

Spec 6.2, 6.3, 6.4 (regras de decisão), 6.6 (passos 3 e 4).

**Arquivos:** modificar `crates/gt-grounded2/src/player.rs`; testes em `mod tests` com `gt_ue::synth`.

**Interfaces:**
- Consome: `FieldDef`, `FieldRule::{Set, Add}`, `FieldRoot`, `GameStateCfg` (Tarefa 4).
- Produz:
  - `fn int_field(index, mem, class, name) -> Result<Field, String>`: `IntProperty` de 4 bytes, senão
    `reflexão incoerente`.
  - `ResolvedRule::Set { fields: Vec<Field>, kind: FieldKind, target: Option<f64> }` e
    `ResolvedRule::Add { field: Field, result_max: i64 }`; `ResolvedField { …, pub root: FieldRoot }`.
  - O `FieldCatalog` resolve o caminho do `game_state` à parte (`Result<GameStatePath, String>`): falta de classe ou
    campo dele deixa indisponível só quem tem `root = "game_state"`; falta no caminho do pawn deixa indisponível só
    quem tem `root = "pawn"`.
  - `read_player`: as entradas com `root = "game_state"` não são lidas; uma falha no objeto de um cheat com
    `object_path` não vazio vai para `missing` (todos, não só o pet); com `object_path` vazio (o próprio pawn), a falha
    segue derrubando a leitura. `FieldValues::Set { obj: u64, fields: Vec<(u64 /*addr*/, [u8; 4])> }`.
  - `pub fn missing_note(resolved: &ResolvedField) -> Note`: `WaitingPet` se `is_secondary()`, senão `Waiting`.
  - `pub fn value_is_sane(kind: FieldKind, bytes: [u8; 4]) -> bool` (finito e em `[-1e9, 1e9]`).
  - `pub fn encode_value(kind: FieldKind, v: f64) -> [u8; 4]` (`i32` ou `f32`, little-endian).
  - `pub fn plan_set(values: &FieldValues, desired: [u8; 4], kind: FieldKind) -> Vec<SetStep>` com
    `pub struct SetStep { pub addr: u64, pub sane: bool }`: um passo por campo, com a sanidade do valor atual
    (comparação por bytes, nunca por número).
  - `pub fn read_game_state(mem, index, catalog, objects, controller: u64, id: &str) -> Result<(u64 /*obj*/, u64 /*addr*/, Vec<GuardBytes>), GameStateAbsent>`
    com `pub enum GameStateAbsent { ControllerInvalid, Missing(String /*salto*/) }`: controller revalidado (passo 1 da
    spec 005 6.3), `Outer` (layout `uobject.outer`) `is_a` `Level`, `OwningWorld` `is_a` `World`, `GameState` `is_a`
    `InGameGameState`, depois o `object` do cheat com a classe final, e as conferências de `PropertiesSize`.
  - `pub enum AddRefusal { OutOfRange, WouldExceed }`; `pub fn plan_add(current: i32, amount: i32, result_max: i64) -> Result<i32, AddRefusal>`
    (soma em `i64`: `current < 0` ou `current > result_max` → `OutOfRange`; `current + amount > result_max` →
    `WouldExceed`).

- [ ] **Passo 1: testes que falham** (cena `SynthUe` com controller, pawn, `HaulingComponent`, componente de movimento
  com os dois multiplicadores e `GravityScale`, `Outer` → `Level` → `World` → `GameState` → `PartyComponent`, todos em
  offsets diferentes dos do jogo):
  - `int_field_checks_type_and_size` (campo `FloatProperty` pedido como `int` → `reflexão incoerente`);
  - `resolves_new_fields_by_name`;
  - `game_state_class_missing_only_affects_action`; `pawn_class_missing_only_affects_pawn_root`;
  - `hauling_null_only_waits_carga` (vida segue com valores; `carga` em `missing` com `Note::Waiting`);
  - `pet_missing_keeps_waiting_pet`;
  - `movement_null_waits_speed_and_gravity`;
  - `plan_set_nan_and_negative_zero` (Foco 3: `NaN` → `sane = false`; `-0.0` com nosso `0.0` conta como diferente);
  - `read_game_state_checks_each_hop` (`Outer` não `Level`, `OwningWorld` nulo, `World` de classe errada, `GameState`
    de classe errada, `PartyComponent` nulo → `Missing` com o nome do salto);
  - `read_game_state_pawn_null_ok` (pawn nulo e controller válido: a leitura do `game_state` funciona);
  - `plan_add_cases` (`7400 + 1000` → `8400`; `-1` → `OutOfRange`; `99500 + 1000` → `WouldExceed`; soma perto de
    `i32::MAX` sem estouro).
- [ ] **Passo 2:** falham. **Passo 3:** implementar. **Passo 4:** passam.
- [ ] **Passo 5:** commit `feat(grounded2): resolução do set, do add e do game_state, com isolamento por cheat`.

---

### Tarefa 6: Cena sintética do `gt-testtarget`

Spec 8 (reflexão sintética).

**Arquivos:** modificar `crates/gt-testkit/src/ue.rs` (`build`, `UeAddrs`, `synth_signatures`); ponteiros pelos
ajudantes de `crates/gt-core/src/mem/test_support.rs`, sem `unsafe` novo fora dele.

**Interfaces (produz):** `UeAddrs` ganha `hauling`, `movement`, `level`, `world`, `game_state`, `party` e os offsets
sintéticos dos campos novos (constantes `pub const` como as de hoje, diferentes dos do jogo); `synth_signatures()`
devolve também os seis `[[field]]` e o `GameStateCfg` sintéticos.

Os testes unitários da Tarefa 5 usam a cena em memória (`SynthUe` direto); os de processo da Tarefa 7 usam esta.

- [ ] **Passo 1:** teste `synthetic_scene_has_game_state_path` em `ue_live.rs`: o alvo `--ue` expõe o caminho
  `Outer → Level → World → GameState → PartyComponent` e os campos novos, lidos pelo índice.
- [ ] **Passo 2:** falha. **Passo 3:** implementar. **Passo 4:** passa.
- [ ] **Passo 5:** commit `test(testkit): cena sintética com estado do jogo, carga e movimento`.

---

### Tarefa 7: Execução no `FieldRuntime` — `set`, briga com o jogo e ação

Spec 6.4 inteira, 6.6 (passos 1 a 6), 6.9.

**Arquivos:** modificar `crates/gt-grounded2/src/fields.rs`; testes em `crates/gt-testkit/tests/fields_live.rs` e,
para as regras sem processo, em `mod tests`.

**Interfaces:**
- Consome: `Engine::{set_word, restore_field, write_data, refresh_data_maps}` (Tarefa 3); a cena da Tarefa 6; `plan_set`,
  `value_is_sane`, `encode_value`, `read_game_state`, `plan_add`, `missing_note` (Tarefa 5).
- Produz:
  - `Note` ganha `InvalidGameValue` (`valor do jogo inválido`) e `GameRefuses` (`o jogo não deixa`).
  - `FieldRuntime::set_desired(&mut self, id: &str, value: f64)`: valor desejado de um `set` com `[field.value]`
    (com `target`, o valor escrito é sempre o `target`, e `set_desired` não se aplica). O `tick` usa o desejado de cada `id` ligado.
  - No `tick`, para cada `set` ligado: resolve antes a entrada de outro objeto (`DataEntryOtherObject` →
    `restore_field` e tenta de novo); chama `set_word` por campo com `accept = value_is_sane`; `Rejected` →
    `Note::InvalidGameValue`; `NewOriginal(x)` → evento `<nome>: o jogo mudou o valor para <X>; novo original` e
    conta na janela de briga; mais de 5 em 10 s → `restore_field`, evento `<nome>: o jogo não deixa`, e o par (`id`,
    objeto) fica bloqueado com `Note::GameRefuses` até mudar o objeto ou `turn_off`. Uma falha de um campo não impede
    o outro (falha parcial). Eventos só nas transições.
  - O relógio da janela de briga vem de um `now: Instant` passado ao `tick` (assinatura passa a
    `tick(…, on: &[String], now: Instant)`).
  - `turn_off` dos `set`: `restore_field(id)`, limpando bloqueio e janela do `id`.
  - `pub enum ActionOutcome { Done { before: i32, after: i32 }, Refused(String), NeedsController, Raced { read: i32 } }`.
    `FieldRuntime::run_action(&mut self, engine, mem, index, objects, controller: Option<u64>, id: &str, amount: i32) -> ActionOutcome`:
    passos 1 a 5 da spec 6.6 (motor `stuck`/sem motor é conferido por quem chama, Tarefa 9). `controller` `None` ou
    `ControllerInvalid` → `NeedsController`; outro `GameStateAbsent` → `Refused("estado do jogo ausente")`; objeto com
    endereço novo → `refresh_data_maps`; `plan_add` → `Refused("valor atual fora da faixa" | "passaria do máximo")`;
    escreve com `write_data` (sem diário) e relê: diferente → `Raced`.

- [ ] **Passo 1: testes que falham** (`fields_live.rs`, alvo `ue: true`, motor com diário em `TempDir`):
  - `set_writes_and_turn_off_restores` (os quatro `set` com valor e o `pulos-infinitos`);
  - `set_value_change_goes_through_next`;
  - `set_game_change_becomes_new_original`;
  - `set_reread_mismatch_next_tick_new_original` (Foco 1: o teste muda o campo logo depois da escrita; o ciclo
    seguinte registra o novo original e o desligar devolve esse valor);
  - `set_game_fights_blocks_after_five` (o teste reescreve o campo a cada ciclo; no 6º, `o jogo não deixa`, entrada
    resolvida; trocar o pawn tira o bloqueio);
  - `set_partial_failure_completes_later` (um campo da `velocidade` fora de região `rw` num ciclo, de volta no seguinte);
  - `set_pawn_swap_resolves_old_entry`;
  - `mount_never_written` (montaria no `Pawn` com componentes próprios: os `set` saem do personagem e não tocam nos da
    montaria);
  - `action_adds_and_rereads`, `action_refusals` (fora da faixa, passaria do máximo, `GameState` ausente com controller
    válido → `estado do jogo ausente`, controller ausente → `NeedsController`), `action_refreshes_maps_on_new_object`.
- [ ] **Passo 2:** falham. **Passo 3:** implementar. **Passo 4:** passam. Os testes de travas, bits e pet continuam
  verdes.
- [ ] **Passo 5:** commit `feat(grounded2): execução dos ajustes set e da ação de somar`.

---

### Tarefa 8: TUI — modelo e tela

Spec 6.6 (linha `[ação]`), 6.9.

**Arquivos:** modificar `apps/gt-tui/src/model.rs` e `ui.rs`; testes em `mod tests`.

**Interfaces (produz):**
- `CheatRow { …, pub action: bool }` (`CheatInfo` também). Uma linha de ação nunca está `on`, não tem `desired`, e a
  tela mostra `[ação]` e `+<quantidade>`.
- As marcas `Note::InvalidGameValue` e `Note::GameRefuses` aparecem como `valor do jogo inválido` e
  `o jogo não deixa`.
- A contagem de ligados do topo ignora as linhas de ação.

- [ ] **Passo 1: testes:** `action_row_shows_acao_and_amount`, `notes_new_texts`, `top_count_ignores_action`,
  `field_row_with_value_shows_value` (`[ON ] Velocidade   2.00x`), `pulos_row_without_value`.
- [ ] **Passo 2:** falham. **Passo 3:** implementar. **Passo 4:** passam.
- [ ] **Passo 5:** commit `feat(tui): linha de ação e marcas novas dos cheats de valor`.

---

### Tarefa 9: TUI — interface de alvo, sessão do jogo, falsa e crua

Spec 6.5 (valor na TUI), 6.6, 6.9.

**Arquivos:** modificar `apps/gt-tui/src/backend.rs`, `values.rs`, `game.rs`, `fake.rs` e `raw.rs`.

**Interfaces:**
- Consome: `FieldRuntime::{set_desired, run_action, tick(now)}`, `ActionOutcome` (Tarefa 7); `ValueGrid` (Tarefa 1).
- Produz (na trait `Session`):
  - `fn field_on(&mut self, id: &str, k: u64) -> OpOutcome` (o `k` vira o desejado; ignorado sem `[field.value]`);
  - `set_value(id, k)` de uma linha de valor chama `set_desired` e devolve `Done` (a escrita vem no `tick`);
  - `fn field_action(&mut self, id: &str, k: u64) -> ActionOutcome`: sem motor ou motor `stuck` →
    `Refused` com a mesma mensagem do ligar; senão `run_action`. `NeedsController` também marca o pedido de procura;
  - `fn action_wants_controller(&self) -> bool` e `fn take_controller_news(&mut self) -> Option<bool>` (resultado de
    uma procura pedida pela ação: `Some(true)` achou, `Some(false)` não achou).
  - `field_infos` preenche `value: def.value.clone()` e `action: matches!(def.rule, FieldRule::Add { .. })`.
- A implementação falsa ganha controles para os testes do controlador: resultado da ação, pedido de procura e notícia
  da procura.

- [ ] **Passo 1: testes** (`values.rs`/`game.rs` com a cena em memória, e `fake.rs`): `field_on_sets_desired_k`,
  `set_value_field_updates_desired`, `action_refused_without_engine_or_stuck`, `action_needs_controller_marks_search`.
- [ ] **Passo 2:** falham. **Passo 3:** implementar. **Passo 4:** passam.
- [ ] **Passo 5:** commit `feat(tui): sessão com valor nos cheats de valor e ação`.

---

### Tarefa 10: TUI — controlador: valor nos cheats de valor

Spec 6.5, 6.9.

**Arquivos:** modificar `apps/gt-tui/src/controller.rs`; testes em `mod tests`.

**Interfaces:** consome `Session::{field_on(id, k), set_value}` (Tarefa 9).

- [ ] **Passo 1: testes:** `field_value_arrows_off_and_on` (setas mudam o mostrado desligado; ligado, a troca respeita
  150 ms e o último valor vence); `field_relink_uses_shown_value` (religar depois de reconectar usa o mostrado);
  `set_maintenance_with_new_original_keeps_keys` (a manutenção que grava um original novo não descarta teclas nem
  invalida buscas); `game_refuses_arrows_only_change_shown`; `game_refuses_cleared_on_reconnect`;
  `set_maintenance_suspended_stuck_or_pending`; `set_turn_off_failure_marks_error_blocks_id`.
- [ ] **Passo 2:** falham. **Passo 3:** implementar. **Passo 4:** passam.
- [ ] **Passo 5:** commit `feat(tui): valor ajustável nos cheats de valor`.

---

### Tarefa 11: TUI — controlador: a ação

Spec 6.6, 6.9.

**Arquivos:** modificar `apps/gt-tui/src/controller.rs`; testes em `mod tests`.

**Interfaces:** consome `Session::{field_action, action_wants_controller, take_controller_news}` (Tarefa 9).

Regras (spec 6.6):
- `espaço`/`Enter` numa linha `[ação]` pede uma execução. Menos de 1 s depois da última execução que escreveu:
  descarta, com `Somar ciência: aguarde` no registro (no máximo uma por segundo). Recusas com o mesmo motivo: no máximo
  uma linha por segundo. A janela e os limites vivem no controlador, por ação, e zeram ao reconectar. Relógio pelo
  `now` já injetado.
- Só uma execução que chega a ler o jogo conta como operação (`after_op_keys`); recusas e descartes não.
- `NeedsController`: `procurando o personagem` se a procura começou ou está em andamento, `aguarde a procura` se caiu
  no intervalo. A procura do controller passa a rodar também quando `action_wants_controller()` é verdadeiro, no
  intervalo de hoje. Ao chegar `take_controller_news()`, evento `personagem encontrado: aperte de novo para somar` ou
  `personagem não encontrado`, e a linha `personagem:` passa a seguir o controller guardado mesmo sem cheat ligado.
- A ação nunca entra nos desejados, não é religada, não é tocada na saída nem na recuperação.

- [ ] **Passo 1: testes:** `action_executes_once_and_logs` (`Somar ciência: 7400 → 8400`);
  `action_key_repeat_one_per_second` (Foco 4: 20 teclas em 2 s → 2 execuções, 2 linhas `aguarde` no máximo);
  `action_refusal_does_not_open_window`; `action_refusal_lines_throttled`; `action_refusal_not_an_op`;
  `action_not_desired_not_relinked`; `action_untouched_on_exit_and_recovery`; `action_triggers_controller_search`
  (dentro do intervalo → `aguarde a procura`, sem nova procura); `controller_news_event_and_player_line`;
  `action_window_resets_on_reconnect`; `action_checking_and_unavailable_states`.
- [ ] **Passo 2:** falham. **Passo 3:** implementar. **Passo 4:** passam.
- [ ] **Passo 5:** commit `feat(tui): ação de somar com limite de uma por segundo`.

---

### Tarefa 12: `gt ue player` com os campos novos

Spec 6.10.

**Arquivos:** modificar `crates/gt-grounded2/src/player.rs` (`player_report`, `FieldReportValue`) e
`apps/gt-cli/src/` (formatação); testes no `gt-grounded2` contra a cena sintética.

**Interfaces (produz):** `FieldReportValue::Set { fields: Vec<(String, u64, String /*valor formatado*/)> }` e
`FieldReportValue::Add { addr: u64, current: i32 }`; `PlayerReport { …, pub game_state: Option<GameStateReport> }`
com os endereços de `Level`, `World`, `GameState` e do objeto da ação (nomes vindos do `[ue.player]` e do `object`).

- [ ] **Passo 1: testes:** `report_includes_set_and_add`, `report_game_state_path`, `report_game_state_missing_hop`.
- [ ] **Passo 2:** falham. **Passo 3:** implementar (texto e `--json`). **Passo 4:** passam.
- [ ] **Passo 5:** commit `feat(cli): gt ue player mostra carga, dano, ciência e movimento`.

---

### Tarefa 13: Documentos

Spec 7, 10.

**Arquivos:** `docs/ARCHITECTURE.md` (3, 5.3, 5.4, 5.6, 5.7 e registro de decisões), `AGENTS.md` 3 (ressalvas da
ação, do `set` e dos valores acima do normal), notas em `docs/specs/002-motor-patches.md`,
`docs/specs/003-interface-tui.md` e `docs/specs/005-sobrevivencia-por-reflexao.md` (6.3 e 6.5), `docs/ROADMAP.md`
(teleporte e speedhack fora da Fase 5).

- [ ] **Passo 1:** escrever as mudanças listadas na spec 10, com os mesmos termos da spec.
- [ ] **Passo 2:** commit `docs: arquitetura, regras e notas da spec 006`.

---

### Tarefa 14: Sessão com o jogo e critério de pronto (sessão principal, com o dono)

Spec 9.

**Arquivos:** testes `#[ignore]` em `apps/gt-tui/tests/game.rs`; `docs/RESEARCH.md`; `docs/ROADMAP.md` (status).

- [ ] **Passo 1:** testes `#[ignore]`: `game_new_fields_available` (os seis disponíveis e verificados, `gt ue player`
  coerente) e `game_set_round_trip` (liga e desliga `carga`, `dano`, `velocidade`, `pulos-infinitos` e `gravidade` e
  confere o original no fim). Sem teste automático da ciência.
- [ ] **Passo 2:** pedir ao dono: FLiNG fechado, jogo num save e **backup do save**. Rodar os `#[ignore]`.
- [ ] **Passo 3:** pedir ao dono para rodar `./target/release/gt-tui` e seguir os itens 2 a 13 da spec 9, um de cada
  vez, com `gt ue player --watch 2` num segundo terminal quando preciso.
- [ ] **Passo 4:** registrar no `RESEARCH.md` o resultado de cada item (efeito, teto da velocidade, save, ciência,
  carga ao desligar). Cheat sem efeito sai da spec (definição, testes e linha da TUI), com registro.
- [ ] **Passo 5:** ROADMAP com a Fase 5 fechada; commit `docs(research): critério de pronto da spec 006 no jogo`.
