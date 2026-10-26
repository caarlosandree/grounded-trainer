# Plano 004 — Primeiros cheats (patch de código simples)

> **Para agentes executores:** SUB-SKILL OBRIGATÓRIA: use superpowers:subagent-driven-development (recomendado) ou superpowers:executing-plans para executar este plano tarefa a tarefa. Os passos usam checkbox (`- [ ]`).

**Objetivo:** três cheats reais na categoria Itens (durabilidade infinita, energia infinita, recarga instantânea), só
com assinaturas novas para o motor existente, mais os testes sem e com o jogo.

**Arquitetura:** nenhum código de produção muda. Os cheats são blocos `[[patch]]` no `generic.toml` e entradas com
`rva` no `c935e799-0b27c000.toml`; o carregador, o motor e a TUI já os tratam. Os testes novos ficam nos testes que já
carregam as assinaturas embutidas (`gt-grounded2`) e nos testes `#[ignore]` do `gt-patch`.

**Tecnologias:** Rust stable (edition 2024), `toml`, `iced-x86` (indireto, via `gt_core::patch::validate`).

**Spec:** `docs/specs/004-cheats-patch-simples.md` (citada como "spec 6.x"). Quem executa lê a spec e este plano.

## Restrições globais

- Regras do `AGENTS.md`: identificadores em inglês; comentários, docs e mensagens em pt-BR; nada de `unwrap()` fora
  de testes; nenhum endereço fixo do jogo em código de produção (os `rva` ficam só no TOML e nos testes).
- O `cargo` está em `~/.cargo/bin` (`export PATH=$HOME/.cargo/bin:$PATH` se preciso).
- Validação de toda tarefa: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`.
- Valores exatos (spec 6.1–6.3), na ordem do arquivo:

  | `id` | `name` | `pattern` | `offset` | `expected` | `replacement` | `rva` |
  |---|---|---|---|---|---|---|
  | `durabilidade-infinita` | `Durabilidade infinita` | `f3 0f 10 8b 98 02 00 00 0f 28 d1 f3 41 0f 5c d0 f3 0f 5d d0 f3 0f 5f d6 0f 2e d1 74` | `11` | `f3 41 0f 5c d0` | `0f 1f 44 00 00` | `0x5a4e777` |
  | `energia-infinita` | `Energia infinita` | `f3 0f 10 81 50 02 00 00 0f 28 c8 f3 0f 58 cb f3 0f 5d ca 0f 2e c8 74` | `11` | `f3 0f 58 cb` | `0f 1f 40 00` | `0x5a5744b` |
  | `recarga-instantanea` | `Recarga instantânea` | `f3 0f 10 bb 90 0b 00 00 0f 57 c9 0f 2f f9 76 ?? 0f 28 c7 f3 0f 5c c6 0f 2f c1 f3 0f 11 83 90 0b 00 00` | `16` | `0f 28 c7` | `0f 57 c0` | `0x5c3bbfc` |

  Todos com `category = "itens"` e sem bloco `value`.
- Nenhuma escrita no jogo fora dos testes `#[ignore]` e do teste manual com o dono. O FLiNG fica fechado.

## Foco da revisão

1. **Padrão que não é único na memória** (o FLiNG ou uma atualização mexeu no código): o cheat precisa aparecer como
   `indisponível (ambíguo|não encontrado)`, nunca aplicado. Já garantido pelo motor; a Tarefa 1 confere antes de
   escrever, e a Tarefa 3 confere `disponível (verificado)` no endereço exato.
2. **Versão sem arquivo de assinaturas:** os três vêm do genérico como "não verificado" (`?`) e só ligam se o padrão
   for único na memória e no disco. Coberto pelo teste existente `real_files_load` (genérico com nome) e pelo teste
   novo da Tarefa 2, que confere o genérico.
3. **Troca que muda fronteira de instrução** (erro de digitação nos bytes): tornaria o patch "perigoso" na pausa. A
   Tarefa 2 valida cada par `expected`/`replacement` com `gt_core::patch::validate`.
4. **Sobreposição entre os três patches** ou com o patch de teste: o motor recusaria o lote. Os endereços estão
   longe uns dos outros; a Tarefa 2 confere que os intervalos `[rva, rva + len)` não se sobrepõem.
5. **Desligar volta exatamente ao original:** a Tarefa 3 confere com `gt diff` sem diferença no endereço de cada
   patch depois de um ciclo.

---

### Tarefa 1: conferência só de leitura no jogo (portão)

Exige o jogo aberto num save, com o FLiNG fechado. **Nenhuma escrita.**

- [ ] **Passo 1: conferir a build**

Run: `cargo run -q -p gt-cli -- inspect`
Esperado: `Chave de versão: c935e799-0b27c000`. Outra chave: pare e avise o dono (os `rva` não valem).

- [ ] **Passo 2: unicidade de cada padrão na memória e no disco**

Run (para cada `pattern` da tabela): `cargo run -q -p gt-cli -- scan "<pattern>"` e o mesmo com `--disk`
Esperado: `Total: 1 (único)` nos dois, no endereço `base + rva - offset` (`+5a4e76c`, `+5a57440`, `+5c3bbec`).

- [ ] **Passo 3: decidir**

Se algum padrão não for único: **pare**. Alongue o padrão com mais bytes do código ao redor (com `gt disasm`),
atualize a spec 6.x e a tabela deste plano, e só então siga. Se todos forem únicos, siga para a Tarefa 2.

---

### Tarefa 2: assinaturas e testes sem o jogo

**Arquivos:**
- Modificar: `crates/gt-grounded2/signatures/generic.toml` (3 blocos `[[patch]]` depois do patch de teste)
- Modificar: `crates/gt-grounded2/signatures/c935e799-0b27c000.toml` (3 blocos `[[patch]]` com `id` e `rva`)
- Teste: `crates/gt-grounded2/src/signatures.rs` (módulo `tests`)

**Interfaces:**
- Consome: `Signatures::embedded() -> Result<Signatures>`, `Signatures::for_version(&str) -> Vec<PatchDef>`
  (`PatchDef { id, name, category, description, pattern, offset, expected, replacement, rva: Option<u64>, value }`),
  `Category::Itens`, `gt_core::patch::{validate, PatchSpec}` e `Validated::{lost_on_apply, lost_on_undo}`.
- Produz: os ids `durabilidade-infinita`, `energia-infinita` e `recarga-instantanea`, usados pela Tarefa 3.

- [ ] **Passo 1: escrever os testes que falham**

Em `signatures.rs`, `mod tests`, uma constante com a tabela das restrições globais e três testes:

```rust
/// (id, name, rva, expected, replacement), na ordem do generic.toml.
const PHASE4: [(&str, &str, u64, &str, &str); 3] = [
    ("durabilidade-infinita", "Durabilidade infinita", 0x5a4e777, "f3 41 0f 5c d0", "0f 1f 44 00 00"),
    ("energia-infinita", "Energia infinita", 0x5a5744b, "f3 0f 58 cb", "0f 1f 40 00"),
    ("recarga-instantanea", "Recarga instantânea", 0x5c3bbfc, "0f 28 c7", "0f 57 c0"),
];

#[test]
fn phase4_cheats_load_verified() {
    // for_version("c935e799-0b27c000"): cada id existe, name igual, category == Category::Itens,
    // value == None, rva == Some(<rva>), expected/replacement iguais aos bytes da tabela,
    // description não vazia; os ids de Itens aparecem nesta ordem.
}

#[test]
fn phase4_cheats_generic_without_rva() {
    // for_version("cccc-dddd"): os três existem com rva == None e os mesmos bytes.
}

#[test]
fn phase4_cheats_keep_instruction_boundaries() {
    // Para cada um: validate(PatchSpec { id, addr: rva, expected, replacement }) é Ok, e
    // lost_on_apply() e lost_on_undo() são vazios. Os intervalos [rva, rva + len) dos três e do
    // teste-codificacao (0x112c468, 3 bytes) não se sobrepõem.
}
```

Use um ajudante local de teste para converter `"f3 41 …"` em `Vec<u8>` se não houver um público.

- [ ] **Passo 2: rodar e ver falhar**

Run: `cargo test -p gt-grounded2 phase4_`
Esperado: FAIL nos três (ids ausentes).

- [ ] **Passo 3: adicionar as assinaturas**

`generic.toml`: um bloco `[[patch]]` por linha da tabela, com `id`, `name`, `category = "itens"`, `description`
(uma linha em pt-BR com o efeito, spec 6.4), `pattern`, `offset`, `expected` e `replacement`, e um comentário acima
de cada bloco no estilo do patch de teste (o que a instrução faz, o campo do jogo e a referência à spec 004 e ao
`RESEARCH.md`). `c935e799-0b27c000.toml`: um `[[patch]]` por cheat com `id` e `rva`.

- [ ] **Passo 4: rodar e ver passar**

Run: `cargo test -p gt-grounded2`
Esperado: PASS, incluindo `real_files_load` e `embedded_files_load`.

- [ ] **Passo 5: validação completa**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Esperado: tudo verde.

- [ ] **Passo 6: commit**

```bash
git add crates/gt-grounded2/signatures/generic.toml crates/gt-grounded2/signatures/c935e799-0b27c000.toml crates/gt-grounded2/src/signatures.rs
git commit -m "feat(grounded2): durabilidade, energia e recarga como patches simples"
```

---

### Tarefa 3: teste com o jogo aberto (`#[ignore]`)

**Arquivos:**
- Modificar: `apps/gt-patch/tests/game.rs`

**Interfaces:**
- Consome: os três ids da Tarefa 2; os ajudantes já existentes do arquivo (`gt_patch`, `gt_json`, `known_build`,
  `parse_hex`).
- Produz: nada para outras tarefas.

- [ ] **Passo 1: extrair o ciclo do teste existente**

Transforme o corpo de `single_toggle_round_trip` em `fn round_trip(id: &str)` (acha a linha do `gt-patch list`
que começa com `id`, exige `disponível (verificado)` nela, calcula `base + offset`, roda `toggle <id> --times 1` e
confere que nenhum trecho do `gt diff` cobre o endereço). `single_toggle_round_trip` passa a chamar
`round_trip("teste-codificacao")`.

- [ ] **Passo 2: o teste novo**

```rust
#[test]
#[ignore = "exige o jogo aberto"]
fn phase4_cheats_round_trip() {
    // known_build() ou retorna; round_trip para os três ids, na ordem da tabela.
}
```

- [ ] **Passo 3: compilar sem o jogo**

Run: `cargo test -p gt-patch --test game`
Esperado: compila; os testes aparecem como `ignored`.

- [ ] **Passo 4: rodar com o jogo aberto** (jogo num save, FLiNG fechado, backup do save feito pelo dono)

Run: `cargo test -p gt-patch --test game -- --ignored`
Esperado: PASS em todos, inclusive `phase4_cheats_round_trip`; `cargo run -q -p gt-cli -- diff` no fim mostra
`nenhuma alteração`.

- [ ] **Passo 5: validação completa e commit**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

```bash
git add apps/gt-patch/tests/game.rs
git commit -m "test(gt-patch): ciclo liga/desliga dos cheats da fase 4 no jogo"
```

---

### Tarefa 4: teste manual com o dono e documentos

Segue a spec, seção 8, com o dono no jogo e o `gt-tui` aberto (`cargo run -p gt-tui`). Pedir uma ação por vez, curta.

- [ ] **Passo 1:** pedir backup do save e confirmar que o FLiNG está fechado.
- [ ] **Passo 2:** os três aparecem em ITENS, `[OFF]`, sem `?`.
- [ ] **Passo 3:** durabilidade: ligar, usar ferramenta ou arma; não cai. Desligar; volta a cair. Perguntar se
  o reparo funciona com o cheat ligado.
- [ ] **Passo 4:** energia: tocha acesa com o cheat ligado; não diminui. Desligar; volta a diminuir.
- [ ] **Passo 5:** recarga: usar um item com recarga e tentar de novo logo em seguida, com e sem o cheat. Anotar
  quais itens foram testados.
- [ ] **Passo 6:** com os três ligados, o dono fecha e reabre o jogo; depois de reconectar eles voltam ligados e
  funcionando.
- [ ] **Passo 7:** sair da TUI (`q`); `cargo run -q -p gt-cli -- diff` mostra `nenhuma alteração`. O dono confirma
  que o jogo ficou estável.
- [ ] **Passo 8: documentos**
  - `docs/RESEARCH.md`: seção da sessão com versão, data, resultado de cada cheat e efeitos colaterais observados
    (reparo, itens da recarga).
  - `docs/ROADMAP.md`: Fase 4 com `**Status:** ✅ concluída em <data>` (spec e plano `004-cheats-patch-simples`).
  - Spec 004: estado "Aprovada; critério de pronto confirmado no jogo pelo dono em <data>".
- [ ] **Passo 9: commit**

```bash
git add docs/RESEARCH.md docs/ROADMAP.md docs/specs/004-cheats-patch-simples.md
git commit -m "docs: fechamento da fase 4"
```
