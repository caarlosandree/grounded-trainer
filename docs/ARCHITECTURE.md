# Arquitetura — grounded-trainer

## 1. Visão geral

Trainer **externo**, binário nativo de Linux. Ele não roda dentro do Wine nem injeta nada no jogo. Funciona porque o jogo, mesmo rodando pelo Proton, é um processo Linux comum: o Wine carrega o `.exe` no mesmo endereço virtual que o Windows usaria.

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
│   │       ├── pe.rs             # cabeçalhos PE lidos da memória (seções) e `.pdata` (funções e unwind encadeado) do disco
│   │       ├── scan.rs           # padrões AOB com curingas
│   │       ├── asm.rs            # wrappers iced-x86: disasm, montagem, relocação RIP-relative; prova de prefixo sem pilha (observe)
│   │       ├── cave.rs           # sequências de CC, alocador de caves, desvio `jmp rel32`, relocação e conferência do código da cave (funções puras)
│   │       ├── memscan.rs        # busca AOB na memória do processo e veredito
│   │       ├── procinfo.rs       # /proc/<pid>/stat, threads, posição das threads, boot_id, identidade
│   │       ├── patch.rs          # validação de patch (`Code` e `Cave`), fronteiras de instrução, regra de perigo, política
│   │       ├── journal.rs        # diário em disco (entradas com `kind`, regiões de variáveis), trava OFD e decisão de recuperação (pura)
│   │       ├── suspend.rs        # pausar/retomar o processo com conferência das threads
│   │       ├── value.rs          # ValueGrid (grade sem slot: i32/u32/f32, passos, formatação, 4 bytes) e ValueSpec { grid, slot }
│   │       ├── global.rs         # globais por padrão AOB: alvo RIP-relativo e decisão de unicidade
│   │       ├── engine.rs         # motor: patches (código e cave) com diário e pausa, variáveis de cave; escrita em dados (bits, inteiro, set_word e "maior ou igual"), sem pausa (feature write)
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
│   │   ├── src/catalog.rs        # busca dos padrões dos patches, dos desvios e das globais numa passada
│   │   ├── src/policy.rs         # política de escrita do jogo, folga da `.data` (variáveis) e alocador de caves sobre a `.text` do disco
│   │   ├── src/caves/            # corpos das caves em Rust (iced-x86): brain, analyzer, collect, observe; tabela fixa das variáveis (`var::*`)
│   │   ├── src/hooks.rs          # HookRuntime: disponibilidade, ligar/desligar, valor, inventário do jogador (coleta) e registro do Brain Power (feature write)
│   │   ├── src/ue.rs             # estado das globais do Unreal e conferência barata (UeScan)
│   │   ├── src/player.rs         # caminho até o personagem e até o GameState, catálogo dos cheats de valor e plano das escritas (lock, bits, set, add)
│   │   ├── src/fields.rs         # execução dos cheats de valor e da ação (inteira ou `double`) no motor (feature write)
│   │   └── signatures/           # *.toml por build do jogo
│   └── gt-testkit/               # apoio a testes (publish = false): TestTarget, bin_path e binários de apoio
│       ├── src/bin/gt-testtarget.rs      # processo-alvo de teste (não é o gt)
│       ├── src/bin/gt-patch-harness.rs   # mesma biblioteca do gt-patch com patch cru (testes)
│       ├── src/ue.rs             # cena de reflexão sintética do gt-testtarget --ue e as definições dela
│       ├── src/module.rs         # módulo sintético (imagem PE mínima com `.text`, `.pdata`, `CC`, folga anônima da `.data` e funções-alvo) do gt-testtarget --module
│       ├── src/rig.rs            # SynthRig: alvo, módulo, disco, diário e motor montados como no gt-patch
│       ├── tests/core_live.rs    # testes de API viva do gt-core contra o alvo
│       ├── tests/engine_live.rs  # testes do motor (patches, pausa, toggle, escrita em dados e bits) contra o alvo
│       ├── tests/ue_live.rs      # leitor de reflexão e caminho até o personagem contra o alvo
│       ├── tests/fields_live.rs  # execução dos cheats de valor contra o alvo
│       ├── tests/hooks.rs, cave_bodies.rs, hook_runtime.rs, synthetic.rs   # desvios, corpos das caves e HookRuntime contra o módulo sintético
│       └── tests/harness.rs      # sinais, pânico, SIGKILL e recuperação pelo harness (patches e bits)
├── apps/
│   ├── gt-cli/                   # ferramentas de pesquisa: inspect, read, scan, disasm, diff, dump, fling, caves, ue (class, player)
│   ├── gt-patch/                 # aplica/desfaz patches (list, run, toggle, restore, profile) e `observe` (cave de observação); com `write`
│   └── gt-tui/                   # interface do jogador: biblioteca + binário; com `write`
│       ├── src/model.rs, keys.rs, controller.rs   # núcleo da TUI: estado, teclas, conexão, desejados, fila de trocas de valor
│       ├── src/backend.rs        # interface de alvo (Backend)
│       ├── src/game.rs           # implementação do jogo (gt-grounded2)
│       ├── src/values.rs         # cheats de valor da sessão: tarefa de reflexão, disponibilidade e manutenção
│       ├── src/hooks.rs          # desvios da sessão: cola com o HookRuntime, disponibilidade (`verificando…`) e erros do runtime
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
  - **Spec 008 (code caves):** o `generic.toml` ganha blocos **`[[hook]]`**, separados dos `[[patch]]` e dos `[[field]]` (com `id` único entre os três). Um `[[hook]]` descreve um **desvio de uma instrução**: `id`, `name`, `category`, `description`, `module` (opcional), `pattern` (AOB único, com curingas para o que a cave lê da própria instrução), `offset` (da ocorrência à instrução desviada, de 5+ bytes), `body` (`brain`, `analyzer` ou `collect`: qual corpo de cave em Rust monta o código; o `collect` é o desvio na `+5615f30`, a implementação nativa do `ServerAddItem`, com `object_class = Item` e grade `[2,3,5,10]`) e `object_class` (caminho da classe conferida pela cave; obrigatório em `analyzer` e `collect`, recusado em `brain`, que usa a `party_class` do `[ue.player]`). Não há `expected` nem `replacement`: os bytes esperados são os da instrução achada (conferidos contra o disco) e o código vem do corpo. O valor é um bloco `[hook.value]` **só por `points`** (`type`, `points`, `default`, `unit`, `decimals`; sem `min`/`max`/`step`/`slot`), ausente no analisador. Não há sobrescrita por `<chave>.toml`: o desvio é sempre achado pelo padrão (único na memória).
  - **Grade por lista (`points`):** `[field.value]` e `[hook.value]` aceitam `points = [...]` no lugar de `min`/`max`/`step`: lista estritamente crescente, com o `default` dentro dela; um ponto só é aceito (sem `←→`). `type` ganha `f64`, **só no `add` de `double`**. Um `label` opcional no `[field.value]` troca o texto da quantidade (`+1h`). Patches com `slot` e as grades antigas não mudam.
  - **`add` de `double` e classe dona:** o `add` aceita `kind = "double"` (8 bytes; grade `f64`; `kind = "float"` continua recusado), e `at_least = true` troca a releitura exata por **"maior ou igual"** (o relógio do jogo anda a cada quadro, então a releitura nunca é exata). O `[[field]]` ganha `owner_class` (só com `root = "game_state"`): a classe que **declara** o primeiro salto (`SurvivalGameState`, filha da `game_state_class`), conferida a cada leitura no `GameState` vivo. `pular-hora` e `congelar-hora` usam `CalendarComponent` por esse caminho.
  - **`[ue.player]`:** campos novos `party_field`/`party_class`/`research_points_field` (`ResearchPoints` do `PartyComponent`, para o `gt ue player` e para o Brain Power conferir o offset do padrão) e `inventory_field` (`InventoryComponent` do personagem, para a coleta).
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
11. **Entradas de cave** (Fase 6, spec 008 6.3 e 6.4): a entrada de patch do diário ganha o campo `kind` (`code`, o padrão, ou `cave`); ele só é gravado para `cave`, então diários antigos e entradas de código ficam byte a byte iguais. Uma entrada `cave` guarda endereço e bytes escritos **antes** da escrita, como as outras. Tamanho: 1 a 64 bytes para `code` e 1 a 4096 para `cave` (a recuperação usa o mesmo limite por tipo). A recuperação (e o `gt-patch restore`) resolve as entradas **em duas fases**: primeiro todas as `code`, depois as `cave`, com a mesma pausa, a conferência do `pc` e as tentativas da saída (ordem das operações na 5.5). A cave só é tentada se nenhuma `code` ficou sem resolver (um desvio vivo ainda aponta para ela); senão ela fica no diário e entra em `kept`/`deferred` para a próxima execução. Um `code` com bytes de outro sai do diário e não segura a cave.
12. **Variáveis das caves** (spec 008 6.4): o diário ganha a lista `vars`, de **regiões** (`addr` em hexadecimal e `len`), sem os valores (a região é de uso exclusivo nosso, e nada guarda um "original" dela). Diários sem `vars` continuam legíveis. A recuperação valida cada região antes de adotá-la (1 a 4096 bytes, início múltiplo de 8, sem estouro, sem sobreposição; a recusada sai do diário sem escrita), confere a identidade do processo (boot, PID e início) e que a região inteira continua `rw-` privada no `maps`, e a zera. O `Engine::open` adota as regiões de um diário do mesmo processo. A região só é zerada quando **nenhuma** entrada nossa (`code` ou `cave`) está ativa ou pendente.

### 5.5 Code caves (Fase 6)
Sem `VirtualAllocEx` de fora do processo, existem duas estratégias:
- **Padrão: espaço livre do próprio executável**, ou seja, sequências de `CC` (preenchimento entre funções) na `.text`. Ficam perto do código (`jmp rel32`, ±2 GB) e não exigem alocação. No build atual só há 4 sequências com 64+ bytes (a de 1871 bytes cabe tudo). O alocador (`gt-core/src/cave.rs`) deixa **16 bytes `CC` intactos** no começo de cada sequência, divide a sequência entre várias caves sem sobreposição (uma sequência serve se `tamanho ≥ cave + 16`) e é alimentado, no jogo, pelas sequências da `.text` do **disco**; as caves do diário são reservadas nele (`reserve`) para um runtime novo não sobrepor uma cave já escrita.
- **Alternativa, se faltar espaço:** ptrace numa thread do jogo para executar um `mmap` dentro do processo. Funciona por ser um processo Linux, mas é mais arriscado; só se necessário e com spec própria.

**Desvio de uma instrução.** O desvio troca **uma instrução inteira** de 5+ bytes por `jmp rel32` para a cave, mais `NOP` até o fim da instrução (`hook_bytes`). A cave executa o nosso código, a instrução original **relocada** pelo `iced-x86` (`BlockEncoder`: endereços relativos ao RIP e desvios relativos são recalculados) e `jmp` de volta para a instrução seguinte. O corpo de cada cave é código Rust com `iced-x86` (`code_asm`, em `gt-grounded2/src/caves/`: `brain`, `analyzer`, `collect`, `observe`); o TOML só dá o ponto e o valor. Os offsets de campo vêm de curingas do padrão (a própria instrução) ou da reflexão, nunca de constantes. Toda cave salva as flags e os registradores que toca e passa por `check_cave_code` depois de montada: sem `call`, `ret`, `int3`, desvio indireto ou salto para fora da cave, terminando no `jmp` de volta. Como a cave fica fora da `.pdata` (um acesso inválido ali derruba o jogo), ela só desreferencia ponteiros que o próprio jogo desreferencia no ponto ou objetos que a pesquisa mostrou serem sempre válidos ali: a subida `ClassPrivate` → `SuperStruct` (até 17 comparações, com ponteiro nulo recusado) e a tabela de níveis do Brain Power.

**Desvio e cave são patches do motor da Fase 2.** O desvio é um patch `PatchKind::Code` (mesmas garantias: bytes esperados, pausa com conferência do `pc`, diário, restauração em toda saída). A cave é um patch `PatchKind::Cave`: `expected` todo `CC`, de 1 a 4096 bytes, escrita por `/proc/<pid>/mem`, e **perigosa para qualquer `pc` dentro dela, nos dois sentidos**. Os ids do motor são `<id>-hook` e `<id>-cave`. As **variáveis** (valor do cheat, ponteiros de classe, contadores, anel da observação) ficam na folga entre o `VirtualSize` e o fim da última página da `.data` (`0x130` bytes no jogo), numa tabela fixa de 280 bytes alinhada a 8: o motor as reclama (`claim_vars`: região `rw-` privada no `maps`, alinhada a 8 e com todos os bytes zero no primeiro uso; que ela está dentro do módulo é conferido por `policy::var_region`, quem a calcula), o trainer escreve sem pausa (`write_var`) e lê (`read_var`), e a limpeza zera a região inteira.

**Ordem das operações:**
- **Ligar:** reclama as variáveis, grava as classes e o valor, e escreve cave e desvio num lote, **cave primeiro**. A cave também é escrita **sob pausa e com o `pc` conferido**, **no mesmo lote e na mesma pausa do desvio** (mais estrito que a spec 008 6.3, que a escreveria sem pausa: a pausa passa a ler a posição das threads e a conferir o `pc` contra a cave nova).
- **Desligar:** só o desvio volta (`undo` de `<id>-hook`). Cave e variáveis **ficam** até sair (uma thread dentro da cave termina e volta ao código do jogo); religar reaproveita a cave já escrita (um runtime novo só a reaproveita se o corpo montado for byte a byte igual).
- **Sair** (`undo_all`, `Drop`, SIGTERM, pânico, jogo fechando): **duas fases**. Fase 1: todos os desvios. Fase 2, por cave: pausa, confere que **nenhuma thread tem `pc` dentro dela** (thread rodando ou de posição ilegível conta como "talvez dentro"), devolve `CC` e retoma; com thread dentro, espera 100 ms e tenta de novo, até 5 vezes. Se não der, a cave fica no registro como pendente e no diário (inalcançável, sem desvio) para a próxima execução. A fase 2 só roda se nenhum desvio ficou sem resolver. Por último, as variáveis são zeradas, **só se nenhuma entrada nossa restou**. Por isso **toda cave de cheat é segura com as variáveis zeradas** (a de observação só conta e grava no anel): valor 0 = passa direto para a instrução original.
- **Recuperação** (depois de SIGKILL, e `gt-patch restore`): a mesma ordem, com a regra da Fase 2 por byte no desvio e, na cave, a regra "cada byte é `CC` ou o nosso" (`CC` é o original, então restaurar uma mistura é inofensivo sob a conferência do `pc`).

**Cave de observação (pesquisa).** `gt-patch observe <padrão> <offset> <registrador>` instala uma cave que só **conta** passagens e guarda num anel de 8 entradas o endereço de retorno (`[rsp]` no ponto) e um registrador; o trainer lê de fora o `ClassPrivate` do valor e mostra os chamadores distintos. Só aceita um **ponto** em que `[rsp]` é o retorno, por uma de três regras provadas pelo disco: começo de função pela `.pdata` (sem unwind encadeado), começo de função **folha** (fora da `.pdata`, logo depois de `CC` ou `ret`), ou **prefixo sem pilha** (dentro da função, com a desmontagem do `start` até o ponto, no máximo 64 bytes, sem instrução que mude o `rsp` e sem nenhum desvio da função com destino entre `start + 1` e o ponto). Qualquer outro ponto é recusado com o motivo. Uma observação por vez, sob a trava do diário, e tudo volta ao sair.

**Testes sem o jogo.** O `gt-testtarget --module` carrega um **módulo sintético** (imagem PE mínima com a forma do jogo: `.text` com funções-alvo, `.pdata`, sequências de `CC` e a cauda da `.data` numa VMA anônima), achado pela descoberta de módulos e pela política de confiança como o jogo. Contra ele rodam o desvio, as caves, o `HookRuntime`, o SIGKILL entre a cave e o desvio, a thread dentro da cave e o `observe`.

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
- **Spec 008:** o calendário (`CalendarComponent`, campo de `SurvivalGameState`, subclasse do `GameState` da 006) é alcançado pela raiz `game_state` com a **classe dona** conferida a cada leitura (`owner_class`, 5.3). `congelar-hora` é um `set` de `GameToRealTimeRatio` (`float`) para 0; `pular-hora` é um `add` de `CurrentTime` (`double`) com releitura "maior ou igual", sem diário e sem desfazer. Os desvios (5.5) dependem da reflexão para os offsets e a classe (`Item`, `ResourceAnalyzerBuilding`, `PartyComponent.ResearchPoints`, `InventoryComponent` do personagem), então ficam `verificando…` até o índice da reflexão, como os cheats de valor. A coleta lê o pawn pelo controller guardado; o `ResearchPoints` aparece no `gt ue player`.

### 5.7 Interface
- `gt-tui`: ratatui num terminal ao lado do jogo. Comandos por teclado **no terminal** (sem hotkeys globais, por causa do Wayland). Detalhes na spec `docs/specs/003-interface-tui.md`.
- **Tela:** lista única agrupada por categoria (`[ON ]`, `[OFF]`, `?` não verificado, `↻` religar pendente, `indisponível (<motivo>)`, `erro`), status de conexão no topo, aviso fixo para diário não resolvido ou jogo possivelmente parado, registro de eventos embaixo (200 linhas guardadas) e teclas no rodapé. Abaixo de 60×15 mostra só `aumente o terminal`.
- **Cheats de valor com valor e ação** (spec 006): os `set` com `[field.value]` mostram o valor como os patches com valor e aceitam `←→`/`hl` (com o cheat desligado, só mudam o valor mostrado). A ação de somar mostra `[ação]` e a quantidade no lugar de `[ON ]`/`[OFF]`, não entra na contagem de cheats ligados do topo e executa no máximo uma vez por segundo (tecla segurada soma uma vez por segundo); sem controller, recusa e pede a procura do controller, e a linha `personagem:` segue o controller guardado depois dela.
- **Desvios com cave** (spec 008): `brain-power` e `analisador-infinito` são linhas `[ON ]`/`[OFF]` como os patches (o Brain Power com valor `5x ◀ ▶`, trocado por uma escrita de variável, sem pausa). `coleta-multiplicada` também é uma linha `[ON ]`/`[OFF]` com valor (`2x`, `3x`, `5x`, `10x`), e `pular-hora` é uma linha `[ação]`. A quantidade ao arrastar saiu da fase (2026-10-28, sem ponto desviável no jogo; `RESEARCH.md`). Os desvios ligados são religados como os patches (mesma chave de versão, esperando a reflexão), seguem a suspensão com motor `stuck`/sem motor e saem pelo motor. A ~10 Hz a TUI lê as variáveis dos desvios e registra `Brain Power: +40 → +200 (próximo nível em 1500)` (com `(+k ganhos não mostrados)`). O `pular-hora` mostra `+1h` e, se o jogo sobrescrever o relógio, `Pular 1 hora: não pegou, aperte de novo`. A busca da conexão carrega a imagem do disco do jogo (~187 MB, ~1 s) para achar as caves, e a mantém na sessão.
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
- Toolchain: `rustup` stable (Fase 0).

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
| 2026-10-27 | Fase 7 (DLL interna) removida | Desnecessária: o trainer fica só externo (decisão do dono) |
| 2026-10-27 | Fase 6 = Brain Power rápido (×2 a ×10, sem pular nível), hora do dia (congelar e pular 1 hora), analisador infinito, quantidade ao arrastar (uso único por tecla) e coleta multiplicada (mundo, loot e criação; baú e item solto pelo dono não); god mode por desvio fora | Dono, spec 008 (o arrastar saiu em 2026-10-28; o item solto pelo dono passou a multiplicar, abaixo) |
| 2026-10-27 | Desvio e cave são patches do motor da Fase 2 (`PatchKind::Code` e `PatchKind::Cave`), com diário, pausa e conferência do `pc`; desfazer em duas fases (todos os desvios, depois as caves) | IA, spec 008 6.3 e 6.4: reaproveita todas as garantias da Fase 2 em vez de um motor paralelo |
| 2026-10-27 | Cave escrita também sob pausa, com o `pc` conferido (a spec 008 6.3 a escreveria sem pausa) | IA: mais estrito que a spec; o motor não tem um caminho de escrita sem pausa para código, e vai no mesmo lote e na mesma pausa do desvio; o custo é a pausa ler a posição das threads e conferir o `pc` contra a cave nova |
| 2026-10-27 | Cave só é apagada com nenhuma thread dentro (`pc`, até 5 tentativas de 100 ms) e só depois de todos os desvios; ao desligar, cave e variáveis ficam até sair | IA, spec 008 6.3: nunca deixar uma thread voltar para uma cave apagada |
| 2026-10-27 | Recuperação da cave por byte (`CC` ou nosso); `Code` com bytes de outro sai do diário e não segura a cave | IA, spec 008 6.3: `CC` é o original, então restaurar uma mistura é inofensivo sob a conferência do `pc` |
| 2026-10-27 | Variáveis das caves na folga da `.data` (região `rw-` privada, dentro do módulo, zero no primeiro uso), sem diário de valor; exceção à regra "a `.data` dos módulos nunca é escrita" da spec 005; zeradas só quando nenhuma entrada nossa (`code` ou `cave`) está ativa ou pendente; a recuperação confere identidade do processo e a região `rw-` privada, sem a conferência `base..SizeOfImage` | IA, spec 008 6.4: uma cave retida ainda pode escrever nelas; toda cave é segura com as variáveis zeradas |
| 2026-10-27 | `check_cave_code`: a cave não tem `call`, `ret`, `int3`, desvio indireto nem salto para fora dela (exceto o `jmp` de volta); "até 16 níveis" de classe = a do objeto + 16 subidas; o MXCSR não é salvo pelo analisador | IA, spec 008 6.2 e 6.7: o tempo dentro da cave fica limitado e nenhum retorno aponta para ela; o `cvtsi2ss` só liga o bit de precisão, que é benigno |
| 2026-10-27 | `write_var` com `BytesMismatch` por corrida com a cave é esperado: o trainer relê | IA, spec 008 6.7.2 (a regra nasceu do arme do arrastar, que saiu em 2026-10-28) |
| 2026-10-27 | Pontos dos desvios por padrão AOB único com os offsets da própria instrução ou da reflexão; nomes de classe no TOML (`object_class`), corpos das caves em Rust | IA, spec 008 12: nenhum offset de campo fixo no código |
| 2026-10-27 | `gt-patch observe` só aceita ponto em que `[rsp]` é o retorno: começo de função pela `.pdata` (sem unwind encadeado), começo de função folha ou prefixo sem pilha provado pela desmontagem | IA, spec 008 6.7.3: a regra só da `.pdata` excluía as funções folha (getter do analisador, entrada do `AddItem`), achado no jogo em 2026-10-27 |
| 2026-10-27 | Hora por valor, sem desvio: `congelar-hora` é um `set` de `GameToRealTimeRatio` para 0 e `pular-hora` um `add` de `CurrentTime` (`double`) com releitura "maior ou igual" (`at_least`), grade de um ponto (`points = [3600.0]`, `label = "+1h"`) e classe dona do salto (`owner_class`) | IA (spike no jogo), spec 008 6.8: o relógio anda a cada quadro, então a releitura nunca é exata |
| 2026-10-27 | Grade por lista de pontos (`points`) em `[field.value]` e `[hook.value]`, com `f64` só no `add` de `double` | IA, spec 008 6.9: ×2/×3/×5/×10 e 10/50/100/250/500/999 não cabem numa grade `min`/`max`/`step` |
| 2026-10-27 | Quantidade ao arrastar como ação com desvio (arma uma vez, expira em 60 s) | IA, spec 008 6.7.2; **substituída em 2026-10-28**: o cheat saiu da Fase 6 (ver abaixo) |
| 2026-10-27 | Coleta multiplicada como tentativa: o corpo da cave só entra depois da pesquisa no jogo (chamadores do `AddItem` por ação); sem separar as três fontes do baú, ela sai da spec e volta ao dono | IA, spec 008 6.7.4 |
| 2026-10-28 | Quantidade ao arrastar fora da Fase 6; código removido (6213ad0) | Dono, spec 008 6.7.2: mover para slot vazio e trocar passam pelo RPC `ServerSwapItemSlots` e não têm instrução de 5+ bytes num ponto aceito pelo `observe`; o `+5bbfe70` do FLiNG não roda no arraste |
| 2026-10-28 | Coleta multiplicada pela implementação nativa do `ServerAddItem` (`+5615f30`), sem limite de pilha; vale também para o item solto pelo dono | Dono, spec 008 6.7.4: o jogo usa o mesmo caminho para chão, colheita, loot e bancada (baú fora); entre os 36 campos do `Item` não há pilha máxima (`RESEARCH.md`, 2026-10-28); a pilha máxima não está entre eles e não se sabe onde fica; a cave não a lê |
| 2026-10-28 | `gt-patch observe` confirmado no jogo real (806 passagens em 15 s na função do relógio, tudo devolvido) | IA, `RESEARCH.md` 2026-10-28: o "0 passagens" do arrastar em `+5bbfe70` vale |
