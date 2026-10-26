# Plano 003 — Interface TUI

> **Para agentes executores:** SUB-SKILL OBRIGATÓRIA: use superpowers:subagent-driven-development (recomendado) ou superpowers:executing-plans para executar este plano tarefa a tarefa. Os passos usam checkbox (`- [ ]`).

**Objetivo:** binário `gt-tui` com o motor de patches no próprio processo: lista de cheats por categoria, liga/desliga e troca de valor pelo teclado, conexão e reconexão automáticas com religação dos desejados, restauração garantida; mais as mudanças do motor (SIGCONT que falha, restos, troca de valor), o modelo de valores e as pendências da Fase 2.

**Arquitetura:** o motor (`gt-core`) ganha três coisas: o estado `stuck` persistente com nova tentativa de SIGCONT, a consulta e a restauração de restos, e a troca dos bytes de um patch ativo. O modelo de valores (grade e codificação) é genérico no `gt-core`, e as assinaturas do `gt-grounded2` o usam. O `gt-tui` é biblioteca + binário: um **modelo** puro (estado da tela), um **controlador** que conversa com o jogo por uma **interface de alvo** (`Backend`/`Session`), com uma implementação do jogo e outra crua para testes contra o `gt-testtarget`, uma **tela** (`ratatui`) e um **terminal** abstrato com implementação falsa para testes. O motor fica sempre na thread principal; a busca dos padrões roda numa thread separada.

**Tecnologias:** Rust 1.99 stable (edition 2024), `ratatui` 0.30, `crossterm` 0.29, `nix` (feature nova `term` para `openpty` nos testes), `iced-x86`, `serde`/`toml`, `tracing`/`tracing-subscriber` (`fmt` em arquivo), `clap`, `anyhow`/`thiserror`.

**Spec:** `docs/specs/003-interface-tui.md` (citada como "spec 6.x"); a spec 002 é citada como "spec 002 6.x". Quem executa lê a spec e este plano.

## Restrições globais

- Regras do `AGENTS.md`: identificadores em inglês; comentários, docs e mensagens em pt-BR; `unsafe` só em `crates/gt-core/src/mem/` com `// SAFETY:`; `thiserror` nas libs e `anyhow` nos binários; sem `unwrap()`/`expect()` fora de testes; `tracing`, nunca `println!` em libs.
- O `cargo` está em `~/.cargo/bin` (`export PATH=$HOME/.cargo/bin:$PATH` se preciso).
- Validação de toda tarefa: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo build -p gt-cli` e `cargo clippy -p gt-cli -- -D warnings` isolados, e `cargo tree -p gt-cli -e normal,build,features -i gt-core` sem `write` nem `test-support`.
- Os testes com processo vivo encontram binários por `gt_testkit::bin_path(nome)`; os do `gt-tui` encontram o próprio binário por `env!("CARGO_BIN_EXE_gt-tui")`.
- Os testes que tocam o diário apontam `XDG_STATE_HOME` (ou o diretório passado à API) para um `tempfile::TempDir`. Testes de binário sem jogo pulam quando o jogo está aberto, com o mesmo critério de `apps/gt-patch/tests/cli.rs` (`game_is_open()`; copiar a função).
- Nenhum endereço fixo do jogo no código de produção.
- `panic = "unwind"` em todos os perfis. Nenhum `Drop` pode entrar em pânico.
- Os ganchos de teste do motor ficam em campos `#[doc(hidden)]` do `PauseConfig` (como o `hook` atual), nunca em flags do binário `gt-tui`.
- Valores fixados por este plano (spec 12): procura do jogo a cada **2 s**; novas buscas automáticas a cada `max(5 s, 2 × duração da última busca)`, por até **120 s** depois de cada conexão (o `r` reinicia o prazo); intervalo mínimo entre o início de duas trocas de valor **150 ms**; laço da TUI acorda a cada **100 ms** (limite de resposta da spec 6.10: 250 ms); terminal mínimo **60×15**; registro de eventos guarda as **200** últimas linhas e mostra as que couberem (mínimo 3).
- Log: `$XDG_STATE_HOME/grounded-trainer/gt-tui.log` (rotação para `gt-tui.log.1`), `RUST_LOG` com padrão `info`.
- Textos exatos (testes procuram estes trechos):
  - estados de conexão: `aguardando o jogo`, `sem acesso`, `conectando`, `conectado`, `vários processos`, `erro`, `o processo terminou`;
  - linhas da lista: `[ON ]`, `[OFF]`, `?` (não verificado), `↻` (religar pendente), `indisponível (<motivo>)` com os rótulos da spec 002 6.4 e mais `jogo não conectado` e `alterado por outro`, e `erro` para restos;
  - lista vazia: `nenhum cheat disponível nesta versão do trainer (use --diagnostico para o patch de teste)`;
  - terminal pequeno: `aumente o terminal`;
  - trava: `outro gt-patch ou gt-tui já está em execução` e a dica `feche o outro programa antes`;
  - sem terminal: `o gt-tui precisa de um terminal`;
  - `--pid` terminado: `reinicie o gt-tui sem --pid, ou com o PID novo`;
  - SIGCONT: `o jogo pode ter ficado parado: kill -CONT <pid>`;
  - patch não restaurado na saída: `não foi possível restaurar <id> agora; a próxima execução do gt-tui (ou gt-patch restore) vai restaurar`;
  - versão nova: `versão nova; aperte espaço para ligar`;
  - diário corrompido: `diário corrompido movido para <caminho>; o jogo pode ter patches sem registro, confira com gt diff`;
  - operações no topo: `recuperando o diário…`, `procurando padrões`, `ligando <nome>…`, `desligando <nome>…`, `trocando o valor de <nome>…`, `religando…`, `restaurando…`.
- Git: branch `fase-3-tui`. O dono autorizou em 2026-10-25 seguir direto da spec para o plano e a execução por subagent-driven-development, com um commit ao fim de cada tarefa (Conventional Commits em pt-BR, terminando com `Claude-Session: https://claude.ai/code/session_01Psp6AHcK8pD1eVRsZut1e4`). Os subagentes executores **devem** fazer o commit do fim da tarefa. Push, merge e rebase só com pedido do dono.
- **A Tarefa 17 roda na sessão principal**, com o dono: pedir o **backup do save** antes; nunca `sudo` nem mudar `ptrace_scope`.
- **Despacho:** cada subagente recebe esta seção "Restrições globais" inteira junto com a tarefa (constantes e textos exatos valem para todas), as seções da spec citadas no cabeçalho da tarefa (e as da spec 002 citadas), e a autorização de commit acima, que prevalece sobre o `AGENTS.md` 8 neste plano.
- Testes de integração novos começam com `#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]` (o `game_is_open()` copiado usa `eprintln!`).
- Cada módulo novo do `gt-tui` entra no `apps/gt-tui/src/lib.rs`. Itens públicos que os testes de integração chamam (`tick`, `handle`, `shutdown`, `search_panicked`, `wait_search`, `note_op`) são `pub`. Item ainda sem uso fora de testes leva `#[allow(dead_code)]` até a tarefa que o usa.
- Rodar um teste por nome: `cargo test -p <crate> --test <arquivo> -- <filtro>` (um filtro por execução, ou nenhum). Antes de testes do `gt-tui` ou do `gt-testkit`, `cargo build --workspace` (o `bin_path` precisa do `gt-testtarget`).
- Hora do registro de eventos: crate novo `chrono` (`default-features = false`, `features = ["clock"]`), na versão estável mais recente (consultar com `cargo info chrono`; exige rede, como `ratatui`/`crossterm` na primeira compilação), fixada no `[workspace.dependencies]` no mesmo estilo dos outros pins e registrada no `ARCHITECTURE.md` 4.
- Aviso de não verificado no registro de eventos: `aviso: a versão do jogo não tem assinaturas conferidas; <nome> foi ligado como não verificado`.
- Resumo de saída do `gt-tui` vai para o `stdout` (o `stderr` só recebe erros de início: trava ocupada e falta de terminal).

## Foco de revisão

Entradas e falhas que a spec implica e que são fáceis de esquecer. Cada uma tem teste na tarefa dona:

1. **O jogo fecha durante a busca em thread** e reabre antes de ela terminar. Esperado: o resultado é descartado pelo número da conexão; nada é aplicado com endereços do processo antigo. Teste `stale_search_result_is_dropped` (Tarefa 11).
2. **Seta segurada com o cheat ligado e o jogo fechando no meio.** Esperado: a troca termina como "o jogo fechou", o valor mostrado fica guardado, nenhuma troca pendente vai para o processo novo antes de religar. Teste `value_queue_survives_game_close` (Tarefa 14).
3. **`Drop` do motor com `stuck` e nada ativo.** Esperado: tenta o SIGCONT, e com a falha persistente o marcador continua no diário. Teste `drop_with_stuck_and_nothing_active_keeps_marker` (Tarefa 1).
4. **Diário da Fase 2 (sem o campo `next`).** Esperado: carrega, recupera e o `list` mostra `ativo`/`pendente`. Teste `phase2_journal_still_loads` (Tarefa 3).
5. **Terminal fechado (SIGHUP) com a escrita do resumo falhando por `EIO`.** Esperado: restauração completa, sem pânico, código pelo diário. Teste `sighup_with_closed_pty_restores` (Tarefa 16).

---

## Mapa de arquivos

```
Cargo.toml                          chrono no [workspace.dependencies]
crates/gt-core/
  src/suspend.rs                    PauseConfig: hook_on_pause, fault (ganchos de teste)
  src/engine.rs                     stuck persistente, retry_resume, pending/restore_pending,
                                    set_replacement, discard_gone, ganchos de falha
  src/journal.rs                    JournalEntry::next, JournalEntry::range, decide_entry com next
  src/recovery.rs                   usa JournalEntry::range (sai o entry_range duplicado)
  src/value.rs                      NOVO: ValueType, ValueSpec, grade, codificação
  src/asm.rs                        immediate_slot (conferência do slot com iced-x86)
  src/modules.rs                    avisos desduplicados (WarnOnce)
  src/error.rs                      AlreadyRunning com texto novo
  src/mem/test_support.rs           value_code (laço com mov eax, imm32)
crates/gt-testkit/
  src/bin/gt-testtarget.rs          página value_page + palavra value_word; --spin-value
  src/lib.rs                        TestTarget.value_page/value_word
  src/bin/gt-patch-harness.rs       --set-value, --hook-pause N, --fault
  tests/engine_live.rs              testes novos do motor
  tests/harness.rs                  SIGKILL na troca, SIGCONT persistente
crates/gt-grounded2/
  signatures/generic.toml           name, category no teste-codificacao
  src/signatures.rs                 name, category, value
  src/catalog.rs                    scan_subset; journal_hit com next
  src/state.rs                      sem mudança de tipos (journal_hit já cobre)
  src/resolve.rs                    permission_hint(program, scope); find_pid público
apps/gt-patch/src/                  dica da trava, permission_hint, profile com identidade
apps/gt-cli/src/main.rs             permission_hint("gt", …)
apps/gt-tui/
  Cargo.toml                        lib + bin; features: test-backend
  src/lib.rs
  src/model.rs                      Model, CheatRow, CheatStatus, Conn, eventos, seleção e rolagem
  src/keys.rs                       KeyEvent → Action
  src/ui.rs                         render(&Model, &mut Frame)
  src/backend.rs                    traits Backend e Session, tipos trocados com o controlador,
                                    recovery_outcome, TuiError
  src/fake.rs                       #[cfg(test)] FakeBackend/FakeSession (Tarefa 11)
  src/app.rs                        laço e saída testáveis (Tarefa 15)
  src/controller.rs                 Controller<B>: conexão e buscas (T11), desejados (T12),
                                    restos e r (T13), valores e saída (T14)
  src/game.rs                       GameBackend (gt-grounded2)
  src/raw.rs                        RawBackend (feature test-backend): patches crus, só gt-core
  src/term.rs                       trait Term, CrosstermTerm, restauração única, gancho de pânico
  src/logfile.rs                    rotação e abertura do log
  src/exit.rs                       resumo e código de saída pela releitura do diário
  src/main.rs                       ordem de início da spec 6.1, laço e saída
  tests/core_raw.rs                 controlador + RawBackend contra o gt-testtarget
  tests/fd_leak.rs                  um teste só: 50 reconexões sem vazar descritores
  tests/panic_hook.rs               um teste só: gancho de pânico (estado global)
  tests/game.rs, tests/game_fd.rs   #[ignore], com o jogo
  tests/bin.rs                      binário em pseudo-terminal
docs/                               ARCHITECTURE, ROADMAP, RESEARCH, spec 002 (Tarefas 16 e 17)
```

---

### Tarefa 1: SIGCONT que falha: `stuck` persistente e nova tentativa

Spec 7 item 1, 6.8. Corrigir primeiro.

**Arquivos:**
- Modificar: `crates/gt-core/src/suspend.rs` (`PauseConfig`), `crates/gt-core/src/engine.rs`
- Teste: `crates/gt-core/src/engine.rs` (`mod tests`), `crates/gt-testkit/tests/engine_live.rs`

**Interfaces:**
- Produz:
  - `PauseConfig { …, #[doc(hidden)] pub fault: Option<Fault> }` e `#[doc(hidden)] pub enum Fault { ContFailsOnce, ContFailsAlways, WriteAndRollbackFail }` (o terceiro é usado na Tarefa 2) em `suspend.rs`; `Default` com `fault: None`.
  - `Engine::is_stuck(&self) -> bool`.
  - `#[doc(hidden)] Engine::force_stuck_for_test(&mut self)` (marca `stuck` e grava o diário com o marcador, sem entradas) e `PauseConfig { …, #[doc(hidden)] pub resume_counter: Option<Arc<AtomicU32>> }` (incrementado a cada tentativa de SIGCONT da `retry_resume`, inclusive as que falham pela injeção).
  - Auxiliar de teste em `engine_live.rs`: `open_engine_with(t, env, cfg: PauseConfig)`, que recebe o `PauseConfig` completo (o `open_engine` atual só aceita o `hook`).
  - `Engine::retry_resume(&mut self) -> Result<()>`: sem `stuck`, devolve `Ok(())` sem enviar sinal; com `stuck`, envia SIGCONT pelo `pidfd`; sucesso limpa o `stuck` e regrava o diário sem o marcador (apagando o arquivo se não houver entradas); `ProcessGone` passa pelo `finish` (registro e diário limpos, inclusive quando chamado pelo `Drop`); outra falha mantém o `stuck` e devolve o erro.

- [x] **Passo 1: testes que falham.** Em `engine_live.rs`, contra `TestTarget::spawn()` e a página `exec2`, com os auxiliares existentes do arquivo (`env()`, `open_engine(...)`, `patch(id, addr, expected, replacement)` e o patch `NOP3` → `NOP2_NOP` de `apply_and_undo_on_exec_page`):
  - `cont_fails_once_then_next_op_resumes`: `fault = ContFailsOnce`; `apply` devolve erro e `is_stuck()`; o alvo está parado (`is_stopped_now`); o diário tem `paused_by_us = true`; como a escrita aconteceu, a entrada fica pendente (`pending_ids() == [id]`, Tarefa 2; nesta tarefa, conferir pelo diário); a operação seguinte (`restore_entries` com as entradas do diário) retoma o alvo (não parado), `!is_stuck()`, restaura os bytes e o diário termina sem marcador.
  - `cont_fails_always_keeps_marker_after_undo_and_drop`: `fault = ContFailsAlways`; depois do `apply` com erro, `undo_all` devolve erro sem escrever e o diário continua com `paused_by_us = true`; depois do `drop(engine)`, o diário continua com o marcador; `recover` (com `fault: None`) retoma o alvo (`report.resumed`).
  - `drop_with_stuck_and_nothing_active_keeps_marker`: `ContFailsAlways` e `resume_counter` ligado; motor sem nenhum patch; `force_stuck_for_test()`; depois de `drop(engine)`, o contador é 1 e o diário continua com `paused_by_us = true` e nenhuma entrada.
  - `retry_resume_without_stuck_sends_nothing`: alvo parado por `kill -STOP` do próprio teste; `retry_resume()` devolve `Ok` e o alvo continua parado.
- [x] **Passo 2:** `cargo test -p gt-testkit --test engine_live` → FALHA de compilação (campo `fault` e métodos inexistentes).
- [x] **Passo 3: implementar.**
  - Campo `stuck: bool` no `Engine`. Todo caminho que hoje chama `save_logged(f.stuck)` passa a fazer `self.stuck |= f.stuck`; `save_journal(paused_by_us)` passa a gravar `paused_by_us || self.stuck`.
  - No início de `apply`, `undo`, `undo_all`, `restore_entries` (e das operações das Tarefas 2 e 3), **antes** de `check_alive`, `is_stopped_now` e dos retornos antecipados (`chosen.is_empty()`, `batch.is_empty()`), chamar `self.retry_resume()?` quando `self.stuck`.
  - No `Drop`: primeiro `retry_resume()` (só registra erro), depois o caminho atual; o `undo_all` só é pulado se não houver ativos, mas o `retry_resume` roda sempre.
  - Injeção: onde o motor chama `guard.resume()` em `locked_write`, se `pause_cfg.fault` for `ContFailsOnce` (ainda não disparado) ou `ContFailsAlways`, não chamar `guard.resume()`: marcar o guarda como já retomado sem sinal (método `#[doc(hidden)] PauseGuard::forget_resume(self)`) e devolver `Err(Error::Io(io::Error::other("falha de SIGCONT simulada")))` ao `settle_resume`. Em `retry_resume`, `ContFailsAlways` também falha sem enviar. `ContFailsOnce` dispara uma vez por motor (campo `cont_fault_fired`).
  - `finish(ProcessGone)` também zera `stuck`.
- [x] **Passo 4:** testes do Passo 1 e os unitários de `settle_resume` existentes passam.
- [x] **Passo 5:** commit `fix(core): SIGCONT que falha mantém o marcador até retomar o jogo`.

---

### Tarefa 2: Restos de uma falha e descarte quando o jogo fecha

Spec 5 (APIs do motor), 6.7, 6.8.

**Arquivos:**
- Modificar: `crates/gt-core/src/engine.rs`
- Teste: `crates/gt-testkit/tests/engine_live.rs`

**Interfaces:**
- Consome: `Fault::WriteAndRollbackFail` (Tarefa 1).
- Produz:
  - `Engine::pending_ids(&self) -> Vec<String>`: `id` das entradas pendentes (restos e intenções não resolvidas).
  - `Engine::restore_pending(&mut self) -> Result<RestoreReport>`: chama `restore_entries` com as pendentes.
  - `Engine::discard_gone(&mut self)`: se `check_alive` disser que o processo terminou, faz o mesmo que `finish(ProcessGone)` (zera ativos, pendentes e `stuck`, limpa o diário) sem escrever; senão não faz nada.
  - Classificação dos desfechos, sem enum novo (documentar no topo do `engine.rs`): `Ok(report)` com o `id` em `report.restored` = sucesso; `id` em `report.foreign` = bytes de outro (patch tirado do registro); qualquer outro `Err` (inclusive, a partir da Tarefa 3, `NotActive` e `BoundariesChanged`; e `ThreadsInPatch`, `PauseTimeout`, `BytesMismatch`, `Journal`, `Io`, `WriteRefused`, `OutsidePolicy`, `Overlap`) com o `id` fora de `pending_ids()` = recusa limpa; depois de **qualquer** operação, `is_stuck()` diz se o SIGCONT falhou (pode acontecer sem restos); `Err(_)` com o `id` em `pending_ids()` = restos; `Err(ProcessGone)` = jogo fechou.

- [x] **Passo 1: testes que falham** (`engine_live.rs`):
  - `dirty_failure_leaves_pending_and_blocks_range`: `fault = WriteAndRollbackFail`; `apply` do patch A devolve erro; `pending_ids() == ["A"]`; os bytes no endereço são os novos; aplicar de novo A devolve `Error::Overlap`; um patch B em outro endereço aplica normalmente; o diário tem a entrada de A.
  - `restore_pending_returns_originals`: depois do anterior (com `fault` desarmado: o motor só injeta uma vez, campo `write_fault_fired`), `restore_pending()` devolve `restored == ["A"]`, os bytes voltam aos originais e `pending_ids()` fica vazio.
  - `discard_gone_after_target_exit`: patch ativo, o teste mata o alvo e espera `waitpid`; `discard_gone()` deixa `active()` vazio e o diário apagado, sem erro.
- [x] **Passo 2:** os testes falham (métodos inexistentes).
- [x] **Passo 3: implementar.** `write_plan` é `&self`: o "já disparou" fica num `Cell<bool>` (`write_fault_fired`). Injeção de `WriteAndRollbackFail` em `write_plan`: na primeira escrita do lote, escrever de verdade os bytes novos (pelo `write_verified`) e devolver `Failure { error: Error::Io(io::Error::other("falha de escrita simulada")), dirty: true, stuck: false }` sem rollback. Uma vez por motor.
- [x] **Passo 4:** testes passam; os existentes continuam passando.
- [x] **Passo 5:** commit `feat(core): consulta e restauração de restos e descarte quando o jogo fecha`.

---

### Tarefa 3: Troca dos bytes de um patch ativo e diário com `next`

Spec 6.7, 7 item 5 (`entry_range`).

**Arquivos:**
- Modificar: `crates/gt-core/src/journal.rs`, `crates/gt-core/src/engine.rs`, `crates/gt-core/src/error.rs` (variantes `NotActive` e `BoundariesChanged`), `crates/gt-core/src/recovery.rs`, `crates/gt-core/src/suspend.rs` (`hook_on_pause`), `crates/gt-core/src/mem/test_support.rs`, `crates/gt-testkit/src/bin/gt-testtarget.rs`, `crates/gt-testkit/src/lib.rs`, `crates/gt-testkit/src/bin/gt-patch-harness.rs`, `crates/gt-grounded2/src/catalog.rs`
- Teste: `journal.rs` (`mod tests`), `engine_live.rs`, `harness.rs`, `catalog.rs` (`mod tests`)

**Interfaces:**
- Produz:
  - `JournalEntry { …, #[serde(default, skip_serializing_if = "Option::is_none", with = …)] pub next: Option<Vec<u8>> }`: os bytes novos só durante uma troca; `replacement` é sempre o que estava no jogo antes dela.
  - `JournalEntry::range(&self) -> Range<u64>` (substitui os dois `entry_range`).
  - `decide_entry`: cada byte igual ao original, ao de `replacement` ou ao de `next` naquela posição → `Restore`; igual ao original → `Drop`; senão `Foreign`.
  - `Engine::set_replacement(&mut self, id: &str, new: Vec<u8>) -> Result<BatchReport>`: troca os bytes do patch ativo `id` numa pausa (spec 6.7, passos 1–5). Desfechos, com o estado exato em memória e no diário:
    - `id` não ativo → `Error::NotActive { id }` (variante nova), nada muda;
    - patch novo sem as mesmas `replacement_starts` do atual → `Error::BoundariesChanged { id }` (variante nova), nada muda;
    - sucesso → `restored == [id]`; o ativo passa a ser o `Validated` novo; diário com `replacement = new`, `next = None`;
    - bytes atuais ≠ os do valor atual sob pausa → `foreign == [id]`; o ativo sai do registro e do diário sem escrever (como no `undo`);
    - recusa limpa (`ThreadsInPatch`, `PauseTimeout`, erro antes da pausa) → `Err`; o ativo continua o antigo; o diário volta a ter a entrada sem `next`;
    - falha suja (escrita interrompida, `dirty`) → `Err`; o ativo antigo sai de `active` e vai para `pending` a entrada `{ original, replacement: antigo, next: Some(new) }`; `pending_ids() == [id]`;
    - SIGCONT que falha → como na Tarefa 1 (`stuck`), com o desfecho de escrita acima;
    - jogo fechou → `Err(ProcessGone)` pelo `finish`, sem escrita.
  - `PauseConfig { …, #[doc(hidden)] pub hook_on_pause: u32 }` (padrão 1): o gancho `InPause` dispara na N-ésima pausa bem-sucedida do motor, não só na primeira.
  - `test_support::value_code(word: u64) -> Vec<u8>`: laço `mov eax, imm32` (`B8 xx xx xx xx`, imediato no deslocamento 1) seguido de `mov [abs], eax` para a palavra `word` e salto para o início.
  - `TestTarget { …, pub value_page: u64, pub value_word: u64 }` e `TargetOpts { …, pub spin_value: bool }` (thread no laço de valor). O imediato inicial é `0x11111111`. Os usos de `TargetOpts { spin: true }` em `engine_live.rs` passam a `TargetOpts { spin: true, ..Default::default() }`.
  - `gt-patch-harness`: opção do `run` `--set-value HEX` (depois do `apply`, troca o `replacement` do patch para esses bytes e segue no laço do `run`) e opções globais `--hook-pause N` (repassa a `hook_on_pause`) e `--fault cont-always|cont-once|write` (repassa a `fault`). As globais vêm antes do subcomando, como o `--hook` atual (`gt-patch-harness --hook kill-in-pause --hook-pause 2 run …`).
  - Todos os literais `JournalEntry { … }` existentes (em `catalog.rs`, `journal.rs`, `engine_live.rs`, `apps/gt-patch/tests/cli.rs`, `harness.rs`) ganham `next: None`. O `next` usa um módulo serde novo de hex opcional (`hex_opt`), ao lado do de hex que o `journal.rs` já tem.
  - `catalog::resolve`: `journal_hit` também vale quando os bytes atuais são os de `entry.next`.

- [x] **Passo 1: testes que falham.**
  - `journal.rs`: `decide_entry_with_next` (original `[1,2]`, replacement `[3,4]`, next `[5,6]`: `[5,6]`, `[3,6]`, `[1,6]` → `Restore`; `[7,6]` → `Foreign`); `phase2_journal_still_loads` (JSON gravado no formato atual, sem `next`, carrega com `next == None`; salvar uma entrada sem `next` não escreve a chave).
  - `catalog.rs`: `journal_hit_accepts_next` (`resolve` com diário cuja entrada tem `next` igual aos bytes atuais → `Active`/`Pending` conforme `locked`).
  - `engine_live.rs`, com `spin_value`: `set_replacement_changes_executed_value` (aplica `B8 11 11 11 11` → `B8 22 22 22 22`, lê `value_word` até ver `0x22222222`; `set_replacement` para `B8 33 33 33 33` e a palavra chega a `0x33333333`); `set_replacement_foreign_drops_patch` (bytes trocados por fora → `foreign == [id]`, `active()` vazio, alvo intacto); `set_replacement_rejects_new_boundaries` (novo `replacement` que decodifica em outras instruções → `BoundariesChanged`); `set_replacement_1000_times_under_load` (alterna entre dois valores 1000 vezes; o alvo continua vivo e a palavra continua mudando; `danger_retries == 0`).
  - `engine_live.rs`, desfechos: `set_replacement_unknown_id` (`NotActive`); `set_replacement_clean_refusal_restores_journal` (depois do `apply`, o diretório do diário perde a permissão de escrita; `set_replacement` falha antes da pausa; o ativo continua o antigo e os bytes no alvo também; devolver a permissão e conferir que o `undo_all` restaura); `set_replacement_dirty_leaves_pending_with_next` (`fault = WriteAndRollbackFail` armado só para a segunda escrita, pelo `hook_on_pause = 2`; `pending_ids() == [id]`; a entrada do diário tem `next`; `restore_pending()` devolve os originais); `set_replacement_target_exit_is_process_gone` (gancho `InPause` com `hook_on_pause = 2` mata o alvo; `ProcessGone`, diário limpo).
  - `harness.rs`: `sigkill_during_value_swap_then_recover` (`--hook kill-in-pause --hook-pause 2 run … --set-value …`; o harness morre dentro da segunda pausa e o alvo fica parado; `gt-patch-harness restore` retoma e restaura os originais `B8 11 11 11 11`); `cont_always_then_restore_resumes` (`--fault cont-always`; o `run` sai com erro e o alvo fica parado; `restore` sem `--fault` retoma o alvo e restaura).
  - Os testes com `spin_value` em `engine_live.rs` se serializam por um `static Mutex<()>` próprio (pausa de 500 ms × 10 tentativas não pode disputar CPU com outros alvos girando). `set_replacement_changes_executed_value` também confere que o diário final tem uma única entrada, com `replacement` = o novo e sem `next`.
  - O teste de 1000 trocas grava o diário 2000 vezes com `fsync`: criar o `TempDir` em `/dev/shm` se existir, para não ficar lento.
- [x] **Passo 2:** os testes falham.
- [x] **Passo 3: implementar.** Contador `pauses_ok: u32` no `Engine`, incrementado em `locked_write` logo depois de `pause()` devolver `Ok` (toda pausa bem-sucedida conta, inclusive as que terminam por perigo ou no ramo `needs_marker`). O gancho `InPause` é armado quando `pauses_ok + 1 == hook_on_pause` (ele dispara dentro de `pause()`); a falha `WriteAndRollbackFail` dispara no `write_plan` quando `pauses_ok == hook_on_pause` (lido por `&self`). Com `hook_on_pause = 1`, o comportamento atual (só a primeira pausa) não muda. `set_replacement`: `retry_resume` se `stuck` (antes de qualquer retorno); achar o ativo; montar o `Validated` novo com `expected` = original; **durante a troca, o estado em memória** tem o ativo fora de `active` e, em `pending`, a entrada `{ original, replacement: antigo, next: Some(new) }`, para que qualquer regravação do diário no meio (ex.: o ramo `needs_marker` de `locked_write`) mantenha o `next`; intenção gravada fora da pausa; `locked_write` com uma `Kind::Swap { from: Vec<u8> }` cujo `decide_now` exige `current == from` (senão `Skip::Foreign`); regra de perigo com uma variante nova `Danger::None` (mesmas fronteiras: nenhum ponto perigoso); sucesso → o `Validated` novo volta para `active` e a pendente sai; recusa limpa → o ativo antigo volta e a pendente sai; regravar o diário. Teste extra `set_replacement_rewrite_keeps_next`: no gancho `InPause` da segunda pausa, ler o diário (caminho num `static` próprio deste teste) e conferir que a entrada tem `next`. Cada teste com gancho tem o **seu** `static` e a **sua** função de gancho (ex.: `KILL_PID_SWAP` e `kill_in_swap`), nunca o `KILL_PID` existente, porque os testes do binário rodam em paralelo. Remover os dois `entry_range` em favor de `JournalEntry::range`.
- [x] **Passo 4:** testes passam.
- [x] **Passo 5:** commit `feat(core): troca dos bytes de um patch ativo com diário e recuperação`.

---

### Tarefa 4: Modelo de valores no `gt-core`

Spec 6.3, 5.

**Arquivos:**
- Criar: `crates/gt-core/src/value.rs` (`pub mod value` sem feature em `lib.rs`)
- Modificar: `crates/gt-core/src/asm.rs`
- Teste: `value.rs` e `asm.rs` (`mod tests`)

**Interfaces:**
- Produz:
  - `pub enum ValueType { I32, U32, F32 }` com `fn size(self) -> usize` (sempre 4).
  - `pub struct ValueSpec { pub ty: ValueType, pub slot: usize, pub min: f64, pub max: f64, pub step: f64, pub default_k: u64, pub unit: Option<String>, pub decimals: u8 }` (`#[derive(Debug, Clone, PartialEq)]`; os números em `f64` cobrem `i32`/`u32`/`f32` exatamente o bastante; a igualdade das definições usa `default_k`).
  - `ValueSpec::steps(&self) -> u64`: maior `k` com `min + k·step ≤ max` (com folga `step · 1e-4`).
  - `ValueSpec::value(&self, k: u64) -> f64`: `min + k·step`, com `k` limitado a `steps()`.
  - `ValueSpec::nearest_k(&self, v: f64) -> u64`: `k` do ponto da grade mais próximo de `v`, limitado a `[0, steps()]`.
  - `ValueSpec::encode(&self, k: u64) -> [u8; 4]`: little-endian; `I32`/`U32` arredondam o valor; `F32` converte.
  - `ValueSpec::bytes(&self, replacement: &[u8], k: u64) -> Vec<u8>`: `replacement` com `slot..slot+4` trocado por `encode(k)`.
  - `ValueSpec::format(&self, k: u64) -> String`: número com `decimals` casas (inteiros sem casas) seguido de `unit` (ex.: `1.5x`).
  - `ValueSpec::check(&self) -> Result<(), String>`: `min ≤ max`, `step > 0`, finitos, representáveis no tipo, `default_k ≤ steps()`.
  - `asm::immediate_slot(code: &[u8], ip: u64) -> Vec<ImmSlot>`, com `pub struct ImmSlot { pub offset: usize, pub size: usize, pub sign_extended_to_64: bool, pub is_branch: bool }`: um item por instrução com imediato (e um para o deslocamento relativo de desvios, com `is_branch = true`), pelos `ConstantOffsets` do `iced-x86`, pegos do mesmo `Decoder` logo depois de decodificar cada instrução; `sign_extended_to_64` vem de `OpKind::Immediate32to64` (e `Immediate8to64`); `is_branch` vem de `has_branch_displacement()` do `ConstantOffsets`.
  - `ValueSpec::check_slot(&self, replacement: &[u8]) -> Result<(), String>`: o `slot` coincide exatamente com um `ImmSlot` de tamanho 4 e `is_branch == false`; com `U32` e `sign_extended_to_64`, exige `max ≤ i32::MAX`; os bytes em `slot` são `encode(default_k)`.

- [x] **Passo 1: testes que falham:** `grid_by_k_has_no_drift` (`F32`, `min 0.1`, `step 0.1`, `max 10`: `value(99)` = `10.0` dentro de `1e-6`, sem somar); `nearest_k_clamps` (abaixo de `min` → 0, acima de `max` → `steps()`); `encode_types` (`I32` −1 → `FF FF FF FF`; `U32` 7 → `07 00 00 00`; `F32` 1.5 → `00 00 C0 3F`); `format_units` (`F32` decimals 1, unit `x`, 1.5 → `1.5x`; `I32` 100 → `100`); `check_slot_accepts_mov_eax_imm32` (`B8 00 00 C0 3F`, slot 1, `F32`, default 1.5); `check_slot_rejects` com um caso por regra: slot 0, `jmp rel32` (`E9 …`), `imm8` (`6A 05`), `mov rax, imm32` com `U32` e `max` > `i32::MAX`, bytes do slot ≠ default; `grid_keeps_instruction_boundaries` (um `ValueSpec` `F32` de 0 a 10 em passos de 0.5 sobre `replacement = B8 00 00 00 00 90`, com `expected = 90 90 90 90 90 90`: os bytes de cada `k` de `0..=steps()` passam no `patch::validate` com as mesmas `replacement_starts`).
- [x] **Passo 2:** os testes falham.
- [x] **Passo 3:** implementar.
- [x] **Passo 4:** os testes passam.
- [x] **Passo 5:** commit `feat(core): modelo de valores ajustáveis e conferência do imediato`.

---

### Tarefa 5: Assinaturas: `name`, `category` e `value`; busca de subconjunto

Spec 6.3, 6.2 (nova busca), 5.

**Arquivos:**
- Modificar: `crates/gt-grounded2/src/signatures.rs`, `crates/gt-grounded2/signatures/generic.toml`, `crates/gt-grounded2/src/catalog.rs`
- Teste: `signatures.rs` e `catalog.rs` (`mod tests`)

**Interfaces:**
- Consome: `gt_core::value::ValueSpec` (Tarefa 4).
- Produz:
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)] pub enum Category { Sobrevivencia, Itens, Progressao, Movimento, Diagnostico }` (ordem de exibição = ordem de declaração, que o `Ord` derivado segue; `fn label(self) -> &'static str` com `SOBREVIVÊNCIA`, `ITENS`, `PROGRESSÃO`, `MOVIMENTO`, `DIAGNÓSTICO`; parse de `sobrevivencia`, `itens`, `progressao`, `movimento`, `diagnostico`).
  - `PatchDef { …, pub name: String, pub category: Category, pub value: Option<ValueSpec> }`. O `PatchDef` e o `catalog::Entry` deixam de derivar `Eq` (o `ValueSpec` tem `f64`); manter `PartialEq` (os `assert_eq!` continuam valendo). O auxiliar `def()` dos testes de `state.rs` ganha os campos novos.
  - TOML: `name` e `category` obrigatórios no genérico, opcionais (sobrescrita) no da versão; bloco `[patch.value]` com `type` (`"i32"|"u32"|"f32"`), `slot`, `min`, `max`, `step`, `default`, `unit`, `decimals`; no da versão, `value` substitui o bloco inteiro. Para `i32`/`u32`, `min`/`max`/`step`/`default` só como inteiro TOML (`1.0` → erro); para `f32`, inteiro ou decimal. `default` precisa cair na grade: `k = round((default − min)/step)` e `|default − (min + k·step)| < step · 1e-4`.
  - Os números do bloco `value` são lidos como `toml::Value` (ou um enum `#[serde(untagged)]` inteiro/decimal) e convertidos à mão, porque `f64` direto recusa inteiro TOML e `i64` recusa decimal.
  - Todos os TOML embutidos nos testes existentes (`signatures.rs`, `catalog.rs` e outros) ganham `name` e `category`.
  - Erros de carga pelo `Error::Signature { file, field, reason }` existente, com `field` = `name`, `category`, `value.type`, `value.slot`, `value.default` etc.
  - `catalog::scan_subset(target: &Target, sigs: &Signatures, ids: &[String]) -> Result<CatalogScan>`: como `scan`, só com as definições cujo `id` está em `ids` (sem diário); `scan` passa a chamar a mesma função interna com todas. O filtro é a função pura `pub fn select_defs(defs: Vec<PatchDef>, ids: Option<&[String]>) -> Vec<PatchDef>` (mantém a ordem do arquivo).
  - `generic.toml`: `teste-codificacao` ganha `name = "Patch de teste"` e `category = "diagnostico"`.

- [x] **Passo 1: testes que falham:** um teste por erro da spec 6.3 (lista na linha "Unitário (assinaturas)" da spec 8), cada um conferindo o `field`; `real_files_load` atualizado (o `teste-codificacao` tem categoria `Diagnostico` e `value == None`); `value_block_builds_valuespec` (bloco `f32` válido sobre `B8 00 00 C0 3F`); `version_overrides_value_whole_block`; `select_defs_keeps_listed_in_order` (com `None` devolve todas; com `["b","a"]` devolve `a, b` na ordem do arquivo; `id` desconhecido é ignorado).
- [x] **Passo 2:** os testes falham.
- [x] **Passo 3:** implementar (a conferência do slot chama `ValueSpec::check` e `check_slot`).
- [x] **Passo 4:** os testes passam, inclusive os do `gt-patch` (o `list` continua igual).
- [x] **Passo 5:** commit `feat(grounded2): nome, categoria e valor nas assinaturas`.

---

### Tarefa 6: Pendências pequenas da Fase 2

Spec 7 itens 2, 4 e 5.

**Arquivos:**
- Modificar: `crates/gt-core/src/modules.rs`, `crates/gt-core/src/error.rs`, `crates/gt-core/src/engine.rs` (doc de `restore_entries`), `crates/gt-grounded2/src/resolve.rs`, `apps/gt-cli/src/main.rs`, `apps/gt-patch/src/main.rs`, `apps/gt-patch/src/cmd/profile.rs`, `apps/gt-patch/tests/cli.rs`, `crates/gt-testkit/tests/harness.rs`
- Teste: `modules.rs`, `resolve.rs`, `profile.rs` (`mod tests`)

**Interfaces:**
- Produz:
  - `modules::WarnOnce` (struct com `HashSet<(&'static str, PathBuf)>`) e `fn first(&mut self, kind: &'static str, path: &Path) -> bool`; um `static WARNED: LazyLock<Mutex<WarnOnce>>` usado por `trusted_extent` (mutex envenenado: usar `into_inner` do `PoisonError`, nunca `unwrap`): primeira vez `warn!`, depois `debug!`, para os dois avisos (`kind` = `"extensão"` e `"size_of_image"`).
  - `permission_hint(program: &str, ptrace_scope: Option<u8>) -> String`: `program` substitui "o programa" nas frases (o `gt` passa `"gt"`, o `gt-patch` passa `"gt-patch"`, a TUI passa `"gt-tui"`).
  - `Error::AlreadyRunning` com o texto `outro gt-patch ou gt-tui já está em execução`; a dica do `gt-patch` vira `feche o outro programa antes`.
  - `profile`: função pura `same_process(a: &Identity, b: &Identity) -> bool`; diferente → erro `o jogo reiniciou`.

- [x] **Passo 1: testes que falham:** `warn_once_per_path` (dois caminhos e dois tipos; `first` só devolve `true` na primeira vez de cada par); `permission_hint_names_program` (com `Some(3)`, o texto contém `o gt-tui não altera nada`); `profile_refuses_other_identity`; atualizar os testes que procuram o texto antigo da trava (`apps/gt-patch/tests/cli.rs` perto da linha 148, `crates/gt-testkit/tests/harness.rs` perto da linha 329, e o da dica no `main.rs` do `gt-patch`).
- [x] **Passo 2:** os testes falham.
- [x] **Passo 3:** implementar; documentar `outside_policy` em `restore_entries`.
- [x] **Passo 4:** os testes passam.
- [x] **Passo 5:** commit `fix: pendências da fase 2 (identidade no profile, dica com o nome do programa, avisos únicos)`.

---

### Tarefa 7: `gt-tui`: modelo e teclas

Spec 6.4, 6.5, 6.6.

**Arquivos:**
- Modificar: `Cargo.toml` (`chrono` no `[workspace.dependencies]`), `apps/gt-tui/Cargo.toml` (lib + bin; deps `gt-core` com `write`, `gt-grounded2` com `write`, `ratatui`, `crossterm`, `clap`, `anyhow`, `thiserror`, `tracing`, `tracing-subscriber` com `env-filter`, `chrono`; feature `test-backend = []`; dev-deps `gt-testkit`, `tempfile`, `serde_json`, `nix` com `features = ["term"]`, e `gt-tui = { path = ".", features = ["test-backend"] }`)
- Criar: `apps/gt-tui/src/lib.rs`, `src/model.rs`, `src/keys.rs`
- Modificar: `docs/ARCHITECTURE.md` 4 (`chrono`) e 8 (decisão "`gt-patch` e `gt-tui` com `write`"), antes do código (`AGENTS.md` 1)
- Teste: `model.rs` e `keys.rs` (`mod tests`)

**Interfaces:**
- Consome: `gt_core::value::ValueSpec`, `gt_grounded2::signatures::Category`.
- Produz (`model.rs`):
  - `pub enum Conn { Waiting, NoAccess(String), Connecting, Connected { pid: i32, version_key: String, has_version_file: bool, disk: Result<(), String>, engine: bool }, Many(Vec<i32>), Error(String), PidGone }` com `fn label(&self) -> &'static str` (textos da seção Restrições).
  - `pub enum Avail { Available { verified: bool }, Unavailable(String) }`.
  - `pub enum CheatStatus { On, Off, Unavailable(String), Residue }`.
  - `pub struct CheatRow { pub id: String, pub name: String, pub category: Category, pub value: Option<ValueSpec>, pub k: u64, pub applied_k: Option<u64>, pub avail: Avail, pub on: bool, pub residue: bool, pub desired: bool }` (`desired` é só um espelho para a tela; a fonte da verdade é o mapa de desejados do controlador, Tarefa 12, que atualiza o espelho) com `fn status(&self, conn: &Conn) -> CheatStatus` (sem `Connected` → `Unavailable("jogo não conectado")`; `residue` → `Residue`; `on` → `On`; `Available` → `Off`; senão `Unavailable(motivo)`).
  - `pub struct Banners { pub corrupt: Option<String>, pub blocked: Option<String>, pub stuck: Option<String>, pub residue: Option<String> }` com `fn lines(&self) -> Vec<&str>` (nessa ordem); cada origem liga e desliga o seu campo.
  - `pub struct Model { pub conn: Conn, pub cheats: Vec<CheatRow>, pub selected: usize, pub busy: Option<String>, pub banners: Banners, pub events: VecDeque<String>, pub diagnostic: bool, pub too_small: bool, last_value_event: Option<String> }` com `Model::new(diagnostic: bool) -> Model`. `selected` é o índice em `visible()`.
  - `k` é o valor mostrado; `applied_k` é o que está no jogo (com o cheat ligado). O resto do estado da fila de valores (alvo pendente, instante da última troca) fica num struct privado do controlador (Tarefa 14).
  - Derives (`Category` já vem com os seus da Tarefa 5): `Conn`, `Avail`, `CheatStatus`, `CheatRow`, `Banners`, `Model`: `Clone + Debug + PartialEq`.
  - `Model::visible(&self) -> Vec<&CheatRow>`: sem `diagnostic`, esconde `Category::Diagnostico`; ordem por categoria e depois pela ordem de chegada.
  - `Model::selected_id(&self) -> Option<&str>` e `Model::move_selection(&mut self, delta: i32)` (sem dar a volta; as linhas de cabeçalho não contam porque `selected` indexa só cheats).
  - `Model::counts(&self) -> (usize, usize)`: (disponíveis ou ligados, total), só dos visíveis.
  - `Model::push_event(&mut self, hhmm: &str, text: String)`: grava `"<hhmm> <text>"`, limite de 200, e emite `tracing::info!` com o texto; `Model::merge_value_event(&mut self, hhmm: &str, id: &str, text: String)`: se a última linha veio de `merge_value_event` com o mesmo `id`, substitui a linha; emite `tracing::debug!`.
- Produz (`keys.rs`): `pub enum Action { Up, Down, Toggle, Dec, Inc, Retry, Quit }` e `pub fn action(ev: &KeyEvent) -> Option<Action>` (`↑`/`k`, `↓`/`j`, espaço/`Enter`, `←`/`h`, `→`/`l`, `r`, `q`/`Esc`/Ctrl+C; Ctrl+Z e o resto → `None`; só `KeyEventKind::Press`).

- [x] **Passo 1: testes que falham:** `keys_map` (cada tecla da tabela da spec 6.6, mais Ctrl+Z → `None`); `selection_moves_over_visible_only`; `status_mapping` (cada caso de `CheatRow` → `CheatStatus`); `diagnostic_hidden_and_not_counted`; `value_events_merge` (duas trocas do mesmo `id` = uma linha; uma troca de outro `id` no meio separa); `events_capped_at_200`.
- [x] **Passo 2:** `cargo test -p gt-tui --lib` falha.
- [x] **Passo 3:** implementar.
- [x] **Passo 4:** os testes passam.
- [x] **Passo 5:** commit `feat(tui): modelo da tela e mapa de teclas`.

---

### Tarefa 8: `gt-tui`: tela

Spec 6.4.

**Arquivos:**
- Criar: `apps/gt-tui/src/ui.rs`
- Teste: `ui.rs` (`mod tests`, com `ratatui::backend::TestBackend`)

**Interfaces:**
- Consome: `Model` (Tarefa 7).
- Produz:
  - `pub const MIN_WIDTH: u16 = 60; pub const MIN_HEIGHT: u16 = 15;` e `pub fn too_small(width: u16, height: u16) -> bool`.
  - `pub fn render(model: &Model, frame: &mut ratatui::Frame)`. Layout da spec 6.4: bloco com título `grounded-trainer ─ <conn>` (e `(pid N)` quando conectado); linha de versão (`versão <chave> (com assinaturas|sem assinaturas|<causa do disco>)  <a>/<t>`) ou a mensagem de `busy` quando houver; faixa dos avisos (`banners.lines()`, uma linha por aviso, destacada); lista com cabeçalhos de categoria e linhas `> nome  estado  [valor ◀ ▶]`, com `?` e `↻`; registro de eventos (as últimas linhas que couberem, mínimo 3); rodapé `↑↓ mover  espaço liga/desliga  ◀▶ valor  r rebuscar  q sair`. Abaixo de 60×15, só `aumente o terminal`. Lista vazia (sem cheats visíveis, conectado): a mensagem da seção Restrições.
  - Rolagem sem estado: `pub fn scroll_offset(is_header: &[bool], selected_line: usize, height: usize) -> usize` (função pura sobre as linhas da lista, `true` nos cabeçalhos: menor deslocamento que mostra `selected_line` e, se couber, o cabeçalho mais próximo acima dela). `render` monta as linhas e usa essa função.

- [x] **Passo 1: testes que falham** (desenhar num `TestBackend` 70×20 e procurar trechos no buffer): `renders_waiting`, `renders_no_access_hint`, `renders_many_processes`, `renders_pid_gone`, `renders_connected_rows` (um cheat de cada `CheatStatus`, um com `?`, um com `↻`, um com valor `1.5x ◀ ▶`), `renders_busy_message`, `renders_empty_list_message`, `renders_banner`, `renders_small_terminal` (50×10 → só `aumente o terminal`), `renders_scrolled_selection` (`TestBackend` 70×20, 20 cheats, seleção no último: o nome dele aparece no buffer); `scroll_keeps_selection_and_header_visible` (20 cheats em 3 categorias com 8, 8 e 4 cheats; `scroll_offset` direto com altura 6 de lista, sem bordas; seleção no último: a linha selecionada e o cabeçalho dela ficam dentro da janela).
- [x] **Passo 2:** os testes falham.
- [x] **Passo 3:** implementar.
- [x] **Passo 4:** os testes passam.
- [x] **Passo 5:** commit `feat(tui): tela da lista agrupada`.

---

### Tarefa 9: `gt-tui`: interface de alvo e implementação crua

Spec 5 (interface de alvo), 6.1, 6.2.

**Arquivos:**
- Criar: `apps/gt-tui/src/backend.rs`, `apps/gt-tui/src/raw.rs` (`#[cfg(feature = "test-backend")]`)
- Teste: `apps/gt-tui/tests/core_raw.rs` (sem `[[test]]` no `Cargo.toml`: a dev-dependency do `gt-tui` em si mesmo com `test-backend`, da Tarefa 7, já liga a feature em `cargo test` e `clippy --all-targets`). Os testes deste arquivo que criam alvos com `spin_value` se serializam por um `static Mutex<()>`, para não disputar CPU e estourar o prazo da pausa.

**Interfaces:**
- Produz (`backend.rs`):
  - `pub enum Find { NotRunning, Found(i32), Many(Vec<i32>), PidGone }` (só a varredura de `cmdline`; com `--pid`, `PidGone` quando o processo não existe).
  - `#[derive(thiserror::Error)] pub enum SearchError { NoAccess(String), Gone, Other(String) }` (`NoAccess` traz a dica de permissão; `Gone` = o processo fechou durante a busca) e `#[derive(thiserror::Error)] pub enum TuiError { #[error("{0}")] Other(String) }` para os outros erros da biblioteca (`AGENTS.md` 5).
  - `pub struct CheatInfo { pub id: String, pub name: String, pub category: Category, pub value: Option<ValueSpec>, pub avail: Avail }`.
  - `pub struct Search<P> { pub pid: i32, pub identity: Identity, pub version_key: String, pub has_version_file: bool, pub disk: Result<(), String>, pub cheats: Vec<CheatInfo>, pub ids: Option<Vec<String>>, pub took: Duration, pub payload: P }` (`ids` = `None` na busca completa, `Some(lista)` numa nova busca).
  - `pub type SearchFn<P> = Arc<dyn Fn(i32, Option<Vec<String>>, Option<Identity>) -> Result<Search<P>, SearchError> + Send + Sync>`: abre o alvo e busca o catálogo; roda na thread de busca. Numa nova busca, recebe a identidade da conexão e devolve `Gone` se o processo não for mais o mesmo.
  - `pub enum Recovered { Clean { lines: Vec<String>, corrupt: Option<PathBuf> }, Blocked { lines: Vec<String>, text: String } }` (o bloqueado também traz as linhas do que foi resolvido) e a função pura única `pub fn recovery_outcome(res: gt_core::Result<RecoveryReport>) -> Recovered`, usada pelos dois backends. Textos: as linhas do relatório são as do `print_recovery` do `gt-patch` (`diário descartado: <motivo>`, `retomado o jogo parado`, `o jogo estava parado por outro processo; não foi retomado`, `restaurado: <id>`, `descartado: <id> (bytes já originais)`, `não restaurado: <id> (bytes alterados por outro)`); `corrupt` vira `banners.corrupt` com o texto da seção Restrições; `kept` → `Blocked { text: "não foi possível restaurar <ids> agora; aperte r para tentar de novo", .. }`; erro → `Blocked { lines: vec![], text: "a recuperação do diário falhou: <erro>; aperte r para tentar de novo" }`. Testes unitários `recovery_outcome_texts` (um caso por campo do relatório e o caso de erro).
  - `pub enum OpOutcome { Done(Duration), Refused(String), Foreign, Residue(String), Gone }` e a função pura única `pub fn outcome_of(res: gt_core::Result<BatchReport>, id: &str, pending: &[String]) -> OpOutcome` com a classificação da Tarefa 2, usada pela `RawSession` e pela `GameSession`. Teste `outcome_of_cases` (Done, Foreign, recusa limpa com `ThreadsInPatch`, `NotActive`, `BoundariesChanged`, Residue, Gone).
  - **Trava:** o backend guarda a `JournalLock` num `Arc<JournalLock>` e entrega um clone a cada sessão; a sessão declara o motor **antes** do `Arc` (o motor cai antes, e a trava só é solta quando a última cópia cai). Assim `open_engine` não precisa receber a trava.
  - `pub trait Session<P> { fn has_engine(&self) -> bool; fn open_engine(&mut self) -> Result<(), TuiError>; fn merge_search(&mut self, s: Search<P>) -> bool; fn exited(&self) -> bool; fn apply(&mut self, items: &[(String, u64)]) -> OpOutcome; fn undo(&mut self, id: &str) -> OpOutcome; fn set_value(&mut self, id: &str, k: u64) -> OpOutcome; fn restore_pending(&mut self) -> OpOutcome; fn retry_resume(&mut self) -> OpOutcome; fn is_stuck(&self) -> bool; fn pending_ids(&self) -> Vec<String>; fn discard_gone(&mut self); fn undo_all(&mut self) -> (Vec<String>, Vec<String>); }`
    - `open_engine`: abre o motor se ainda não houver e o disco da busca guardada estiver `Ok` (sem disco, `Err(causa)`); chamado pelo controlador quando o diário fica resolvido.
    - `merge_search(&mut self, s) -> bool`: confere `s.identity` com a da sessão (diferente → `false`, nada muda); guarda o `disk`, a `version_key` e o resto do payload necessário para `open_engine` (no jogo, o `module` e o cabeçalho do disco do `CatalogScan`); atualiza endereços e definições **só** dos `id` em `s.ids` (com `None`, todos), sem tocar nos ativos; o controlador só chama com `id` desligados.
    - `apply` recebe `(id, k)` (o `k` é ignorado em cheat sem valor). `undo_all` devolve (restaurados, alterados por outro). Sem motor, as operações devolvem `Refused("sem motor")`.
  - `pub trait Backend { type Payload: Send + 'static; type Session: Session<Self::Payload>; fn find(&mut self) -> Find; fn search_fn(&self) -> SearchFn<Self::Payload>; fn recover(&mut self) -> Recovered; fn open(&mut self, search: Search<Self::Payload>, engine: bool) -> Result<Self::Session, TuiError>; }` — `open` abre o `pidfd` de monitoramento (conferido com a identidade da busca) e, com `engine == true` e o disco `Ok`, o motor.
- Produz (`raw.rs`): `pub struct RawDef { pub id: String, pub name: String, pub category: Category, pub addr: u64, pub expected: Vec<u8>, pub replacement: Vec<u8>, pub value: Option<ValueSpec>, pub verified: bool }`; `pub struct RawControl { pub pid: Option<i32>, pub defs: Vec<RawDef>, pub version_key: String, pub search_delay: Duration, pub search_panics: bool }` (os endereços mudam a cada alvo novo, por ASLR: o teste troca `pid` e `defs` juntos); `pub struct RawBackend` com `RawBackend::new(control: Arc<Mutex<RawControl>>, journal_dir: PathBuf) -> gt_core::Result<RawBackend>` (pega a trava do diário). Sem `unwrap` fora de testes (mutex envenenado → `into_inner`). `find` devolve `Found` enquanto o `pid` existe e não é zumbi, `NotRunning` com `None`. A busca dorme `search_delay`, entra em pânico se `search_panics`, e confere `expected` (ou os bytes de algum valor da grade, para cheat ligado) no endereço: disponível, ou `Unavailable("bytes diferentes")`. A política de escrita aceita as páginas executáveis do alvo (como o `gt-patch-harness`).
- `ValueSpec` de teste (usado nas Tarefas 9–14): `U32`, `slot 1`, `min 0`, `max 0x33333333`, `step 0x11111111` (grade `0, 0x11111111, 0x22222222, 0x33333333`), `default_k 2`, sobre o `value_page` (`expected = B8 11 11 11 11`, `replacement` padrão `B8 22 22 22 22`: ligar no padrão muda bytes).

- [x] **Passo 1: testes que falham** (`core_raw.rs`): `raw_search_and_toggle` (alvo com `spin_value`; `search_fn` acha os dois `RawDef`, `open`, `apply`, `undo` → `Done`); `raw_set_value` (o valor executado muda para `0x33333333`, pela `value_word`); `raw_exited_after_kill`; `raw_merge_search_updates_only_listed`; `raw_outcomes_foreign_and_refused` (bytes trocados por fora antes do `undo` → `Foreign`; `set_value` de um `id` não ligado → `Refused`); `raw_research_other_identity_is_gone` (nova busca com a identidade de um alvo já morto → `Gone`; `merge_search` com outra identidade → `false`).
- [x] **Passo 2:** os testes falham.
- [x] **Passo 3:** implementar.
- [x] **Passo 4:** `cargo build --workspace && cargo test -p gt-tui --test core_raw` passa.
- [x] **Passo 5:** commit `feat(tui): interface de alvo e implementação crua para testes`.

---

### Tarefa 10: `gt-tui`: implementação do jogo

Spec 6.1, 6.2, 6.5.

**Arquivos:**
- Criar: `apps/gt-tui/src/game.rs`
- Modificar: `crates/gt-grounded2/src/resolve.rs` (`find_pid` público)
- Teste: `game.rs` (`mod tests`, partes puras), `apps/gt-tui/tests/game.rs` (`#[ignore]`) e `apps/gt-tui/tests/game_fd.rs` (`#[ignore]`, um teste só)

**Interfaces:**
- Consome: `Backend`/`Session` (Tarefa 9), `catalog::scan`/`scan_subset` (Tarefa 5), `patching::open_engine`, `recovery::recover`, `permission_hint` (Tarefa 6).
- Produz:
  - `resolve::find_pid(pid: Option<i32>) -> Result<(i32, Vec<(i32, String)>)>`: casca pública sobre a `find_game` privada (mesma regra de descarte e a mesma lista de descartados, que o `gt inspect` usa), sem abrir o alvo; com `Some(pid)`, só confere que o processo existe (`ProcessGone` se não) e devolve a lista vazia.
  - `pub struct GameBackend` com `GameBackend::new(pid: Option<i32>, store: JournalStore, lock: Arc<JournalLock>) -> GameBackend`. `type Payload = (Target, CatalogScan)`; se o `Target` não for `Send`, a busca devolve só o `CatalogScan` e o `open` reabre o `Target` na thread principal conferindo a identidade (escrever o que foi escolhido no topo do `game.rs`). `find` mapeia `find_pid`: `GameNotFound` → `NotRunning`, `ManyCandidates` → `Many`, com `--pid` e processo inexistente → `PidGone`. Erros de permissão na busca → `SearchError::NoAccess(permission_hint("gt-tui", …))`; processo que sumiu → `Gone`. `recover` usa `recovery::recover` e a regra do `ensure_recovered` do `gt-patch`.
  - `pub struct GameSession` (implementa `Session`): guarda por `id` a `PatchDef` e o endereço resolvido; `merge_search` troca só os listados. A busca registra `tracing::info!(duracao_ms = …, "busca dos padrões")`.
  - `Search.disk`: `Ok(())` se `CatalogScan.disk` for `Some`; senão `Err(causa)` tirada do primeiro `DiskUnreadable(causa)` das entradas, ou `"disco ilegível"` sem entradas.
  - Funções puras: `pub fn avail_of(state: &PatchState) -> Avail` (`Verified` → `Available { verified: true }`, `Unverified` → `{ verified: false }`, todos os outros, inclusive `Active` e `Pending`, → `Unavailable(label)`) e `pub fn merge_entries(old: &mut Vec<Entry>, new: Vec<Entry>, ids: Option<&[String]>)`.
  - Trava compartilhada por `Arc<JournalLock>` (Tarefa 9); no `GameSession`, o campo do motor vem antes do `Arc`.

- [x] **Passo 1: testes que falham:** `avail_mapping` (cada `PatchState`); `merge_entries_only_listed`; `tests/game.rs` (`#[ignore]`, só com a chave `c935e799-0b27c000`): `game_backend_toggles_test_patch` (`find` → `Found`, busca, `teste-codificacao` `Available { verified: true }`, `apply`/`undo` conferindo os bytes `48 09 d0`/`48 0b c2`); `tests/game_fd.rs`: `game_backend_reopen_50_times_no_fd_leak` (50 buscas + `open` + drop; a contagem de `/proc/self/fd` no fim fica no máximo 2 acima da inicial).
- [x] **Passo 2:** os testes falham (os `#[ignore]` só compilam).
- [x] **Passo 3:** implementar.
- [x] **Passo 4:** os unitários passam; os `#[ignore]` ficam para a Tarefa 17.
- [x] **Passo 5:** commit `feat(tui): implementação do jogo da interface de alvo`.

---

### Tarefa 11: Controlador (1/4): conexão, busca e recuperação

Spec 6.1, 6.2, 6.10. Constantes: procura a cada 2 s; novas buscas a cada `max(5 s, 2 × took)` por até 120 s depois de cada conexão; laço de 100 ms.

**Arquivos:**
- Criar: `apps/gt-tui/src/controller.rs`, `apps/gt-tui/src/fake.rs` (`#[cfg(test)] pub(crate) mod fake`: `FakeBackend`/`FakeSession` em memória, com respostas programadas, registro de chamadas e acesso ao último `Model` desenhado; reusado pela Tarefa 15)
- Teste: `controller.rs` (`mod tests`), `apps/gt-tui/tests/core_raw.rs`, `apps/gt-tui/tests/fd_leak.rs` (um teste só)

**Interfaces:**
- Consome: `Model`, `Action` (Tarefa 7), `Backend`/`Session`/`Search` (Tarefa 9).
- Produz:
  - `pub type Clock = Box<dyn Fn() -> String>` (devolve `HH:MM`; o `main` usa `chrono::Local`, os testes um valor fixo).
  - `pub struct Controller<B: Backend> { pub model: Model, … }`, com os campos nesta ordem: `session: Option<B::Session>` **antes** de `backend: B` (a sessão, e o motor dela, caem antes da trava).
  - `Controller::new(backend: B, clock: Clock, diagnostic: bool, pid_fixed: bool) -> Controller<B>`.
  - `fn start(&mut self, draw: &mut dyn FnMut(&Model))`: desenha `recuperando o diário…`, chama `recover`; `Clean` → linhas no registro e `corrupt` em `banners.corrupt`; `Blocked` → linhas no registro, `text` em `banners.blocked` e escritas bloqueadas (resolvido, `banners.blocked` some).
  - `fn tick(&mut self, now: Instant, draw: &mut dyn FnMut(&Model))`: sem conexão, `find` a cada 2 s; com `Found(pid)`, recupera de novo (com `busy` desenhado), põe `Connecting` e inicia a busca numa thread (`std::thread::spawn`, resultado por `mpsc`); colhe o resultado (ver regras); com sessão, consulta `exited()` e, se fechou, `discard_gone`, evento `o jogo fechou`, volta a `Waiting` (ou `PidGone` com `pid_fixed`, estado final); dispara as novas buscas automáticas.
  - `pub(crate) fn note_op(&mut self, id: &str)`: incrementa o contador de operações do cheat (a Tarefa 12 chama em toda operação; os testes desta tarefa chamam direto).
  - `#[doc(hidden)] pub fn wait_search(&mut self, now: Instant, timeout: Duration) -> bool`: bloqueia no `Receiver` até o resultado chegar (ou o prazo) e o processa como o `tick` com o mesmo `now` (todo instante do controlador vem do `now` recebido, nunca de `Instant::now()` interno); os testes usam isso em vez de `sleep`.
  - `fn search_panicked(&self) -> bool`: a thread de busca terminou sem mandar resultado (`mpsc::RecvError`/`Disconnected` com a thread acabada) — o `main` sai pelo caminho de pânico.
  - Regras: número da conexão gravado ao iniciar a busca, resultado descartado se mudou; contador de operações por cheat gravado ao iniciar uma nova busca, resultado daquele cheat descartado se mudou (Tarefa 12 incrementa o contador); cheat já resolvido nesta conexão que a nova busca não acha (`não encontrado`/`inconclusivo`) mantém o estado; motor só aberto com diário resolvido e disco `Ok` (senão `Connected { engine: false }`); `SearchError::NoAccess` → `Conn::NoAccess`, `Gone` → `Waiting`, `Other` → `Error`; `Find::Many` → `Conn::Many`; disco ilegível não dispara busca automática. Nesta tarefa, as novas buscas automáticas disparam enquanto houver cheat visível `não encontrado` ou `inconclusivo`; a Tarefa 12 acrescenta "ou desejado indisponível" e a religação depois delas. Depois de **toda** chamada da sessão (aqui e nas Tarefas 12–14), o controlador consulta `is_stuck()` e liga ou desliga `banners.stuck` (`o jogo pode ter ficado parado: kill -CONT <pid>`).

- [x] **Passo 1: testes que falham:** `start_blocked_recovery_blocks_toggles`; `start_corrupt_goes_to_banner`; `connect_flow_waiting_connecting_connected`; `recover_runs_before_each_search` (a cada conexão, `recover` antes do `search_fn`, com `busy` desenhado); `connect_without_desired_never_applies`; `stale_search_result_is_dropped`; `op_during_search_drops_that_cheat_result`; `unresolved_cheat_keeps_state_on_research`; `auto_search_cadence_and_120s_limit` (com `took` de 4 s, o intervalo é 8 s; nada depois de 120 s); `disk_unreadable_connected_without_engine` (sem busca automática); `no_access_many_error_states`; `pid_fixed_gone_is_final`; `game_close_detected_while_idle`; `search_panic_is_reported` (`search_fn` que entra em pânico → `search_panicked()`); `research_other_identity_dropped` (`merge_search` devolvendo `false` não muda nada e volta a `Waiting`).
  - `core_raw.rs`: `raw_slow_search_responsive` (`search_delay` 3 s; `tick` e `handle(Quit)` voltam em menos de 500 ms (o dobro do limite da spec, por folga em máquina carregada) durante a busca; `handle` vem na Tarefa 12, então este teste entra com `#[ignore = "Tarefa 12"]` e a Tarefa 12 tira o `ignore`).
  - `fd_leak.rs`: `raw_50_reconnects_no_fd_leak` (50 ciclos de conectar, matar o alvo e reconectar a um alvo novo; `/proc/self/fd` no fim no máximo 2 acima do início).
- [x] **Passo 2:** os testes falham.
- [x] **Passo 3:** implementar.
- [x] **Passo 4:** os testes passam.
- [x] **Passo 5:** commit `feat(tui): controlador de conexão, busca e recuperação`.

---

### Tarefa 12: Controlador (2/4): desejados, ligar/desligar e religação

Spec 6.2, 6.5, 6.6, 6.7 (desligar).

**Arquivos:**
- Modificar: `apps/gt-tui/src/controller.rs`
- Teste: `controller.rs` (`mod tests`), `apps/gt-tui/tests/core_raw.rs`

**Interfaces:**
- Consome: Tarefa 11 (inclusive `note_op` e `wait_search`).
- Produz:
  - `pub struct Desired { pub k: u64, pub version_key: String }` (mapa `id → Desired` no controlador; o campo `CheatRow.desired` é espelho dele).
  - `fn handle(&mut self, action: Action, now: Instant, draw: &mut dyn FnMut(&Model)) -> bool`: devolve `true` se chamou a sessão (o laço então descarta teclas, Tarefa 14). Com `model.too_small`, só `Quit` e `Retry` fazem algo. Nesta tarefa, `Retry` só faz a nova busca dos desligados (reiniciando o prazo de 120 s) quando conectado, e `find` na hora sem conexão; a Tarefa 13 acrescenta os degraus anteriores.
  - `fn quit_requested(&self) -> bool`.
  - Regras: desenhar a mensagem de `busy` (`ligando <nome>…` etc.) **antes** de cada chamada da sessão e limpar depois; `note_op(id)` em toda operação num cheat; ligar com sucesso põe nos desejados com a `version_key` da conexão; falha não põe; desligar tira; espaço em indisponível escreve o motivo e, com `↻`, tira dos desejados; teclas de ligar bloqueadas com diário não resolvido e durante `Connecting`; as novas buscas automáticas também disparam enquanto houver desejado indisponível, e cada uma religa os desejados que aparecerem (mesma `version_key`); religar depois de conectar: lote com os desejados disponíveis da **mesma** `version_key`; se o lote vier `Refused`, cada um sozinho; `Gone` encerra a religação; `Residue` no lote marca como `erro` todos os `id` de `pending_ids()` (o motor deixa o lote inteiro pendente) e não tenta um a um; `Residue` no um a um marca aquele e segue com os demais; desejado de outra versão fica `↻` com o evento `versão nova; aperte espaço para ligar` (espaço liga e atualiza a versão guardada); aviso de não verificado uma vez por cheat por conexão; desfechos de ligar/desligar pela Tarefa 2 e pela spec 6.7 (`Refused` no desligar mantém `On` e desejado; `Foreign` → `Unavailable("alterado por outro")` e fora dos desejados; `Residue` → `residue = true`, `banners.residue`, fora dos desejados); jogo fechou no meio (`Gone`): desligar interrompido tira dos desejados, ligar interrompido não põe; `r` não faz nada em `Connecting`, `PidGone` e durante uma operação.

- [x] **Passo 1: testes que falham:** `toggle_on_adds_desired_and_off_removes`; `failed_toggle_not_desired`; `busy_drawn_before_session_call` (o `FakeBackend` confere que o último `Model` desenhado tem `busy` quando recebe `apply`); `toggles_blocked_while_connecting`; `game_close_keeps_desired_and_reconnect_reapplies`; `reapply_batch_then_one_by_one` (lote `Refused` → um a um; lote `Gone` → para; lote `Residue` → todos os pendentes em `erro`, sem um a um; um `Residue` no um a um não impede os outros); `reapply_only_same_version`; `close_during_off_removes_desired`; `close_during_on_does_not_add`; `undo_failures_follow_table`; `space_on_marked_unavailable_clears_desired`; `auto_search_stops_without_pending_desired`; `auto_search_reapplies_found_desired`; `unverified_warning_once_per_connection`; `too_small_only_quit_and_retry`; `retry_noop_states`; `op_during_search_drops_that_cheat_result` passa a usar `handle` em vez de `note_op` direto.
  - `core_raw.rs`: tirar o `ignore` de `raw_slow_search_responsive`; `raw_connect_toggle_reconnect` (liga o cheat de toggle, mata o alvo → `o jogo fechou` sem escrita, novo alvo: o teste troca `RawControl.pid` e `RawControl.defs` → religa sozinho); `raw_panic_in_main_loop_restores` (dois cheats ligados; um `catch_unwind` com o controlador dentro do fecho e um `panic!` depois de ligar; o `Drop` do controlador restaura: bytes originais no alvo e diário vazio).
- [x] **Passo 2:** os testes falham.
- [x] **Passo 3:** implementar.
- [x] **Passo 4:** os testes passam.
- [x] **Passo 5:** commit `feat(tui): desejados, ligar e desligar, religação`.

---

### Tarefa 13: Controlador (3/4): restos, SIGCONT e a tecla `r` completa

Spec 6.1 (diário não resolvido), 6.6 (`r`), 6.8 (restos e SIGCONT).

**Arquivos:**
- Modificar: `apps/gt-tui/src/controller.rs`
- Teste: `controller.rs` (`mod tests`)

**Interfaces:**
- Consome: Tarefas 11 e 12.
- Produz:
  - `Retry` na ordem da spec 6.6, só o primeiro degrau que se aplica: `is_stuck()` → `retry_resume`; restos (`pending_ids()` não vazio) → `restore_pending`; diário bloqueado → `recover` e, se `Clean` com o jogo conectado, `open_engine` e religar (regras da Tarefa 12); conectado → nova busca dos desligados (Tarefa 12); sem conexão → `find` na hora.
  - Espaço num cheat em `erro` → `restore_pending` (spec 6.8).
  - Depois de **toda** chamada da sessão: `is_stuck()` liga ou desliga `banners.stuck`; `pending_ids()` atualiza `residue` de cada linha e `banners.residue`.

- [x] **Passo 1: testes que falham:** `retry_order` (um caso por degrau); `restore_pending_clears_residue` (pelo `r` e pelo espaço: o cheat volta a `[OFF]` e `banners.residue` some); `stuck_banner_appears_and_clears`; `retry_opens_engine_when_diary_resolves` (diário `Blocked` → `r` com `recover` devolvendo `Clean` → `open_engine` e religação dos desejados da mesma versão); `retry_opens_engine_when_disk_becomes_readable` (conectado com `disk = Err`; `r` refaz a busca e ela volta com `disk = Ok`; `merge_search` guarda o disco novo e o `open_engine` abre o motor).
- [x] **Passo 2:** os testes falham.
- [x] **Passo 3:** implementar.
- [x] **Passo 4:** os testes passam.
- [x] **Passo 5:** commit `feat(tui): restos, SIGCONT e a tecla r`.

---

### Tarefa 14: Controlador (4/4): valores, teclas depois de uma operação e saída

Spec 6.2 (valor guardado), 6.6, 6.7, 6.8. Constante: 150 ms entre o início de duas trocas.

**Arquivos:**
- Modificar: `apps/gt-tui/src/controller.rs`
- Teste: `controller.rs` (`mod tests`), `apps/gt-tui/tests/core_raw.rs`

**Interfaces:**
- Consome: Tarefas 11 e 12, `ValueSpec::nearest_k`/`format` (Tarefa 4).
- Produz:
  - `Dec`/`Inc` mudam o `k` mostrado (limitado a `0..=steps()`), também com o cheat desligado, indisponível ou `↻`; o desejado guarda o `k` mostrado.
  - Fila de troca: com o cheat ligado, uma troca vai na hora se não houver troca em andamento e já passaram 150 ms do início da anterior; senão o `k` fica como alvo e o `tick` envia quando o intervalo passar, direto para o último alvo. Evento por `merge_value_event` (`<nome>: <antes> → <depois>`).
  - Desfechos da troca (spec 6.7): `Refused` → o `k` mostrado volta ao que está no jogo, alvo descartado; `Foreign` → `alterado por outro`, fora dos desejados; `Residue` → `erro`; `Gone` → o `k` mostrado fica guardado no desejado.
  - Espaço com troca pendente: desliga direto, sem enviar a troca; o `k` mostrado fica guardado.
  - Versão nova com bloco `value` diferente (por `ValueSpec` diferente na busca): `k` guardado vira `nearest_k(valor antigo)` e o evento diz o valor usado.
  - `fn after_op_keys(&mut self, keys: Vec<Action>) -> Vec<Action>`: das teclas lidas logo depois de uma operação, devolve só `Quit` e as setas.
  - `fn shutdown(&mut self, draw: &mut dyn FnMut(&Model)) -> (Vec<String>, Vec<String>)`: desenha `restaurando…`, abandona a busca em andamento sem esperar (só solta o `Receiver`), `undo_all` da sessão; depois disso o `main` faz `drop(controller)`.

- [x] **Passo 1: testes que falham:** `value_queue_rate_limited_and_ends_on_last` (três `Inc` em 50 ms → uma troca na hora e uma depois de 150 ms com o último `k`); `value_clamped_to_grid`; `value_change_while_off_only_updates_display`; `value_failures_follow_table`; `off_with_pending_value_skips_swap`; `value_queue_survives_game_close`; `value_regridded_on_new_version`; `after_op_keys_keeps_only_quit_and_arrows`; `shutdown_does_not_wait_for_search` (busca do `FakeBackend` dormindo 10 s; `shutdown` volta em menos de 1 s).
  - `core_raw.rs`: `raw_value_reapplied_after_reconnect` (troca para `0x33333333`, mata e reabre o alvo; depois de religar, a `value_word` do alvo novo chega a `0x33333333`); `raw_shutdown_restores` (dois cheats ligados, `shutdown`, `drop`: bytes originais no alvo, diário vazio); `raw_search_panic_restores` (um cheat ligado, `RawControl.search_panics = true`, `r` dispara a nova busca; `search_panicked()` fica `true`; `shutdown` e `drop` restauram os bytes).
- [x] **Passo 2:** os testes falham.
- [x] **Passo 3:** implementar.
- [x] **Passo 4:** os testes passam.
- [x] **Passo 5:** commit `feat(tui): troca de valor com limite de ritmo e saída restaurando`.

---

### Tarefa 15: `gt-tui`: terminal, log, saída e binário

Spec 6.1, 6.8, 6.9.

**Arquivos:**
- Criar: `apps/gt-tui/src/term.rs`, `src/logfile.rs`, `src/exit.rs`, `src/app.rs`, `src/main.rs`, `apps/gt-tui/tests/panic_hook.rs` (um teste só)
- Teste: `term.rs`, `logfile.rs`, `exit.rs`, `app.rs` (`mod tests`, com um `FakeTerm` e o `FakeBackend` de `fake.rs`, Tarefa 11), `tests/panic_hook.rs`

**Interfaces:**
- Consome: `Controller` (Tarefas 11–14), `GameBackend` (Tarefa 10), `render`/`too_small` (Tarefa 8), `gt_core::session::Shutdown`.
- Produz:
  - `pub trait Term { fn size(&self) -> io::Result<(u16, u16)>; fn draw(&mut self, model: &Model) -> io::Result<()>; fn read_keys(&mut self, timeout: Duration) -> io::Result<Vec<KeyEvent>>; fn restore(&mut self) -> io::Result<()>; }`; `pub struct CrosstermTerm` (tela alternativa, modo cru, cursor escondido).
  - `pub struct RestoreOnce { done: AtomicBool }` com `fn run(&self, f: impl FnOnce() -> io::Result<()>) -> io::Result<()>` (só a primeira chamada roda `f`) e `fn done(&self) -> bool` (depois de feito, o `main` não desenha mais).
  - `RestoreOnce::new() -> RestoreOnce` é `const fn`, para o `main` ter um `static RESTORE: RestoreOnce`.
  - `pub fn restore_terminal() -> io::Result<()>`: função livre do crossterm sobre o `stdout` (`disable_raw_mode`, `LeaveAlternateScreen`, `cursor::Show`); `CrosstermTerm::restore` chama a mesma função. O gancho de pânico do `main` usa `RESTORE.run(restore_terminal)`.
  - `pub fn install_panic_hook(main_thread: ThreadId, restore: Arc<dyn Fn() + Send + Sync>, elsewhere: Arc<AtomicBool>)`: guarda o gancho anterior (`take_hook`); na thread principal, chama `restore()` e depois o anterior; em outra thread, marca `elsewhere`, registra com `tracing::error!` e **não** chama o anterior (nada no terminal).
  - `pub fn open_log(dir: &Path) -> Option<File>`: renomeia `gt-tui.log` para `gt-tui.log.1` (substituindo) e cria um novo; `None` em qualquer erro. O `fmt` do `tracing-subscriber` usa `with_ansi(false)`.
  - `pub struct Summary { pub lines: Vec<String>, pub code: i32 }` e `pub fn summarize(store: &JournalStore, restored: &[String], foreign: &[String], panicked: bool) -> Summary`: relê o diário (`peek`; o `<pid>` do `kill -CONT` é o do diário relido); com entradas, a linha da spec 002 6.7 citando `gt-tui` e `gt-patch restore` e `code = 1`; com `paused_by_us`, `o jogo pode ter ficado parado: kill -CONT <pid>` e `code = 1`; pânico → `code = 101`; senão `0`.
  - `pub struct RunEnd { pub restored: Vec<String>, pub foreign: Vec<String>, pub panicked: bool }`; `panicked` é `true` em pânico no laço **e** quando a saída foi por `elsewhere` ou `search_panicked()` (código 101).
  - `pub fn app::run<T: Term, B: Backend>(term: &mut T, controller: &mut Controller<B>, shutdown: &Shutdown, restore: &RestoreOnce, elsewhere: &AtomicBool) -> RunEnd`: o laço abaixo dentro de `catch_unwind(AssertUnwindSafe(…))` (só o laço; o controlador continua com quem chamou), depois `controller.shutdown(draw)` sempre (inclusive depois de pânico, com `panicked = true`). O `draw` passado ao controlador é `|m| if !restore.done() { let _ = term.draw(m); }` e marca "terminal sumiu" na primeira falha.
  - `pub fn app::finish<T: Term, B: Backend>(term: &mut T, controller: Controller<B>, restore: &RestoreOnce, store: &JournalStore, end: RunEnd) -> Summary`: `drop(controller)` → `restore.run(|| term.restore())` (erro ignorado) → `summarize`. O `main.rs` só faz o início, chama `run` e `finish`, imprime e sai.
  - `main.rs` (com `anyhow`), na ordem da spec 6.1: `Shutdown::install` → trava (ocupada: mensagem e dica no `stderr`, código 2) → `isatty` de `stdin` e `stdout` (senão `o gt-tui precisa de um terminal` no `stderr`, código 2) → log (sem arquivo, sem saída de log e o evento `log em arquivo indisponível` no registro) → `install_panic_hook` com o `static RESTORE` → `CrosstermTerm` → `Controller::start` → `app::run` → `app::finish`. O laço de `run`: `too_small` pelo `size()`; `read_keys(100 ms)`; para cada tecla, `action` e `handle`; se `handle` devolveu `true`, as teclas restantes do lote e as de um `read_keys(Duration::ZERO)` passam por `after_op_keys`; `tick`; `draw`. Sai em `Quit`, `shutdown.requested()`, `elsewhere`, `search_panicked()`, erro de leitura do terminal ou **erro de `draw`** (terminal sumiu: sair em silêncio pelo caminho normal). Depois de `finish`, o `main` escreve as linhas no `stdout` com `let _ = writeln!` e também no log, e chama `std::process::exit(code)`. Opções `clap`: `--pid <PID>`, `--diagnostico`.

- [x] **Passo 1: testes que falham:** `app.rs`: `exit_restores_terminal_before_summary` (o `FakeTerm` grava `restore` dentro de `finish`, antes de devolver o `Summary`, e nenhum `draw` depois dele); `search_panic_gives_101` (o `FakeBackend` com busca que entra em pânico: `run` sai, `panicked = true`, `code == 101`); `panic_in_loop_gives_101` (o `FakeBackend` entra em pânico no `apply`; `run` devolve `panicked = true` depois de chamar `shutdown`; `finish` restaura antes do resumo; `code == 101`); `draw_eio_exits_quietly` (o `FakeTerm` falha o `draw` com `EIO`: `run` sai pelo caminho normal, `shutdown` chamado, código pelo diário); `restore_eio_keeps_code` (`restore` falha: o código continua o do diário); `quit_and_shutdown_signal_exit` (`Quit` e `shutdown.request()`); `keys_after_op_filtered`; `term.rs`: `restore_once_runs_once` (instância local); `logfile.rs`: `rotates_one_generation`, `unwritable_dir_gives_none`; `exit.rs`: `summary_codes` (diário vazio → 0; com entrada → 1 e a linha; com `paused_by_us` → 1 e `kill -CONT`; pânico → 101); `tests/panic_hook.rs`: `panic_hook_restores_on_main_and_flags_elsewhere` (um `FakeTerm` grava a ordem; pânico na thread principal dentro de `catch_unwind` chama `restore` antes do gancho anterior; pânico numa thread nova só marca `elsewhere` e não chama `restore`).
- [x] **Passo 2:** os testes falham.
- [x] **Passo 3:** implementar.
- [x] **Passo 4:** os testes passam; `cargo run -p gt-tui -- --help` mostra as duas opções.
- [x] **Passo 5:** commit `feat(tui): binário com terminal restaurado, log em arquivo e saída pelo diário`.

---

### Tarefa 16: Testes do binário em pseudo-terminal e documentos

Spec 8 (integração do binário), 10.

**Arquivos:**
- Criar: `apps/gt-tui/tests/bin.rs`
- Modificar: `docs/ARCHITECTURE.md` (3, 4 com `chrono`, 5.3, 5.4, 5.7, 8), `docs/specs/002-motor-patches.md` (nota na 6.8 e na 6.11), `docs/ROADMAP.md` (Fase 3 "em validação no jogo")

**Interfaces:**
- Consome: binário `gt-tui` (Tarefa 15), por `env!("CARGO_BIN_EXE_gt-tui")`.
- O pseudo-terminal é aberto com `nix::pty::openpty(Some(&Winsize { ws_row: 24, ws_col: 80, ws_xpixel: 0, ws_ypixel: 0 }), None::<&Termios>)` (escravo duplicado com `try_clone` para `stdin` e `stdout`; o `isatty` do `main` usa `std::io::IsTerminal`) (seguro, sem `unsafe`; sem o tamanho, o terminal fica 0×0 e a TUI só mostraria `aumente o terminal`); o filho recebe o escravo como `stdin`/`stdout` (`Stdio::from(OwnedFd)`) e o `stderr` num pipe; uma thread lê o mestre para um buffer compartilhado. Os sinais vão por `nix::sys::signal::kill`.

- [x] **Passo 1: testes** (todos com `XDG_STATE_HOME` temporário e pulando com o jogo aberto): `no_tty_exits_with_message` (sem pty: código 2, `stderr` com `o gt-tui precisa de um terminal`); `lock_busy_exits_with_message` (trava pega pelo teste, sem pty: código 2 e o texto da trava); `waiting_then_sigterm` (pty; em até 5 s a saída do mestre contém as palavras `aguardando` e `jogo` — o ratatui pode trocar espaços por movimentos de cursor, então nunca procurar a frase inteira; SIGTERM → código 0; a saída **contém** `\x1b[?1049l` (sair da tela alternativa); `stderr` vazio); `waiting_then_sigquit` (o mesmo com SIGQUIT); `sighup_with_closed_pty_restores` (fecha o mestre e o clone dele que a thread leitora usa, envia SIGHUP; o processo termina sem pânico, código 0, `stderr` vazio).
- [x] **Passo 2:** `cargo build --workspace && cargo test -p gt-tui --test bin` passa.
- [x] **Passo 3:** atualizar os documentos listados (registro de decisões da spec 003 seção 11 no `ARCHITECTURE.md` 8, incluindo "`gt-patch` e `gt-tui` com `write`").
- [x] **Passo 4:** validação completa das Restrições globais.
- [x] **Passo 5:** commit `test(tui): binário em pseudo-terminal; docs da fase 3`.

---

### Tarefa 17: Sessão com o jogo e critério de pronto (sessão principal)

Spec 9. **Não delegar.**

- [ ] **Passo 1:** pedir ao dono o **backup do save** e esperar a confirmação. Jogo fechado, FLiNG fechado.
- [ ] **Passo 2:** critério 9.1 a 9.5 com o dono, um passo por vez, conferindo com `gt diff` e `gt-patch list` num segundo terminal. Anotar quando a religação aconteceu no passo 5 e o tempo da busca (`gt-tui.log`, linha `busca dos padrões` com `duracao_ms`).
- [ ] **Passo 3:** `cargo test --workspace -- --ignored` com o jogo aberto (inclui `apps/gt-patch/tests/game.rs`, `apps/gt-tui/tests/game.rs` e `game_fd.rs`). Falha → causa no `RESEARCH.md` e voltar ao dono.
- [ ] **Passo 4:** o dono usa a TUI por alguns minutos e opina; ajustes pequenos de texto/layout entram aqui; mudança de comportamento vira emenda à spec.
- [ ] **Passo 5:** `RESEARCH.md` (tempo da busca, momento da religação, testes `#[ignore]`), `ROADMAP.md` (Fase 3 ✅ com a nota do valor provado só contra o alvo de teste), memória do projeto atualizada; commit `docs: fechamento da fase 3`.
