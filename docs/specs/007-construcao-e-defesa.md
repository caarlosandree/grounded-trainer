# Spec 007 — Construir sem ingredientes e dano recebido (configurações do modo de jogo)

| Campo | Valor |
|---|---|
| Fase | Complemento da Fase 5 (cheats de valor por reflexão), antes da Fase 6 (ver `docs/ROADMAP.md`) |
| Estado | Aprovada pelo dono em 2026-10-27; implementada no branch, aguardando o teste no jogo |
| Data | 2026-10-26 |

## 1. Intenção

O dono quer, nesta ordem (conversa de 2026-10-26):

1. **Construir e criar sem ingredientes**, só com o que ele já desbloqueou no jogo. É o principal. Na construção de
   base, a construção também fica pronta na hora, sem levar materiais até a planta.
2. **Multiplicador de defesa** (dano recebido menor).

Os dois saem das **configurações do modo de jogo** (`SurvivalGameModeSettings`): um objeto vivo por mundo, com os
interruptores e multiplicadores que os modos Criativo e Personalizado do jogo usam. O FLiNG faz o primeiro por patch
de código (`+593AF1C`, lendo `[rax+0x148]`, e `+57689DB`) e o segundo por code cave (`+5943ec8`). Aqui os dois viram
**escritas de valor** com a regra `set` da spec 006, sem tocar no código do jogo. O FLiNG serviu só para saber
**quais** opções existem (`AGENTS.md` 3).

A defesa entra como **tentativa**: se o jogo não ler o multiplicador na hora, ela sai da spec no fechamento e volta
para a Fase 6 (code cave), decisão do dono.

## 2. Objetivos

1. **`root = "game_state"` com `rule = "set"`** (hoje é erro de carga, spec 006 6.3), com manutenção a ~10 Hz (6.3).
2. **`set` com `kind = "bool"`** para `BoolProperty` de byte inteiro, com o valor desejado **por campo** (6.4).
3. **Dois cheats** (6.5): `construir-sem-ingredientes` (ITENS) e `dano-recebido` (SOBREVIVÊNCIA).
4. `gt ue player` mostra o caminho até as configurações e os três campos; testes sem o jogo contra a reflexão
   sintética e testes `#[ignore]` com o jogo aberto.

## 3. Fora do objetivo

- **Liberar receitas não desbloqueadas** (`bUnlockAllRecipes`): o dono quer só o que já ganhou jogando.
- Construção instantânea separada de "sem ingredientes": um cheat só liga as duas coisas (decisão do dono).
- Outros campos da mesma classe (`PlayerDamageMultiplier`, `TimeOfDaySpeedMultiplier`, `bCreaturesIgnorePlayers`,
  `FallDamageMultiplier` e o resto): ficam para quando o dono pedir.
- **Multijogador**: as configurações valem para o mundo inteiro. Como host, devem valer para todos os jogadores; como
  convidado, o efeito é desconhecido (o servidor provavelmente manda). Não é testado nesta spec.
- **Code cave**, Brain Power rápido e hora do dia (Fase 6).
- Patches de código para construção (`+57689DB`) e para o modo de jogo (`+593AF1C`): o caminho é o de valor.

## 4. Anti-objetivos (falha mesmo que "funcione")

Valem os da spec 006 (seção 4). Além deles:

- Liberar receita ou construção que o dono ainda não desbloqueou.
- Deixar as configurações do mundo com o nosso valor depois de desligar, sair ou recuperar, quando o objeto ainda é
  o mesmo e o valor ainda é o nosso, inclusive depois de um SIGKILL.
- Escrever num objeto de configurações que não é o vivo do mundo atual (um CDO, como os `Default__…` que têm o
  mesmo componente, ou o de um mundo que já saiu). Exceção aceita: o intervalo curto até a procura do controller
  achar o controller novo depois de carregar outro save (6.3).
- Escrever num `bool` de bits (máscara diferente de `0xff`) como se fosse um byte inteiro.
- Um cheat do `game_state` parar porque o personagem sumiu (morte, menu de morte, montaria, pawn de classe errada), ou
  um cheat do pawn parar porque o caminho do `game_state` falhou. Exceção: controller ausente ou inválido para os dois
  (6.3).

## 5. Onde cada coisa mora

| O quê | Onde |
|---|---|
| `set` com `root = "game_state"` e com `kind = "bool"` (regras de carga) | `crates/gt-grounded2/src/signatures.rs` e `generic.toml` |
| Leitura do `game_state` no ciclo da manutenção, `bool` no plano do `set` | `crates/gt-grounded2/src/player.rs` e `fields.rs` |
| Reflexão sintética com `SurvivalModeManagerComponent` e as configurações, e um objeto com o bit de CDO | `crates/gt-testkit/` (e `crates/gt-ue/src/synth.rs` se precisar marcar as `ObjectFlags`) |
| `gt ue player` | `crates/gt-grounded2/src/player.rs` (biblioteca) e `apps/gt-cli/` (formatação) |
| Caminho do `set` de N bytes (1 ou 4) com valor desejado por campo (6.4) | `crates/gt-grounded2/src/player.rs` e `fields.rs` |
| Marca `aguardando o mundo`, estado de cada raiz no relatório do ciclo, procura do controller (6.3) | `crates/gt-grounded2/src/fields.rs` e `apps/gt-tui/src/` (`values.rs`, `controller.rs`, `ui.rs`, `backend.rs`, backend falso) |
| Achados da pesquisa e do teste no jogo | `docs/RESEARCH.md` |

O diário não muda: a entrada de modo inteiro da spec 006 (6.8) já aceita 1 byte.

## 6. Comportamento

Dados do build `c935e799-0b27c000`, lidos em 2026-10-26 com o dono num save (modo Normal).

### 6.1 Pesquisa (feita, só leitura)

| # | Pergunta | Resposta |
|---|---|---|
| Q1 | Onde está o objeto vivo | `GameState` (`BP_SurvivalGameState_C`) → `InGameGameState.SurvivalModeManagerComponent` (`+0x328`) → `SurvivalModeManagerComponent.GameModeSettings` (`+0x148`, o mesmo offset do FLiNG) → `BP_NormalModeSettings_C` (outer `Augusta_Main`), subclasse de `/Script/Maine.SurvivalGameModeSettings` |
| Q2 | Tipo dos campos | `bRecipesRequireIngredients` (`+0x3d`) e `bAutoCompleteBuildings` (`+0x3b`): `BoolProperty` de 1 byte, máscara `0xff`. `EnemyDamageMultiplier` (`+0x58`): `FloatProperty` |
| Q3 | Valores no modo Normal | `bRecipesRequireIngredients = 1`, `bAutoCompleteBuildings = 0`, `EnemyDamageMultiplier = 1,0` (`bUnlockAllRecipes = 0`) |
| Q4 | Save | Nenhum dos campos tem `CPF_SaveGame`. O ponteiro `GameModeSettings` é `Transient`: o objeto parece ser criado a partir do modo ao carregar o mundo. Indício, não prova |
| Q5 | Outras instâncias | Há um `SurvivalModeManagerComponent` em cada CDO de `GameState` (`Default__InGameGameState`, `Default__BP_SurvivalGameState_C` etc.); só o do `BP_SurvivalGameState_C` vivo (outer `PersistentLevel`) é o certo, e o caminho a partir do `GameState` vivo já o escolhe |

Os CDOs dos modos Criativo têm `bAutoCompleteBuildings = 1` e `bRecipesRequireIngredients = 0` (`RESEARCH.md`):
é exatamente o que o cheat liga.

O que a leitura **não** responde fica para o teste no jogo (seção 9): se o jogo lê os campos na hora (o patch do
FLiNG em `+593AF1C` lê o objeto no meio do jogo, o que é um bom indício para os dois `bool`); se "sem ingredientes"
vale também para a bancada e para consertar; se o `EnemyDamageMultiplier` muda o dano que o personagem recebe, e se
afeta também o pet e as construções; e se o save grava algum dos valores. Um cheat sem efeito sai da spec no
fechamento (definição, testes e linha da TUI), com registro no `RESEARCH.md`. O que fica da infraestrutura:
- sem a defesa, o `set` na raiz `game_state` continua usado pelo cheat de construção;
- sem o cheat de construção, a regra `bool` sai junto (definição, carga e testes), para não ficar regra sem uso, e o
  assunto volta ao dono antes de fechar, porque é o cheat principal;
- se **só uma das duas metades** do cheat de construção funcionar (por exemplo, a bancada sem ingredientes e a
  construção ainda manual), ele não fecha: volta ao dono, que decide entre manter, dividir ou trocar de caminho;
- sem os dois cheats, o `set` na raiz `game_state` e a regra `bool` saem também;
- a generalização do `set` para 1 ou 4 bytes fica em qualquer caso (não muda o comportamento da 006);
- se o cheat de construção liberar algo que o dono ainda não desbloqueou, ele não fecha: volta ao dono.

### 6.2 Caminho

Os dois cheats usam `root = "game_state"` e `object = "SurvivalModeManagerComponent.GameModeSettings"`, com
`object_class = "/Script/Maine.SurvivalGameModeSettings"` (a instância viva é uma subclasse de Blueprint, aceita pelo
`is_a`). O resolvedor já aceita caminhos com mais de um salto: os saltos do meio pelo `PropertyClass`, e o último
na leitura, com `is_a` contra o `object_class` (spec 006, 6.3). Nenhum offset vai para o código ou para as assinaturas.

### 6.3 `set` na raiz `game_state`

A regra de carga da spec 006 (6.3) muda: `root = "game_state"` passa a aceitar `add` **e** `set`. Com `lock` e
`clear_bits` continua erro de carga. As outras regras de carga do `set` (spec 006, 6.4) valem igual, inclusive a de
dois `set` sobre o mesmo campo; a chave da conferência passa a incluir a raiz (`root`, `object`, nome), porque o mesmo
`object` em raízes diferentes é outro objeto.

Hoje o caminho do `game_state` só é lido sob demanda (a ação e o `gt ue player`). Um `set` precisa de manutenção:

- **O ciclo de ~10 Hz lê o caminho do `game_state`** quando algum `set` dessa raiz está ligado. Sem nenhum ligado, o ciclo não lê esse caminho.
- **Isolamento por raiz** (spec 006, 6.3) vale para a manutenção: pawn nulo, de classe errada ou a montaria não
  afetam os cheats do `game_state`; caminho do `game_state` falhando não afeta os do pawn. Controller inválido ou
  ausente afeta os dois, como hoje.
- **Marcas de espera:**
  - controller ausente ou inválido (o caso comum ao voltar ao menu e carregar um save): todos os cheats ligados, das
    duas raízes, ficam com `aguardando o personagem`, como hoje. O controller é a raiz dos dois caminhos;
  - controller válido e um salto do caminho do `game_state` nulo, de classe errada, ilegível ou fora do objeto
    (`Outer`, `World`, `GameState`, `SurvivalModeManagerComponent`, `GameModeSettings`): **só** os cheats dessa raiz
    ficam com a marca nova **`aguardando o mundo`** (`Note` própria; texto exato com quem implementa, desde que não
    diga "personagem");
  - registro de eventos: só para os cheats do `game_state`, **uma linha por raiz** (não por cheat) quando o estado do
    caminho muda entre ok e `aguardando o mundo` (`mundo perdido`/`mundo encontrado`, texto com quem implementa). A
    espera por controller continua só com os eventos atuais de "personagem perdido/encontrado". As marcas do pawn e do
    pet não ganham linha nova;
  - quando o caminho volta, a manutenção retoma sozinha.
- **Procura do controller com o pawn nulo:** hoje, com o pawn nulo por mais de 30 s (`PAWN_NULL_RESEARCH`), a TUI
  **descarta** o controller e pede uma procura nova (spec 005, 6.3: um controller velho pode sobreviver depois de
  carregar outro save). Isso deixaria os cheats do `game_state` em `aguardando o personagem` durante a procura, que se
  repete enquanto o personagem está morto (depois dos primeiros 30 s, no intervalo da tarefa de reflexão). Com esta spec, o gatilho continua (mesmos 30 s, mesmo intervalo da tarefa),
  mas a procura roda **sem descartar** o controller guardado: ele segue em uso até a procura terminar. Se a procura
  acha um controller diferente, ele substitui o guardado e os objetos novos seguem a regra de objeto novo (a entrada
  do antigo é resolvida antes, pela guarda). Se acha o mesmo, nenhum (`Absent`), vários sem pawn (`Ambiguous`) ou falha (`Err`), o
  guardado continua em uso e nada muda; a procura se repete no intervalo normal. A lógica fica no `ValueCheats`
  (`values.rs`: o descarte no `tick` e o `merge` do resultado). Vale para as duas raízes: os
  cheats do pawn continuam em `aguardando o personagem` porque o pawn é nulo, não por causa da procura.
- **Risco aceito:** entre carregar outro save e a procura achar o controller novo (até o personagem novo existir e a
  procura seguinte terminar; com o personagem vivo, ~30 s mais a procura), os
  cheats do `game_state` podem continuar escrevendo nas configurações do mundo que saiu, se o controller velho ainda
  passar na conferência. É um objeto que o jogo já não usa; a guarda impede escrever em memória reaproveitada, e a
  entrada dele é resolvida (ou descartada) quando o objeto novo chega. O cheat parece ligado sem efeito nesse
  intervalo.
- **`maps` ao trocar de objeto:** hoje o `maps` só é relido quando o pawn muda. Passa a ser relido também quando o
  objeto final de um cheat do `game_state` muda (endereço ou guarda), antes da primeira escrita nele.
- **Pawn com problema e controller válido**: só controller ausente ou inválido afeta as duas raízes; **qualquer outra**
  falha do `read_player` (pawn nulo, de classe errada, montaria sem alternativa, classe do pawn ausente no índice,
  pawn ou campo `Pawn` ilegível ou fora do objeto): os cheats do `game_state` continuam escrevendo normalmente; só os do pawn esperam ou ficam
  indisponíveis. Hoje `read_player` falha em bloco nesses casos; a leitura do `game_state` no ciclo não pode depender
  dela.
- **Índice:** um campo ausente do caminho das configurações (`InGameGameState.SurvivalModeManagerComponent`,
  `SurvivalModeManagerComponent.GameModeSettings`, os três campos) ou a classe `SurvivalGameModeSettings` ausente deixa só os cheats que dependem dele
  `indisponível (classe/campo X não encontrado)`, como a ação na spec 006.
- O relatório do ciclo passa a ter o estado de cada raiz. A linha `personagem:` e os eventos "personagem
  encontrado/perdido" continuam seguindo **só o pawn**, mesmo quando só há cheats do `game_state` ligados; o estado
  do caminho do `game_state` aparece apenas nas linhas dos cheats e no registro de eventos (uma linha por mudança).
- A classificação da marca (`missing_kind`/`is_secondary`) passa a olhar a raiz: um caminho de mais de um salto na
  raiz `game_state` nunca é "pet".
- **Nunca num CDO:** antes de escrever, o objeto final da raiz `game_state` é conferido pelo bit de CDO das
  `ObjectFlags` (`[ue.layout.uobject]`, `flags` e `cdo_flag`, já no layout). Um CDO vira `aguardando o mundo` e não
  é escrito. O caminho a partir do `GameState` vivo não deveria chegar a um CDO; a conferência cobre um ponteiro
  errado.
- **Objeto novo** (outro mundo carregado, outro `GameState`, outras configurações no mesmo endereço com guarda
  diferente): a entrada do objeto antigo é resolvida antes de o novo ganhar a dele, como no pawn (spec 006, 6.4). O
  original do objeto novo é lido dele.
- Todas as conferências de escrita (`PropertiesSize`, região `rw`, tamanho exato, guarda do objeto) valem igual.
- A ação `somar-ciencia` não muda: continua lendo o caminho sob demanda e nunca entra na manutenção.

### 6.4 `set` com `kind = "bool"`

Hoje `set` com `kind = "bool"` é erro de carga. Passa a ser aceito assim:

- Cada campo tem o **seu** valor desejado (`true` ou `false`), e não um valor comum: o cheat de construção põe
  `bAutoCompleteBuildings = true` e `bRecipesRequireIngredients = false`. O formato no TOML fica com quem implementa;
  um campo sem valor, valor que não é `bool` ou nome repetido é erro de carga.
- Só liga/desliga: `[field.value]`, `target` comum e `min`/`max` soltos são erro de carga com `kind = "bool"`.
- **Só `bool` de byte inteiro**, conferido por uma função **nova** (`byte_bool_field`, ao lado do `int_field`), sem mexer no `bool_field` atual do `clear_bits`, que aceita qualquer máscara (bits de verdade como os do
  `nao-morrer`, `0x02`/`0x04`, e bools nativos como o `bTakeFallDamage` do `sem-dano-queda`). O `clear_bits` não
  muda. Com o que o
  `gt_ue::Field` já expõe, sem mudar o `gt-ue`: tipo `BoolProperty`, `size` (o `ElementSize`) igual a 1, `bits.mask
  = 0xff` e `bits.byte` igual ao `offset` do campo. Qualquer outra coisa deixa o cheat
  `indisponível (reflexão incoerente)`; bits continuam sendo assunto do `clear_bits`. A escrita é no `offset`.
- **Valor do jogo são** para `bool`: o byte é `0` ou `1`. Outro valor vira `valor do jogo inválido` e não é escrito
  (spec 006, 6.4).
- Escrita: `0x01` para `true`, `0x00` para `false`. No diário, uma entrada de modo inteiro de **1 byte** por campo
  (spec 006, 6.8), sem mudança no formato.
- **O caminho do `set` deixa de ser fixo em 4 bytes.** Hoje `FieldValues::Set`, `raw4`, `SetStep`, `plan_set`,
  `value_is_sane`, `encode_value`, a escrita do `set` e o `SetFieldReport` usam `[u8; 4]`, e o valor desejado é um só
  por cheat. Eles passam a trabalhar com o tamanho do campo (1 ou 4 bytes) e com um valor desejado **por campo**: nos
  `int`/`float`, todos os campos recebem o valor da grade ou o `target`, como hoje; nos `bool`, cada campo recebe o
  seu. Hoje `desired_value()` devolve `None` para um `set` sem `target` e sem grade, e o ciclo pula o cheat; o `bool`
  não passa por esse caminho: o valor de cada campo vem da definição. O valor mostrado na TUI (`set_desired`) não se
  aplica a ele. O comportamento dos `set` da 006 não muda.
- Ligar, manutenção, falha parcial, novo original quando o jogo muda o campo, `o jogo não deixa`, desligar, sair e
  recuperar: tudo como no `set` da spec 006 (6.4), campo a campo, comparando por bytes.

### 6.5 Os cheats

Ambos começam `OFF`.

| `id` | Nome | Categoria | Campos | Regra e valor |
|---|---|---|---|---|
| `construir-sem-ingredientes` | Construir sem ingredientes | ITENS | `bRecipesRequireIngredients` → `false`, `bAutoCompleteBuildings` → `true` | `set`, `kind = "bool"`, só liga/desliga |
| `dano-recebido` | Dano recebido | SOBREVIVÊNCIA | `EnemyDamageMultiplier` (`float`) | `set`; `f32` de 0 a 1, passo 0,05, padrão 0,25, unidade `x`, 2 casas |

Os dois com `root = "game_state"`, o objeto e a classe de 6.2. Ficam no fim do `generic.toml`, então aparecem no fim
das categorias deles na TUI (intencional).

- **Construir sem ingredientes:** vale para o que o dono já desbloqueou. O jogo decide o que está desbloqueado; o
  cheat não toca em `bUnlockAllRecipes`. O esperado (modo Criativo) é que receitas da bancada e construções não peçam
  materiais e que a construção fique pronta ao ser colocada. Construções que já estavam em andamento antes de ligar:
  comportamento desconhecido, anotado no teste.
- **Dano recebido:** é a tentativa do multiplicador de defesa. O valor é quanto do dano chega: `0,25x` é um quarto do
  dano; `0x` é nenhum dano. O nome segue o que o campo faz, e não "defesa", para não inverter a leitura do valor.
  `1x` é o normal do jogo. Se o teste mostrar que o campo não muda o dano recebido na hora, o cheat sai (seção 1).

### 6.6 Na TUI

Valem as regras da spec 006 (6.9) para os `set`. A linha de `dano-recebido` mostra o valor como os outros
(`[ON ] Dano recebido   0.25x`); a de `construir-sem-ingredientes` não mostra valor. Religar depois de reconectar
(mesma chave de versão) vale para os dois. A marca de espera do 6.3 aparece na linha, com uma linha no registro de
eventos por mudança de estado.

### 6.7 `gt ue player`

Mostra também o caminho até as configurações (`SurvivalModeManagerComponent`, `GameModeSettings`, com a classe da
instância) e os três campos com endereço e valor. Um `bool` aparece como `true`/`false`, com o byte cru ao lado. O relatório do `game_state` passa a ter **uma entrada
por caminho de objeto distinto** (na ordem dos `[[field]]`) (hoje um só objeto, o da ciência): o `PartyComponent` e as configurações, cada um com os saltos e os
campos dos cheats que o usam. O JSON pode mudar; não há consumidor externo. A parte do `game_state` é mostrada mesmo quando a do pawn falha (pawn
nulo, montado), desde que o controller seja válido. Quando um salto do caminho falha, a entrada daquele caminho mostra
os saltos que deram certo e o motivo (como o `error` do relatório do `game_state` hoje), sem os campos. Continua só
leitura.

## 7. Segurança e save

- Antes do primeiro teste que escreve no jogo, o dono faz backup do save (`AGENTS.md` 3).
- **Risco aceito pelo dono:** com o cheat ligado, um save pode gravar as configurações, se o jogo as salvar por código
  próprio. Se o trainer fechar sem restaurar (travamento, SIGKILL sem a recuperação rodar depois) e o jogo salvar, o
  mundo pode ficar "sem ingredientes" ou com dano reduzido até o dono abrir o trainer, ligar e desligar. A pesquisa
  (6.1, Q4) indica que o objeto é recriado ao carregar, o que deixaria o save limpo; o teste confere (seção 9).
- O cheat de construção muda o mundo para todos os jogadores numa partida com amigos (seção 3).

## 8. Testes

| Teste | Exige jogo? |
|---|---|
| Assinaturas: `set` com `root = "game_state"` aceito; `lock` e `clear_bits` com `root = "game_state"` continuam recusados; `set` com `kind = "bool"` com um valor por campo; recusa com valor faltando, valor não `bool`, `[field.value]`, `target` comum ou nome repetido; dois `set` sobre o mesmo campo do `game_state` recusados; os dois `[[field]]` reais carregam | Não |
| `byte_bool_field`: `size = 1`, `bits.mask = 0xff` e `bits.byte = offset` aceito; máscara de bit vira `reflexão incoerente`; o `clear_bits` (`sem-dano-queda`, `nao-morrer`) continua igual | Não |
| Conferência de `set` repetido com a raiz na chave: o mesmo `object` e campo em raízes diferentes é aceito | Não |
| `set` de N bytes: os `set` `int`/`float` da 006 continuam iguais; um `set` com campos de 1 byte e valor por campo liga, mantém e restaura; `gt ue player` mostra `true`/`false` | Não |
| Plano do `set` com `bool`: valor desejado por campo; byte diferente de 0 e 1 vira `valor do jogo inválido` sem escrever; novo original quando o jogo muda o byte; falha parcial com dois campos | Não |
| **Reflexão sintética** (`gt-testtarget`): `GameState` com `SurvivalModeManagerComponent` → `GameModeSettings` (subclasse da classe esperada) e os três campos, com offsets diferentes dos do jogo; o trainer acha tudo por nome, liga, mantém e restaura os dois cheats | Não |
| Na reflexão sintética: `SurvivalModeManagerComponent` nulo ou `GameModeSettings` nulo ou de classe errada nunca são escritos e deixam só os dois cheats em espera, com os do pawn funcionando; pawn nulo, pawn de classe errada e classe do pawn ausente no índice, com controller válido, mantêm os dois funcionando; `SurvivalModeManagerComponent`, `GameModeSettings` ou `SurvivalGameModeSettings` ausentes no índice deixam só os dois indisponíveis, com os do pawn funcionando; depois de `aguardando o mundo`, o caminho voltando faz a manutenção retomar sozinha; `GameModeSettings` apontando para um CDO (`Default__…`, com o bit de CDO) nunca é escrito e fica em espera; troca do objeto de configurações (outro endereço, e mesmo endereço com guarda diferente) resolve a entrada antiga antes; objeto novo numa região que só aparece depois (sem o pawn mudar) é escrito depois de reler o `maps` | Não |
| Manutenção não lê o caminho do `game_state` sem nenhum `set` dessa raiz ligado (o relatório do ciclo não traz estado dessa raiz, e um `GameModeSettings` nulo não gera marca nem linha) | Não |
| **`ValueCheats` real** (`values.rs`, testes de unidade com relógio injetado e/ou `apps/gt-tui/tests/` com o backend cru sobre o `gt-testtarget`): pawn nulo por mais de 30 s pede a procura sem descartar o controller, e os cheats do `game_state` seguem escrevendo, com cheats das duas raízes ou só dessa; resultado com controller diferente troca e resolve a entrada antiga antes; resultado igual, `Absent`, `Ambiguous` ou `Err` mantém o guardado | Não |
| SIGKILL com os dois ligados: a próxima execução devolve os originais pelo diário; pânico e SIGTERM: os originais voltam na saída | Não |
| TUI com o backend falso: `aguardando o mundo` só nos cheats do `game_state`, com os do pawn normais; controller ausente deixa as duas raízes em `aguardando o personagem`; com só cheats do `game_state` ligados, a linha `personagem:` segue o pawn; uma linha por raiz no registro quando o caminho do `game_state` cai ou volta, e nenhuma linha nova para as marcas do pawn; religar depois de reconectar | Não |
| `gt ue player` com o caminho e os campos novos (contra a reflexão sintética), inclusive com o pawn nulo e o controller válido | Não |
| Com o jogo aberto: os dois disponíveis e verificados; liga e desliga cada um e confere o original no fim (`gt ue player`) | Sim (`#[ignore]`) |

Os testes existentes continuam passando, menos os que afirmam o que esta spec muda e são reescritos para o
comportamento novo: em `signatures.rs`, `set_bool_refused` (passa a recusar só `bool` com `[field.value]`, `target`
comum, `min`/`max` ou campo sem valor) e `game_state_only_with_add` (passa a aceitar `set` e recusar `lock` e
`clear_bits`). O `controller_needs_search_after_30s_null` (`gt-grounded2`) continua valendo: o pedido de procura não
muda, só o descarte na TUI. Nenhum teste sem o jogo
depende de offsets do Grounded 2.

## 9. Critério de pronto

Com backup do save feito pelo dono e o FLiNG fechado:

1. A TUI mostra os dois como disponíveis nas categorias certas, e `gt ue player` mostra o caminho e os valores
   originais (`bRecipesRequireIngredients = true`, `bAutoCompleteBuildings = false`, `EnemyDamageMultiplier = 1.0`).
2. **Construir sem ingredientes:** ligado, colocar uma construção já desbloqueada sem ter os materiais: ela fica pronta
   na hora. Criar na bancada um item desbloqueado sem os ingredientes. Se só uma das duas coisas funcionar, vale 6.1.
   Anotar: consertar, construção que já estava em
   andamento, e se aparece algo que ainda não foi desbloqueado (não deve). Desligado, as receitas voltam a pedir
   materiais.
3. **Dano recebido:** com `0,25x`, levar dano de um inimigo e comparar com o normal; com `0x`, nenhum dano. Anotar se
   o pet e as construções também recebem menos dano. Desligado, volta ao normal. Se não mudar nada, vale 6.5.
4. **Morte e menu:** com só os dois ligados, morrer não muda nada neles (seguem valendo, também depois de 30 s
   morto). Voltar ao menu e carregar o save: a TUI mostra `aguardando o personagem` (ou `aguardando o mundo`) e
   retoma sozinha (`gt ue player` mostra os nossos valores no objeto novo). Desligando em seguida, `gt ue player`
   mostra os valores do jogo (os mesmos do item 1). Isso confere a volta ao normal; a troca de objeto em si é
   provada pelo teste sintético (seção 8).
5. **Save:** com os dois ligados, fazer um **save manual**; fechar o jogo sem salvar de novo, fechando a TUI depois do
   jogo; abrir o jogo sem a TUI e carregar esse save: os três campos com os valores do jogo (`gt ue player`) e as
   receitas pedindo materiais. Se não, vale a seção 7 e vai para o `RESEARCH.md`.
6. Reiniciar o jogo com a TUI aberta e os dois ligados: depois de reconectar, eles voltam ligados, com o mesmo valor.
7. Sair da TUI: os originais voltam (`gt ue player`), e a `.text` não tem alteração (`gt diff`).
8. O log da TUI não tem "novo original" se repetindo, nem `o jogo não deixa`.
9. O dono confirma que o jogo ficou estável e que o save carrega normal depois.

## 10. Documentos a atualizar

- `docs/RESEARCH.md`: caminho, campos e marcas da pesquisa (6.1) e o resultado do teste no jogo.
- `docs/ARCHITECTURE.md`: 5.3 (`set` com `root = "game_state"` e com `kind = "bool"`), 5.6 (leitura do
  `game_state` no ciclo da manutenção, marca de espera própria) e registro de decisões.
- `docs/specs/006-progressao-e-movimento.md`: nota em 6.3 de que a spec 007 libera `set` na raiz `game_state`, e em
  6.4 de que libera `set` com `bool`.
- `docs/specs/005-sobrevivencia-por-reflexao.md`: nota em 6.3 de que a procura por pawn nulo não descarta mais o
  controller guardado (spec 007, 6.3).
- `docs/ROADMAP.md`: a spec 007 como complemento da Fase 5; se a defesa e a construção funcionarem, saem da lista da
  Fase 6 ("multiplicador de defesa", "construção sem requisitos").

## 11. Decisões tomadas

| Decisão | Quem |
|---|---|
| Construir sem ingredientes só com o que já foi desbloqueado; sem `bUnlockAllRecipes` | Dono |
| Sem ingredientes e construção pronta na hora num cheat só | Dono |
| Caminho de valor pelas configurações do modo, aceitando o risco de save | Dono |
| Defesa nesta spec pelo `EnemyDamageMultiplier`, como tentativa; se não funcionar, volta para a Fase 6 | Dono |
| Code cave, Brain Power e hora do dia na fase seguinte | Dono |
| `set` liberado na raiz `game_state`, com manutenção e isolamento por raiz | IA |
| `set` com `bool` de byte inteiro, valor por campo, só liga/desliga | IA |
| Cheat de construção em ITENS, dano recebido em SOBREVIVÊNCIA | IA |
| Nome "Dano recebido" (quanto do dano chega) em vez de "Defesa" | IA |
| Grade do dano recebido: 0 a 1, passo 0,05, padrão 0,25 | IA |
| Multijogador fora do teste | IA |
| Procura do controller por pawn nulo (30 s) sem descartar o controller guardado; troca só se achar outro | IA |
| Risco aceito: até a procura terminar, os cheats do `game_state` podem escrever no mundo que saiu | IA |
| `bool` de byte inteiro por função nova, sem mudar o `gt-ue` nem o `bool_field` do `clear_bits` | IA |
| Caminho do `set` generalizado para 1 ou 4 bytes, com valor desejado por campo | IA |
| `gt ue player` mostra o `game_state` mesmo com o pawn falhando | IA |

## 12. Deixado para quem implementa

- O formato no TOML do valor por campo dos `bool`.
- O texto exato da marca de espera do `game_state`, das descrições e do registro de eventos.
- Como o `gt-testtarget` monta o `SurvivalModeManagerComponent` e as configurações sintéticas.
- Como a leitura do `game_state` entra no ciclo (junto do snapshot do pawn ou em separado), desde que o isolamento
  por raiz valha.
