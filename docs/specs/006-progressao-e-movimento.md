# Spec 006 — Carga, dano, ciência e movimento (reflexão do Unreal)

| Campo | Valor |
|---|---|
| Fase | 5, segunda e última parte (ver `docs/ROADMAP.md`) |
| Estado | Implementada e confirmada no jogo pelo dono em 2026-10-26 (Fase 5 fechada) |
| Data | 2026-10-26 |

## 1. Intenção

Fechar a Fase 5 com os cheats de valor que faltam: **capacidade de carga**, **multiplicador de dano**, **somar
ciência**, **velocidade**, **pulos infinitos** e **gravidade**. Tudo reaproveita o que a spec 005 construiu: o leitor
de reflexão (`gt-ue`), o caminho até o personagem, a escrita em dados do motor com diário, os blocos `[[field]]` e a
manutenção de ~10 Hz da TUI. Nenhum offset de campo vai para o código ou para as assinaturas: tudo vem da reflexão, por
nome.

A spec 005 tinha dois tipos de cheat de valor: **travas de barra** (reescrevem um valor que o jogo muda o tempo todo e,
ao desligar, só param) e **bits** (limpam bits e devolvem o original ao desligar). Esta spec acrescenta dois:

- **Ajuste** (`set`): troca um valor de configuração do jogo (um multiplicador, uma quantidade) pelo nosso enquanto
  ligado e **devolve o original ao desligar**, como os bits. A maioria tem valor ajustável com `←→`, como os patches
  com valor da spec 003.
- **Ação** (`add`): uma escrita única, feita quando o dono aperta a tecla, sem estado ligado e sem desfazer. Só a
  ciência usa, por decisão do dono.

Os campos foram achados na pesquisa de 2026-10-26 (`RESEARCH.md`, "campos da spec 006"). O FLiNG serviu só para
saber **quais** opções existem (`AGENTS.md` 3).

## 2. Objetivos

1. **Tipo `int`** nos `[[field]]` (hoje só `float` e `bool`): carga, pulos e ciência são `IntProperty`.
2. **Regra `set`** (6.4): original no diário antes da escrita, manutenção a ~10 Hz, original devolvido ao desligar, na
   saída e na recuperação depois de um SIGKILL.
3. **Valor ajustável em cheats de valor** (6.5): bloco `[field.value]` com a mesma grade e as mesmas teclas dos
   patches com valor.
4. **Regra `add`** (6.6), uma ação única na TUI.
5. **Raiz `game_state`** (6.3): a ciência não fica no personagem, e sim no `PartyComponent` do `GameState`.
6. **Seis cheats** (6.7), nas categorias ITENS, PROGRESSÃO e MOVIMENTO.
7. `gt ue player` mostra os campos novos; testes sem o jogo contra a reflexão sintética e testes `#[ignore]` com o
   jogo aberto.

## 3. Fora do objetivo

- **Teleporte** (salvar e carregar posição): o jogo tem viagem rápida e o dono quer explorar o mapa (decisão do dono).
- **Velocidade do jogo** (speedhack): menor prioridade, sem fase prevista.
- **Trava de ciência** ("ciência infinita"): só a ação de somar (decisão do dono).
- Editar vida e stamina máximas, bloqueio perfeito, sem atordoamento e furtividade (catálogo do FLiNG no
  `RESEARCH.md`): ficam para quando o dono pedir.
- Efeito dos cheats de movimento **montado**: a montaria usa o componente de movimento dela, e não o do personagem
  (6.7.3).
- **Sobrescrever `[[field]]` no `<chave>.toml`**: hoje o arquivo de versão só sobrescreve `[[patch]]` e `[ue]`, e
  continua assim. Os `[[field]]`, com o `[field.value]`, só existem no `generic.toml`.
- Configurações do modo de jogo (`SurvivalGameModeSettings`), Brain Power, multiplicador de defesa, hora do dia
  (Fase 6).
- **Riscos aceitos**: velocidade alta pode fazer o personagem atravessar o cenário ou cair do mundo, e gravidade baixa
  pode deixá-lo num lugar alto difícil de sair. O dono desliga o cheat; o autosave pode gravar essa posição, e o backup
  do save (7) cobre isso.

## 4. Anti-objetivos (falha mesmo que "funcione")

Valem todos os da spec 005 (seção 4), com uma exceção: o "valor fora da faixa normal do jogo" passa a ser aceito nos
`set` (dano ×100, carga 100, `JumpMaxCount` 1000), porque o original volta ao desligar, e na ciência, por decisão do
dono (7). Os valores continuam limitados pela grade de cada cheat. Além deles:

- Deixar um valor de configuração nosso no jogo depois de desligar, sair ou recuperar, quando o objeto ainda é o
  mesmo e o valor no campo ainda é o nosso, **inclusive** depois de um SIGKILL no meio de uma troca de valor (6.8).
- Devolver um "original" velho por cima de um valor que o jogo mudou enquanto o cheat estava ligado (um upgrade de
  carga, por exemplo). O valor do jogo vence (6.4).
- Restaurar pedaço de um valor: num `set`, ou o campo inteiro volta ao original, ou nada é escrito (6.8).
- Escrever ciência fora da faixa sã, ou somar sobre um valor atual absurdo.
- Somar ciência sem uma tecla do dono, ou mais de uma vez por segundo: religar depois de reconectar, recuperação,
  manutenção e procura do personagem nunca somam, e uma tecla segurada soma no máximo uma vez por segundo (6.6).
- Escrever no componente de movimento ou de carga de uma criatura ou da montaria.

## 5. Onde cada coisa mora

| O quê | Onde |
|---|---|
| Tipo `int`, regras `set` e `add`, bloco `[field.value]`, `root`, campos novos do `[ue.player]` | `crates/gt-grounded2/src/signatures.rs` e `generic.toml` |
| Grade de valores sem `slot` (6.5) | `crates/gt-core/src/value.rs` |
| Raiz `game_state`, plano das escritas `set`/`add`, leitura própria do caminho do `game_state` | `crates/gt-grounded2/src/player.rs` e `fields.rs` |
| Entrada de dados do diário com N bytes, modo inteiro e valor seguinte | `crates/gt-core/src/journal.rs`, `engine.rs`, `recovery.rs` (feature `write`) e `apps/gt-patch` (`restore`) |
| Valor ajustável e ação na TUI | `apps/gt-tui/src/` (`controller.rs`, `values.rs`, `ui.rs`, backends) |
| Reflexão sintética com `Outer`, `Level`, `World`, `GameState` e os campos novos | `crates/gt-testkit/`; ponteiros escritos pelos ajudantes de `crates/gt-core/src/mem/test_support.rs`, único lugar com `unsafe` |
| `gt ue player` | `crates/gt-grounded2/src/player.rs` (biblioteca) e `apps/gt-cli/` (formatação) |
| Achados da pesquisa e do teste no jogo | `docs/RESEARCH.md` |

A regra de dependência não muda.

## 6. Comportamento

Os dados de exemplo são do build `c935e799-0b27c000`, lidos em 2026-10-26.

### 6.1 Pesquisa (feita, só leitura)

A pesquisa de 2026-10-26 respondeu o que define os alvos (`RESEARCH.md`):

| # | Pergunta | Resposta |
|---|---|---|
| Q1 | Campo da carga | `SurvivalCharacter.HaulingComponent` → `HaulingComponent.Capacity` (`int`, 5) |
| Q2 | Dano causado | `SurvivalCharacter.CustomDamageMultiplier` (`float`, 1,0); multiplica o dano do atacante (Fase 3) e o jogo não o reescreveu atacando |
| Q3 | Ciência | `PartyComponent` do `GameState` (`InGameGameState.PartyComponent`), campo `ScienceFound` (`int`); caiu de 7400 para 6400 quando o dono gastou |
| Q4 | Velocidade | `MaxWalkSpeed` não muda ao correr. O cálculo da velocidade máxima do jogo multiplica por `CustomGroundSpeedMultiplier` (terra) e `CustomSwimSpeedMultiplier` (água), ambos `float` 1,0 no `MaineCharMovementComponent` |
| Q5 | Pulos | `Character.JumpMaxCount` (`int`, 1); o jogo só mexe no `JumpCurrentCount` |
| Q6 | Gravidade | `CharacterMovementComponent.GravityScale` (`float`, 1,0); não muda nem planando |
| Q7 | Save | Nenhum desses campos tem `CPF_SaveGame`. A ciência é salva pelo próprio jogo (persiste entre sessões) |

O que a pesquisa só de leitura **não** responde fica para o teste no jogo (seção 9): o efeito real de cada escrita,
se `MaximumCalculatedCharacterSpeed` (1500) ou outro teto limita a velocidade, se `JumpMaxCount` alto dá pulos no ar
e se o jogo reescreve algum desses campos ao carregar, renascer ou ganhar um upgrade. A manutenção (6.4) cobre a
reescrita; um cheat sem efeito no jogo sai da spec no fechamento (definição, testes dele e linha da TUI), com registro
no `RESEARCH.md`, como o P9 da 005.

### 6.2 Tipo `int`

`kind = "int"` aceita `IntProperty` (4 bytes, com sinal). Um `int_field` novo no `gt-grounded2` confere o nome do
tipo e o tamanho do campo achado pelo `find_field`, como `float_field` e `bool_field` já fazem; tipo ou tamanho diferente na reflexão deixa o cheat `indisponível (reflexão incoerente)`. As
regras `lock` da spec 005 continuam só com `float`.

### 6.3 Raiz `game_state`

Hoje todo `[[field]]` parte do pawn. O campo novo **`root`** (opcional, padrão `"pawn"`) escolhe a raiz:

- `"pawn"`: como na 005 (personagem, com `alt_pawn_fields` montado);
- `"game_state"`: controller → `Outer` → `Level.OwningWorld` → `World.GameState`.

O `Outer` é o `OuterPrivate` do cabeçalho do `UObject` (`[ue.layout.uobject]`, `outer = 0x20`, já no layout), e não
um campo da reflexão. Com o dono num save, o controller e o pawn têm o mesmo `Outer` (`PersistentLevel`). O
`[ue.player]` ganha (todos obrigatórios quando algum `[[field]]` usa `root = "game_state"`):

```toml
level_class = "/Script/Engine.Level"
world_field = "OwningWorld"
world_class = "/Script/Engine.World"
game_state_field = "GameState"
game_state_class = "/Script/Maine.InGameGameState"
```

**Conferência a cada leitura**, além da do controller (spec 005, 6.3, passo 1): cada salto é conferido com `is_a` da
classe esperada (`Outer` é `Level`, `OwningWorld` é `World`, `GameState` é `InGameGameState`). Depois vem o `object` do
`[[field]]` (ex.: `PartyComponent`), com o `PropertyClass` de cada salto, como os caminhos do pet, e a classe final
(`object_class`). As conferências de escrita da spec 005 (6.4: `PropertiesSize`, região `rw`, tamanho exato) valem
igual.

**Isolamento por raiz e por cheat.** Hoje uma falha no caminho do pawn derruba a leitura inteira (`read_player`
devolve erro), um componente nulo ou de classe errada derruba todos os cheats do pawn (só o pet tem o tratamento
`missing`, por `is_secondary`), e uma classe do pawn que falta derruba o catálogo inteiro. Com os componentes novos
(`HaulingComponent`, `CharacterMovement`), isso faria um componente ausente durante um carregamento desligar a
sobrevivência. Com esta spec:

- **Cada cheat é isolado**: o tratamento `missing` do pet passa a valer para **todo** objeto depois do pawn. Um
  componente nulo ou de classe errada deixa a marca de espera só nos cheats que dependem dele; os outros seguem. O
  texto da marca vem do tipo do objeto: `aguardando o pet` nos caminhos do pet (mais de um salto, como hoje) e
  `aguardando o personagem` nos componentes do personagem. Pawn nulo, pawn de classe errada (sem alternativa) ou controller inválido continuam afetando
  todos os cheats da raiz.
- **Cada raiz é isolada**: o caminho do `game_state` tem uma **função de leitura própria**, sob demanda, usada pela
  ação (6.6) e pelo `gt ue player`, fora do snapshot do pawn (que continua sendo o da manutenção). Uma falha no
  caminho do pawn não afeta a ação da ciência, e uma falha no caminho do `GameState` não afeta os cheats do pawn.
  Controller inválido ou ausente afeta todos.
- No índice, a falta de uma classe ou campo do caminho do `game_state` (`Level`, `World`, `InGameGameState`,
  `OwningWorld`, `GameState`) deixa `indisponível (classe X não encontrada)` ou `(campo X não encontrado)` **só** a
  ação, e a falta de uma classe do caminho do pawn deixa indisponíveis só os cheats do pawn.
- A linha `personagem:` do topo continua mostrando o personagem (pawn), e não o estado do jogo; depois de uma procura,
  ela segue o controller guardado também sem cheat ligado (6.6).

Combinações: nesta spec, `root = "game_state"` só é aceito com `rule = "add"`; com `lock`, `clear_bits` ou `set` é
erro de carga, para não ter caminho sem uso e sem teste. `object = "pawn"` com `root = "game_state"` também é erro de
carga. `add` com `root = "pawn"` é aceito (sem uso nesta spec). Dois `set` sobre o mesmo campo (mesmo `object` e
nome) são erro de carga, porque um guardaria o valor do outro como original.

Os cheats sobre o **próprio pawn** (`object = "pawn"`, como `dano` e `pulos-infinitos`) dependem só do pawn: pawn
nulo ou de classe errada (sem alternativa) os afeta como a todos os cheats da raiz.

### 6.4 Regra `set` (ajuste com original devolvido)

Um `[[field]]` com `rule = "set"` tem:
- `fields`: um ou mais campos do mesmo objeto, com o `kind` do bloco (`int` ou `float`), escritos com o mesmo valor;
- **exatamente um** entre o bloco `[field.value]` (6.5) e `target` constante (cheat só liga/desliga). Os dois, ou
  nenhum, é erro de carga;
- com `target`, `min`/`max` obrigatórios e o `target` dentro deles. É **só conferência de carga** (o valor escrito é
  sempre o `target`); `min`/`max` não limitam o valor lido do jogo, que segue a regra de "valor do jogo são"; com `[field.value]`, `min`/`max` soltos são erro de carga (a faixa é a da grade).

Outros erros de carga: `set` com `kind = "bool"`; `fields` vazio ou com nome repetido; `target` não inteiro com
`kind = "int"`; `[field.value]` em `lock` ou `clear_bits`; `lock` com `kind = "int"`.

As comparações de "valor nosso" são **por bytes** (os 4 bytes do campo), nunca por número, para que `NaN` e `-0.0`
não enganem. **Valor do jogo são** quer dizer finito e dentro de `[−1e9, 1e9]` (para `int`, a mesma faixa); isso vale para
qualquer valor lido do jogo que possa virar original.

**Ligar:** com o objeto conferido no mesmo ciclo (spec 005, 6.4), para cada campo:
1. lê o valor atual. Se ele não for são, não escreve: o cheat fica `[ON ]` com `valor do jogo inválido` e tenta de
   novo a cada ciclo;
2. se já existe uma entrada viva deste `id` e campo neste objeto (religar depois de uma falha), ela é mantida, com o
   original dela; senão, cria a entrada com **original = valor atual**, mesmo que o valor atual já seja igual ao nosso
   por coincidência;
3. grava o diário (6.8) **antes** de escrever, escreve e relê.

**Falha parcial** (`velocidade` tem dois campos): se um campo é escrito e outro é recusado (guarda, região `rw`,
releitura diferente), o cheat fica `[ON ]` e a manutenção tenta o campo que falta a cada ciclo; os campos já escritos
seguem com as entradas deles. Desligar resolve cada entrada que existir. Uma falha que deixa resto no motor segue a
spec 003 (6.8), como os bits.

**Manutenção (~10 Hz):** o alvo de cada campo é o **valor desejado** (o mostrado na tela, ou o `target`). Para cada
campo:
- sem entrada ainda (falha parcial ao ligar): faz o "ligar" daquele campo;
- com `next` e bytes atuais iguais ao `next` (a escrita aconteceu, mas a gravação final do diário não): finaliza a
  entrada (`written = next`, sem `next`);
- com `next` e bytes atuais iguais ao `written` (a escrita não aconteceu): retira o `next` e segue como abaixo;
- com entrada e bytes atuais iguais ao `written`, mas `written` diferente do desejado (uma troca de valor que falhou
  naquele campo): refaz a troca daquele campo pelo protocolo do `next` (abaixo);
- com bytes atuais diferentes do `written` (e do `next`), o jogo mudou o campo (ao carregar, renascer, por um
  upgrade). Se o valor do jogo é são, ele vira o **novo original**: a entrada do diário é atualizada antes, o valor
  desejado é escrito de novo e o log registra `<nome>: o jogo mudou o valor para X; novo original`. Assim, ao
  desligar, volta o valor que o jogo quer agora, e não um velho (seção 4). Se não é são, nada é escrito e o cheat fica
  `[ON ]` com `valor do jogo inválido` até o valor voltar a ser são.

**Limite conhecido:** se o jogo mudar o campo **relativamente ao valor atual** (somar +N num upgrade, multiplicar e
depois dividir num efeito), ele parte do nosso valor, e o original aprendido fica contaminado (ligado em 20, upgrade
+5 vira 25; ao desligar volta 25, não 10). O teste no jogo confere isso com um upgrade de carga e um efeito de
velocidade (seção 9). Se acontecer, aquele cheat volta ao dono antes de fechar.

**Registro de eventos:** as marcas (`valor do jogo inválido`, `o jogo não deixa`) e a volta ao normal geram uma linha
por mudança de estado, nunca uma por ciclo.

**Jogo brigando com o cheat:** se os campos de um cheat somam mais de 5 originais novos nos últimos 10 s (janela
deslizante, por cheat e objeto), o jogo está reescrevendo o valor o tempo todo. O cheat para de escrever naquele
objeto, em **todos** os campos: fica `[ON ]` com a marca `o jogo não deixa`, com uma linha no registro de eventos, e
cada entrada do diário dele é resolvida como num desligar (restaura se o valor for o nosso, senão esquece). A marca some, e o cheat volta a escrever, quando o objeto muda (outro pawn), quando o dono desliga e liga, ou numa
reconexão. Com a marca ativa, as setas só mudam o valor mostrado.
Isso limita a escrita do diário e as linhas de log a algumas por campo.

**Trocar o valor** com `←→` (ligado), campo a campo: grava `next = valor novo` no diário, escreve, relê e grava a
entrada com `written = valor novo` e sem `next`. Se a escrita é recusada antes de acontecer, o `next` é retirado do
diário e a manutenção tenta de novo (acima); se a releitura não bate, vale a regra do "jogo mudou o campo". Mesmo
intervalo mínimo de 150 ms entre trocas e mesma regra de "o último valor mostrado vence" da spec 003: aqui não há
pausa do jogo, mas cada troca grava o diário duas vezes, e o intervalo limita isso. As novas tentativas da manutenção
não são trocas pedidas por tecla e seguem o ciclo de 100 ms. Desligado, as
setas só mudam o valor mostrado.

**Desligar, sair e recuperar:** para cada entrada, pela regra de 6.8 (identidade do objeto, região `rw` e campo
inteiro igual ao nosso `written`, ou ao `next` numa troca interrompida): bate, escreve o original; o campo tem outro
valor, só esquece a entrada, com uma linha no log; identidade não bate, descarta sem escrever. Objeto novo (outro pawn,
outro `GameState`): a entrada antiga é resolvida antes de o objeto novo ganhar a dele, como nos bits (spec 005, 6.5.2).

### 6.5 Valor ajustável (`[field.value]`)

Campos: `type` (`i32` ou `f32`), `min`, `max`, `step`, `default`, `unit` e `decimals`, com o mesmo significado do
`value` dos patches (spec 003, 6.3), e **sem `slot`**. Regras de carga:
- as da spec 003 que não dependem do `slot` (`min ≤ default ≤ max`, `step > 0`, finitos e representáveis, `default`
  na grade, só inteiros no TOML para `i32`);
- o `type` tem de casar com o `kind` dos campos (`int` → `i32`, `float` → `f32`); `u32` é recusado nos `[[field]]`.

**No `gt-core`**, a grade (passos, valor de cada ponto, ponto mais próximo, formatação e codificação de 4 bytes) é
separada do `slot` num tipo próprio, usado pelos patches (que continuam com `slot`, `bytes()` e `check_slot()`) e pelos
`[[field]]` (que usam só a grade). O comportamento dos patches com valor não muda.

Na TUI, valem as regras da spec 003 (6.2) para valores: as setas mudam o valor mostrado também com o cheat desligado,
indisponível ou com religar pendente; ligar e religar usam o valor mostrado.

### 6.6 Regra `add` (ação)

Um `[[field]]` com `rule = "add"` tem um único campo `int`, o bloco `[field.value]` (a quantidade a somar, `i32`, com
`min > 0`) e o teto do resultado **`result_max`** (maior que o `max` da grade). `target`, `min`/`max` soltos ou
`kind = "float"` são erro de carga.

- Na TUI, a linha mostra `[ação]` no lugar de `[ON ]`/`[OFF]`, com a quantidade (`+1000`). `←→` trocam a quantidade;
  `espaço`/`Enter` pedem **uma execução**.
- **Executar**, como uma operação do laço principal (spec 003, 6.10), num ciclo só:
  1. o motor tem de estar disponível: com motor `stuck` ou sem motor (disco ilegível, diário não resolvido), recusa
     com a mesma mensagem que ligar um cheat receberia;
  2. revalida o caminho **naquele ciclo** (controller, raiz e objeto, spec 005 6.3 e 6.4, e 6.3 desta spec). Se o
     caminho falha com o controller válido, recusa com `estado do jogo ausente`. Quando **o controller** está ausente ou
     inválido, pede uma procura do controller, que segue o intervalo da tarefa de reflexão (`max(10 s, 2 × duração)`,
     spec 005 6.3), e recusa com `procurando o personagem` se a procura começou ou já estava em andamento, ou com
     `aguarde a procura` se o pedido caiu dentro do intervalo e não disparou nada. A linha `personagem:` segue o
     controller guardado também sem cheat ligado (`encontrado` ou `não encontrado` depois de uma procura pedida pela
     ação; `—` antes de qualquer procura); sem cheat ligado ou desejado, nenhuma procura nova roda sozinha. O controller achado fica guardado na sessão, como o achado para os outros cheats (spec 005,
     6.3), e é revalidado a cada uso. Quando uma procura pedida por uma ação termina, o registro de eventos diz
     `personagem encontrado: aperte de novo para somar` (ou `personagem não encontrado`). A ação **não** fica esperando
     nem executa sozinha;
  3. relê o `maps` se o endereço do objeto (o `PartyComponent`) mudou desde a última execução, ou se é a primeira, e
     confere a região `rw` (spec 005, 6.4);
  4. lê o valor atual `v` (a soma é feita em 64 bits). Se `v < 0` ou `v > result_max`, recusa (`valor atual fora da faixa`). Se `v + quantidade >
     result_max`, recusa (`passaria do máximo`);
  5. escreve `v + quantidade` e relê. Se a releitura não for `v + quantidade`, registra `Somar ciência: o jogo mudou
     o valor durante a escrita (lido X)` e **não** tenta de novo. A leitura e a escrita não são atômicas: um gasto do
     jogo nos microssegundos entre as duas seria sobrescrito, e a releitura não pega esse caso. O risco é aceito (o
     dono não gasta ciência no mesmo instante em que aperta a tecla);
  6. o registro de eventos mostra `Somar ciência: 7400 → 8400`.
- **No máximo uma execução por segundo**, por ação. A janela conta só a partir de uma execução que escreveu; uma
  recusa (passos 1 a 4) não abre janela. A janela vive na sessão e recomeça ao reconectar. Uma tecla que chega menos de 1 s depois da última execução, inclusive a
  repetição automática de uma tecla segurada (o terminal não distingue repetir de apertar), é descartada, com
  `Somar ciência: aguarde` no registro de eventos (uma vez por segundo, no máximo). Segurar a tecla soma, portanto, no
  máximo uma vez por segundo. O relógio é injetável para os testes.
- **Recusas no registro:** cada motivo de recusa (passos 1 a 4) gera no máximo uma linha por segundo, para uma tecla
  segurada não encher o registro.
- **O que conta como operação** (spec 003, 6.10): só uma execução que chega a ler o jogo (passos 3 a 6). Uma recusa
  dos passos 1 e 2 ou um descarte por `aguarde` não conta e não descarta teclas.
- **Sem diário e sem desfazer.** É a exceção à regra "toda escrita é reversível" (`AGENTS.md` 3), decidida pelo dono
  sabendo que a ciência vai para o save (7).
- Uma ação nunca está "ligada": não entra nos desejados, não é religada depois de reconectar, não tem manutenção e não
  é tocada na saída, no `undo_all`, no `Drop` do motor nem na recuperação. Ela tem o mesmo `indisponível (<motivo>)` e
  `verificando…` dos outros cheats de valor (spec 005, 6.7).

### 6.7 Os cheats

Todos começam `OFF` (a ação não tem estado). Na mesma categoria, a TUI mostra os `[[patch]]` antes dos `[[field]]`,
cada grupo na ordem do arquivo (spec 005, 6.5).

#### 6.7.1 ITENS

| `id` | Nome | Raiz e objeto | Campos | Regra e valor |
|---|---|---|---|---|
| `carga` | Capacidade de carga | pawn → `HaulingComponent` (`/Script/Maine.HaulingComponent`) | `Capacity` (`int`) | `set`; `i32` de 1 a 100, passo 1, padrão 20 |

#### 6.7.2 PROGRESSÃO

| `id` | Nome | Raiz e objeto | Campos | Regra e valor |
|---|---|---|---|---|
| `dano` | Multiplicador de dano | pawn (`/Script/Maine.SurvivalCharacter`) | `CustomDamageMultiplier` (`float`) | `set`; `f32` de 1 a 100, passo 1, padrão 10, unidade `x`, 0 casas |
| `somar-ciencia` | Somar ciência | `game_state` → `PartyComponent` (`/Script/Maine.PartyComponent`) | `ScienceFound` (`int`) | `add`; `i32` de 100 a 10000, passo 100, padrão 1000; `result_max` 100000 |

O multiplicador vale para o dano que **o personagem** causa (o atacante, `RESEARCH.md` Fase 3). Pela análise estática
(o campo só é lido do atacante), o pet e as torres não são afetados; isso é inferência, não testada.

**Carga ao desligar:** se o personagem estiver carregando mais itens do que o original quando a carga volta, o
comportamento do jogo é desconhecido (pode soltar itens). O teste no jogo confere isso (seção 9); se houver perda de
itens, volta ao dono. A grade começa em 1 porque o original pode mudar com upgrades; um valor abaixo do normal só
diminui a carga enquanto ligado.

#### 6.7.3 MOVIMENTO

| `id` | Nome | Raiz e objeto | Campos | Regra e valor |
|---|---|---|---|---|
| `velocidade` | Velocidade | pawn → `CharacterMovement` (`/Script/Maine.MaineCharMovementComponent`) | `CustomGroundSpeedMultiplier`, `CustomSwimSpeedMultiplier` (`float`) | `set`; `f32` de 0,5 a 4, passo 0,25, padrão 2, unidade `x`, 2 casas |
| `pulos-infinitos` | Pulos infinitos | pawn (`/Script/Engine.Character`) | `JumpMaxCount` (`int`) | `set`; `target = 1000`, faixa 1 a 1000, só liga/desliga |
| `gravidade` | Gravidade | pawn → `CharacterMovement` (`/Script/Maine.MaineCharMovementComponent`) | `GravityScale` (`float`) | `set`; `f32` de 0,1 a 2, passo 0,1, padrão 0,5, unidade `x`, 1 casa |

- **Montado:** os campos continuam sendo do personagem (`CachedSurvivalPlayerCharacter`, spec 005), então nada é
  escrito na montaria. A velocidade, os pulos e a gravidade do personagem não valem enquanto ele está montado; voltam
  a valer ao desmontar, sem o dono fazer nada.
- **Pulos infinitos** é uma tentativa: o esperado é poder pular de novo no ar. Se o teste mostrar que o jogo ignora
  `JumpMaxCount`, a alternativa (zerar `JumpCurrentCount` na manutenção, como o FLiNG) volta ao dono antes de entrar.
- **Velocidade:** com valores altos, o personagem pode bater num teto do jogo (`RESEARCH.md`). O teste anota o maior
  valor que ainda faz diferença; se houver teto, a grade é ajustada no fechamento.

### 6.8 Diário: entradas de dados de N bytes

A entrada de dados da spec 005 guarda um byte (original, escrito, máscara). Ela passa a guardar **N bytes**
contíguos, em dois modos:

- **bits** (os da 005): continuam com **1 byte** e uma máscara; só os bits da máscara voltam ao original, e só se
  esses bits forem os nossos;
- **inteiro** (os `set`): de 1 a 8 bytes (esta spec só usa 4); o campo é uma unidade. Restaurar exige que **todos** os bytes atuais sejam iguais ao
  `written` (ou ao `next`, abaixo); então escreve o `original` inteiro numa escrita só. Qualquer byte diferente: não
  escreve nada e só esquece a entrada. Nunca sobra um valor misturado. A máscara não se aplica a esse modo.

Campos novos da entrada: o modo, o tamanho, `original` e `written` como sequências de bytes do mesmo tamanho e
**`next`** opcional (só durante uma troca de valor, 6.4). Na restauração, um campo igual a `written` ou a `next` conta
como nosso, como na troca de valor dos patches (spec 003, 6.7). Assim, um SIGKILL entre gravar o `next` e escrever
ainda devolve o original.

- **Diários antigos** (spec 005, entradas de um byte sem modo) continuam legíveis e são tratados como entradas de modo
  bits com 1 byte.
- **Atualizar uma entrada** (original novo na manutenção, `next` na troca) grava o diário antes da escrita, como criar
  uma. Só a manutenção que acha um original novo grava o diário; a manutenção normal (valor já é o nosso) não grava.
- Uma entrada por cheat, campo e objeto; no máximo uma viva por `id` e endereço. **A identidade de uma entrada é o
  par (`id`, endereço)**, e não a igualdade da estrutura inteira: hoje o motor acha, remove e marca como pendente uma
  entrada comparando todos os campos, o que deixaria uma cópia velha depois de atualizar o original ou o `next`.
  Atualizar substitui a entrada do mesmo par; remover e marcar como pendente usam o par. **Mesmo objeto** é mesmo
  endereço **e** mesma guarda (vtable, `InternalIndex`, `ClassPrivate`): um objeto novo no mesmo endereço (guarda
  diferente) resolve a entrada antiga antes (spec 005, 6.5.2) e só então ganha a dele, com o original lido dele.
- A recuperação, o `gt-patch restore` e a conferência de região `rw` tratam as entradas de N bytes pela mesma regra.
- O `gt-core` continua genérico: confere bytes, sem saber o que é `int` ou `float`.

### 6.9 Na TUI

Valem as regras da spec 005 (6.7) para os cheats de valor: disponibilidade em duas etapas, `verificando…` até o
índice da reflexão, `aguardando o personagem`, a linha `personagem:`, suspensão da manutenção com motor `stuck`, sem
motor ou com resto pendente, religar depois de reconectar (só na mesma chave de versão, com o valor mostrado), saída
restaurando pelo motor. Além disso:

- Os `set` com valor mostram o valor como os patches com valor (`[ON ] Velocidade   2.00x`); `pulos-infinitos` não
  mostra valor.
- A ação mostra `[ação]` e a quantidade. Ela não entra na contagem de cheats ligados do topo.
- A procura do controller (spec 005, 6.3) ganha um gatilho: uma ação recusada por controller ausente ou inválido
  (6.6, passo 2), dentro do mesmo intervalo da tarefa.
- Montar, morrer, ir ao menu ou carregar outro save com um `set` ligado segue a spec 005 (6.5.2 e 6.7): `aguardando o
  personagem`, a entrada do pawn antigo resolvida antes, e o pawn novo ganha a dele.
- Ligar, desligar e trocar o valor de um `set` ligado contam como operação; a ação, só quando chega a ler o jogo
  (6.6). A manutenção não conta,
  mesmo quando grava um original novo no diário: não descarta teclas e não invalida buscas.

### 6.10 `gt ue player`

Como hoje, mostra os campos dos `[[field]]` (agora também carga, multiplicador de dano, ciência, os dois
multiplicadores de velocidade, `JumpMaxCount` e `GravityScale`), com o endereço de cada campo, e o caminho até o
`GameState` pelos nomes do `[ue.player]` (`Level`, `World`, `GameState`) e do `object` da ação (`PartyComponent`).
Nada fora das assinaturas é lido por nome fixo no código. Continua só leitura.

## 7. Segurança e save

- Antes do primeiro teste que escreve no jogo, o dono faz backup do save (`AGENTS.md` 3).
- **Ajustes (`set`):** nenhum campo tem a marca `SaveGame`; é indício, não prova, de que não vão para o save. Como o
  original volta ao desligar, um save gravado depois de desligar tem os valores do jogo. Com o cheat **ligado**, um
  save pode gravar o nosso valor, se o jogo salvar o campo por código próprio. O teste no jogo confere isso com todos
  os `set` (seção 9). Se algum for salvo, isso vai para o `RESEARCH.md` e o dono decide.
- **Variante da reversibilidade:** num `set`, o "original" restaurado é o último valor que o jogo pôs no campo, e não
  necessariamente o do momento de ligar (6.4).
- **Ciência:** vai para o save por decisão do dono. O resultado fica abaixo de `result_max` (100000, decisão a
  confirmar pelo dono) e é sempre `atual + quantidade`, nunca um número fixo. O save de pesquisa tinha 7400; não se sabe
  se o jogo chega a 100000 sozinho.
- `AGENTS.md` 3 ganha duas ressalvas: a das ações ("uma ação, spec 006 6.6, é uma escrita única sem desfazer, só onde
  o dono decidiu; caminho revalidado antes e faixa sã do resultado") e a do `set` (original trocado pelo valor do
  jogo; a conferência antes de escrever é a identidade do objeto e do campo mais o valor do jogo são, e não bytes
  esperados, como nas travas de barra).

## 8. Testes

| Teste | Exige jogo? |
|---|---|
| Assinaturas: `kind = "int"`; `rule = "set"` com `value` **ou** `target` (os dois, ou nenhum, recusam); `set` com `target` sem `min`/`max` ou fora deles; `set` com `value` e `min`/`max` soltos; `set` com `kind = "bool"`; `[field.value]` em `lock` ou `clear_bits`; `[field.value]` com as regras da 6.5 (grade, `default` na grade, inteiros para `i32`, `u32` recusado, `type` que não casa com o `kind`); `add` sem `value`, com `target`, com `kind = "float"`, com `min ≤ 0` ou com `result_max ≤ max`; `root` desconhecida; `root = "game_state"` com regra diferente de `add` ou com `object = "pawn"`; `root = "game_state"` sem os campos do `[ue.player]`; `set` com `fields` vazio ou repetido; `target` não inteiro com `kind = "int"`; `lock` com `kind = "int"`; `add` com `min`/`max` soltos; dois `set` sobre o mesmo campo; os seis `[[field]]` reais carregam | Não |
| `int_field` (`gt-grounded2`) com `IntProperty`: tipo e tamanho conferidos; campo com outro tipo vira `reflexão incoerente` | Não |
| Grade de valores do `gt-core` sem `slot`: os patches com valor continuam iguais (testes existentes) | Não |
| Plano do `set` (função pura): ligar cria a entrada antes; valor atual igual ao nosso sem entrada cria entrada com original = atual; valor do jogo não são ao ligar e na manutenção vira `valor do jogo inválido`, sem escrever; manutenção com valor do jogo são vira original novo; mais de 5 originais novos em 10 s vira `o jogo não deixa`, e a marca some com objeto novo ou ao desligar e ligar; troca de valor com `next`; manutenção com `next` e valor igual ao `next` finaliza a entrada, e com valor igual ao `written` retira o `next`; comparação por bytes (`NaN`, `-0.0`) | Não |
| Plano do `set` com dois campos: um escrito e outro recusado, ao ligar e numa troca de valor, fica `[ON ]` e a manutenção leva os dois ao valor desejado | Não |
| Motor: entrada atualizada (original novo ou `next`) seguida de falha na restauração não deixa cópia velha no pendente nem no diário; no máximo uma entrada viva por (`id`, endereço) | Não |
| Plano do `add` (função pura): soma, recusa com atual fora da faixa, recusa quando passaria do máximo, releitura diferente registrada sem repetir | Não |
| Diário: entradas de bits (1 byte) e inteiras de 1, 4 e 8 bytes; diário da spec 005 legível; restauração inteira com valor parcialmente igual (ex.: 3 dos 4 bytes nossos) não escreve nada; restauração com o valor igual ao `next` | Não |
| Recuperação e `gt-patch restore` com uma entrada `set` de 4 bytes: restaura quando a identidade bate e o valor é o nosso (`written` ou `next`); descarta quando não bate; só esquece quando o valor é do jogo | Não |
| **Reflexão sintética** (`gt-testtarget`): `Outer`, `Level`, `World`, `GameState`, `PartyComponent`, `HaulingComponent`, um componente de movimento e os campos novos, com offsets diferentes dos do jogo; o trainer acha tudo por nome, aplica os `set`, executa o `add` e restaura | Não |
| Na reflexão sintética: `Outer` que não é `Level`, `OwningWorld` nulo, `World` de classe errada, `GameState` de classe errada e `PartyComponent` nulo nunca são escritos; a ação relê o `maps` quando o `PartyComponent` muda de endereço; pawn nulo com controller válido mantém a ação disponível; `InGameGameState` ausente deixa só a ação indisponível, com os cheats do pawn funcionando; classe do caminho do pawn ausente deixa a ação funcionando; `HaulingComponent` nulo deixa só `carga` em `aguardando o personagem`, com `vida-infinita` funcionando, e o pet ausente continua com `aguardando o pet` | Não |
| SIGKILL com um `set` ligado, e SIGKILL entre gravar o `next` e escrever o valor novo: a próxima execução devolve o original pelo diário | Não |
| Pânico e SIGTERM com um `set` ligado: o original volta na saída | Não |
| Troca de pawn com um `set` ligado: o original do antigo é resolvido antes de o novo ganhar entrada; pawn novo no **mesmo endereço** com `InternalIndex` diferente também | Não |
| Montaria simulada no `Pawn`, com `CharacterMovement` e `HaulingComponent` próprios: os `set` saem do `CachedSurvivalPlayerCharacter` e nunca escrevem nos componentes da montaria; `CharacterMovement` nulo deixa só `velocidade` e `gravidade` esperando | Não |
| TUI com o backend falso: valor ajustável num `[[field]]` (setas desligado e ligado, intervalo de 150 ms, o último valor vence, religar com o valor mostrado); linha `[ação]`; ação não entra nos desejados nem é religada; ação não é tocada na saída nem na recuperação; ação com controller ausente recusada e procura pedida, respeitando o intervalo; ação com `GameState` ausente e controller válido recusada sem pedir procura; ação recusada com motor `stuck` ou sem motor; segunda tecla em menos de 1 s depois de uma execução descartada, e recusa sem abrir janela (relógio injetado); ação fora da contagem do topo; ação em `verificando…` e `indisponível (<motivo>)`; recusa e `aguarde` sem descartar teclas; procura pedida pela ação termina com a linha `personagem encontrado: aperte de novo`; marcas `valor do jogo inválido` e `o jogo não deixa` na linha e no registro, uma linha por mudança de estado; recusas repetidas da ação (tecla segurada) no máximo uma linha por segundo por motivo; `aguarde a procura` dentro do intervalo; a linha `personagem:` seguindo o controller guardado depois de uma procura pedida pela ação; com `o jogo não deixa`, as setas só mudam o valor mostrado, e a reconexão zera a marca; a janela de 1 s recomeça ao reconectar; manutenção do `set` suspensa com motor `stuck`, sem motor ou com resto pendente; desligar um `set` com falha na escrita fica `erro` e bloqueia entrada nova do `id` até o `r`; a manutenção do `set`, mesmo gravando um original novo, não descarta teclas nem invalida buscas | Não |
| `gt ue player` com os campos novos (função de biblioteca, contra a reflexão sintética) | Não |
| Com o jogo aberto: os seis disponíveis e verificados; `gt ue player` lê valores coerentes | Sim (`#[ignore]`) |
| Com o jogo aberto: liga e desliga `carga`, `dano`, `velocidade`, `pulos-infinitos` e `gravidade` e confere o original no fim | Sim (`#[ignore]`) |

Os testes existentes continuam passando. Nenhum teste sem o jogo depende de offsets do Grounded 2. A ação de somar
ciência **não** tem teste automático com o jogo (ela vai para o save): é testada só à mão (seção 9).

## 9. Critério de pronto

Com backup do save feito pelo dono e o FLiNG fechado:

1. A TUI mostra os seis como disponíveis nas categorias certas, e `gt ue player` mostra os valores novos.
2. **Carga:** com o cheat em 20, dá para carregar mais itens do que os 5 normais. Desligado, volta a 5. Desligar
   **carregando mais de 5** itens: anotar o que o jogo faz (solta itens, perde itens, nada). Se perder itens, vale
   6.7.2. Com o cheat ligado, comprar ou ganhar um upgrade de carga, se houver um à mão, e conferir o valor ao
   desligar (6.4, limite conhecido). Com o cheat ligado e carregando 10 itens, baixar o valor com `←` até 5 e anotar o
   que acontece com os itens.
3. **Dano:** com ×10, o golpe tira visivelmente mais vida de um inimigo. Desligado, volta ao normal.
4. **Somar ciência:** com +1000, a ciência no jogo sobe 1000 a cada execução, e segurar a tecla não soma mais de uma
   vez por segundo. Depois, gastar ciência numa compra: o saldo cai do valor somado. Salvar, carregar o save: a
   ciência continua lá.
5. **Velocidade:** com ×2, andar, correr e nadar ficam mais rápidos. Anotar o maior valor que ainda faz diferença.
   Desligado, volta ao normal. Com o cheat ligado, pegar um efeito de velocidade do jogo (comida, mutação ou o que
   houver à mão) e conferir, ao desligar, que os multiplicadores voltam a 1,0.
6. **Pulos infinitos:** dá para pular de novo no ar. Desligado, volta a um pulo. Se não funcionar, vale 6.7.3.
7. **Gravidade:** com ×0,5, os pulos ficam mais altos e as quedas mais lentas. Desligado, volta ao normal.
8. **Montado, morte e menu:** com os cheats de movimento ligados, montar e desmontar não dá erro, e eles voltam a
   valer a pé. Morrer ou voltar ao menu com os `set` ligados: a TUI mostra `aguardando o personagem` e retoma sozinha
   quando o personagem volta, com os valores do jogo como originais (conferido com `gt ue player`).
9. **Save:** com carga (carregando mais de 5 itens), dano, velocidade, pulos e gravidade ligados, fazer um **save
   manual**; fechar o jogo **sem salvar de novo** (e sem a TUI restaurar antes de um autosave: fechar a TUI depois do
   jogo); abrir o jogo sem a TUI e carregar esse save: os cinco campos com os valores do jogo (conferido com
   `gt ue player`), e anotar o que aconteceu com os itens carregados. Se não, vale a seção 7.
10. Reiniciar o jogo com a TUI aberta e os `set` ligados: depois de reconectar, eles voltam ligados, com o mesmo
    valor, e a ação não é repetida.
11. Sair da TUI: os originais voltam (`gt ue player`), e a `.text` não tem alteração (`gt diff`).
12. O log da TUI não tem linhas de "novo original" se repetindo durante o teste, nem `o jogo não deixa`.
13. O dono confirma que o jogo ficou estável e que o save carrega normal depois.

Com isso, a **Fase 5 fecha**, com a pendência da spec 005 (confirmar na próxima atualização do jogo que tudo continua
funcionando sem mudar o trainer) anotada no ROADMAP.

## 10. Documentos a atualizar

- `docs/RESEARCH.md`: resultado do teste no jogo (efeito de cada cheat, teto da velocidade, save, ciência).
- `docs/ARCHITECTURE.md`: 5.3 (`kind = "int"`, regras `set` e `add`, `[field.value]`, `root`, campos novos do
  `[ue.player]`, `[[field]]` sem sobrescrita por versão), 5.4 (entradas de dados de N bytes, modos bits e
  inteiro, `next`), 5.6 (raiz `game_state` com leitura própria e isolamento por cheat), 5.7 (linha `[ação]`), 3 (grade de valores no
  `gt-core`) e registro de decisões.
- `AGENTS.md` 3: ressalvas da ação e do `set` (7), inclusive valores acima da faixa normal do jogo nos `set` (4).
- `docs/specs/002-motor-patches.md`: nota de que as entradas de dados do diário passam a ter N bytes, dois modos e
  `next` (spec 006, 6.8).
- `docs/specs/005-sobrevivencia-por-reflexao.md`: nota em 6.3 de que a procura do controller tem um gatilho novo (a
  ação, spec 006) e de que o isolamento por cheat vale para todos os componentes; e em 6.5, de que a spec 006 traz
cheats de valor com valor ajustável.
- `docs/specs/003-interface-tui.md`: nota de que cheats de valor também podem ter valor e de que existe a linha
  `[ação]` (spec 006).
- `docs/ROADMAP.md`: teleporte e velocidade do jogo fora da Fase 5; status ao terminar; Fase 5 fechada.

## 11. Decisões tomadas

| Decisão | Quem |
|---|---|
| Sem teleporte: o jogo tem viagem rápida e o dono quer explorar | Dono |
| Dano, velocidade, gravidade e carga com valor ajustável pelas setas; ao desligar volta o original do jogo | Dono |
| Ciência como ação de somar, sem trava, sabendo que vai para o save | Dono |
| Velocidade do jogo (speedhack) fora | Dono |
| Ação: no máximo uma execução por segundo (tecla segurada soma uma vez por segundo); sem controller, recusa e pede a procura (não fica esperando) | IA, aprovada pelo dono |
| Num `set`, o original devolvido ao desligar é o último valor que o jogo pôs no campo, e não o do momento de ligar | IA, aprovada pelo dono |
| Ressalvas novas no `AGENTS.md` 3: ação sem desfazer, `set` sem bytes esperados e valores acima da faixa normal nos `set` | IA, aprovada pelo dono |
| `o jogo não deixa`: o cheat para naquele objeto até mudar o objeto ou desligar e ligar, sem nova tentativa por tempo | IA, aprovada pelo dono |
| Carga baixada abaixo dos itens carregados enquanto ligado: risco aceito de o jogo soltar ou perder itens (o teste anota) | IA, aprovada pelo dono |
| Valores acima da faixa normal do jogo nos `set` (dano ×100, carga 100, `JumpMaxCount` 1000), porque o original volta | IA, aprovada pelo dono |
| Carga com grade a partir de 1 (abaixo do normal); se desligar carregando mais itens perder itens, volta ao dono | IA, aprovada pelo dono |
| `root = "game_state"` só com `add` nesta spec; isolamento por cheat para todo componente | IA |
| Identidade da entrada de dados pelo par (`id`, endereço) | IA |
| Teto da ciência (`result_max`) em 100000 | IA, aprovada pelo dono |
| Regra `set` com original no diário; valor mudado pelo jogo durante o cheat vira o original novo; mais de 5 em 10 s para de escrever (`o jogo não deixa`) | IA: não devolver um original velho por cima de um upgrade, sem brigar com o jogo |
| Entrada de dados do diário de N bytes (até 8), modos bits e inteiro, com `next` na troca de valor; compatível com a da 005 | IA |
| Raízes independentes (pawn e `game_state`): leitura própria do caminho do `game_state` e índice separado | IA |
| Grade de valores separada do `slot` no `gt-core` | IA |
| `[[field]]` continua sem sobrescrita no `<chave>.toml` | IA |
| Velocidade pelos multiplicadores do próprio jogo (`CustomGround`/`CustomSwimSpeedMultiplier`), não pelo `MaxWalkSpeed` | IA: o `MaxWalkSpeed` não é a velocidade de corrida (pesquisa) |
| Pulos por `JumpMaxCount = 1000`, sem valor ajustável | IA |
| Ciência pelo `GameState` (`root = "game_state"`), com cada salto conferido por classe | IA: o `CachedPartyComponent` é nulo |
| Faixas e padrões: carga 1–100 (20), dano ×1–×100 (×10), ciência +100–+10000 (+1000), velocidade ×0,5–×4 (×2), gravidade ×0,1–×2 (×0,5) | IA, aprovada pelo dono |
| Montado: cheats de movimento só do personagem, sem efeito na montaria | IA |
| `id`, nomes e descrições em pt-BR desta spec | IA |

## 12. Deixado para quem implementa

- O formato exato de `root`, `[field.value]`, `result_max` e das regras no TOML (seguindo as regras de carga de 6.3 a
  6.6).
- O formato da entrada de N bytes no diário, desde que os diários da 005 continuem legíveis.
- O nome e a divisão do tipo da grade de valores no `gt-core`.
- Como o `gt-testtarget` monta o `Outer`, o `World` e o `GameState` sintéticos.
- O texto exato das descrições, dos motivos de recusa e da linha `[ação]`.
