# Plano 007 — Construir sem ingredientes e dano recebido (configurações do modo de jogo)

> **Para agentes executores:** SUB-SKILL OBRIGATÓRIA: use superpowers:subagent-driven-development (recomendado) ou superpowers:executing-plans para executar este plano tarefa a tarefa. Os passos usam checkbox (`- [ ]`).

**Objetivo:** dois cheats de valor novos sobre o objeto vivo `SurvivalGameModeSettings` (`construir-sem-ingredientes`
e `dano-recebido`), com `set` liberado na raiz `game_state` e `set` de `bool` de byte inteiro com valor por campo.

**Arquitetura:** as assinaturas aceitam `kind = "bool"` (com `bools`) e `root = "game_state"` no `set`. O
`gt-grounded2` generaliza o caminho do `set` de `[u8; 4]` para o tamanho do campo (1 ou 4 bytes), ganha uma leitura
do mundo no ciclo (`read_world`, separada do `read_player`, com conferência de CDO) e o `FieldRuntime::tick` passa a
tratar as duas raízes isoladas, com a marca `aguardando o mundo`. A TUI pede a procura do controller por pawn nulo sem
descartar o controller guardado. O diário e o motor não mudam (`set_word` já aceita 1 a 8 bytes).

**Tecnologias:** Rust stable (edition 2024), `serde`/`toml`/`serde_json`, `thiserror`, `tracing`, `ratatui`.

**Spec:** `docs/specs/007-construcao-e-defesa.md` (citada como "spec 6.x"). A spec 006
(`docs/specs/006-progressao-e-movimento.md`, "spec 006 6.x") e o plano 006 descrevem o código que este estende. Quem
executa lê a spec e este plano.

## Restrições globais

- Regras do `AGENTS.md`: identificadores em inglês; comentários, docs e mensagens em pt-BR; `unsafe` só em
  `crates/gt-core/src/mem/` com `// SAFETY:`; `thiserror` nas libs e `anyhow` nos binários; sem `unwrap()`/`expect()`
  fora de testes; `tracing`, nunca `println!` em libs.
- **Nenhum offset de campo do jogo em código de produção nem nas assinaturas.** Campos por nome, da reflexão. Os
  offsets do `RESEARCH.md` (`+0x328`, `+0x148`, `+0x3b`, `+0x3d`, `+0x58`) só aparecem em testes `#[ignore]`.
- Regra de dependência inalterada; o `gt-cli` continua sem `write`
  (`cargo tree -p gt-cli -e normal,build,features -i gt-core` sem `write` nem `test-support`).
- Validação de toda tarefa: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`. Antes de testes do `gt-testkit`/`gt-tui`, `cargo build --workspace` (o `bin_path` precisa
  do `gt-testtarget`).
- **Valores fixados pela spec:**
  - `construir-sem-ingredientes`: categoria `itens`, `root = "game_state"`,
    `object = "SurvivalModeManagerComponent.GameModeSettings"`, `object_class = "/Script/Maine.SurvivalGameModeSettings"`,
    `rule = "set"`, `kind = "bool"`, `fields = ["bRecipesRequireIngredients", "bAutoCompleteBuildings"]`,
    `bools = [false, true]`;
  - `dano-recebido`: categoria `sobrevivencia`, mesma raiz/objeto/classe, `rule = "set"`, `kind = "float"`,
    `fields = ["EnemyDamageMultiplier"]`, `[field.value]` `f32` de 0 a 1, passo 0,05, padrão 0,25, `unit = "x"`,
    `decimals = 2`;
  - os dois no **fim** do `generic.toml`; `bool` são: byte `0` ou `1`; escrita `0x01`/`0x00`.
- **Textos exatos** (os testes procuram estes trechos): marca `aguardando o mundo`; eventos `mundo perdido` e
  `mundo encontrado` (uma linha por raiz, não por cheat); no `gt ue player`, `bool` como `true`/`false`.
- **Git:** branch `spec-007-construcao` (já criado, spec em `dc30d6d`, pesquisa em `07f83eb`). Commit por tarefa,
  depois da validação verde, em Conventional Commits pt-BR, terminando com a linha
  `Claude-Session: https://claude.ai/code/session_01VqZGobwf5HFKTonm3gya8y`.
- **Tarefa com o jogo** (9) roda na sessão principal, com o dono. Antes dela, pedir o **backup do save**. FLiNG
  fechado. Nunca `sudo` nem mudar o `ptrace_scope`.
- **Despacho:** cada subagente recebe esta seção inteira, a tarefa e as seções da spec citadas no cabeçalho dela.
- Testes de integração novos começam com `#![allow(clippy::unwrap_used, clippy::expect_used)]`.

## Foco de revisão

1. **Carregar outro save com cheats do mundo ligados e o personagem ainda nulo** (vários controllers sem pawn,
   `Ambiguous`). Esperado: o controller guardado continua em uso, nenhuma linha "personagem não encontrado" nem marca
   nova piscando, e a troca acontece quando o controller novo tem pawn. Teste na Tarefa 6 (`next_controller` com
   `Ambiguous` e guardado `Some`).
2. **`GameModeSettings` trocando de objeto no mesmo endereço** (mundo recarregado, guarda diferente). Esperado: a
   entrada antiga é resolvida antes e o original vem do objeto novo. Teste na Tarefa 5
   (`world_settings_swap_same_addr_resolves_old_first`).
3. **Byte de `bool` com lixo** (`0x02`, `0xff`) vindo do jogo. Esperado: `valor do jogo inválido`, nada escrito, e
   volta sozinho quando o byte vira `0`/`1`. Teste na Tarefa 3 (`bool_sanity`) e na Tarefa 5
   (`world_bool_garbage_note_and_resume`).
4. **Pawn de classe errada (montaria sem alternativa) com só cheats do mundo ligados.** Esperado: eles seguem
   escrevendo; a linha `personagem:` não muda de estado por causa deles. Teste na Tarefa 5
   (`world_cheats_survive_pawn_wrong_class`).
5. **Os dois cheats do mundo ligados e um cheat do pawn ligados, controller inválido.** Esperado: os três em
   `aguardando o personagem`, sem `mundo perdido` no registro. Teste na Tarefa 5
   (`controller_invalid_waits_both_roots_without_world_event`).

---

### Tarefa 1: Assinaturas — `bool` no `set`, `set` na raiz `game_state` e os dois `[[field]]`

Spec 6.3 (regra de carga), 6.4 (carga do `bool`), 6.5.

**Arquivos:**
- Modificar: `crates/gt-grounded2/src/signatures.rs` (`FieldKind`, `FieldRule::Set`, `GenericField`, `build_field`,
  `check_field_set`, testes)
- Modificar: `crates/gt-grounded2/signatures/generic.toml` (os dois `[[field]]` no fim)

**Interfaces:**
- Produz: `FieldKind::Bool` (1 byte); `FieldRule::Set { fields: Vec<String>, kind: FieldKind, target: Option<f64>,
  bools: Option<Vec<bool>> }` — `bools` é `Some` (mesmo tamanho de `fields`) se e só se `kind == Bool`; nos outros,
  `None`. `GenericField` ganha `bools: Option<Vec<bool>>`.

- [ ] **Passo 1: testes que falham**, em `signatures.rs` (módulo de testes, reaproveitando `SET_TARGET` e
  `field_err`):
  - `set_bool_with_bools_loads`: `kind = "bool"`, `fields = ["A", "B"]`, `bools = [false, true]` carrega com
    `FieldRule::Set { kind: FieldKind::Bool, bools: Some(vec![false, true]), target: None, .. }` e `value == None`.
  - **reescrever** `set_bool_refused` → `set_bool_refusals`: com `kind = "bool"` recusa sem `bools`; com `bools` de
    tamanho diferente de `fields`; com `[field.value]`; com `target`; com `min` ou `max`. E `bools` com
    `kind = "int"` ou em `lock`/`clear_bits`/`add` também recusa.
  - **reescrever** `game_state_only_with_add` → `game_state_with_add_or_set`: `root = "game_state"` aceita `add` e
    `set` (inclusive `bool`); recusa `lock` e `clear_bits`; `object = "pawn"` com `game_state` continua recusado.
  - `same_object_in_other_root_is_not_a_duplicate`: dois `set` com o mesmo `object` e campo, um `pawn` e outro
    `game_state`, carregam; dois na mesma raiz continuam recusados.
  - `real_generic_has_world_cheats`: o `generic.toml` real carrega e tem `construir-sem-ingredientes` e
    `dano-recebido` com os valores das restrições globais.
- [ ] **Passo 2:** `cargo test -p gt-grounded2 signatures` — falham.
- [ ] **Passo 3: implementar.** No ramo `Some("set")` do `build_field`: `kind = "bool"` vira `FieldKind::Bool` e
  exige `bools` do tamanho de `fields`, recusando `value`, `target`, `min`, `max`. Nos outros ramos e kinds, `bools`
  presente é erro (`"só vale com rule = \"set\" e kind = \"bool\""`). Trocar a recusa final de raiz por: `game_state`
  com `lock`/`clear_bits` é erro (`"game_state só vale com rule = \"add\" ou \"set\""`). Em `check_field_set`, a chave
  vira `(root, object, nome)`. Acrescentar os dois `[[field]]` ao fim do `generic.toml`, com um comentário de uma
  linha citando a spec 007. Adicionar `bools: None` nos `FieldRule::Set` existentes. Onde o compilador pedir um ramo
  para `FieldKind::Bool` em `player.rs`/`fields.rs` (`value_is_sane`, `encode_value`, `decode_value`,
  `format_value`, `resolve_field`), o ramo é o mais seguro possível até a Tarefa 3: `value_is_sane` devolve `false`
  e `resolve_field` devolve `Err("reflexão incoerente")` para `Bool` (o cheat fica indisponível, nunca escreve);
  `encode`/`decode`/`format` tratam o primeiro byte. Sem `todo!()`/`unreachable!()`.
- [ ] **Passo 4:** `cargo test -p gt-grounded2` — verde (inclusive os testes do catálogo que carregam o
  `generic.toml` real).
- [ ] **Passo 5: commit** `feat(signatures): set de bool e set na raiz game_state; cheats de construção e dano recebido`.

### Tarefa 2: Cena sintética — gerenciador de modo, configurações e CDO

Spec 6.1 (Q1, Q5), seção 8 (reflexão sintética).

**Arquivos:**
- Modificar: `crates/gt-testkit/src/ue.rs`

**Interfaces:**
- Produz, em `gt_testkit::ue`: constantes `MODE_MANAGER_OFF: u32 = 0x50` (campo `SurvivalModeManagerComponent` no
  `InGameGameState`), `SETTINGS_OFF: u32 = 0x38` (`GameModeSettings` no componente), `RECIPES_OFF: u32 = 0x31`,
  `AUTO_COMPLETE_OFF: u32 = 0x33`, `ENEMY_DAMAGE_OFF: u32 = 0x3c`, `ENEMY_DAMAGE_START: f32 = 1.0` (offsets
  diferentes dos do jogo, de propósito); campos novos em `UeAddrs`: `mode_manager: u64`, `settings: u64`,
  `cdo_settings: u64`; `synth_signatures()` passa a devolver também os dois `[[field]]` da Tarefa 1 (copiados do
  `generic.toml`, mesmos `id`).

- [ ] **Passo 1: teste que falha** em `crates/gt-testkit/tests/ue_live.rs` (ou no módulo de testes de `ue.rs`, onde
  já se confere a cena): `scene_has_mode_settings` — no `SparseMem` da imagem: `game_state + MODE_MANAGER_OFF` aponta
  para `mode_manager`; `mode_manager + SETTINGS_OFF` aponta para `settings`; em `settings`, `RECIPES_OFF = 1`,
  `AUTO_COMPLETE_OFF = 0`, `ENEMY_DAMAGE_OFF = 1.0f32`; o `ObjectFlags` de `cdo_settings` tem o bit
  `layout.uobject.cdo_flag` e o de `settings` não.
- [ ] **Passo 2:** `cargo test -p gt-testkit scene_has_mode_settings` — falha.
- [ ] **Passo 3: implementar.** Classes: `SurvivalModeManagerComponent` (campo de objeto `GameModeSettings`),
  `SurvivalGameModeSettings` (`bRecipesRequireIngredients` e `bAutoCompleteBuildings` com
  `boolean(name, off, 0xff)`, `EnemyDamageMultiplier` float) e a subclasse `BP_NormalModeSettings_C` (super
  `SurvivalGameModeSettings`); `InGameGameState` ganha o campo de objeto `SurvivalModeManagerComponent`. Ligar o
  `PropertyClass` dos dois campos de objeto com `link_struct` (o mesmo ponteiro em `fstruct.struct_ptr` que o
  `object_field_class` lê). Instâncias: `mode_manager` (outer `game_state`), `settings` da subclasse (outer `world`) e
  `cdo_settings` (outra instância da subclasse com o bit de CDO escrito por `s.set(obj, layout.uobject.flags, ..)`,
  fora do caminho). Os testes antigos da cena continuam verdes.
- [ ] **Passo 4:** `cargo build --workspace && cargo test -p gt-testkit` — verde.
- [ ] **Passo 5: commit** `test(testkit): configurações do modo de jogo na cena sintética`.

### Tarefa 3: `set` de N bytes e `bool` de byte inteiro na resolução

Spec 6.4.

**Arquivos:**
- Modificar: `crates/gt-grounded2/src/player.rs`
- Teste: `crates/gt-grounded2/src/player/tests.rs`

**Interfaces:**
- Consome: `FieldKind::Bool`, `FieldRule::Set { bools, .. }` (Tarefa 1).
- Produz:
  - `fn byte_bool_field<M: ReadMem + ?Sized>(index, mem, class: u64, name: &str) -> Result<Field, String>`: aceita
    só `kind == "BoolProperty"`, `size == 1`, `bits == Some(BoolBits { mask: 0xff, byte })` com `byte == offset`;
    senão `INCOHERENT`. O `bool_field` do `clear_bits` não muda.
  - `ResolvedRule::Set { fields: Vec<Field>, kind: FieldKind, target: Option<f64>, bools: Option<Vec<bool>> }`.
  - `FieldKind::size(self) -> u32` (`Bool` 1, `Int`/`Float` 4), em `signatures.rs`.
  - `FieldValues::Set { obj: u64, fields: Vec<(u64, Vec<u8>)> }` (bytes do tamanho do campo).
  - `pub fn value_is_sane(kind: FieldKind, bytes: &[u8]) -> bool` (`Bool`: `[0]` ou `[1]`; tamanho errado: `false`).
  - `pub fn encode_value(kind: FieldKind, v: f64) -> Vec<u8>` (`Bool`: `v != 0.0` → `[1]`, senão `[0]`).
  - `decode_value(kind, &[u8]) -> f64` (`Bool`: `0.0`/`1.0`) e `format_value` (`Bool`: `"true"`/`"false"`).
  - `pub fn desired_bytes(rule: &ResolvedRule, value: Option<f64>) -> Option<Vec<Vec<u8>>>`: um vetor de bytes por
    campo. `bools` → cada campo o seu; senão `encode_value(kind, value?)` repetido.
  - `plan_set(values: &FieldValues, desired: &[Vec<u8>], kind: FieldKind) -> Vec<SetStep>` (um desejado por campo).
  - `SetFieldReport.raw` continua `u32` (bytes zero-estendidos).

- [ ] **Passo 1: testes que falham** em `player/tests.rs`:
  - `byte_bool_field_accepts_native_bool_only`: na reflexão sintética, `bRecipesRequireIngredients` resolve; um bool
    de máscara `0x04` (o `DIE_BYTE` do `HealthComponent`) dá `"reflexão incoerente"`.
  - `bool_sanity`: `value_is_sane(Bool, &[0])` e `&[1]` verdadeiros; `&[2]`, `&[0xff]`, `&[]`, `&[0, 0]` falsos;
    `Float`/`Int` iguais aos testes antigos com `&[u8]`.
  - `desired_bytes_per_field`: a regra resolvida de `construir-sem-ingredientes` dá `[[0], [1]]` com `value = None`;
    a de `velocidade` com `Some(2.0)` dá dois `2.0f32.to_le_bytes()`.
  - `world_set_resolves_two_hops`: `FieldCatalog::resolve` na cena sintética deixa os dois cheats novos `Ok`, com
    `object_path` de 2 saltos e `root == GameState`.
  - Adaptar os testes existentes de `plan_set` (`player/tests.rs:1486-1512`) para a assinatura nova, sem mudar o que
    afirmam.
- [ ] **Passo 2:** `cargo test -p gt-grounded2 player` — falham.
- [ ] **Passo 3: implementar.** Em `resolve_field`, `FieldKind::Bool` usa `byte_bool_field`; `Reader::raw4` vira
  `raw(obj, field, len)` com `check_fits(.., len)`. O `run_set_fields` de `fields.rs` passa a usar `desired_bytes` e
  `write_set_field(engine, id, addr, &[u8], guard, kind)` com `accept = |b| value_is_sane(kind, b)`; para
  `int`/`float` o comportamento é idêntico ao de hoje (os testes de `fields_live.rs` provam). `desired_value` continua
  devolvendo `None` para `set` sem `target`/grade, mas o `bool` não passa por ele: `desired_bytes` com `bools` ignora
  o valor.
- [ ] **Passo 4:** `cargo build --workspace && cargo test --workspace` — verde (inclusive `fields_live.rs`, que
  cobre os `set` da 006).
- [ ] **Passo 5: commit** `feat(player): set de 1 ou 4 bytes com valor por campo e bool de byte inteiro`.

### Tarefa 4: Leitura do mundo no ciclo (`read_world`) e conferência de CDO

Spec 6.2, 6.3 (marcas, nunca num CDO, índice), 4.

**Arquivos:**
- Modificar: `crates/gt-grounded2/src/player.rs`
- Teste: `crates/gt-grounded2/src/player/tests.rs`

**Interfaces:**
- Consome: `ResolvedRule::Set`, `FieldValues::Set` (Tarefa 3); cena da Tarefa 2.
- Produz:
  - `MissingKind::World` e `missing_kind` devolvendo `World` para `root == GameState` (antes do teste de
    `is_secondary`).
  - `pub struct WorldSnapshot { pub game_state: u64, pub objects: Vec<ObjSnap>, pub values: HashMap<String,
    FieldValues>, pub missing: HashMap<String, GameStateAbsent> }`.
  - `pub fn read_world<M: ReadMem + ?Sized>(mem: &M, index: &ReflectionIndex, catalog: &FieldCatalog, objects:
    &ObjectArray, controller: u64) -> Result<WorldSnapshot, GameStateAbsent>`: confere o controller
    (`GameStateAbsent::ControllerInvalid`), anda `Outer` → `World` → `GameState` **uma vez** (falha aí:
    `Err(Missing(salto))`), e lê só os `set` de raiz `game_state` resolvidos; um salto do objeto nulo, de classe
    errada, ilegível, fora do objeto **ou com o bit de CDO** vai para `missing` daquele `id` (`Missing(nome do
    salto)`). Não depende de `catalog.pawn_class` nem do pawn.
  - `GameStateAbsent::Missing` cobre o CDO com o nome do último salto (`GameModeSettings`).

- [ ] **Passo 1: testes que falham** (cena sintética, `SparseMem`, escrevendo direto na memória para montar cada
  caso):
  - `read_world_reads_both_world_cheats`: valores `[1]`, `[0]` e `1.0f32` nos endereços de `settings`.
  - `read_world_ignores_pawn`: com o `Pawn` do controller zerado e com o pawn apontando para a criatura (classe
    errada), `read_world` continua `Ok` com os mesmos valores.
  - `read_world_hop_missing_is_per_cheat`: `mode_manager` zerado → os dois `id` em `missing` com
    `Missing("SurvivalModeManagerComponent")`; `GameModeSettings` apontando para o `party` (classe errada) →
    `Missing("GameModeSettings")`.
  - `read_world_never_reads_a_cdo`: `GameModeSettings` apontando para `cdo_settings` → os dois em `missing`, nenhum
    valor.
  - `read_world_controller_invalid`: controller com `InternalIndex` trocado → `Err(ControllerInvalid)`.
  - `missing_kind_world_for_game_state_root`: `missing_kind` dos dois cheats novos é `World`; o do pet continua `Pet`.
  - `index_missing_class_only_hits_world_cheats`: catálogo resolvido com `object_class` trocado para uma classe
    inexistente nos dois novos → `avail` deles `Err("classe … não encontrada")`, os do pawn `Ok`; e
    `defs` com o `object` `"Nada.GameModeSettings"` → `campo Nada não encontrado` só neles.
- [ ] **Passo 2:** `cargo test -p gt-grounded2 read_world` — falham.
- [ ] **Passo 3: implementar** `read_world`, reaproveitando o `Reader` e o trecho de `walk_game_state` que anda até o
  `GameState` (extrair uma função `walk_to_game_state(&mut Reader, controller, gs) -> Result<u64, GameStateAbsent>`
  usada pelas duas). A conferência de CDO lê `ObjectFlags` (`layout.uobject.flags`, `u32`) do objeto final e testa
  `layout.uobject.cdo_flag`. O `read_player` continua pulando a raiz `game_state`.
- [ ] **Passo 4:** `cargo test -p gt-grounded2` — verde.
- [ ] **Passo 5: commit** `feat(player): leitura do mundo no ciclo com conferência de CDO`.

### Tarefa 5: `FieldRuntime::tick` com as duas raízes

Spec 6.3 (tudo), 4.

**Arquivos:**
- Modificar: `crates/gt-grounded2/src/fields.rs`
- Teste: `crates/gt-testkit/tests/fields_live.rs` (alvo vivo `gt-testtarget`, como os testes de `set` da 006); `crates/gt-testkit/tests/harness.rs` (SIGKILL com 1 byte); unidade em `fields.rs`

**Interfaces:**
- Consome: `read_world`, `WorldSnapshot`, `MissingKind::World` (Tarefa 4); `desired_bytes` (Tarefa 3).
- Produz:
  - `Note::WaitingWorld` (`aguardando o mundo`) e `missing_note(MissingKind::World) == Note::WaitingWorld`.
  - `pub enum WorldState { Found, Absent(GameStateAbsent) }` e `TickReport.world: Option<WorldState>` (`None` quando
    nenhum `set` da raiz `game_state` está em `on`). `TickReport.player` continua sendo só o pawn.
  - `fn world_object_changed(prev: Option<&Vec<GuardBytes>>, now: &[GuardBytes]) -> bool` (privada, pura).
  - Eventos `mundo perdido` / `mundo encontrado` nas transições de `world` (um por raiz), guardados num
    `world_found: Option<bool>` no runtime; nada na primeira leitura com sucesso além de `mundo encontrado` se antes
    era `Some(false)`.

- [ ] **Passo 1: testes que falham** em `fields_live.rs`, com o `Scene` existente (acrescentar os dois `id` ao
  `on`):
  - `world_set_writes_and_turn_off_restores`: ligado, `settings` fica `[0]`/`[1]` e `0.25f32`; `turn_off` de cada
    um devolve `[1]`/`[0]` e `1.0f32`; o diário fica vazio.
  - `world_cheats_survive_pawn_null_and_wrong_class`: `Pawn` zerado (e depois apontando para a criatura) com os dois
    ligados: continuam escritos, sem nota; os do pawn ligados ficam `Note::Waiting`; `report.player` é
    `Absent(PawnNull)`/`Absent(PawnWrongClass)` e `report.world == Some(WorldState::Found)`.
  - `controller_invalid_waits_both_roots_without_world_event`: controller inválido → todos os `on` com
    `Note::Waiting`, nenhum evento `mundo perdido`.
  - `world_hop_missing_notes_and_resumes`: zerar `mode_manager` → os dois com `Note::WaitingWorld`, um único evento
    `mundo perdido` com os dois ligados, os do pawn sem nota; restaurar o ponteiro → escrevem de novo e um único
    `mundo encontrado`.
  - `world_settings_swap_same_addr_resolves_old_first`: trocar a guarda de `settings` (o `InternalIndex`) com os dois
    ligados → no ciclo seguinte a entrada antiga é resolvida (`restore_field`) antes da nova e o original novo é lido
    do objeto (escrever `[1]`/`[0]` novos antes do ciclo e conferir no `turn_off`).
  - `world_bool_garbage_note_and_resume`: `settings + RECIPES_OFF = 0x02` antes de ligar → `Note::InvalidGameValue`,
    nada escrito; `0x01` → escreve no ciclo seguinte.
  - em `fields.rs` (unidade): `world_object_changed_detects_new_guard` — `world_object_changed(None, &g)` e
    `world_object_changed(Some(&g1), &g2)` com `g1 != g2` são `true`; com a mesma guarda, `false`.
  - `world_not_read_without_world_cheats`: só cheats do pawn ligados e `GameModeSettings` zerado → `report.world ==
    None`, nenhuma nota nem evento de mundo.
  - `world_sigkill_restores_by_journal`: em `crates/gt-testkit/tests/harness.rs`, um `set-word` de **1 byte**
    (`--value 00`) seguido de SIGKILL e `restore` devolve o byte original (espelhar `sigkill_during_value_swap_then_recover`).
- [ ] **Passo 2:** `cargo build --workspace && cargo test -p gt-testkit --test fields_live world` — falham.
- [ ] **Passo 3: implementar** o `tick` em duas partes isoladas:
  1. **pawn**: `read_player` como hoje. Um `Err` com `NoController`/`ControllerInvalid` põe `Note::Waiting` em
     **todos** os `on` e retorna (como hoje). Qualquer outro `Err` põe `Note::Waiting` só nos `on` de raiz `pawn` e
     **segue** para a parte do mundo.
  2. **mundo** (só se algum `on` é `set` de raiz `game_state`): `read_world`; `Err(ControllerInvalid)` → igual ao
     controller inválido acima; `Err(Missing(_))` → `Note::WaitingWorld` nos `on` do mundo; `Ok` → para cada `id` do
     mundo em `on`: em `missing` → `Note::WaitingWorld`; senão `tick_set` com o `SetJob` vindo do `WorldSnapshot`.
  - `SetJob.fields` passa a `&[(u64, Vec<u8>)]`; `tick_set`/`run_set_fields` recebem a guarda do objeto pela lista
    `objects` do snapshot de onde o `set` veio (troque o `&PlayerSnapshot` por `&[ObjSnap]`).
  - **`maps`**: um `world_objects: HashMap<String, Vec<GuardBytes>>` por `id`; quando
    `world_object_changed(prev, &guard)` → `engine.refresh_data_maps()` antes de escrever. O alvo de teste não cria
    região nova em tempo de execução, então a chamada é conferida na revisão e a decisão por um teste de unidade.
- [ ] **Passo 4:** `cargo test --workspace` — verde (os testes da 006 em `fields_live.rs` sem mudança).
- [ ] **Passo 5: commit** `feat(fields): manutenção dos set do mundo isolada do personagem`.

### Tarefa 6: TUI — procura sem descartar o controller e a marca do mundo

Spec 6.3 (procura do controller com o pawn nulo), 6.6.

**Arquivos:**
- Modificar: `apps/gt-tui/src/values.rs` (`tick`, `merge` do `JobKind::Controller`), `apps/gt-tui/src/ui.rs` (texto
  da `Note::WaitingWorld`), `apps/gt-tui/src/backend.rs` (`idle_tick` com `world: None`), `apps/gt-tui/src/fake.rs`
  se construir `TickReport`
- Teste: `apps/gt-tui/src/values.rs` (unidade), `apps/gt-tui/src/ui.rs`, `apps/gt-tui/src/controller.rs` (backend
  falso)

**Interfaces:**
- Consome: `Note::WaitingWorld`, `TickReport.world` (Tarefa 5).
- Produz: `fn next_controller(kept: Option<u64>, found: &ControllerData) -> (Option<u64>, Option<PlayerState>)` em
  `values.rs`, pura: `Found(c)` → `(Some(c), Some(Found))`; `Absent`/`Ambiguous`/`Err` com `kept == Some(k)` →
  `(Some(k), None)` (nada muda, sem `player_changed`); com `kept == None` → `(None, Some(Absent(..)))` como hoje.

- [ ] **Passo 1: testes que falham:**
  - `values.rs`: `next_controller_keeps_on_absent_ambiguous_err` (os três com `kept = Some(0x10)` mantêm e devolvem
    `None`), `next_controller_swaps_on_found` (`Found(0x20)` com `kept = Some(0x10)` → `Some(0x20)`),
    `next_controller_without_kept_as_today`.
  - `ui.rs`: uma linha com `note = Some(Note::WaitingWorld)` mostra `aguardando o mundo` (ao lado dos testes de
    `ui.rs:872-985`).
  - `controller.rs` (backend falso): `world_note_only_on_world_rows` — relatório com `Note::WaitingWorld` em dois
    `id` e nada no terceiro mostra a marca só nos dois; `personagem:` segue o `report.player`.
- [ ] **Passo 2:** `cargo test -p gt-tui` — falham.
- [ ] **Passo 3: implementar.** No `ValueCheats::tick`, o ramo `PawnNull` só atualiza `pawn_null_since` (não zera
  `self.controller`); o `needs_controller` continua pedindo a procura pelo `controller_needs_search`. No `merge`,
  usar `next_controller`; quando ele devolve `None` para o estado, não pôr `news.player_changed` e `news.retry =
  false`. `Note::WaitingWorld` → `" aguardando o mundo"`.
- [ ] **Passo 4:** `cargo build --workspace && cargo test --workspace` — verde (inclusive `apps/gt-tui/tests/`).
- [ ] **Passo 5: commit** `feat(tui): procura do controller sem descartar o guardado e marca aguardando o mundo`.

### Tarefa 7: `gt ue player` com o caminho do mundo

Spec 6.7.

**Arquivos:**
- Modificar: `crates/gt-grounded2/src/player.rs` (`PlayerReport`, `GameStateReport`, `player_report`),
  `apps/gt-cli/src/cmd/ue.rs` (formatação, por volta da linha 312)
- Teste: `crates/gt-grounded2/src/player/tests.rs`

**Interfaces:**
- Produz: `GameStateReport { level, world, game_state: Option<u64>, path: String, hops: Vec<HopReport>, object:
  Option<u64>, error: Option<String> }` com `HopReport { name: String, addr: u64, class: String }` (classe pelo nome
  da instância, `BP_NormalModeSettings_C` no jogo); `PlayerReport.game_state: Vec<GameStateReport>` (uma entrada por
  `object` distinto entre os cheats de raiz `game_state`, na ordem dos `[[field]]`); `PlayerReport.pawn_error:
  Option<String>`; `player_report` só devolve `Err` para controller ausente/inválido.

- [ ] **Passo 1: testes que falham** (cena sintética):
  - `report_world_path_and_bools`: duas entradas em `game_state` (`PartyComponent` e
    `SurvivalModeManagerComponent.GameModeSettings`); a segunda com dois `hops` e a classe `BP_NormalModeSettings_C`;
    em `fields`, `construir-sem-ingredientes` como `Set` com valores `"true"` e `"false"` e `raw` 1 e 0, e
    `dano-recebido` com `"1.00x"`.
  - `report_world_survives_pawn_null`: `Pawn` zerado → `Ok`, `pawn_error == Some("Pawn nulo")`, cheats do pawn
    `Unavailable`, os do mundo com valores.
  - `report_world_hop_error`: `mode_manager` zerado → a entrada do mundo com `error` e sem `object`, os dois cheats
    `Unavailable` com o motivo.
- [ ] **Passo 2:** `cargo test -p gt-grounded2 report_world` — falham.
- [ ] **Passo 3: implementar.** `player_report` lê `read_player` (erro do pawn vira `pawn_error` e `Unavailable` nos
  cheats do pawn) e `read_world` para os `set` do mundo; a ação continua pelo `walk_game_state`. No `gt-cli`, imprimir
  uma seção por entrada de `game_state` (caminho, saltos com classe e endereço, erro) e a linha `pawn:` com o erro
  quando houver. O JSON muda junto (sem consumidor externo).
- [ ] **Passo 4:** `cargo test --workspace` — verde; `cargo tree -p gt-cli -e normal,build,features -i gt-core` sem
  `write`.
- [ ] **Passo 5: commit** `feat(cli): gt ue player mostra o caminho e os campos das configurações do modo`.

### Tarefa 8: Documentos

Spec 10.

**Arquivos:**
- Modificar: `docs/ARCHITECTURE.md` (5.3, 5.6, registro de decisões), `docs/specs/006-progressao-e-movimento.md`
  (notas em 6.3 e 6.4), `docs/specs/005-sobrevivencia-por-reflexao.md` (nota em 6.3), `docs/ROADMAP.md` (spec 007
  como complemento da Fase 5), `apps/gt-tui/tests/game.rs` (o teste `#[ignore]` de disponibilidade passa a esperar
  os dois cheats novos), e um teste `#[ignore]` novo `world_cheats_toggle_and_restore` no mesmo arquivo (liga e
  desliga os dois com o jogo aberto e confere `true`/`false`/`1.0` no fim pelo `player_report`).

- [ ] **Passo 1:** escrever as notas (uma ou duas frases cada, citando a spec 007) e o teste `#[ignore]`.
- [ ] **Passo 2:** `cargo test --workspace` — verde; `cargo test -p gt-tui --test game -- --list --ignored` mostra o
  teste novo.
- [ ] **Passo 3: commit** `docs: spec 007 na arquitetura, no roadmap e nas notas das specs 005 e 006`.

### Tarefa 9: Sessão com o jogo e critério de pronto (sessão principal, com o dono)

Spec 9 (itens 1 a 9), 6.1 (saídas), 7.

- [ ] **Passo 1:** pedir ao dono o backup do save e o FLiNG fechado; `cargo build --release --workspace`.
- [ ] **Passo 2:** `cargo test --workspace -- --ignored` com o jogo aberto (inclusive `world_cheats_toggle_and_restore`).
- [ ] **Passo 3:** com o dono, percorrer os itens 1 a 9 da spec, anotando cada resultado. Para os itens que dependem
  só de leitura, usar `gt ue player`.
- [ ] **Passo 4:** aplicar as saídas da spec 6.1 se algum cheat não funcionar (e voltar ao dono nos casos que a spec
  manda); registrar no `RESEARCH.md` o resultado (consertar, construção em andamento, efeito no pet e nas
  construções, save).
- [ ] **Passo 5:** marcar a spec como implementada e confirmada (tabela do topo) e o ROADMAP; commit
  `docs(research): critério de pronto da spec 007 no jogo`.
