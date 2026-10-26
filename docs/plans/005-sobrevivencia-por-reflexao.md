# Plano 005 — Sobrevivência por valores (reflexão do Unreal)

> **Para agentes executores:** SUB-SKILL OBRIGATÓRIA: use superpowers:subagent-driven-development (recomendado) ou superpowers:executing-plans para executar este plano tarefa a tarefa. Os passos usam checkbox (`- [ ]`).

**Objetivo:** oito cheats de sobrevivência por escrita de valor (seis travas e dois bits), achando classes e campos
pela reflexão do Unreal com o jogo rodando, integrados ao motor e à TUI, mais os comandos de leitura `gt ue`.

**Arquitetura:** o `gt-core` ganha a leitura em lote, a resolução RIP-relativa de globais (pura) e a escrita em dados
no motor, com entradas de bits no diário e na recuperação. O `gt-ue` (hoje vazio) vira o leitor genérico de reflexão
UE5: nomes, tabela de objetos, índice de classes e campos, mais um construtor de reflexão sintética para testes. O
`gt-grounded2` carrega `[ue]` e `[[field]]` das assinaturas, resolve as globais na mesma busca dos patches, acha o
personagem e planeja as escritas de cada ciclo. A TUI ganha uma tarefa de reflexão em segundo plano (índice e procura
do controller) e a manutenção de ~10 Hz no laço principal, que não conta como operação.

**Tecnologias:** Rust stable (edition 2024), `nix` (`process_vm_readv` com vários `iovec`), `serde`/`toml`,
`thiserror`, `tracing`, `clap`, `ratatui`.

**Spec:** `docs/specs/005-sobrevivencia-por-reflexao.md` (citada como "spec 6.x"). Specs 002/003 citadas como "spec
002 6.x"/"spec 003 6.x". Quem executa lê a spec e este plano.

## Restrições globais

- Regras do `AGENTS.md`: identificadores em inglês; comentários, docs e mensagens em pt-BR; `unsafe` só em
  `crates/gt-core/src/mem/` com `// SAFETY:`; `thiserror` nas libs e `anyhow` nos binários; sem `unwrap()`/`expect()`
  fora de testes; `tracing`, nunca `println!` em libs.
- **Nenhum offset de campo do jogo em código de produção nem nas assinaturas.** Campos vêm só da reflexão, por nome.
  Os offsets das estruturas de reflexão e os padrões ficam no `[ue]` do `generic.toml`. Os valores de `RESEARCH.md`
  só aparecem em testes `#[ignore]` e na conferência manual.
- Regra de dependência: `gt-ue → gt-core` (sem `write`); `gt-grounded2 → gt-ue`; `gt-cli → gt-ue, gt-grounded2`
  (continua sem `write`: `cargo tree -p gt-cli -e normal,build,features -i gt-core` sem `write` nem `test-support`).
  `gt-testkit → gt-ue` com a feature `synth`.
- O `cargo` está em `~/.cargo/bin` (`export PATH=$HOME/.cargo/bin:$PATH` se preciso).
- Validação de toda tarefa: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, mais `cargo build -p gt-cli` e `cargo clippy -p gt-cli -- -D warnings` isolados.
- Antes de testes do `gt-testkit`/`gt-tui`, rode `cargo build --workspace`, porque o `bin_path` precisa do
  `gt-testtarget`. Os testes que tocam o diário apontam o diretório para um `tempfile::TempDir`.
- **Valores fixados por este plano** (spec 12):
  - leitura em lote: até **1024** `iovec` por chamada;
  - varredura de objetos: blocos de até **1 MiB** da tabela, **1 ms** de pausa entre blocos e entre lotes, teto de
    **60 s**;
  - intervalo da tarefa de reflexão: `max(10 s, 2 × duração da última varredura)`, com repetição do índice por até
    **120 s** depois da conexão (o `r` reinicia o prazo);
  - nova procura do controller com `Pawn` nulo há mais de **30 s**;
  - tetos do leitor: cadeia de `SuperStruct` ≤ **64**, campos por struct ≤ **4096**, nome ≤ **1024** bytes, leitura
    de ponteiro só em endereço canônico de usuário (`0x10000 ≤ a < 0x8000_0000_0000`);
  - amostra da conferência barata: itens **0..16** e mais **16** itens espalhados por igual até `NumElements`;
  - manutenção no ciclo do laço existente (`LOOP_INTERVAL` = 100 ms).
- **Textos exatos** (os testes procuram estes trechos):
  - marcas de linha: `verificando…`, `aguardando o personagem`, `alvo inválido`;
  - linha do topo: `personagem: encontrado`, `personagem: não encontrado`, `personagem: —`;
  - motivos: `reflexão não reconhecida`, `reflexão incoerente`, `reflexão inconclusiva`, `classe <Nome> não
    encontrada`, `classe <Nome> ambígua`, `campo <Nome> não encontrado`. As globais usam os textos dos patches:
    `não encontrado`, `ambíguo`, `inconclusivo`, `fora do lugar`;
  - eventos: `personagem encontrado`, `personagem perdido`, `<nome>: alvo inválido`;
  - ligar durante a verificação: `<nome>: verificando a reflexão do jogo; tente de novo em instantes`;
  - `gt-patch toggle` com id de valor: `<id> é um cheat de valor; use o gt-tui`;
  - bit não restaurado na saída: o mesmo texto dos patches (spec 003), com o `id`.
- **Git:** branch `fase-5-sobrevivencia`. **O dono não autorizou commits** ("não faça commit sem eu pedir"). O passo
  "Commit" de cada tarefa só é feito se o dono autorizar commits por tarefa ao escolher a execução. Senão, a tarefa
  termina com a validação verde e o diff pronto. Mensagens em Conventional Commits pt-BR, terminando com
  `Claude-Session: https://claude.ai/code/session_01VTrKj6TmUd3YyVvJVxKE8q`.
- **Tarefas com o jogo** (1, 10 e 19) rodam na sessão principal, com o dono. As Tarefas 1 e 10 são só leitura.
  Antes da Tarefa 19, pedir o **backup do save**. Nunca usar `sudo` nem mudar o `ptrace_scope`. O FLiNG fica fechado.
- **Cuidado com a memória do jogo:** nenhum script ou comando varre o heap inteiro. As varreduras usam blocos de
  1 MiB com pausa, e `nice 19` nos scripts de `scratch/`.
- **Despacho:** cada subagente recebe esta seção inteira, a tarefa e as seções da spec citadas no cabeçalho dela.
- Testes de integração novos começam com `#![allow(clippy::unwrap_used, clippy::expect_used)]`.

## Foco de revisão

1. **O pawn muda entre a leitura e a escrita** (morte ou carregamento dentro de um ciclo). Esperado: a escrita confere
   a identidade do objeto (vtable, `InternalIndex`, `ClassPrivate`) imediatamente antes e não escreve. Teste
   `write_data_guard_mismatch_skips` (Tarefa 12).
2. **O jogo fecha ou reabre durante a tarefa de reflexão.** Esperado: o resultado é descartado pelo número da conexão,
   e nenhum endereço do processo antigo é usado. Teste `stale_reflection_outcome_dropped` (Tarefa 16).
3. **Alvo lido como NaN, infinito ou negativo absurdo** (campo de máximo ainda não inicializado no carregamento).
   Esperado: `alvo inválido`, sem escrita. Teste `nan_or_out_of_range_target_is_invalid` (Tarefa 8).
4. **O jogo muda outro bit do mesmo byte enquanto o nosso está limpo.** Esperado: a manutenção e a restauração só
   mexem nos bits da máscara e preservam os outros. Teste `restore_keeps_foreign_bits` (Tarefa 12).
5. **Diário com entradas de dados de outro processo ou boot** (o jogo reiniciou depois de um SIGKILL). Esperado: é
   descartado sem ler nem escrever nada. Teste `data_entries_other_process_discarded` (Tarefa 12).

---

## Mapa de arquivos

```
crates/gt-core/
  src/mem/mod.rs                    trait ReadMem; MemReader::read_many (lote de iovec)
  src/mem/test_support.rs           write_local (escrita no próprio processo, para a reflexão sintética)
  src/global.rs                     NOVO: GlobalPattern, rip_target, decide_global, GlobalVerdict
  src/journal.rs                    DataEntry, GuardBytes, Journal.data, decide_data, DataAction
  src/engine.rs                     DataPolicyFn, write_data, set_bits, restore_bits; undo_all/Drop com dados
  src/recovery.rs                   entradas de dados pela regra de decide_data
  src/error.rs                      DataOutsidePolicy, GuardMismatch, NoDataPolicy
crates/gt-ue/
  Cargo.toml                        gt-core, thiserror, tracing; feature synth
  src/lib.rs, error.rs              UeError
  src/layout.rs                     UeLayout (sem Default de produção)
  src/names.rs                      Names, decode_entry
  src/objects.rs                    ObjectArray, ScanBudget, scan
  src/check.rs                      cheap_check
  src/index.rs                      ReflectionIndex: classes, campos, is_a, instances_of, deep_check
  src/dump.rs                       describe_class
  src/synth.rs                      (feature synth) SynthUe, SparseMem
crates/gt-grounded2/
  src/signatures.rs                 [ue], [ue.layout], [ue.player], [[field]]; [ue] da versão
  src/ue.rs                         NOVO: resolução das globais na busca, UeScan
  src/catalog.rs                    padrões das globais na mesma passada; CatalogScan.ue
  src/player.rs                     NOVO: controller, caminho, FieldCatalog, snapshot, plan_tick, report
  src/fields.rs                     NOVO: FieldRuntime (executa o plano no motor)
  src/patching.rs                   open_engine com a política de dados
  signatures/generic.toml           [ue] (Tarefa 6) e [[field]] (Tarefa 10)
  signatures/c935e799-0b27c000.toml [ue] gobjects_rva/names_rva
crates/gt-testkit/
  src/bin/gt-testtarget.rs          --ue: reflexão sintética na própria memória
  src/lib.rs                        TargetOpts.ue, TestTarget.ue: Option<UeAddrs>
  src/bin/gt-patch-harness.rs       subcomando set-bits
  tests/ue_live.rs                  NOVO: leitor e caminho contra o alvo
  tests/fields_live.rs              NOVO: escrita em dados e FieldRuntime contra o alvo
  tests/engine_live.rs, harness.rs  testes de dados do motor e de sinais
apps/gt-cli/src/cmd/ue.rs           NOVO: gt ue class|player
apps/gt-patch/src/cmd/toggle.rs     recusa de id de [[field]]
apps/gt-tui/src/
  model.rs, ui.rs                   Avail::Checking, CheatRow.note, linha personagem
  backend.rs                        Session: tarefa de reflexão, field_on/off/tick
  fake.rs, game.rs, raw.rs          implementações
  controller.rs                     agendamento da tarefa, manutenção, regras de nova tentativa
apps/gt-tui/tests/core_raw.rs       testes com o alvo --ue
docs/                               RESEARCH, ARCHITECTURE, AGENTS, ROADMAP, spec 002/003, CLAUDE.md
```

---

### Tarefa 1: Pesquisa P1 — padrões das globais (sessão principal, só leitura, jogo aberto)

Spec 6.1 (passo 1, P1), 6.2 ("Globais por padrão AOB").

- [ ] **Passo 1:** `cargo run -q -p gt-cli -- inspect`. Esperado: `c935e799-0b27c000`. Outra chave: pare e avise o
  dono.
- [ ] **Passo 2:** script `scratch/fling/tools/globals.py` (somente leitura, `nice 19`, `/proc/<pid>/mem` em blocos de
  1 MiB com 1 ms de pausa, só a `.text` do exe). Para um padrão com um curinga de 4 bytes no `disp32`, ele:
  - lista cada ocorrência e o alvo `fim da instrução + disp32`;
  - conta os alvos distintos;
  - para um alvo dado (`--target 0x14a499e50`), procura toda instrução RIP-relativa da `.text` que referencia esse
    alvo ou o alvo menos 0x0–0x40, decodificando os candidatos com `gt disasm`.
- [ ] **Passo 3:** confere o padrão `48 8b 05 ?? ?? ?? ?? 48 8b 0c c8 48 8d 04 d1` (`disp_offset` 3, `insn_end` 7).
  Esperado: todas as ocorrências resolvem para `+a53bc30`. Se o alvo for o início da `FUObjectArray` e não a lista de
  blocos, anotar o `adjust`.
- [ ] **Passo 4:** acha as instruções que referenciam o `FNamePool` (`+a499e50` ou a estrutura que o contém) e monta
  um padrão (com o contexto) que cumpra a regra da spec 6.2, conferido na memória e no disco (`gt scan`).
- [ ] **Passo 5:** registra no `RESEARCH.md` (sessão nova "Fase 5: globais"): padrões, `disp_offset`, `insn_end`,
  `adjust`, contagem e alvo de cada um, mais as hipóteses descartadas.
- [ ] **Portão:** se alguma global não tiver padrão aceitável, **pare** e leve ao dono (spec 6.1).

---

### Tarefa 2: Leitura em lote e interface de leitura no `gt-core`

Spec 6.2 (Entradas), 6.3 (varredura).

**Arquivos:** modificar `crates/gt-core/src/mem/mod.rs` e `crates/gt-core/src/mem/test_support.rs`; teste em
`crates/gt-testkit/tests/core_live.rs`.

**Interfaces (produz):**
- `pub trait ReadMem { fn read_exact(&self, addr: u64, buf: &mut [u8]) -> Result<()>; fn read_many(&self, reqs: &[(u64, usize)]) -> Result<Vec<Option<Vec<u8>>>> { /* padrão: um read_exact por pedido; Unmapped vira None */ } }`.
  Erros que não são `Unmapped` (`ProcessGone`, `PermissionDenied`) interrompem e voltam como `Err`.
- `impl ReadMem for MemReader`. O `read_many` usa `process_vm_readv` com até 1024 `iovec` remotos por chamada.
  Numa leitura curta, o pedido em que ela parou vira `None` (relido sozinho com `read_exact`, que decide entre `None`
  e o erro), e o lote continua do pedido seguinte. A ordem do resultado é a dos pedidos.
- `test_support::write_local(addr: u64, bytes: &[u8]) -> Result<()>`: copia para a memória do próprio processo. O
  `// SAFETY:` exige endereço de uma região `rw` alocada pelo próprio teste (`anon_rw`).

- [ ] **Passo 1: testes que falham** (`core_live.rs`, contra `TestTarget::spawn()`):
  - `read_many_more_than_iov_max`: 3000 pedidos de 8 bytes em `big_region` (offsets `i * 8`). Todos `Some`, e iguais
    aos lidos com `read_exact`;
  - `read_many_hole_in_the_middle`: pedidos antes de `region_start`, dentro da página de guarda e depois dela. O da
    guarda é `None`, e os outros, `Some` com os bytes certos, na ordem;
  - `read_many_gone_process_is_error`: alvo morto → `Err(ProcessGone)`.
- [ ] **Passo 2:** os testes falham (método inexistente).
- [ ] **Passo 3:** implementar.
- [ ] **Passo 4:** testes e validação global passam.
- [ ] **Passo 5:** commit `feat(core): leitura em lote e interface ReadMem` (ver Git).

---

### Tarefa 3: Resolução de globais por padrão (pura, `gt-core`)

Spec 6.2 ("Globais por padrão AOB", regra de unicidade para globais).

**Arquivos:** criar `crates/gt-core/src/global.rs` e registrar em `lib.rs` (sem feature). Testes em `mod tests`.

**Interfaces (produz):**
- `pub struct GlobalPattern { pub pattern: Pattern, pub disp_offset: usize, pub insn_end: usize, pub adjust: i64 }`.
  `insn_end` é a distância do início da ocorrência até o fim da instrução que tem o `disp32`.
- `pub fn rip_target(hit: u64, disp: i32, insn_end: usize, adjust: i64) -> Option<u64>`: `hit + insn_end + disp +
  adjust`, com `None` em estouro.
- `pub enum GlobalVerdict { Found(u64), NotFound, Ambiguous(Vec<u64>), Inconclusive }`.
- `pub fn decide_global(targets: &[u64], verdict: Verdict) -> GlobalVerdict`. `verdict` é o da busca (`memscan`), e
  `targets` são os alvos já calculados das ocorrências legíveis. Regras:
  - `None` → `NotFound`; `InconclusiveNone` → `Inconclusive`;
  - com 1 ou mais alvos: todos iguais → `Found`, senão `Ambiguous` (lista sem repetição, ordenada).
- `pub fn label(&self) -> &'static str` em `GlobalVerdict`: `não encontrado`, `ambíguo`, `inconclusivo` (no `Found`,
  `disponível`).

- [ ] **Passo 1: testes:** `rip_target_positive_and_negative_disp`, `rip_target_overflow_is_none`,
  `decide_one_hit`, `decide_many_agreeing` (281 alvos iguais → `Found`), `decide_many_disagreeing` (→ `Ambiguous`
  com 2 alvos), `decide_inconclusive_none`, `decide_inconclusive_unique_counts` (`InconclusiveUnique` com um alvo →
  `Found`).
- [ ] **Passo 2:** os testes falham. **Passo 3:** implementar. **Passo 4:** passam.
- [ ] **Passo 5:** commit `feat(core): resolução RIP-relativa de globais por padrão`.

---

### Tarefa 4: `gt-ue` — layout, nomes, tabela de objetos, conferência barata e reflexão sintética

Spec 6.2 (layout, conferência barata, tetos), 6.3 (varredura).

**Arquivos:** criar os arquivos do `gt-ue` do mapa (menos `index.rs`/`dump.rs`). No `Cargo.toml`: `gt-core`,
`thiserror`, `tracing`, e `[features] synth = []`. Em `[dev-dependencies]`, o próprio `gt-ue` com `synth`.

**Interfaces (produz):**
- `pub struct UeLayout { pub objects: ObjectsLayout, pub uobject: UObjectLayout, pub ustruct: UStructLayout, pub ffield: FFieldLayout, pub fprop: FPropLayout, pub fbool: FBoolLayout }`,
  com os campos da tabela da spec 6.2 (todos `u32`, nomes em snake_case: `chunks`, `max_elements`, `num_elements`,
  `max_chunks`, `num_chunks`, `items_per_chunk`, `item_size`, `item_object`; `flags`, `index`, `class`, `name`,
  `outer`, `cdo_flag`; `super_struct`, `child_properties`, `properties_size`; `class_private`, `next`, `name`;
  `element_size`, `offset`; `field_size`, `byte_offset`, `byte_mask`, `field_mask`). **Sem `Default`.** Com a feature
  `synth`, `UeLayout::observed()` devolve os valores do build atual, só para testes.
- `pub enum UeError { Read { addr: u64 }, Malformed(String), NotFound(String), Ambiguous(String), Deadline, Core(gt_core::Error) }`
  (`thiserror`, mensagens em pt-BR).
- `pub fn decode_entry(header: u16, raw: &[u8]) -> Result<String, UeError>` (tamanho `header >> 6`, bit 0 = UTF-16LE,
  latin-1 no ANSI, teto 1024 bytes).
- `pub struct Names<'m, M: ReadMem> { … }`: `new(mem: &'m M, pool_blocks: u64) -> Result<Self, UeError>`, que lê os
  ponteiros dos blocos; `name(&self, index: u32) -> Result<String, UeError>`, com cache interno; `fname(&self,
  comparison: u32, number: u32) -> Result<String, UeError>`, que acrescenta `_{number-1}` quando `number > 0`.
- `pub struct ObjectArray { pub chunks: Vec<u64>, pub num_elements: u32, pub max_elements: u32, pub num_chunks: u32, pub max_chunks: u32 }`:
  - `read(mem, layout, gobjects) -> Result<Self, UeError>`;
  - `object_at(mem, layout, index) -> Result<Option<u64>, UeError>`;
  - `scan(mem, layout, budget: &ScanBudget, visit: impl FnMut(ObjHeader)) -> Result<ScanEnd, UeError>`.
- `pub struct ScanBudget { pub block_bytes: usize, pub batch: usize, pub pause: Duration, pub deadline: Duration, pub sleep: fn(Duration) }`.
  `ScanBudget::standard()` usa 1 MiB, 1024, 1 ms, 60 s e `std::thread::sleep`. Nos testes, `sleep` é uma função que
  não dorme.
- `pub struct ObjHeader { pub index: u32, pub addr: u64, pub flags: u32, pub internal_index: u32, pub class: u64, pub name: (u32, u32), pub outer: u64 }`.
- `pub enum ScanEnd { Done, DeadlineExceeded }`.
- `pub fn cheap_check(mem, layout, gobjects, names_blocks) -> Result<(), String>`: os três itens baratos da spec 6.2
  (nome 0 `None`, contagens, amostra). O `Err` traz o detalhe para o log. O motivo mostrado é sempre
  `reflexão não reconhecida`.
- (feature `synth`) `pub struct SynthUe` (construtor) e `pub struct SparseMem` (`impl ReadMem`, mais `write(addr,
  bytes)` para os testes simularem o jogo):
  - `SynthUe::new(layout: UeLayout, arena: u64, arena_len: usize)`;
  - `add_package(name)`, `add_class(pkg, name, super_class: Option<u64>, props_size, fields: &[SynthField]) -> u64`,
    `add_object(class, name, outer) -> u64`, `set(obj, offset, bytes)`;
  - `finish(self) -> SynthImage { pub writes: Vec<(u64, Vec<u8>)>, pub gobjects: u64, pub names: u64 }`.

  `SynthField { name, kind: &'static str, offset: u32, size: u32, bool_mask: Option<(u8 /*byte_offset*/, u8 /*field_mask*/)> }`.
  O construtor cria sozinho `Class` (que é classe de si mesma), os pacotes `/Script/CoreUObject` e `/Script/Engine`,
  e as classes `Object`, `Class` e `Actor` (com `RootComponent` do tipo `ObjectProperty`).

- [ ] **Passo 1: testes** (`mod tests`, com `SparseMem`):
  - `decode_ansi_and_utf16`, `decode_rejects_oversized`;
  - `fname_with_number`, `name_zero_is_none`;
  - `object_at_reads_item_offset`;
  - `scan_visits_all_skipping_null`;
  - `scan_respects_deadline` (budget com `deadline = 0` → `DeadlineExceeded`);
  - `cheap_check_ok_on_synth`;
  - `cheap_check_rejects`: nome 0 ≠ `None`; `NumElements > MaxElements`; `NumChunks × 65536 < NumElements`; amostra
    com `InternalIndex` trocado;
  - `cheap_check_accepts_extra_chunks` (10 blocos para 506 mil elementos → `Ok`).
- [ ] **Passo 2:** falham. **Passo 3:** implementar. **Passo 4:** passam.
- [ ] **Passo 5:** commit `feat(ue): nomes, tabela de objetos e conferência barata da reflexão`.

---

### Tarefa 5: `gt-ue` — índice de classes e campos

Spec 6.2 (operações, âncora, família de propriedades de objeto, parte da conferência feita no índice), 6.8
(`gt ue class`).

**Arquivos:** criar `crates/gt-ue/src/index.rs` e `dump.rs`; testes em `mod tests` (com `SparseMem`).

**Interfaces:**
- Consome: `ObjectArray::scan`, `Names`, `UeLayout`, `ScanBudget` (Tarefa 4).
- Produz:
  - `pub struct ReflectionIndex { … }`, construída por `ReflectionIndex::build(mem, layout, objects: &ObjectArray,
    names: &Names, budget: &ScanBudget) -> Result<ReflectionIndex, UeError>`. Uma passada pela tabela:
    - acha a âncora `Class` (o objeto `/Script/CoreUObject.Class` cuja classe é ele mesmo);
    - guarda todo objeto cuja classe é `is_a(Class)`, com cache por ponteiro de classe, numa tabela `(pacote, nome) →
      Vec<u64>`;
    - em `DeadlineExceeded`, devolve `UeError::Deadline`.
  - `deep_check(&self) -> Result<(), String>`: `Object`, `Class` e `Actor` achados, e `Actor.RootComponent` da
    família de objeto. Falha vira o motivo `reflexão incoerente`.
  - `find_class(&self, pkg: &str, name: &str) -> Result<u64, UeError>` (`NotFound`/`Ambiguous`);
    `classes_named(&self, name) -> Vec<(String /*pacote*/, u64)>`.
  - `find_field(&self, mem, class: u64, name: &str) -> Result<Field, UeError>`: sobe pela `SuperStruct`, com os tetos.
  - `pub struct Field { pub owner: String, pub name: String, pub kind: String, pub offset: u32, pub size: u32, pub bits: Option<BoolBits> }`;
    `pub struct BoolBits { pub byte: u32 /* offset + ByteOffset */, pub mask: u8 /* FieldMask */ }`.
  - `pub fn is_object_family(kind: &str) -> bool` (`ObjectProperty`, `ObjectPtrProperty`).
  - `is_a(&self, mem, class: u64, ancestor: u64) -> Result<bool, UeError>`; `class_of(mem, obj) -> Result<u64,
    UeError>`; `props_size(mem, class) -> Result<u32, UeError>`.
  - `instances_of(&self, mem, objects, class: u64, budget) -> Result<Vec<u64>, UeError>`: só não-CDO, a classe ou
    suas filhas.
  - Em `dump.rs`: `describe_class(index, mem, query: &str) -> Result<ClassQuery, UeError>`, com `pub enum ClassQuery
    { Found(ClassDump), Ambiguous(Vec<String /*Pacote.Nome*/>) }` e `pub struct ClassDump { pub path: String, pub
    fields: Vec<Field> }`. A `query` aceita `Pacote.Nome` ou só `Nome`.

- [ ] **Passo 1: testes:**
  - `index_finds_anchor_and_classes`;
  - `find_class_wrong_package_not_found`;
  - `find_class_blueprint_child_of_class` (classe cuja classe é uma `BlueprintGeneratedClass` filha de `Class` é
    achada);
  - `find_class_two_same_name_ambiguous`;
  - `find_field_walks_super`;
  - `find_field_bool_bits` (byte = offset + `ByteOffset`, máscara = `FieldMask`);
  - `super_chain_cycle_is_malformed` (ciclo na `SuperStruct` → `Malformed`, sem laço infinito);
  - `field_list_cap`;
  - `object_ptr_property_is_object_family`;
  - `deep_check_rejects_missing_root_component`;
  - `instances_skip_cdo_and_include_children`;
  - `describe_class_ambiguous_lists_paths`.
- [ ] **Passo 2:** falham. **Passo 3:** implementar. **Passo 4:** passam.
- [ ] **Passo 5:** commit `feat(ue): índice de classes e campos da reflexão`.

---

### Tarefa 6: Assinaturas `[ue]`, `[ue.player]` e `[[field]]`

Spec 6.2 (layout configurável, `rva` depois do `adjust`), 6.5 (formato do `[[field]]`, ordem, `id` único), 6.6
(`gt-patch`).

**Arquivos:** modificar `crates/gt-grounded2/src/signatures.rs`, `crates/gt-grounded2/Cargo.toml` (`gt-ue`),
`signatures/generic.toml` e `signatures/c935e799-0b27c000.toml` (só o `[ue]`, com os padrões da Tarefa 1),
`apps/gt-patch/src/cmd/toggle.rs` e `crates/gt-grounded2/src/error.rs`.

**Interfaces (produz):**
- TOML do genérico:
  - `[ue]` com `gobjects = { pattern, disp_offset, insn_end, adjust }` e `names = { … }`;
  - `[ue.layout]` com as chaves de `UeLayout` em tabelas (`objects`, `uobject`, `ustruct`, `ffield`, `fprop`, `fbool`);
  - `[ue.player]` com `controller_class = "/Script/Maine.SurvivalPlayerController"`, `pawn_field = "Pawn"`,
    `pawn_class = "/Script/Maine.SurvivalPlayerCharacter"` e `alt_pawn_fields = []`.
- TOML da versão: `[ue] gobjects_rva = 0x…, names_rva = 0x…` e, opcionalmente, `[ue.layout]` (substitui o bloco
  inteiro).
- `[[field]]`:
  - `id`, `name`, `category`, `description`;
  - `object`: o nome do campo de componente no pawn, ou `"pawn"` para o próprio pawn;
  - `object_class` (`Pacote.Nome`, conferido com `is_a`);
  - `rule = "lock" | "clear_bits"`;
  - na trava: `field`, `kind = "float"`, `mode = "raise" | "lower" | "exact"`, `target_field` **ou** `target`
    (constante), `min` e `max`;
  - nos bits: `fields = [...]`, `kind = "bool"`.
- `pub struct UeSigs { pub layout: UeLayout, pub gobjects: GlobalDef, pub names: GlobalDef, pub player: PlayerCfg }`;
  `pub struct GlobalDef { pub pattern: GlobalPattern, pub rva: Option<u64> }`;
  `pub struct PlayerCfg { pub controller_class: String, pub pawn_field: String, pub pawn_class: String, pub alt_pawn_fields: Vec<String> }`.
- `pub struct FieldDef { pub id, pub name, pub category: Category, pub description, pub object: String, pub object_class: String, pub rule: FieldRule }`;
  `pub enum FieldRule { Lock { field: String, mode: LockMode, target: LockTarget, min: f32, max: f32 }, ClearBits { fields: Vec<String> } }`;
  `pub enum LockMode { Raise, Lower, Exact }`; `pub enum LockTarget { Field(String), Const(f32) }` (outro nome para não
  colidir com o `Target` do processo).
- `Signatures::ue_for_version(&self, key) -> Option<UeSigs>` (`None` sem `[ue]` no genérico);
  `Signatures::fields(&self) -> &[FieldDef]`.
- Regras de carga: erro claro com arquivo e campo; `id` único entre `[[patch]]` e `[[field]]`; `min ≤ max` e os dois
  finitos; `target` e `target_field` não ao mesmo tempo; `fields` não vazio; `kind` coerente com a regra.
- `gt-patch toggle <id>` com id de `[[field]]` → erro `<id> é um cheat de valor; use o gt-tui` (variante nova
  `Error::FieldCheat { id }`). O `list` continua só com os patches.

- [ ] **Passo 1: testes** (`signatures.rs`):
  - `ue_block_loads_and_version_rva`;
  - `ue_layout_override_replaces_block`;
  - `field_block_lock_and_bits`;
  - `field_id_collides_with_patch`;
  - `field_target_and_target_field_both_is_error`;
  - `field_min_gt_max_is_error`;
  - `unknown_field_key_is_error`;
  - `real_files_load` (estendido: `[ue]` presente e padrões da Tarefa 1);
  - no `gt-patch` (`tests/cli.rs`): `toggle_field_id_refused` (assinaturas de teste com um `[[field]]`, ou o embutido
    depois da Tarefa 10; se o caminho de teste não aceitar assinaturas próprias, o teste fica para a Tarefa 10).
- [ ] **Passo 2:** falham. **Passo 3:** implementar e preencher o `[ue]` nos dois TOML. **Passo 4:** passam.
- [ ] **Passo 5:** commit `feat(grounded2): assinaturas da reflexão e dos cheats de valor`.

---

### Tarefa 7: Globais na busca de padrões e conferência barata

Spec 6.2 (busca em memória e disco, regras do `rva`, bytes ilegíveis), 6.7 (motivos das globais, `?`).

**Arquivos:** criar `crates/gt-grounded2/src/ue.rs`; modificar `catalog.rs`.

**Interfaces:**
- Consome: `UeSigs` (Tarefa 6), `decide_global` (Tarefa 3), `cheap_check` (Tarefa 4).
- Produz:
  - `pub enum GlobalState { Verified(u64), Unverified(u64), Unavailable(&'static str) }`. O `&'static str` é um dos
    textos dos patches, mais `fora do lugar`.
  - `pub struct UeScan { pub sigs: UeSigs, pub gobjects: GlobalState, pub names: GlobalState, pub cheap: Result<(), String> }`.
    `cheap` só é calculado com as duas globais disponíveis; senão fica `Err("globais indisponíveis")`.
  - `CatalogScan { …, pub ue: Option<UeScan> }`. Os dois padrões entram na mesma passada da `.text` dos patches
    (`scan_defs`).
  - Regras:
    - memória e disco seguem a regra existente de quando o disco é lido;
    - com os dois lidos, o alvo tem de ser igual nos dois, senão `inconclusivo`;
    - com `rva`, alvo diferente de `base + rva` → `fora do lugar`;
    - sem arquivo de versão, ou sem `rva`, → `Unverified`;
    - o alvo tem de cair numa seção de dados (não executável) do módulo, senão `não encontrado`.
  - `scan_subset` também resolve as globais quando recebe o id especial `"@ue"`. O controlador usa isso na nova busca
    automática.

- [ ] **Passo 1: testes** (`ue.rs`/`catalog.rs`, com buffers: as funções de decisão recebem as ocorrências e os bytes,
  como o `resolve` de hoje):
  - `global_verified_with_rva`, `global_out_of_place`, `global_unverified_without_version`;
  - `global_target_in_text_is_not_found`;
  - `global_disk_and_memory_disagree_inconclusive`;
  - `cheap_check_skipped_without_globals`.
- [ ] **Passo 2:** falham. **Passo 3:** implementar. **Passo 4:** passam.
- [ ] **Passo 5:** commit `feat(grounded2): globais do Unreal na busca de padrões`.

---

### Tarefa 8: Caminho até o personagem e planejamento das escritas

Spec 6.3 (controller, revalidação, desempate), 6.4 (conferências), 6.5.1 (regras das travas), 6.5.2 (bits).

**Arquivos:** criar `crates/gt-grounded2/src/player.rs`; testes em `mod tests` com `gt_ue::synth`
(`gt-grounded2` ganha `gt-ue` com `synth` em `dev-dependencies`).

**Interfaces:**
- Consome: `ReflectionIndex`, `Field`, `ObjectArray`, `ScanBudget` (Tarefas 4–5), `FieldDef`/`PlayerCfg` (Tarefa 6).
- Produz:
  - `pub struct FieldCatalog { … }`, criado por `FieldCatalog::resolve(index, mem, cfg: &PlayerCfg, defs: &[FieldDef]) -> FieldCatalog`.
    Para cada `id`, guarda `Result<ResolvedField, String /*motivo exato da lista de textos*/>`. Também resolve uma vez
    as classes do controller e do pawn e os campos `pawn_field`/`alt_pawn_fields` (família de objeto).
    `avail(&self, id) -> Result<(), String>`.
  - `pub enum ControllerFind { Found(u64), Absent, Ambiguous }`: `find_controller(index, mem, objects, catalog,
    budget) -> Result<ControllerFind, UeError>`. Faz `instances_of` e o desempate pelo `Pawn` não nulo.
  - `pub struct PlayerSnapshot { pub controller: u64, pub pawn: u64, pub objects: Vec<ObjSnap>, pub values: HashMap<String /*id*/, FieldValues> }`.
    Em `ObjSnap { addr, guard: Vec<GuardBytes> }` (`gt_core::journal::GuardBytes`, Tarefa 12; se a Tarefa 8 vier
    antes, ela cria o tipo no `journal.rs` sem mudar o resto), a guarda tem vtable (8 bytes em `+0`),
    `InternalIndex` e `ClassPrivate`.
  - `pub enum Absent { NoController, ControllerInvalid, PawnNull, PawnWrongClass, ComponentWrongClass(String), Unreadable }`:
    `read_player(mem, catalog, objects_arr, controller: u64) -> Result<PlayerSnapshot, Absent>`. Faz os passos 1–4 da
    spec 6.3, com cache de `is_a` por (endereço, classe), e as conferências de 6.4 que não dependem do `maps`
    (`offset + tamanho ≤ PropertiesSize`). Se `Pawn` não for `is_a(pawn_class)`, testa os `alt_pawn_fields`.
  - `pub enum FieldAction { WriteF32 { id: String, addr: u64, value: f32, guard: Vec<GuardBytes> }, ClearBits { id: String, addr: u64, mask: u8, guard: Vec<GuardBytes> }, InvalidTarget { id: String, target: f32 } }`.
  - `pub fn plan_tick(snapshot: &PlayerSnapshot, catalog: &FieldCatalog, on: &[String]) -> Vec<FieldAction>`, com as
    regras da spec 6.5.1:
    - `Raise`: só escreve se atual < alvo;
    - `Lower`: só se atual > alvo;
    - `Exact`: só se atual ≠ alvo;
    - alvo não finito ou fora de `[min, max]` → `InvalidTarget`;
    - bits: `ClearBits` só se algum bit da máscara está ligado. Com 2 campos no mesmo byte, as máscaras são unidas; em
      bytes diferentes, gera uma ação por byte.
  - `pub struct PlayerReport` (`serde::Serialize`): controller, `Pawn`/`Character`/`AcknowledgedPawn` (lidos pela
    reflexão de `Controller`/`PlayerController`), os valores e alvos de cada `[[field]]` e os bits.
    `player_report(mem, index, catalog, controller) -> Result<PlayerReport, Absent>`.
  - `pub fn controller_needs_search(valid: bool, pawn_null_since: Option<Instant>, now: Instant) -> bool`: inválido,
    ou `Pawn` nulo há mais de 30 s.

- [ ] **Passo 1: testes** (`SynthUe` com um controller, um pawn `SurvivalPlayerCharacter` e componentes em offsets
  **diferentes** dos do jogo, e uma criatura `SurvivalCharacter` com os mesmos componentes):
  - `resolves_all_fields_by_name`;
  - `missing_field_reason_text` (`campo CurrentFood não encontrado`);
  - `two_controllers_pick_non_null_pawn`, `two_controllers_both_null_ambiguous`;
  - `pawn_null_is_absent`;
  - `mount_in_pawn_never_targeted` (`Pawn` aponta para a criatura → `PawnWrongClass`, e nenhuma ação);
  - `component_wrong_class_absent`;
  - `props_size_exceeded_absent`;
  - `raise_never_lowers` (120 com alvo 100 → nenhuma ação);
  - `lower_writes_zero_damage`;
  - `exact_temperature`;
  - `writes_only_when_changed`;
  - `nan_or_out_of_range_target_is_invalid` (Foco 3);
  - `bits_same_byte_merged_mask`, `bits_already_clear_no_action`;
  - `controller_needs_search_after_30s_null`.
- [ ] **Passo 2:** falham. **Passo 3:** implementar. **Passo 4:** passam.
- [ ] **Passo 5:** commit `feat(grounded2): caminho até o personagem e plano das escritas de valor`.

---

### Tarefa 9: `gt ue class` e `gt ue player`

Spec 6.8.

**Arquivos:** criar `apps/gt-cli/src/cmd/ue.rs`; modificar `apps/gt-cli/src/main.rs`, `cmd/mod.rs` e `Cargo.toml`
(`gt-ue`). A lógica fica em `gt_ue::dump` e `gt_grounded2::player`. O `gt-cli` só resolve as globais (pela busca, ou
por `--gobjects`/`--names`, no formato do `gt read`) e formata texto ou `--json`.

**Interfaces:**
- Consome: `describe_class` (Tarefa 5), `UeScan` (Tarefa 7), `FieldCatalog`/`find_controller`/`player_report`
  (Tarefa 8).
- Produz:
  - `gt ue class <Classe> [--gobjects A --names A] [--json]`;
  - `gt ue player [--gobjects A --names A] [--controller A] [--watch SEG] [--json]`.
  - `gt_grounded2::player::watch_player(mem, index, catalog, controller: Option<u64>, every: Duration, rounds:
    Option<u32>, sleep: &mut dyn FnMut(Duration), out: &mut dyn FnMut(&PlayerReport))`. Só varre de novo quando o
    controller deixa de ser válido.

- [ ] **Passo 1: testes:**
  - em `gt-grounded2` (`SparseMem`): `watch_player_rounds_without_rescan` (3 rodadas, a varredura só na primeira) e
    `watch_player_rescans_when_controller_invalid`;
  - em `gt-ue`: `describe_class_found_lists_super_fields`.
- [ ] **Passo 2:** falham. **Passo 3:** implementar o subcomando. O caminho sem jogo dá a mesma mensagem dos outros
  comandos.
- [ ] **Passo 4:** passam. `cargo tree -p gt-cli … -i gt-core` continua sem `write`.
- [ ] **Passo 5:** commit `feat(cli): comandos gt ue class e gt ue player`.

---

### Tarefa 10: Pesquisa P2–P9 e definições dos cheats (sessão principal, só leitura, jogo aberto)

Spec 6.1 (P2–P9, custo medido), 6.5 (tabelas), 7.

- [ ] **Passo 1:** `gt ue player` (sem endereços) mede a busca e a varredura. Registrar o tempo do índice e da
  procura do controller no `RESEARCH.md`. Se a varredura passar de 60 s, **pare** e leve ao dono (o teto da spec não
  serviria).
- [ ] **Passo 2:** `gt ue class` em `SurvivalCharacter`, `SurvivalPlayerCharacter`, `HealthComponent`,
  `StaminaComponent`, `SurvivalComponent`, `BodyTemperatureComponent`, `Controller`, `PlayerController` e
  `SurvivalPlayerController`, para responder P2, P3, P5 e P8. Inclui conferir `OuterPrivate`,
  `FieldSize`/`ByteOffset` e o byte de `bCanDie…` contra o `+0x11d` já lido.
- [ ] **Passo 3:** `gt ue player --watch 2` com o dono:
  - P3: correr até a stamina cair;
  - P4: frio e calor;
  - P6: ver se os bits voltam sozinhos;
  - P7: morrer, ir ao menu, carregar outro save e montar, se houver montaria.
- [ ] **Passo 4:** P9 por pesquisa estática: `gt scan` pelo acesso ao byte de `bTakeFallDamage` (offset da reflexão)
  e `gt disasm` dos usos.
- [ ] **Passo 5:** registrar P2–P9 no `RESEARCH.md`. Escrever os oito `[[field]]` no `generic.toml`, com:
  - `object` e `fields` confirmados em P8;
  - alvos e faixas sãs de P2–P4, com folga para upgrades e a origem anotada em comentário;
  - `vida-infinita` com `mode = "lower"` e `target = 0`, `temperatura-estavel` com `mode = "exact"`, e as barras com
    `mode = "raise"`.

  Escrever também os `rva` das globais na versão e os `alt_pawn_fields` se P7 pedir.
- [ ] **Portão:** se P4 não tiver valor neutro único, ou se P9 mostrar que o jogo não consulta `bTakeFallDamage`, ou
  se outra resposta mudar **o que** um cheat faz, pare e leve ao dono antes da Tarefa 12. A mudança volta para a
  spec. Se P9 for inconclusiva, siga (spec 6.1).
- [ ] **Passo 6:** `real_files_load` confere os oito `[[field]]` na ordem da spec 6.5, todos `sobrevivencia`. O
  `toggle_field_id_refused` passa a usar o embutido, se ficou pendente da Tarefa 6.
- [ ] **Passo 7:** commit `docs(research): reflexão do personagem; feat(grounded2): definições dos cheats de
  sobrevivência`.

---

### Tarefa 11: Reflexão sintética no `gt-testtarget` e leitura contra o processo

Spec 8 (reflexão sintética no alvo, endereços das globais entregues direto).

**Arquivos:** modificar `crates/gt-testkit/Cargo.toml` (`gt-ue` com `synth`, `gt-grounded2`),
`src/bin/gt-testtarget.rs` e `src/lib.rs`; criar `tests/ue_live.rs`.

**Interfaces (produz):**
- `TargetOpts { …, pub ue: bool }` → `--ue`. O alvo aloca uma arena `anon_rw` de 4 MiB, monta a mesma cena do teste da
  Tarefa 8 com o `SynthUe` e grava tudo com `write_local`. Os campos usam os nomes reais e offsets diferentes dos do
  jogo.
- `TestTarget { …, pub ue: Option<UeAddrs> }`, com `pub struct UeAddrs { pub gobjects: u64, pub names: u64, pub
  layout_json: String, pub controller: u64, pub pawn: u64, pub creature: u64, pub pawn_field_addr: u64, pub
  health: u64, pub alt_pawn: u64 }`. O JSON do alvo ganha a chave `ue`. `alt_pawn` é um segundo
  `SurvivalPlayerCharacter` "novo", não referenciado, para simular a troca. Também é exportada a função
  `gt_testkit::synth_signatures() -> (UeSigs, Vec<FieldDef>)`, com `[ue.player]` e `[[field]]` iguais aos do jogo e o
  layout da cena.

- [ ] **Passo 1: testes** (`ue_live.rs`, alvo com `ue: true`):
  - `live_cheap_check_and_index`;
  - `live_find_controller_and_read_player`;
  - `live_scan_batches_with_budget` (a varredura usa `read_many` e termina);
  - `live_pawn_swap_and_null` (o teste escreve no alvo com `MemWriter`; o `read_player` acompanha).
- [ ] **Passo 2:** falham. **Passo 3:** implementar. **Passo 4:** passam.
- [ ] **Passo 5:** commit `test(testkit): reflexão sintética no alvo de teste`.

---

### Tarefa 12: Escrita em dados no motor, diário e recuperação

Spec 6.6 inteira, 6.5.2 (uma entrada por `id` e byte), 7.

**Arquivos:** modificar `crates/gt-core/src/journal.rs`, `engine.rs`, `recovery.rs`, `error.rs` e
`crates/gt-testkit/src/bin/gt-patch-harness.rs`; testes em `journal.rs` (`mod tests`), `engine_live.rs` e
`harness.rs`.

**Interfaces (produz):**
- `pub struct GuardBytes { pub addr: u64, pub bytes: Vec<u8> }` (hex no JSON).
- `pub struct DataEntry { pub id: String, pub addr: u64, pub original: u8, pub written: u8, pub mask: u8, pub guard: Vec<GuardBytes> }`.
- `Journal { …, #[serde(default, skip_serializing_if = "Vec::is_empty")] pub data: Vec<DataEntry> }`. Literais
  existentes de `Journal { … }` ganham `data: Vec::new()`.
- `pub enum DataAction { Drop, Restore(u8 /*byte a escrever*/), Discard }`; `pub fn decide_data(e: &DataEntry,
  current: Option<u8>, guard_ok: bool, writable: bool) -> DataAction`. As regras estão na spec 6.6. Ao restaurar, só
  os bits da máscara voltam ao original. Bits da máscara que não são nem os nossos nem os originais → `Discard`.
- `pub type DataPolicyFn = Box<dyn FnMut(&[MapRegion]) -> WritePolicy>`; `Engine::set_data_policy(&mut self, p:
  DataPolicyFn)`. Sem política, as escritas em dados devolvem `Error::NoDataPolicy`. A política fica em cache:
  `Engine::refresh_data_maps(&mut self)` relê o `maps`. O motor também relê sozinho depois de uma escrita em dados que
  falhou.
- `Engine::write_data(&mut self, addr: u64, bytes: &[u8], guard: &[GuardBytes]) -> Result<()>`:
  - no máximo 8 bytes;
  - começa por `retry_resume`, como toda operação do motor (com `stuck` persistente, devolve o erro dele);
  - identidade do processo;
  - política (`DataOutsidePolicy`);
  - guarda relida imediatamente antes (`GuardMismatch`);
  - escreve e relê. Não grava diário.
- `Engine::set_bits(&mut self, id: &str, addr: u64, mask: u8, guard: &[GuardBytes]) -> Result<()>`:
  - entrada `(id, addr)` existente: relê e, se algum bit da máscara voltou a ligar, limpa de novo (manutenção);
  - senão: confere a guarda, lê o byte, grava a entrada no diário **antes** e escreve `byte & !mask`, recalculado de
    uma leitura imediatamente anterior;
  - recusa se o `id` tem entrada de dados pendente.
- `Engine::restore_bits(&mut self, id: &str) -> Result<BatchReport>`: aplica `decide_data` a todas as entradas do `id`
  (`restored`/`foreign`, com `Discard` em `foreign`).
- `undo_all` e `Drop` restauram também as entradas de dados. Falha suja deixa a entrada em `pending`, e
  `pending_ids`/`restore_pending` as incluem.
- `recover`: entradas de dados por `decide_data`. `writable` é região `rw` do `maps` atual, e a guarda é relida.
  Nunca passam pela política executável. Os relatórios usam os mesmos vetores por `id`.
- `gt-patch-harness set-bits --id ID --addr A --mask M --guard A:HEX [--guard …]`: abre o motor com política de dados
  "qualquer região `rw`", faz `set_bits` e fica no laço do `run`, que desfaz na saída por sinal. Mais o gancho global
  `--hook panic-after-bits`.

- [ ] **Passo 1: testes que falham:**
  - `journal.rs`: `decide_data_cases` (todas as combinações da regra); `journal_without_data_still_loads` (JSON sem
    `data`, e salvar sem `data` não escreve a chave); `data_entries_other_process_discarded` (Foco 5: `decide`
    devolve `Discard`, e nenhuma leitura é pedida).
  - `engine_live.rs` (palavra em `big_region` como "objeto" e guarda em bytes vizinhos):
    - `write_data_writes_and_rereads`;
    - `write_data_outside_policy_refused` (endereço na `exec_page`);
    - `write_data_guard_mismatch_skips` (Foco 1);
    - `set_bits_journals_before_write` (o gancho `InPause` não serve, porque não há pausa: conferir o diário logo
      depois pela API do store, e que o `original` é o byte anterior);
    - `set_bits_maintenance_reclears`;
    - `restore_keeps_foreign_bits` (Foco 4: o teste liga outro bit do byte, restaura e confere que ele continua);
    - `restore_guard_mismatch_discards`;
    - `undo_all_restores_bits`;
    - `set_bits_refused_with_pending`.
  - `harness.rs`:
    - `set_bits_sigkill_then_recover` (o `restore` devolve o bit);
    - `set_bits_sigkill_object_gone_discards` (o teste muda a guarda antes do `restore` → descarta sem escrever);
    - `set_bits_sigterm_restores`;
    - `set_bits_panic_restores` (gancho novo `panic-after-bits` do harness, que entra em pânico logo depois do
      `set_bits`; o `Drop` do motor restaura).
- [ ] **Passo 2:** falham. **Passo 3:** implementar. **Passo 4:** passam. Os testes existentes do motor e da
  recuperação continuam verdes.
- [ ] **Passo 5:** commit `feat(core): escrita em dados com bits no diário e recuperação`.

---

### Tarefa 13: Execução dos cheats de valor (`FieldRuntime`)

Spec 6.4, 6.5.1, 6.5.2 (troca de objeto, uma entrada viva), 6.6 (estados do motor).

**Arquivos:** criar `crates/gt-grounded2/src/fields.rs` (feature `write`); modificar `patching.rs` (política de dados:
regiões `rw` fora das imagens de módulo do `maps`); testes em `crates/gt-testkit/tests/fields_live.rs`.

**Interfaces:**
- Consome: `plan_tick`, `read_player`, `FieldCatalog` (Tarefa 8), `Engine::{write_data, set_bits, restore_bits, refresh_data_maps}` (Tarefa 12).
- Produz:
  - `pub struct FieldRuntime { … }`, criado por `FieldRuntime::new(catalog: FieldCatalog)`.
  - `tick(&mut self, engine: &mut Engine, mem, objects, controller: u64, on: &[String]) -> TickReport`.
  - `pub struct TickReport { pub player: PlayerState, pub notes: HashMap<String, Note>, pub events: Vec<String>, pub pawn_null_since: Option<Instant> }`.
  - `pub enum PlayerState { Found, Absent(Absent) }`; `pub enum Note { Waiting, InvalidTarget }`.
  - `turn_off(&mut self, engine, id) -> Result<BatchReport>`: travas só param; bits, `restore_bits`.
  - Regras:
    - objeto com endereço novo → `refresh_data_maps` antes de escrever;
    - antes de criar a entrada de bits do objeto novo, `restore_bits` do `id` (resolve a antiga);
    - `id` em `pending_ids` → pulado sem nota nova;
    - `InvalidTarget` vira `Note::InvalidTarget` e um evento `<nome>: alvo inválido` só na transição.

- [ ] **Passo 1: testes** (`fields_live.rs`, alvo `ue: true`, motor com diário em `TempDir`):
  - `locks_raise_values_in_target`;
  - `life_lower_to_zero`;
  - `bits_cleared_and_restored_on_turn_off`;
  - `creature_never_written` (a criatura com os mesmos componentes continua com os valores originais depois de vários
    ticks);
  - `pawn_swap_resolves_old_entry_first` (o antigo ainda vivo: bits restaurados antes da entrada nova; nunca duas
    entradas do mesmo `id` e byte);
  - `pawn_destroyed_entry_discarded`;
  - `invalid_target_note_and_resume`;
  - `pending_id_skipped`;
  - `region_not_rw_refused` (componente apontando para a `exec_page` → `Absent`/sem escrita).
- [ ] **Passo 2:** falham. **Passo 3:** implementar. **Passo 4:** passam.
- [ ] **Passo 5:** commit `feat(grounded2): execução dos cheats de valor`.

---

### Tarefa 14: TUI — modelo e tela

Spec 6.7 (marcas, linha `personagem:`, `verificando…`).

**Arquivos:** modificar `apps/gt-tui/src/model.rs` e `ui.rs`; testes em `mod tests`.

**Interfaces (produz):**
- `Avail::Checking`: o `status` dá `CheatStatus::Unavailable("verificando…")`, e a tela mostra só `verificando…`, sem
  `indisponível (…)`.
- `CheatRow { …, pub field: bool, pub note: Option<Note> }` (o `Note` vem do `gt-grounded2`). Com `on` e `note`, a
  linha mostra `[ON ]` seguido de `aguardando o personagem` ou `alvo inválido`.
- `Model { …, pub player: PlayerLine }`, com `pub enum PlayerLine { Idle, Found, NotFound }` →
  `personagem: —`/`encontrado`/`não encontrado`, numa linha do topo abaixo da conexão.

- [ ] **Passo 1: testes:** `checking_shows_verificando`, `on_with_waiting_note`, `on_with_invalid_target_note`,
  `player_line_labels`, `render_includes_player_line` (no buffer do `ratatui`, como os testes de tela existentes).
- [ ] **Passo 2:** falham. **Passo 3:** implementar. **Passo 4:** passam.
- [ ] **Passo 5:** commit `feat(tui): estados e linha do personagem para os cheats de valor`.

---

### Tarefa 15: TUI — interface de alvo, tarefa de reflexão e implementações falsa e do jogo

Spec 6.3 (tarefa de fundo), 6.7.

**Arquivos:** modificar `apps/gt-tui/src/backend.rs`, `fake.rs` e `game.rs`.

**Interfaces (produz):**
- `pub enum JobKind { Index, Controller }`;
  `pub struct ReflectionJob { pub kind: JobKind, pub run: Box<dyn FnOnce() -> ReflectionOutcome + Send> }`;
  `pub struct ReflectionOutcome { pub kind: JobKind, pub took: Duration, pub data: Box<dyn Any + Send> }`.
- `Session<P>` ganha:
  - `fn reflection_job(&self, kind: JobKind) -> Option<ReflectionJob>`: `None` quando não há o que fazer, por exemplo
    globais indisponíveis;
  - `fn merge_reflection(&mut self, out: ReflectionOutcome) -> ReflectionNews`;
  - `fn field_avail(&self) -> Vec<(String, Avail)>`: `Checking` até o primeiro índice bom;
  - `fn needs_controller(&self, now: Instant) -> bool`;
  - `fn field_on(&mut self, id: &str) -> OpOutcome`: trava aceita sempre, bit escreve já se o personagem está
    presente;
  - `fn field_off(&mut self, id: &str) -> OpOutcome`;
  - `fn field_tick(&mut self, on: &[String], now: Instant) -> TickReport`.
- `pub struct ReflectionNews { pub index_ok: bool, pub retry: bool /*motivo da reflexão que pede nova tentativa*/, pub player_changed: Option<PlayerState> }`.
- `CheatInfo` ganha `field: bool`. Os `[[field]]` entram em `Search.cheats`, depois dos patches da mesma categoria,
  com `Avail::Checking`, ou `Unavailable` com o motivo das globais, da conferência barata ou `disco ilegível` (com o
  disco ilegível, como os patches, spec 6.7).
- `GameSession`: o `ReflectionJob` captura `pid`, `MemReader`, `UeScan` e o catálogo, e roda
  `ReflectionIndex::build` + `FieldCatalog::resolve` ou `find_controller` com `ScanBudget::standard()`. O
  `FieldRuntime` fica na sessão. `field_tick` sem motor, com `stuck` ou com o diário bloqueado devolve um relatório
  vazio (manutenção suspensa).
- `FakeSession`: jobs e resultados programáveis, para os testes do controlador.

- [ ] **Passo 1: testes** (`game.rs`, `mod tests`, funções puras): `field_cheats_after_patches_in_category`,
  `globals_unavailable_reason_on_fields`, `cheap_check_failure_reason`, `disk_unreadable_makes_fields_unavailable`.
- [ ] **Passo 2:** falham. **Passo 3:** implementar. **Passo 4:** passam.
- [ ] **Passo 5:** commit `feat(tui): sessão com tarefa de reflexão e cheats de valor`.

---

### Tarefa 16: TUI — controlador

Spec 6.3 (agendamento), 6.7 inteira.

**Arquivos:** modificar `apps/gt-tui/src/controller.rs`; testes em `mod tests` com o `FakeBackend`.

**Interfaces:**
- Consome: Tarefas 14–15.
- Produz o comportamento:
  - **Tarefa de reflexão:**
    - uma thread por job, só um de cada vez;
    - resultado por canal, com o número da conexão;
    - `Index` logo depois de abrir a sessão, repetido em `max(10 s, 2 × took)` enquanto `retry`, por até 120 s depois
      da conexão (o `r` reinicia);
    - `Controller` só com algum `[[field]]` desejado e `needs_controller`, no mesmo intervalo;
    - na saída, a thread é abandonada.
  - **Teclas:**
    - ligar um `[[field]]` com `Avail::Checking` é recusado com o texto da seção de textos exatos;
    - ligar/desligar chamam `field_on`/`field_off` e contam como operação;
    - `write_block` vale também para eles (diário não resolvido, sem motor, `stuck`).
  - **Manutenção:** `field_tick` a cada `tick`, com os ids desejados de `[[field]]`. Não marca operação, não passa por
    `after_op_keys` e não invalida buscas. Fica suspensa com `stuck`, sem motor ou com o diário não resolvido. Atualiza
    `note`, `PlayerLine` e os eventos `personagem encontrado`/`personagem perdido`, só na transição.
  - **`keeps_state` e `wants_auto_search`:** passam a aceitar também `reflexão não reconhecida` (constante nova).
    `wants_auto_search` dispara com esse motivo ou com uma global `não encontrado`/`inconclusivo` mesmo sem cheat
    desejado. A nova busca usa o id `"@ue"` (Tarefa 7).
  - **Religar depois de reconectar:** mesma regra dos patches (mesma chave de versão), por `field_on`.
  - **Saída:** o `undo_all` da sessão já restaura os bits. Nada novo além de parar a manutenção.

- [ ] **Passo 1: testes:**
  - `stale_reflection_outcome_dropped` (Foco 2);
  - `index_retry_interval_and_window` (repete com `max(10 s, 2×took)`, para em 120 s, e o `r` reinicia);
  - `good_index_not_replaced_by_worse`;
  - `controller_job_only_with_field_desired`;
  - `field_on_while_checking_refused`;
  - `maintenance_not_an_operation` (teclas depois de um `tick` com `field_tick` não são filtradas);
  - `maintenance_suspended_when_stuck_or_no_engine`;
  - `player_events_only_on_transition`;
  - `reflexao_nao_reconhecida_keeps_state_and_auto_searches`;
  - `field_relinked_after_reconnect_same_version`;
  - `field_on_absent_player_waits` e `field_off_absent_player_succeeds`.
- [ ] **Passo 2:** falham. **Passo 3:** implementar. **Passo 4:** passam.
- [ ] **Passo 5:** commit `feat(tui): agendamento da reflexão e manutenção dos cheats de valor`.

---

### Tarefa 17: TUI — implementação crua com reflexão e testes contra o alvo

Spec 8 (TUI com o alvo; saída restaurando bits).

**Arquivos:** modificar `apps/gt-tui/src/raw.rs` (`RawBackend::new` aceita `Option<RawUe { ue: UeAddrs, sigs: UeSigs, fields: Vec<FieldDef> }>`)
e `apps/gt-tui/tests/core_raw.rs`.

- [ ] **Passo 1: testes** (`core_raw.rs`, alvo `ue: true`):
  - `raw_fields_checking_then_available`;
  - `raw_lock_holds_value`;
  - `raw_bits_restored_on_shutdown`;
  - `raw_absent_then_found_resumes` (o teste anula o `Pawn` no alvo e depois devolve).
- [ ] **Passo 2:** falham. **Passo 3:** implementar (mesma lógica do `GameSession`, com as globais vindas do
  `UeAddrs`, sem busca de padrões). **Passo 4:** passam.
- [ ] **Passo 5:** commit `test(tui): cheats de valor contra o alvo de teste`.

---

### Tarefa 18: Documentos

Spec 10.

- [ ] **Passo 1:** atualizar os documentos:
  - `ARCHITECTURE.md`:
    - 3: módulos do `gt-ue`, `player.rs`, `fields.rs`, `ue.rs`, `cmd/ue.rs`, testes novos;
    - 5.3: `[ue]` e `[[field]]`, unicidade das globais;
    - 5.4: escrita em dados, sem pausa, `DataEntry`, recuperação;
    - 5.6: layout no `[ue]`, manutenção no laço principal, tarefa de reflexão;
    - registro de decisões: as linhas da spec 11.
  - `AGENTS.md`: 3 (as duas ressalvas das travas) e 4 (exceção de unicidade para globais lidas).
  - Specs 002 e 003: a nota da spec 10.
  - `CLAUDE.md`: `gt ue class` e `gt ue player` nos comandos frequentes.
  - `ROADMAP.md`: Fase 5 dividida em spec 005 e spec 006.
- [ ] **Passo 2:** validação global.
- [ ] **Passo 3:** commit `docs: fase 5 (sobrevivência por reflexão)`.

---

### Tarefa 19: Sessão com o jogo e critério de pronto (sessão principal, com o dono)

Spec 9.

- [ ] **Passo 1:** pedir ao dono o **backup do save** e fechar o FLiNG. Rodar `cargo build --workspace`.
- [ ] **Passo 2:** testes `#[ignore]` novos em `apps/gt-patch/tests/game.rs` ou `apps/gt-tui/tests/game.rs`:
  - `game_globals_layout_and_fields_available` (os oito disponíveis, `gt ue player` coerente);
  - `game_bits_round_trip` (liga e desliga `sem-dano-queda` e `nao-morrer`; o byte volta ao original).

  Rodar com `cargo test --workspace -- --ignored` (a varredura demora; anotar os tempos).
- [ ] **Passo 3:** critério de pronto 1–11 da spec 9 com o dono, pela TUI, com `gt ue player --watch 2 --gobjects …
  --names …` num segundo terminal. Anotar o resultado de "Não morrer" e de "Sem dano de queda". Se algum cheat sair,
  remover a definição, os testes e a linha no fechamento (spec 6.1, P9).
- [ ] **Passo 4:** registrar a sessão no `RESEARCH.md` e o status no `ROADMAP.md` (spec 005 fechada; pendência:
  confirmar na próxima atualização do jogo).
- [ ] **Passo 5:** commit `docs: fechamento da spec 005` (se autorizado).
