# Spec 003 — Interface TUI

| Campo | Valor |
|---|---|
| Fase | 3 (ver `docs/ROADMAP.md`) |
| Estado | Aprovada pelo dono em 2026-10-25 (autorizou seguir direto para o plano) |
| Data | 2026-10-25 |

## 1. Intenção

Controlar os cheats com conforto, num terminal ao lado do jogo, sem digitar comandos. A Fase 2 entregou o motor
de patches e o `gt-patch`, que liga um patch por linha de comando e fica parado até Ctrl+C. Esta fase entrega o
`gt-tui`: uma tela com a lista de cheats por categoria, o estado de cada um (`ON`, `OFF`, `indisponível`), valores
ajustáveis por tecla, o status do jogo e a reconexão automática quando o jogo reinicia.

Ainda não existe cheat real (eles chegam na Fase 4). O único patch é o de teste da Fase 2 (`teste-codificacao`),
que a TUI mostra só no modo de diagnóstico (6.5). Os **cheats com valor** (multiplicadores, velocidade) também não
existem ainda: esta fase define o modelo e o controle na tela e os prova contra o processo-alvo de teste. O
primeiro valor real chega na Fase 4 (velocidade) ou na 6 (XP).

As garantias da Fase 2 continuam valendo sem exceção: conferir os bytes antes de escrever, pausa conferida,
diário em disco e tudo desfeito ao sair.

## 2. Objetivos

1. Binário `gt-tui` que usa o motor de patches **no próprio processo**, com as mesmas garantias do `gt-patch`
   (decisão do dono).
2. Tela única com status, lista agrupada por categoria e registro de eventos (6.4), operada só pelo teclado do
   terminal (6.6).
3. Conexão e reconexão automáticas: espera o jogo abrir, percebe quando ele fecha e, quando volta, religa sozinho os
   cheats que estavam ligados (6.2, decisão do dono).
4. Cheats com valor: formato nas assinaturas, controle na tela e troca de valor com o cheat ligado, numa única
   pausa (6.3 e 6.7).
5. Categoria e nome curto de cada patch nas assinaturas (6.3).
6. Pendências da Fase 2 (seção 7), começando pela do SIGCONT que falha.

## 3. Fora do objetivo

- Cheats reais (Fases 4–6). Nenhuma assinatura nova de jogo nesta fase.
- Valores em dados com laço de "congelar" (Fase 5) e valores dentro de code caves (Fase 6). Aqui um valor só pode
  morar num **imediato** de uma instrução do patch (6.3).
- Hotkeys globais (fora do terminal), overlay no jogo e interface gráfica.
- Lembrar entre sessões quais cheats ficaram ligados ou os valores escolhidos (decisão do dono: sempre começa tudo
  `OFF`, com os valores padrão).
- Mudanças de comportamento no `gt-patch` além das pendências da seção 7.
- Mouse.

## 4. Anti-objetivos (falha mesmo que "funcione")

- **Aplicar** um patch no jogo sem o dono apertar uma tecla naquela sessão do `gt-tui`. A única aplicação automática
  é religar o que o dono ligou antes nesta mesma sessão e não desligou, durante a reconexão depois de o jogo fechar
  e abrir de novo (incluindo as novas buscas automáticas logo depois dela, 6.2). **Restaurar** bytes originais e
  **retomar** o jogo (recuperação do diário, desfazer na saída, nova tentativa de SIGCONT, restos de uma falha) não
  precisam de tecla: são as garantias da Fase 2.
- Perder alguma garantia da Fase 2 (spec 002, seção 4) por a TUI ficar aberta por horas, reconectar ou rodar com o
  terminal em modo cru.
- Sair do `gt-tui` (tecla, sinal, pânico, terminal fechado) deixando patch aplicado sem avisar como restaurar.
- Deixar o terminal estragado (modo cru, tela alternativa, cursor escondido) depois de sair, inclusive por pânico.
- Escrever no terminal (`stdout`/`stderr`) enquanto a TUI está desenhada, por log ou por mensagem solta.
- Um valor de cheat que mude a fronteira de instrução do patch.
- Interface que fica sem resposta sem mostrar o que está fazendo (6.10).

## 5. Onde cada coisa mora

| Crate | Conteúdo desta fase |
|---|---|
| `gt-core` | No motor: a operação de troca de valor de um patch ativo (6.7), com diário e recuperação; resultados que distinguem os desfechos de ligar, desligar e trocar valor (sucesso, recusa limpa, bytes de outro com o patch tirado do registro, falha com restos, jogo fechou); consulta das entradas pendentes (restos) e um pedido para restaurá-las (6.8); consulta do estado de SIGCONT que falhou (seção 7, item 1); um jeito de quem usa o motor perceber que o jogo fechou e mandar o motor descartar o registro sem escrever, sem precisar rodar uma operação (hoje isso só acontece dentro de uma operação ou no `Drop`). Uma chamada para "tentar o SIGCONT agora" (para o `r`, 6.6). A forma exata dessas APIs fica para o plano. Fora do motor, um módulo genérico de **valores** (`ValueSpec`: tipo, `slot`, `min`, `max`, `step`, `default`, `unit`, `decimals`; a grade por `k`; o ponto mais próximo da grade; a montagem dos bytes `i32`/`u32`/`f32`), usado pelo `gt-grounded2`, pelo núcleo da TUI e pela implementação de teste da interface de alvo. Também a desduplicação dos avisos (seção 7, item 4) |
| `gt-grounded2` | Campos novos das assinaturas (`name`, `category`, bloco `value`, convertido para o `ValueSpec` do `gt-core`) e as regras de carga (6.3), inclusive a do imediato pelo `iced-x86`; o estado `ativo`/`pendente` pelo diário (`catalog.rs`, `state.rs`) passa a reconhecer uma troca de valor interrompida (6.7); busca do catálogo para um subconjunto de definições (as novas buscas da 6.2); `teste-codificacao` ganha categoria `diagnostico` |
| `apps/gt-tui` | Biblioteca e binário. A biblioteca é o **núcleo da TUI** (estado, teclas, conexão e reconexão, fila de escritas de valor), a interface de alvo e as duas implementações dela (abaixo), e uma interface de terminal com implementação falsa para os testes. O núcleo é testável sem terminal e sem o jogo (seção 8). Ativa a feature `write` |
| `apps/gt-patch` | Só as pendências da seção 7 |
| `gt-testkit` | Alvo de teste com uma página executável que roda sem parar um laço com uma instrução de imediato (`mov eax, imm32`) e grava o `eax` numa palavra de dados que o teste lê, para provar que o código executado usa o valor novo; o binário de apoio ganha a troca de valor e ganchos só de teste que disparam numa pausa escolhida (ex.: a N-ésima, ou a da troca), numa falha de SIGCONT (persistente ou de uma vez só) e numa escrita que falha no meio com o desfazer também falhando, para deixar restos (seção 8). Se um teste precisar de pseudo-terminal, o código com `unsafe` dele fica em `gt-core/src/mem/` (`AGENTS.md` 5) |
| `gt-cli` | Só a chamada nova do `permission_hint` (seção 7, item 5); continua sem a feature `write` |

Muda a decisão de 2026-10-25 do `ARCHITECTURE.md` ("`gt-patch` é o único aplicativo com `write`"): passam a ser
`gt-patch` e `gt-tui`. O `gt` continua somente leitura, com a mesma prova (`cargo build -p gt-cli` isolado compila
sem `write`). A regra de dependência do `ARCHITECTURE.md` 3 não muda.

O núcleo da TUI conversa com o jogo por uma **interface de alvo** (abrir/achar o alvo, buscar o catálogo, ligar,
desligar, trocar valor, restaurar restos, saber se o alvo terminou). A interface mora na biblioteca do `gt-tui`, e as
duas implementações também: a do jogo (usando o `gt-grounded2`) e uma de teste, contra o processo-alvo do
`gt-testkit` com patches crus, para testar conexão, reconexão, sinais e valores sem o jogo. A implementação de teste é
um módulo normal da biblioteca, atrás de uma feature só de teste (não `#[cfg(test)]`), para que os testes de
integração em `apps/gt-tui/tests/` a usem; ela depende só do `gt-core`, e os testes trazem o processo-alvo pelo
`gt-testkit` como dependência de desenvolvimento (o `gt-testkit` não depende do `gt-tui`, então não há ciclo). O
formato da interface fica para o plano.

## 6. Comportamento

### 6.1 Início, trava e recuperação

Ordem no início do `gt-tui`, antes de desenhar a tela:

1. Ignorar SIGTSTP/SIGTTIN/SIGTTOU e registrar SIGINT/SIGTERM/SIGHUP/SIGQUIT (o `Shutdown` da Fase 2).
2. Pegar a trava do diário (`journal.lock`). Ocupada: sai com "outro gt-patch ou gt-tui já está em execução" e a
   dica "feche o outro programa antes", no `stderr` (hoje a mensagem cita só o `gt-patch` e a dica fala em Ctrl+C; as
   duas mudam também no `gt-patch`, seção 7, item 5). Vem antes da conferência do terminal para poder ser testada
   sem pseudo-terminal.
3. Conferir que `stdin` e `stdout` são um terminal. Se não forem, sai com erro ("o gt-tui precisa de um terminal")
   sem escrever nada no jogo.
4. Iniciar o log em arquivo (6.9), que não toca o terminal, para que os avisos da recuperação também fiquem nele.
5. Entrar na tela alternativa e no modo cru, e desenhar a tela com "recuperando o diário…" no topo.
6. Recuperar o diário (spec 002, 6.8) e mostrar o relatório no registro de eventos (6.4). A recuperação pode pausar
   o jogo e, no pior caso, levar alguns segundos: ela segue a regra das operações do motor (6.10).

A trava fica com o `gt-tui` do início ao fim, conectado ou não. Enquanto ele estiver aberto, `gt-patch run`,
`toggle`, `restore` e `profile` recusam; `gt-patch list` funciona em paralelo e mostra os patches da TUI como
`ativo` (pelo diário, spec 002 6.4).

**Outros resultados da recuperação:** diário corrompido (renomeado para `.bad`, spec 002 6.8) vira aviso fixo: "diário
corrompido movido para <caminho>; o jogo pode ter patches sem registro, confira com gt diff". Jogo retomado ou parado
por outro vão para o registro de eventos.

**Diário não resolvido.** A recuperação pode deixar entradas no diário de dois jeitos: com `kept` no relatório
(spec 002 6.7) ou devolvendo erro (threads que não saem do trecho, prazo da pausa, permissão, E/S), com as entradas
mantidas. Nos dois casos, aqui e a cada conexão (6.2), a TUI **não escreve no jogo**: mostra o aviso fixo (6.4) com
os `id` (ou a causa do erro) e a orientação, e as teclas de ligar ficam bloqueadas. O motor não é aberto (6.2): a
lista mostra os cheats com o estado do catálogo, se o jogo estiver conectado, mas nenhum pode ser ligado. A TUI não
sai por isso. A tecla
`r` (6.6) tenta a recuperação de novo. Isso segue a regra do `gt-patch` (`ensure_recovered`): um motor novo
regravaria o diário só com os patches dele e apagaria as entradas não resolvidas.

Opções de linha de comando: `--pid` e `--diagnostico` (6.5). `--help` e `--version` pelo `clap`.

**`--pid <pid>`** (como no `gt` e no `gt-patch`): a TUI usa só esse processo. Um PID que fechou nunca volta (o jogo
reaberto tem outro PID), então, com `--pid`, quando o processo termina a TUI vai para o estado final
`o processo terminou` (6.2), que diz "reinicie o gt-tui sem --pid, ou com o PID novo", e não reconecta. Se o PID não
existir ao abrir, o estado é o mesmo. Conferir que o PID é mesmo o jogo segue a regra atual do `Target::open` (a
busca do catálogo falha se o exe do jogo não estiver no processo, e o estado vira `erro`).

### 6.2 Conexão e reconexão

A TUI está sempre num destes estados de conexão, mostrado no topo da tela (6.4):

| Estado | Quando | O que a TUI faz |
|---|---|---|
| `aguardando o jogo` | Nenhum processo do jogo encontrado (mesma regra de busca do `gt`) | Procura de novo a cada ~2 s |
| `sem acesso` | Achou o jogo, mas a abertura falhou por permissão | Mostra a dica do `ptrace_scope` (com o nome `gt-tui`) e tenta de novo a cada ~2 s |
| `conectando` | Achou o jogo; abrindo o alvo e buscando os padrões | Mostra "procurando padrões"; teclas de ligar ficam bloqueadas |
| `conectado` | Alvo aberto e catálogo resolvido | Opera normalmente. Se o cabeçalho do disco não pôde ser lido (ou a versão do disco difere da memória), o motor não abre (spec 002 6.6): todos os cheats ficam `indisponível (disco ilegível)` e o topo mostra a causa |
| `vários processos` | A busca achou mais de um processo do jogo (`ManyCandidates`) | Mostra os PIDs e a dica do `--pid`; tenta de novo a cada ~2 s |
| `erro` | A abertura falhou por outro motivo (ex.: módulo do jogo ainda não mapeado) | Mostra a causa e tenta de novo a cada ~2 s |
| `o processo terminou` | Só com `--pid` (6.1) | Estado final; só `q` sai |

Os intervalos exatos ficam para o plano. A busca pelo processo enquanto espera é só a varredura de
`/proc/*/cmdline`; a abertura completa do alvo só acontece quando um processo do jogo aparece.

**Conectar**, na ordem da spec 002 6.8 (recuperação antes de resolver o alvo, porque ela confere a identidade
registrada no diário direto no `/proc`): recuperar o diário de novo (6.1; normalmente não há nada, porque o diário
da conexão anterior é limpo quando o jogo fecha, mas ele pode ter entradas deixadas por uma recuperação bloqueada),
abrir o alvo, buscar o catálogo (uma varredura por conexão, spec 002 6.4, sem o diário, como o `gt-patch`) e abrir o
motor (conferindo a identidade, como o `gt-patch`). O motor só abre com o diário resolvido (6.1) e com o cabeçalho do
disco lido (tabela acima). Faltando um dos dois, a TUI fica `conectado` **sem motor**. Qualquer `r` ou nova busca que
termine com os dois resolvidos abre o motor nessa hora.

**Nova busca** (pelo `r` ou automática, abaixo): reabre o alvo (o `maps` e os módulos do `Target` são uma fotografia
da abertura, e um jogo recém-aberto ainda pode estar mapeando módulos), confere que é o mesmo processo (identidade) e
busca o catálogo só das definições dos cheats que estão desligados. O resultado só vale para os cheats que continuam
desligados, e sem operação em andamento neles, no momento em que a busca termina. Para isso, a busca leva consigo o
número da conexão e, para cada cheat, um contador de operações, lidos quando ela começa: se a conexão mudou (o jogo
fechou ou reabriu no meio), o resultado inteiro é descartado; se um cheat passou por alguma operação durante a busca
(ligado e desligado de novo, por exemplo), o resultado dele é descartado. Os ligados ficam com o estado que
já tinham. A memória lida pela nova busca pode ter outros cheats aplicados, e o padrão de um cheat desligado pode
cobrir bytes de um deles: por isso, um cheat que já tinha endereço resolvido nesta conexão e que a nova busca não
acha (`não encontrado`, `inconclusivo`) **mantém o estado anterior**; só um resultado `disponível` ou um estado
estável diferente (`ambíguo`, `bytes diferentes`, `fora do lugar`) troca o estado dele. O tempo de uma busca no jogo real ainda não foi medido: a sessão do critério de pronto o mede e registra no
`RESEARCH.md` (seção 9). Como a busca roda em thread (6.10), o tempo dela não trava a tela; ele só ajusta a cadência:
o intervalo entre duas buscas automáticas é o maior entre ~5 s e duas vezes a duração da última busca, para que elas
nunca ocupem mais da metade do tempo.

> **Nota (spec 006, 6.5, 6.6 e 6.9):** os cheats de valor (`[[field]]`) também podem ter valor ajustável: o bloco
> `[field.value]` é a grade do `[patch.value]` sem `slot`, com as mesmas teclas `←→` e as regras de 6.2 e 6.7
> (desligado, as setas só mudam o valor mostrado; intervalo de 150 ms; o último valor mostrado vence). Existe também
> a linha `[ação]` (Somar ciência): mostra `[ação]` e a quantidade no lugar de `[ON ]`/`[OFF]`, não entra na contagem
> de cheats ligados do topo, executa no máximo uma vez por segundo e não tem estado, religar nem restauração.
> **Nota (spec 005, 6.7):** as globais do Unreal `não encontrado`/`inconclusivo` e a conferência barata
> `reflexão não reconhecida` também disparam a nova busca automática (mesmo sem cheat desejado) e também mantêm o estado
> anterior de um cheat já disponível (`keeps_state`). A nova busca leva o id especial `@ue` enquanto não há índice bom.

**Jogo fechou** (o `pidfd` fica legível, ou a identidade muda): o motor descarta os patches sem escrever (spec 002
6.7), o registro de eventos diz "o jogo fechou", e a TUI volta para `aguardando o jogo` (ou `o processo terminou`,
com `--pid`). Os cheats que estavam ligados continuam marcados como **desejados** (abaixo). Se isso acontecer no meio
de uma operação, a operação termina com "o jogo fechou" sem erro de escrita, e a **intenção do dono** decide os
desejados: um desligar interrompido tira o cheat dos desejados; um ligar interrompido não o coloca (a aplicação
automática só vale para o que ligou com sucesso, 4); uma troca de valor interrompida guarda o valor novo; um religar
interrompido não muda nada.

Mesmo sem operação em andamento, o fechamento é percebido, com ou sem motor: a TUI abre um `pidfd` próprio do jogo na
conexão (conferido com a identidade, spec 002 6.7), independente do motor, e o consulta a cada volta do laço. Quando o
jogo fechou, ela pede ao motor (se houver) o descarte sem escrever (5) e volta a `aguardando o jogo`. Isso vale também
para `conectado` sem motor.

**Religar depois de reconectar** (decisão do dono). A TUI guarda, só na memória, o conjunto de cheats **desejados**:
um cheat entra quando o dono o liga com sucesso e sai quando o dono o desliga (o valor escolhido vai junto). Ao
conectar de novo:
- cada cheat desejado que estiver disponível (verificado ou não verificado) é religado, com o valor guardado, num
  único lote (spec 002 6.2). Se o lote for recusado, cada um é tentado sozinho, para que um patch com problema não
  impeça os outros;
- cada cheat desejado que não puder ser religado aparece como `indisponível` com o motivo e a marca de religar
  pendente (6.4), e o registro de eventos diz que ele não foi religado. Ele **continua desejado**;
- **jogo recém-aberto:** o jogo pode ainda estar carregando quando a TUI conecta. Depois de **toda** conexão (a
  primeira também), enquanto houver cheat mostrado `não encontrado` ou `inconclusivo`, ou desejado indisponível, a TUI
  refaz a busca dos cheats desligados sozinha, a cada ~5 s, por até ~2 minutos, e religa os desejados que
  aparecerem. Depois disso, só o `r` refaz a busca, e ele reinicia o prazo de ~2 minutos. `disco ilegível` não
  dispara busca automática (não muda sozinho); só o `r`. A pausa com o jogo carregando nunca foi
  observada (o portão da spec 002 foi com o jogo na partida); o comportamento visto na sessão do critério de pronto
  (seção 9) vai para o `RESEARCH.md`;
- **só na mesma versão:** a religação automática só acontece se a chave de versão do jogo for a mesma da conexão em
  que o dono ligou aquele cheat. Numa versão diferente (o jogo atualizou entre uma abertura e outra), o código é outro
  e o dono nunca o viu: o cheat fica com a marca de religar pendente e o registro de eventos diz "versão nova; aperte
  espaço para ligar". O espaço liga normalmente (é uma tecla do dono) e atualiza a versão guardada;
- religar um não verificado escreve o aviso do 6.5.

Para tirar um cheat dos desejados sem que ele esteja ligado, o dono aperta espaço nele (6.6): a marca de religar
pendente some e o registro de eventos confirma.

Depois de um `r` que resolve o diário e abre o motor (6.6), os desejados são religados pelas mesmas regras.

Um cheat que falha ao ligar pela tecla (bytes diferentes, threads que não saem do trecho, prazo da pausa) **não** entra
nos desejados: fica `OFF` e o motivo vai para o registro de eventos. Quando a falha deixa o motor com restos (escrita
interrompida, SIGCONT que falhou), vale a 6.8.

**Valor guardado:** é sempre o valor mostrado na tela. As setas mudam esse valor também num cheat desligado,
indisponível ou com a marca de religar pendente; religar usa o valor mostrado naquele momento.

**Valor guardado numa versão nova:** se a reconexão cair numa versão em que o bloco `value` do cheat mudou (6.3), o
valor guardado é levado ao ponto mais próximo da grade nova, dentro de `[min, max]`, e o registro de eventos diz o
valor usado.

### 6.3 Assinaturas: nome, categoria e valor

Campos novos no `generic.toml` (spec 002, 6.3), também sobrescrevíveis no `<chave>.toml` como os demais:

- **`name`** (obrigatório): nome curto em pt-BR para a lista (ex.: "Cargas infinitas"). A `description` continua
  como texto longo.
- **`category`** (obrigatório): `sobrevivencia`, `itens`, `progressao`, `movimento` ou `diagnostico`. A lista mostra
  as categorias nessa ordem, e os cheats dentro de cada uma na ordem do arquivo. `teste-codificacao` passa a ter
  `category = "diagnostico"` e um `name`.
- **`value`** (opcional): faz do patch um **cheat com valor**. Campos:
  - `type`: `i32`, `u32` ou `f32` (little-endian);
  - `slot`: offset, dentro de `replacement`, onde os bytes do valor vão;
  - `min`, `max`, `step` e `default`;
  - `unit` (opcional): sufixo mostrado depois do número (ex.: `"x"`);
  - `decimals` (opcional, só `f32`): casas mostradas.

  Os bytes de `replacement` no `slot` são a codificação do `default`.

Regras de carga, com erro claro (arquivo e campo, como na Fase 2):
- `name` não vazio; `category` uma das cinco;
- `min ≤ default ≤ max`, `step > 0`, todos finitos e representáveis no `type`;
- `default` cabe exatamente na grade `min + k·step` (para `f32`: `k` é recalculado arredondando `(default − min) / step`, e `|default − (min + k·step)|` precisa ficar
  abaixo de `step · 1e-4`);
- `slot + tamanho do tipo` cabe em `replacement`, e os bytes ali são a codificação do `default`;
- **o `slot` fica inteiro dentro do imediato de uma única instrução de `replacement`**, e tem exatamente o tamanho
  desse imediato (conferido com o `iced-x86`, pelos offsets de constante da instrução decodificada). Assim, nenhum
  valor muda a fronteira de instrução: os bytes de qualquer valor decodificam nas mesmas instruções, com os mesmos
  tamanhos. Imediato com extensão de sinal menor que o tipo (ex.: `imm8` lido como 32 bits) é recusado nesta fase;
- a instrução do `slot` não é um desvio (`jmp`/`call`/`jcc` relativos): o `iced-x86` também informa o deslocamento
  relativo de um desvio como constante, e trocá-lo mudaria o destino, não um valor;
- com `type = "u32"` num imediato de 32 bits estendido com sinal para 64 bits (ex.: `mov rax, imm32`), `max` precisa
  caber em `i32::MAX`, para que o valor lido pelo jogo seja o mesmo mostrado.

Com `type = "f32"`, os números do bloco podem ser escritos no TOML como inteiro ou decimal; com `i32`/`u32`, só como
inteiro (`1.0` é erro de carga). No `<chave>.toml`, o bloco `value` é sobrescrito inteiro (nunca campo a campo), e as
regras de carga valem para o resultado. `name` e `category` também podem ser sobrescritos, pela regra geral.

**Risco conhecido:** o primeiro cheat com valor previsto (velocidade, Fase 4) ainda não foi investigado, e o valor
dele pode não estar num imediato. Se não estiver, a fase dele estende este modelo, com spec própria.

**Bytes de um valor:** `replacement` com o `slot` trocado pela codificação do valor. Cada valor passado ao motor é
sempre um ponto da grade `min + k·step`, limitado a `[min, max]` (calculado por `k`, sem somar `step` repetidas
vezes, para o `f32` não acumular erro). O patch montado passa pela validação da spec 002 6.1 como qualquer outro.

### 6.4 Tela

Decisão do dono: **lista única agrupada**, com status no topo e registro de eventos embaixo.

```
┌ grounded-trainer ─ jogo conectado (pid 4123) ───────┐
│ versão c935e799-0b27c000 (com assinaturas) 3/4      │
├─────────────────────────────────────────────────────┤
│ SOBREVIVÊNCIA                                       │
│   God mode               indisponível (não achado)  │
│ ITENS                                               │
│ > Cargas infinitas       [ON ]                      │
│   Duplicar ao arrastar   [OFF]                      │
│ MOVIMENTO                                           │
│   Velocidade             [OFF]  1.5x  ◀ ▶           │
├─────────────────────────────────────────────────────┤
│ 12:04 Cargas infinitas ligado (pausa 3 ms)          │
└ ↑↓ mover  espaço liga/desliga  ◀▶ valor  q sair ────┘
```

(Os cheats do desenho são ilustrativos; nesta fase a lista normal fica vazia, 6.5.)

- **Topo:** estado de conexão (6.2) e, conectado, PID, chave de versão e se ela tem assinaturas conferidas
  (`com assinaturas` quando há arquivo da versão, `sem assinaturas` quando não há, ou o motivo de não haver disco; um
  cheat sem entrada no arquivo da versão aparece com `?` mesmo assim), e quantos cheats mostrados estão disponíveis
  ou ligados (os `ON` contam), do total mostrado (os da categoria `diagnostico` só contam com `--diagnostico`). Durante uma operação
  demorada, o topo mostra o que está sendo feito (6.10).
- **Lista:** uma linha por cheat, sob o cabeçalho da categoria. Categoria sem cheat mostrado não aparece. Estado:
  - `[ON ]`: aplicado por esta TUI;
  - `[OFF]`: disponível; com a marca `?` ao lado quando é `disponível (não verificado)` (decisão do dono);
  - `indisponível (<motivo curto>)`: os demais estados da spec 002 6.4 (`não encontrado`, `ambíguo`,
    `bytes diferentes`, `fora do lugar`, `inconclusivo`, `disco ilegível`), "jogo não conectado" e "alterado por
    outro" (ao desligar ou trocar valor, 6.7); os textos são os da tabela da spec 002 6.4 (ex.: `não encontrado`;
    o desenho acima abrevia);
  - `erro`: o cheat ficou com restos no jogo depois de uma falha (6.8) e só a recuperação resolve;
  - **religar pendente:** um cheat desejado (6.2) que não está ligado ganha a marca `↻` ao lado do estado;
  - cheat com valor: o valor atual com `unit` e as setas `◀ ▶`, também com o cheat `OFF` (o valor vale quando
    ligar).
  Os estados `ativo`/`pendente` da spec 002 não aparecem: com a trava na TUI, ninguém mais aplica, e o pendente é
  resolvido pela recuperação (6.1).
- **Registro de eventos:** as últimas linhas, com hora, do que aconteceu: ligado/desligado (com a pausa medida),
  valor trocado, recusas com o motivo, jogo fechou/conectou, religados ou não depois de reconectar, relatório da
  recuperação, avisos de não verificado. O conteúdo completo vai também para o log em arquivo (6.9).
- **Rodapé:** as teclas.
- **Aviso fixo** (faixa destacada acima da lista) para o que exige atenção: diário com entradas não restauradas
  (6.1) e jogo que pode ter ficado parado (6.8).
- **Terminal pequeno:** abaixo de um tamanho mínimo (ex.: 60×15, o plano fixa), a tela mostra só "aumente o
  terminal"; só `q` (sair) e `r` funcionam, porque a lista não está visível.

A seleção (`>`) anda só pelas linhas de cheat (pula os cabeçalhos). Um cheat indisponível pode ser selecionado.
Quando a lista não cabe na altura disponível, ela rola para que a linha selecionada fique sempre visível (com o
cabeçalho da categoria dela, se couber).

### 6.5 Modo de diagnóstico

Decisão do dono: a categoria `diagnostico` só aparece com `gt-tui --diagnostico`. Sem a opção, os patches dela não
são mostrados nem podem ser ligados. Enquanto não houver cheats reais, a lista normal fica vazia, com a mensagem
"nenhum cheat disponível nesta versão do trainer (use --diagnostico para o patch de teste)".

Ligar um cheat `disponível (não verificado)` funciona normalmente (decisão do dono) e escreve no registro de eventos
o aviso de versão sem assinaturas conferidas, na primeira vez que cada cheat é ligado em cada conexão (pela tecla
ou ao religar, 6.2).

### 6.6 Teclas

| Tecla | Ação |
|---|---|
| `↑`/`↓` (ou `k`/`j`) | Mover a seleção |
| `espaço` ou `Enter` | Ligar/desligar o cheat selecionado. Num indisponível, escreve o motivo no registro de eventos e, se ele tiver a marca de religar pendente, tira dos desejados (6.2). Num cheat em `erro`, pede a restauração dos restos (6.8) |
| `←`/`→` (ou `h`/`l`) | Diminuir/aumentar o valor em um `step`, até `min`/`max` |
| `r` | Ver abaixo |
| `q`, `Esc` ou `Ctrl+C` | Sair (6.8), sem pedir confirmação (decisão do dono) |

**`r`**, conforme o estado, na ordem (só a primeira que se aplica):
- SIGCONT que falhou (6.8): tenta o SIGCONT de novo;
- restos de uma falha no motor (6.8): pede ao motor para restaurá-los;
- diário não resolvido (6.1): tenta a recuperação de novo. Se ela resolver tudo e o jogo estiver conectado, refaz a
  conexão (6.2) a partir da abertura do motor;
- `conectado`, sem diário pendente: refaz a busca do catálogo **só para os cheats desligados** (os ligados ficam
  como estão; o padrão deles pode não casar mais, porque o patch está aplicado) e religa os desejados que aparecerem
  (é um pedido do dono pela tecla, e esses cheats foram ligados por ele antes, então não fere os anti-objetivos,
  seção 4);
- `aguardando o jogo`, `sem acesso`, `vários processos`, `erro`: tenta de novo na hora, sem esperar o intervalo;
- `conectando`, `o processo terminou` ou durante uma operação: não faz nada.

No modo cru, Ctrl+C chega como tecla, não como SIGINT; os dois saem do mesmo jeito. Ctrl+Z chega como tecla e é
ignorado. Teclas que chegam durante uma operação do motor (6.10): `q`/`Esc`/Ctrl+C são atendidas logo depois dela;
as setas de valor seguem a regra da 6.7; as demais (espaço, `Enter`, `r`, movimento) são **descartadas**, para que
toques repetidos com o jogo pausado não virem uma sequência de liga-desliga depois. Como os eventos do terminal não
têm horário, "chegou durante a operação" é tudo o que estiver na fila logo depois dela: a TUI esvazia a fila e
classifica cada tecla.

### 6.7 Trocar o valor de um cheat ligado

Decisão do dono: a troca vai para o jogo **na hora**, a cada tecla.

- **Desligado:** a tecla só muda o número na tela. O valor vale quando ligar.
- **Desligar com troca pendente:** o espaço desliga direto; o valor-alvo pendente não vai para o jogo, mas continua
  sendo o valor mostrado e guardado.
- **Registro de eventos:** trocas seguidas do mesmo cheat atualizam uma única linha ("Velocidade: 1.5x → 2.0x"), em vez
  de uma linha por troca. No log em arquivo, cada troca sai em nível `debug`.
- **Ligado:** cada tecla pede ao motor a troca do valor, que é uma operação nova do motor (genérica, no `gt-core`):
  1. Intenção no diário, **antes da pausa**, com os bytes originais, os bytes do valor atual e os do valor novo;
  2. pausa conferida (spec 002 6.5);
  3. com o jogo pausado, reler: os bytes atuais precisam ser os do valor atual. Se não forem, recusa sem escrever,
     retoma e avisa (como o "bytes diferentes" da spec 002 6.2);
  4. escrever os bytes do valor novo, ler de volta, retomar;
  5. diário atualizado só com o valor novo.

  Pelo 6.3, os dois lados têm as mesmas fronteiras de instrução, então não há ponto perigoso na conferência das
  threads (novas tentativas esperadas: zero).
- **Teclas seguidas** (repetição automática do teclado): nunca há mais de uma troca em andamento, e o início de uma
  troca fica a pelo menos ~150 ms do início da anterior (decisão do dono, porque cada troca pausa o jogo ~15 ms,
  `RESEARCH.md`). Uma tecla isolada vai na hora. Teclas que chegam durante uma troca ou dentro do intervalo só
  atualizam o valor-alvo na tela; quando o intervalo passa, se o valor-alvo for diferente do que está no jogo, uma
  nova troca vai direto para ele. O valor intermediário não precisa passar pelo jogo, e o jogo sempre termina com o
  último valor mostrado (salvo falha, abaixo).
- **Diário:** o formato novo (com o valor antigo e o novo durante a troca) precisa continuar lendo diários antigos
  (dois conjuntos de bytes): `recover`, `restore` e `gt-patch list` funcionam com um diário gravado pela Fase 2. A
  identidade de uma entrada (usada para esquecer ou manter entradas) muda com o valor: o plano garante que a troca
  substitui a entrada do patch, sem deixar a antiga para trás. Campo canônico: `replacement` é sempre o valor que
  estava no jogo antes da troca, e um campo opcional novo guarda o valor novo só durante a troca. O estado
  `ativo`/`pendente` do `gt-patch list` (spec 002 6.4) aceita os bytes de um ou de outro.
- **Recuperação** (spec 002 6.8) de um diário com uma troca interrompida: cada byte precisa ser igual ao original, ao
  do valor antigo ou ao do valor novo naquela posição para restaurar os originais; qualquer outro byte segue a regra
  de "bytes de outro" (não escreve, avisa).
- **Troca que falha**, conforme o caso (o motivo sempre vai para o registro de eventos e o valor-alvo pendente é
  descartado):
  - **recusada sem escrever** por pausa (threads no trecho, prazo): os bytes do valor atual continuam no jogo, o
    cheat continua `[ON ]` com o valor atual, e a tela volta a mostrá-lo;
  - **bytes atuais diferentes dos do valor atual** (alguém mexeu depois de nós): o patch deixa de ser nosso. O motor
    o tira do registro e do diário sem escrever (como no desfazer da spec 002 6.2), o cheat aparece como
    `indisponível (alterado por outro)` e sai dos desejados;
  - **escrita interrompida ou SIGCONT que falhou** (o motor fica com restos): vale a 6.8; o cheat aparece como
    `erro` e sai dos desejados.

**Desligar que falha** segue as mesmas regras: recusa limpa por pausa deixa o cheat `[ON ]`, ainda desejado, com o
motivo no registro de eventos (o dono pode tentar de novo); bytes alterados por outro tiram o patch do registro sem
escrever (spec 002 6.2), e o cheat aparece como `indisponível (alterado por outro)`, fora dos desejados; restos levam
a `erro` (6.8), fora dos desejados.

### 6.8 Saída e restauração

Saídas: `q`/`Esc`/Ctrl+C (6.6), SIGINT/SIGTERM/SIGHUP (terminal fechado)/SIGQUIT e pânico. Em todas:

1. A TUI para de aceitar teclas, desenha "restaurando…" no topo (o desfazer segue a regra das operações do motor,
   6.10) e desfaz todos os patches (`undo_all`, spec 002 6.7). Sinais repetidos não interrompem a restauração. A
   saída não espera uma busca em andamento (6.10).
2. Restaura o terminal (sai do modo cru e da tela alternativa, mostra o cursor), **uma vez só**: uma marca
   compartilhada diz que o terminal já foi restaurado, e depois dela desenhar não faz nada e a restauração normal
   pula esse passo.

   **Pânico:** na thread principal, o gancho de pânico restaura o terminal (pela marca) **antes** de imprimir a
   mensagem, e o desenrolar chega ao `Drop` do motor, que desfaz (spec 002 6.7); o `main` captura o pânico
   (`catch_unwind` ou equivalente), imprime o resumo (passo 3) e sai com código diferente de 0. Na thread da busca, o
   gancho só registra o pânico e não mexe no terminal; a thread principal percebe e sai pelo caminho normal (passos 1
   a 3), também com código diferente de 0.
3. Já no terminal normal, imprime o resumo: o que restaurou e, se algo não pôde ser desfeito, a mensagem da spec 002
   6.7 ("a próxima execução vai restaurar", citando `gt-tui` e `gt-patch restore`). Se o marcador "pausado por nós"
   continuar no diário (SIGCONT que falhou, abaixo), o resumo diz "o jogo pode ter ficado parado: kill -CONT <pid>",
   e o código de saída é diferente de 0.

Se o terminal já foi fechado (SIGHUP), restaurá-lo e imprimir o resumo podem falhar (`EIO`): essas falhas são
ignoradas (o resumo vai também para o log em arquivo) e não mudam a restauração do jogo nem o código de saída.

Código de saída 0 quando tudo foi restaurado (inclusive quando nada estava ligado, e quando o jogo já tinha fechado),
diferente de 0 quando algo ficou no diário ou a TUI terminou por pânico. "Algo ficou no diário" é decidido depois do
desfazer e do `drop` do motor, lendo o diário de novo (o `Drop` só registra no log o que não conseguiu restaurar); o
resumo do passo 3 sai dessa mesma leitura.

Um erro na leitura do terminal (`EIO`, fim de arquivo), sem SIGHUP, segue o mesmo caminho de saída.

**Restos de uma falha.** Quando ligar, desligar ou trocar valor falha com escrita interrompida no meio, o motor
guarda a entrada como pendente, no diário (spec 002 6.7); o trecho fica bloqueado para novos patches (os outros
cheats continuam podendo ser ligados, se não se sobrepõem a ele). O cheat aparece
como `erro` (6.4) e o aviso fixo diz que há restos a restaurar. O `r` (ou `espaço` no cheat) pede ao motor para
restaurar as entradas pendentes (como na recuperação, spec 002 6.8); resolvidas, o cheat volta a `[OFF]`. Na saída, o
`Drop` do motor já tenta restaurá-las; o que sobrar fica para a próxima execução (passo 3 acima).

**SIGCONT que falhou** (seção 7, item 1): o aviso fixo diz que o jogo pode ter ficado parado e mostra
`kill -CONT <pid>`. O motor expõe esse estado para a TUI. Com ele, toda operação seguinte (inclusive o desfazer da
saída e o `Drop`, mesmo sem nenhum patch ativo) tenta o SIGCONT **antes** de qualquer outra coisa (seção 7, item 1); se essa nova tentativa falhar, a operação é
recusada com o erro e o aviso continua. Com a TUI ociosa, nada tenta sozinho: o `r` tenta o SIGCONT de novo. Quando
um SIGCONT dá certo (ou o jogo fecha), o aviso some.

### 6.9 Log em arquivo

Enquanto a TUI estiver desenhada, nada vai para `stdout`/`stderr`. O `tracing` grava em
`$XDG_STATE_HOME/grounded-trainer/gt-tui.log` (o mesmo diretório do diário), nível controlado por `RUST_LOG`
(padrão `info`). A cada início, o log anterior vira `gt-tui.log.1` (só uma geração). Se o arquivo não puder ser
aberto, a TUI segue sem log em arquivo e diz isso no registro de eventos.

### 6.10 Responsividade

A TUI redesenha ao menos algumas vezes por segundo e responde às teclas enquanto espera o jogo.

**Limite de resposta:** com ou sem busca em andamento, a tela redesenha e uma tecla é atendida em até ~250 ms, salvo
durante uma operação do motor (abaixo).

**Busca dos padrões** (abrir o alvo e varrer a `.text` de ~128 MB, na conexão e nas novas buscas): roda numa
**thread separada**, porque a varredura do `gt-core` é uma chamada só, sem retomada, e o tempo dela no jogo ainda não
foi medido. A busca só lê (não pausa nem escreve), e o motor fica sempre na thread principal. Enquanto ela roda, o
topo mostra "procurando padrões", a tela redesenha e as teclas são atendidas (ligar e desligar os cheats já
resolvidos continuam valendo durante uma nova busca). Se `q` ou um sinal chegarem durante uma busca, a saída **não
espera**: o desfazer acontece na hora, na thread principal, e a busca é abandonada (o resultado dela é descartado).
Um pânico na thread da busca é tratado como um pânico do `gt-tui` (6.8): a thread principal percebe e sai pelo
caminho de restauração.

**Operações do motor** (ligar, desligar, trocar valor, religar, restaurar restos): rodam no laço principal, uma por
vez. O motor não pode ir para outra thread sem mudar a política de escrita dele (não é `Send`), e a pausa típica é de
~15 ms (`RESEARCH.md`). No pior caso (todas as tentativas de pausa estourando o prazo, spec 002 6.5) uma operação
leva alguns segundos: por isso, **antes** de chamar o motor, a TUI desenha o que vai fazer (ex.: "ligando Cargas
infinitas…" no topo), e essa mensagem fica na tela até a operação terminar. Teclas e sinais que chegam nesse tempo
são tratados conforme a 6.6; um sinal de encerramento nunca é perdido. Cada operação grava o diário duas vezes com
`fsync` (intenção e final), fora da pausa (spec 002 6.2); num disco lento isso soma tempo à operação, mas não à pausa
do jogo.

### 6.11 Erros obrigatórios

Mensagens claras, na tela ou na saída, para: sem terminal (6.1), outra instância segurando a trava, sem permissão
(`ptrace_scope`, com o nome `gt-tui`), jogo não encontrado (estado `aguardando o jogo`), patch indisponível (com o
motivo), bytes diferentes do esperado (mostrando os encontrados), threads que não saem do trecho, jogo que não pausa no
prazo, jogo fechou durante a operação, SIGCONT que falhou (6.8), diário com entradas não restauradas (6.1), troca de
valor recusada ou com bytes alterados por outro (6.7), restos de uma falha a restaurar (6.8), vários processos do
jogo (6.2), processo do `--pid` que terminou (6.1), terminal pequeno (6.4), log em arquivo indisponível (6.9).

## 7. Pendências da Fase 2 incluídas

1. **SIGCONT que falha sem ESRCH** (corrigir primeiro). Hoje a falha volta com `stuck` e o diário guarda o marcador
   "pausado por nós" naquela gravação, mas o motor não lembra disso: a próxima gravação (o `undo_all` ou o `Drop`)
   grava o diário com o marcador limpo, e o jogo pode ficar parado sem recuperação. Correção: o motor guarda o
   estado `stuck` até um SIGCONT bem-sucedido (ou o jogo fechar). Enquanto ele valer, **toda** gravação do diário
   mantém o marcador, e a próxima operação do motor (inclusive o `undo_all` e o `Drop`) tenta o SIGCONT antes de
   qualquer outra coisa. Essa tentativa vem antes até da conferência "o alvo já está parado?" (`is_stopped_now`):
   um alvo que nós deixamos parado pareceria "parado por outro", e o motor não mandaria SIGCONT nem gravaria o
   marcador. Se a nova tentativa falhar, a operação é recusada com o erro. O motor expõe o estado para quem o usa
   (6.8). Testes com uma falha de SIGCONT provocada por um gancho só de teste no envio do SIGCONT, em dois modos:
   **persistente** (o marcador continua no diário depois do `undo_all` e do `Drop`, e a recuperação seguinte retoma
   o alvo) e **de uma vez só** (a nova tentativa da operação seguinte retoma o alvo e limpa o marcador). A nova
   tentativa é um passo próprio do motor, chamado no início de toda operação **antes** dos retornos antecipados de
   hoje (ex.: o `undo_all` sem nada a desfazer devolve cedo, e o `Drop` pula o `undo_all` quando não há patch ativo),
   e também pelo `Drop`, mesmo sem patch ativo nem restos. Sem `stuck`, esse passo não faz nada (não envia sinal):
   um jogo parado por outro (spec 002 6.5, item 1) continua parado. Quando a nova tentativa dá certo, o diário é
   regravado sem o marcador, mesmo sem nenhuma entrada (o arquivo some, como hoje com o diário vazio). O teste persistente inclui o caso `stuck` sem nenhum patch ativo (só
   restos ou nada). Isso pesa
   mais na TUI, que fica aberta por horas.
2. **`gt-patch profile` sem conferir a identidade:** o `open_checked` passa a ser comparado com o `target.identity`
   do `Target::open`; diferente, recusa com "o jogo reiniciou". Teste unitário da comparação.
3. **Testes `#[ignore]` de `apps/gt-patch/tests/game.rs`:** rodam na sessão com o jogo do critério de pronto
   (seção 9, item 6), junto com os novos testes de jogo desta fase. Se algum falhar, a causa vai para o
   `RESEARCH.md` e a IA volta ao dono antes de fechar a fase.
4. **Aviso repetido ao reabrir o alvo:** cada `Target::open` gera um `tracing::warn!` por módulo confiável com
   disco ilegível ("extensão do módulo não conferida com o disco", `gt-core/src/modules.rs`). Na TUI, que reabre o
   alvo a cada reconexão, o aviso passa a sair como `warn` só na primeira vez por caminho de módulo em cada execução
   do programa, e como `debug` nas seguintes. O mesmo vale para o aviso de `SizeOfImage` diferente entre memória e
   disco, também por módulo. A decisão "já avisei este caminho?" é uma função pura sobre um conjunto passado por quem
   chama, testada sem estado global; onde o conjunto mora (ex.: um estático no `gt-core`) fica para o plano.
5. **Itens pequenos** (corrigir quando o código for tocado, como na Fase 2):
   - `permission_hint` passa a receber o nome do programa, que substitui "o programa" no texto da dica (a TUI usa a
     dica; item obrigatório nesta fase). Muda a
     chamada em `apps/gt-cli/src/main.rs` e as do `gt-patch`;
   - a mensagem da trava ocupada (`gt-core/src/error.rs`) passa a citar o `gt-patch` e o `gt-tui` (6.1); os testes
     que conferem o texto (`apps/gt-patch/tests/cli.rs`, `crates/gt-testkit/tests/harness.rs`) são atualizados;
   - `entry_range` duplicado em `recovery`/`engine` (a troca de valor mexe nos dois; item obrigatório nesta fase);
   - `restore_entries` passa a documentar `outside_policy`;
   - demais itens da memória do projeto (`WritePolicy` fundindo seções contíguas, `+` no hex do diário,
     `RENAME_NOREPLACE` em NFS, lacunas de teste da Fase 2) só se o plano tocar no código deles.

## 8. Testes

| Nível | O que cobre | Exige jogo? |
|---|---|---|
| Unitário (valores, `gt-core`) | Grade por `k` sem acúmulo; ponto mais próximo da grade (valor guardado numa versão nova, 6.2); bytes de `i32`, `u32` e `f32`; limites `min`/`max` | Não |
| Unitário (assinaturas) | Carga de `name`, `category` e `value` com todos os erros da 6.3 (campo faltando, categoria inválida, `min > max`, `default` fora da grade, `slot` fora de `replacement`, `slot` fora de um imediato, imediato de tamanho diferente do tipo, imediato de desvio relativo, `u32` acima de `i32::MAX` em imediato estendido com sinal, bytes do `slot` diferentes do `default`, inteiro e decimal no TOML para `f32`); carga dos arquivos reais; `ValueSpec` montado a partir do TOML; todo valor da grade de um patch sintético decodifica nas mesmas instruções | Não |
| Unitário (núcleo da TUI) | Teclas → ações, inclusive o `r` em cada estado (6.6); seleção pulando cabeçalhos; estado da spec 002 6.4 → linha da tela; conjunto de desejados (entra ao ligar com sucesso, sai ao desligar e com espaço num indisponível marcado, não entra quando a ligação falha, sobrevive à reconexão, preservado quando o jogo fecha no meio de uma operação); religar em lote e, se recusado, um a um; novas buscas automáticas depois da reconexão (param quando não há desejado indisponível ou depois do prazo); fila de troca de valor (uma em andamento, intervalo mínimo, alvo pendente, valor intermediário pulado, termina no último valor); cada caso de falha da troca (6.7); limites `min`/`max`; bloqueio das teclas de ligar com diário não resolvido (com `kept` e com erro), com restos e durante `conectando`; `--pid` levando a `o processo terminou`; `vários processos`; `conectado` sem motor (disco ilegível); aviso de não verificado uma vez por cheat por conexão; modo de diagnóstico escondendo a categoria e fora da contagem do topo; código de saída de cada cenário da 6.8 | Não |
| Unitário (tela) | Desenho com o `TestBackend` do `ratatui` para: `aguardando o jogo`, `sem acesso`, `vários processos`, `o processo terminou`, `conectado` com cheats `ON`/`OFF`/`?`/indisponível/`erro`/`↻`/valor, mensagem de operação em andamento, lista vazia sem `--diagnostico`, aviso fixo, terminal pequeno | Não |
| Unitário (motor) | Diário da Fase 2 (sem o campo novo) lido pelo formato novo; `list` com um diário no meio de uma troca mostra `ativo`/`pendente`; recuperação de troca de valor interrompida (bytes original, antigo, novo e mistura byte a byte; byte de outro); `stuck` mantido em todas as gravações seguintes até um SIGCONT bem-sucedido, e a nova tentativa antes da conferência de alvo parado; comparação de identidade do `profile`; desduplicação dos dois avisos | Não |
| Integração (motor) | Contra o alvo de teste, na instrução com imediato da página executável dele (5, `gt-testkit`): troca de valor com o patch ligado; recusa quando os bytes atuais não são os do valor atual (o patch sai do registro sem escrever); troca de valor alternada 1000 vezes com uma thread do alvo executando a instrução (o alvo não cai e o laço continua); SIGKILL durante a pausa da troca (gancho que dispara nessa pausa), seguido de recuperação, restaura os originais; SIGCONT que falha (gancho de teste), persistente e de uma vez só (seção 7, item 1); jogo que fecha no meio de uma troca → `ProcessGone` sem escrita; o valor novo é o que o código executa (a palavra de dados gravada pelo laço do alvo muda para ele); escrita que falha no meio com o desfazer também falhando (gancho) deixa restos visíveis pela consulta de pendentes, o trecho fica bloqueado para novos patches, e o pedido de restaurar os pendentes devolve os originais; desfechos distintos de ligar, desligar e trocar valor (recusa limpa, bytes de outro, restos) | Não |
| Integração (núcleo da TUI) | Pela implementação de teste da interface de alvo (5): conectar, ligar e desligar; trocar valor ligado; alvo que termina → "o jogo fechou" sem escrita, inclusive no meio de uma operação; alvo novo → reconexão e religação dos desejados com o valor guardado; desejado indisponível na reconexão fica desejado e indisponível; `r` com diário bloqueado; pedido de encerramento (como o `Shutdown` da Fase 2) desfaz tudo; pânico provocado (gancho só de teste), no laço principal e na thread da busca se houver, desfaz tudo; intenção do dono nos desejados quando o jogo fecha no meio de ligar, desligar e trocar valor (6.2); fechamento percebido com a TUI ociosa; nova busca que reabre o alvo e só vale para os cheats ainda desligados; `conectado` sem motor que abre o motor quando o `r` resolve o diário; código de saída decidido pela releitura do diário | Não |
| Unitário (terminal e log) | Pela interface de terminal falsa: o terminal é restaurado antes de qualquer impressão, na saída normal e no pânico (ordem das chamadas gravada pelo falso); falha ao restaurar ou imprimir (`EIO`) não muda a restauração do jogo nem o código de saída. Log: rotação para `gt-tui.log.1` (uma geração) e TUI seguindo sem log, com o aviso no registro de eventos, quando o arquivo não abre | Não |
| Integração (núcleo, tempo e recursos) | Pela interface de alvo falsa com busca lenta: resultado de busca de uma conexão antiga descartado, e de um cheat que passou por operação durante a busca também; a tela redesenha e as teclas respondem durante a busca; `q` e pedido de encerramento durante uma busca desfazem na hora, sem esperar a busca; pânico na thread da busca sai pelo caminho de restauração. A mensagem da operação já está no último quadro desenhado quando a interface falsa recebe o pedido de ligar (prova a ordem "desenha, depois chama o motor"). Teclas de ação que chegaram durante uma operação são descartadas e `q` não. Conectar sem desejados nunca chama "ligar" na interface falsa; reconectar numa versão diferente não religa e deixa a marca. Nova busca que não acha um cheat já resolvido mantém o estado dele. Rolagem: a seleção fora da janela fica visível (também no `TestBackend`). Contra o alvo de teste: 50 conexões e reconexões seguidas não aumentam o número de descritores abertos (`/proc/self/fd`), para a TUI aberta por horas | Não |
| Integração (binário) | Com `XDG_STATE_HOME` temporário e pulando quando o jogo está aberto (como o `game_is_open()` de `apps/gt-patch/tests/cli.rs`): `gt-tui` sem terminal sai com erro e sem escrever; com a trava ocupada, sai com a mensagem; `gt-tui` num pseudo-terminal sem jogo mostra `aguardando o jogo` e sai com código 0 em SIGTERM, SIGHUP (com o pseudo-terminal já fechado: sem pânico pelo `EIO`) e SIGQUIT, com o terminal restaurado, e com o `stderr` num pipe que termina vazio (nada escrito fora da TUI). Se um pseudo-terminal não for viável no `cargo test`, o plano diz o que o substitui para cobrir sinais e restauração do terminal no binário de verdade | Não |
| Jogo (`#[ignore]`) | Os de `apps/gt-patch/tests/game.rs` (seção 7, item 3); implementação do jogo da interface de alvo: conecta, `teste-codificacao` aparece `disponível (verificado)` com `--diagnostico`, liga e desliga conferindo os bytes; reabrir o alvo e refazer a busca 50 vezes sem fechar o jogo não aumenta o número de descritores abertos (`/proc/self/fd`). Testes com bytes de uma versão só rodam com a chave esperada | Sim |

`cargo fmt --check`, `clippy -D warnings` e `cargo test --workspace` passam (`AGENTS.md` 6). `cargo build -p gt-cli`
isolado continua compilando sem a feature `write`.

## 9. Critério de pronto

Uma sessão com o jogo. Antes dela, **o dono faz backup do save** (pedido pela IA). Jogo num save de teste, FLiNG
fechado. Os passos usam `gt-tui --diagnostico` e, num segundo terminal, `gt diff` para conferir o que está escrito.

1. `gt-tui --diagnostico` com o jogo **fechado**: mostra `aguardando o jogo`. O dono abre o jogo: a TUI conecta
   sozinha, mostra a versão `com assinaturas` e `teste-codificacao` como `[OFF]`.
2. Espaço liga: `[ON ]` e a pausa no registro de eventos; `gt diff` mostra o patch; `gt-patch list` mostra `ativo`.
   Espaço de novo: `[OFF]` e `gt diff` mostra "nenhuma alteração".
3. Ligado, `q`: a TUI sai dizendo o que restaurou, o terminal volta ao normal e `gt diff` mostra "nenhuma
   alteração". O mesmo fechando a janela do terminal (SIGHUP) e com `kill <pid>` (SIGTERM).
4. Ligado, `kill -9 <pid>` da TUI: `gt diff` mostra o patch; abrir o `gt-tui` de novo restaura sozinho (o registro de
   eventos mostra "restaurado: teste-codificacao") e o `gt diff` volta a "nenhuma alteração".
5. Ligado, o dono fecha o jogo: a TUI mostra "o jogo fechou" e `aguardando o jogo`, sem erro de escrita. O dono abre o
   jogo de novo: a TUI reconecta e religa `teste-codificacao` sozinha (talvez só depois de uma das novas buscas
   automáticas, se o jogo ainda estiver carregando, 6.2); `gt diff` mostra o patch. `q` restaura. O momento em que
   ela religou (tela de carregamento, menu ou partida) vai para o `RESEARCH.md`, junto com o tempo medido de uma
   busca do catálogo no jogo (6.2).
6. Os testes `#[ignore]` (seção 8) passam com o jogo aberto.
7. O dono usa a TUI por alguns minutos e dá a opinião sobre usabilidade e layout (ROADMAP). Ajustes pequenos de
   texto e layout entram antes de fechar a fase; mudanças de comportamento viram emenda a esta spec.

O controle de valor só é provado pelos testes contra o alvo de teste (seção 8), porque ainda não há cheat com valor no
jogo; a prova no jogo fica para a fase do primeiro valor real.

**O dono faz:** backup do save; abrir e fechar o jogo quando eu pedir; dizer se o jogo travou, engasgou ou caiu;
opinar sobre a TUI.

## 10. Documentos a atualizar

- `ARCHITECTURE.md`: 3 (o `gt-tui` com a feature `write` e o núcleo da TUI), 5.3 (campos `name`, `category` e
  `value` das assinaturas), 5.4 (troca de valor e o `stuck` mantido), 5.7 (tela, teclas, log em arquivo, reconexão),
  8 (decisões desta spec, incluindo a mudança "único aplicativo com `write`").
- `ROADMAP.md`: status da Fase 3 ao terminar, com a nota de que o controle de valor foi provado só contra o alvo de
  teste.
- `RESEARCH.md`: resultado dos testes `#[ignore]` com o jogo e qualquer comportamento novo observado (ex.: reconexão
  com o jogo recém-aberto).
- Spec 002: nota na 6.8 sobre a recuperação de troca de valor (três conjuntos de bytes) e na 6.11 sobre a mensagem
  da trava citando o `gt-tui`.

## 11. Decisões tomadas

| Decisão | Quem decidiu |
|---|---|
| Motor no próprio processo do `gt-tui` (feature `write`), em vez de chamar o `gt-patch` | Dono |
| Modelo e controle de valor nesta fase, provados contra o alvo de teste | Dono |
| Depois de reconectar, religar sozinho os cheats que estavam ligados | Dono |
| Sempre começa tudo `OFF`; nada é lembrado entre sessões | Dono |
| Lista única agrupada, status no topo, registro de eventos embaixo | Dono |
| Troca de valor vai para o jogo a cada tecla | Dono |
| Intervalo mínimo de ~150 ms entre o início de duas trocas de valor, terminando no último valor mostrado | Dono |
| Não verificado liga normal, com marca `?` e aviso | Dono |
| `q` desliga tudo e sai sem confirmação | Dono |
| Patch de teste só com `--diagnostico` | Dono |
| Valor só em imediato de uma instrução, com o tamanho exato dele; tipos `i32`/`u32`/`f32` | IA |
| Troca de valor numa pausa só, com três conjuntos de bytes no diário e na recuperação | IA |
| No máximo uma troca de valor em andamento e um alvo pendente | IA |
| Trava do diário com a TUI do início ao fim; outra instância recusa; `list` funciona em paralelo | IA |
| Recuperação no início e a cada conexão; diário não resolvido bloqueia as escritas | IA |
| Cheat que falha ao ligar não entra nos desejados; desejado indisponível na reconexão continua desejado | IA |
| Religar em lote e, se recusado, um a um | IA |
| `name` e `category` obrigatórios nas assinaturas; ordem fixa das categorias | IA |
| Log só em arquivo enquanto a TUI está aberta, com uma geração anterior | IA |
| Núcleo da TUI testável por uma interface de alvo com implementação de teste | IA |
| `stuck` mantido até um SIGCONT bem-sucedido, com nova tentativa antes de cada operação | IA |
| Aviso de extensão não conferida uma vez por módulo por execução | IA |
| `--pid` fixa o processo; quando ele termina, estado final `o processo terminou`, sem reconectar | IA |
| Diário não resolvido (`kept` ou erro da recuperação) bloqueia as escritas sem fechar a TUI; `r` tenta de novo | IA |
| Log em arquivo iniciado antes da recuperação | IA |
| Disco ilegível = `conectado` sem motor, todos `indisponível (disco ilegível)` | IA |
| Novas buscas automáticas (~5 s, até ~2 min) depois de reconectar, enquanto houver desejado indisponível | IA |
| Marca `↻` para desejado não ligado; espaço nele o tira dos desejados | IA |
| Operações do motor no laço principal, com a mensagem desenhada antes de chamar o motor | IA |
| Estados depois de uma troca de valor que falha (6.7) e o estado `erro` para restos | IA |
| `r` com prioridade: SIGCONT, restos, diário, nova busca ou nova tentativa de conexão | IA |
| Aviso de não verificado uma vez por cheat por conexão | IA |
| Valor só fora de desvios relativos; `u32` estendido com sinal limitado a `i32::MAX` | IA |
| APIs novas do motor: desfechos distintos, consulta e restauração de restos, estado do SIGCONT, descarte quando o jogo fecha | IA |
| Restaurar e retomar sem tecla são permitidos; só aplicar exige tecla (ou religar) | IA |
| Intenção do dono decide os desejados quando o jogo fecha no meio de uma operação | IA |
| Nova busca reabre o alvo e só vale para cheats ainda desligados; motor aberto assim que diário e disco estiverem resolvidos | IA |
| Tela desenhada antes da recuperação inicial, com mensagem | IA |
| Desligar que falha segue as regras da troca de valor | IA |
| Teclas de ação que chegam durante uma operação do motor são descartadas | IA |
| Código de saída e resumo decididos relendo o diário depois do `drop` do motor | IA |
| Interface de terminal com implementação falsa para testar a restauração do terminal | IA |
| Bloco `value` sobrescrito inteiro por versão; `i32`/`u32` só com inteiros no TOML | IA |
| Mensagem da trava: "outro gt-patch ou gt-tui já está em execução" | IA |
| Busca dos padrões em thread separada; a saída não espera a busca | IA |
| `pidfd` próprio da TUI para perceber o fechamento, com ou sem motor | IA |
| Novas buscas automáticas depois de toda conexão, também sem desejados | IA |
| Religação automática só na mesma chave de versão; versão nova exige espaço | IA |
| Nova busca que não acha um cheat já resolvido mantém o estado dele | IA |
| Resumo de saída com `kill -CONT` quando o marcador persiste; "restaurando…" antes do desfazer | IA |
| Nova tentativa de SIGCONT como passo próprio do motor, antes dos retornos antecipados | IA |
| Rolagem da lista com a seleção sempre visível | IA |
| Diário novo lê diários da Fase 2 | IA |
| Implementação de teste da interface de alvo atrás de feature só de teste | IA |
| `ValueSpec`, grade e montagem dos bytes num módulo genérico do `gt-core` | IA |
| Terminal restaurado uma vez só, por marca compartilhada; pânico fora da thread principal não mexe no terminal | IA |
| Resultado de busca descartado se a conexão mudou ou se o cheat passou por operação durante ela | IA |
| Nova tentativa de SIGCONT não faz nada sem `stuck`; chamada própria para o `r` | IA |
| Resposta da tela em até ~250 ms; cadência das buscas automáticas ajustada pela duração medida | IA |
| Trava antes da conferência do terminal | IA |
| `replacement` canônico no diário, com o valor novo num campo opcional durante a troca | IA |
| Diário corrompido vira aviso fixo | IA |

## 12. Deixado para quem implementa

- Organização interna do núcleo da TUI e o formato da interface de alvo.
- Intervalos de procura do jogo, taxa de redesenho, tamanho mínimo do terminal, quantas linhas o registro de eventos
  mostra.
- Se a busca dos padrões roda numa thread separada ou em partes (6.10).
- Formato exato do bloco `value` no TOML e da troca de valor no diário, desde que a recuperação da 6.7 seja possível.
- Como provocar a falha de SIGCONT, o pânico e o SIGKILL na pausa da troca de valor nos testes (ganchos só de teste,
  sem flag oculta no binário).
- Como o `f32` é guardado nas definições sem quebrar a igualdade delas (ex.: pelos bits ou pelo índice `k`).
- Intervalos exatos das novas buscas automáticas depois de reconectar e do limite entre trocas de valor (em torno dos
  valores da spec).
- Cores e estilo da tela, mantendo o layout aprovado.
- Se o pseudo-terminal do teste do binário é viável e, se não for, o que o substitui.
