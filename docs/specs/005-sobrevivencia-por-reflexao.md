# Spec 005 — Sobrevivência por valores (reflexão do Unreal)

| Campo | Valor |
|---|---|
| Fase | 5, primeira parte (ver `docs/ROADMAP.md`) |
| Estado | Aprovada pelo dono em 2026-10-26 (passagem para o plano 005) |
| Data | 2026-10-26 |

## 1. Intenção

Ter os cheats de sobrevivência que o FLiNG não consegue mais fazer neste build: vida, stamina, comida, água,
oxigênio, temperatura, sem dano de queda e uma tentativa de "não morrer". Eles são feitos **por escrita de valor**
nos objetos do personagem do jogador, e não por patch de código.

O que esta fase traz de novo é **achar os campos pelo nome**. O trainer lê a reflexão do Unreal com o jogo
rodando (`GUObjectArray`, `FNamePool`, `UStruct`/`FProperty`), acha as classes e os campos pelo nome e usa os
offsets que o próprio jogo informa. Uma atualização que mude o layout das classes não deve quebrar nenhum cheat
desta spec. O único conhecimento fixo é o layout das estruturas de reflexão do motor, que só muda com a versão do
Unreal, e dois padrões AOB que levam às globais. Os padrões podem quebrar quando o compilador reorganiza o código, como
aconteceu com os do FLiNG. Por isso a spec aceita padrões com muitas ocorrências, desde que todas concordem (6.2),
porque resistem melhor. A concordância das 281 ocorrências conhecidas ainda é conferida na pesquisa (P1). O arquivo de versão só confere o endereço achado pelo padrão e não serve de reserva.

O controller do jogador, o pawn, os componentes e todos os offsets estão em `RESEARCH.md` (sessões de 2026-10-26,
"o pawn do jogador e as tabelas do Unreal" e "catálogo de todos os cheats do FLiNG"). A implementação é nossa. O
FLiNG serviu só para saber **quais** campos interessam (`AGENTS.md` 3).

Esta é a primeira parte da Fase 5. Carga, dano causado, ciência e movimento ficam para a spec 006, que reaproveita
tudo o que esta spec constrói (decisão do dono).

## 2. Objetivos

1. **Leitor de reflexão** no crate `gt-ue` (hoje vazio). Ele acha `GUObjectArray` e `FNamePool` por AOB, lê nomes,
   percorre objetos, acha uma classe pelo nome e acha um campo pelo nome (com o offset e, para `bool`, a máscara),
   subindo pela `SuperStruct`. É somente leitura e genérico para UE5: não sabe nada do Grounded 2.
2. **Caminho até o personagem**, no `gt-grounded2`: controller do jogador → `Controller.Pawn` → componentes do
   `SurvivalCharacter`. É revalidado a cada leitura e não faz varredura grande de memória (6.3).
3. **Oito cheats** na categoria **Sobrevivência** da TUI (6.5):
   - travar no máximo enquanto ligados: **Vida infinita**, **Stamina infinita**, **Comida infinita**, **Água
     infinita**, **Oxigênio infinito** e **Temperatura estável** (travada no valor neutro);
   - bits que voltam ao original ao desligar: **Sem dano de queda** e **Não morrer**.
4. **Escrita em dados** pelo motor (6.6): os bits passam pelo diário e são restaurados na saída e na recuperação
   depois de um SIGKILL. As barras são reescritas a cerca de 10 Hz e não vão para o diário.
5. **Integração na TUI** sem mudar o uso: os itens novos ligam e desligam como os da Fase 4, religam depois de o
   jogo reiniciar e mostram um aviso quando o personagem não está disponível (6.7).
6. **Comandos de leitura no `gt`** (só leitura), para pesquisa e para conferir o jogo depois de atualizações:
   `gt ue class <Classe>` (campos com offset e tipo) e `gt ue player` (estado atual do personagem) (6.8).
7. Testes sem o jogo, contra uma reflexão sintética montada no `gt-testtarget`, e testes `#[ignore]` com o jogo
   aberto (seção 8).

## 3. Fora do objetivo

- Carga, dano causado, ciência, velocidade, pulos, gravidade e teleporte (spec 006, ainda na Fase 5).
- Velocidade do jogo (speedhack), em qualquer fase próxima.
- Valor ajustável nestes cheats: todos são liga/desliga. Também não entram vida ou stamina máximas editáveis.
- Painel com os números do personagem na TUI (decisão do dono). O `gt ue player` cobre a conferência.
- God mode por desvio de código e multiplicador de defesa (Fase 6).
- Configurações do modo de jogo (`SurvivalGameModeSettings`), que podem ir para o save.
- Gerar SDK com o Dumper-7 ou manter arquivo de offsets de campos por versão.
- Multiplayer: o caminho até o personagem considera só o jogador local de uma partida solo.

## 4. Anti-objetivos (falha mesmo que "funcione")

- Escrever num endereço sem ter conferido, imediatamente antes, que ele ainda pertence ao objeto esperado do
  personagem do jogador (6.4). Ponteiro velho, objeto destruído ou classe diferente: não escreve.
- Escrever fora do campo resolvido pela reflexão: cada escrita tem o tamanho do campo e fica dentro do objeto.
- Escrever valores fora da faixa normal do jogo. O FLiNG escreve `999999`, e nós escrevemos o máximo do próprio jogo.
  Um valor absurdo poderia acabar no save.
- Varrer a memória do jogo inteira, ou ler em blocos grandes sem pausa (o jogo já fechou durante uma varredura
  grande; `RESEARCH.md`, 2026-10-26).
- Afetar outros personagens. Inimigos e criaturas têm os mesmos componentes, e só os do pawn do jogador são tocados.
- Patch de código, code cave, ou qualquer escrita na `.text` para estes cheats.
- Pausar o jogo (SIGSTOP) a cada escrita de valor.
- Offsets de campo fixos no código ou nas assinaturas: offsets de campo só vêm da reflexão.
- Qualquer alteração em arquivo de save ou do jogo em disco.

## 5. Onde cada coisa mora

| O quê | Onde |
|---|---|
| Leitor de reflexão (nomes, objetos, classes, campos), genérico UE5 | `crates/gt-ue/src/` |
| Leitura em lote (vários `iovec` por chamada) | `crates/gt-core/src/mem/` |
| Layout das estruturas de reflexão e padrões AOB das globais | `[ue]` em `crates/gt-grounded2/signatures/generic.toml`, com `rva` verificado em `c935e799-0b27c000.toml` (6.2) |
| Definição dos cheats de valor (campo, alvo, tipo) | `[[field]]` em `generic.toml` (6.5) |
| Caminho até o personagem e aplicação dos cheats de valor | `crates/gt-grounded2/src/` (módulo novo) |
| Escrita em dados com diário e recuperação | `crates/gt-core/src/` (motor, diário e recuperação; feature `write`) |
| Integração na TUI | `apps/gt-tui/src/` (backend do jogo `game.rs`, cru `raw.rs` e falso `fake.rs`, controlador e tela) |
| Comandos `gt ue …` | `apps/gt-cli/` (continua sem a feature `write`) |
| Reflexão sintética para testes | `crates/gt-testkit/` (`gt-testtarget`, `gt-patch-harness` estendido para os testes de sinal com bit limpo, e testes); a escrita dos ponteiros no próprio processo usa ajudantes novos em `crates/gt-core/src/mem/test_support.rs`, único lugar com `unsafe` |
| Achados da pesquisa e do teste no jogo | `docs/RESEARCH.md` |

A regra de dependência continua valendo: `gt-ue` depende só do `gt-core` (leitura), e o `gt-grounded2` depende do
`gt-ue`. As dependências novas são `gt-ue → gt-core`, `gt-grounded2 → gt-ue` e, para o `gt ue`, `gt-cli → gt-ue`. O
`gt-tui` chega ao `gt-ue` pelo `gt-grounded2`.

## 6. Comportamento

Os dados de exemplo são do build `c935e799-0b27c000` (`Augusta-CL-3207493`), lidos em 2026-10-26.

### 6.1 Pesquisa antes de escrever (só leitura)

O plano tem passos **só de leitura**, com o jogo aberto, o FLiNG fechado e o dono num save, sempre em blocos
pequenos e com pausas (6.3). A ordem é esta:

1. **P1**, antes do leitor de reflexão, com `gt scan`/`gt disasm`/`gt read` e um script somente leitura em
   `scratch/`. O script decodifica o `disp32` de cada ocorrência, confere a concordância e procura referências
   RIP-relativas ao `FNamePool`. Ele lê a `.text` em blocos de 1 MB com pausas e é descartável (`AGENTS.md` 2).
2. Os padrões achados são registrados no `RESEARCH.md`. Eles entram no `[ue]` junto com a mudança do carregador de
   assinaturas, que hoje recusa campos desconhecidos.
3. Implementação do leitor de reflexão e do `gt ue`. Para a pesquisa, o `gt ue` também aceita os endereços das
   globais por argumento (`--gobjects`, `--names`), o que pula a busca de padrões.
4. **P2 a P8** com o `gt ue`.

O passo resolve o que falta para definir os alvos:

| # | Pergunta | Por que importa |
|---|---|---|
| P1 | Padrões AOB que levem a `GUObjectArray` (`+a53bc30`) e ao `FNamePool` (`+a499e50`) pela regra de 6.2 (único, ou várias ocorrências que resolvem todas para o mesmo endereço). O padrão de acesso conhecido tem 281 ocorrências; a concordância delas ainda é conferida aqui. O `FNamePool` ainda não tem padrão de código: procurar as instruções que referenciam `+a499e50` só na `.text` (busca pelo deslocamento RIP-relativo), nunca no heap | Sem as globais, nenhum cheat de valor funciona |
| P2 | Qual campo ou valor é o máximo de comida, água e oxigênio (`SurvivalComponent` e superclasses), e uma faixa sã para cada um com folga para upgrades | Alvo da trava sem valor absurdo; faixa que não vire `alvo inválido` à toa |
| P3 | Por que a stamina leu `120/100`, e qual é o máximo efetivo (o campo `MaxStamina`, outro campo ou um modificador) | Travar em `MaxStamina` poderia **baixar** a stamina |
| P4 | Qual valor de `CurrentBodyTemperature` é neutro (o FLiNG usa `0`), e a faixa observada no frio e no calor (base da faixa sã) | Alvo da temperatura estável |
| P5 | Máscara e byte de `bTakeFallDamage` (`FBoolProperty`: `ByteOffset`, `FieldMask`), e as máscaras de `bCanDieFromDamage`/`bCanDieEver` (já lidas: `0x02`/`0x04` em `+0x11d`). Confirmar contra o `+0x11d` se o byte é `Offset_Internal` ou `Offset_Internal + ByteOffset` | Bits certos |
| P6 | Se o jogo volta a ligar esses bits sozinho (ao carregar, renascer ou trocar de equipamento) | Define se a manutenção de 6.5.2 é necessária |
| P7 | O que acontece com `Controller.Pawn`, `Character` e `AcknowledgedPawn` ao morrer, renascer, ir ao menu, carregar outro save e **montar** (se o jogo tiver montaria acessível no save de teste) | Valida 6.3 e 6.7, inclusive a hierarquia de classe da montaria. Se, ao montar, `Pawn` passar a ser a montaria, vale a regra de 6.3 (o objeto escrito tem de ser `is_a(SurvivalPlayerCharacter)`) |
| P8 | Se os nomes de classe usados (`SurvivalPlayerController`, `SurvivalCharacter`, `HealthComponent`, `StaminaComponent`, `SurvivalComponent`, `BodyTemperatureComponent`) são únicos entre os `UClass`, e qual é o pacote deles (`/Script/Maine`). Também a classe C++ do pawn do jogador (esperado `SurvivalPlayerCharacter`, pai de `BP_SurvivalPlayerCharacter_Gellarde_C`), o **nome do tipo** dos campos de ponteiro (`ObjectProperty` ou `ObjectPtrProperty`) e **o nome de cada campo de componente no `SurvivalCharacter`**: o `RESEARCH.md` lista `Health`, `Stamina`, `BodyTemperature` e `Survival`, sem o sufixo `Component` | `find_class` usa nome + pacote; o caminho de cada `[[field]]` usa o nome do campo |
| P9 | Se o jogo consulta `bTakeFallDamage` no cálculo de dano de queda. É resolvido **antes do código**, por pesquisa estática: acessos ao byte do campo na `.text` (`gt scan` pelo deslocamento) e desmontagem do uso | Se não consultar, "Sem dano de queda" sai desta spec antes do código, e a alternativa (`FallDamageMultiplier` das configurações de modo, com risco de save) volta ao dono. Se a pesquisa for inconclusiva, o cheat é implementado, e se falhar no critério de pronto é removido no fechamento (definição, testes dele e linha da TUI), com registro no `RESEARCH.md` |

**Se P1 não achar um padrão aceitável** para alguma das globais, o trabalho para e volta ao dono antes de seguir.
As alternativas, como só `rva` por versão ou procurar o ponteiro na `.data`, mudam a promessa de sobreviver a
atualizações. Por isso a decisão é dele.

O resultado vai para `RESEARCH.md`. Se alguma resposta mudar **o que** um cheat faz (por exemplo, a temperatura não
ter um valor neutro único), a mudança volta para esta spec antes do código dos cheats. Respostas que só preenchem um
alvo (qual campo é o máximo) vão direto para o `generic.toml`.

### 6.2 Leitor de reflexão (`gt-ue`)

**Entradas:** um leitor de memória (uma interface de leitura, com o `MemReader` como implementação real e um buffer
sintético nos testes puros), a base do módulo, os endereços resolvidos de `GUObjectArray` e do
`FNamePool` e o layout (tabela abaixo). Saídas tipadas, com erro claro quando algo não bate.

**Layout configurável, não fixo no código.** Os offsets das estruturas do motor ficam no bloco `[ue]` do
`generic.toml` e podem ser sobrescritos por versão, como os patches. Valores do build atual (`RESEARCH.md`):

| Estrutura | Campos |
|---|---|
| `GUObjectArray` | lista de blocos `+0x0`, `MaxElements +0x10`, `NumElements +0x14`, `MaxChunks +0x18`, `NumChunks +0x1c`; 65.536 itens por bloco, item de 24 bytes, objeto em `+0x8` do item |
| `UObject` | `ObjectFlags +0x8` (`0x10` = CDO), `InternalIndex +0xC`, `ClassPrivate +0x10`, `NamePrivate +0x18`, `OuterPrivate +0x20` |
| `FNamePool` | blocos a partir da global; entrada com cabeçalho `u16` (tamanho `>> 6`, bit 0 = UTF-16); índice → bloco `idx >> 16`, deslocamento `(idx & 0xffff) * 2` |
| `UStruct` | `SuperStruct +0x40`, `ChildProperties +0x50`, `PropertiesSize +0x58` |
| `FField` | `ClassPrivate +0x08` (nome do tipo em `+0` da `FFieldClass`), `Next +0x18`, `NamePrivate +0x20` |
| `FProperty` | `ElementSize +0x34`, `Offset_Internal +0x44` |
| `FBoolProperty` | `FieldSize +0x70`, `ByteOffset +0x71`, `ByteMask +0x72`, `FieldMask +0x73` |

Três valores da tabela ainda não foram conferidos no jogo e são conferidos na pesquisa (6.1): `OuterPrivate +0x20`
(suposição comum de UE5) e `FBoolProperty.FieldSize +0x70`/`ByteOffset +0x71` (inferidos a partir de `ByteMask +0x72`
e `FieldMask +0x73`, que já foram lidos).

**Globais por padrão AOB.** O `[ue]` também traz os dois padrões. Cada um tem:
- o `offset` do deslocamento RIP-relativo dentro da ocorrência;
- o tamanho da instrução até o fim, para calcular `endereço = fim da instrução + disp32`;
- um `adjust` opcional, somado ao endereço resolvido. Ele serve para quando o código referencia o início da estrutura
  e o leitor precisa de um campo dela, por exemplo os blocos do `FNamePool`. O `rva` da versão é comparado **depois**
  do `adjust`, ou seja, com o endereço que o leitor usa (`+a53bc30` e `+a499e50` no build atual).

- **Regra de unicidade para leitura de globais** (exceção ao `AGENTS.md` 4, decisão do dono): um padrão de global é
  aceito se tiver uma ocorrência, **ou várias que resolvem todas para o mesmo endereço**. Ocorrências que resolvem
  para endereços diferentes deixam a global `ambíguo`, e 0 ocorrências deixam `não encontrado` (mesmos textos dos patches). O endereço resolvido
  tem de cair na `.data` (ou outra seção de dados) do módulo. A exceção vale **só** para achar globais que vão ser
  lidas. Patches de código continuam exigindo padrão único.
- A busca é a mesma `.text` em memória e em disco dos patches, e segue a mesma regra de quando o disco é lido
  (`catalog`). Quando os dois são lidos, o endereço resolvido tem de ser o mesmo nos dois. Bytes ilegíveis seguem o
  veredito da busca dos patches (`memscan`): as ocorrências legíveis e concordantes valem, e a global só fica
  `inconclusivo` quando nenhuma ocorrência legível foi achada e houve trecho ilegível.
- O `<chave>.toml` traz, por global, o `rva` **da global resolvida** (não da instrução). Com arquivo de versão, o
  endereço resolvido pelo padrão tem de ser igual a `base + rva` ("verificado"). Se for diferente, fica "fora do
  lugar" e indisponível. Sem arquivo de versão, ou com arquivo sem `rva` daquela global, fica "não verificado" (`?`), como os patches.
- A resolução (achar as ocorrências, decodificar o `disp32`, conferir a concordância) é uma função pura, testada
  sobre buffers.

**Conferência do layout**, em duas partes.

Parte barata, feita na busca de padrões. Sem ela, todos os cheats de valor ficam `indisponível (reflexão não
reconhecida)`:
- o nome de índice 0 é `None`;
- `0 < NumElements ≤ MaxElements`, `NumChunks ≤ MaxChunks` e `NumChunks × 65.536 ≥ NumElements`. No jogo foram lidos
  `NumChunks = 10` para ~506 mil elementos, e o jogo pode alocar blocos adiantados, por isso a regra não é igualdade;
- em uma amostra de itens (os primeiros e alguns espalhados), `item[o.InternalIndex].Object == o`. Isso confirma o
  objeto em `+0x8` do item e o `InternalIndex`;

Parte feita pela tarefa de reflexão (6.3), no índice. Sem ela, todos ficam `indisponível (reflexão incoerente)`:
- as classes `/Script/CoreUObject.Object`, `/Script/CoreUObject.Class` e `/Script/Engine.Actor` são achadas;
- `Actor` tem o campo `RootComponent` de um tipo da família de propriedades de objeto.

**Família de propriedades de objeto:** `ObjectProperty` e `ObjectPtrProperty`. No UE5, campos `TObjectPtr` podem
aparecer como `ObjectPtrProperty`. O nome real é conferido em P8. Todo lugar desta spec que espera um ponteiro para
objeto aceita a família.

**Operações:**
- `name(FName)`: lê e guarda em cache (os nomes não mudam com o jogo rodando).
- A âncora é a própria `Class`: o objeto `/Script/CoreUObject.Class` cuja `ClassPrivate` aponta para ele mesmo.
- `find_class(pacote, nome)`: acha o objeto cujo nome é `nome`, cujo `Outer` é o pacote (ex.: `/Script/Maine`) e
  cuja classe é `Class` **ou filha dela**, o que inclui as classes de Blueprint (`BlueprintGeneratedClass`). Se
  houver 0 ou 2+, a classe é indisponível. A busca percorre o `GUObjectArray` (6.3). O
  resultado **positivo** fica em cache por conexão. Não há cache de resultado negativo: uma classe não achada (jogo
  ainda carregando) é procurada de novo na próxima busca.
- `find_field(classe, nome)`: percorre `ChildProperties` e sobe pela `SuperStruct`. Devolve offset, tamanho, tipo
  (`FloatProperty`, `BoolProperty`, `ObjectProperty`, …) e, para `bool`, o byte e a máscara. Campo não achado, ou
  tipo diferente do esperado: indisponível.
- `is_a(objeto, classe)`: sobe pela `SuperStruct` da classe do objeto.
- `instances_of(classe)`: instâncias (sem CDO) de uma classe ou de suas filhas.

Toda leitura passa por limites: endereço canônico de usuário, tamanho máximo por leitura, laços com teto (cadeia de
`SuperStruct` ≤ 64, lista de campos ≤ 4096, nome ≤ 1024 bytes). Uma estrutura corrompida dá erro, não laço
infinito.

### 6.3 Caminho até o personagem e cuidado com a memória

**Achar o controller** (caro, feito raramente): procurar em `instances_of(SurvivalPlayerController)` a instância
única. A classe é a C++ do módulo `Maine`, e o Blueprint `BP_SurvivalPlayerController_Augusta_C` é filha dela. Se
houver 0 instâncias, o personagem está indisponível. Se houver 2 ou mais (por exemplo, o controller antigo ainda
não coletado depois de carregar outro save), vale a única com `Pawn` não nulo. Se ainda sobrar mais de uma, o
resultado é ambíguo e nada é escrito.
A varredura:
- lê a tabela de itens em blocos de no máximo 1 MB, com ~1 ms de pausa entre blocos;
- lê só `ClassPrivate`/`ObjectFlags` de cada objeto, em lotes, com pausa entre lotes. Isso exige uma leitura em lote
  nova no `gt-core` (`process_vm_readv` com vários `iovec`, até o limite do kernel), porque o `MemReader` de hoje
  lê uma região por chamada;
- percorre até o `NumElements` lido no início, e tem teto de tempo de 60 s. Passou do teto, é abandonada como
  inconclusiva, e a próxima tentativa segue o intervalo normal.

O custo de uma varredura completa é medido no começo da pesquisa (6.1) e registrado no `RESEARCH.md`.

> **Nota (spec 006, 6.3 e 6.6):** a procura do controller ganha um gatilho novo: uma ação (Somar ciência) recusada
> por controller ausente ou inválido também a pede, dentro do mesmo intervalo da tarefa, e a linha `personagem:`
> passa a seguir o controller guardado depois dessa procura. O isolamento por cheat (o tratamento `missing`, antes só
> do pet) vale agora para **todo** componente depois do pawn: um componente nulo ou de classe errada deixa em espera
> só os cheats que dependem dele.

**Quando a varredura roda.** A busca de padrões da conexão **não** varre objetos. Ela só resolve as duas globais e faz
as conferências baratas do layout (nome 0, contagens, amostra de itens). Assim a conexão e os cheats de patch não
ficam mais lentos do que hoje. Toda varredura de objetos acontece numa **tarefa de fundo própria** (a "tarefa de
reflexão"), separada da busca de padrões e com dois trabalhos:

1. **Índice da reflexão**, uma vez por conexão, logo que as globais estão resolvidas. Uma passada pelo
   `GUObjectArray` acha a `Class`, as classes do catálogo (`find_class`) e os campos (`find_field`). Com isso se
   decide a disponibilidade dos cheats de valor (6.7). O resultado positivo fica guardado na sessão até a conexão
   mudar. Se faltar classe ou campo, ou se a passada estourar o teto, ela é repetida no intervalo normal da tarefa, por
   até 120 s depois da conexão (jogo ainda carregando). O `r` reinicia esse prazo.
2. **Procura do controller**, só quando há pelo menos um cheat de valor ligado ou desejado e o controller guardado não
   é válido. Também roda quando o controller guardado continua válido, mas com `Pawn` nulo há mais de 30 s. Esse é o
   caso de um controller antigo que ainda não foi coletado depois de carregar outro save. Com 0 instâncias (menu, carregamento), devolve "personagem ausente" e
   tenta de novo depois do intervalo. Com todos os cheats de valor desligados, ninguém varre a memória, e a linha
   `personagem:` mostra `—`.

**Intervalo da tarefa:** `max(10 s, 2 × duração da última varredura)` entre duas varreduras, como nas buscas da spec
003. A tarefa roda um trabalho de cada vez e devolve o resultado ao laço principal por canal, como a busca. O resultado é
descartado se a conexão mudou (outro processo ou reconexão). Na saída, a tarefa é abandonada como a busca: o laço
não espera por ela.

**A cada leitura** (barato, poucos ponteiros), o caminho é revalidado:
1. O controller guardado ainda é válido: o item do `GUObjectArray` no `InternalIndex` dele ainda aponta para ele, e
   a `ClassPrivate` é a mesma de quando foi achado.
2. `pawn = [controller + Controller.Pawn]`. Se for nulo, o personagem está ausente (menu, carregamento, morte).
3. O pawn é `is_a(SurvivalPlayerCharacter)` (classe do personagem do jogador, P8), e não só `SurvivalCharacter`. Assim
   uma montaria ou criatura que fique no `Pawn` nunca é escrita. Se P7 mostrar que, montado, outro campo do controller
   aponta para o personagem, o caminho pode usá-lo, mas o objeto também passa nessa mesma conferência. O resultado fica em cache por endereço de pawn e classe.
4. Cada componente é lido do pawn pelo campo `ObjectProperty` do `SurvivalCharacter` (nomes confirmados em P8) e
   conferido com `is_a` da classe do componente (`HealthComponent`, `StaminaComponent`, `SurvivalComponent`,
   `BodyTemperatureComponent`), com o mesmo cache.

Os offsets de `Controller.Pawn` e dos campos de componente vêm de `find_field`, nunca dos valores de
`RESEARCH.md`. Aqueles valores servem só para conferir os testes no jogo.

### 6.4 Conferência antes de cada escrita

Toda escrita de valor é precedida, no mesmo ciclo, da revalidação de 6.3 (passos 1 a 4). Além disso:
- o endereço é `objeto + offset do campo`, e `offset + tamanho ≤ PropertiesSize` da classe do objeto;
- o endereço está numa região `rw` do `maps`, fora das imagens de módulo. O `maps` é relido quando o endereço do
  objeto muda e quando uma escrita falha, e não a cada ciclo (ele tem milhares de linhas com o Wine);
- a escrita tem o tamanho exato do campo (4 bytes para `float`, 1 byte para o byte do `bool`);
- só objetos do caminho do jogador são tocados. Outro personagem com os mesmos componentes (criaturas, um segundo
  `SurvivalCharacter`) nunca é escrito, porque o endereço sai sempre do `Controller.Pawn` revalidado.

Se qualquer conferência falhar, nada é escrito naquele ciclo, e o cheat mostra o motivo (6.7).

### 6.5 Os cheats

Todos ficam na categoria `sobrevivencia`, nesta ordem, e começam `OFF`. Nenhum tem valor ajustável (o bloco
`[patch.value]` da spec 003 não se aplica). *Nota (spec 006, 6.5): a spec 006 traz cheats de valor com valor ajustável, pelo bloco `[field.value]` do `[[field]]`.* Cada um é um bloco **`[[field]]`** no `generic.toml`. O nome é outro para
não confundir com `[patch.value]`. Cada bloco tem:
- `id`, `name`, `description` e `category`;
- o caminho (componente e campo);
- a regra (trava ou bits);
- nas travas, a fonte do alvo (um campo do mesmo objeto, como `MaxStamina`, ou uma constante) e a faixa sã.

O `id` é único entre `[[patch]]` e `[[field]]`. Numa mesma categoria, a TUI mostra os `[[patch]]` primeiro e depois
os `[[field]]`, cada grupo na ordem do arquivo. Os campos citados nas tabelas são nomes da reflexão.

#### 6.5.1 Travas (enquanto ligado, reescreve a ~10 Hz)

| `id` | Nome | Campo escrito | Alvo |
|---|---|---|---|
| `vida-infinita` | Vida infinita | `HealthComponent.CurrentDamage` (`float`) | `0` (vida = `MaxHealth − CurrentDamage`). Um golpe maior que a vida inteira entre duas reescritas ainda mata; o dono aceitou esse risco, e para golpes fatais há o "Não morrer" |
| `stamina-infinita` | Stamina infinita | `StaminaComponent.CurrentStamina` | o máximo efetivo (P3) |
| `comida-infinita` | Comida infinita | `SurvivalComponent.CurrentFood` | o máximo (P2) |
| `agua-infinita` | Água infinita | `SurvivalComponent.CurrentWater` | o máximo (P2) |
| `oxigenio-infinito` | Oxigênio infinito | `SurvivalComponent.CurrentBreath` | o máximo (P2) |
| `temperatura-estavel` | Temperatura estável | `BodyTemperatureComponent.CurrentBodyTemperature` | o valor neutro (P4) |

Regras:
- **O alvo é relido do personagem a cada ciclo** quando vem de um campo (ex.: `MaxStamina`), para acompanhar
  upgrades e efeitos. O alvo só é uma constante do `generic.toml` quando a pesquisa mostrar que não há campo; nesse
  caso a constante fica registrada com a origem em `RESEARCH.md`.
- **Nunca baixa um valor de barra** que já está acima do alvo (ex.: stamina 120 com alvo 100). Barra: só escreve
  quando o valor atual é menor que o alvo. Na vida, só escreve quando `CurrentDamage > 0`. A temperatura é a exceção
  e é escrita sempre que for diferente do neutro.
- Só escreve se o valor mudou, para não gerar tráfego à toa.
- O alvo tem de ser finito e estar dentro de uma faixa sã, definida por cheat no `generic.toml` (ex.: `0..=10000`).
  Com o alvo fora dela, nada é escrito. O cheat continua `[ON ]`, com a marca `alvo inválido` (como
  `aguardando o personagem`), e uma linha no registro. Ele retoma sozinho quando o alvo volta à faixa, por exemplo
  com um pawn novo. Não usa o estado `erro` da spec 003, que significa restos no jogo.
- **Ao desligar**, para de escrever. A barra fica onde está e volta a cair normalmente (decisão do dono). Nada vai ao
  diário, porque não há o que desfazer.

#### 6.5.2 Bits (originais restaurados ao desligar)

| `id` | Nome | Campo | Ligado |
|---|---|---|---|
| `sem-dano-queda` | Sem dano de queda | `SurvivalCharacter.bTakeFallDamage` | bit limpo |
| `nao-morrer` | Não morrer | `HealthComponent.bCanDieFromDamage` e `bCanDieEver` | os dois bits limpos |

- A máscara é a `FieldMask` da `FBoolProperty`, aplicada no byte `Offset_Internal + ByteOffset`, conforme P5. Num
  `bool` nativo (não bitfield), a `FieldMask` é `0xFF`, e "limpar" grava `0` no byte inteiro, que é o próprio campo.
- Ao ligar: lê o byte, guarda o original **no diário antes de escrever** (6.6), escreve o byte com os bits limpos e
  relê. Os outros bits do mesmo byte são preservados.
- Manutenção: no mesmo ciclo de ~10 Hz, se o jogo voltar a ligar o bit, ele é limpo de novo (P6). O original
  guardado continua sendo o do primeiro registro daquele objeto.
- **Uma entrada por cheat e byte** (chave: `id` + endereço do byte). Há no máximo uma entrada viva por `id` e byte. Se
  P5 mostrar que `bCanDieFromDamage` e `bCanDieEver` ficam em bytes diferentes, "Não morrer" tem duas entradas, uma
  por byte, sempre do mesmo objeto.
- **Objeto novo** (outro pawn depois de morrer, carregar ou montar): antes de tocar o objeto novo, o trainer resolve a
  entrada do antigo na hora. Se o antigo ainda passa na conferência de identidade (um pawn morto ainda não coletado,
  por exemplo), os bits originais dele são restaurados. Se não passa, a entrada é descartada sem escrever. Depois
  disso o objeto novo ganha a entrada dele. Assim o diário nunca acumula entradas de objetos antigos.
- Ao desligar, na saída da TUI e na recuperação: restaura o bit original de cada entrada cujo objeto ainda passa na
  conferência. Os outros bits do byte não são tocados.
- **"Não morrer" é uma tentativa.** O esperado é você continuar levando o golpe, com a vida caindo, mas sem morrer.
  O que acontece com a vida em zero (ficar em 0, ficar negativa, algum estado estranho) é observado no teste. Se não
  funcionar, ou deixar o personagem num estado ruim, o cheat sai desta spec e o god mode fica todo para a Fase 6.
  "Não morrer" e "Vida infinita" são independentes e podem ser ligados juntos (decisão do dono).

### 6.6 Escrita em dados pelo motor (`gt-core`, feature `write`)

O motor da Fase 2 só escreve nas seções executáveis do módulo (`ARCHITECTURE.md` 5.4). Esta spec acrescenta um
segundo tipo de escrita, **em dados**, com regras próprias:

- **Sem pausa.** É uma escrita alinhada de 1 ou 4 bytes num campo de dados. Não há fronteira de instrução a
  proteger, e pausar a 10 Hz pesaria no jogo. A corrida possível (o jogo gravar o mesmo byte no mesmo instante) só
  afeta o byte dos bits. Ela é aceita e coberta pela releitura e pela manutenção.
- **Mesma identidade do processo** (PID + horário de início, descritor do `mem` aberto com a identidade conferida) e
  mesma trava do diário que os patches.
- **Bits vão para o diário.** É uma entrada nova, que convive com as de patch, com: `id` do cheat, endereço do byte,
  byte original, byte escrito, máscara, e uma **conferência de identidade do objeto**, que é uma lista de
  `(endereço, bytes esperados)`: vtable, `InternalIndex` e `ClassPrivate` do objeto dono do byte. O `InternalIndex`
  reduz o risco de um objeto novo da mesma classe no mesmo endereço ser tomado pelo antigo. O `gt-core` continua
  genérico: confere bytes, sem saber o que é um `UObject`.
- **Restaurar** uma entrada (ao desligar, sair ou recuperar) exige que a conferência de identidade bata, que o
  endereço esteja em região `rw` do `maps` atual e, nos bits da máscara, que o byte atual seja o nosso. Com isso, só
  os bits da máscara voltam ao original. Se a conferência falhar (objeto destruído, endereço reaproveitado), a
  entrada é descartada sem escrever e registrada no log. Se os bits da máscara já forem os originais, a entrada só é
  esquecida.
- **Barras não passam pelo diário**: não há original a restaurar. Elas usam o mesmo descritor e as mesmas
  conferências de 6.4.
- **Recuperação** (`recovery`): as entradas de dados seguem as regras acima, e **não** a política de memória
  executável. Hoje a recuperação trata como "de outro" e tira do diário qualquer entrada fora de memória executável.
  Isso muda para as entradas de dados e é uma tarefa própria do plano. A recuperação de entradas de patch não muda.
- **Diários antigos** (só com entradas de patch) continuam legíveis.
- **Estados do motor:** a manutenção de ~10 Hz (travas e bits) fica **suspensa** enquanto o motor está `stuck`, quando
  não há motor (diário não resolvido, disco ilegível) e, para um `id`, enquanto ele tem resto pendente
  (`pending_ids`). Com resto pendente, nenhuma entrada nova é criada para aquele `id` até a recuperação (`r`)
  resolver o resto.
- **`gt-patch`:** `list` e `toggle` continuam só com os `[[patch]]` (um `id` de `[[field]]` no `toggle` é recusado
  com mensagem clara). O `restore` e a recuperação no início de cada comando tratam também as entradas de dados.

### 6.7 Na TUI

- Os oito aparecem em **SOBREVIVÊNCIA**, na ordem de 6.5, com as mesmas teclas e estados da spec 003 (`[ON ]`,
  `[OFF]`, `indisponível (<motivo>)`, `erro`).
- **Disponibilidade** em duas etapas:
  - a busca de padrões resolve as globais e confere o layout barato;
  - a tarefa de reflexão (6.3) monta o índice e confere as classes e os campos do caminho de cada cheat, com o tipo
    esperado.

  Enquanto o índice não chega, os cheats de valor mostram `verificando…` e ligá-los é recusado com essa mensagem. Os
  cheats de patch não esperam por isso. Motivos de `indisponível (<motivo>)`:
  - das globais, com os mesmos textos e constantes dos patches (`não encontrado`, `ambíguo`, `inconclusivo`, `fora do
    lugar`), para que o `keeps_state` e a nova busca casem sem mudança de texto;
  - da busca: `reflexão não reconhecida` (parte barata do layout, 6.2);
  - da tarefa de reflexão: `reflexão incoerente`, `classe X não encontrada`, `classe X ambígua`, `campo X não
    encontrado`, `reflexão inconclusiva` (teto de 60 s estourado).
- **`?` (não verificado):** os cheats de valor seguem as globais. Com arquivo de versão e globais verificadas, aparecem
  sem `?`. Numa versão sem arquivo, aparecem com `?` e o aviso de não verificado da spec 003 (uma vez por cheat por
  conexão), como os patches. Os campos vêm da reflexão e não mudam essa marca.
- **Nova tentativa automática:**
  - As globais `não encontrada` e `inconclusiva`, e o layout `reflexão não reconhecida` (que vem da busca), entram na
    regra de nova busca automática da spec 003 (6.2), igual a `não encontrado`/`inconclusivo`, mesmo sem cheat
    desejado. Eles também entram na regra de **manter o estado anterior** (`keeps_state`): uma nova busca que devolve
    um deles não derruba um cheat que estava disponível. `ambígua` e `fora do lugar` não são tentados de novo
    sozinhos, como nos patches.
  - Os motivos da tarefa de reflexão são tentados de novo pela própria tarefa (6.3: no intervalo da tarefa, por até 120 s, e o
    `r` reinicia o prazo), com o mesmo princípio: um índice já bom não é trocado por um pior.
  - Assim, um jogo ainda carregando, com tabelas incompletas, se resolve sozinho.
- **Disco ilegível, diário não resolvido ou motor `stuck`:** os cheats de valor seguem exatamente as regras dos
  patches (spec 003). Sem o disco, todos ficam `indisponível (disco ilegível)`. Ligar é recusado com a mesma
  mensagem dos patches. Os já ligados ficam com a manutenção suspensa (6.6) até o `r` resolver.
- **Ligar com o personagem ausente:** é aceito. O cheat fica `[ON ]` com `aguardando o personagem`, e o desejado fica
  guardado. Nos bits, a entrada do diário só nasce quando o objeto existe e é escrito pela primeira vez.
- **Desligar com o personagem ausente:** as travas só param. Nos bits, o motor tenta restaurar cada entrada. A que
  não passa na conferência de identidade é descartada (o objeto não existe mais) e conta como desligado com sucesso,
  com uma linha no log.
- **Personagem ausente** (controller não achado, `Pawn` nulo, conferência falhando, inclusive montado se P7 não achar
  outro campo): os cheats ligados continuam `[ON ]`, mas com a marca `aguardando o personagem`, e nada é escrito. Uma
  linha no topo mostra `personagem: encontrado`, `personagem: não encontrado` ou `personagem: —` (nenhum cheat de valor
  ligado ou desejado, então ninguém procura; 6.3). Quando o personagem volta, os cheats retomam sozinhos, sem evento
  repetido no registro (um evento ao perder e outro ao achar).
- **Religar depois de reconectar:** mesma regra da spec 003 para patches e cheats de valor (só na mesma chave de
  versão).
- **Saída** (`q`, sinais, pânico): as travas param, os bits são restaurados pelo motor (com o diário) e a saída segue o fluxo e os códigos de saída
  da spec 003.
- O laço de ~10 Hz usa o ciclo do laço principal que já existe (`LOOP_INTERVAL` = 100 ms). Ele faz só poucas
  leituras de ponteiro e as escritas necessárias, e nunca a varredura do controller (6.3). **A manutenção não conta
  como operação** no sentido da spec 003 (6.10): ela não descarta teclas (`after_op_keys`) e não invalida buscas.
  Ligar, desligar e restaurar os bits (escritas com diário) contam como operação, como os patches.

### 6.8 `gt ue` (só leitura)

- `gt ue class <Classe>`: os campos da classe e das superclasses, com offset, tamanho, tipo e, para `bool`, byte e
  máscara. Serve para conferir offsets depois de uma atualização e para a spec 006. Aceita `Pacote.Classe` (ex.:
  `/Script/Maine.SurvivalCharacter`) ou só o nome. Só com o nome e 2+ classes encontradas, lista as opções com o
  pacote e não escolhe nenhuma.
- `gt ue player`: acha o personagem (6.3) e mostra:
  - o endereço do controller e os ponteiros `Pawn`, `Character` e `AcknowledgedPawn`;
  - vida, stamina, comida, água, oxigênio e temperatura, com os valores máximos usados como alvo;
  - os bits de 6.5.2.

  Opções para a pesquisa (P6, P7) e o teste no jogo:
  - `--controller <endereço>` pula a varredura;
  - `--watch <segundos>` repete a leitura nesse intervalo, sem varrer de novo, e só refaz a varredura se o controller
    deixar de ser válido.
- Ambos aceitam `--json`, como as outras ferramentas do `gt`. Seguem os limites de leitura de 6.3 e não ativam a
  feature `write`.
- Custo esperado: cada chamada acha as globais pelos padrões (a mesma busca na `.text` que leva ~10 s na TUI), monta
  o índice de classes e, no `player`, varre o controller. Algumas dezenas de segundos por chamada são aceitáveis.
  Com `--gobjects`/`--names`, a busca de padrões é pulada. Os endereços aceitam o mesmo formato do `gt read`
  (absoluto ou `módulo+offset`).
- A lógica dos dois comandos fica em funções de biblioteca, que recebem o leitor e os endereços das globais, para
  poder testar contra a reflexão sintética sem o jogo.

## 7. Segurança e save

- Antes de qualquer teste que escreva no jogo, o dono faz backup do save (`AGENTS.md` 3). O plano pede isso
  explicitamente antes do primeiro passo com escrita.
- Os valores escritos estão na faixa normal do jogo, então um save gravado com um cheat ligado tem valores que o jogo
  produziria sozinho (barra cheia). Se os bits forem salvos, são restaurados ao desligar. Se a pesquisa (P6) ou o
  teste mostrarem que algum bit vai para o save e não volta, isso é registrado e o dono decide.
- As regras "toda escrita é reversível" e "verifique antes de escrever" (`AGENTS.md` 3) continuam valendo para
  patches e bits. Nas travas de barra, o dono decidiu que desligar só para a trava. A conferência antes de escrever
  passa a ser a identidade do objeto e do campo (6.4), e não bytes esperados, porque o valor de uma barra muda o
  tempo todo. O `AGENTS.md` ganha essas duas ressalvas, e também a exceção de unicidade para globais lidas (6.2).

## 8. Testes

| Teste | Exige jogo? |
|---|---|
| Resolução de global por padrão: `disp32` RIP-relativo, uma ocorrência, várias concordantes (aceita), várias discordantes (ambígua), endereço fora da seção de dados (recusa), comparação com o `rva` da versão | Não |
| Decodificação de entradas do `FNamePool` (ANSI e UTF-16, `Number` > 0), índices e blocos, sobre buffers | Não |
| `find_field` sobe pela `SuperStruct`, respeita os tetos e devolve offset, tipo e máscara de `bool` | Não |
| Conferência do layout recusa uma reflexão malformada (nome 0 ≠ `None`, contagens incoerentes, laço na `SuperStruct`) | Não |
| Assinaturas: `[ue]` e os oito `[[field]]` carregam; erros claros para campo faltando, tipo desconhecido ou faixa inválida | Não |
| Regras das travas (6.5.1): não baixa barra, só escreve quando muda, alvo fora da faixa vira `alvo inválido` (nunca `erro`) | Não |
| Decisão de restauração das entradas de bits (identidade bate/não bate, bits nossos/originais/de outro) | Não |
| Diário antigo, só com patches, continua legível | Não |
| **Reflexão sintética no `gt-testtarget`**: o alvo monta na própria memória um `GUObjectArray`, `FNamePool`, classes, campos, um controller, um pawn e componentes, com offsets diferentes dos do jogo. O alvo não é PE, então os endereços das duas globais chegam ao trainer direto (pelo backend cru da TUI e pelos testes da biblioteca), sem a busca de padrões. A busca fica coberta pelos testes puros acima e pelos testes com o jogo. O trainer acha tudo pelo nome, trava as barras, limpa e restaura os bits | Não |
| Na reflexão sintética: trocar o pawn por um novo com o antigo destruído (identidade não bate) e anular o `Pawn`; a escrita para, a entrada antiga é descartada sem escrever e a nova é criada | Não |
| SIGKILL com um bit limpo: a próxima execução restaura pelo diário; com o objeto "destruído" (identidade diferente), descarta sem escrever | Não |
| Leitura em lote do `gt-core` contra o `gt-testtarget`: mais de 1.024 itens, página ilegível no meio (leitura parcial do kernel tratada), ordem dos resultados | Não |
| Conferências de 6.4 na reflexão sintética: pawn com classe errada, componente com classe errada, `offset + tamanho > PropertiesSize`, endereço fora de região `rw` ou dentro de módulo; um segundo personagem com os mesmos componentes nunca é escrito | Não |
| Escrita em dados do motor: identidade do processo, recusa fora de região `rw`, diário gravado antes da escrita dos bits | Não |
| Recuperação com entrada de dados: restaurada quando a identidade bate, descartada quando não bate, e nunca tratada como "fora de memória executável" | Não |
| Pânico e SIGTERM com um bit limpo: o bit volta ao original na saída | Não |
| Troca de pawn com o antigo ainda vivo: os bits do antigo são restaurados antes de o novo ganhar entrada; nunca há duas entradas vivas para o mesmo `id` | Não |
| Alvo fora da faixa: `[ON ]` com `alvo inválido`, sem escrita, e retoma quando o alvo volta | Não |
| `gt ue class` e `gt ue player` (funções de biblioteca: a de classe no `gt-ue`, a do personagem no `gt-grounded2`; o `gt-cli` só formata) contra a reflexão sintética, inclusive nome de classe ambíguo | Não |
| A manutenção de ~10 Hz não descarta teclas nem invalida buscas | Não |
| `find_class` na biblioteca: pacote errado, classe de Blueprint filha de `Class`, 0 e 2+ resultados; família de propriedades de objeto | Não |
| Tarefa de reflexão: teto de tempo vira `reflexão inconclusiva`; repetição no intervalo por até 120 s e o `r` reiniciando o prazo; índice bom não é trocado por pior; intervalo `max(10 s, 2 × duração)` | Não |
| Procura do controller: só com cheat de valor ligado ou desejado; 2+ controllers (vale o único com `Pawn` não nulo, senão ambíguo); controller válido com `Pawn` nulo há mais de 30 s dispara nova procura | Não |
| Pawn que não é `SurvivalPlayerCharacter` (montaria simulada) nunca é escrito | Não |
| `gt ue player` com `--controller` e `--watch` (função de biblioteca, com relógio e leitor de teste) | Não |
| Cheats de valor em `verificando…` até o índice da reflexão chegar (ligar recusado), sem atrasar a conexão nem os cheats de patch; manutenção suspensa com motor `stuck`, sem motor ou com resto pendente | Não |
| TUI com o backend falso: estados `aguardando o personagem`, linha `personagem:`, ligar e desligar com o personagem ausente, nova busca automática por `reflexão não reconhecida` (e estado mantido numa nova busca transitória), procura do personagem só com cheat de valor ligado e resultado descartado ao mudar de conexão, religar depois de reconectar, saída restaurando bits | Não |
| Com o jogo aberto: globais resolvidas, layout conferido, os oito disponíveis, `gt ue player` lê valores coerentes | Sim (`#[ignore]`) |
| Com o jogo aberto: liga e desliga `sem-dano-queda` e `nao-morrer` e confere o byte original no fim | Sim (`#[ignore]`) |

Os testes existentes continuam passando. Nenhum teste sem o jogo depende de offsets do Grounded 2.

## 9. Critério de pronto

Com backup do save feito pelo dono e o FLiNG fechado:

1. A TUI mostra os oito em SOBREVIVÊNCIA como disponíveis, e `gt ue player` mostra os valores do personagem.
2. **Vida:** com o cheat ligado, levar dano (queda ou inimigo): a vida volta ao máximo em menos de 1 s. Desligado,
   o dano fica.
3. **Stamina:** correr ou atacar até ela cair sem o cheat; com ele, ela fica cheia. Anotar se ela fica acima de
   `MaxStamina` (como os 120/100 observados).
4. **Comida, água e oxigênio:** com cada um ligado, o valor fica no máximo (oxigênio testado debaixo d'água).
   Desligado, volta a cair.
5. **Temperatura:** num lugar frio ou quente, com o cheat ligado, a temperatura não sai do neutro.
6. **Sem dano de queda:** cair de uma altura que dá dano: com o cheat, não dá. Desligado, volta a dar.
7. **Não morrer:** registrar o que acontece (vida em zero sem morrer, ou morte). Funcionar não é exigido para fechar
   a fase, mas o resultado decide se o cheat fica (6.5.2).
8. **Morte e menu:** morrer (com "Não morrer" desligado) ou voltar ao menu com cheats ligados: a TUI mostra
   `aguardando o personagem` e retoma sozinha quando o personagem volta.
9. Reiniciar o jogo com a TUI aberta e cheats ligados: depois de reconectar, eles voltam ligados e funcionando.
10. Sair da TUI: os bits voltam ao original (conferido com `gt ue player`), e a `.text` não tem alteração (`gt diff`).
11. O dono confirma que o jogo ficou estável e que o save carrega normal depois.

**Sobrevivência a atualizações** (parte do critério do ROADMAP): hoje isso é provado pelos testes com reflexão
sintética, com offsets diferentes dos do jogo, e pela ausência de offsets de campo no código e nas assinaturas. A
confirmação no jogo fica para a próxima atualização do Grounded 2. A spec 005 é fechada com essa pendência anotada no
ROADMAP. A Fase 5 só fecha com a spec 006.

**Como observar:** os efeitos dos itens 2 a 8 são vistos no HUD do jogo. Os números (stamina acima de `MaxStamina`,
bits) são conferidos com `gt ue player --gobjects … --names …` num segundo terminal. Com os endereços, a chamada não
refaz a busca de padrões.

## 10. Documentos a atualizar

- `docs/RESEARCH.md`: respostas de 6.1 (P1–P9), custo medido da varredura, padrões das globais e resultado do teste no jogo.
- `docs/ARCHITECTURE.md` 5.3: formato das assinaturas com `[ue]` e `[[field]]`, e a regra de unicidade das globais.
- `docs/specs/003-interface-tui.md`: nota em 6.2 sobre os motivos novos na nova busca automática e em `keeps_state`.
- `CLAUDE.md`: comandos `gt ue class` e `gt ue player` nos comandos frequentes.
- `docs/ARCHITECTURE.md`: 5.4 (escrita em dados, sem pausa, entrada de bits no diário), 5.6 (layout no `[ue]`,
  laço no ciclo do laço principal em vez de thread própria), 3 (módulos do `gt-ue` e do `gt-grounded2`, comandos
  `gt ue`) e registro de decisões.
- `AGENTS.md` 3: ressalvas das travas de barra (7); `AGENTS.md` 4: exceção de unicidade para globais lidas (6.2).
- `docs/specs/002-motor-patches.md`: nota de que a escrita em dados e as entradas de dados do diário (spec 005, 6.6) não seguem a política de memória executável.
- `docs/ROADMAP.md`: Fase 5 dividida em spec 005 (sobrevivência) e 006 (resto); status ao terminar.

## 11. Decisões tomadas

| Decisão | Quem |
|---|---|
| Barras travadas no máximo enquanto ligadas, sem valor ajustável | Dono |
| "Não morrer" separado de "Vida infinita"; podem ser usados juntos | Dono |
| Carga, dano, ciência e movimento na spec 006, ainda na Fase 5 | Dono |
| Ao desligar, as barras ficam onde estão; os bits voltam ao original | Dono |
| Sem painel de valores na TUI | Dono |
| Padrão de global aceito com várias ocorrências se todas resolvem para o mesmo endereço (só para leitura de globais) | Dono |
| Vida infinita com `CurrentDamage = 0`, aceitando que um golpe maior que a vida entre duas reescritas mate | Dono |
| `rva` da versão é o da global resolvida; ligar com o personagem ausente fica `ON` aguardando; reflexão incompleta entra na nova busca automática | IA |
| `find_class` por pacote + nome; desempate de controllers pelo `Pawn` não nulo | IA |
| Procura do personagem numa tarefa de fundo própria, só com cheat de valor ligado ou desejado, com intervalo `max(10 s, 2 × duração)` e teto de 60 s | IA: não varrer a memória à toa (o jogo já fechou numa varredura grande) |
| Cheats de valor em blocos `[[field]]`, separados de `[[patch]]`; alvo fora da faixa vira `alvo inválido`, não `erro` | IA |
| Uma entrada de bits por `id`; na troca de objeto, a entrada antiga é resolvida antes de criar a nova; identidade inclui `InternalIndex` | IA |
| `gt ue` aceita os endereços das globais por argumento; se P1 não achar padrão aceitável, para e volta ao dono | IA |
| A busca de padrões não varre objetos; índice da reflexão e procura do controller numa tarefa de fundo, com os cheats de valor em `verificando…` até o índice chegar | IA: a conexão e os cheats de patch não ficam mais lentos |
| Manutenção suspensa com motor `stuck`, sem motor ou com resto pendente; `gt-patch` ignora `[[field]]` no `list`/`toggle` e restaura entradas de dados | IA |
| `gt ue player` com `--controller` e `--watch` para a pesquisa | IA |
| Pawn escrito só se `is_a(SurvivalPlayerCharacter)`; família `ObjectProperty`/`ObjectPtrProperty`; conferência do layout dividida entre busca (barata) e tarefa de reflexão | IA |
| P9 (dano de queda) resolvido por pesquisa estática antes do código | IA |
| A parte interna fica com a IA | Dono |
| Máximo relido do personagem a cada ciclo; nunca baixar uma barra acima do alvo | IA |
| Sem dano de queda por `bTakeFallDamage` do personagem, não pelas configurações de modo | IA (risco de save das configurações) |
| Escrita em dados sem pausa; bits no diário com conferência de identidade por bytes (vtable + classe) | IA |
| Layout de reflexão e padrões das globais no `[ue]` das assinaturas, sobrescritível por versão | IA |
| Controller achado por `instances_of(SurvivalPlayerController)` (classe C++), único, em varredura lenta e rara | IA |
| Laço de ~10 Hz no ciclo do laço principal da TUI, não numa thread própria (muda `ARCHITECTURE.md` 5.6) | IA: o motor e o diário moram no laço principal |
| Personagem ausente: cheats continuam `ON`, com `aguardando o personagem`, e retomam sozinhos | IA |
| Comandos `gt ue class` e `gt ue player` | IA |
| `id`, nomes e descrições em pt-BR desta spec | IA |

## 12. Deixado para quem implementa

- Os tipos e a divisão em módulos do `gt-ue` e do `gt-grounded2`, e o formato exato de `[ue]` e `[[field]]` no TOML
  (seguindo as regras de carga e de erro de 6.2 e 6.5).
- O formato da entrada de bits no diário, desde que os diários antigos continuem legíveis.
- O tamanho dos lotes e das pausas da varredura do controller, dentro dos limites de 6.3.
- Como o `gt-testtarget` monta a reflexão sintética.
- O texto exato das descrições e dos motivos de indisponível.
