# Plano 002 — Motor de patches seguro

> **Para agentes executores:** SUB-SKILL OBRIGATÓRIA: use superpowers:subagent-driven-development (recomendado) ou superpowers:executing-plans para executar este plano tarefa a tarefa. Os passos usam checkbox (`- [ ]`).

**Objetivo:** motor de patches reversível no `gt-core` (validação, pausa com conferência das threads, registro, diário, recuperação, laço com sinais), lado do jogo no `gt-grounded2` (assinaturas TOML, estado, política, catálogo) e o binário `gt-patch` (`list`, `run`, `toggle`, `restore`, `profile`), mais as pendências da Fase 1.

**Arquitetura:** tudo que é decisão (validação, regra de perigo, decisão de recuperação, estado de patch, política de escrita, parse de `/proc`) é função pura testada sem processo. O que pausa ou escreve fica atrás da feature `write` (em `gt-core` e, em cascata, em `gt-grounded2`). O `gt-patch` só junta argumentos, assinaturas embutidas e saída; os comportamentos de sinal, pânico e recuperação são testados com um binário de apoio (`gt-patch-harness`) que usa a mesma biblioteca contra o processo-alvo de teste. O alvo de teste, o binário de apoio e os testes com processo vivo vão para um crate novo, `gt-testkit`.

**Tecnologias:** Rust 1.99 stable (edition 2024), `nix` (sinais, `fcntl` OFD, `renameat2`, `poll`, `sysconf`), `libc` (`pidfd_open`/`pidfd_send_signal`, só em `gt-core/src/mem/`), `iced-x86`, `goblin`, `serde`/`serde_json`/`toml`, `signal-hook`, `clap`, `thiserror`/`anyhow`, `tracing`.

**Spec:** `docs/specs/002-motor-patches.md` (citada como "spec 6.x"). Quem executa lê a spec e este plano.

## Restrições globais

- Regras do `AGENTS.md`: identificadores em inglês; comentários, docs e mensagens em pt-BR; `unsafe` só em `crates/gt-core/src/mem/` com `// SAFETY:`; `thiserror` nas libs e `anyhow` nos binários; sem `unwrap()`/`expect()` fora de testes; `tracing`, nunca `println!` em libs; saída de binário por `writeln!` em `stdout().lock()`.
- O `cargo` está em `~/.cargo/bin` (`export PATH=$HOME/.cargo/bin:$PATH` se preciso).
- Validação de toda tarefa: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` **e** `cargo build -p gt-cli` isolado e `cargo clippy -p gt-cli -- -D warnings` isolado (o `gt` não compila o escritor nem o `test-support`, e o código sem `write` não tem aviso de código morto). Conferir também `cargo tree -p gt-cli -e normal,build,features -i gt-core` (sem dev-dependencies): não pode aparecer `write` nem `test-support` (a partir da Tarefa 1).
- Os testes de processo vivo encontram os binários do `gt-testkit` por `gt_testkit::bin_path(nome)` (diretório do executável de teste, `target/<perfil>/`). Funcionam no `cargo test --workspace`; num `cargo test -p <outro>` isolado, falham com a mensagem "rode `cargo build -p gt-testkit --bins` antes".
- Nenhum endereço fixo do jogo no código de produção; só em `signatures/*.toml`, documentos e testes `#[ignore]`.
- `panic = "unwind"` (padrão): nenhum perfil do workspace pode usar `abort`; a restauração em pânico depende do `Drop`.
- Nenhum `Drop` do motor pode entrar em pânico (só registra erro com `tracing`).
- O `gt-patch` e o `gt-patch-harness` são de uma thread só (fora a do `signal-hook`, se houver): pânico em "qualquer thread" (spec 6.7) é pânico na thread principal, coberto pelo `Drop` no desenrolar.
- Caminhos públicos novos do `gt-core`: `gt_core::{procinfo, patch, journal, memscan}` sem feature; `gt_core::{engine, recovery, session}` e `gt_core::mem::{Pidfd, ignore_job_control}` atrás de `#[cfg(feature = "write")]`; o módulo `gt_core::suspend` existe sempre, com as funções puras sem gate e os itens que pausam (`pause`, `PauseGuard`, `sample_pcs`, `is_stopped_now`, `open_checked`) marcados com `#[cfg(feature = "write")]`.
- Saída de texto do `gt-patch`/harness depois de um sinal (terminal possivelmente fechado): falha de `writeln!` nunca interrompe a restauração (`let _ =` nas mensagens finais).
- Mensagens exatas (testes procuram estes trechos): estados da spec 6.4 (`disponível (verificado)`, `disponível (não verificado)`, `ativo`, `pendente`, `bytes diferentes`, `fora do lugar`, `não encontrado`, `ambíguo`, `inconclusivo`, `disco ilegível`); `AlreadyRunning` → "outro gt-patch já está em execução"; `ProcessGone` no `gt-patch` → "o jogo fechou"; restore sem diário → "nada a restaurar"; jogo ausente → "jogo não encontrado; abra o Grounded 2 e tente de novo".
- Valores fixados pelo plano (spec 12): prazo de pausa 500 ms; até 10 tentativas por lote (perigo e prazo somados); espera de 20 ms entre tentativas; intervalo padrão do `toggle` 50 ms; laço do `run` acorda a cada 200 ms; `profile` padrão 200 amostras a cada 300 ms (60 s).
- Diário em `$XDG_STATE_HOME/grounded-trainer/` (padrão `~/.local/state/grounded-trainer/`): `journal.json`, `journal.lock`, `journal.json.bad`. Os testes apontam `XDG_STATE_HOME` para um diretório temporário.
- Git: trabalho no branch `fase-2-motor-patches`. O dono autorizou a execução por subagent-driven-development em 2026-10-25, o que inclui um commit ao fim de cada tarefa (como na Fase 1); push, merge e rebase continuam exigindo pedido explícito. Os subagentes executores **devem** fazer o commit do fim da tarefa (esta autorização prevalece, para este plano, sobre a regra geral do `AGENTS.md` 8). Commit ao fim de cada tarefa, Conventional Commits em pt-BR, terminando com `Claude-Session: https://claude.ai/code/session_01Psp6AHcK8pD1eVRsZut1e4`. Push, merge e rebase só com pedido do dono.
- **Tarefas 8 (a partir do Passo 3) e 15 inteira rodam na sessão principal, não em subagente**: exigem conversar com o dono e, na 15, engenharia reversa (reservada ao modelo principal pelo `CLAUDE.md`). Os Passos 1–2 da Tarefa 8 podem ser delegados.
- Tarefas com o jogo (8 e 15): a IA pede ao dono, em frases curtas, para abrir o jogo/FLiNG e espera a confirmação. Antes da sessão da Tarefa 15, pede **backup do save**. Nunca `sudo` nem mudar `ptrace_scope`.

## Foco de revisão

Entradas e falhas que a spec implica e que nenhum outro teste cobre, por risco. Cada uma tem teste na tarefa dona:

1. **O jogo fecha no meio da pausa** (entre o SIGSTOP e a escrita). Esperado: lote abortado como "processo terminou", nenhuma escrita, diário limpo, sem pânico. Teste `apply_when_target_dies_during_pause` (Tarefa 11, gancho que mata o alvo dentro da pausa).
2. **Diretório do diário sem permissão de escrita.** Esperado: recusa **antes** de pausar; alvo não pausado e bytes intactos. Teste `apply_refuses_when_journal_unwritable` (Tarefa 11).
3. **SIGINT chegando durante a pausa.** Esperado: o lote termina, SIGCONT, depois desfaz e sai com 0. Teste `sigint_during_pause_restores` (Tarefa 12, gancho que levanta SIGINT dentro da pausa).
4. **Patch que cruza fronteira de página.** Esperado: aplica e desfaz normalmente; recuperação byte a byte funciona. Teste `patch_across_page_boundary` (Tarefa 11).
5. **`run` com vários `id`, um deles indisponível ou desconhecido.** Esperado: recusa tudo antes de qualquer escrita, citando o `id` e o estado. Teste `select_refuses_if_any_unavailable` (Tarefa 13).

---

## Mapa de arquivos

```
Cargo.toml                         nix features (+ signal, fs, poll, feature); gt-testkit no [workspace.dependencies]
crates/gt-core/
  Cargo.toml                       deps novas: serde, serde_json, tracing; write = ["dep:libc"]
  src/error.rs                     variantes novas (Tarefas 6, 9, 10, 11)
  src/memscan.rs                   NOVO: busca em memória com lacunas e veredito (saiu do gt-cli)
  src/scan.rs                      scan_chunked_many
  src/pe.rs                        read_file_headers, DiskImage::functions (.pdata)
  src/modules.rs                   discover com extensão min(memória, disco)
  src/procinfo.rs                  NOVO: stat, syscall, tids, boot_id
  src/patch.rs                     NOVO: PatchSpec, validação, fronteiras, regra de perigo, WritePolicy
  src/journal.rs                   NOVO: modelo, store, trava OFD, decisão de recuperação
  src/suspend.rs                   NOVO (write): pausa com conferência, PauseGuard, amostragem
  src/engine.rs                    NOVO (write): aplicar/desfazer com diário e identidade
  src/recovery.rs                  NOVO (write): executa a recuperação do diário
  src/session.rs                   NOVO (write): sinais, laço do run, toggle
  src/mem/mod.rs                   page_size() dinâmico
  src/mem/writer.rs                WriteZero → ProcessGone
  src/mem/sys.rs                   NOVO (write): Pidfd, ignore_job_control
  src/mem/test_support.rs          páginas de código, threads, pipe, região grande
crates/gt-testkit/                 NOVO (publish = false)
  src/lib.rs                       TestTarget, bin_path, helpers
  src/bin/gt-testtarget.rs         movido do gt-cli e ampliado
  src/bin/gt-patch-harness.rs      binário de apoio da Tarefa 12
  tests/core_live.rs               movido do gt-cli
  tests/engine_live.rs             Tarefas 7 e 11
  tests/harness.rs                 Tarefa 12
crates/gt-grounded2/
  Cargo.toml                       feature write = ["gt-core/write"]; deps serde, toml, anyhow não
  signatures/generic.toml          definições de patch
  signatures/c935e799-0b27c000.toml  rva verificado
  src/error.rs                     NOVO: Error do crate
  src/resolve.rs                   NOVO: Target (saiu do gt-cli/context.rs)
  src/signatures.rs                NOVO: parse, regra de carga, embutidos
  src/state.rs                     NOVO: PatchState e compute_state
  src/policy.rs                    NOVO: política de escrita e área de busca do jogo
  src/catalog.rs                   NOVO: varredura única + estado por patch
  src/profile.rs                   NOVO: agregação de pc por função
  src/patching.rs                  NOVO (write): abrir o motor para o jogo
  tests/game_pause.rs              NOVO: portão da pausa (#[ignore])
apps/gt-cli/                       usa memscan e resolve; sem test-support; dump/compare/inspect
apps/gt-patch/                     NOVO: binário gt-patch
  src/main.rs, src/cmd/{list,run,toggle,restore,profile}.rs
  tests/cli.rs, tests/game.rs
```

---

### Tarefa 1: `gt-testkit`, página dinâmica e escritor

Itens 7.4 e 7.7 da spec e o `WriteZero` da 6.7.

**Arquivos:**
- Criar: `crates/gt-testkit/{Cargo.toml,src/lib.rs}`; mover `apps/gt-cli/src/bin/gt-testtarget.rs` → `crates/gt-testkit/src/bin/gt-testtarget.rs`, `apps/gt-cli/tests/support/mod.rs` → `crates/gt-testkit/src/lib.rs`, `apps/gt-cli/tests/core_live.rs` → `crates/gt-testkit/tests/core_live.rs`
- Modificar: `Cargo.toml` raiz (features `fs` e `feature` do `nix`), `apps/gt-cli/Cargo.toml` (tira o `[[bin]] gt-testtarget`, tira `test-support` das deps normais, tira `write` das dev-deps, dev-dep `gt-testkit`), `apps/gt-cli/tests/cli.rs` (usa `gt_testkit`), `crates/gt-core/src/mem/{mod.rs,writer.rs,test_support.rs}`, `docs/ARCHITECTURE.md` (árvore da seção 3)

**Interfaces:**
- Produz:
  - `gt_core::mem::page_size() -> u64` (`nix::unistd::sysconf(PAGE_SIZE)`, guardado em `OnceLock`; 4096 se falhar). `read_lossy` e `test_support` usam isto no lugar de `PAGE`.
  - `MemWriter::write` traduz `io::ErrorKind::WriteZero` (sem `raw_os_error`) para `Error::ProcessGone { pid }`.
  - `test_support`: `page_size()` reexportado; `anon_rw(len: usize) -> Result<u64>` (mapeamento `rw` zerado de `len` bytes).
  - `gt_testkit::bin_path(name: &str) -> PathBuf`: `current_exe()` → sobe até o diretório que contém `deps` → `join(name)`; se não existir, `panic!` com "binário {name} não encontrado; rode `cargo build --workspace` antes".
  - `gt_testkit::TestTarget` (mesmos campos de hoje + `big_region: u64`, `big_len: usize`, `page: u64`), `spawn()` usando `bin_path("gt-testtarget")`. `marker_bytes()` e `hex_with_spaces()` continuam.
  - Alvo de teste: imprime também `big_region` (2 MiB + 1 página, `anon_rw`), `big_len` e `page`.

- [ ] **Passo 1: Criar o branch, registrar spec e plano, criar o crate**

```bash
git switch -c fase-2-motor-patches
git add docs/specs/002-motor-patches.md docs/plans/002-motor-patches.md
git commit -m "docs: spec e plano da fase 2 (motor de patches)"   # + linha Claude-Session
```

`crates/gt-testkit/Cargo.toml`: `publish = false`, lints do workspace, deps `gt-core = { workspace = true, features = ["write", "test-support"] }` (os binários de `src/bin/` não enxergam dev-dependencies; o `gt-patch-harness` da Tarefa 12 precisa de `write`), `serde_json`, `nix` (com `signal`, para o harness da Tarefa 12, que faz parse manual dos argumentos, sem `clap`); dev-deps `tempfile`. Efeito aceito: `cargo test -p gt-cli` compila o `gt-core` com `write` pela cadeia do dev-dep; o `cargo build -p gt-cli` isolado não. Adicionar `gt-testkit = { path = "crates/gt-testkit" }` ao `[workspace.dependencies]`. Mover os três arquivos com `git mv`.

- [ ] **Passo 2: Testes que falham** (em `crates/gt-testkit/tests/core_live.rs`)

```rust
#[test]
fn writer_handles_more_than_one_mib() {
    let t = TestTarget::spawn();
    let data: Vec<u8> = (0..t.big_len).map(|i| (i % 251) as u8).collect();
    MemWriter::open(t.pid).unwrap().write(t.big_region, &data).unwrap();
    let mut back = vec![0u8; t.big_len];
    MemReader::new(t.pid).read_exact(t.big_region, &mut back).unwrap();
    assert_eq!(back, data);
}

#[test]
fn writer_on_prot_none_page() {
    // Kernel de referência (Fedora 44, kernel 7.2, proc_mem.force_override = always): a escrita
    // forçada em página PROT_NONE privada é aceita (verificado em 2026-10-25).
    let t = TestTarget::spawn();
    let result = MemWriter::open(t.pid).unwrap().write(t.region_start + t.page, &[1, 2, 3]);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn writer_to_dead_process_is_gone() // mata o alvo (kill + wait) depois de abrir o MemWriter; write → ProcessGone
```

Teste unitário em `mem/mod.rs`: `page_size_is_power_of_two_and_at_least_4096`.

- [ ] **Passo 3: Rodar e ver falhar**

Run: `cargo test --workspace` → falha (campos novos e `bin_path` inexistentes).

- [ ] **Passo 4: Implementar** os itens das Interfaces. No `gt-cli/tests/cli.rs` e no `core_live.rs` movido, trocar `mod support; use support::*` (e os `support::...`) por `gt_testkit`.

- [ ] **Passo 5: Validar**

Run: validação global. Esperado: tudo passa; `cargo tree -p gt-cli -e normal,build,features -i gt-core` não mostra `write` nem `test-support` (nem a feature `libc` do `gt-core`).

- [ ] **Passo 6: Commit** — `refactor(testes): crate gt-testkit, página dinâmica e testes do escritor`

---

### Tarefa 2: Busca em memória reutilizável

Item 7.5 da spec (parte da busca).

**Arquivos:**
- Criar: `crates/gt-core/src/memscan.rs`
- Modificar: `crates/gt-core/src/scan.rs`, `crates/gt-core/src/lib.rs`, `apps/gt-cli/src/cmd/scan.rs` (passa a usar o `gt-core`; os testes de `Verdict`/`merge_ranges` migram junto)

**Interfaces:**
- Produz:
  - `gt_core::scan::scan_chunked_many(total_len: u64, chunk: usize, patterns: &[Pattern], read: impl FnMut(u64, &mut [u8]) -> Result<()>) -> Result<Vec<Vec<u64>>>`: um resultado por padrão, mesma semântica de `scan_chunked` (sobreposição de `max_len - 1`). `scan_chunked` vira um invólucro de um padrão.
  - `gt_core::memscan::Verdict` (pública, mesmas variantes e métodos `decide`, `json`, `text` do `gt-cli`).
  - `gt_core::memscan::MemScan { pub hits: Vec<u64>, pub unreadable_bytes: u64, pub verdict: Verdict }`.
  - `gt_core::memscan::scan_memory(reader: &MemReader, regions: &[Range<u64>], patterns: &[Pattern]) -> Result<Vec<MemScan>>`: lê cada região em blocos de 1 MiB com a leitura tolerante (o `read_block` atual), descarta ocorrências que tocam lacunas, ordena e remove duplicatas. **Não** chama a guarda: quem chama confere antes.
  - `gt_core::memscan::scan_bytes(regions: &[(u64, &[u8])], patterns: &[Pattern]) -> Vec<Vec<u64>>` (busca no disco).
  - `gt_core::memscan::merge_ranges(Vec<Range<u64>>) -> Vec<Range<u64>>`.

- [ ] **Passo 1: Testes que falham**

Em `scan.rs`: `many_patterns_share_one_pass` (dois padrões de tamanhos 3 e 7 num buffer com ocorrências na fronteira de blocos de 16 bytes; resultado igual ao de `find_all` para cada um). Em `memscan.rs`: os testes de `Verdict` e `merge_ranges` movidos do `gt-cli`, e `scan_bytes_offsets_are_absolute` (região em `0x1000` com o padrão no byte 5 → `vec![vec![0x1005]]`). Em `crates/gt-testkit/tests/core_live.rs`: `scan_memory_finds_marker_once` (região do marcador, `verdict == Verdict::Unique`) e `scan_memory_skips_hole` (antes, o teste preenche as páginas 0 e 2 do `region_start` com `0xAA` via `MemWriter`; padrão `00 00 00 00` nas 3 páginas → nenhuma ocorrência, porque só a lacuna "tem" zeros; `unreadable_bytes == page`).

- [ ] **Passo 2: Rodar e ver falhar** — `cargo test --workspace` (símbolos inexistentes).
- [ ] **Passo 3: Implementar**; o `gt-cli` `scan` mantém a guarda por seção e a mesma saída (os testes de CLI existentes não mudam).
- [ ] **Passo 4: Validar** (validação global).
- [ ] **Passo 5: Commit** — `refactor(core): busca em memória e veredito no gt-core`

---

### Tarefa 3: Resolução do alvo no `gt-grounded2`

Seção 5 da spec e item 7.5 (`many_candidates_message`).

**Arquivos:**
- Criar: `crates/gt-grounded2/src/{error.rs,resolve.rs}`
- Modificar: `crates/gt-grounded2/{Cargo.toml,src/lib.rs}`, `apps/gt-cli/src/{context.rs,main.rs}` e os `cmd/*.rs` que usam `Target` (atenção a `cmd/fling.rs`, que faz `downcast_ref::<gt_core::Error>()` no erro de `Target::module`: passa a casar `gt_grounded2::Error::Core(e)`)

**Interfaces:**
- Produz:
  - `gt_grounded2::Error` (`thiserror`; a variante `Core` é `#[error(transparent)]`, para o `report()` não repetir a mensagem): `GameNotFound` ("jogo não encontrado; abra o Grounded 2 e tente de novo"), `ManyCandidates { pids: Vec<i32> }` (mensagem atual de `many_candidates_message`), `DiskOpen { path: PathBuf, source: gt_core::Error }` ("não foi possível abrir {path} no disco"), `Core(#[from] gt_core::Error)`. `pub type Result<T>`.
  - `gt_grounded2::resolve::Target` com os campos e métodos atuais do `gt-cli` (`open`, `module`, `resolve`, `label`, `module_label`, `open_disk`), devolvendo `gt_grounded2::Result`.
  - `gt-cli/src/context.rs` fica só com `pub use gt_grounded2::resolve::Target;` (ou some, com os `use` trocados). O `hint()` do `gt-cli` passa a fazer `downcast_ref` também de `gt_grounded2::Error::Core`.
- Feature: `gt-grounded2` ganha `[features] write = ["gt-core/write"]` (vazia de código por enquanto).

- [ ] **Passo 1: Testes que falham** — mover `many_candidates_lists_pids_and_mentions_pid_flag` para `resolve.rs` (testando `Error::ManyCandidates` via `to_string()`); `hint_finds_core_error_inside_grounded2_error` no `gt-cli/src/main.rs`.
- [ ] **Passo 2: Rodar e ver falhar.**
- [ ] **Passo 3: Implementar** (mover o código; `anyhow` sai do caminho, `with_context` vira `Error::DiskOpen`).
- [ ] **Passo 4: Validar** — os testes de CLI existentes passam sem mudança de texto.
- [ ] **Passo 5: Commit** — `refactor(grounded2): resolução do alvo sai do gt-cli`

---

### Tarefa 4: Extensão `min(memória, disco)` e `inspect`

Spec 6.12 e item 7.6.

**Arquivos:**
- Modificar: `crates/gt-core/src/{pe.rs,modules.rs}`, `crates/gt-core/Cargo.toml` (dep `tracing`), `crates/gt-grounded2/src/resolve.rs`, `apps/gt-cli/src/cmd/inspect.rs`, `docs/specs/001-nucleo-memoria-gt.md` (6.12, "Limitações conhecidas": apontar a decisão da spec 002 6.12), `docs/ARCHITECTURE.md` (tabela 4: `tracing` no `gt-core`)

**Interfaces:**
- Produz:
  - `gt_core::pe::read_file_headers(path: &Path) -> Result<PeHeaders>`: lê só os primeiros `0x1000` bytes (ou menos, se o arquivo for menor) e chama `PeHeaders::parse`.
  - `gt_core::modules::open_in_root(root: &Path, path: &Path) -> Result<PeHeaders>`: tenta `root` + caminho e, se falhar, o caminho direto (mesma regra do `open_disk`).
  - `Module` ganha `pub memory_size: u64` e `pub disk_size: Option<u64>`; `size` passa a ser `min(memory_size, disk_size)` para confiáveis com disco lido, senão `memory_size`. `pub fn extent_checked(&self) -> bool` = `trusted && disk_size.is_some()`.
  - `discover(maps, reader, policy, root: &Path) -> Result<Vec<Module>>`: para cada módulo **confiável**, `open_in_root`; diferença entre os tamanhos → `tracing::warn!(module, memory, disk)`; falha → `tracing::warn!` e `disk_size: None`. Terceiros não abrem o disco.
  - `inspect`: `InspectOutput` perde `memory_readable` e ganha `disk_size_of_image: Option<u32>` e `extent_checked: bool`. Texto: a linha "Leitura de memória: …" vira `Extensão do módulo: conferida com o disco`, `Extensão do módulo: não conferida (disco ilegível)` ou, para módulo de terceiros, `Extensão do módulo: não conferida (módulo de terceiros)` (`InspectOutput` ganha `trusted: bool`); se `disk_size_of_image` existe e difere, linha `SizeOfImage (disco): {:#x}` logo após a do `SizeOfImage`.

- [ ] **Passo 1: Testes que falham**

Em `modules.rs` (arquivos PE sintéticos em `tempfile`, reaproveitando o gerador de PE de teste do `pe.rs` exposto como `#[cfg(test)] pub(crate)`):
- `trusted_extent_is_min_of_memory_and_disk` (memória `0x3000`, disco `0x2000` → `size == 0x2000`, `extent_checked()`);
- `trusted_extent_without_disk_keeps_memory` (arquivo ausente → `size == memory_size`, `!extent_checked()`);
- `third_party_extent_ignores_disk` (disco menor, módulo não confiável → `size == memory_size`, `disk_size == None`).

Em `pe.rs`: `read_file_headers_reads_only_the_header` (arquivo de 64 KiB → mesmos `PeHeaders` de `from_file_bytes`).
Em `inspect.rs`: `text_shows_extent_status_and_disk_size` (chama `write_text` num `Vec<u8>`; com `extent_checked: false` aparece "não conferida (disco ilegível)"; com `disk_size_of_image: Some(0x2000)` e memória `0x3000` aparece "SizeOfImage (disco): 0x2000"; não aparece "Leitura de memória").

- [ ] **Passo 2: Rodar e ver falhar.**
- [ ] **Passo 3: Implementar.** `Target::open` passa `/proc/<pid>/root` ao `discover`.
- [ ] **Passo 4: Validar.**
- [ ] **Passo 5: Commit** — `feat(core): extensão de módulo confiável limitada pelo disco`

---

### Tarefa 5: `dump --force` atômico e lacuna no fim do bloco

Itens 7.2 e 7.3.

**Arquivos:**
- Modificar: `apps/gt-cli/src/cmd/{dump.rs,compare.rs}`, `apps/gt-cli/Cargo.toml` (dep `nix`, que já tem `fs` desde a Tarefa 1), `apps/gt-cli/tests/cli.rs`

**Interfaces:**
- `dump`: antes de gravar, pré-valida o destino: sem `--force`, recusa se **qualquer um** dos dois existir; com `--force`, recusa se algum deles for diretório. Depois grava `.bin` e `.json` em temporários no mesmo diretório (`.<nome>.tmp-<pid>`); se qualquer escrita falhar, apaga os temporários e devolve o erro. Depois renomeia **primeiro o `.json`, depois o `.bin`**. Sem `--force`, usa `nix::fcntl::renameat2(..., RenameFlags::RENAME_NOREPLACE)`; com `--force`, `std::fs::rename`. Falha num rename apaga os temporários que sobraram; sem `--force`, se o `.bin` falhar depois do `.json` já renomeado, apaga o `.json` recém-criado (com `--force`, esse limite fica documentado num comentário).
- `compare_section`: quando o último trecho do bloco é lacuna pura que toca o fim do bloco, o teste de "trecho aberto" olha o último trecho **não lacuna**; se ele termina a menos de `GROUP_GAP` do fim do bloco, o bloco é adiado a partir dele.

- [ ] **Passo 1: Testes que falham**

Em `compare.rs`: `hole_at_block_end_defers_previous_difference`. Seção de 192 bytes, bloco 64; diferença de 1 byte no offset 58; lacuna `[60, 70)`; diferença no offset 72 (distância 14 < `GROUP_GAP`: na seção inteira vira 1 trecho fundido `unreadable`; sem a correção, o bloco a bloco dá 3). Asserção: `compare_section(start, &disk, 64, read) == build_chunks(&mem, &holes, &disk, start)` (a seção inteira como referência).

Em `tests/cli.rs`: `dump_force_refuses_directory_and_keeps_previous_pair`. Cria `x.bin` com `OLD` e um **diretório** `x.json`; `gt dump <marcador> 32 -o x.bin --force` falha; `x.bin` continua `OLD`; nenhum arquivo `.x.*tmp*` sobra no diretório. Unitário em `dump.rs`: `failed_temp_write_removes_temps` (a gravação do par é `write_pair(bin: &Path, json: &Path, bin_bytes: &[u8], json_bytes: &[u8], force: bool) -> anyhow::Result<()>`; com `json` num diretório inexistente → erro, nenhum temporário sobra, `.bin` anterior intacto).

- [ ] **Passo 2: Rodar e ver falhar.**
- [ ] **Passo 3: Implementar.**
- [ ] **Passo 4: Validar.**
- [ ] **Passo 5: Commit** — `fix(cli): dump atômico e lacuna no fim do bloco do diff`

---

### Tarefa 6: Informações de `/proc`, `pidfd` e sinais

Base da spec 6.5, 6.7 e 6.8.

**Arquivos:**
- Criar: `crates/gt-core/src/procinfo.rs`, `crates/gt-core/src/mem/sys.rs`
- Modificar: `crates/gt-core/{Cargo.toml,src/lib.rs,src/mem/mod.rs}`, `Cargo.toml` (features `signal` e `poll` do `nix`)

**Interfaces:**
- Produz (`procinfo`, sem feature):
  - `pub struct ProcStat { pub state: char, pub start_time: u64 }`; `pub fn parse_stat(text: &str) -> Result<ProcStat>` (o nome vai até o **último** `)`; estado é o campo 3, `start_time` o 22). `pub fn read_stat(pid: i32) -> Result<ProcStat>` (`ENOENT`/`ESRCH` → `ProcessGone`); `pub fn read_task_stat(pid: i32, tid: i32) -> Result<Option<ProcStat>>` (`None` = a thread sumiu; não é `ProcessGone`).
  - `pub fn is_dead(state: char) -> bool` (`Z` ou `X`); `pub fn is_stopped(state: char) -> bool` (`T` ou `t`).
  - `pub enum ThreadPos { Running, Stopped { syscall: Option<i64>, pc: u64 } }` (`syscall: None` para `-1`); `pub fn parse_syscall(text: &str) -> Result<ThreadPos>`; `pub fn read_thread_pos(pid: i32, tid: i32) -> Result<Option<ThreadPos>>` (`None` = thread sumiu ou arquivo ilegível).
  - `pub fn list_tids(pid: i32) -> Result<Vec<i32>>`; `pub fn boot_id() -> Result<String>` (conteúdo de `/proc/sys/kernel/random/boot_id` sem espaços).
  - `pub struct Identity { pub pid: i32, pub start_time: u64, pub boot_id: String }`; `pub fn identity(pid: i32) -> Result<Identity>`; `pub fn check_alive(id: &Identity) -> Result<()>` (`ProcessGone` se sumiu, `start_time` diferente, morto ou `boot_id` diferente).
- Produz (`mem::sys`, feature `write`, único `unsafe` novo):
  - `pub struct Pidfd(OwnedFd)`: `open(pid: i32) -> Result<Pidfd>` (`libc::syscall(SYS_pidfd_open)`), `send(&self, sig: nix::sys::signal::Signal) -> Result<()>` (`SYS_pidfd_send_signal`; `ESRCH` → `ProcessGone`), `exited(&self, timeout: Duration) -> Result<bool>` (`nix::poll` no fd; `EINTR` = `Ok(false)`, porque é o caminho normal quando chega SIGINT/SIGTERM).
  - `pub fn ignore_job_control() -> Result<()>`: `SIG_IGN` para SIGTSTP, SIGTTIN e SIGTTOU.
- Erros novos: `Error::BadProc { file: String, reason: String }` ("formato inesperado em {file}: {reason}").

- [ ] **Passo 1: Testes que falham** (unitários em `procinfo.rs`)

```rust
#[test]
fn stat_with_spaces_and_parens_in_name() {
    let s = parse_stat("1234 (a b) (c)) T 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 987654 0").unwrap();
    assert_eq!((s.state, s.start_time), ('T', 987654));
}
#[test]
fn syscall_variants() {
    assert_eq!(parse_syscall("running\n").unwrap(), ThreadPos::Running);
    assert_eq!(parse_syscall("-1 0x7fe12e3fc6e0 0x7fe13ea38d4c\n").unwrap(),
        ThreadPos::Stopped { syscall: None, pc: 0x7fe13ea38d4c });
    assert_eq!(parse_syscall("202 0x7fe13eda65c0 0x89 0x166 0x7ffdf0fff480 0x0 0xffffffff 0x7ffdf0fff338 0x7fe13e682352\n").unwrap(),
        ThreadPos::Stopped { syscall: Some(202), pc: 0x7fe13e682352 });
}
#[test] fn bad_stat_and_syscall_are_errors()   // "", "1 (x", "abc" → BadProc
#[test] fn dead_and_stopped_states()           // is_dead('Z'), is_dead('X'), is_stopped('t'), !is_stopped('R')
```

Em `crates/gt-testkit/tests/core_live.rs`: `identity_and_pidfd_on_live_target` (identidade do alvo; `check_alive` ok; a mesma `Identity` com `start_time + 1` → `ProcessGone`; `Pidfd::open` + `send(SIGSTOP)` deixa todas as threads em `T` em até 500 ms; `send(SIGCONT)` volta; depois de `kill`+`wait`, `check_alive` → `ProcessGone` e `pidfd.exited(0)` → `true`).

- [ ] **Passo 2: Rodar e ver falhar.**
- [ ] **Passo 3: Implementar.**
- [ ] **Passo 4: Validar.**
- [ ] **Passo 5: Commit** — `feat(core): leitura de /proc, pidfd e sinais`

---

### Tarefa 7: Pausa com conferência das threads

Spec 6.5 (sem escrita) e a amostragem do `profile`.

**Arquivos:**
- Criar: `crates/gt-core/src/suspend.rs` (feature `write`, exceto as funções puras, que ficam fora da feature)
- Modificar: `crates/gt-core/src/{lib.rs,error.rs,mem/test_support.rs}`, `crates/gt-testkit/src/{lib.rs,bin/gt-testtarget.rs}`, `crates/gt-testkit/tests/engine_live.rs` (novo)

**Interfaces:**
- Produz (puras):
  - `pub fn resume_points(pos: &ThreadPos) -> Vec<u64>`: `Stopped { syscall: None, pc }` → `[pc]`; `Some(_)` → `[pc, pc - 2]` (`wrapping_sub`); `Running` → `[]`.
  - `pub fn all_stopped(states: &[char]) -> bool`: ignora mortos; vazio → `true`.
  - `pub fn wait_until(mut probe: impl FnMut() -> Result<bool>, timeout: Duration, step: Duration) -> Result<bool>`: `true` quando `probe` deu `true` dentro do prazo.
  - `#[derive(Clone)] pub struct PauseConfig { pub stop_timeout: Duration, pub max_attempts: u32, pub retry_wait: Duration, #[doc(hidden)] pub hook: Option<fn(HookPoint)> }` com `Default` (500 ms, 10, 20 ms, `None`). `pub enum HookPoint { InPause, AfterApply }`.
- Produz (feature `write`):
  - `pub struct Thread { pub tid: i32, pub pos: Option<ThreadPos> }` (`None` = posição ilegível). Threads mortas (`Z`/`X`) e as que sumiram não entram em `threads()`, para não bloquearem a regra de perigo.
  - `pub struct PauseGuard<'a>`: resultado de uma pausa; `threads(&self) -> &[Thread]`; `sent_stop(&self) -> bool`; `still_stopped(&self) -> Result<bool>`; `resume(self) -> Result<()>`; `Drop` envia SIGCONT se `sent_stop` e ainda não retomou (erro só em `tracing`).
  - `pub fn is_stopped_now(pid: i32) -> Result<bool>`: todas as threads vivas em `T`/`t` (usado antes de gravar o marcador no diário).
  - `pub fn open_checked(pid: i32) -> Result<(Pidfd, Identity)>`: horário → `Pidfd::open` → horário de novo (a parte de sinais da ordem da spec 6.7; o `Engine` e o `profile` usam).
  - `pub fn pause<'a>(pidfd: &'a Pidfd, pid: i32, cfg: &PauseConfig, need_positions: bool) -> Result<PauseGuard<'a>>`: se todas as threads vivas já estão paradas, não envia sinal (`sent_stop = false`); senão SIGSTOP + `wait_until(all_stopped)`; estouro → SIGCONT e `Error::PauseTimeout { attempts: 1 }`; com `need_positions`, lê `read_thread_pos` de cada tid (relê a lista de tids até ficar estável); sem, `threads()` traz `pos: None` (patch sem fronteira perdida não precisa, e a pausa fica mais curta). O `PauseGuard` é criado **antes** de chamar o gancho, para que um pânico no gancho envie SIGCONT. O gancho (`cfg.hook(HookPoint::InPause)`) não é chamado se `std::thread::panicking()`; o motor (Tarefa 11) garante que cada gancho dispara no máximo uma vez por `Engine` e nunca durante o `Drop`.
  - `pub fn sample_pcs(pidfd: &Pidfd, pid: i32, cfg: &PauseConfig, samples: u32, interval: Duration, before_pause: impl FnMut(bool) -> Result<()>, after_resume: impl FnMut() -> Result<()>, should_stop: impl FnMut() -> bool) -> Result<Vec<u64>>` (`should_stop` consultado antes de cada amostra; `true` devolve as amostras já coletadas) (`PauseTimeout` numa amostra: tenta de novo até `max_attempts` vezes seguidas, depois aborta com o erro; `ProcessGone` aborta na hora. `before_pause` recebe `will_stop` = `!is_stopped_now()`, para o marcador do diário só ser gravado quando vamos enviar SIGSTOP; `after_resume` limpa).
- Erro novo: `PauseTimeout { attempts: u32 }` ("o jogo não pausou no prazo depois de {attempts} tentativas").
- `test_support`: `exec_code(code: &[u8], pages: usize) -> Result<u64>` (mapeia `pages` páginas, preenche com `0x90`, copia `code` no início, `PROT_READ|PROT_EXEC`); `spawn_thread_at(addr: u64) -> Result<()>` (thread que salta para `addr` e nunca volta; `SAFETY`: o código é do próprio teste, não retorna e só usa a zona vermelha da pilha); `pipe_never_written() -> Result<i32>` (fd de leitura; a ponta de escrita fica aberta e vazada).
- Alvo de teste: novos campos `spin_page` (código `48 8B F9 EB FB`), `syscall_page` (código abaixo, numa thread bloqueada para sempre, sem gastar CPU), `exec2` (2 páginas `0x90`, sem thread), `ro_shared` (Tarefa 11). O alvo só imprime o JSON depois que as threads estão no lugar: a do `syscall_page` com `/proc/self/task/<tid>/syscall` mostrando a syscall 0 e `pc` = `syscall_page + 22` (poll a cada 1 ms, até 2 s), e a do `spin_page` (com `--spin`) com estado `R`. A thread que gira no `spin_page` só é criada com a opção `--spin` (`TestTarget::spawn_with(TargetOpts { spin: true })`; `spawn()` = sem `--spin`), para não saturar a CPU com dezenas de alvos em paralelo. Código do `syscall_page` (fd montado em tempo de execução):

```
+0  B8 00 00 00 00   mov eax, 0        ; read
+5  BF <fd u32 le>   mov edi, fd
+10 48 8D 74 24 F0   lea rsi, [rsp-16]
+15 BA 01 00 00 00   mov edx, 1
+20 0F 05            syscall           ; pc parado = +22; pc - 2 = +20
+22 90 90 90 90
+26 EB E4            jmp +0
```

- [ ] **Passo 1: Testes que falham**

Unitários: `resume_points_variants`, `all_stopped_ignores_dead`, `wait_until_times_out` (probe sempre `false`, prazo 30 ms → `Ok(false)`), `wait_until_succeeds`.
Em `engine_live.rs`:
- `pause_stops_and_guard_resumes`: `pause` → todas as threads em `T`; o `pc` da thread do `syscall_page` aparece em `threads()` com `syscall: Some(0)` e `pc == syscall_page + 22`; `resume()` → nenhuma thread em `T`.
- `guard_drop_resumes`: `drop(guard)` sem `resume()` → nenhuma thread em `T`.
- `already_stopped_target_is_left_stopped`: o teste manda SIGSTOP antes e espera `wait_until(is_stopped_now)`; `pause` devolve `sent_stop() == false`; depois de `resume()`, as threads continuam em `T`.
- `sample_pcs_stops_early` (alvo sem `spin`): `should_stop` que devolve `true` na 3ª consulta → 2 amostras, alvo não parado.
- `sample_pcs_sees_spin_loop` (alvo com `spin: true`): 50 amostras a cada 2 ms; pelo menos uma em `[spin_page, spin_page + 5)`.

- [ ] **Passo 2: Rodar e ver falhar.**
- [ ] **Passo 3: Implementar.**
- [ ] **Passo 4: Validar.**
- [ ] **Passo 5: Commit** — `feat(core): pausa do processo com conferência das threads`

---

### Tarefa 8: Sessão A com o jogo (portão da pausa e `gt scan`)

> **Sessão principal** a partir do Passo 3 (precisa do dono).

Spec 6.5 (portão), 6.12 (contagem) e 7.1. **Precisa do dono.**

**Arquivos:**
- Criar: `crates/gt-grounded2/tests/game_pause.rs` (`#[ignore]`)
- Modificar: `crates/gt-grounded2/Cargo.toml` (dev-dep `gt-core` com `write`), `docs/RESEARCH.md`

**Interfaces:**
- Consome: `Target::open(None)` (Tarefa 3), `Pidfd`, `pause`, `PauseGuard` (Tarefas 6–7), `Module::disk_size`/`memory_size` (Tarefa 4).

- [ ] **Passo 1: Escrever os testes `#[ignore]`**

- `pause_gate`: 100 pausas com 300 ms entre elas, sem escrita. Cada pausa: `pause` (com uma nova tentativa permitida por prazo; total de novas tentativas ≤ 2); toda thread viva com `pos` legível; `resume`; depois do SIGCONT, `wait_until` (500 ms) que nenhuma thread esteja em `T`. Imprime com `eprintln!`: tempo até parar (mín/médio/máx), tids vistos em `D`, os 20 `pc` mais frequentes com `módulo+offset`.
- `size_of_image_matches_disk`: conta os módulos confiáveis com `disk_size != Some(memory_size)` e imprime cada um; asserção `== 0`.

- [ ] **Passo 2: Compilar** — `cargo test -p gt-grounded2 --no-run`.

- [ ] **Passo 3: Pedir ao dono:** "abra o Grounded 2 num save de teste, abra o FLiNG e conecte, sem ligar nenhuma opção; me avise quando estiver na partida".

- [ ] **Passo 4: Rodar o portão**

Run: `cargo test -p gt-grounded2 --test game_pause -- --ignored --nocapture`
Esperado: os dois testes passam. Perguntar ao dono se o jogo engasgou ou travou. **Se `pause_gate` falhar:** parar o plano e levar ao dono as alternativas da spec 6.5 (emenda à spec).

- [ ] **Passo 5: Reconferir o `gt scan`** (spec 7.1), com `cargo run -p gt-cli --`:
  - `scan "e8 ?? ?? ?? ?? 48 83 be b0 00 00 00 00 48 8b d8"` e o mesmo com `--disk` → "único" nos dois;
  - `scan "f3 0f 10 89 74 09 00 00 f3 0f 58 c9"` → 1 ocorrência na memória; com `--disk` → 2;
  - `scan --all "<god mode>"` → "único", sem bloqueio nem `inconclusive` (se vier `inconclusive`, registrar as lacunas e explicar antes de seguir);
  - `inspect --module TrSpeedHack_x64.dll` e `scan --module TrSpeedHack_x64.dll "48 8B"`: se a DLL estiver no `maps`, o `scan` é recusado pela guarda; senão, repetir com qualquer PE não confiável do `maps` (achado com `gt inspect`/`maps`); se não houver nenhum, registrar o fato.

- [ ] **Passo 6: Registrar no `RESEARCH.md`** — nova sessão "2026-10-?? — portão da pausa e reconferência do `gt scan`": versão, tempos de pausa, threads em `D`, `pc` frequentes, saídas do `scan`, contagem do `SizeOfImage`.

- [ ] **Passo 7: Avisar o dono** que pode fechar o FLiNG e o jogo.

- [ ] **Passo 8: Commit** — `test(grounded2): portão da pausa no jogo e reconferência do scan`

---

### Tarefa 9: Patch: validação, fronteiras e regra de perigo

Spec 6.1, 6.5 item 4 e 6.6 (parte genérica).

**Arquivos:**
- Criar: `crates/gt-core/src/patch.rs` (sem feature)
- Modificar: `crates/gt-core/src/{lib.rs,error.rs}`

**Interfaces:**
- Produz:
  - `pub const MAX_PATCH_LEN: usize = 64;`
  - `#[derive(Clone, Debug, PartialEq, Eq)] pub struct PatchSpec { pub id: String, pub addr: u64, pub expected: Vec<u8>, pub replacement: Vec<u8> }`; `pub fn range(&self) -> Range<u64>`.
  - `#[derive(Clone, Debug, PartialEq, Eq)] pub struct Validated { pub spec: PatchSpec, pub expected_starts: Vec<u64>, pub replacement_starts: Vec<u64> }` (endereços absolutos de início de instrução de cada lado, incluindo `addr`); métodos `lost_on_apply(&self) -> Vec<u64>` (inícios dos esperados que não são inícios dos novos) e `lost_on_undo(&self) -> Vec<u64>`.
  - `pub fn validate(spec: PatchSpec) -> Result<Validated>`: tamanhos iguais, `1..=MAX_PATCH_LEN`; cada lado decodifica (iced-x86, 64 bits, IP = `addr`) em instruções válidas que terminam exatamente no fim; regra do `call` nos dois sentidos (spec 6.1), onde "call" é `Mnemonic::Call` (relativo e indireto); `syscall`/`sysenter` **não** contam (o `FlowControl::Call` do iced-x86 inclui `syscall`; não usar), porque para eles quem vigia é a regra `pc`/`pc - 2`. Erros: `Error::BadPatch { id, reason }` ("patch {id} inválido: {reason}").
  - `pub enum Direction { Apply, Undo }`.
  - `pub fn is_dangerous(v: &Validated, dir: Direction, point: Option<u64>) -> bool`: `None` (posição ilegível) → `true` só se a direção tem fronteira perdida; `Some(p)` → `addr < p < fim` e `p` não é início no lado escrito.
  - `pub struct WritePolicy { ranges: Vec<Range<u64>> }`: `new(Vec<Range<u64>>)` (funde intervalos que se tocam ou se sobrepõem: VMAs vizinhas do `maps` da mesma seção não podem recusar um patch que cruza a emenda), `allows(&self, r: &Range<u64>) -> bool` (o intervalo inteiro dentro de **um** intervalo da política).
  - `pub fn overlaps(a: &Range<u64>, b: &Range<u64>) -> bool`.

- [ ] **Passo 1: Testes que falham**

```rust
#[test] fn equivalent_mov_has_no_lost_boundaries()   // 48 8B F9 → 48 89 CF: válido, lost_on_apply/undo vazios
#[test] fn rejects_size_mismatch_empty_and_too_long() // 3 vs 2 bytes; vazio; 65 bytes → BadPatch
#[test] fn rejects_partial_instruction()              // expected "48 8B" (instrução cortada) → BadPatch
#[test] fn two_into_one_loses_middle_boundary()       // "90 90" → "66 90": lost_on_apply == [addr + 1]
#[test] fn call_return_point_must_exist_on_both_sides()
    // expected "E8 00 00 00 00 90" (call + nop) → replacement "0F 1F 44 00 00 90": ok (addr+5 é início nos dois);
    // replacement "66 0F 1F 44 00 00" (um nop de 6): BadPatch citando "call"; e o inverso (call só nos novos) também.
#[test] fn danger_rule() {
    // "90 0F 05 90 90" → "66 90 0F 1F 00" (addr = 0x1000): inícios esperados {0,1,3,4}, novos {0,2}
    // Apply: Some(0x1000) false; Some(0x1003) true; Some(0x1001) true; Some(0x1005) false (fim); None true.
    // Patch sem fronteira perdida: None false.
}
#[test] fn syscall_is_not_a_call()                    // "0F 05 90 90 90 90" → "66 66 66 66 66 90": válido (Ok), lost_on_apply contém addr + 2
#[test] fn policy_allows_only_whole_ranges()          // [0x1000,0x2000): 0x1ffe..0x2002 recusado; 0x1000..0x1003 aceito
```

- [ ] **Passo 2: Rodar e ver falhar.**
- [ ] **Passo 3: Implementar.**
- [ ] **Passo 4: Validar.**
- [ ] **Passo 5: Commit** — `feat(core): validação de patch e regra de perigo`

---

### Tarefa 10: Diário

Spec 6.8 (modelo, armazenamento, trava, decisão pura).

**Arquivos:**
- Criar: `crates/gt-core/src/journal.rs`
- Modificar: `crates/gt-core/{Cargo.toml,src/lib.rs,src/error.rs}` (deps `serde`, `serde_json`), `docs/ARCHITECTURE.md` (tabela 4)

**Interfaces:**
- Produz (sem feature):
  - `#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)] pub struct Journal { pub boot_id: String, pub pid: i32, pub start_time: u64, pub version_key: String, pub paused_by_us: bool, pub entries: Vec<JournalEntry> }`; `JournalEntry { pub id: String, pub addr: u64, pub original: Vec<u8>, pub replacement: Vec<u8> }`. No JSON, bytes como texto hex sem espaços (`"488bf9"`) e `addr` como `"0x..."`.
  - `pub enum Loaded { Missing, Ok(Journal), Corrupt { moved_to: PathBuf } }`.
  - `#[derive(Clone)] pub struct JournalStore { dir: PathBuf }`: `new(dir)`, `default_dir() -> Option<PathBuf>` (lê o ambiente e chama a pura `default_dir_from(xdg: Option<OsString>, home: Option<OsString>) -> Option<PathBuf>`: `$XDG_STATE_HOME/grounded-trainer` (ignorado se for relativo, como manda a especificação XDG) ou `$HOME/.local/state/grounded-trainer`; testar a pura, sem `set_var`), `load(&self) -> Result<Loaded>` (corrompido → `rename` para `journal.json.bad`), `peek(&self) -> Result<Loaded>` (igual, mas **sem** renomear o corrompido: o `list` não escreve nada), `save(&self, j: &Journal) -> Result<()>` (cria o diretório; temporário + `fsync` + `rename` + `fsync` do diretório), `clear(&self) -> Result<()>` (sem arquivo é ok).
  - Decisão pura (usa `procinfo::ProcStat`): `pub enum Recovery { Nothing, Discard(DiscardReason), SameProcess { resume: bool, stopped_by_other: bool, actions: Vec<(JournalEntry, EntryAction)> } }`; `pub enum DiscardReason { OtherBoot, ProcessGone, OtherProcess }`; `pub enum EntryAction { Drop, Restore, Foreign }`.
  - `pub fn decide_entry(e: &JournalEntry, current: &[u8]) -> EntryAction`: igual aos originais → `Drop`; cada byte igual ao original ou ao novo daquela posição → `Restore`; senão `Foreign`.
  - `pub fn decide(j: &Journal, live_boot: &str, live: Option<ProcStat>, current: impl Fn(&JournalEntry) -> Option<Vec<u8>>) -> Recovery`: `boot_id` diferente → `Discard(OtherBoot)`; `live == None` ou morto → `Discard(ProcessGone)`; `start_time` diferente → `Discard(OtherProcess)`; senão `SameProcess` com `resume = stopped && paused_by_us`, `stopped_by_other = stopped && !paused_by_us`; bytes ilegíveis (`None`) → `Foreign`.
- Produz (feature `write`):
  - `pub struct JournalLock { file: File }` (solta ao cair); `JournalStore::lock(&self) -> Result<JournalLock>` (cria o diretório; `F_OFD_SETLK` exclusivo em `journal.lock` com `l_pid = 0`; `EAGAIN`/`EACCES` → `Error::AlreadyRunning`); `JournalStore::is_locked(&self) -> Result<bool>` (abre `O_RDONLY` sem criar; `F_OFD_GETLK` com `l_pid = 0`, não segura nada; sem arquivo → `false`).
- Erros novos: `AlreadyRunning` ("outro gt-patch já está em execução"), `Journal(String)` ("diário: {0}").

Observação do plano: a recuperação compara `boot_id`, PID e `start_time`. A versão do jogo não muda dentro do mesmo processo, então "mesma versão" (spec 6.8) é consequência do mesmo processo; a `version_key` do diário serve para o `list` e para as mensagens.

- [ ] **Passo 1: Testes que falham**

Unitários: `json_round_trip_uses_hex`; `decide_entry_cases` (originais → `Drop`; novos → `Restore`; metade original + metade nova → `Restore`; um byte estranho → `Foreign`); `decide_cases` (outro boot; processo sumido; zumbi `'Z'`; outro `start_time`; parado com e sem marcador → `resume`/`stopped_by_other`; bytes ilegíveis → `Foreign`).
Com `tempfile`: `save_load_clear` (salva, carrega igual, `clear`, `Missing`); `corrupt_is_moved_to_bad` (`journal.json` = `"{"` → `Corrupt`, arquivo `.bad` existe, `journal.json` não); `lock_is_exclusive_and_query_does_not_hold` (`lock()` ok; `lock()` de novo no mesmo processo por outro `File` → `AlreadyRunning`, porque OFD vale por descrição de arquivo; `is_locked()` → `true`; depois do `drop`, `is_locked()` → `false` e `lock()` ok).

- [ ] **Passo 2: Rodar e ver falhar.**
- [ ] **Passo 3: Implementar.**
- [ ] **Passo 4: Validar.**
- [ ] **Passo 5: Commit** — `feat(core): diário de patches com trava e decisão de recuperação`

---

### Tarefa 11: Motor (aplicar e desfazer)

Spec 6.2, 6.5 (tentativas), 6.6 (política crua), 6.7.

**Arquivos:**
- Criar: `crates/gt-core/src/engine.rs` (feature `write`)
- Modificar: `crates/gt-core/src/{lib.rs,error.rs,mem/writer.rs,mem/test_support.rs}`, `crates/gt-testkit/{src/lib.rs,src/bin/gt-testtarget.rs,tests/engine_live.rs}`

**Interfaces:**
- Consome: `Validated`, `is_dangerous`, `WritePolicy` (T9); `JournalStore`, `JournalLock`, `Journal` (T10); `pause`, `PauseGuard`, `PauseConfig` (T7); `Pidfd`, `procinfo::identity`/`check_alive` (T6); `MemReader`, `MemWriter`.
- Produz:
  - `pub type PolicyFn = Box<dyn FnMut(&[MapRegion]) -> WritePolicy>`.
  - `pub struct EngineOptions { pub version_key: String, pub pause: PauseConfig }`.
  - `pub struct Engine` com `pub fn open(pid: i32, store: JournalStore, lock: &JournalLock, policy: PolicyFn, opts: EngineOptions) -> Result<Engine>`: o `lock` é só testemunha de que quem chama segura a trava (não é guardado; quem chama mantém o `JournalLock` vivo enquanto o `Engine` existir). Ordem horário → `Pidfd::open` → `MemWriter::open` → horário de novo (spec 6.7). O `MemWriter` passa a abrir `/proc/<pid>/mem` em leitura e escrita e ganha `read_exact(&self, addr, buf)`; o motor lê os bytes atuais por ele (preso ao processo original), não pelo `MemReader` por PID.
  - `pub fn restore_entries(&mut self, entries: Vec<JournalEntry>) -> Result<RestoreReport>`: para cada entrada, sob a mesma pausa e com a mesma política (os intervalos das próprias entradas), relê os bytes e aplica `decide_entry`: `Restore` escreve os originais (aceita bytes misturados, escrita interrompida); `Drop` não escreve; `Foreign` não escreve e avisa. `RestoreReport { pub restored: Vec<String>, pub dropped: Vec<String>, pub foreign: Vec<String> }`. Usado pela recuperação (Tarefa 12) e pelo `Drop`.
  - `pub struct BatchReport { pub pause: Duration, pub danger_retries: u32, pub timeout_retries: u32, pub restored: Vec<String>, pub foreign: Vec<String> }` (`restored` = ids aplicados ou restaurados de fato; `foreign` = ids não tocados porque os bytes eram de outro).
  - `pub fn apply(&mut self, batch: Vec<Validated>) -> Result<BatchReport>`: passos 1–8 da spec 6.2. Política: relê o `maps` (`read_maps`) e chama `policy`; fora → `OutsidePolicy`; sobreposição com ativo ou dentro do lote → `Overlap`; leitura prévia ≠ esperados → `BytesMismatch` (nada escrito, nada pausado, diário intacto); grava intenção (+ `paused_by_us` se vai pausar) **antes** de pausar; tentativas até `max_attempts` (perigo → `danger_retries`, prazo → `timeout_retries`); esgotou → `ThreadsInPatch` ou `PauseTimeout`; `still_stopped()` antes de escrever (falso → nova tentativa); releitura sob pausa; escreve e relê; falha no meio → desfaz o que escreveu ainda pausado; `hook(AfterApply)` depois do SIGCONT; diário final sem marcador. Nos caminhos de falha (`BytesMismatch`, `ThreadsInPatch`, `PauseTimeout`) o diário volta ao estado anterior ao lote (sem a intenção e sem o marcador). O marcador `paused_by_us` só é gravado se `is_stopped_now()` deu `false` logo antes de gravar a intenção.
  - Ganchos: cada `HookPoint` dispara no máximo uma vez por `Engine` e nunca dentro do `Drop`: o `Engine` passa ao `pause` uma cópia do `PauseConfig` (que deriva `Clone`) com `hook = None` depois do primeiro disparo. `InPause` roda depois de `all_stopped` e da leitura das posições.
  - `pub fn undo(&mut self, ids: &[&str]) -> Result<BatchReport>` e `pub fn undo_all(&mut self) -> Result<BatchReport>`: bytes atuais = novos → escreve originais; outros → `tracing::warn!`, tira do registro e do diário sem escrever.
  - **Um só procedimento para os três caminhos** (`apply`, `undo`/`undo_all`, `restore_entries`): identidade → política → intenção e marcador no diário antes da pausa → **uma** pausa para o lote todo → tentativas com a regra de perigo → `still_stopped` → releitura sob pausa → escrita e releitura → SIGCONT → diário. `apply` usa `Direction::Apply`; `undo`/`undo_all` usam `Direction::Undo` (sobre o `Validated` guardado no registro); `need_positions` = alguma fronteira perdida na direção. `restore_entries` valida cada entrada com `validate(PatchSpec { expected: original, replacement: replacement })`: entrada que não valida vira `Foreign` (sem escrita); com bytes misturados, os pontos perigosos são a **união** das fronteiras perdidas dos dois sentidos. Falha por perigo ou prazo mantém as entradas no diário (a próxima execução tenta de novo).
  - Antes de reler sob pausa, `check_alive`; leitura curta num processo morto vira `ProcessGone`.
  - `pub fn active(&self) -> Vec<&PatchSpec>`; `pub fn pid(&self) -> i32`; `pub fn pidfd(&self) -> &Pidfd`; `pub fn store(&self) -> &JournalStore`.
  - Identidade conferida antes de cada pausa e de cada escrita; `ProcessGone` (inclusive vindo do escritor por `WriteZero`) → registro descartado, diário limpo, erro `ProcessGone` devolvido.
  - Escrita recusada pelo kernel (`PermissionDenied`/`Unmapped`) num intervalo da política → `Error::WriteRefused { addr }` ("o kernel recusou a escrita em {addr:#x}; confira proc_mem.force_override").
  - `Drop`: `undo_all`; depois `restore_entries` com as entradas que restarem no diário (intenções); limpa o diário se tudo foi resolvido. Nunca entra em pânico. Se algo não pôde ser restaurado, o diário fica e o `tracing::error!` diz que a próxima execução vai restaurar.
- Erros novos: `BadPatch` já existe; `OutsidePolicy { id: String, addr: u64 }` ("patch {id} fora da área permitida para escrita ({addr:#x})"), `Overlap { id: String }` ("patch {id} se sobrepõe a um patch ativo"), `BytesMismatch { id: String, expected: String, found: String }` ("bytes diferentes do esperado no patch {id}: esperados {expected}, encontrados {found}", hex com espaços), `ThreadsInPatch { id: String, attempts: u32 }` ("threads do jogo não saem do trecho do patch {id} depois de {attempts} tentativas"), `WriteRefused { addr: u64 }`.

- [ ] **Passo 1: Testes que falham** (em `engine_live.rs`; cada teste usa `tempfile` para o diário e a política crua com as páginas do alvo)

- `apply_and_undo_on_exec_page`: `exec2`, `90 90 90` → `66 90 90`; bytes trocados, diário com 1 entrada sem marcador; `undo_all` → bytes originais, diário vazio.
- `mismatch_refuses_without_pausing`: esperados errados → `BytesMismatch`; nenhuma thread ficou em `T`; diário `Missing`.
- (Nos dois testes `lost_boundary_*` abaixo, afirmar também diário `Missing` depois do `ThreadsInPatch`.)
- `outside_policy_refuses`: endereço fora da política → `OutsidePolicy`.
- `overlap_refuses`: segundo patch que toca o primeiro → `Overlap`.
- `drop_restores`: `apply` e `drop(engine)` → bytes originais, diário `Missing`.
- `dead_target_discards_without_writing`: `apply`; `kill` + `wait` do alvo; `undo_all` → `ProcessGone`, diário `Missing`.
- `zombie_target_is_closed`: `apply`; `kill` **sem** `wait`; `wait_until` (500 ms) que `read_stat(pid).state` seja `Z`; `undo_all` → `ProcessGone`, sem `WriteRefused`.
- (Os testes com gancho usam cada um o próprio `static` e a própria `fn`, para rodarem em paralelo no mesmo binário.)
- `apply_when_target_dies_during_pause`: `hook` em `InPause` que mata o alvo (pid num `static AtomicI32`) → `ProcessGone`; diário `Missing`.
- `apply_refuses_when_journal_unwritable`: cria o `journal.lock` (pega a trava) e depois põe o diretório em modo `0o500` → `Error::Journal` (a gravação do diário mapeia erros de E/S para `Journal` com o caminho); restaurar o modo antes do fim do teste; bytes intactos; nenhuma thread em `T`.
- `patch_across_page_boundary`: `exec2` no endereço `exec2 + page - 2`, `90 90 90 90` → `66 90 66 90`; aplica e desfaz.
- `already_stopped_target_gets_written_and_stays_stopped`: o teste para o alvo e espera `wait_until(is_stopped_now)`; `apply` funciona; as threads continuam em `T` depois.
- `lost_boundary_at_syscall_pc_is_refused`: em `syscall_page + 20`, `0F 05 90 90 90 90` → `66 66 66 66 66 90` (uma instrução de 6 bytes). `+22` (o `pc`) é fronteira perdida → `ThreadsInPatch` depois de 10 tentativas, bytes intactos, alvo não parado.
- `lost_boundary_at_syscall_pc_minus_2_is_refused`: `+15`, `BA 01 00 00 00 0F 05` → `0F 1F 80 00 00 00 00`; `+20` é fronteira perdida → `ThreadsInPatch`.
- `patch_keeping_syscall_boundaries_is_accepted`: `+22`, `90 90 90 90` → `66 90 66 90`; aplica e desfaz.
- `failed_write_mid_batch_rolls_back`: lote de 2 patches, o primeiro em `exec2`, o segundo em `ro_shared` (página `MAP_SHARED` somente leitura de um arquivo temporário: `process_vm_readv` lê, mas a escrita forçada falha porque não há `VM_MAYWRITE`); política crua com as duas páginas → `WriteRefused`; o primeiro patch voltou aos originais; diário `Missing`; alvo não parado. `test_support` ganha `readonly_shared_page(fill: u8) -> Result<u64>` (arquivo temporário aberto com `O_RDONLY`: com `O_RDWR` o mapeamento teria `VM_MAYWRITE` e a escrita forçada alteraria o arquivo).
- `undo_foreign_bytes_does_not_write`: `apply`; o teste escreve outros bytes no local com `MemWriter`; `undo_all` não escreve (os bytes do teste continuam), devolve `foreign == [id]` e `restored` vazio, o patch sai de `active()` e do diário.
- `bytes_changed_during_pause_refuses`: gancho `InPause` que escreve outros bytes no local (endereço num `static`) → `BytesMismatch`, nada escrito pelo motor, diário `Missing`, alvo não parado.
- `restore_entries_handles_mixed_bytes`: escreve à mão (com `MemWriter`) metade dos bytes novos em `exec2`; `restore_entries` com a entrada correspondente → `restored`, bytes originais.
- `stress_1000_toggles_on_spinning_loop` (alvo com `spin: true`): `spin_page`, `48 8B F9` ↔ `48 89 CF`, 1000 ciclos de `apply` + `undo_all`; no fim o alvo está vivo (não em `Z`) e `sample_pcs` (20 amostras) ainda vê o `pc` em `[spin_page, spin_page + 5)`.

- [ ] **Passo 2: Rodar e ver falhar.**
- [ ] **Passo 3: Implementar.**
- [ ] **Passo 4: Validar.**
- [ ] **Passo 5: Commit** — `feat(core): motor de patches com diário e conferência das threads`

---

### Tarefa 12: Recuperação, sessão e binário de apoio

Spec 6.7 (sinais, pânico), 6.8 (recuperação, ordem), 6.10 (laço do `run`, `toggle`).

**Arquivos:**
- Criar: `crates/gt-core/src/{recovery.rs,session.rs}` (feature `write`), `crates/gt-testkit/src/bin/gt-patch-harness.rs`, `crates/gt-testkit/tests/harness.rs`
- Modificar: `crates/gt-core/{Cargo.toml (dep opcional signal-hook; `write = ["dep:libc", "dep:signal-hook"]`),src/lib.rs}`, `docs/ARCHITECTURE.md` (5.4: conferência das threads, identidade, diário; árvore da seção 3)

**Interfaces:**
- Produz:
  - `pub struct RecoveryReport { pub resumed: bool, pub stopped_by_other: bool, pub restored: Vec<String>, pub dropped: Vec<String>, pub foreign: Vec<String>, pub discarded: Option<DiscardReason>, pub corrupt: Option<PathBuf> }` com `is_empty()`.
  - `pub fn recover(store: &JournalStore, lock: &JournalLock, cfg: &PauseConfig) -> Result<RecoveryReport>`: carrega; `Corrupt` → relatório; `decide` com `boot_id()`/`read_stat(j.pid)`; `Discard` → `clear`; `SameProcess` → `resume` envia SIGCONT por `Pidfd::open(j.pid)` (conferido com `start_time`); abre um `Engine::open(j.pid, store.clone(), lock, política = intervalos das entradas que caem inteiros em regiões `x` do `maps` atual e têm no máximo 64 bytes (entrada fora disso vira `Foreign`), version_key do diário)` — decisão do plano: a recuperação é genérica (não conhece o jogo) e só escreve onde cada byte é o original ou o nosso, por isso basta exigir região executável e chama `restore_entries(j.entries)`; limpa o diário. (`JournalStore` deriva `Clone`.)
  - `pub struct Shutdown`: `install() -> Result<Shutdown>` (chama `ignore_job_control()` — assim o harness e o `gt-patch` passam pelo mesmo código — e registra `signal_hook::flag::register` para SIGINT, SIGTERM, SIGHUP, SIGQUIT); `requested(&self) -> bool`; `#[doc(hidden)] pub fn manual() -> Shutdown` e `request(&self)` (para testes, sem sinais).
  - `pub enum RunEnd { Signal, ProcessGone }`.
  - Recuperação: depois do SIGCONT de `resume`, regrava o diário com `paused_by_us = false` antes de restaurar.
  - `pub fn run_until_shutdown(engine: &mut Engine, shutdown: &Shutdown, tick: Duration) -> Result<RunEnd>`: a cada `tick` (200 ms), `pidfd.exited(tick)` → `ProcessGone`; `shutdown.requested()` → `Signal`. Quem chama faz `undo_all` (o `Drop` cobre o resto); com `RunEnd::ProcessGone`, o `Err(ProcessGone)` do `undo_all` é esperado e engolido (sai com 0).
  - `pub struct ToggleReport { pub cycles: u32, pub danger_retries: u32, pub timeout_retries: u32, pub pause_avg: Duration, pub pause_max: Duration }`.
  - `pub fn toggle(engine: &mut Engine, patch: Validated, times: u32, interval: Duration, shutdown: &Shutdown) -> Result<ToggleReport>`: `times` vezes `apply` + `undo_all`, dormindo `interval` entre trocas; a pausa de cada troca vem do `BatchReport`; para no primeiro erro (o patch fica desligado) ou num sinal.
  - `gt-patch-harness`: `gt-patch-harness --journal-dir D [--pid P --allow START:LEN] [--hook panic-in-pause|kill-in-pause|sigint-in-pause|panic-after-apply] (run --addr A --expected HEX --replacement HEX | restore)`. `run`: `Shutdown::install`, lock, `recover`, `Engine::open`, `apply`, imprime `ativo` e faz `flush`, `run_until_shutdown`, `undo_all`, imprime `restaurado`, sai 0. `restore` (sem `--pid`/`--allow`): lock, `recover`, imprime o relatório, sai 0. Os ganchos usam `PauseConfig.hook` (`kill-in-pause` faz `raise(SIGKILL)`; `sigint-in-pause` faz `raise(SIGINT)`).

- [ ] **Passo 1: Testes que falham** (em `harness.rs`, com `TestTarget` e `bin_path("gt-patch-harness")`; patch `exec2`, `90 90 90` → `66 90 90`; helpers `wait_line(child, "ativo")`, `bytes_at(pid, addr, 3)`, `stopped(pid) -> bool`)

- `sigint_restores`, `sigterm_restores`, `sighup_restores`: `run`, espera `ativo`, envia o sinal → sai 0, bytes originais, diário `Missing`.
- `sigtstp_is_ignored`: `run`, `ativo`, envia SIGTSTP; 200 ms depois o harness não está em `T`; SIGINT → restaura, sai 0.
- `panic_after_apply_restores`: `--hook panic-after-apply` → código ≠ 0, bytes originais, alvo não parado.
- `panic_in_pause_resumes_and_leaves_bytes`: `--hook panic-in-pause` → código ≠ 0, alvo não parado, bytes originais, diário `Missing`.
- `sigint_during_pause_restores`: `--hook sigint-in-pause` → sai 0, bytes originais, alvo não parado.
- `sigkill_then_restore`: `run`, `ativo`, `SIGKILL` no harness → bytes = novos, diário com a entrada; `restore` → bytes originais, diário `Missing`.
- `sigkill_in_pause_then_restore_resumes`: `--hook kill-in-pause` → alvo parado; `restore` → alvo não parado, bytes originais, diário `Missing`.
- `toggle_cycles_and_ends_off` (em `engine_live.rs`, chamando `session::toggle` direto): `exec2`, 20 ciclos, intervalo 0, `Shutdown::manual()` → `cycles == 20`, bytes originais no fim, `pause_max >= pause_avg`; com `request()` antes → para sem erro com `cycles == 0` e bytes originais.
- `second_instance_is_refused`: com um `run` vivo, outro `run` → código ≠ 0 e stderr com "outro gt-patch já está em execução".
- `run_ends_when_target_exits`: `run`, `ativo`, mata e colhe o alvo → harness sai 0, diário `Missing`.
- `restore_discards_dead_process_journal`: grava um diário com o PID de um processo já colhido → `restore` sai 0, diário `Missing`.

- [ ] **Passo 2: Rodar e ver falhar.**
- [ ] **Passo 3: Implementar.**
- [ ] **Passo 4: Validar.**
- [ ] **Passo 5: Commit** — `feat(core): recuperação do diário, sinais e laço de execução`

---

### Tarefa 13: Lado do jogo: assinaturas, estado, política e catálogo

> Pode ser executada em duas entregas, cada uma com o próprio ciclo de teste, revisão e commit: **13a** (`Pattern::token`, `signatures.rs`, `state.rs` e os testes deles) e **13b** (`policy.rs`, `catalog.rs`, `pe::functions`, `profile.rs`, `patching.rs` e os testes deles; os TOML reais e a seção 5.3 do `ARCHITECTURE.md` entram na 13a).

Spec 6.3, 6.4, 6.6 (cálculo do jogo), `.pdata` da 6.9/6.10.

**Arquivos:**
- Criar: `crates/gt-grounded2/src/{signatures.rs,state.rs,policy.rs,catalog.rs,profile.rs,patching.rs}`, `crates/gt-grounded2/signatures/{generic.toml,c935e799-0b27c000.toml}`
- Modificar: `crates/gt-grounded2/{Cargo.toml (deps serde, toml),src/lib.rs,src/error.rs}`, `crates/gt-core/src/pe.rs` (`.pdata`), `crates/gt-core/src/scan.rs` (acessor `pub fn token(&self, i: usize) -> Option<Option<u8>>`: `None` fora do padrão, `Some(None)` curinga, `Some(Some(b))` fixo), `docs/ARCHITECTURE.md` (5.3: formato das assinaturas)

**Interfaces:**
- Produz:
  - Formato TOML:
    ```toml
    # generic.toml
    [[patch]]
    id = "teste-codificacao"
    description = "Troca sem efeito (codificação equivalente), para testar o motor"
    module = "Grounded2Steam-Win64-Shipping.exe"   # opcional; padrão GAME_EXE
    pattern = "48 8B F9 ..."
    offset = 0
    expected = "48 8B F9"
    replacement = "48 89 CF"

    # <chave>.toml
    version = "c935e799-0b27c000"
    product_version = "Augusta-CL-3207493"
    [[patch]]
    id = "teste-codificacao"
    rva = 0x0
    # opcionais: pattern, offset, expected, replacement
    ```
    Nesta tarefa os arquivos reais ficam sem patches (`generic.toml` só com comentário; `c935e799-0b27c000.toml` com `version` e `product_version`); o `teste-codificacao` entra na Tarefa 15.
  - `pub struct PatchDef { pub id: String, pub description: String, pub module: String, pub pattern: Pattern, pub offset: usize, pub expected: Vec<u8>, pub replacement: Vec<u8>, pub rva: Option<u64> }`.
  - `pub struct Signatures`: `parse(files: &[(&str, &str)]) -> Result<Signatures>` (nome do arquivo + conteúdo; `generic.toml` obrigatório; demais são `<chave>.toml` e o `version` interno precisa bater com o nome), `embedded() -> Result<Signatures>` (lista `include_str!` dos arquivos de `signatures/`), `has_version(&self, key: &str) -> bool`, `for_version(&self, key: &str) -> Vec<PatchDef>` (aplica as sobrescritas e o `rva` daquela versão), `ids(&self) -> Vec<&str>`.
  - Regra de carga (spec 6.3) e erros: `Error::Signature { file: String, field: String, reason: String }` ("assinatura inválida em {file}, campo {field}: {reason}"). Cobre: TOML inválido, `module` diferente do exe do jogo (só ele nesta fase, spec 6.6), `id` repetido, `id` da versão sem definição genérica, hex inválido, `expected`/`replacement` de tamanhos diferentes, trecho fora do `pattern`, byte fixo do `pattern` diferente de `expected`.
  - `state.rs`: `pub enum PatchState { Verified { addr: u64 }, Unverified { addr: u64 }, Active { addr: u64 }, Pending { addr: u64 }, BytesDiffer { addr: u64, found: Vec<u8> }, Misplaced { found_rva: u64 }, NotFound, Ambiguous(usize), Inconclusive, DiskUnreadable(String) }` com `label(&self) -> &'static str` (textos exatos da spec 6.4), `apply_addr(&self) -> Option<u64>` (só `Verified`/`Unverified`) e `is_unverified(&self) -> bool`.
  - `pub struct StateInputs<'a> { pub journal_hit: Option<(u64, bool)>, pub disk: std::result::Result<(), String>, pub memory: &'a MemScan, pub disk_hits: Option<&'a [u64]>, pub module_base: u64, pub current: Option<&'a [u8]> }` (`journal_hit` = endereço + trava ocupada, já filtrado por mesmo processo e bytes = novos; `disk_hits` = ocorrências do padrão no disco, em endereços da base atual; `current` = bytes em ocorrência + `offset`).
  - `pub fn compute_state(def: &PatchDef, inp: &StateInputs) -> PatchState`, nesta ordem: diário → `Active`/`Pending`; disco ilegível → `DiskUnreadable`; veredito da memória `Many(n)` → `Ambiguous(n)`, inconclusivo → `Inconclusive`, `None` → `NotFound`; `Unique(hit)`: `addr = hit + offset`; com `rva` → `addr - base != rva` dá `Misplaced`; sem `rva` → `disk_hits` precisa ser exatamente `[hit]` (0 → `NotFound`; ≥2 → `Ambiguous(n)`; 1 em outro lugar → `NotFound`); então `current == expected` → `Verified`/`Unverified`, senão `BytesDiffer`.
  - `policy.rs`: `pub fn exec_ranges(module: &Module, disk: &PeHeaders) -> Vec<Range<u64>>` (seções executáveis do disco, cortadas na extensão do módulo); `pub fn write_policy(module: &Module, disk: &PeHeaders, maps: &[MapRegion]) -> WritePolicy` (`exec_ranges` ∩ regiões `x` do `maps`; módulo não confiável → política vazia).
  - `CatalogScan` também carrega `module: Module` e `disk: Option<PeHeaders>` (o cabeçalho do disco já lido), usados pelo `open_engine` na Tarefa 14. O cabeçalho do disco passa por `check_same_version` contra o da memória; divergência → todos os patches `DiskUnreadable("versão do disco difere da memória")`.
  - `catalog.rs`, parte pura: `pub fn resolve(defs: &[PatchDef], module_base: u64, mem: &[MemScan], disk: std::result::Result<Option<Vec<Vec<u64>>>, String>, current: impl Fn(u64, usize) -> Option<Vec<u8>>, journal: Option<(&Journal, &Identity)>, locked: bool) -> Vec<Entry>` (um `MemScan` e uma lista de ocorrências em disco por definição, na mesma ordem; o diário só conta se `pid`, `start_time` e `boot_id` batem com a `Identity` do alvo e os bytes no endereço registrado são os novos). `scan` é o invólucro que lê do processo e chama `resolve`.
  - `pub struct Entry { pub def: PatchDef, pub state: PatchState }`; `pub fn scan(target: &Target, sigs: &Signatures, journal: Option<&Journal>, locked: bool) -> Result<CatalogScan>` com `CatalogScan { pub version_key: String, pub has_version_file: bool, pub module_name: String, pub entries: Vec<Entry> }`. Lê o cabeçalho do disco do módulo (`open_in_root`); falha → todos `DiskUnreadable`. `guard::check` em cada `exec_range`, depois **uma** `scan_memory` com todos os padrões. Disco completo (`Target::open_disk`) só se algum patch não tem `rva`. Diário só conta se `pid`, `start_time` e `boot_id` batem com o alvo.
  - `pub fn select(scan: &CatalogScan, ids: &[String]) -> Result<Vec<(PatchDef, u64, bool)>>` (definição, endereço, não verificado): qualquer `id` desconhecido → `Error::UnknownPatch { id }` ("patch desconhecido: {id}"); qualquer estado sem `apply_addr` → `Error::Unavailable { id, state }` ("patch {id} indisponível: {state}"); nada é devolvido parcialmente.
  - `gt_core::pe::DiskImage::functions(&self) -> Result<Vec<Range<u32>>>` (`parse_headers` passa a guardar também o diretório de exceção, como já faz com o de realocação; entradas `RUNTIME_FUNCTION` de 12 bytes; ordenadas).
  - `profile.rs`: `pub struct FunctionHits { pub start_rva: u32, pub end_rva: u32, pub pcs: Vec<u64> }`; `pub fn aggregate(pcs: &[u64], base: u64, functions: &[Range<u32>]) -> Vec<FunctionHits>` (por função, mais amostradas primeiro; `pc` fora de funções é ignorado).
  - `patching.rs` (feature `write`): `pub fn open_engine(target: &Target, store: JournalStore, lock: &JournalLock, module: &Module, disk: PeHeaders) -> Result<Engine>` (quem chama guarda o `JournalLock` até depois do `drop` do `Engine`) (política = `write_policy` recalculada a cada lote com o `maps` novo).

- [ ] **Passo 1: Testes que falham**

`signatures.rs`: `parses_generic_and_version_with_override`; `load_rule_errors` (um caso por regra, conferindo `file` e `field` no erro, inclusive a regra de carga violada só depois da sobrescrita do `<chave>.toml`); `embedded_files_load` (`Signatures::embedded()` ok e `has_version("c935e799-0b27c000")`).
`state.rs`, `compute_state` com entradas sintéticas, um teste por estado da tabela: `state_verified`, `state_unverified_needs_unique_disk_at_same_offset`, `state_unverified_disk_elsewhere_is_not_found`, `state_unverified_disk_many_is_ambiguous`, `state_active_and_pending_from_journal_win_over_scan`, `state_bytes_differ_with_wildcards`, `state_misplaced`, `state_not_found`, `state_ambiguous`, `state_inconclusive`, `state_disk_unreadable`; `labels_match_spec` (os 10 textos).
`policy.rs`: `exec_section_allowed_data_refused`, `range_crossing_section_end_refused`, `untrusted_module_has_empty_policy`, `policy_intersects_maps` (seção executável cuja região do `maps` é `r--` → recusada).
`catalog.rs`: `resolve_chain_from_toml` (TOML sintético → `Signatures::parse` → `for_version` → `resolve` com buffers sintéticos de memória/disco → `Verified` no endereço certo); `resolve_ignores_journal_of_other_process` (diário com outro `start_time` → estado pela busca, não `Pending`); `select_refuses_if_any_unavailable` (dois ids, um `NotFound` → `Unavailable`, nada devolvido); `select_unknown_id`.
`pe.rs`: `functions_from_synthetic_pdata`. `profile.rs`: `aggregate_counts_by_function` (5 `pc` em 2 funções + 1 fora → 2 entradas, a maior primeiro).

- [ ] **Passo 2: Rodar e ver falhar.**
- [ ] **Passo 3: Implementar.**
- [ ] **Passo 4: Validar** — inclusive `cargo build -p gt-cli` isolado (o `gt-cli` não ativa `gt-grounded2/write`).
- [ ] **Passo 5: Commit** — `feat(grounded2): assinaturas, estado dos patches e política de escrita`

---

### Tarefa 14: Binário `gt-patch`

Spec 6.10 e 6.11.

**Arquivos:**
- Criar: `apps/gt-patch/{Cargo.toml,src/main.rs,src/cmd/{mod.rs,list.rs,run.rs,toggle.rs,restore.rs,profile.rs}}`, `apps/gt-patch/tests/{cli.rs,game.rs}`
- Modificar: `docs/ARCHITECTURE.md` (seção 3: `gt-patch`), `apps/gt-cli/src/main.rs` e `crates/gt-grounded2/src/{resolve.rs,lib.rs}` (extração do `permission_hint`)

**Interfaces:**
- Consome: `Target::open` (T3), `Signatures::embedded`, `catalog::scan`/`select`, `patching::open_engine`, `profile::aggregate` (T13), `JournalStore`, `recover`, `Shutdown`, `run_until_shutdown`, `toggle`, `sample_pcs`, `validate` (T6–T12).
- `apps/gt-patch/Cargo.toml`: `[[bin]] name = "gt-patch"`; deps `gt-core` (`write`), `gt-grounded2` (`write`), `anyhow`, `clap`, `tracing`, `tracing-subscriber` (`env-filter`); dev-deps `gt-testkit`, `tempfile`, `serde_json`.
- CLI (clap): opção global `--pid`; subcomandos `list`, `run <ID>...`, `toggle <ID> --times N [--interval MS]` (padrão 50), `restore`, `profile [--samples N] [--interval MS]` (padrões 200 e 300).
- `profile`: `sample_pcs` com `should_stop` = `shutdown.requested()` (Ctrl+C imprime o parcial e sai 0) e `before_pause(will_stop)` = `store.save(Journal { paused_by_us: will_stop, entries: vec![], .. })` (identidade do jogo) e `after_resume` = `store.clear()`; as funções vêm de `DiskImage::functions()` da imagem em disco (`Target::open_disk`).
- Início do `main`, para todos os comandos: `ignore_job_control()` (spec 6.5); os comandos que escrevem ainda passam por `Shutdown::install()`. O `JournalLock` vive no `main` até depois do `drop` do `Engine`. O `profile` abre o alvo com `open_checked` (Tarefa 7).
- Ordem (spec 6.8): `run`/`toggle`/`restore`/`profile` fazem `Shutdown::install()` → `store.lock()` → `recover()` (imprime o relatório se não vazio) → (exceto `restore`) `Target::open`. `list` não pega a trava nem escreve: `store.peek()` + `store.is_locked()`.
- `catalog::scan` e `select` da Tarefa 13; um patch `não encontrado` no erro `Unavailable` ganha a dica "confira com `gt scan --disk`; outro trainer pode ter alterado o local".
- Textos:
  - `list`: `Chave de versão: <chave>`; `Assinaturas: conferidas para esta versão` ou `Assinaturas: versão sem arquivo (patches marcados como não verificados)`; uma linha por patch `<id>  <estado>  <módulo+offset ou ->`; `Diário pendente: sim (rode gt-patch restore)` quando houver diário do **mesmo jogo** (vivo) com a trava livre.
  - aplicar não verificado: `aviso: a versão do jogo não tem assinaturas conferidas; <id> foi aplicado como não verificado`.
  - `run`: `ativo: <id> em <módulo+offset>` por patch; ao sair, `restaurado: <id>` para cada id em `BatchReport.restored` do `undo_all` e `não restaurado: <id> (bytes alterados por outro)` para cada id em `foreign`; jogo fechou → `o jogo fechou; nada a restaurar` e código 0.
  - `toggle`: `ciclos: N`, `novas tentativas: X (threads no trecho) + Y (prazo)`, `pausa média: A ms, máxima: B ms`; máxima ≥ 50 ms → `aviso: pausa máxima acima de 50 ms`.
  - `restore`: sem diário → `nada a restaurar`; senão as linhas do relatório (`retomado o jogo parado`, `restaurado: id`, `descartado: id (bytes já originais)`, `não restaurado: id (bytes alterados por outro)`, `diário descartado: o jogo fechou/reiniciou`, `diário corrompido movido para <caminho>`, `o jogo estava parado por outro processo; não foi retomado`).
  - Desfazer que falha no fim do `run`/`toggle`: `não foi possível restaurar <id> agora; a próxima execução do gt-patch vai restaurar` (spec 6.7), código ≠ 0.
  - `profile`: as 20 funções mais amostradas: `<módulo+offset início>-<fim>  <n> amostras`, seguidas dos `pc` (como `módulo+offset`).
  - Erros: `report()` igual ao do `gt` (erro, causas e dica), com dicas para `ProcessGone` ("o jogo fechou"), `AlreadyRunning`, `PermissionDenied` (mesma dica de `ptrace_scope` do `gt`; extrair `permission_hint` para `gt-grounded2::resolve` e usar nos dois binários), `WriteRefused`.

- [ ] **Passo 1: Testes que falham** (em `tests/cli.rs`; cada teste define `XDG_STATE_HOME` num `tempfile`)

- `list_without_game_says_so` (pular com aviso se o jogo estiver aberto, como no `gt`): código ≠ 0, stderr com "jogo não encontrado".
- `restore_without_journal`: código 0, stdout "nada a restaurar".
- `restore_discards_dead_process_journal`: diário com PID colhido → código 0, stdout com "diário descartado", arquivo removido.
- `restore_moves_corrupt_journal`: `journal.json` = `"{"` → código 0, `journal.json.bad` existe, stdout com "corrompido".
- `toggle_and_profile_without_game_say_so` (pular se o jogo estiver aberto): `toggle x --times 1` e `profile --samples 1` → código ≠ 0, stderr com "jogo não encontrado", e nenhum `journal.json` criado (o `journal.lock` pode existir).
- `restore_refused_while_locked`: segura a trava com `JournalStore::lock()` no teste → código ≠ 0, stderr com "outro gt-patch já está em execução".
- Em `main.rs`: `toggle_report_text` (função de formatação: 100 ciclos, 0+0, máxima 60 ms → contém "ciclos: 100" e o aviso de 50 ms).

`tests/game.rs` (`#[ignore]`, só roda com a chave `c935e799-0b27c000`, senão pula com aviso): `list_shows_test_patch_verified` (stdout contém `teste-codificacao` e `disponível (verificado)`); `single_toggle_round_trip` (`toggle teste-codificacao --times 1` sai 0; `bin_path("gt")` `diff` depois não mostra alteração no endereço do patch).

- [ ] **Passo 2: Rodar e ver falhar.**
- [ ] **Passo 3: Implementar.**
- [ ] **Passo 4: Validar.**
- [ ] **Passo 5: Commit** — `feat(patch): binário gt-patch`

---

### Tarefa 15: Sessão B com o jogo: patch de teste e critério de pronto

> **Sessão principal**, não em subagente (dono + engenharia reversa).

Spec 6.9, seção 9 e seção 10. **Precisa do dono.**

**Arquivos:**
- Modificar: `crates/gt-grounded2/signatures/{generic.toml,c935e799-0b27c000.toml}` (ou a chave da build atual), `docs/RESEARCH.md`, `docs/ROADMAP.md`, `docs/ARCHITECTURE.md` (seção 8: decisões da spec 002)

- [ ] **Passo 1: Pedir ao dono:** "faça backup do save; abra o Grounded 2 num save de teste, com o FLiNG **fechado** (se ele estava aberto, reinicie o jogo) e me avise quando estiver na partida, andando pelo mapa".

- [ ] **Passo 2: Perfil**

Run: `cargo run -p gt-patch -- profile` (60 s). Escolher, entre as funções com ≥ 5 amostras, uma instrução com codificação equivalente do mesmo tamanho (ex.: `mov r64, r64` com `8B`↔`89`), no mesmo bloco básico de uma amostra (conferir com `gt disasm` da função inteira: nenhum desvio nem alvo de desvio entre a amostra e a instrução), fora dos 5 locais do FLiNG (`RESEARCH.md`). Montar um padrão AOB com contexto até ser **único** com `gt scan` e `gt scan --disk`. Se nada servir, repetir com `--samples 1000`; se ainda não houver, parar e levar ao dono o que foi achado.

- [ ] **Passo 3: Registrar a assinatura** — `generic.toml` (`teste-codificacao`) e `c935e799-0b27c000.toml` (`rva`). Rodar `cargo test -p gt-grounded2 embedded_files_load` e recompilar `gt-patch`.

- [ ] **Passo 4: Critério de pronto** (spec 9, itens 1–5), na ordem, mostrando cada saída ao dono:
  1. `gt-patch list` → `teste-codificacao  disponível (verificado)`.
  2. `gt-patch toggle teste-codificacao --times 100` → sai 0, `ciclos: 100`, `novas tentativas: 0 + 0`; pedir ao dono para andar pelo mapa por um minuto e dizer se o jogo engasgou ou travou.
  3. `gt-patch run teste-codificacao` (em segundo plano), Ctrl+C (`kill -INT`) → `gt diff` sem alteração; repetir com `kill <pid>`.
  4. `gt-patch run teste-codificacao`, `kill -9 <pid>` → `gt diff` mostra o patch; `gt-patch restore` → `gt diff` sem alteração.
  5. `gt-patch run teste-codificacao` e pedir ao dono para fechar o jogo → o `gt-patch` imprime "o jogo fechou" e sai 0.
  Antes do item 5, rodar `cargo build --workspace && cargo test -p gt-patch --test game -- --ignored`.

- [ ] **Passo 4b:** Se algum item falhar: parar, investigar (superpowers:systematic-debugging) e não marcar a fase como pronta.

- [ ] **Passo 5: Documentos** — `RESEARCH.md`: perfil (contagens), patch escolhido (versão, `módulo+offset`, bytes, desmontagem, como foi achado), pausa média/máxima do `toggle`. `ROADMAP.md`: Fase 2 ✅ com data e spec/plano `002-motor-patches`; critério "matar o trainer restaura" reescrito como na spec 10. `ARCHITECTURE.md` 8: decisões da spec 002 (tabela da spec 11, linhas do dono e as principais da IA).

- [ ] **Passo 6: Validação final** — validação global.

- [ ] **Passo 7: Commit** — `feat: patch de teste e fechamento da fase 2`
