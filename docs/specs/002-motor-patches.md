# Spec 002 — Motor de patches seguro

| Campo | Valor |
|---|---|
| Fase | 2 (ver `docs/ROADMAP.md`) |
| Estado | Aprovada pelo dono em 2026-10-25 |
| Data | 2026-10-25 |

## 1. Intenção

Alterar o código do Grounded 2 em memória de forma **reversível e à prova de crash**. A Fase 1 só lia o
jogo; esta fase cria o motor que todos os cheats das Fases 3–6 vão usar para escrever: confere os bytes antes,
pausa o jogo durante a escrita, guarda os originais e desfaz tudo ao sair, aconteça o que acontecer.

Não há cheat nesta fase. O motor é provado com um **patch de teste sem efeito na partida** (6.9), ligado e
desligado 100 vezes com o jogo rodando. Quem aciona o motor é um binário novo, `gt-patch` (6.10). O `gt` da
Fase 1 continua somente leitura.

## 2. Objetivos

1. Motor de patches genérico no `gt-core` (não sabe nada do Grounded 2): patch com bytes esperados e bytes
   originais, pausa com conferência das threads, registro de patches ativos com restauração garantida e diário
   em disco.
2. Assinaturas por versão do jogo em `signatures/*.toml` e cálculo do estado de cada patch (6.4).
3. Binário `gt-patch` para listar, aplicar, alternar e restaurar patches no jogo.
4. Fechar a pendência do `SizeOfImage` (6.12) e os itens pequenos deixados pela Fase 1 (seção 7).

## 3. Fora do objetivo

- Qualquer cheat real (Fases 4–6). O único patch desta fase é o de teste.
- Code caves e desvios para código novo (Fase 6). Um patch aqui só **troca bytes por bytes do mesmo tamanho**.
- Congelar valores em dados (`Freeze`, Fase 5). O motor só escreve em código (6.6).
- TUI (Fase 3), hotkeys globais e saída `--json` no `gt-patch`.
- Processo guardião contra SIGKILL: decisão do dono pelo diário em disco (6.8).

## 4. Anti-objetivos (falha mesmo que "funcione")

- Escrever em um endereço cujos bytes atuais não sejam exatamente os esperados.
- Deixar o jogo pausado (SIGSTOP) depois que o `gt-patch` terminar ou falhar, salvo SIGKILL no meio da pausa
  (limitação conhecida, 6.5).
- Uma thread do jogo voltar a rodar no meio de uma instrução que trocamos.
- Restaurar bytes por cima de algo que não é nosso: se os bytes no endereço não forem os que escrevemos, não
  tocar.
- Escrever num processo que não é mais o jogo que abrimos (PID reaproveitado depois que o jogo fechou).
- Escrever fora de uma seção executável de um módulo confiável.
- Endereços fixos do jogo no código de produção (`AGENTS.md` 4): só em `signatures/*.toml`, documentos e testes
  `#[ignore]`.
- Alterar arquivos do jogo em disco, saves ou configurações do sistema.

## 5. Onde cada coisa mora

| Crate | Conteúdo desta fase |
|---|---|
| `gt-core` | `patch.rs` (patch, validação, registro), `suspend.rs` (pausa e conferência das threads), diário (6.8), identidade do processo (6.7). Tudo genérico: a política de onde se pode escrever entra como parâmetro |
| `gt-grounded2` | `signatures/` (arquivos TOML) e o leitor deles, cálculo do estado de cada patch (6.4), a política de escrita do jogo (6.6) e a resolução do alvo hoje em `gt-cli/src/context.rs`, movida para cá para ser usada pelo `gt`, pelo `gt-patch` e, na Fase 3, pela TUI |
| `apps/gt-patch` (novo) | Binário `gt-patch` (6.10). Único crate de aplicativo que ativa a feature `write` |
| `gt-cli` | Passa a usar a resolução do alvo do `gt-grounded2`; correções da seção 7. Continua sem a feature `write` |

**Feature `write` em cascata:** o `gt-grounded2` ganha uma feature `write` que repassa a `gt-core/write` e esconde
tudo o que escreve ou pausa (aplicar, laço, recuperação, perfil). O que só lê (assinaturas, estado da 6.4, resolução
do alvo, extensão da 6.12) fica fora dela, para o `gt-cli` usar sem compilar o escritor. A resolução do alvo, ao ir
do `gt-cli` para o `gt-grounded2`, troca `anyhow` por erros `thiserror` (`AGENTS.md` 5).

A regra de dependência do `ARCHITECTURE.md` 3 vale (`gt-patch` fica no mesmo nível de `gt-cli`/`gt-tui`). Partes
puras (validação de patch, leitura do TOML, cálculo de estado, decisão de "PC dentro do trecho", decisão de
recuperação do diário) são testáveis sem o jogo e sem processo.

## 6. Comportamento

### 6.1 Patch

Um patch é: identificador, endereço absoluto, **bytes esperados** e **bytes novos**, do mesmo tamanho
(1 a 64 bytes). Ao aplicar, os bytes originais lidos ficam guardados no registro (6.7).

Validação antes de qualquer escrita (função pura, sobre os bytes esperados e os novos, com `iced-x86`):
- tamanhos iguais e dentro do limite;
- os bytes esperados se decodificam em **instruções inteiras** que terminam exatamente no fim do trecho; os bytes
  novos também;
- **endereços de retorno, nos dois sentidos:** se um dos lados (esperados ou novos) tem um `call` que não é a última
  instrução, o endereço logo depois dele precisa ser início de instrução **no outro lado** também. Uma thread que
  está dentro dessa chamada volta para esse ponto depois de aplicarmos (original → novo) ou de desfazermos
  (novo → original), e a pausa (6.5) não enxerga a pilha.

Patch que falha na validação é recusado com o motivo.

**Fronteiras de instrução.** Chamamos de *fronteiras perdidas* os inícios de instrução de um lado que não são início
de instrução do outro (ex.: trocar duas instruções de 3 bytes por uma de 6). São os únicos pontos onde uma thread
parada pode cair no meio de uma instrução; a conferência da 6.5 vigia exatamente eles. Um patch com as mesmas
fronteiras nos dois lados (como o de teste, 6.9) não tem fronteira perdida.

Patches com fronteiras perdidas são permitidos, porque os cheats das próximas fases precisam deles (ex.: `nop` sobre
duas instruções). **Limitação conhecida:** um salto vindo de fora do trecho que caia numa fronteira perdida não é
detectável pelo motor. O mesmo vale para uma thread que estava dentro de um tratador de sinal ou de exceção do Wine:
o ponto interrompido fica guardado no quadro de sinal ou no `CONTEXT`, fora da vista do `/proc`. Escolher um local sem esse risco é responsabilidade da pesquisa que cria a assinatura, e
fica registrado no `RESEARCH.md`.

### 6.2 Aplicar e desfazer

**Aplicar** (para um lote de um ou mais patches):
1. Conferir a identidade do processo (6.7) e a política de escrita (6.6).
2. Ler os bytes atuais de cada patch (sem pausa). Se algum não for igual aos esperados, recusar o lote e informar
   o patch e os bytes encontrados (ex.: outro trainer já alterou o local, ou a versão é outra).
3. Verificar se o jogo já está parado por outro (6.5 item 1). **Gravar no diário (6.8), antes de pausar**, a
   intenção do lote (originais e novos de cada patch) e, **só se formos enviar SIGSTOP**, o marcador "pausado por
   nós". O marcador fica gravado durante todas as novas tentativas da 6.5 e só é limpo depois do SIGCONT final.
   Toda gravação do diário (com `fsync`) acontece **fora** da pausa, para a pausa não depender do disco.
4. Pausar (6.5).
5. **Com o jogo pausado**, reler os bytes atuais. Se mudaram desde o passo 2, recusar o lote, retomar e seguir para
   o passo 8 limpando a intenção.
6. Escrever os bytes novos com o escritor da Fase 1 (`/proc/<pid>/mem`) e **ler de volta** para confirmar. Se a
   escrita ou a releitura falhar no meio do lote, os patches já escritos do lote são desfeitos ainda com o jogo
   pausado.
7. Retomar o jogo (6.5).
8. Atualizar o diário: patches do lote marcados como ativos (ou removidos, se o lote falhou) e o marcador "pausado
   por nós" limpo. Marcar os patches como ativos no registro.

**Desfazer** um patch ativo: mesmo caminho, com os papéis trocados (intenção e marcador no diário antes da pausa,
atualização depois). Com o jogo pausado, se os bytes atuais forem os que escrevemos, escreve os originais e
confere. Se forem outros (alguém mexeu depois de nós), **não escreve**, avisa e tira o patch do registro e do
diário.

Patches do registro não podem se sobrepor: um patch que toca um intervalo já ativo é recusado.

### 6.3 Assinaturas (`signatures/*.toml`)

Moram em `crates/gt-grounded2/signatures/` e são embutidas no binário na compilação (uma atualização de
assinatura exige recompilar, o que já acontece em toda sessão de pesquisa).

- **`generic.toml`**: a definição de cada patch, válida para qualquer versão:
  - `id` e descrição em pt-BR;
  - `module` (padrão: o exe do jogo);
  - `pattern`: padrão AOB (mesma sintaxe do `gt scan`), descrevendo os bytes **originais**;
  - `offset`: distância do início da ocorrência até o início do patch;
  - `expected` e `replacement`: bytes em hex.
  - Regra de carga: o trecho `[offset, offset + tamanho)` precisa caber dentro do `pattern`, e cada byte fixo do
    `pattern` nesse trecho precisa ser igual ao de `expected` (erro de carga se não for). Curingas no trecho são
    permitidos; só por eles o estado `bytes diferentes` (6.4) pode acontecer.
- **`<chave-de-versão>.toml`** (ex.: `c935e799-0b27c000.toml`): o que foi verificado numa build. Para cada `id`, o
  `rva`: offset no módulo do **início do patch** (ocorrência + `offset`) onde ele foi confirmado. Pode sobrescrever `pattern`, `offset`, `expected` e
  `replacement` quando a build mudou o código.

O formato exato dos campos fica para o plano, desde que um arquivo inválido seja um erro claro na carga (com
arquivo e campo), coberto por teste unitário que carrega os arquivos reais.

### 6.4 Detecção de versão e estado de cada patch

A chave de versão é a da Fase 1 (`TimeDateStamp`-`SizeOfImage`, `ARCHITECTURE.md` 5.3). Para cada patch:

1. Busca o `pattern` nas seções executáveis do módulo, **na memória**, com o mesmo veredito de unicidade do
   `gt scan` (`none`, `unique`, `many`, `inconclusive`).
2. Estado:

| Estado | Quando | Pode aplicar? |
|---|---|---|
| `disponível (verificado)` | Ocorrência única, versão com arquivo, ocorrência + `offset` = `rva` do arquivo, e os bytes atuais são os esperados | Sim |
| `disponível (não verificado)` | Versão **sem** arquivo (ou com arquivo, mas sem entrada para esse `id`); ocorrência única na memória **e** única no arquivo em disco, no mesmo offset; bytes atuais são os esperados | Sim (decisão do dono) |
| `ativo` | Aplicado por este `gt-patch`, ou pelo diário (abaixo) com outro `gt-patch` rodando agora | Desfazer (só quem aplicou) |
| `pendente` | O diário mostra o patch aplicado no mesmo jogo, mas nenhum `gt-patch` está rodando (sobrou de uma queda) | `restore` |
| `bytes diferentes` | Ocorrência única, mas os bytes atuais não são os esperados (só possível com curingas no trecho do patch) | Não |
| `fora do lugar` | Versão com arquivo, mas a ocorrência única não cai no `rva` registrado | Não |
| `não encontrado` | Nenhuma ocorrência | Não |
| `ambíguo` | 2 ou mais ocorrências (`AGENTS.md` 4) | Não |
| `inconclusivo` | Leitura com partes ilegíveis e 0 ou 1 ocorrência | Não |
| `disco ilegível` | O cabeçalho ou as seções do arquivo em disco não puderam ser lidos (sem eles não há área de busca nem política de escrita, 6.6) | Não |

**O diário vem antes do AOB:** um patch nosso já aplicado pode impedir o padrão de casar (se o padrão cobre os bytes
trocados). Por isso, antes da busca, o estado consulta o diário (6.8): se ele é do mesmo jogo (identidade e
versão) e os bytes no endereço registrado são os novos, o estado é `ativo` (a trava do diário está ocupada por
outro `gt-patch`) ou `pendente` (trava livre). Consultar a trava não pode segurá-la, nem por um instante, para não
fazer um `run` simultâneo falhar: `F_OFD_GETLK` no `journal.lock` (6.8). Só sem entrada no diário vale a tabela pela
busca. O `pattern` descreve os bytes **originais**.

A busca em memória pode não achar o padrão se outro trainer tiver alterado o local; nesse caso o estado é
`não encontrado` e a mensagem sugere conferir com `gt scan --disk`. Sem arquivo de versão (sem `rva` para conferir),
a unicidade também é exigida **em disco**: outro trainer pode ter alterado uma de duas ocorrências na memória e
tornado a outra "única" (caso real das cargas, `RESEARCH.md`). Disco sem ocorrência única no mesmo offset dá
`ambíguo` ou `não encontrado`, conforme o caso; disco ilegível deixa o patch indisponível com a causa.

**Uma busca por comando:** o endereço de cada patch é resolvido uma vez no início do comando, com uma única
varredura das seções servindo a todos os patches. Depois disso (`run` mostrando o estado, cada ciclo do `toggle`, o
teste de estresse), só os bytes no endereço resolvido são relidos e conferidos sob pausa; a `.text` não é varrida de
novo.

### 6.5 Pausa com conferência das threads

Decisão do dono: pausar **e** conferir onde cada thread parou.

1. Se o processo já estiver parado antes de começarmos (todas as threads em `T`, ex.: um depurador ou `kill -STOP`
   do próprio dono), não enviamos SIGSTOP e, no fim, **não** enviamos SIGCONT: deixamos como encontramos. A escrita
   segue normalmente, porque o jogo já está pausado. A recuperação do diário (6.8) usa a mesma regra.
2. Enviar `SIGSTOP` ao processo e esperar até que todas as threads de `/proc/<pid>/task/` estejam paradas (estado
   `T` ou `t` em `stat`), com prazo (ordem de centenas de milissegundos). Threads zumbis (`Z`/`X`) são ignoradas.
   Estourou o prazo (threads em `D`, como num `ioctl` do driver de vídeo, só param quando a chamada volta): SIGCONT,
   e conta como uma nova tentativa do item 4, no mesmo limite de tentativas; o relatório do `toggle` separa as duas
   causas. O prazo mínimo é 500 ms. Os sinais vão por `pidfd` aberto na abertura do alvo (e conferido com o
   horário de início, 6.7), para não atingir um PID reaproveitado; se não houver API segura para isso, o plano
   decide onde fica a chamada respeitando a regra do `unsafe` (`AGENTS.md` 5).
3. Ler a posição de cada thread em `/proc/<pid>/task/<tid>/syscall`, **sem ptrace**. Verificado em 2026-10-25 num
   processo de teste: thread parada fora de syscall mostra `-1 <sp> <pc>`; dentro de syscall, `<nº> <args...> <sp>
   <pc>`; thread rodando mostra `running`. Threads que surgem ou somem durante a conferência são relidas.
   **Pontos de retomada** de cada thread: o `pc`; e, se ela está dentro de uma syscall (`nº` diferente de -1),
   também `pc - 2`, porque o kernel recua o `pc` para reexecutar a instrução `syscall` quando a chamada é
   reiniciada depois do SIGCONT.
4. **Regra de perigo** (função pura): um ponto de retomada `p` é perigoso para um patch se `início < p < fim` e `p`
   não é início de instrução no lado que está sendo escrito (fronteira perdida, 6.1). O início do trecho nunca é
   perigoso: a thread executa uma instrução inteira, velha ou nova. Uma thread cuja posição não pôde ser lida é
   tratada como perigosa **só para patches com fronteira perdida**: sem fronteira perdida, qualquer ponto onde uma
   thread possa ter parado é início de instrução nos dois lados.
   Havendo ponto perigoso, retomar, esperar alguns milissegundos e tentar de novo, algumas vezes. Esgotadas as
   tentativas, recusar e informar. Com o patch de teste (mesmas fronteiras), as novas tentativas esperadas são zero.
5. Fazer a escrita (6.2) e enviar `SIGCONT`.

**SIGCONT garantido:** depois de um SIGSTOP enviado por nós, todo caminho de saída (sucesso, erro, pânico,
Ctrl+C/SIGTERM chegando durante a pausa) envia SIGCONT antes de seguir. O `gt-patch` ignora (`SIG_IGN`, vale para o
processo inteiro) SIGTSTP, SIGTTIN e SIGTTOU desde o início do `main`, para que um Ctrl+Z no terminal não o congele
com o jogo parado ou com patches ativos. Imediatamente antes de escrever, confere de novo que todas as threads
continuam paradas (um SIGCONT de fora durante a pausa faz recomeçar a conferência). Os números de tentativas e prazos ficam
para o plano; a pausa típica é registrada nos logs e no relatório do `toggle` (6.10).

**Limitação conhecida:** um SIGKILL (ou um SIGSTOP direto, que não pode ser ignorado) no `gt-patch` exatamente
durante a pausa (milissegundos) deixa o jogo parado.
A mensagem de recuperação do diário (6.8) detecta o jogo parado e o retoma; à mão, `kill -CONT <pid>`.

**Risco a validar no jogo (portão do plano):** o efeito de SIGSTOP/SIGCONT no Wine e no container da Steam (o
processo pai recebe `SIGCHLD`) e a leitura de `/proc/<tid>/syscall` nas threads do jogo não foram observados ainda.
O plano começa por um teste curto de pausa e retomada, **sem escrita**, com o jogo aberto, que passa quando:
- todas as threads (exceto zumbis) chegam a `T`/`t` dentro do prazo em 100 pausas seguidas (no máximo 2 delas
  precisando de nova tentativa por prazo); o tempo até parar e as threads vistas em `D` são registrados;
- o `syscall` de todas elas é legível com o jogo pausado;
- depois do SIGCONT nenhuma thread fica parada, o processo continua vivo e o dono confirma que o jogo não travou.

As posições (`pc`) e os tempos de pausa observados vão para o `RESEARCH.md`. **Se o portão falhar**, a
implementação da pausa para e a IA volta ao dono com as alternativas (ex.: `PTRACE_SEIZE`/`PTRACE_INTERRUPT`,
freezer do cgroup), como emenda a esta spec.

### 6.6 Política de escrita

O motor do `gt-core` recebe de quem o usa a lista de intervalos onde pode escrever e recusa qualquer patch fora
dela. Para o jogo, o `gt-grounded2` passa só as **seções executáveis do módulo declarado pelo patch** (6.3), que
precisa ser confiável. Nesta fase todo patch declara o exe do jogo; as DLLs do Wine/Proton, embora confiáveis para
leitura, ficam fora. As seções são calculadas assim:
- as seções e as permissões vêm do cabeçalho do **arquivo em disco**, não do cabeçalho em memória, que outro
  trainer poderia alterar. Sem leitura do disco, **não se escreve** naquele módulo (escrever é mais rígido que ler).
  A busca da 6.4 usa as mesmas seções do disco, para que busca e escrita olhem a mesma área;
- interseção com a extensão do módulo e com as regiões executáveis do `maps`;
- o `maps` é relido antes de cada lote (a extensão e as seções do disco são calculadas uma vez na abertura).

Um patch que cruza o fim de uma seção é recusado. Nos testes, o alvo de teste passa a página executável dele.

### 6.7 Registro e identidade do processo

- **Abertura, nesta ordem:** ler o horário de início, abrir o `pidfd`, abrir o `/proc/<pid>/mem` para escrita e
  conferir o horário de novo. O descritor do `mem` aberto continua preso à memória do processo original, e o `pidfd`
  ao processo original, então um PID reaproveitado depois disso nunca recebe escrita nem sinal.
- **Kernel:** escrever em página de código depende de o kernel permitir escrita forçada em `/proc/<pid>/mem`
  (verificado em 2026-10-25: `CONFIG_PROC_MEM_ALWAYS_FORCE=y`). Se a escrita voltar `EIO`/`EPERM` numa página
  mapeada, a mensagem explica essa dependência (`proc_mem.force_override`).
- **Identidade:** PID **e** horário de início do processo (campo 22 de `/proc/<pid>/stat`), lidos na abertura.
  Antes de cada escrita, os dois são conferidos, junto com o estado (campo 3). Se o processo sumiu, o horário mudou
  (PID reaproveitado) ou o estado é zumbi (`Z`/`X`: o jogo fechou e o `reaper` da Steam ainda não colheu o status),
  o jogo fechou: o registro é descartado **sem escrever nada** e o diário é limpo.
- **Escrita num processo que acabou de fechar:** o kernel devolve 0 bytes em `/proc/<pid>/mem` de um processo sem
  memória, e o `write_all_at` transforma isso em `WriteZero`. O escritor passa a traduzir esse caso para "processo
  terminou" (hoje vira erro de E/S genérico), e o motor o trata como "jogo fechou".
- **Detecção de "jogo fechou" no `run`:** pelo `pidfd` (fica legível quando o processo termina) ou conferindo a
  identidade cerca de uma vez por segundo; o plano escolhe.
- **Registro:** guarda os patches ativos (endereço, originais, novos, `id`). Desfaz tudo:
  - em saída normal e no `Drop`;
  - em Ctrl+C (SIGINT), SIGTERM, SIGHUP (terminal fechado) e SIGQUIT: o tratador só marca o pedido; o laço
    principal desfaz e sai. Sinais repetidos durante o desfazer (ex.: um segundo Ctrl+C) não interrompem a
    restauração;
  - em pânico, em qualquer thread do `gt-patch`;
  - se o jogo fechar, descartando sem escrever (acima).
- Se desfazer falhar (ex.: as threads nunca saem do trecho), o patch continua no diário e a mensagem diz que a
  próxima execução do `gt-patch` vai restaurar.

### 6.8 Diário em disco

Decisão do dono: cobrir SIGKILL, travamento e queda de energia com um diário.

- **Local:** `$XDG_STATE_HOME/grounded-trainer/journal.json` (padrão `~/.local/state/grounded-trainer/`).
  Nos testes, um diretório temporário.
- **Conteúdo:** `boot_id` (`/proc/sys/kernel/random/boot_id`, para que um reboot nunca seja confundido com o mesmo
  jogo), PID, horário de início do processo, chave de versão e, para cada patch, `id`, endereço, bytes originais e
  bytes novos.
- **Escrita atômica:** arquivo temporário no mesmo diretório, `fsync` e `rename`. A intenção é gravada **antes**
  de escrever no jogo e atualizada depois de cada desfazer.
- **Uma instância por vez:** trava OFD do `fcntl` (`F_OFD_SETLK`) num arquivo **separado**, `journal.lock`, no mesmo
  diretório (não no `journal.json`, que é trocado por `rename` a cada gravação). Todo comando que escreve pega a
  trava **antes** da recuperação e a segura até sair; ordem: trava → recuperação → escritas. Um segundo `gt-patch`
  que escreveria no jogo recusa com mensagem. `gt-patch list` não pega a trava: só a consulta com `F_OFD_GETLK`
  (6.4) e funciona em paralelo.
- **Pausa sem escrita:** o `gt-patch profile` também pega a trava e grava só o marcador (sem patches) antes de cada
  pausa, para que um SIGKILL dele no meio de uma pausa seja recuperado como os demais e para que ele nunca rode ao
  lado de um `run`/`toggle`.
- **Ordem no início de todo comando que escreve ou pausa** (`run`, `toggle`, `restore`, `profile`): trava → leitura
  do diário → recuperação → resolução do alvo. A recuperação confere a identidade do jogo **registrada no diário**
  direto no `/proc` (PID, horário de início, estado, `boot_id`), sem depender de achar o jogo pelo nome. Assim
  `restore` funciona com o jogo fechado. Depois da recuperação, `run`, `toggle` e `profile` resolvem o alvo e, sem
  jogo, falham com "jogo não encontrado"; `restore` não resolve alvo nenhum.
- **Recuperação automática** (decisão do dono):
  - mesmo `boot_id`, PID, horário de início e versão: o jogo é o mesmo. Se ele estiver parado **e** o diário tiver o
    marcador "pausado por nós", envia SIGCONT e avisa (cobre o SIGKILL durante a pausa). Jogo parado sem o marcador
    foi parado por outro (ex.: depurador): não envia SIGCONT, avisa e segue com a recuperação abaixo, como na 6.5
    item 1. Para cada patch: bytes atuais = originais → só descarta; **cada byte**
    igual ao original ou ao novo daquela posição (inclui escrita interrompida no meio por SIGKILL) → restaura pela
    6.2, escrevendo os originais; algum byte diferente dos dois → não escreve, descarta e avisa;
  - outro processo (jogo reiniciado), outro boot ou jogo fechado: a memória antiga não existe mais; descarta o diário
    e avisa. `restore` com o jogo fechado não é erro: descarta e sai com 0;
  - diário ilegível ou corrompido: não escreve nada no jogo, renomeia o arquivo para `journal.json.bad` e avisa.
- A decisão de recuperação (o que fazer com cada entrada) é uma função pura, testada sem processo.

### 6.9 Patch de teste

Decisão do dono: uma troca **sem efeito** na partida.

- Uma instrução trocada por outra codificação do mesmo tamanho e do mesmo efeito. Exemplo: `mov rdi, rcx` pode ser
  `48 8B F9` ou `48 89 CF`.
- Escolhida pela IA numa sessão de pesquisa com o jogo aberto, seguindo:
  - num trecho **quente**, para que a troca aconteça com o código em uso. Método: perfil por amostragem, com o jogo
    na partida, de 200 pausas (6.5, sem escrita) espaçadas ao longo de pelo menos um minuto, contando os `pc` das
    threads que caem em cada função da `.text` do jogo (limites das funções pela `.pdata` do arquivo em disco). A
    função escolhida precisa ter **pelo menos 5 amostras**, e a instrução trocada precisa estar no **mesmo bloco
    básico** de pelo menos uma amostra (entre a amostra e a instrução não há desvio nem alvo de desvio da função,
    conferido pela desmontagem da função inteira). As contagens vão para o `RESEARCH.md`;
  - fora dos 5 locais que o FLiNG altera (`RESEARCH.md`, sessão de 2026-10-25);
  - padrão AOB **único** em disco (`gt scan --disk`) e na memória;
  - registrada em `RESEARCH.md` (versão, `módulo+offset`, bytes, desmontagem, como foi achada) e em
    `generic.toml` + `c935e799-0b27c000.toml` (ou a chave da build atual).
- `id`: `teste-codificacao`.
- Se nenhum local cumprir todos os critérios, a IA tenta uma vez com 1000 amostras; se ainda não houver, volta ao
  dono com o que achou, sem relaxar os critérios por conta própria.

### 6.10 Binário `gt-patch`

Texto em pt-BR; erros com causa e ação sugerida, código de saída diferente de 0 em erro, logs (`tracing`) em
stderr controlados por `RUST_LOG`. Opção global `--pid` como no `gt`.

**Fino por construção:** o laço de `run`/`toggle`, o tratamento de sinais, o pânico e a recuperação do diário moram
numa biblioteca (no `gt-core` a parte genérica, no `gt-grounded2` a do jogo). O `gt-patch` só junta argumentos,
assinaturas embutidas e saída. Isso permite testar esses comportamentos contra o alvo de teste (ELF, sem PE e sem
assinaturas) com um binário de apoio a testes que chama a **mesma** biblioteca com um patch cru (endereço e bytes
passados pelo teste), sem flag oculta nem endereço fixo no `gt-patch` (8).

Comandos:

- **`gt-patch list`**: chave de versão, se há arquivo de assinaturas para ela, e cada patch com o estado (6.4) e o
  `módulo+offset`. Mostra também se existe diário pendente. Não escreve nada.
- **`gt-patch run <id>...`**: recupera o diário, aplica os patches e fica rodando, mostrando o estado, até Ctrl+C,
  SIGTERM, SIGHUP, SIGQUIT ou o jogo fechar. Então desfaz (6.7) e sai dizendo o que restaurou.
- **`gt-patch toggle <id> --times N [--interval MS]`**: recupera o diário, liga e desliga o patch `N` vezes com
  intervalo `MS` entre as trocas (padrão do plano), e termina com o patch **desligado**. Para no primeiro erro,
  desfazendo. Relatório: ciclos feitos, novas tentativas por thread no trecho, pausa média e máxima.
- **`gt-patch restore`**: só recupera o diário e sai. Sem diário: "nada a restaurar".
- **`gt-patch profile [--samples N] [--interval MS]`**: ferramenta de pesquisa do perfil da 6.9. Faz `N` pausas
  (padrão 200, sem escrever nada na memória), conta os `pc` por função da `.text` (`.pdata` do disco, lida por uma
  função nova do `pe.rs`) e mostra as funções mais amostradas com `módulo+offset` e os `pc` de cada uma. Pega a
  trava e usa o marcador (6.8). Mora no
  `gt-patch` porque pausa o jogo (envia sinais), o que o `gt` somente leitura não faz. Também serve para escolher
  locais nas próximas fases.

Aplicar um patch `disponível (não verificado)` imprime um aviso dizendo que a versão do jogo não tem assinaturas
conferidas, sem pedir confirmação. O `run` que termina porque o jogo fechou sai com código 0.

No `run`, "mostrando o estado" é: os patches ativos e, a cada mudança, uma linha nova (jogo fechou, restauração). No
`toggle`, a pausa medida de cada troca vai do SIGSTOP ao SIGCONT da tentativa que escreveu; as novas tentativas são
contadas à parte.

### 6.11 Erros obrigatórios

Mensagens claras para: jogo não encontrado (mesma regra do `gt`), sem permissão (`ptrace_scope`, como na Fase 1),
patch desconhecido, patch indisponível (com o estado), bytes diferentes do esperado (mostrando os encontrados),
threads que não saem do trecho, jogo que não pausa no prazo, outro `gt-patch` em execução, jogo fechou durante a
operação.

### 6.12 Extensão dos módulos confiáveis (pendência da Fase 1)

Decisão do dono: **`min(memória, disco)`**.

- Para cada módulo **confiável**, a extensão passa a ser `[base, base + min(SizeOfImage na memória, SizeOfImage do
  arquivo em disco))`. O arquivo é lido como no `gt diff` (pelo `/proc/<pid>/root` e, se falhar, pelo caminho
  direto), só o cabeçalho.
- Lê-se só o início do arquivo (cabeçalho), por uma função nova: o `DiskImage::load` atual lê o arquivo inteiro e
  não serve para os ~129 módulos.
- Não se espera diferença legítima (o carregador do Wine mapeia pelo `SizeOfImage` do próprio arquivo). Toda
  diferença sai como aviso nos logs, com o módulo e os dois valores. Na sessão de reconferência do `gt scan`
  (seção 7, item 1) a IA conta os módulos com diferença no jogo real; o esperado é zero. Se houver algum, ele vai
  para o `RESEARCH.md` e a IA volta ao dono antes de seguir, porque encolher a extensão pode fazer a guarda
  bloquear código legítimo.
- Se o arquivo não puder ser lido, fica o valor da memória, o módulo é marcado como "extensão não conferida" e isso
  sai nos logs e no `gt inspect` do módulo escolhido.
- Módulos de terceiros continuam com o valor da memória (do lado restritivo: uma extensão maior bloqueia mais).
- Vale para a guarda (6.12 da spec 001), para a política de escrita (6.6) e para o `módulo+offset` das saídas.
- `gt inspect` mostra o `SizeOfImage` do disco ao lado do da memória quando forem diferentes.
- Atualizar a "Limitações conhecidas" da 6.12 da spec 001 para apontar esta decisão.

## 7. Pendências da Fase 1 incluídas

Decididas pela IA, a pedido do dono ("corrija quando mexer no código deles"):

1. **`gt scan` com o jogo aberto** (guarda nas seções e veredito `inconclusive`, commit `85bfa2f`). Passo próprio
   com o jogo aberto e o **FLiNG aberto e conectado** (como na sessão de 2026-10-25), antes do critério de pronto.
   Passa quando:
   - o padrão do god mode (`RESEARCH.md`) dá "único" na memória e com `--disk`;
   - o padrão original das cargas (`f3 0f 10 89 74 09 00 00 f3 0f 58 c9`) dá 2 ocorrências com `--disk` e 1 na
     memória (o FLiNG alterou a outra), mostrando por que `--disk` existe;
   - `gt scan --all` com o padrão do god mode termina sem ser bloqueado pela guarda (todas as seções são do módulo
     confiável) e dá "único". Se vier `inconclusive`, as lacunas ilegíveis são registradas no `RESEARCH.md` e
     explicadas antes de seguir (o esperado é não haver, mas isso nunca foi observado);
   - `gt scan --module TrSpeedHack_x64.dll` (a DLL do FLiNG) é recusado pela guarda. Nunca foi confirmado que essa
    DLL aparece no `maps` com o FLiNG conectado e nenhuma opção ligada. Se ela não aparecer, vale o mesmo teste com
    qualquer PE não confiável do `maps`; se não houver nenhum, o fato vai para o `RESEARCH.md` e o caso fica coberto
    pelos testes da guarda;
   - contagem dos módulos com `SizeOfImage` diferente entre memória e disco (6.12), esperado zero.

   Fora disso, o veredito `inconclusive` segue coberto pelos testes unitários. Resultado no `RESEARCH.md`. A
   contagem exige a 6.12 implementada, então a sessão A acontece depois dela no plano.
2. **`dump --force` atômico:** o `.bin` e o `.json` são gravados em arquivos temporários e renomeados no fim; uma
   falha no meio não deixa par incompleto nem apaga o anterior.
3. **`compare.rs`:** uma lacuna pura no fim do bloco passa a adiar a diferença anterior (com teste do caso).
4. **`test-support` nas dependências normais do `gt-cli`:** o `gt-testtarget` sai do `gt-cli` (ex.: crate próprio de
   apoio a testes) para que o `gt` não compile o código de `mmap`/`prctl`. Onde ele mora fica para o plano.
5. **Busca reutilizável:** a lógica do `gt scan` presa em `apps/gt-cli/src/cmd/scan.rs` (veredito, leitura em
   blocos com lacunas, filtragem de ocorrências nas lacunas, escolha de seções) vai para o `gt-core`, com os testes
   existentes, para a 6.4 usar o mesmo veredito. A `many_candidates_message` (`gt-cli/src/main.rs`) acompanha a
   resolução do alvo para o `gt-grounded2`.
6. **"Leitura de memória: ok" no `inspect`:** troca por informação útil (ex.: quantos bytes do cabeçalho foram lidos
   e se a extensão do módulo foi conferida com o disco, 6.12) ou some.
7. **Escritor (`gt-core/src/mem/writer.rs`):** o `PAGE` fixo em 4096 do `test_support` e do `read_lossy`
   (`mem/mod.rs`) passa a vir de `sysconf(_SC_PAGESIZE)`; novos testes de escrita acima de 1 MiB e em página `PROT_NONE`. O teste de `PROT_NONE`
   afirma o comportamento observado no kernel de referência (`ARCHITECTURE.md` 7), descoberto ao escrever o teste e
   anotado num comentário; o motor não depende dele, porque a política (6.6) só deixa escrever em código.
8. **Sem mudança nesta fase** (já documentados na spec 001): erros com `--json` saindo em texto, e o snapshot do
   `maps` na abertura. O motor de patches não herda o problema do snapshot: relê o `maps` antes de cada lote para a
   política de escrita.

## 8. Testes

| Nível | O que cobre | Exige jogo? |
|---|---|---|
| Unitário | Validação de patch (6.1, inclusive o `call` nos dois sentidos e fronteiras perdidas); leitura do TOML, regra de carga e erros de campo; carga dos arquivos reais de `signatures/`; cálculo de estado (6.4) para todos os estados; regra de perigo (6.5: início do trecho, `pc` e `pc - 2` em syscall, posição ilegível com e sem fronteira perdida); parse de `/proc/<pid>/task/<tid>/syscall` (`running`, `-1 sp pc`, syscall com argumentos) e do campo 22 do `stat` (nome com espaços e parênteses); espera da pausa ignorando zumbis; decisão de recuperação do diário (6.8) para cada caso, inclusive `boot_id` diferente, escrita interrompida e jogo parado com e sem o marcador; patches sobrepostos recusados; extensão `min(memória, disco)` | Não |
| Integração (API) | Contra o processo-alvo de teste: aplicar e desfazer na página executável dele; recusa com bytes diferentes; recusa fora da política; restauração no `Drop`; identidade (processo que terminou → descarta sem escrever); segunda instância recusada pela trava, e `list` consultando a trava sem atrapalhar um `run`; processo zumbi (o teste mata o alvo e ainda não colhe o status) reconhecido como "jogo fechou", sem erro de escrita; estado `ativo`/`pendente` pelo diário; alvo já parado antes de nós (o motor não envia SIGSTOP nem SIGCONT e o alvo continua parado no fim); prazo de pausa estourado (simulado por um alvo cujas threads não param, se viável, senão pela função pura de espera) termina com SIGCONT e erro | Não |
| Fronteira perdida (API) | O alvo mantém uma thread bloqueada para sempre numa syscall (ex.: leitura de um pipe vazio) dentro de código na página executável dele. Um patch que tem o `pc` (logo depois da `syscall`) como fronteira perdida é recusado depois das tentativas, com a mensagem da 6.11; outro que tem `pc - 2` como fronteira perdida também; um patch que mantém as duas fronteiras é aceito | Não |
| Estresse (API) | O alvo ganha uma thread que executa sem parar um laço numa página executável; o motor alterna 1000 vezes duas codificações equivalentes de uma instrução do laço; o alvo não pode cair e o laço continua rodando no fim | Não |
| Integração (binário de apoio) | O binário de apoio a testes (6.10) contra o alvo: Ctrl+C (SIGINT), SIGTERM e SIGHUP durante `run` restauram; pânico provocado restaura, inclusive pânico **durante a pausa**, que também precisa enviar SIGCONT (como provocar fica para o plano, só no binário de apoio); SIGKILL seguido de recuperação restaura; SIGKILL **durante a pausa** (o binário de apoio se mata depois do SIGSTOP, por um gancho só de teste) deixa o alvo parado, e a recuperação o retoma e restaura | Não |
| Unitário (lado do jogo) | Cálculo da política de escrita do jogo (6.6) como função pura, com `maps` e cabeçalhos PE sintéticos: seção executável aceita, seção de dados recusada, patch que cruza o fim da seção recusado, disco ilegível sem política, interseção com o `maps`; leitura da `.pdata` e agregação de `pc` por função do `profile` sobre bytes sintéticos; cadeia assinatura TOML → estado (6.4) → endereço, sobre buffers sintéticos de "memória" e "disco" | Não |
| Pendências (seção 7) | `dump --force` com falha no meio (ex.: o caminho do `.json` é um diretório) não deixa `.bin` novo e preserva o par anterior; formatação do `inspect` (linha que substitui "Leitura de memória: ok" e `SizeOfImage` do disco quando difere) por teste unitário da função de formatação; caso da lacuna no fim do bloco em `compare.rs` | Não |
| Integração (CLI) | `gt-patch` ignora SIGTSTP durante um `run` no binário de apoio (continua respondendo e restaura no SIGINT seguinte); `gt-patch` sem jogo: "jogo não encontrado"; `restore` sem diário ("nada a restaurar") e com diário de um processo que já terminou (descarta, sai com 0); diário corrompido vira `.bad` | Não |
| Jogo (`#[ignore]`) | Pausa e retomada sem escrita (primeiro passo do plano, ver 6.5); `list` mostra `teste-codificacao` como `disponível (verificado)`; um liga/desliga confere os bytes. Testes com bytes de uma versão só rodam com a chave esperada | Sim |

`cargo fmt --check`, `clippy -D warnings` e `cargo test --workspace` passam (`AGENTS.md` 6). `cargo build -p gt-cli`
isolado continua compilando sem a feature `write`. É essa a prova de que o `gt` não usa o escritor: num build do
workspace inteiro as features se unificam e o `gt-core` compilado tem `write` por causa do `gt-patch`.

## 9. Critério de pronto

São duas sessões com o jogo. **Sessão A** (FLiNG aberto e conectado, nenhuma opção ligada): a reconferência do
`gt scan` da seção 7, item 1. Depois o dono fecha o FLiNG e **reinicia o jogo** (para que os desvios do FLiNG saiam
da memória). **Sessão B**, antes dela **o dono faz backup do save** (pedido pela IA). Jogo aberto num save de teste,
FLiNG **fechado**:

1. `gt-patch list` mostra `teste-codificacao` como `disponível (verificado)`.
2. `gt-patch toggle teste-codificacao --times 100` termina sem erro, e o jogo continua rodando normal (o dono
   confirma, andando pelo mapa por um minuto). O relatório mostra os 100 ciclos, zero novas tentativas e a pausa
   máxima. Referência: pausa máxima abaixo de 50 ms; acima disso o relatório avisa e o valor vai para o
   `RESEARCH.md`, mas o critério continua sendo o jogo estável.
3. `gt-patch run teste-codificacao`, depois Ctrl+C: `gt diff` mostra "nenhuma alteração". O mesmo com `kill <pid>`
   (SIGTERM).
4. `gt-patch run teste-codificacao`, depois `kill -9 <pid>`: `gt diff` mostra o patch; `gt-patch restore` o desfaz e
   o `gt diff` volta a "nenhuma alteração".
5. `gt-patch run teste-codificacao` e fechar o jogo: o `gt-patch` sai dizendo que o jogo fechou, sem erro de escrita.
6. (Sessão A, já feita) `gt scan` reconferido (seção 7, item 1).

**O dono faz:** backup do save; abrir o jogo quando eu pedir (no início do plano, para o teste de pausa e a escolha
do patch de teste; no fim, para o critério acima); dizer se o jogo travou, engasgou ou caiu.

## 10. Documentos a atualizar

- `ARCHITECTURE.md`: 3 (crate `gt-patch`, novo lugar da resolução do alvo e da busca, novo lugar do
  `gt-testtarget`), 5.3 (formato de assinaturas da 6.3),
  5.4 (conferência das threads, identidade do processo, diário), 8 (decisões desta spec).
- `ROADMAP.md`: status da Fase 2 ao terminar, e o critério "matar o trainer restaura os bytes" passa a dizer: Ctrl+C,
  SIGTERM e pânico restauram na hora; SIGKILL restaura na próxima execução, pelo diário.
- `RESEARCH.md`: comportamento do jogo com SIGSTOP/SIGCONT, patch de teste escolhido, reconferência do `gt scan`.
- Spec 001, 6.12: apontar a decisão do `SizeOfImage`.

## 11. Decisões tomadas

| Decisão | Quem decidiu |
|---|---|
| Extensão dos módulos confiáveis = `min(memória, disco)`; sem disco, memória + "não conferida" | Dono |
| Binário separado `gt-patch`; o `gt` continua somente leitura | Dono |
| Patch de teste = troca de codificação equivalente, sem efeito na partida | Dono |
| SIGKILL coberto por diário em disco, com recuperação automática na próxima execução | Dono |
| Pausa com conferência da posição das threads via `/proc`, sem ptrace | Dono |
| Versão sem arquivo de assinaturas: patch disponível como "não verificado" se o AOB for único e os bytes baterem | Dono |
| Patch só troca bytes por bytes do mesmo tamanho; validação de instruções inteiras e da regra do `call` nos dois sentidos | IA |
| Fronteiras perdidas permitidas e vigiadas pela pausa; saltos externos para elas são limitação conhecida | IA |
| Escrita só em seções executáveis de módulos confiáveis, com seções do disco, por política passada ao `gt-core` | IA |
| Estado `ativo`/`pendente` pelo diário antes do AOB; uma varredura por comando | IA |
| Versão com arquivo mas sem entrada para o `id` = `não verificado` | IA |
| Recuperação byte a byte (escrita interrompida); `boot_id` no diário; sinais por `pidfd` | IA |
| Portão do SIGSTOP no jogo com critério definido; se falhar, emenda à spec com o dono | IA |
| Laço, sinais e diário numa biblioteca; testes por binário de apoio, não por flag oculta no `gt-patch` | IA |
| Diário gravado antes da pausa, com marcador "pausado por nós"; nenhum `fsync` durante a pausa | IA |
| Pontos de retomada `pc` e `pc - 2` para threads em syscall; posição ilegível só bloqueia patch com fronteira perdida | IA |
| Função quente do patch de teste escolhida por perfil de 200 pausas, mínimo de 5 amostras | IA |
| Reconferência do `gt scan` com o FLiNG aberto, com resultados esperados definidos | IA |
| Trecho do patch dentro do `pattern`, com bytes fixos iguais a `expected`; `rva` = início do patch | IA |
| Escrita só no módulo declarado pelo patch (nesta fase, o exe); busca e escrita pelas seções do disco | IA |
| `não verificado` exige unicidade também em disco | IA |
| Zumbi e `WriteZero` tratados como "jogo fechou" | IA |
| `SIG_IGN` em SIGTSTP/SIGTTIN/SIGTTOU no `gt-patch` inteiro | IA |
| Comando de pesquisa `gt-patch profile`; instrução de teste no mesmo bloco básico de uma amostra | IA |
| Feature `write` também no `gt-grounded2`, em cascata | IA |
| Jogo já parado por outro: escrever sem enviar sinais, também na recuperação | IA |
| Duas sessões no critério de pronto (FLiNG aberto, depois fechado com o jogo reiniciado) | IA |
| Marcador "pausado por nós" só quando enviamos SIGSTOP; `profile` sob a mesma trava e marcador | IA |
| Ordem trava → diário → recuperação → alvo; recuperação pela identidade registrada | IA |
| Prazo de pausa estourado conta como nova tentativa; portão com 100 pausas | IA |
| Ordem de abertura horário → `pidfd` → `mem` → horário | IA |
| Aviso (sem confirmação) ao aplicar patch não verificado; `run` sai com 0 quando o jogo fecha | IA |
| Identidade do processo por PID + horário de início, conferida antes de cada escrita | IA |
| Restaurar só onde os bytes ainda são os nossos; senão avisar e não tocar | IA |
| Processo já parado antes de nós: não enviar SIGSTOP nem SIGCONT | IA |
| Diário em `$XDG_STATE_HOME/grounded-trainer/`, escrita atômica, trava OFD em `journal.lock` para uma instância | IA |
| Assinaturas embutidas na compilação; `generic.toml` + `<chave>.toml` com `rva` verificado | IA |
| Resolução do alvo movida do `gt-cli` para o `gt-grounded2` | IA |
| Sem `--json` no `gt-patch` nesta fase | IA |
| Tratamento das pendências da Fase 1 (seção 7) | IA |

## 12. Deixado para quem implementa

- Organização interna dos módulos e tipos do motor.
- Prazos, número de tentativas e espera entre tentativas da pausa (6.5); intervalo padrão do `toggle`.
- Formato exato dos campos TOML e do JSON do diário.
- Onde moram o `gt-testtarget` e o binário de apoio a testes, e como os testes os encontram no
  `cargo test --workspace` puro. O `test_support` ganha o que esses testes pedem (região grande para o teste acima de
  1 MiB, página executável com código real para o laço e a syscall).
- Como provocar o pânico e o SIGKILL durante a pausa no binário de apoio.
- Features do `nix` a ativar (`signal`, `fs`...) e dependências novas do `gt-core` (`serde`/`serde_json` para o
  diário, `tracing`), registradas no `ARCHITECTURE.md` 4. O código que envia sinais ao jogo fica atrás da feature
  `write` do `gt-core`. Toda chamada `unsafe` nova (`SIG_IGN`, `pidfd_open`) fica em `gt-core/src/mem/` (`AGENTS.md`
  5), e o `libc` deixa de ser opcional só para `test-support`. O `SIG_IGN` fica na biblioteca compartilhada, para que
  o teste pelo binário de apoio valha também para o `gt-patch`.
- Como o tratador de sinais conversa com o laço principal (ex.: `signal-hook` com flag).
