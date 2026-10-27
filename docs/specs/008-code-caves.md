# Spec 008 — Code caves: Brain Power, hora do dia, analisador, arrastar e coleta

| Campo | Valor |
|---|---|
| Fase | 6 (ver `docs/ROADMAP.md`) |
| Estado | Aprovada pelo dono em 2026-10-27 (Fase 6 em andamento) |
| Data | 2026-10-27 |
| Depende de | Spec 007 na `main` (raiz `game_state` com `set`), só para `congelar-hora` (6.8.1) |

## 1. Intenção

O dono quer, na Fase 6 (conversa de 2026-10-27):

1. **Brain Power rápido**: cada ganho de Brain Power vale ×N (×2 a ×10), **sem pular nível**. É o critério de
   pronto da fase.
2. **Hora do dia**: **congelar** o relógio e uma ação que **pula 1 hora** por tecla.
3. **Cargas infinitas do analisador** de recursos.
4. **Quantidade ao arrastar**: o dono escolhe N e "arma"; o **próximo** item arrastado fica com N unidades (uso
   único por tecla).
5. **Coleta multiplicada** (não existe no FLiNG): ao **coletar no mundo**, ao pegar **loot de criaturas** e no
   **resultado de criação**, o item vem ×N (×2 a ×10). **Tirar do baú e pegar de volta um item que o dono soltou não
   multiplicam.**

O que é novo no motor é o **desvio com code cave**: código nosso dentro do jogo, que muda um cálculo em vez de trocar
um valor. Brain Power, analisador, arrastar e coleta precisam disso. Os dois de hora não precisam: a pesquisa achou
o relógio no `CalendarComponent` do `GameState`, e congelar ou pular uma hora são escritas de valor.

O FLiNG serviu só para saber **onde** o jogo faz algumas dessas coisas (`AGENTS.md` 3). Os pontos de desvio, as
caves e o código são nossos.

## 2. Objetivos

1. **Motor de desvios** (6.2 a 6.5): achar espaço livre no executável, montar o código da cave com `iced-x86`,
   instalar e remover o desvio com as garantias do motor da Fase 2 (bytes esperados, pausa com conferência do `pc`
   das threads, diário, restauração em toda saída, recuperação depois de SIGKILL).
2. **Dois tipos novos de escrita** no motor (6.4): **cave** (bytes na `.text`, em preenchimento `CC`) e **variável**
   (folga gravável no fim da `.data`), com política e entradas de diário próprias.
3. **Quatro cheats por desvio** (6.6 e 6.7): `brain-power`, `analisador-infinito`, `quantidade-arrastar` e
   `coleta-multiplicada` (este como **tentativa**, 6.7.4).
4. **Dois cheats de hora por valor** (6.8): `congelar-hora` (`set`) e `pular-hora` (`add` generalizado para
   `double`).
5. **Grade por lista de pontos** (6.9).
6. `gt caves` e `gt diff` cientes dos desvios; `gt ue player` mostra `ResearchPoints`; `gt-patch observe`, a cave de
   observação para pesquisa (6.7.3 e 6.10).
7. Testes sem o jogo contra o alvo de teste e testes `#[ignore]` com o jogo aberto (8).

## 3. Fora do objetivo

- **God mode por desvio**: o "não morrer" da spec 005 funciona; fica para quando o dono pedir.
- **Multiplicador de defesa**: saiu na spec 007 (`EnemyDamageMultiplier`).
- **Velocidade do tempo ajustável**: o dono escolheu congelar e pular hora. `TimeOfDaySpeedMultiplier` e
  `TimeMultiplier` ficam para quando ele pedir.
- **Multiplicar ao tirar do baú** ou em qualquer transferência entre inventários (decisão do dono).
- Alocar memória dentro do jogo (`mmap` por ptrace, `ARCHITECTURE.md` 5.5): só se faltar espaço, com spec própria.
- Desvios fora do executável do jogo. Multijogador.

## 4. Anti-objetivos (falha mesmo que "funcione")

Valem os das specs anteriores. Além deles:

- **Crash por desvio**: uma thread do jogo executar meio desvio, voltar para uma cave apagada ou executar `CC`.
  Ligar, desligar, trocar valor, sair, SIGKILL e recuperação não podem deixar o jogo nesse estado.
- Escrever cave em bytes que não são preenchimento `CC` conferido na hora, ou variável fora da folga conferida (6.4).
- Deixar desvio nosso no jogo depois de desligar, ou desvio, cave ou variável nossa depois de sair do trainer,
  inclusive depois de SIGKILL seguido de nova execução (ressalva: 6.3, cave que não pôde ser limpa com segurança).
- Offset de campo ou de estrutura fixo no código: vem da instrução do jogo (curinga do padrão), da reflexão, do
  `[ue.layout]` das assinaturas (`ClassPrivate`, `SuperStruct`), ou é um
  byte **fixo do padrão** (se mudar, o padrão não acha e o cheat fica indisponível, nunca usa um valor velho).
- Cave que assume um registrador que o padrão não confere.
- **Falha de página dentro da cave**: a cave fica fora da `.pdata` (sem unwind), e um acesso inválido ali derruba o
  jogo. A cave só desreferencia ponteiros que a pesquisa mostrou serem sempre válidos naquele ponto (o próprio jogo
  os desreferencia ali, ou são objetos conferidos pela pesquisa), e nunca segue um ponteiro lido de uma variável sem
  conferir que não é nulo.
- Cave com `call`: nenhuma cave chama função do jogo (o tempo dentro dela fica limitado e nenhum endereço de retorno
  aponta para ela).
- **Brain Power pular um nível** (decisão do dono, 6.6.1).
- Quantidade ao arrastar mudar mais de um item por arme, ou mudar item sem o dono ter armado.
- Coleta multiplicada multiplicar o que sai de baú, outra transferência ou um item que o dono soltou e pegou de volta.
- Pular hora "devolver" tempo.

## 5. Onde cada coisa mora

| O quê | Onde |
|---|---|
| Achar folgas, alocador de caves, montagem com `iced-x86`, desvio `jmp rel32` | `crates/gt-core/src/` (módulo novo de caves; `asm.rs` ganha a montagem) |
| Escritas de cave e de variável, política da folga, entradas novas do diário, limpeza com conferência do `pc` | `crates/gt-core/src/` (`engine.rs`, `journal.rs`, `recovery.rs`, `patch.rs`, `mem/`) e `apps/gt-patch` (`restore`) |
| Grade por lista, `f64` na grade | `crates/gt-core/src/value.rs` |
| Pontos dos desvios (`[[hook]]` ou equivalente) e corpos das caves (Rust) | `crates/gt-grounded2/signatures/generic.toml` e `crates/gt-grounded2/src/` |
| Calendário, `add` de `double` com `at_least`, salto com classe dona própria | `crates/gt-grounded2/src/` (`player.rs`, `fields.rs`, `signatures.rs`) e, no motor, uma escrita de dados com releitura **"maior ou igual"** (`crates/gt-core/src/engine.rs`; hoje a releitura exige igualdade exata). Os tipos da ação (`plan_add`, `ActionOutcome`, `Backend::field_action`, backend falso, formatação no `controller.rs`) passam de `i32` para um valor numérico de 4 ou 8 bytes, de ponta a ponta |
| Linhas na TUI (desvio com valor, ação de armar, `[ação]` de pular hora, registro do Brain Power) | `apps/gt-tui/src/` |
| `gt caves`, `gt diff`, `gt ue player` (só leitura; `gt diff` e `gt caves` passam a ler o diário sem a trava, conferindo a identidade boot/PID/início antes de usar) | `apps/gt-cli/` e `crates/gt-grounded2/src/player.rs` |
| `gt-patch observe` (escreve; usa trava do diário e recuperação) | `apps/gt-patch/` |
| **Módulo sintético** no alvo de teste: imagem com a mesma forma do jogo (cabeça mapeada de arquivo e cauda da `.data` em VMA **anônima** `rw-p`), cabeçalho PE mínimo, `.text` com funções e `CC`, `.data` com `VirtualSize` menor que a página, achada pela descoberta de módulos e pela política de confiança como o jogo, e o arquivo servindo de "disco" para o `gt diff`. É infraestrutura grande (hoje o alvo é só páginas anônimas) e vira tarefa própria do plano | `crates/gt-testkit/` (`gt-testtarget`) |
| Achados da pesquisa e do teste no jogo | `docs/RESEARCH.md` |

## 6. Comportamento

### 6.1 Pesquisa (feita em 2026-10-27, build `c935e799-0b27c000`)

Detalhes em `RESEARCH.md` ("Sessão de 2026-10-27 (Fase 6)"). Resumo:

| Cheat | Ponto | Achado |
|---|---|---|
| Brain Power | `+5970ee7` `mov r14,[rax+0x118]` (7 bytes), na função `+5970e80` (`rbx` = `PartyComponent`); padrão estendido de `+5970ec3` a `+5970f1e` (6.6.1), único | Antes do ponto: `esi` = `ResearchPoints` antigo, `[rbx+0x514]` = novo (já somado, `+5970ed7`), `rax` = objeto da tabela de níveis (de `call +59e1390`). Tabela: início `[rax+0x118]`, contagem `[rax+0x120]`, `0x30` bytes por nível (`lea rcx,[rax+rax*2]; shl rcx,4`), limite em `+0x10` (`mov eax,[r14+0x10]`). O laço sobe **no máximo um nível** por chamada. Único chamador: `+5971304` (ganho buscado numa tabela por fonte) |
| Analisador | `+57bd6d0` `movss xmm1,[rcx+0x974]` (8 bytes), primeira instrução do getter das cargas (`rcx` = `ResourceAnalyzerBuilding`) | Cargas atuais (`float`, dentro do `PowerReservoir`); `MaxPowerCapacity` (`IntProperty`) pela reflexão. Padrão único |
| Arrastar | `+5bbfe70` `mov [rsp+8],rbx` (5 bytes), começo da função | `rcx` = item (testado em `+5bbfe8f`). `Item.StackSize` pela reflexão. Qual item é o `rcx` **não confirmado**. Padrão único (com o começo da função) |
| Coleta | `AddItem` nativo `+5941660` (`rcx` = `InventoryComponent`, `rdx` = item), pela vtable da interface em `InventoryComponent+0xe0`, entrada `0x50` | Genérico: provavelmente também usado por transferências. Separar as fontes exige observar os chamadores no jogo (6.7.4) |
| Hora | `GameState` → `SurvivalGameState.CalendarComponent` → `/Script/Maine.CalendarComponent` | `CurrentTime` (`double`, segundos de jogo), `GameToRealTimeRatio` (`float`, 30). `SurvivalGameState` é subclasse de `InGameGameState`. Spike: ratio 0 congela; `+3600` avança 1 hora |
| Espaço | `.text` em disco | Só **4** sequências de `CC` com 64+ bytes: 66, 248, **1871** e 102 bytes. Folga da `.data`: `0x134` bytes |

### 6.2 Desvio

Um desvio troca **uma instrução inteira** do jogo, de pelo menos 5 bytes, por `jmp rel32` para a cave, mais `NOP` até
o fim da instrução. A cave executa o nosso código, a instrução original (relocada pelo `iced-x86` quando é relativa
ao RIP) e `jmp` de volta para a instrução seguinte.

- **Uma instrução só**, mesmo com a conferência do `pc` da pausa (`patch.rs`): simplifica a relocação e o
  `expected`. Os quatro pontos da 6.1 têm uma instrução de 5+ bytes (o da coleta, a escolher na pesquisa, também
  precisa ter).
- O ponto é achado por padrão AOB **único** (`AGENTS.md` 4); os bytes da instrução são conferidos (`expected`); os
  offsets que a cave usa saem de curingas do padrão ou da reflexão. Bytes diferentes: `indisponível (bytes diferentes)`.
- Instrução original relativa ao `rsp` (o `mov [rsp+8],rbx` do arrastar): a cave a executa com o `rsp` igual ao da
  entrada (desempilha antes, ou a executa antes de empilhar).
- A cave **preserva** todos os registradores e flags que o código do jogo usa depois do ponto (quais são, por ponto,
  fica no plano, a partir da desmontagem). Não chama nada.

### 6.3 Ordem das operações

- **Ligar**: escreve e confere as variáveis e a cave (sem pausa: nada aponta para elas ainda); depois, com o jogo
  pausado e o `pc` das threads conferido como nos patches, escreve o desvio e confere.
- **Desligar**: com o jogo pausado e o `pc` conferido, devolve os bytes originais da instrução. A cave e as variáveis
  **ficam** até sair (uma thread dentro da cave termina e volta ao código do jogo). Religar reaproveita a cave.
- **Sair** (normal, Ctrl+C, SIGTERM, pânico, jogo fechando): devolve todos os desvios; depois, para cada cave:
  pausa, confere que **nenhuma thread tem `pc` dentro da cave** (thread rodando ou com posição ilegível conta como
  "talvez dentro"), devolve `CC` e confere, retoma. Se houver thread
  dentro, retoma, espera 100 ms e tenta de novo, até 5 vezes; se não der, deixa a cave (inalcançável, sem desvio),
  registra e mantém a entrada no diário para a próxima execução. Por último, zera as variáveis. Por isso **toda cave
  é segura com as variáveis zeradas**: valor 0 = não faz nada (passa direto para a instrução original).
- **Recuperação** (próxima execução depois de SIGKILL, e `gt-patch restore`): mesma ordem. Desvio: regra da Fase 2
  por byte (original ou nosso; byte de outro não é escrito). Cave: só devolve `CC` se **cada byte** for `CC` ou o
  nosso (`CC` é o original) e não houver desvio nosso instalado apontando para ela, **e com a mesma pausa, conferência do `pc` e até 5
  tentativas da saída** (depois de um SIGKILL o desvio estava vivo, então pode haver thread dentro). Variáveis: 6.4.

### 6.4 Escritas de cave e de variável (tipos novos do motor)

As regras de escrita de hoje não servem (os patches são de 1 a 64 bytes de instruções inteiras; as escritas em dados
excluem as imagens de módulo e exigem guarda de objeto). Entram dois tipos novos, com política própria. É uma
**exceção explícita** à regra da spec 005 de que a `.data` dos módulos nunca é escrita; ela vai para o `AGENTS.md` 3.

- **Cave**: bytes na `.text` do executável, dentro de uma sequência de `CC`, de qualquer tamanho até o da sequência.
  Antes de escrever: todos os bytes do trecho são `CC`, e o trecho fica dentro da sequência com pelo menos 16 bytes
  `CC` intactos no começo dela (o fim de função antes). O diário guarda endereço e bytes escritos **antes** da
  escrita. Escrita por `/proc/<pid>/mem` (página de código).
- **Alocador**: várias caves podem dividir a mesma sequência; duas caves nunca se sobrepõem. Uma sequência serve se
  `tamanho ≥ cave + 16`. Tamanhos estimados: Brain Power ~150 bytes, analisador ~50, arrastar ~60, coleta ~150 (o
  plano mede). A sequência de 1871 bytes cabe tudo.
- **Variável**: na folga entre o fim do `VirtualSize` da `.data` (arredondado para cima a 8) e o fim da última página
  dela, conferida no `maps`: `rw-` e dentro do intervalo do módulo (`base` até `base + SizeOfImage`). No jogo essa
  cauda é uma VMA **anônima** (`14a41d000-14a840000 rw-p`), separada da parte mapeada do arquivo; as duas formas são
  aceitas e lida como zeros no primeiro uso da sessão. Sem guarda de objeto. O diário guarda só a
  **região** usada, não os valores. A variável muda sem diário (o trainer troca valor; a cave do arrastar zera a
  marca).
- **Orçamento** (folga de `0x130` = 304 bytes depois de alinhar a 8), tabela fixa, alinhada a 8:

  | Variáveis | Bytes |
  |---|---|
  | Brain Power: N, último ganho original, aplicado, `L1`, `L2`, contador de sequência | 24 |
  | Arrastar: N armado, contador de aplicadas | 16 |
  | Analisador: ligado (0/1), ponteiro da classe `ResourceAnalyzerBuilding` | 16 |
  | Coleta: N, ponteiro do inventário do jogador, até 8 endereços de retorno permitidos | 80 |
  | Arrastar e coleta: ponteiro da classe `Item` (6.7.2) | 8 |
  | Cave de observação: contador e anel de 8 entradas (retorno + 1 registrador), **uma de cada vez** | 136 |
  | Total | 280 |

  Uma folga menor que 280 bytes deixa indisponível o que não couber, **por cheat, com as dependências** (a
  observação sai primeiro; o ponteiro da classe `Item` só sai junto com o arrastar e a coleta). Na folga real (304)
  cabe tudo.
- **Leitura pelo trainer** de variáveis com mais de uma palavra (registro do Brain Power): contador de sequência lido
  antes e depois; só aceita se forem iguais **e pares** (a cave incrementa o contador antes e depois de gravar, então
  ímpar é gravação em andamento); senão, lê de novo.
- **Limpeza das variáveis** (saída e recuperação): zera a região inteira, sem conferir os valores (ninguém mais usa a
  folga), desde que ela ainda seja a folga da imagem (mesmo processo, `maps` igual, depois do `VirtualSize`) e que
  nenhum desvio nosso esteja instalado.
- Sem folga (0 bytes, página não `rw-`, bytes não zero no primeiro uso): os desvios ficam
  `indisponível (sem espaço para variáveis)`; sem sequência de `CC` que caiba: `indisponível (sem espaço para a cave)`.

### 6.5 Valor de um desvio

O valor de um desvio fica numa variável. Trocar o valor é **uma escrita na variável**, sem pausa e sem mexer no
código. Desligar não muda a variável; sair a zera.

### 6.6 PROGRESSÃO — `brain-power` ("Brain Power rápido")

#### 6.6.1 Comportamento

- **Padrão estendido** (único, começa em `+5970ec3`; desvio em `+0x24` dele):
  `8b b1 ?? ?? 00 00 4d 89 73 ?? 89 75 ?? 8d 04 16 48 8b 51 ?? 89 81 ?? ?? 00 00 e8 ?? ?? ?? ?? e8 ?? ?? ?? ?? 4c 8b b0 ?? ?? 00 00 48 63 80 ?? ?? 00 00 48 8d 0c 40 48 c1 e1 04 49 03 ce 4c 3b f1 0f 84 ?? ?? ?? ?? 0f 1f 80 00 00 00 00 41 8b 46 ?? 3b f0 7d ?? 3b 83 ?? ?? 00 00`.
  Ele confere os registradores que a cave usa: `esi` = antigo (`mov esi,[rcx+X]` e `cmp esi,eax`), `rbx` = o objeto
  (`cmp eax,[rbx+X]`), `rax` = tabela. Os três `disp` de `ResearchPoints` (`mov esi`, `mov [rcx+…],eax`,
  `cmp eax,[rbx+…]`) têm de ser iguais entre si e ao offset de `PartyComponent.ResearchPoints` da reflexão; senão,
  `indisponível (padrão incoerente)`. Início e contagem da tabela e o offset do limite vêm dos curingas; o passo `0x30`
  vem dos bytes fixos (`lea rcx,[rax+rax*2]; shl rcx,4`).
- A cave calcula `ganho = [rbx+R] − esi` (R = offset de `ResearchPoints`). Se `ganho > 0` e N > 0:
  1. `ganho' = ganho × N`, limitado ao maior `int32` e a `int32_max − esi`;
  2. **limite de nível**: percorre a tabela (no máximo 256 entradas; contagem maior ou negativa: não multiplica) e acha, entre os limites **distintos** maiores que `esi`, o menor (`L1`,
     o próximo nível) e o segundo menor (`L2`), sem supor ordem. Se `L2` existir, `ganho' = min(ganho', L2 − 1 − esi)`. Assim o novo total atravessa no
     máximo um limite, e o jogo sobe esse nível com a recompensa;
  3. nunca diminui o ganho original: `ganho' = max(ganho', ganho)`;
  4. grava `[rbx+R] = esi + ganho'`.
  Depois executa a instrução original. Ganho zero ou negativo passa sem mudança.
- Valor: ×2, ×3, ×5, ×10 (padrão ×2), grade por lista (6.9), mostrado como `5x`.
- **Registro**: a cave grava em variáveis o último ganho original, o aplicado, `L1` e `L2` (0 se não houver), com o
  contador de sequência. A TUI, quando o contador muda, registra `Brain Power: +40 → +200 (próximo nível em 1500)`,
  com `L1` em valor absoluto de `ResearchPoints`. O contador sobe 2 por ganho; se a TUI vê um salto maior (vários
  ganhos em menos de 100 ms), registra o último e `(+k ganhos não mostrados)`. Isso é o que
  mostra no jogo que a multiplicação e a leitura da tabela estão certas.

#### 6.6.2 Riscos aceitos

- O jogo já marcou o campo para replicação com o valor sem multiplicar (`call +1fed760` antes do ponto); no solo não
  importa. Não é testado em multijogador.
- O ganho multiplicado é progresso que vai para o save (propósito do cheat).

### 6.7 ITENS

#### 6.7.1 `analisador-infinito` ("Analisador infinito")

- Desvio em `+57bd6d0`, padrão `f3 0f 10 89 ?? ?? 00 00 f3 0f 58 ?? f3 0f 5c ?? ?? ?? ?? ?? f3 0f 2d ?? d1 f8 c3`
  (único). Antes da instrução original, se o `rcx` for da classe `ResourceAnalyzerBuilding` (conferida como o `Item`
  no arrastar, com o ponteiro da classe numa variável), a cave grava `float(MaxPowerCapacity)` nas cargas quando
  `MaxPowerCapacity > 0`.
- **Pesquisa antes do código** (com a cave de observação): os chamadores do getter e o `ClassPrivate` do `rcx` (só
  analisadores?), e os valores de cargas e `MaxPowerCapacity` lado a lado num analisador cheio e num gasto (cheio é
  mesmo `float(MaxPowerCapacity)`?). Se não for, o valor certo vem dessa observação e vai para o `RESEARCH.md`. Offset das cargas: o `disp32` da instrução (curinga do padrão). `MaxPowerCapacity`: reflexão.
- Sem valor, mas com a variável "ligado": 0 = passa direto. Ligar grava 1 antes de instalar o desvio. Desligar
  para de encher; as cargas ficam onde estão.

#### 6.7.2 `quantidade-arrastar` ("Quantidade ao arrastar")

- Desvio em `+5bbfe70`. Uma **variável única**: `0` = desarmado, `N > 0` = armado com N. A cave, com `rcx` não
  nulo e **da classe `Item` ou subclasse** (sobe `ClassPrivate` → `SuperStruct` até 16 níveis, sem `call`, comparando
  com o ponteiro da classe `Item` que o trainer põe numa variável pela reflexão), pega a variável com `xchg` (lê e
  zera numa operação, para duas threads nunca aplicarem duas vezes) e, se era `N > 0`, grava N em `Item.StackSize`.
  Classe errada ou ponteiro da classe zerado: não faz nada. Ler `ClassPrivate` do `rcx` só é seguro se o `rcx` for
  sempre um `UObject` válido ali: a pesquisa (6.7.3) confirma isso registrando o `rcx` de cada chamada, e o trainer
  lê de fora o `ClassPrivate` deles; um `rcx` que não seja objeto tira o cheat da spec (volta ao dono).
- Linha `[ação]` com a quantidade (10, 50, 100, 250, 500, 999; padrão 100). `espaço`/`Enter` **arma**: instala o
  desvio se ainda não estiver instalado e grava N na variável.
- O trainer acompanha a variável a ~10 Hz: quando ela volta a 0 sem ter sido o trainer, registra
  `Quantidade ao arrastar: aplicada (250)`. Depois do `xchg` que achou `N > 0`, a cave incrementa o **contador de
  aplicadas**.
- **Apertar de novo com a variável armada desarma.** O trainer não tem operação atômica na memória do jogo (as
  escritas são cópias simples), então: lê o contador, escreve 0 na variável, espera 50 ms e relê o contador. Se ele
  subiu, o jogo aplicou antes (`aplicada`); senão, `desarmado`. A escrita do 0 nunca causa duas aplicações: só o
  `xchg` da cave aplica. Para trocar N armado: desarmar, mudar, armar. Com o arrastar armado, `←→` mudam só o número mostrado, que
  fica marcado como diferente do armado (`armado: 250`) até desarmar.
- **Expira** em 60 s: armado e não usado, o trainer desarma do mesmo jeito e registra `expirou` (ou `aplicada`).
- **Ciclo de vida** (ação com desvio, tipo novo): não conta como ligada no topo; não é religada depois de reconectar
  (o desvio só volta no próximo arme); com motor `stuck` ou sem motor, armar é recusado como uma ação; o desvio sai
  na saída, no `undo_all`, no `Drop` e na recuperação como os outros. A janela de 1 s das ações (spec 006) vale.
- **Antes de implementar**, o plano confirma no jogo qual item chega em `rcx` e se a função roda só ao arrastar (com
  uma cave de observação, 6.7.4). Se não for o item arrastado, procura outro ponto de uma instrução de 5+ bytes; se
  não achar, o cheat sai da spec e volta para o dono.
- Quantidade acima da pilha máxima do item é aceita (até 999); o teste anota o que o jogo faz.

#### 6.7.3 Cave de observação (ferramenta de pesquisa)

O motor de desvios é usado primeiro para pesquisa: uma cave que só **conta** passagens e guarda, num anel de
variáveis, o endereço de retorno (`[rsp]` no ponto) e um registrador. Ela não muda nada no jogo e não desreferencia
nada além do `[rsp]`; o trainer lê de fora o que precisar (por exemplo o `ClassPrivate` do registrador).
É instalada por `gt-patch observe <padrão> <offset> <registrador>` (no `gt-patch`, que já escreve com diário e
recuperação; o `gt` continua só leitura), que mostra os chamadores distintos e devolve tudo ao sair. O registrador do
anel é escolhido no comando; para a coleta, observar duas vezes (`rcx` e `rdx`). Uma observação por vez. Serve para o
arrastar (6.7.2), o analisador (6.7.1) e a coleta (6.7.4).

**Ponto aceito** (só nele `[rsp]` é o endereço de retorno). O comando recusa qualquer outro, com o motivo:

1. **Começo de função pela `.pdata`**: o ponto é o `start` de uma entrada da `.pdata` do disco.
2. **Começo de função folha**: o ponto não fica dentro de nenhuma entrada da `.pdata` (funções folha, que não mexem na
   pilha, não têm entrada) e o byte logo antes dele é `CC` (preenchimento) ou o fim de um `ret`. Exemplos: o getter do
   analisador (`+57bd6d0`) e a entrada da interface do `AddItem` (`+5942240`).
3. **Prefixo sem pilha**: o ponto fica dentro de uma entrada da `.pdata`, depois do `start`, e a desmontagem linear do
   `start` até o ponto (no máximo 64 bytes) chega exatamente nele numa fronteira de instrução, sem nenhuma instrução
   que mude o `rsp` (`push`, `pop`, `call`, `ret`, `enter`, `leave`, ou destino `rsp`/`esp`), e **nenhum desvio da
   função inteira** (desmontada pela entrada da `.pdata`) tem destino entre `start + 1` e o ponto. Desvios condicionais
   no prefixo são aceitos (eles saem; quem chega ao ponto veio direto do `start`). Exemplo: `+5941669` no `AddItem`
   (`test rdx,rdx; je …; mov [rsp+0x18],r8b`).

Funções com unwind encadeado (`UNW_FLAG_CHAININFO`) não contam como começo de função na regra 1.

#### 6.7.4 `coleta-multiplicada` ("Coleta multiplicada") — tentativa

- **Pesquisa no plano, antes do código do cheat**: com a cave de observação no `AddItem` nativo (`+5941660`) e, se
  preciso, nas funções que criam itens, o dono faz cada ação no jogo (pegar do chão, colher, loot de criatura, criar
  na bancada, **tirar do baú**, soltar e pegar de volta). O resultado (chamadores por ação) vai para o `RESEARCH.md`.
- **Mecanismo** (escolhido pela pesquisa): multiplicar o `StackSize` do item antes de ele entrar no inventário, só
  nos caminhos das três fontes. Pode ser um desvio por fonte (no ponto de cada uma) ou um desvio no `AddItem` que
  confere o endereço de retorno contra uma lista de até 8 chamadores achados por padrão.
- **Só o inventário do jogador**: o trainer põe numa variável o endereço do `InventoryComponent` do personagem (pelo
  caminho do pawn e da reflexão, pelo campo `SurvivalPlayerCharacter.InventoryComponent`; o
  `MountInventoryComponent` e o `ProximityInventoryComponent` do personagem não contam), atualizado no ciclo de ~10 Hz junto com os cheats de valor, e zera a
  variável quando o personagem some (morte, menu, outro mundo). A cave só multiplica se o inventário de destino for
  igual à variável não nula.
- **Quanto**: `StackSize × N`, **limitado à pilha máxima do item**; item de pilha máxima 1 (ferramentas, armas) não
  muda. De onde vem a pilha máxima é parte da pesquisa; sem achar, o cheat sai pela saída abaixo.
- O item é conferido como `Item` como no arrastar.
- Valor: ×2, ×3, ×5, ×10 (padrão ×2), mostrado como `2x`.
- Sem desfazer: o item extra fica no inventário depois de desligar.
- **Saída**: se a pesquisa não conseguir separar as três fontes do baú, ou um caminho exigir desvio de duas
  instruções, o cheat sai da spec (as outras partes seguem) e volta para o dono com o que foi achado.
- **Item solto pelo próprio dono e pego de volta não multiplica** (decisão do dono): só multiplica o que nasce no
  mundo (recursos, loot) ou sai da criação. Se a pesquisa não conseguir separar esse caso de "pegar do chão", vale a
  saída acima.

### 6.8 Os cheats de hora (por valor, sem desvio)

Os dois usam a raiz `game_state` (o `add` nela já existe desde a 006; o `set`, desde a 007). O salto até o calendário é um campo de **outra classe dona**
(`/Script/Maine.SurvivalGameState.CalendarComponent`, subclasse de `InGameGameState`): o `[[field]]` declara a classe
dona do campo e o trainer confere que o `GameState` vivo é dessa classe (ou subclasse) antes de seguir; o objeto
seguinte é conferido como `/Script/Maine.CalendarComponent`. O formato no TOML fica com o plano.

#### 6.8.1 PROGRESSÃO — `congelar-hora` ("Congelar hora do dia")

- `set` de `GameToRealTimeRatio` (`float`) para `0`, sem valor ajustável; ao desligar, volta o original (regra `set`
  da 006, manutenção de ~10 Hz da 007).
- Risco: **dormir** com a hora congelada. Se o jogo mexer no campo ao dormir, a regra `set` aprende o valor como
  original novo e pode chegar a `o jogo não deixa`; se o sono travar, o teste anota e o dono decide. A TUI não muda.

#### 6.8.2 PROGRESSÃO — `pular-hora` ("Pular 1 hora")

Generaliza o `add` da spec 006 (que hoje é só `int`):

- `kind = "double"` aceito no `add`, com `f64` na grade de valores (6.9). `kind = "float"` continua recusado.
- Quantidade fixa: grade com **um ponto só** (`points = [3600.0]`), sem `←→`. A linha mostra `[ação]` e `+1h` (o
  texto da quantidade pode vir de um `label` do TOML).
- **Releitura com limite inferior** (`at_least = true` no `add`; padrão: releitura exata, como a ciência hoje). O
  contrato é: releitura **≥ `v + 3600`** = aplicada (o jogo soma ao relógio a cada quadro, então ela nunca é exata);
  releitura **< `v + 3600`** = o jogo sobrescreveu: `Pular 1 hora: não pegou, aperte de novo`, sem repetir sozinho.
  No motor isso é uma escrita de dados cuja releitura aceita "maior ou igual" em vez de "igual". Uma releitura acima
  de `v + 3600 + 300` também é aplicada, só com `(o relógio andou mais que o esperado)` no registro (o 300 é um número
  do código, não do motor).
- Faixa sã: `result_max = 1000000000` (inteiro no TOML, como hoje): recusa se `v < 0` ou `v + 3600 > result_max`.
- `double` exige leitura e escrita de 8 bytes no `add` e na conferência de valor são (hoje de 4 bytes).
- Funciona com a hora congelada ou não. Sem diário e sem desfazer (a hora vai para o save).

### 6.9 Grade por lista de pontos

`[field.value]` e o valor de um desvio aceitam **`points = [...]`** no lugar de `min`/`max`/`step`: lista
estritamente crescente, com o `default` dentro dela; um ponto só é aceito (sem `←→`). `←→` andam pela lista. O tipo
pode ser `i32`, `f32` ou `f64` (`f64` só no `add` de `double`). As outras regras de carga da spec 006 (6.5) valem.
Patches com `slot` e grades existentes não mudam. O `unit` continua sufixo (`5x`).

### 6.10 `gt caves`, `gt diff`, `gt ue player` e `gt-patch observe`

- `gt caves`: só leitura. Lista as sequências de `CC` (endereço, tamanho, quanto está em uso), a folga da `.data` e,
  pelo diário, os desvios, caves e variáveis nossos ativos.
- `gt diff`: desvios e caves nossos aparecem marcados como nossos (pelo diário), separados de patches de outros.
- `gt ue player`: mostra `PartyComponent.ResearchPoints` (só leitura), pelo caminho do `game_state`.
- `gt-patch observe` (6.7.3): ferramenta de pesquisa; instala, mostra e devolve na saída, com o mesmo motor.

### 6.11 Na TUI

- `brain-power`, `coleta-multiplicada` e `analisador-infinito` são linhas `[ON ]`/`[OFF]` como os patches; os dois
  primeiros com valor (regras da spec 003, 6.2).
- `quantidade-arrastar` e `pular-hora` são linhas `[ação]`.
- `congelar-hora` é um `set` como os da 007.
- Os desvios ligados seguem as regras dos patches: religar depois de reconectar (mesma chave de versão), suspensão
  com motor `stuck`, saída pelo motor.
- Os quatro desvios usam a reflexão (offsets ou conferência), então ficam
  `verificando…` até o índice da reflexão, como os cheats de valor, e `indisponível (campo X não encontrado)` se o
  campo faltar. Religar depois de reconectar espera a reflexão.

## 7. Segurança e save

- Antes do primeiro teste que escreve no jogo, o dono faz backup do save (`AGENTS.md` 3).
- **Brain Power** e **coleta**: o progresso e os itens extras vão para o save (propósito dos cheats).
- **Analisador**: cargas cheias ficam depois de desligar. **Arrastar**: pilhas acima do máximo podem ir para o save.
- **Hora**: `pular-hora` muda a hora salva; `congelar-hora` devolve o original ao desligar.
- `AGENTS.md` 3 ganha a ressalva dos desvios: escrita de cave em `CC` e de variáveis na folga da `.data` (exceção à
  regra da 005), cave mantida até sair, limpeza com conferência do `pc`, variáveis sem diário de valor.

## 8. Testes

| Teste | Exige jogo? |
|---|---|
| Achar folgas (função pura): sequências de `CC`, margem de 16, `tamanho ≥ cave + 16`, várias caves na mesma sequência sem sobreposição; folga da `.data` pelo `VirtualSize` e pelo `maps` | Não |
| Montagem do desvio (função pura): `jmp rel32` + `NOP`; instrução original relocada (com e sem RIP relativo); recusa instrução < 5 bytes e distância > ±2 GB | Não |
| Grade por lista: carga (crescente, `default`, inteiros, um ponto só), `←→`, `f64`; grades antigas iguais | Não |
| Assinaturas: os quatro desvios e os dois cheats de hora carregam; erros de carga do formato novo; `add` com `double`, `at_least` e um ponto | Não |
| Módulo sintético: ligar um desvio numa função "soma" multiplica; trocar valor muda o resultado sem pausa; desligar devolve; sair limpa cave e variáveis (`.text` igual à cópia "em disco") | Não |
| Módulo sintético com uma thread chamando a função em laço: 1000 ligar/desligar e a saída sem crash, resultado sempre o original ou o multiplicado | Não |
| Limpeza com thread parada dentro da cave (thread de teste segura num laço dentro dela): a cave não é apagada; com a thread fora, é | Não |
| SIGKILL com desvio ligado, e entre escrever a cave e o desvio: a próxima execução e o `gt-patch restore` devolvem tudo, na ordem da 6.3 | Não |
| Pânico e SIGTERM com desvio ligado: tudo devolvido | Não |
| Recusas: bytes do desvio diferentes, `CC` sobrescrito, folga não zero no primeiro uso, sem espaço | Não |
| Cave do Brain Power contra tabela sintética (fora de ordem inclusive): multiplica; limita em `L2 − 1`; nunca abaixo do ganho original; sem `L2` só limita ao `int32`; ganho ≤ 0 intacto; sem estouro; variáveis de registro | Não |
| Cave do analisador: grava `float(Max)` só com `Max > 0` e com `rcx` da classe certa | Não |
| Cave do arrastar: grava uma vez e zera; desarmado não grava; `rcx` nulo ou de outra classe não grava; duas threads armadas uma vez aplicam uma só | Não |
| Cave da coleta (módulo sintético, escrito **depois** da pesquisa, com o mecanismo escolhido; os casos abaixo valem para qualquer mecanismo): multiplica só no inventário da variável; limita à pilha máxima; pilha máxima 1 intacta; N = 0 e inventário nulo não fazem nada; chamada que não é de uma das três fontes não multiplica | Não |
| Todas as caves com as variáveis zeradas: passam direto, resultado original | Não |
| Cada cave gerada, desmontada: sem `call`, sem `ret`, sem `int3`/`CC`; termina no `jmp` de volta | Não |
| Desarmar o arrastar com a cave aplicando ao mesmo tempo (thread de teste): uma aplicação no máximo, e o registro diz `aplicada` ou `desarmado` conforme o contador | Não |
| `pular-hora`: recusa com `v < 0` e com `v + 3600 > result_max` | Não |
| Padrão do Brain Power com `disp` diferentes entre si ou da reflexão: `indisponível (padrão incoerente)` | Não |
| Cave de observação: conta, guarda retornos no anel, não muda registradores nem o resultado | Não |
| Calendário sintético: salto com classe dona (`GameState` de outra classe recusado); `set` de `float`; `add` de `double` com releitura "maior ou igual" (igual, acima, muito acima com a nota, abaixo = não pegou) | Não |
| TUI com o backend falso: desvio com valor; ação de armar (`aplicada`, desarmar, `expirou`, recusa com motor `stuck`, não religa ao reconectar); `verificando…` dos desvios até a reflexão; registro do Brain Power; `pular-hora` com `não pegou`; desvio ligado religado ao reconectar | Não |
| `gt caves`, `gt diff` (desvio marcado como nosso) e `gt ue player` (`ResearchPoints`) contra o módulo sintético | Não |
| Com o jogo aberto: os desvios disponíveis; `brain-power` liga e desliga parado (sem ganho durante o teste, nada vai ao save; o analisador fica de fora, porque enche as cargas); depois da saída do motor (`undo_all`), `gt diff` sem diferença e `gt caves` sem nada nosso | Sim (`#[ignore]`) |

Brain Power, coleta, arrastar e pular hora **não** têm teste automático que escreve no jogo (vão para o save): são
testados à mão (seção 9).

## 9. Critério de pronto

Com backup do save feito pelo dono e o FLiNG fechado:

1. A TUI mostra os seis (ou cinco, se a coleta sair pela 6.7.4) como disponíveis nas categorias certas.
2. **Brain Power** (critério da fase): com ×5, os ganhos no jogo aparecem no registro como `+X → +5X` (ou com
   `ganhos não mostrados`, se vierem juntos) e o
   `ResearchPoints` (`gt ue player`) sobe nessa medida. Quando `ResearchPoints` passa do "próximo nível em" (`L1`)
   registrado, o jogo sobe um nível.
   Se der para chegar perto de um limite, um ganho que passaria de dois sobe só um, com a recompensa. Desligado,
   volta ao normal.
3. **Analisador**: com o cheat ligado, as cargas não acabam. Desligado, voltam a gastar.
4. **Arrastar**: armar com 250 e arrastar um item: a pilha fica com 250 e o registro diz `aplicada`; o próximo
   arrasto não muda nada. Armar e apertar de novo desarma. Anotar o efeito de 999 num item de pilha menor e depois de salvar e carregar.
5. **Coleta**: com ×3, pegar do chão, colher, pegar loot de uma criatura e criar um item: cada um vem ×3. Tirar do
   baú: vem ×1. Soltar um item e pegar de volta: vem ×1.
6. **Congelar hora**: o relógio para; desligado, volta. Tentar dormir com a hora congelada e anotar.
7. **Pular hora**: cada toque avança 1 hora, com e sem a hora congelada; segurar avança no máximo uma vez por
   segundo.
8. Reiniciar o jogo com a TUI aberta e os desvios ligados: voltam ligados depois de reconectar (o arrastar, não).
9. Sair da TUI com tudo ligado: `gt diff` limpo e `gt caves` sem nada nosso.
10. `kill -9` na TUI com os desvios ligados e abrir de novo (ou `gt-patch restore`): tudo devolvido, jogo estável.
11. O dono confirma que o jogo ficou estável e que o save carrega normal.

Com o item 2, a **Fase 6 fecha**.

## 10. Documentos a atualizar

- `docs/RESEARCH.md`: pesquisa de 2026-10-27, resultados da cave de observação (arrastar e coleta) e do teste.
- `docs/ARCHITECTURE.md`: 5.5 (desvio de uma instrução, alocador, cave e variável como tipos de escrita, ordem de
  ligar/desligar/sair, limpeza com `pc`, cave de observação), 5.3 (formato do desvio, `points`, `add` de `double`,
  `at_least`, classe dona no salto), 5.4 (entradas novas do diário) e registro de decisões.
- `AGENTS.md` 3: ressalva dos desvios (7).
- `docs/specs/005-sobrevivencia-por-reflexao.md` e `006-progressao-e-movimento.md`: notas da exceção da `.data` e do
  `add` generalizado.
- `docs/ROADMAP.md`: Fase 6 com os seis cheats; god mode por desvio fora; status ao terminar.

## 11. Decisões tomadas

| Decisão | Quem |
|---|---|
| Fase 6 = Brain Power, hora do dia, analisador, quantidade ao arrastar e coleta multiplicada | Dono |
| Brain Power como multiplicador ×2 a ×10, limitado para não pular nível | Dono |
| Hora: congelar e pular 1 hora por tecla | Dono |
| Arrastar: uso único por tecla, 10 a 999 | Dono |
| Coleta: mundo, loot e criação, ×2 a ×10; baú não; dentro da 008 | Dono |
| Item solto pelo dono e pego de volta não multiplica | Dono |
| God mode por desvio fora | IA |
| Hora por valor (`CalendarComponent`), congelar por `GameToRealTimeRatio = 0` | IA (spike) |
| Desvio de uma instrução de 5+ bytes; pontos próprios (Brain Power em `+5970ee7`, arrastar no começo da função) | IA |
| Brain Power no ponto onde a tabela já está em `rax`: sem `call` na cave, offsets das instruções | IA |
| Cave e variável como tipos novos de escrita; exceção explícita para a folga da `.data` | IA |
| Variáveis sem diário de valor; limpeza zera a região | IA |
| Cave mantida ao desligar; apagada na saída só sem thread dentro (`pc`), até 5 tentativas | IA |
| Arrastar com uma variável só (0 = desarmado), `xchg`, conferência da classe `Item`, desarmar na segunda tecla e expirar em 60 s | IA |
| Coleta: só o inventário do jogador (variável mantida pelo trainer), limitado à pilha máxima, pilha 1 intacta | IA |
| Padrão do Brain Power estendido para conferir registradores e o offset de `ResearchPoints` | IA |
| Orçamento fixo das variáveis; observação uma de cada vez, no `gt-patch` | IA |
| Desarmar sem operação atômica: escrever 0 e decidir pelo contador de aplicadas | IA |
| Releitura "maior ou igual" no motor para o `pular-hora` | IA |
| Coleta como tentativa, com a cave de observação para achar os chamadores | IA |
| Grades: ×2/×3/×5/×10 (padrão ×2) e 10/50/100/250/500/999 (padrão 100); `unit` sufixo | IA, a partir das faixas do dono |
| `pular-hora`: 3600 fixos; aplicada se releitura ≥ `v + 3600` | IA |
| Corpo das caves em Rust; TOML com o ponto e o valor | IA |

## 12. Deixado para quem implementa

- O formato do desvio no TOML e como ele nomeia o corpo da cave; o formato da classe dona no `[[field]]`.
- O formato das entradas novas do diário (diários antigos continuam legíveis).
- Quais registradores e flags cada cave salva (a partir da desmontagem de cada ponto).
- O nome e a forma do comando da cave de observação.
- A ordem do plano: motor, cave de observação e as pesquisas do arrastar e da coleta primeiro; os cheats por desvio
  depois; `pular-hora` pode vir antes; `congelar-hora` só depois que a 007 estiver na `main`.
- O texto exato das descrições e mensagens.
- Montar as caves com a feature `code_asm` do `iced-x86` ou com `Instruction::with*` e `BlockEncoder`.
