# Arquitetura — grounded-trainer

## 1. Visão geral

Trainer **externo**, binário nativo de Linux. Ele não roda dentro do Wine nem injeta nada no jogo (até a Fase 7 opcional). Funciona porque o jogo, mesmo rodando pelo Proton, é um processo Linux comum: o Wine carrega o `.exe` no mesmo endereço virtual que o Windows usaria.

```
┌─────────────── Linux (host) ────────────────┐
│                                             │
│  gt-tui / gt-cli  ──/proc/<pid>/mem──►  ┌───┴──────────────────────────┐
│  (Rust nativo)    ◄─process_vm_readv──  │ Grounded2Steam-Win64-        │
│                                         │ Shipping.exe (Wine/Proton,   │
│                                         │ container pressure-vessel)   │
│                                         └───┬──────────────────────────┘
└─────────────────────────────────────────────┘
```

## 2. Linguagem: Rust

| Critério | Decisão |
|---|---|
| Leitura/escrita de memória | Empate com Go |
| Montar/desmontar x86-64 (code caves, fronteiras de instrução) | **Rust**: `iced-x86` monta *e* desmonta; em Go só há desmontador |
| DLL Windows futura (Fase 7) | **Rust**: `cdylib` enxuta para `x86_64-pc-windows-gnu`; Go embutiria runtime e GC no processo do jogo |
| Structs com layout C (SDK do Unreal) | **Rust**: `#[repr(C)]` |
| Curva de aprendizado | Irrelevante: o código é escrito pela IA |

## 3. Workspace

```
grounded-trainer/
├── Cargo.toml                    # workspace
├── crates/
│   ├── gt-core/                  # genérico, não sabe nada do Grounded 2
│   │   └── src/
│   │       ├── error.rs          # Error (thiserror) e Result comuns
│   │       ├── process.rs        # achar processo, cmdline, ptrace_scope
│   │       ├── maps.rs           # parse de /proc/<pid>/maps (regiões, permissões, caminhos)
│   │       ├── addr.rs           # parse de endereços e tamanhos (hex e decimal)
│   │       ├── modules.rs        # Module, TrustPolicy, descoberta de imagens PE
│   │       ├── guard.rs          # guarda: recusa código fora dos módulos do jogo/Wine
│   │       ├── diff.rs           # agrupamento de diferenças entre leituras
│   │       ├── mem/              # ÚNICO lugar com unsafe: leitura/escrita
│   │       ├── pe.rs             # cabeçalhos PE lidos da memória (seções)
│   │       ├── scan.rs           # padrões AOB com curingas
│   │       ├── asm.rs            # wrappers iced-x86: disasm, montagem, relocação RIP-relative
│   │       ├── memscan.rs        # busca AOB na memória do processo e veredito
│   │       ├── procinfo.rs       # /proc/<pid>/stat, threads, posição das threads, boot_id, identidade
│   │       ├── patch.rs          # validação de patch, fronteiras de instrução, regra de perigo, política
│   │       ├── journal.rs        # diário em disco, trava OFD e decisão de recuperação (pura)
│   │       ├── suspend.rs        # pausar/retomar o processo com conferência das threads
│   │       ├── value.rs          # ValueGrid (grade sem slot: i32/u32/f32, passos, formatação, 4 bytes) e ValueSpec { grid, slot }
│   │       ├── global.rs         # globais por padrão AOB: alvo RIP-relativo e decisão de unicidade
│   │       ├── engine.rs         # motor: patches com diário e pausa; escrita em dados (bits, inteiro e set_word), sem pausa (feature write)
│   │       ├── recovery.rs       # recuperação do diário no início de cada comando (feature write)
│   │       └── session.rs        # sinais de encerramento, laço do run e toggle (feature write)
│   ├── gt-ue/                    # leitor genérico da reflexão do Unreal Engine 5 (Fase 5), só leitura
│   │   └── src/
│   │       ├── layout.rs         # UeLayout (offsets das estruturas de reflexão, vindos do [ue] das assinaturas)
│   │       ├── names.rs          # FNamePool: nomes por índice, com cache
│   │       ├── objects.rs        # GUObjectArray: cabeçalho, consulta por índice e varredura em lotes com pausa
│   │       ├── check.rs          # conferência barata do layout (nome 0, contagens, amostra de itens)
│   │       ├── index.rs          # ReflectionIndex: classes por (pacote, nome), campos (com caminho Struct.Campo), is_a
│   │       ├── dump.rs           # describe_class (gt ue class)
│   │       └── synth.rs          # reflexão sintética e memória esparsa para testes (feature synth)
│   ├── gt-grounded2/             # conhecimento específico do jogo
│   │   ├── src/catalog.rs        # busca dos padrões dos patches e das globais numa passada
│   │   ├── src/ue.rs             # estado das globais do Unreal e conferência barata (UeScan)
│   │   ├── src/player.rs         # caminho até o personagem e até o GameState, catálogo dos cheats de valor e plano das escritas (lock, bits, set, add)
│   │   ├── src/fields.rs         # execução dos cheats de valor e da ação no motor (feature write)
│   │   └── signatures/           # *.toml por build do jogo
│   └── gt-testkit/               # apoio a testes (publish = false): TestTarget, bin_path e binários de apoio
│       ├── src/bin/gt-testtarget.rs      # processo-alvo de teste (não é o gt)
│       ├── src/bin/gt-patch-harness.rs   # mesma biblioteca do gt-patch com patch cru (testes)
│       ├── src/ue.rs             # cena de reflexão sintética do gt-testtarget --ue e as definições dela
│       ├── tests/core_live.rs    # testes de API viva do gt-core contra o alvo
│       ├── tests/engine_live.rs  # testes do motor (patches, pausa, toggle, escrita em dados e bits) contra o alvo
│       ├── tests/ue_live.rs      # leitor de reflexão e caminho até o personagem contra o alvo
│       ├── tests/fields_live.rs  # execução dos cheats de valor contra o alvo
│       └── tests/harness.rs      # sinais, pânico, SIGKILL e recuperação pelo harness (patches e bits)
├── apps/
│   ├── gt-cli/                   # ferramentas de pesquisa: inspect, read, scan, disasm, diff, dump, fling, ue (class, player)
│   ├── gt-patch/                 # aplica/desfaz patches (list, run, toggle, restore, profile); com `write`
│   └── gt-tui/                   # interface do jogador: biblioteca + binário; com `write`
│       ├── src/model.rs, keys.rs, controller.rs   # núcleo da TUI: estado, teclas, conexão, desejados, fila de trocas de valor
│       ├── src/backend.rs        # interface de alvo (Backend)
│       ├── src/game.rs           # implementação do jogo (gt-grounded2)
│       ├── src/values.rs         # cheats de valor da sessão: tarefa de reflexão, disponibilidade e manutenção
│       ├── src/raw.rs            # implementação crua contra o gt-testtarget (feature test-backend)
│       ├── src/fake.rs           # implementação falsa do alvo para os testes unitários (#[cfg(test)])
│       ├── src/term.rs, ui.rs, app.rs, exit.rs, logfile.rs   # terminal (crossterm/ratatui), laço, saída, log
│       ├── src/main.rs           # binário: sinais, trava, terminal, log, gancho de pânico
│       └── tests/                # núcleo e binário em pseudo-terminal (bin.rs)
└── docs/
```

**Regra de dependência:** `gt-tui`/`gt-cli`/`gt-patch` → `gt-grounded2` → `gt-ue` → `gt-core`. Nunca o contrário. `gt-core` deve servir para outro jogo sem mudanças.

**Feature `write`:** só `gt-patch` e `gt-tui` a ativam. O `gt-cli` continua somente leitura, e `cargo tree -p gt-cli -e normal,build,features -i gt-core` não mostra `write` nem `test-support`.

## 4. Dependências principais

| Crate | Uso |
|---|---|
| `iced-x86` | Desmontar e montar x86-64 (code caves, cálculo de tamanho de instrução) |
| `goblin` | Interpretar cabeçalhos PE |
| `nix` | `process_vm_readv`, sinais (`SIGSTOP`/`SIGCONT`) |
| `ratatui` + `crossterm` | TUI |
| `clap` | CLI |
| `serde` + `toml` | Arquivos de assinatura e configuração |
| `thiserror` / `anyhow` | Erros (libs / binários) |
| `serde_json` | Saída em JSON das ferramentas do `gt` e diário de patches (`journal.json`) |
| `libc` | Chamadas de sistema sem wrapper no `nix` (ex.: `mmap`/`prctl` do alvo de teste) |
| `tempfile` | Diretórios e arquivos temporários nos testes (só `dev-dependencies`) |
| `tracing` + `tracing-subscriber` | Logs (em arquivo enquanto a TUI estiver aberta); o `gt-core` usa só `tracing` (avisos de extensão de módulo) |
| `signal-hook` | Restauração ao receber SIGINT/SIGTERM |
| `chrono` (`default-features = false`, `clock`) | Hora local (HH:MM) no registro de eventos do `gt-tui` |

As versões são fixadas no `Cargo.toml` na Fase 0, usando a estável mais recente.

## 5. Decisões técnicas

### 5.1 Acesso à memória
- **Leitura em massa** (varrer os ~128 MB da `.text`): `process_vm_readv`, que é rápido e não exige anexar ao processo (`PTRACE_ATTACH`), mas passa pela mesma checagem de permissão de ptrace (afetada pelo Yama).
- **Escrita**: `pwrite` em `/proc/<pid>/mem`. O kernel permite escrever em páginas somente leitura (código) quando o processo tem permissão de ptrace. `process_vm_writev` falharia nelas.
- **Permissão**: exige `kernel.yama.ptrace_scope = 0` (estado atual) ou `CAP_SYS_PTRACE`. O trainer detecta e explica o erro em vez de falhar silenciosamente.

### 5.2 Localização do alvo
- PID: varrer `/proc/*/cmdline` pelo nome `Grounded2Steam-Win64-Shipping.exe`, excluindo os processos `reaper`/`proton`/launcher.
- Base: primeira região do arquivo do executável em `/proc/<pid>/maps` com offset 0.
- Seções: lidas do cabeçalho PE **na memória**, não do disco.

### 5.3 Versão do jogo e assinaturas
- Chave de versão: campo `TimeDateStamp` do PE (hash de build reprodutível, ex. `c935e799`) + `SizeOfImage`.
- Assinaturas em `crates/gt-grounded2/signatures/*.toml`, embutidas no binário (`include_str!`); atualizar exige recompilar. Detalhes do comportamento na spec `002-motor-patches.md` (6.3 e 6.4).
  - `generic.toml` (obrigatório): um bloco `[[patch]]` por patch, válido para qualquer versão: `id`, `name` (nome curto em pt-BR para a lista da TUI, obrigatório), `category` (`sobrevivencia`, `pet`, `itens`, `progressao`, `movimento` ou `diagnostico`; obrigatória; a TUI mostra as categorias nessa ordem e os cheats na ordem do arquivo), `value` (opcional, faz do patch um cheat com valor: `type` `i32`/`u32`/`f32`, `slot`, `min`, `max`, `step`, `default`, `unit`, `decimals`; o `slot` fica inteiro dentro do imediato de uma instrução de `replacement` e tem exatamente o tamanho dele (imediato de tamanho diferente do tipo é recusado, e o deslocamento relativo de um desvio não conta como imediato); `u32` num imediato de 32 bits estendido com sinal só vai até `i32::MAX`; no `<chave>.toml` o bloco é sobrescrito inteiro), `description` (pt-BR), `module` (opcional; só o exe do jogo nesta fase), `pattern` (AOB com os bytes originais, mesma sintaxe do `gt scan`), `offset` (do início da ocorrência ao início do patch), `expected` e `replacement` (bytes em hex separados por espaço, mesmo tamanho).
  - `<chave>.toml` (ex.: `c935e799-0b27c000.toml`): `version` (igual ao nome do arquivo), `product_version` (opcional, informativo) e, por patch verificado naquela build, `[[patch]]` com `id` (já definido no genérico) e `rva` (início do patch relativo à base do módulo, aceita hexadecimal `0x...`); opcionalmente sobrescreve `pattern`, `offset`, `expected` e `replacement` quando a build mudou o código.
  - Regra de carga (erro claro com arquivo e campo, também depois das sobrescritas): `id` único; `module` igual ao exe do jogo; hex válido; `expected` e `replacement` do mesmo tamanho; o trecho `[offset, offset + tamanho)` cabe no `pattern`; cada byte fixo do `pattern` nesse trecho é igual ao de `expected` (curingas no trecho são permitidos).
- Versão com arquivo e `rva` conferido: patch "disponível (verificado)". Versão sem arquivo (ou sem entrada para o `id`) usa a definição genérica e, se o padrão for único na memória e no disco no mesmo lugar, fica "disponível (não verificado)".
- Cada patch informa o próprio estado (tabela da spec 002, 6.4): disponível (verificado ou não verificado), ativo, pendente, bytes diferentes, fora do lugar, não encontrado, ambíguo, inconclusivo ou disco ilegível.
- **Fase 5 (spec 005, 6.2 e 6.5):** o `generic.toml` também traz o bloco `[ue]`: os padrões das duas globais do Unreal (`gobjects` e `names`, com `disp_offset`, `insn_end` e `adjust` da resolução RIP-relativa), o `[ue.layout]` (offsets das estruturas de reflexão: `objects`, `uobject`, `ustruct`, `ffield`, `fprop`, `fbool`, `fstruct`) e o `[ue.player]` (classe do controller, campo do pawn, classe do pawn e campos alternativos, por nome). O `<chave>.toml` registra no `[ue]` o `rva` de cada global **resolvida** e pode sobrescrever o layout inteiro. Os cheats de valor são blocos `[[field]]`, separados dos `[[patch]]` (com `id` único entre os dois): objeto (campo do pawn ou `pawn`) e classe dele, regra `lock` (campo `float`, `mode` `raise`/`lower`/`exact`, alvo `target_field` (aceita `Struct.Campo`) ou `target` constante, faixa sã `min`/`max`) ou `clear_bits` (campos `bool`). Nenhum offset de campo do jogo vai às assinaturas: campos vêm da reflexão, por nome.
  - **Spec 006 (carga, dano, ciência e movimento):** o `[[field]]` ganha `kind = "int"` (`IntProperty`, 4 bytes; o `lock` continua só com `float`) e duas regras: `set` (troca um valor de configuração do jogo pelo nosso enquanto ligado e devolve o original ao desligar; `fields` com um ou mais campos, e **exatamente um** entre `[field.value]` e `target`) e `add` (uma ação única de somar, com `result_max`, sem diário e sem desfazer). O bloco `[field.value]` (`type` `i32`/`f32`, `min`, `max`, `step`, `default`, `unit`, `decimals`, **sem `slot`**) dá ao cheat de valor a mesma grade e as mesmas teclas dos patches com valor; com `target`, `min`/`max` só conferem o `target` na carga. O campo opcional `root` (`"pawn"`, padrão, ou `"game_state"`) escolhe a raiz do caminho; `"game_state"` só vale com `add` (até a spec 007) e usa os campos novos do `[ue.player]` (`level_class`, `world_field`, `world_class`, `game_state_field`, `game_state_class`). Dois `set` sobre o mesmo campo são erro de carga. Os `[[field]]` **continuam sem sobrescrita** no `<chave>.toml`: só existem no `generic.toml`.
  - **Spec 007 (construção e dano recebido):** o `set` passa a aceitar `kind = "bool"` (byte inteiro; `bools = [..]` com um valor por campo, só liga/desliga, sem `[field.value]`, `target`, `min` nem `max`) e `root = "game_state"` (`lock` e `clear_bits` continuam recusados nela). A chave de unicidade do `set` passa a ser (`root`, `object`, nome).
- **Unicidade das globais:** um padrão de global é aceito com uma ocorrência ou com várias que resolvem todas para o mesmo endereço (só para **leitura** de globais; patches continuam exigindo ocorrência única). Com `rva`, o endereço resolvido tem de ser `base + rva` (senão `fora do lugar`); sem `rva`, fica não verificado.

### 5.4 Escrita segura de patches
Detalhes na spec `docs/specs/002-motor-patches.md` (6.2 a 6.8).
1. **Validação:** mesmo tamanho, instruções completas, no máximo 64 bytes, dentro da política de escrita (intervalos permitidos, recalculados do `maps` a cada lote). Bytes atuais diferentes dos esperados recusam o lote.
2. **Diário antes de escrever:** a intenção (e o marcador "pausado por nós") vai para `$XDG_STATE_HOME/grounded-trainer/journal.json` por escrita atômica antes da pausa; o diário é atualizado depois de cada lote. Uma trava OFD em `journal.lock` garante uma instância por vez; ordem de todo comando que escreve: trava → recuperação → escritas.
3. **Pausa conferida:** `SIGSTOP` pelo `pidfd` e conferência, thread por thread, de que todas pararam (prazo de 500 ms). Se alguma thread está parada num ponto em que a troca de bytes muda a fronteira de instrução (regra de perigo, incluindo `pc - 2` em syscall), retoma e tenta de novo (até 10 tentativas, 20 ms entre elas). Releitura dos bytes e conferência de que tudo continua parado imediatamente antes de escrever; escrita por `/proc/<pid>/mem`, releitura e `SIGCONT`.
4. **Identidade:** PID **e** horário de início (`/proc/<pid>/stat`), conferidos antes de cada escrita; `pidfd` e descritor do `mem` abertos com a identidade conferida, de modo que um PID reaproveitado nunca recebe sinal nem escrita. Processo sumido, horário diferente ou zumbi: o jogo fechou e o registro é descartado sem escrever.
5. **Restauração:** o motor desfaz tudo no `undo_all` e no `Drop` (inclusive no desenrolar de um pânico). SIGINT/SIGTERM/SIGHUP/SIGQUIT só marcam o pedido (`signal-hook`); o laço do `run` desfaz e sai. SIGTSTP/SIGTTIN/SIGTTOU são ignorados.
6. **Troca de valor** (patch ativo com `value`): o diário guarda, antes da pausa, os bytes originais, os do valor atual (`replacement`, canônico) e os do valor novo (campo opcional `next`, só durante a troca). Com o jogo pausado, relê os bytes (precisam ser os do valor atual; senão recusa sem escrever e o patch sai do registro), escreve os do valor novo, relê, retoma e grava o diário só com o valor novo. Como o `slot` fica num imediato, as fronteiras de instrução são as mesmas nos dois lados. A recuperação aceita, por byte, o original, o do valor antigo ou o do valor novo. Diários da Fase 2 (sem `next`) continuam legíveis.
7. **`stuck` persistente:** se o `SIGCONT` falha, o marcador "pausado por nós" fica no diário e o estado `stuck` é mantido até um `SIGCONT` bem-sucedido; toda operação seguinte (inclusive o desfazer da saída e o `Drop`) tenta de novo antes de qualquer outra coisa. A TUI mostra `kill -CONT <pid>` e o `r` tenta de novo.
8. **Recuperação** (SIGKILL, travamento, queda de energia): no início de cada comando, o diário é conferido contra o `/proc` (`boot_id`, PID, horário de início). Outro boot ou processo: descarta. Mesmo processo: `SIGCONT` se estava pausado por nós e, para cada entrada em memória executável, reescreve os originais onde cada byte é o original ou o nosso; byte de outro → não escreve. O diário só é limpo quando tudo foi resolvido.
9. **Escrita em dados** (Fase 5, spec 005 6.6): um segundo tipo de escrita, **sem pausa**, em campos de dados (1 a 8 bytes), numa política própria (regiões `rw-` do `maps` fora das imagens de módulo, em cache e relida quando o objeto muda ou uma escrita falha). Toda escrita relê imediatamente antes uma guarda do objeto (vtable, `InternalIndex`, `ClassPrivate`) e recusa se ela mudou. Travas de barra não vão ao diário (não há original a restaurar). Bits limpos vão: uma entrada `DataEntry` por cheat e byte (byte original, escrito, máscara e guarda), gravada **antes** da escrita. Restaurar (desligar, `undo_all`, `Drop`, recuperação) só devolve os bits da máscara ao original, preservando os outros; objeto trocado, byte ilegível ou fora de região `rw` descartam a entrada sem escrever. A recuperação trata as entradas de dados por essa regra, e não pela política de memória executável. Diários antigos (sem `data`) continuam legíveis.
10. **Entradas de dados de N bytes** (spec 006, 6.8): a entrada de dados guarda, além dos bits da 005 (**1 byte** e máscara), o modo **inteiro** dos `set` (1 a 8 bytes; esta fase usa 4). `original` e `written` são sequências de bytes do mesmo tamanho, e `next` é opcional (só durante uma troca de valor). Restaurar no modo inteiro exige que **todos** os bytes atuais sejam iguais ao `written` (ou ao `next`) e escreve o `original` inteiro numa escrita só; qualquer byte diferente não escreve nada e só esquece a entrada, e nunca sobra um valor misturado. A identidade de uma entrada é o par (`id`, endereço), e não a estrutura inteira; atualizar (original novo na manutenção, `next` na troca) grava o diário antes da escrita e substitui a entrada do mesmo par. `Engine::set_word` escreve um campo `set` com esse protocolo e `Engine::restore_field` (antes `restore_bits`) resolve uma entrada de qualquer modo. A recuperação e o `gt-patch restore` seguem a mesma regra. Diários da 005 (um byte, sem modo) continuam legíveis como modo bits.

### 5.5 Code caves (Fase 6)
Sem `VirtualAllocEx` de fora do processo, existem duas estratégias:
- **Padrão: espaço livre do próprio executável**, ou seja, sequências de `CC`/`00` (preenchimento entre funções) na `.text`. Ficam perto do código (`jmp rel32`, ±2 GB) e não exigem alocação. O alocador de caves rastreia o que já foi usado.
- **Alternativa, se faltar espaço:** ptrace numa thread do jogo para executar um `mmap` dentro do processo. Funciona por ser um processo Linux, mas é mais arriscado; só se necessário e com spec própria.

As instruções originais copiadas para a cave passam por relocação (endereços relativos ao RIP são recalculados com `iced-x86`).

### 5.6 Valores via Unreal Engine (Fase 5)
- Offsets dos campos vêm da **reflexão do jogo em execução**: `GUObjectArray` e `FNamePool` resolvidos por assinatura
  AOB (na mesma passada da busca dos patches), classes achadas por pacote e nome e campos lidos da lista `FField`
  (nome, tipo, `Offset_Internal`, máscara dos `bool`; alvos dentro de structs por `Struct.Campo`). Não há SDK gerado
  nem arquivo de offsets de campo por versão; só os offsets das estruturas de reflexão ficam no `[ue.layout]`.
  Detalhes na spec `005-sobrevivencia-por-reflexao.md`; layout e campos observados em `RESEARCH.md`.
- A busca de padrões só resolve as globais e faz a conferência barata. A varredura da tabela de objetos (índice das
  classes, uma vez por conexão, e procura do controller, só com cheat de valor ligado ou desejado) roda numa **tarefa
  de fundo** da TUI, um trabalho de cada vez, em blocos de 1 MiB e lotes com pausa, com teto de 60 s; resultado de
  outra conexão é descartado.
- "Congelar" valores = a manutenção de ~10 Hz no **ciclo do laço principal** da TUI (onde moram o motor e o diário),
  nunca dentro do jogo nem numa thread própria. Ela revalida o caminho controller → pawn → componente a cada ciclo e
  não conta como operação.
- **Spec 006:** o cheat de valor ganha as regras `set` (original no diário, devolvido ao desligar, na saída e na
  recuperação; o valor que o jogo põe no campo durante o cheat vira o original novo, e mais de 5 originais novos em
  10 s param a escrita com `o jogo não deixa`) e `add` (ação única, no máximo uma por segundo, sem diário). A raiz
  `game_state` (controller → `Outer` → `Level.OwningWorld` → `World.GameState` → `PartyComponent`) tem **leitura
  própria**, sob demanda, fora do snapshot do pawn que a manutenção usa (até a spec 007); cada salto é conferido por classe. O
  **isolamento é por raiz e por cheat**: um componente nulo ou de classe errada deixa só os cheats dele em espera
  (`aguardando o personagem` ou `aguardando o pet`), e uma falha num caminho não afeta o outro. Detalhes na spec
  `006-progressao-e-movimento.md`.
- **Spec 007:** os `set` da raiz `game_state` (configurações do modo de jogo) são lidos a **cada ciclo** da manutenção (só quando algum `set` dessa raiz está ligado), independente do pawn, com a conferência do bit de CDO no objeto final; as duas raízes são isoladas (controller ausente ou inválido afeta as duas; as demais falhas do pawn não afetam o mundo, e uma falha no caminho do `game_state` não afeta os cheats do pawn). A espera do mundo tem marca própria (`aguardando o mundo`) e o registro de eventos tem `mundo perdido`/`mundo encontrado` (um por raiz). A procura do controller por pawn nulo não descarta o controller guardado. Detalhes na spec `007-construcao-e-defesa.md`.

### 5.7 Interface
- `gt-tui`: ratatui num terminal ao lado do jogo. Comandos por teclado **no terminal** (sem hotkeys globais, por causa do Wayland). Detalhes na spec `docs/specs/003-interface-tui.md`.
- **Tela:** lista única agrupada por categoria (`[ON ]`, `[OFF]`, `?` não verificado, `↻` religar pendente, `indisponível (<motivo>)`, `erro`), status de conexão no topo, aviso fixo para diário não resolvido ou jogo possivelmente parado, registro de eventos embaixo (200 linhas guardadas) e teclas no rodapé. Abaixo de 60×15 mostra só `aumente o terminal`.
- **Cheats de valor com valor e ação** (spec 006): os `set` com `[field.value]` mostram o valor como os patches com valor e aceitam `←→`/`hl` (com o cheat desligado, só mudam o valor mostrado). A ação de somar mostra `[ação]` e a quantidade no lugar de `[ON ]`/`[OFF]`, não entra na contagem de cheats ligados do topo e executa no máximo uma vez por segundo (tecla segurada soma uma vez por segundo); sem controller, recusa e pede a procura do controller, e a linha `personagem:` segue o controller guardado depois dela.
- **Teclas:** `↑↓`/`jk` movem; `espaço`/`Enter` liga e desliga; `←→`/`hl` trocam o valor (na hora, com intervalo mínimo de 150 ms entre trocas); `r` tenta SIGCONT, restos, diário ou nova busca, nessa ordem; `q`/`Esc`/Ctrl+C saem, desligando tudo sem confirmação.
- **Núcleo testável:** o controlador fala com o jogo por uma interface de alvo (`Backend`), com a implementação do jogo, uma crua (contra o `gt-testtarget`, atrás da feature `test-backend`) e uma falsa nos testes unitários; o terminal também é uma interface, com versão falsa para testar a restauração.
- **Reconexão:** procura o jogo a cada 2 s; depois de cada conexão, novas buscas a cada `max(5 s, 2 × duração da última)` por até 120 s; o `r` reinicia esse prazo de 120 s; cheats ligados antes da queda são religados sozinhos, só na mesma chave de versão (versão nova exige espaço). Com `--pid`, o fim do processo é um estado final. Disco do jogo ilegível = conectado sem motor, com todos os cheats em `indisponível (disco ilegível)` até um `r` com o disco legível.
- **Log em arquivo:** `$XDG_STATE_HOME/grounded-trainer/gt-tui.log` (uma geração anterior em `.log.1`, `RUST_LOG` com padrão `info`); nada vai ao `stdout`/`stderr` com a TUI desenhada. Erros de início (trava ocupada, sem terminal) saem no `stderr` com código 2; o resumo de saída vai ao `stdout`.
- **Leitura de teclas em thread própria:** com o terminal fechado (hangup), a leitura do `crossterm` repete o `read` com `EIO` sem parar e nunca devolveria o controle ao laço. A thread `gt-keys` lê e manda as teclas por um canal; o laço espera no canal e confere o hangup com `poll(2)` no `stdin`. Sai em `q`, SIGINT, SIGTERM, SIGQUIT, SIGHUP e `EIO`.
- **Saída:** desenha `restaurando…`, desfaz tudo, restaura o terminal uma vez só (marca compartilhada com o gancho de pânico) e imprime o resumo lendo o diário depois do `drop` do motor. O código de saída é 0 só se nada ficou no diário; fica em 1 com entradas restantes ou com o marcador de SIGCONT (jogo possivelmente parado), e em 101 em pânico.
- Hotkeys globais via evdev (`/dev/input`, exige o grupo `input`) ficam como melhoria futura, com spec própria.

## 6. Estratégia de testes

| Nível | Como | Exige jogo? |
|---|---|---|
| Unitário | Busca AOB, parse de padrão, montagem e relocação de patches sobre buffers sintéticos | Não |
| Integração local | Processo-alvo de teste (binário Rust simples) que o trainer modifica: valida leitura, escrita, pausa e restauração | Não |
| Jogo | `#[ignore]` + `cargo test -- --ignored` com o jogo aberto | Sim |
| Manual | Dono testa no jogo e confirma o efeito | Sim |

## 7. Ambiente de referência

- Fedora 44, kernel 7.2, Wayland, GPU RTX 3060 Laptop.
- Steam nativa (`~/.local/share/Steam`), Proton Experimental, Steam Linux Runtime 4.
- Grounded 2: AppID `2661300`, executável em `steamapps/common/Grounded2/Augusta/Binaries/Win64/`.
- Toolchain: `rustup` stable (Fase 0). Target `x86_64-pc-windows-gnu` + `mingw64-gcc` só na Fase 7.

## 8. Registro de decisões

| Data | Decisão | Motivo |
|---|---|---|
| 2026-10-22 | Rust em vez de Go | `iced-x86` para code caves; caminho para DLL na Fase 7 |
| 2026-10-22 | Trainer externo primeiro | Mais simples e seguro; nada é injetado no jogo |
| 2026-10-22 | TUI em vez de GUI | Simples, funciona bem no Wayland |
| 2026-10-22 | Assinaturas em TOML por versão | O jogo atualiza com frequência (Early Access) |
| 2026-10-25 | Extensão de módulo confiável = `min(SizeOfImage memória, disco)` | Um trainer poderia aumentar o `SizeOfImage` na memória para "cobrir" código dele (decisão do dono, spec 002) |
| 2026-10-25 | Binário separado `gt-patch`; o `gt` continua somente leitura | Escrita isolada atrás da feature `write`, que o `gt-cli` não ativa (dono) |
| 2026-10-25 | Pausa por SIGSTOP com conferência da posição das threads via `/proc/<tid>/syscall`, sem ptrace | Evita thread retomando no meio de instrução trocada (dono); posição transitória por anexo do `wineserver` é relida |
| 2026-10-25 | Diário em disco + recuperação automática na próxima execução | Cobre SIGKILL, travamento e queda de energia sem processo guardião (dono) |
| 2026-10-25 | Versão sem assinaturas: patch "não verificado" se o AOB for único em memória e disco e os bytes baterem | Não parar o trainer a cada atualização, sem abrir mão da conferência de bytes (dono) |
| 2026-10-25 | Identidade do processo por PID + horário de início + `boot_id`; sinais por `pidfd`; escrita pelo descritor do `mem` | Nunca escrever nem sinalizar um PID reaproveitado |
| 2026-10-25 | Escrita só nas seções executáveis (do disco) do módulo declarado pelo patch | Escrever é mais rígido que ler; o cabeçalho em memória poderia ser alterado |
| 2026-10-25 | `gt-patch` e `gt-tui` com a feature `write`; o `gt` continua só leitura | Spec 003: a TUI aplica patches pelo mesmo motor do `gt-patch`; a escrita segue isolada atrás da feature (decisão do dono) |
| 2026-10-25 | Motor no próprio processo do `gt-tui` (feature `write`), em vez de chamar o `gt-patch`; trava do diário com a TUI do início ao fim (outra instância recusa; `gt-patch list` funciona em paralelo) | Uma só escrita no jogo por vez e restauração garantida pelo mesmo motor (spec 003, dono) |
| 2026-10-25 | Sempre começa tudo `OFF`; nada é lembrado entre sessões; depois de reconectar, religa sozinho o que estava ligado (só na mesma chave de versão) | Não ligar nada que o dono não pediu; recuperar o que ele ligou depois de o jogo reiniciar (spec 003, dono) |
| 2026-10-25 | `q` desliga tudo e sai sem confirmação; patch de teste só com `--diagnostico`; não verificado liga normal, com `?` e aviso | Spec 003, dono |
| 2026-10-25 | Troca de valor vai para o jogo a cada tecla, com intervalo mínimo de ~150 ms entre o início de duas trocas, terminando no último valor mostrado | Cada troca pausa o jogo ~15 ms (spec 003, dono) |
| 2026-10-25 | Valor só em imediato de uma instrução (tamanho exato, fora de desvios relativos), tipos `i32`/`u32`/`f32`; `ValueSpec` e a montagem dos bytes num módulo genérico do `gt-core` | Nenhum valor muda a fronteira de instrução, então a pausa não tem ponto perigoso novo (spec 003, 6.3) |
| 2026-10-25 | Troca de valor numa pausa só, com três conjuntos de bytes no diário (`replacement` canônico e `next` opcional) e na recuperação; no máximo uma troca em andamento e um alvo pendente | Recuperar uma troca interrompida sem apagar o que é de outro (spec 003, 6.7) |
| 2026-10-26 | Cheats de sobrevivência por escrita de valor, com campos achados pela reflexão do jogo por nome; layout de reflexão e padrões das globais no `[ue]`, sobrescritível por versão | Os offsets vêm do próprio jogo e sobrevivem a atualizações (spec 005) |
| 2026-10-26 | Padrão de global aceito com várias ocorrências que resolvem para o mesmo endereço (só leitura de globais) | Dono, spec 005 6.2 |
| 2026-10-26 | Barras travadas no máximo, sem valor ajustável; ao desligar ficam onde estão; nunca baixar uma barra acima do alvo; alvo relido do personagem a cada ciclo | Dono e IA, spec 005 6.5.1 |
| 2026-10-26 | Vida infinita por `CurrentDamage = 0`; "Não morrer" separado, pelos bits do `HealthComponent`; sem dano de queda por `bTakeFallDamage` do personagem | Dono e IA, spec 005 6.5 |
| 2026-10-26 | Escrita em dados sem pausa; bits no diário com guarda do objeto por bytes (vtable, `InternalIndex`, `ClassPrivate`); uma entrada por cheat e byte, a antiga resolvida antes de criar a nova | Spec 005 6.6 |
| 2026-10-26 | Índice da reflexão e procura do controller numa tarefa de fundo, com os cheats de valor em `verificando…` até o índice; manutenção de ~10 Hz no laço principal | A conexão e os cheats de patch não ficam mais lentos; o motor mora no laço principal (spec 005 6.3) |
| 2026-10-26 | Pawn escrito só se `is_a(SurvivalPlayerCharacter)`; montado, o personagem vem do `CachedSurvivalPlayerCharacter` do controller | Uma montaria no `Pawn` nunca é escrita (spec 005 6.3, P7) |
| 2026-10-26 | Comandos `gt ue class` e `gt ue player` (com `--controller` e `--watch`), só leitura | Pesquisa e conferência depois de atualizações (spec 005 6.8) |
| 2026-10-26 | Cheats do pet (vida, não morre, stamina) na categoria `PET`, pelo caminho `PetMasterComponent.CurrentPetFull.<componente>` com a classe de cada salto conferida (`PropertyClass`); sem o pet no mundo, só eles esperam (`aguardando o pet`) | Pedido do dono, sem spec por decisão dele; a regra "nunca escrever em criaturas" da spec 005 passa a ter a exceção do pet apontado pelo próprio personagem |
| 2026-10-26 | Teleporte e velocidade do jogo (speedhack) fora da Fase 5 | Dono, spec 006: o jogo tem viagem rápida e o dono quer explorar |
| 2026-10-26 | Regra `set`: original no diário antes da escrita, devolvido ao desligar; valor mudado pelo jogo durante o cheat vira o original novo; mais de 5 em 10 s para de escrever (`o jogo não deixa`) | IA, spec 006 6.4: não devolver um original velho por cima de um upgrade, sem brigar com o jogo |
| 2026-10-26 | Num `set`, o original devolvido é o último valor que o jogo pôs no campo, e não o do momento de ligar | IA, aprovada pelo dono, spec 006 7 |
| 2026-10-26 | Entrada de dados do diário de N bytes (até 8), modos bits e inteiro, com `next` na troca; restauração tudo-ou-nada no modo inteiro; identidade da entrada pelo par (`id`, endereço); diários da 005 legíveis | IA, spec 006 6.8: nunca sobra um valor misturado nem uma cópia velha |
| 2026-10-26 | Grade de valores (`ValueGrid`) separada do `slot` no `gt-core`; `[field.value]` sem `slot`; `[[field]]` continua sem sobrescrita no `<chave>.toml` | IA, spec 006 6.5 |
| 2026-10-26 | Ciência como ação de somar (`add`), sem trava e sem desfazer, sabendo que vai para o save; no máximo uma execução por segundo; faixa sã com `result_max` | Dono (ação, ciência no save) e IA aprovada pelo dono (limite e teto), spec 006 6.6 e 7 |
| 2026-10-26 | Raízes independentes (pawn e `game_state`), cada uma com leitura própria; isolamento por cheat para todo componente; `root = "game_state"` só com `add` | IA, spec 006 6.3 |
| 2026-10-26 | Velocidade pelos multiplicadores do próprio jogo (`CustomGroundSpeedMultiplier`/`CustomSwimSpeedMultiplier`), não pelo `MaxWalkSpeed`; pulos por `JumpMaxCount = 1000` | IA, spec 006 6.1 e 6.7.3: o `MaxWalkSpeed` não é a velocidade de corrida |
| 2026-10-26 | Ressalvas novas no `AGENTS.md` 3 (ação sem desfazer, `set` sem bytes esperados) e valores acima da faixa normal do jogo nos `set` (dano ×100, carga 100, `JumpMaxCount` 1000) | IA, aprovada pelo dono, spec 006 4 e 7: o original volta ao desligar |
| 2026-10-26 | Carga baixada abaixo dos itens carregados enquanto ligado: risco aceito de o jogo soltar ou perder itens (o teste anota); carga com grade a partir de 1 | IA, aprovada pelo dono, spec 006 6.7.2 |
| 2026-10-26 | Teto da ciência (`result_max`) em 100000 | IA, aprovada pelo dono, spec 006 7 |
| 2026-10-26 | Faixas e padrões: carga 1–100 (20), dano ×1–×100 (×10), ciência +100–+10000 (+1000), velocidade ×0,5–×4 (×2), gravidade ×0,1–×2 (×0,5) | IA, aprovada pelo dono, spec 006 6.7 |
| 2026-10-26 | Dano, velocidade, gravidade e carga com valor ajustável pelas setas; ao desligar volta o original do jogo | Dono, spec 006 11 |
| 2026-10-26 | `o jogo não deixa`: o cheat para de escrever naquele objeto até mudar o objeto ou desligar e ligar, sem nova tentativa por tempo | IA, aprovada pelo dono, spec 006 6.4 |
| 2026-10-26 | Pulos infinitos por `JumpMaxCount = 1000`, sem valor ajustável | IA, spec 006 6.7.3 |
| 2026-10-26 | Ciência pelo `GameState` (`root = "game_state"`), com cada salto conferido por classe | IA, spec 006 6.3: o `CachedPartyComponent` é nulo |
| 2026-10-26 | Montado: cheats de movimento só do personagem, sem efeito na montaria | IA, aprovada pelo dono, spec 006 6.7.3 |
| 2026-10-25 | `name` e `category` obrigatórios nas assinaturas; bloco `value` sobrescrito inteiro por versão | Lista agrupada e sem campos pela metade (spec 003, 6.3) |
| 2026-10-25 | Núcleo da TUI testável por uma interface de alvo (jogo, crua e falsa) e por uma interface de terminal com versão falsa | Testar conexão, reconexão, sinais e restauração sem o jogo |
| 2026-10-25 | `stuck` mantido até um `SIGCONT` bem-sucedido, com nova tentativa antes de cada operação; restos de falha viram `erro` e só a recuperação (`r`) resolve | Nunca deixar o jogo parado sem aviso (spec 003, 6.8) |
| 2026-10-25 | Recuperação do diário no início e a cada conexão; diário não resolvido bloqueia as escritas sem fechar a TUI; `r` tenta de novo | Um motor novo regravaria o diário e apagaria entradas não resolvidas (spec 003, 6.1) |
| 2026-10-25 | Operações do motor no laço principal, com a mensagem desenhada antes; busca dos padrões em thread separada; teclas de ação durante uma operação são descartadas (menos `q` e as setas de valor) | Resposta da tela em até ~250 ms sem liga-desliga acumulado com o jogo pausado (spec 003, 6.10) |
| 2026-10-25 | Terminal restaurado uma vez só, por marca compartilhada; pânico fora da thread principal não mexe no terminal; código de saída decidido relendo o diário depois do `drop` do motor | Sem terminal quebrado e sem código de saída enganoso (spec 003, 6.8) |
| 2026-10-25 | Log só em arquivo enquanto a TUI está aberta, iniciado antes da recuperação, com uma geração anterior; trava antes da conferência do terminal | Avisos da recuperação ficam no log; um segundo `gt-tui` recusa sem tocar o terminal (spec 003, 6.1 e 6.9) |
| 2026-10-25 | Código de terceiros pode ser desmontado e decompilado para entender o que faz; copiar ou redistribuir continua proibido | Entender as caves do FLiNG acelera a pesquisa; a implementação segue nossa, sem código deles (decisão do dono, `AGENTS.md` 3) |
| 2026-10-26 | Teclas lidas numa thread própria (`gt-keys`) e hangup conferido com `poll(2)` no `stdin` | A leitura do `crossterm` repete `EIO` sem parar num terminal fechado e nunca devolveria o controle ao laço |
| 2026-10-26 | Teste do binário em pseudo-terminal (`openpty` do `nix`, sem `unsafe`), com `--pid` de um processo já terminado nos testes de sinal e restauração | Roda mesmo com o jogo aberto; só o teste de `aguardando o jogo` pula quando ele está aberto |
| 2026-10-26 | Fases 4–6 reorganizadas: Fase 4 só com trocas de bytes do mesmo tamanho (durabilidade, energia, recarga); valores na Fase 5; desvios na Fase 6 | A análise do FLiNG mostrou que os cheats da Fase 4 antiga precisam do pawn do jogador ou de código novo (decisão do dono) |
| 2026-10-26 | Offsets da Fase 5 lidos da reflexão do jogo em execução, sem Dumper-7 | Não depende de regenerar SDK a cada atualização; `GUObjectArray`, `FNamePool` e o layout de `FProperty` já foram localizados (decisão do dono) |
| 2026-10-27 | Construir sem ingredientes só com o que já foi desbloqueado, num cheat só (sem ingredientes e construção pronta na hora); defesa como `EnemyDamageMultiplier` (tentativa; se não funcionar, volta à Fase 6) | Dono, spec 007 |
| 2026-10-27 | Caminho de valor pelas configurações do modo de jogo (`GameModeSettings`), aceitando o risco de save | Dono, spec 007 |
| 2026-10-27 | `set` liberado na raiz `game_state`, com leitura a cada ciclo e isolamento por raiz; chave de unicidade do `set` = (`root`, `object`, nome) | IA, spec 007 6.2 e 6.3 |
| 2026-10-27 | `set` com `bool` de byte inteiro por função nova (`byte_bool_field`), valor por campo, só liga/desliga, sem mudar o `gt-ue` nem o `bool_field` do `clear_bits`; caminho do `set` de 1 ou 4 bytes | IA, spec 007 6.4 |
| 2026-10-27 | Procura do controller por pawn nulo (30 s) sem descartar o controller guardado; risco aceito de os cheats do `game_state` escreverem no mundo que saiu até a procura terminar | IA, spec 007 6.3 |
| 2026-10-27 | Construção em ITENS e "Dano recebido" (grade 0 a 1, passo 0,05, padrão 0,25) em SOBREVIVÊNCIA; `gt ue player` mostra o `game_state` mesmo com o pawn falhando | IA, spec 007 |
