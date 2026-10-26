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
│   │       ├── value.rs          # ValueSpec (i32/u32/f32), grade por k e bytes de um valor
│   │       ├── engine.rs         # motor: aplica/desfaz lotes e troca valor, com diário e pausa (feature write)
│   │       ├── recovery.rs       # recuperação do diário no início de cada comando (feature write)
│   │       └── session.rs        # sinais de encerramento, laço do run e toggle (feature write)
│   ├── gt-ue/                    # estruturas do Unreal Engine 5 (Fase 5)
│   │   └── src/                  # FName/GNames, GObjects, GWorld, leitura de UObjects
│   ├── gt-grounded2/             # conhecimento específico do jogo
│   │   ├── src/cheats/           # 1 módulo por cheat (sobrevivência, itens, progressão, movimento)
│   │   └── signatures/           # *.toml por build do jogo
│   └── gt-testkit/               # apoio a testes (publish = false): TestTarget, bin_path e binários de apoio
│       ├── src/bin/gt-testtarget.rs      # processo-alvo de teste (não é o gt)
│       ├── src/bin/gt-patch-harness.rs   # mesma biblioteca do gt-patch com patch cru (testes)
│       ├── tests/core_live.rs    # testes de API viva do gt-core contra o alvo
│       ├── tests/engine_live.rs  # testes do motor, da pausa e do toggle contra o alvo
│       └── tests/harness.rs      # sinais, pânico, SIGKILL e recuperação pelo harness
├── apps/
│   ├── gt-cli/                   # ferramentas de pesquisa: inspect, read, scan, disasm, diff, dump, fling
│   ├── gt-patch/                 # aplica/desfaz patches (list, run, toggle, restore, profile); com `write`
│   └── gt-tui/                   # interface do jogador: biblioteca + binário; com `write`
│       ├── src/model.rs, keys.rs, controller.rs   # núcleo da TUI: estado, teclas, conexão, desejados, fila de trocas de valor
│       ├── src/backend.rs        # interface de alvo (Backend)
│       ├── src/game.rs           # implementação do jogo (gt-grounded2)
│       ├── src/raw.rs            # implementação crua contra o gt-testtarget (feature test-backend)
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
  - `generic.toml` (obrigatório): um bloco `[[patch]]` por patch, válido para qualquer versão: `id`, `name` (nome curto em pt-BR para a lista da TUI, obrigatório), `category` (`sobrevivencia`, `itens`, `progressao`, `movimento` ou `diagnostico`; obrigatória; a TUI mostra as categorias nessa ordem e os cheats na ordem do arquivo), `value` (opcional, faz do patch um cheat com valor: `type` `i32`/`u32`/`f32`, `slot`, `min`, `max`, `step`, `default`, `unit`, `decimals`; o `slot` fica inteiro dentro do imediato de uma instrução de `replacement`, fora de desvios relativos; no `<chave>.toml` o bloco é sobrescrito inteiro), `description` (pt-BR), `module` (opcional; só o exe do jogo nesta fase), `pattern` (AOB com os bytes originais, mesma sintaxe do `gt scan`), `offset` (do início da ocorrência ao início do patch), `expected` e `replacement` (bytes em hex separados por espaço, mesmo tamanho).
  - `<chave>.toml` (ex.: `c935e799-0b27c000.toml`): `version` (igual ao nome do arquivo), `product_version` (opcional, informativo) e, por patch verificado naquela build, `[[patch]]` com `id` (já definido no genérico) e `rva` (início do patch relativo à base do módulo, aceita hexadecimal `0x...`); opcionalmente sobrescreve `pattern`, `offset`, `expected` e `replacement` quando a build mudou o código.
  - Regra de carga (erro claro com arquivo e campo, também depois das sobrescritas): `id` único; `module` igual ao exe do jogo; hex válido; `expected` e `replacement` do mesmo tamanho; o trecho `[offset, offset + tamanho)` cabe no `pattern`; cada byte fixo do `pattern` nesse trecho é igual ao de `expected` (curingas no trecho são permitidos).
- Versão com arquivo e `rva` conferido: patch "disponível (verificado)". Versão sem arquivo (ou sem entrada para o `id`) usa a definição genérica e, se o padrão for único na memória e no disco no mesmo lugar, fica "disponível (não verificado)".
- Cada patch informa o próprio estado (tabela da spec 002, 6.4): disponível (verificado ou não verificado), ativo, pendente, bytes diferentes, fora do lugar, não encontrado, ambíguo, inconclusivo ou disco ilegível.

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

### 5.5 Code caves (Fase 6)
Sem `VirtualAllocEx` de fora do processo, existem duas estratégias:
- **Padrão: espaço livre do próprio executável**, ou seja, sequências de `CC`/`00` (preenchimento entre funções) na `.text`. Ficam perto do código (`jmp rel32`, ±2 GB) e não exigem alocação. O alocador de caves rastreia o que já foi usado.
- **Alternativa, se faltar espaço:** ptrace numa thread do jogo para executar um `mmap` dentro do processo. Funciona por ser um processo Linux, mas é mais arriscado; só se necessário e com spec própria.

As instruções originais copiadas para a cave passam por relocação (endereços relativos ao RIP são recalculados com `iced-x86`).

### 5.6 Valores via Unreal Engine (Fase 5)
- Offsets das classes vêm de um dump do **Dumper-7** (rodado no prefixo do jogo, uma vez por atualização), convertidos para um `offsets/<chave>.toml`.
- `GNames`/`GObjects`/`GWorld` são resolvidos por assinatura AOB.
- "Congelar" valores = um laço de ~10 Hz que reescreve o campo. Ele roda numa thread do trainer, nunca dentro do jogo.

### 5.7 Interface
- `gt-tui`: ratatui num terminal ao lado do jogo. Comandos por teclado **no terminal** (sem hotkeys globais, por causa do Wayland). Detalhes na spec `docs/specs/003-interface-tui.md`.
- **Tela:** lista única agrupada por categoria (`[ON ]`, `[OFF]`, `?` não verificado, `↻` religar pendente, `indisponível (<motivo>)`, `erro`), status de conexão no topo, aviso fixo para diário não resolvido ou jogo possivelmente parado, registro de eventos embaixo (200 linhas guardadas) e teclas no rodapé. Abaixo de 60×15 mostra só `aumente o terminal`.
- **Teclas:** `↑↓`/`jk` movem; `espaço`/`Enter` liga e desliga; `←→`/`hl` trocam o valor (na hora, com intervalo mínimo de 150 ms entre trocas); `r` tenta SIGCONT, restos, diário ou nova busca, nessa ordem; `q`/`Esc`/Ctrl+C saem, desligando tudo sem confirmação.
- **Núcleo testável:** o controlador fala com o jogo por uma interface de alvo (`Backend`), com a implementação do jogo, uma crua (contra o `gt-testtarget`, atrás da feature `test-backend`) e uma falsa nos testes unitários; o terminal também é uma interface, com versão falsa para testar a restauração.
- **Reconexão:** procura o jogo a cada 2 s; depois de cada conexão, novas buscas a cada `max(5 s, 2 × duração da última)` por até 120 s; cheats ligados antes da queda são religados sozinhos, só na mesma chave de versão (versão nova exige espaço). Com `--pid`, o fim do processo é um estado final.
- **Log em arquivo:** `$XDG_STATE_HOME/grounded-trainer/gt-tui.log` (uma geração anterior em `.log.1`, `RUST_LOG` com padrão `info`); nada vai ao `stdout`/`stderr` com a TUI desenhada. Erros de início (trava ocupada, sem terminal) saem no `stderr` com código 2; o resumo de saída vai ao `stdout`.
- **Leitura de teclas em thread própria:** com o terminal fechado (hangup), a leitura do `crossterm` repete o `read` com `EIO` sem parar e nunca devolveria o controle ao laço. A thread `gt-keys` lê e manda as teclas por um canal; o laço espera no canal e confere o hangup com `poll(2)` no `stdin`. Sai com código 0 em `q`, SIGTERM, SIGQUIT, SIGHUP e `EIO`.
- **Saída:** desenha `restaurando…`, desfaz tudo, restaura o terminal uma vez só (marca compartilhada com o gancho de pânico) e imprime o resumo lendo o diário depois do `drop` do motor. Pânico: código 101.
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
| 2026-10-25 | `name` e `category` obrigatórios nas assinaturas; bloco `value` sobrescrito inteiro por versão | Lista agrupada e sem campos pela metade (spec 003, 6.3) |
| 2026-10-25 | Núcleo da TUI testável por uma interface de alvo (jogo, crua e falsa) e por uma interface de terminal com versão falsa | Testar conexão, reconexão, sinais e restauração sem o jogo |
| 2026-10-25 | `stuck` mantido até um `SIGCONT` bem-sucedido, com nova tentativa antes de cada operação; restos de falha viram `erro` e só a recuperação (`r`) resolve | Nunca deixar o jogo parado sem aviso (spec 003, 6.8) |
| 2026-10-25 | Recuperação do diário no início e a cada conexão; diário não resolvido bloqueia as escritas sem fechar a TUI; `r` tenta de novo | Um motor novo regravaria o diário e apagaria entradas não resolvidas (spec 003, 6.1) |
| 2026-10-25 | Operações do motor no laço principal, com a mensagem desenhada antes; busca dos padrões em thread separada; teclas de ação durante uma operação são descartadas (menos `q` e as setas de valor) | Resposta da tela em até ~250 ms sem liga-desliga acumulado com o jogo pausado (spec 003, 6.10) |
| 2026-10-25 | Terminal restaurado uma vez só, por marca compartilhada; pânico fora da thread principal não mexe no terminal; código de saída decidido relendo o diário depois do `drop` do motor | Sem terminal quebrado e sem código de saída enganoso (spec 003, 6.8) |
| 2026-10-25 | Log só em arquivo enquanto a TUI está aberta, iniciado antes da recuperação, com uma geração anterior; trava antes da conferência do terminal | Avisos da recuperação ficam no log; um segundo `gt-tui` recusa sem tocar o terminal (spec 003, 6.1 e 6.9) |
| 2026-10-26 | Teclas lidas numa thread própria (`gt-keys`) e hangup conferido com `poll(2)` no `stdin` | A leitura do `crossterm` repete `EIO` sem parar num terminal fechado e nunca devolveria o controle ao laço |
| 2026-10-26 | Teste do binário em pseudo-terminal (`openpty` do `nix`, sem `unsafe`), com `--pid` de um processo já terminado nos testes de sinal e restauração | Roda mesmo com o jogo aberto; só o teste de `aguardando o jogo` pula quando ele está aberto |
| 2026-10-25 | Código de terceiros pode ser desmontado e decompilado para entender o que faz; copiar ou redistribuir continua proibido | Entender as caves do FLiNG acelera a pesquisa; a implementação segue nossa, sem código deles (decisão do dono, `AGENTS.md` 3) |
