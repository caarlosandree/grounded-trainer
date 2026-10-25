# Spec 001 — Núcleo de memória e ferramenta de pesquisa `gt`

| Campo | Valor |
|---|---|
| Fase | 1 (ver `docs/ROADMAP.md`) |
| Estado | Aprovada pelo dono em 2026-10-24 |
| Data | 2026-10-24 |

## 1. Intenção

Enxergar o Grounded 2 **de fora**, sem alterar nada: achar o processo, ler a memória, procurar padrões
de bytes, desmontar código e comparar o código em execução com o executável em disco.

É a base de todas as fases seguintes: os cheats das Fases 4–6 só podem ser construídos depois de
localizados, e a localização acontece com estas ferramentas. Quem usa o `gt` no dia a dia é a IA,
durante as sessões de pesquisa. O dono deixa o jogo aberto e vê os resultados resumidos.

## 2. Objetivos

1. Biblioteca `gt-core` com as peças genéricas (não sabe nada do Grounded 2):
   processo, módulos, leitura e escrita de memória, cabeçalho PE, busca AOB e desmontagem.
2. Executável `gt` (crate `apps/gt-cli`) com sete comandos, **todos somente leitura no jogo**:
   `inspect`, `read`, `scan`, `disasm`, `diff`, `dump` e `fling`.
3. Saída em texto legível por padrão e em JSON com `--json`.
4. Reproduzir, com as ferramentas, os achados já registrados em `docs/RESEARCH.md`.

## 3. Fora do objetivo (fica para outras fases)

- Qualquer comando do `gt` que escreva na memória do jogo (Fase 2, com verificação de bytes, pausa e restauração).
- Pausar o processo (`SIGSTOP`), `PatchRegistry` e restauração de patches (Fase 2).
- Arquivos `signatures/*.toml` e detecção de versão contra assinaturas (Fase 2). Nesta fase o `gt` só **mostra** a chave de versão.
- TUI (Fase 3), estruturas do Unreal (Fase 5) e code caves (Fase 6).

## 4. Anti-objetivos (falha mesmo que "funcione")

- O `gt` alterar um único byte do jogo, em memória ou em disco.
- O `gt` travar, pausar ou derrubar o jogo. Ler a memória não pode interferir na partida.
- Desmontar ou exibir o código injetado pelo FLiNG ou outro trainer, em qualquer comando (ver 6.12). Só mostramos **o que** ele alterou e **para onde** o desvio aponta.
- Endereços fixos do jogo no código de produção. Os únicos endereços aceitos são os digitados pelo usuário na linha de comando ou lidos do `symbols.ini` do FLiNG. Testes `#[ignore]` e documentos podem citar endereços e chaves de versão.

## 5. Onde cada coisa mora

| Crate | Conteúdo desta fase |
|---|---|
| `gt-core` | `process.rs` (achar processo, ler `/proc/<pid>/maps`, módulos), `mem/` (leitura e escrita, único lugar com `unsafe`), `pe.rs`, `scan.rs`, `asm.rs` (só desmontagem nesta fase) |
| `gt-grounded2` | Constantes do jogo (nome do processo e do módulo, AppID `2661300`, local do prefixo do Proton), a **política de módulos confiáveis** (6.1) passada ao `gt-core`, e o leitor do `symbols.ini` do FLiNG |
| `gt-cli` | Binário `gt` (`[[bin]] name = "gt"`): argumentos (`clap`), formatação de texto e JSON, mensagens de erro em pt-BR |
| `gt-ue`, `gt-tui` | Sem mudanças |

**Somente leitura por construção:** `gt-core` separa o leitor de memória (`process_vm_readv`, página a
página quando um bloco falha, porque a chamada para na primeira página ilegível) do escritor (6.11), que é
um tipo à parte e só existe com a feature `write` do `gt-core`. O `gt-cli` não ativa essa feature.
`cargo build -p gt-cli` **isolado** compilando prova que ele não usa o escritor (num build do workspace
inteiro as features se unificam). O plano inclui esse comando na validação.

**Features nos testes:** os testes que dependem de `write` e `test-support` (6.13) precisam rodar no
`cargo test --workspace` puro do `AGENTS.md` 6, sem flags extras (ex.: `gt-core` como autodependência em
`[dev-dependencies]` com essas features). Onde mora o binário do alvo de teste e como o teste de CLI o
encontra é decisão do plano, desde que funcione nesse mesmo comando.

**Dependências novas** (registrar em `ARCHITECTURE.md` 4 com versão fixada): `serde_json` para o `--json`, e
o que mais o plano precisar (ex.: `libc`), sempre justificado.

A regra de dependência do `ARCHITECTURE.md` vale (`gt-cli` → `gt-grounded2` → `gt-ue` → `gt-core`). Nesta fase `gt-grounded2` só usa `gt-core`.

## 6. Comportamento

### 6.1 Escolha do processo alvo (todos os comandos)

- **Padrão:** procurar em `/proc/*/cmdline` o processo cujo **primeiro argumento** (até o primeiro NUL)
  aponta para `Grounded2Steam-Win64-Shipping.exe`. Ele vem em formato Windows e o Wine pode juntar os
  argumentos com espaço no mesmo campo (ex. `S:\steamapps\...\Grounded2Steam-Win64-Shipping.exe Augusta`).
  Regra (sem diferenciar maiúsculas): o campo casa se contiver o nome do exe precedido por `\`, `/` ou pelo
  início do campo, e seguido pelo fim do campo ou por espaço. Isso já descarta `reaper`, `proton`,
  `steam.exe` e o launcher `Grounded2.exe`. Verificado em 2026-10-24: no jogo os argumentos vêm separados por NUL
  (`...\Grounded2Steam-Win64-Shipping.exe`, `Augusta`), e os demais processos com `Grounded2` no `cmdline` são
  `reaper`, `python3 .../proton`, `steam.exe` e o launcher `S:\...\Grounded2.exe`.
  - Nenhum candidato: erro "jogo não encontrado; abra o Grounded 2 e tente de novo".
  - Mais de um candidato: erro listando os PIDs e pedindo `--pid`.
- **`--pid <PID>`** (opção global): usa esse processo diretamente. Serve para escolher manualmente e para
  os testes com o processo-alvo de teste.
- **`--module <nome>`** (opção global): o módulo usado nos endereços `+offset` (ver 6.2) e nos comandos que
  operam "no executável". Padrão: `Grounded2Steam-Win64-Shipping.exe`. Com `--pid` num processo que não é o
  jogo, o usuário informa o módulo dele.
- **Base do módulo:** primeira região de `/proc/<pid>/maps` cujo arquivo tem esse nome (comparação pelo
  nome do arquivo, sem diferenciar maiúsculas) e cujo offset é 0. Nunca assumir `0x140000000`.
- **Extensão do módulo:** `[base, base + SizeOfImage)`, com `SizeOfImage` lido do cabeçalho PE na memória.
  **Não** usar `maps` para isso: sob o Wine, `.text` e `.rdata` aparecem como regiões anônimas, sem o nome
  do arquivo (verificado em 2026-10-24).
- **Módulos conhecidos:** todas as imagens PE carregadas no processo, ou seja, cada região de `maps` com
  arquivo, offset 0 e que começa com `MZ`/`PE`. Cada uma tem a mesma extensão `[base, base + SizeOfImage)`.
  Servem para mostrar `módulo+offset` em qualquer saída.
- **Módulos confiáveis** (subconjunto usado pela guarda da 6.12). O `gt-core` recebe a política como uma
  lista de **diretórios absolutos** e não conhece o jogo; um PE é confiável se o caminho do arquivo em `maps`
  estiver dentro de um deles, comparando **por componentes de caminho** (nunca por substring:
  `/tmp/steamapps/common/Proton x/evil.dll` não casa). Não há exceção para `--module`: a confiança vem só da
  política. `gt-grounded2` monta a lista para o Grounded 2:
  - **Bibliotecas Steam consideradas:** a que contém o jogo, obtida do caminho do exe em `maps` (tudo até o
    componente `steamapps` inclusive), e a padrão `~/.local/share/Steam/steamapps`.
  - Em cada biblioteca: `common/Grounded2/` (o jogo), cada `common/Proton*/` e `common/SteamLinuxRuntime*/`
    (expandidos listando o diretório `common/`).
  - `~/.local/share/Steam/compatibilitytools.d/` (Protons instalados à parte).
  - No prefixo do jogo (`<biblioteca do jogo>/compatdata/2661300/pfx/drive_c/`): `windows/`,
    `Program Files (x86)/Steam/` e `Program Files/Steam/` (DLLs da própria Steam, como `steamclient64.dll` e
    `GameOverlayRenderer64.dll`).

  Qualquer outro PE é **módulo de terceiros**. Exemplo verificado: a DLL do FLiNG fica em
  `drive_c/users/steamuser/AppData/Local/FLiNGTrainer/TrSpeedHack_x64.dll`. Na dúvida, a lista fica do lado
  restritivo: um falso "terceiros" só bloqueia mais.
- **Ordem dos erros:** primeiro "processo não encontrado", depois "módulo não encontrado", depois
  "o módulo X não é uma imagem PE". O módulo só é procurado quando o comando precisa dele: `read` e
  `disasm` com endereço absoluto funcionam mesmo se o módulo de `--module` não existir.
- **Alvo sem PE** (ex.: o processo-alvo de teste, que é ELF): `read` com endereço absoluto funciona.
  Comandos que dependem do PE do módulo (`inspect`, `scan`, `diff`, `dump` por seção, `fling` e o atalho
  `+offset`) falham com a mensagem "o módulo X não é uma imagem PE".

### 6.2 Endereços aceitos

| Forma | Exemplo | Significado |
|---|---|---|
| Absoluto | `0x1456FC11C` | Endereço virtual |
| Módulo + offset | `Grounded2Steam-Win64-Shipping.exe+56FC11C` | Base do módulo + offset |
| Atalho | `+56FC11C` | Base do módulo de `--module` + offset |

Offsets e endereços são sempre **hexadecimais**, com ou sem `0x`. Tamanhos e contagens (`read`, `dump`,
`disasm -n`, limites) são **decimais**, ou hexadecimais com o prefixo `0x`. Na saída, todo endereço dentro de um
módulo conhecido aparece nas duas formas: absoluto e `módulo+offset`.

### 6.3 `gt inspect`

Mostra:
- PID escolhido e os candidatos descartados (com o motivo): todo processo cujo `cmdline` contém `Grounded2`
  (sem diferenciar maiúsculas) e não passou na regra acima.
- Caminho do executável, base, `SizeOfImage` e `ImageBase` do cabeçalho.
- **Chave de versão** (`ARCHITECTURE.md` 5.3): `TimeDateStamp` em hex + `SizeOfImage`, ex. `c935e799-0b27c000`.
- `ProductVersion` do recurso de versão (ex. `Augusta-CL-3207493`), se conseguir ler; senão, "indisponível".
- Tabela de **todas** as seções lidas do cabeçalho PE **na memória**: nome, endereço, `VirtualSize`,
  `SizeOfRawData` e permissões (das `Characteristics` da seção no PE). O tamanho da seção, em todos os comandos, é o `VirtualSize`. O trecho entre
  `SizeOfRawData` e `VirtualSize` (ex.: o final da `.data`) existe só na memória e é lido normalmente.
- Valor de `kernel.yama.ptrace_scope` ("Yama ausente" se o arquivo não existir) e se a leitura de memória funcionou.

### 6.4 `gt read <endereço> [tamanho]`

Hexdump (16 bytes por linha, com coluna ASCII) a partir do endereço. Tamanho padrão: 64 bytes.
Páginas ilegíveis aparecem como `??` e são indicadas, sem abortar o comando. Se **nenhum** byte do
intervalo estiver mapeado, é erro (6.10). Sujeito à guarda da 6.12
(dados comuns, que não são executáveis, continuam livres).

### 6.5 `gt scan "<padrão>"`

- Padrão AOB: bytes em hex separados por espaço, curinga `??` ou `?`. Ex.: `48 8B ?? ?? 05 E8`.
- Padrão inválido: erro dizendo qual token está errado.
- Área de busca padrão: seções executáveis do módulo. `--section <nome>` restringe a uma seção;
  `--all` busca todas as seções do módulo.
- Lista **todas** as ocorrências (com limite de exibição, padrão 50, e o total real), cada uma com
  endereço e `módulo+offset`. O resumo diz explicitamente "único", "nenhum" ou "N ocorrências"
  (regra do `AGENTS.md` 4: só "único" serve como assinatura).
- A busca deve encontrar ocorrências que atravessem a fronteira entre blocos de leitura.
- Por padrão a busca é na **memória**. Com `--disk`, busca nas mesmas seções do arquivo em disco (mesmo
  acesso e mesma checagem de versão da 6.7). Use `--disk` para validar assinaturas, porque um patch de outro
  trainer ativo na memória pode esconder uma ocorrência. A saída informa a origem dos dados (memória ou disco).

### 6.6 `gt disasm <endereço> [-n N]`

- Desmonta `N` instruções (padrão 20) a partir do endereço, em sintaxe Intel.
- Cada linha: endereço (`módulo+offset`), bytes da instrução e a instrução.
- Destinos de `call`/`jmp` e operandos relativos ao RIP aparecem resolvidos como `módulo+offset` quando
  caem num módulo conhecido.
- Começar no meio de uma instrução é responsabilidade de quem chamou; o comando não tenta corrigir.

### 6.7 `gt diff`

Compara o código na memória com o mesmo executável em disco e mostra o que foi alterado
(por exemplo, por outro trainer).

- **Arquivo em disco:** o mesmo que o processo carregou, conforme `/proc/<pid>/maps`. O jogo roda no
  container pressure-vessel, então o caminho visto em `maps` deve ser aberto pelo ponto de vista do
  processo (`/proc/<pid>/root/...`) ou por meio equivalente. Somente leitura.
- **Sem ruído esperado:** em 2026-10-24, com o FLiNG sem patch ativo, a `.text` inteira na memória era
  idêntica ao disco (0 diferenças), lida por `/proc/<pid>/root/<caminho em maps>`, com base igual ao `ImageBase`.
- **Mesma versão:** se `TimeDateStamp` ou `SizeOfImage` do arquivo forem diferentes dos da memória
  (jogo atualizado com ele aberto), recusar com mensagem explicando.
- **Realocação:** se a base na memória for diferente do `ImageBase`, aplicar as realocações do `.reloc`
  aos bytes do disco antes de comparar, para não reportar diferenças falsas.
- **Área:** seções executáveis por padrão; `--section <nome>` para outra. Fora das seções executáveis
  (`.rdata`, `.data`), diferenças são normais (tabela de importação preenchida, dados do jogo) e a saída
  avisa isso no início.
- **Saída:** diferenças agrupadas em trechos (diferenças a menos de 16 bytes uma da outra viram um trecho
  só). Para cada trecho:
  - endereço (`módulo+offset`) e tamanho;
  - bytes originais × bytes atuais;
  - desmontagem do original e do atual (algumas instruções antes e depois do trecho);
  - se o atual for um desvio (`jmp rel32`, `jmp [rip+x]`, `call rel32`), o **endereço de destino** e se ele
    fica num módulo confiável, num módulo de terceiros ou fora de todos os módulos ("provável código
    injetado"). Para `jmp [rip+x]`, o ponteiro só é lido se o slot `[rip+x]` estiver num módulo confiável;
    senão o destino aparece como "não resolvido".
  - **Não** desmontar nem ler o código no destino quando ele estiver fora dos módulos (ver 6.12).
- Total de trechos no final. Zero diferenças é um resultado válido ("nenhuma alteração").

### 6.8 `gt dump <seção | endereço tamanho> [-o arquivo]`

- Salva bytes da memória em arquivo, para abrir em ferramentas externas (ex.: Ghidra).
- Por nome de seção (`.text`, `.rdata`...) ou por intervalo.
- Destino padrão: `scratch/dumps/<chave-de-versão>/<nome>.bin`, relativo ao diretório atual (na raiz do
  repositório, `scratch/` é ignorado pelo git). `<nome>` é a seção sem o ponto inicial (`text.bin`) ou o
  endereço inicial em hex. Sem chave de versão (alvo sem PE), a pasta é `pid<PID>`. O `.json` tem o mesmo
  nome do `.bin` (`text.json`).
- Sujeito à guarda da 6.12.
  Grava ao lado um `.json` com base, endereço inicial, tamanho e chave de versão.
- Páginas ilegíveis viram zeros e são listadas no `.json`.
- Nunca sobrescreve um arquivo existente sem `--force`.

### 6.9 `gt fling`

Automatiza a pesquisa feita à mão em `RESEARCH.md`: lê os pontos que o FLiNG encontrou e mostra o estado de cada um.

- **Fonte:** o `symbols.ini` que o FLiNG grava no prefixo do Proton do jogo, em
  `.../compatdata/2661300/pfx/drive_c/users/steamuser/AppData/Local/FLiNGTrainer/trainer-logs/grounded-2-trainer/version/<id>/symbols.ini`.
  - Biblioteca Steam padrão: `~/.local/share/Steam/steamapps`. `--steam-library <caminho>` para outra.
  - **`<id>` é o `TimeDateStamp` do PE em decimal** (`0xc935e799` = `3375753113`). O `fling` usa a pasta
    cujo `<id>` bate com o jogo aberto. Se ela não existir, erro listando as pastas encontradas (offsets de
    outra build dariam resultados errados).
  - `--ini <caminho>` escolhe um arquivo direto; se o `<id>` da pasta dele não bater com o jogo, avisar.
- **Formato** (verificado em 2026-10-24): UTF-16LE com BOM (`FF FE`), quebras CRLF, seção `[default]` e linhas
  `nome=Grounded2Steam-Win64-Shipping.exe+56FC11C`. Só traz `módulo+offset`, sem padrão nem estado, e continua
  no disco com o FLiNG fechado. O formato também é registrado em `RESEARCH.md`.
- **Para cada símbolo:** nome, endereço (`módulo+offset`), estado, desmontagem curta do código atual e, se
  alterado, o destino do desvio.
  - **Alterado** = algum byte em `[endereço, endereço + 16)` difere do disco (mesma comparação da 6.7).
    Os desvios do FLiNG têm de 6 a 8 bytes e começam no endereço do símbolo.
  - O mesmo limite da 6.12 vale: nada do código do FLiNG é desmontado.
  - **Limitação conhecida:** a janela de 16 bytes vem da observação de dois símbolos (`RESEARCH.md`). Um patch
    fora da janela, ou um cheat feito só pela DLL injetada (ex.: velocidade), aparece como "original". A saída
    do texto deixa claro que "original" significa "sem alteração nos 16 bytes a partir do endereço".
- Se o FLiNG não estiver aberto, os pontos aparecem como "original"; isso não é erro.

### 6.10 Saída e erros

- Texto legível em pt-BR por padrão; `--json` produz um único objeto JSON por execução, estável o bastante
  para scripts (campos documentados no código).
- Erros com causa e ação sugerida. Casos obrigatórios:
  - `EPERM` (`process_vm_readv`) ou `EACCES` (`/proc/<pid>/mem`) ao ler: explicar `ptrace_scope` e mostrar o
    valor atual. Nunca sugerir `sudo` como primeira opção.
  - Endereço fora de qualquer região mapeada.
  - O processo terminou durante o comando.
- Código de saída diferente de 0 em erro.
- Logs (`tracing`) vão para stderr, controlados por `RUST_LOG`; a saída do comando vai para stdout.

### 6.11 Escrita de memória na biblioteca

`gt-core` ganha a primitiva de escrita (`pwrite` em `/proc/<pid>/mem`, `ARCHITECTURE.md` 5.1), mas:
- é só a escrita **crua**, sem guardar bytes originais nem verificar bytes esperados. A Fase 2 constrói essas
  garantias (`AGENTS.md` 3) por cima dela;
- nenhum comando do `gt` a chama;
- ela só é exercitada nos testes contra o processo-alvo de teste (seção 7).

### 6.12 Guarda contra código de terceiros

Decisão do dono: a regra "nunca desmontar código de outros trainers" é uma garantia da ferramenta, não só
de conduta.

- A guarda avalia **intervalos de bytes**, não regiões inteiras do `maps`: o kernel funde regiões vizinhas com
  as mesmas permissões, então uma região anônima executável pode cobrir o fim de um módulo confiável e um
  trecho fora dele. Só a parte fora é código de terceiros.
- **Código de terceiros** = bytes de região **executável** (permissão `x` em `maps`) que satisfazem uma destas:
  - é anônima e não fica dentro de nenhum módulo confiável (6.1). É assim que aparece um código injetado
    (ex.: a *cave* do FLiNG). A `.text` do jogo também é anônima, mas fica dentro da extensão de um módulo
    confiável e por isso é permitida;
  - fica dentro da extensão de um **módulo de terceiros** (6.1), como uma DLL injetada.

  Regiões executáveis com arquivo que não é PE (bibliotecas `.so` do Wine e do sistema, binários ELF) são
  permitidas. Dados (sem `x`) de um módulo de terceiros, como o cabeçalho e a `.rdata`, são permitidos.
- "Anônima" = sem caminho em `maps`, ou com pseudo-caminho `[heap]`, `[stack]` ou `[anon:...]`. `[vdso]` e
  `[vsyscall]` são do kernel e permitidos. Um arquivo marcado `(deleted)` conta como arquivo.
- Uma região não executável (dados) nunca é código de terceiros.
- `read`, `disasm` e `dump` recusam qualquer intervalo que toque código de terceiros, com a mensagem
  "código fora dos módulos do jogo/Wine (pode ser de outro trainer): bloqueado pela regra do projeto".
- **Falsos positivos aceitos:** em 2026-10-24, com o FLiNG fechado, o jogo já tinha 6 regiões executáveis
  anônimas fora dos 129 módulos PE (uma de 64 KB `rwxp` em `0x7700000` e cinco páginas `r-xp` isoladas no topo
  do espaço de endereços, provavelmente do Wine ou do driver). A guarda bloqueia essas também. Isso é aceito:
  nenhuma delas é alvo de pesquisa, e o erro é do lado restritivo.
  `read` de dados (vida, ponteiros, floats) continua livre.
- `diff` e `fling` nunca leem bytes no destino de um desvio que caia em código de terceiros.
- `scan` e `diff` só operam em módulos confiáveis. Com `--module` apontando para um módulo de terceiros,
  `inspect` funciona (só lê o cabeçalho), e `scan`, `diff` e `dump` de seção executável são recusados.
- Testes:
  - **unitário** com mapa de memória sintético: região executável anônima fora dos módulos é recusada, `.text`
    anônima dentro do módulo confiável é aceita, PE de terceiros é recusado (inclusive quando é o `--module`),
    região de arquivo ELF é aceita, região de dados anônima é aceita, região fundida que cruza o fim de um
    módulo confiável é aceita até o fim do módulo e recusada depois dele, PE em caminho forjado
    (`/tmp/steamapps/common/Proton x/`) é de terceiros;
  - **integração (CLI)**: o processo-alvo de teste cria uma página executável anônima com `mmap`, e
    `gt read`/`disasm`/`dump` nela são recusados.

### 6.13 Processo-alvo de teste e `unsafe`

O alvo precisa de `mmap` com `PROT_EXEC` (e, para funcionar com `ptrace_scope = 1`, de
`prctl(PR_SET_PTRACER, PR_SET_PTRACER_ANY)`), que são `unsafe`. Para cumprir a regra do `AGENTS.md` 5, esse
código fica em `gt-core/src/mem/` (módulo de apoio a testes, atrás de uma feature `test-support`), com
`// SAFETY:`. O binário do alvo só chama funções seguras desse módulo.

## 7. Testes

| Nível | O que cobre | Exige jogo? |
|---|---|---|
| Fixtures | Primeiro passo do plano, com o jogo aberto (pedido ao dono): copiar `/proc/<pid>/cmdline` e `/proc/<pid>/maps` do jogo para `crates/gt-core/tests/fixtures/` (trocando o nome de usuário no caminho por `user`). O `symbols.ini` real já está no disco e é copiado sem precisar do jogo. Casos que podem não aparecer no `maps` real (`(deleted)`, pseudo-caminhos) entram como linhas sintéticas | Sim, uma vez (só `cmdline`/`maps`) |
| Unitário | Parse de padrão AOB; busca com curingas e na fronteira de blocos; parse de endereço; parse de `/proc/<pid>/cmdline` e `/proc/<pid>/maps` a partir das fixtures reais (incluindo caminhos com espaços, como `Proton - Experimental` e `Program Files (x86)`, `(deleted)` e pseudo-caminhos); cabeçalho PE a partir de bytes sintéticos; aplicação de realocações; agrupamento de diferenças; critério "alterado" da 6.9; guarda da 6.12; hexdump; parse do `symbols.ini` (UTF-16LE) a partir de uma cópia do arquivo real | Não |
| Integração (API) | Processo-alvo de teste (binário Rust simples, ELF, com um marcador conhecido em memória), usando a API do `gt-core`: abrir pelo PID, achar a região do marcador em `maps`, ler o marcador, a busca AOB acha o marcador uma vez nessa região, a escrita da 6.11 altera o marcador e a leitura confirma | Não |
| Integração (CLI) | Contra o mesmo alvo, com `--module <nome do binário de teste>`: `gt read <endereço absoluto>` mostra o marcador; `gt inspect` falha com "não é uma imagem PE"; `read`/`disasm`/`dump` na página executável anônima são recusados (6.12) | Não |
| Jogo (`#[ignore]`) | `inspect` acha o módulo e as seções; leitura do cabeçalho PE confere `MZ`/`PE`. Testes com bytes específicos de uma versão só rodam se a chave de versão for a esperada; senão, são pulados com aviso | Sim |

`cargo fmt --check`, `clippy -D warnings` e `cargo test --workspace` passam (AGENTS.md 6).

## 8. Critério de pronto

Com o jogo aberto num save de teste e o FLiNG aberto com **usos/cargas infinitas** e **quantidade ao
arrastar** ligados:

1. `gt inspect` mostra base, seções e a chave de versão.
2. `gt disasm Grounded2Steam-Win64-Shipping.exe+56FC11C` mostra o código do god mode registrado em `RESEARCH.md`.
3. `gt diff` encontra sozinho os dois patches do FLiNG (`+57BD6D0` e `+5BBFE8C` na build `c935e799`),
   mostrando o desvio para fora do módulo.
4. `gt fling` lista os símbolos do FLiNG com os dois acima como "alterado".

Se o jogo tiver sido atualizado desde 2026-10-22, os offsets mudam: o critério passa a ser encontrar os
mesmos dois patches onde eles estiverem, e `RESEARCH.md` é atualizado com a nova versão.

Eu mostro ao dono a saída lado a lado com `RESEARCH.md`. O dono confirma.

**O dono faz:** abrir o jogo duas vezes quando eu pedir: no início do plano, só para capturar as fixtures
(seção 7), e no fim, num save de teste com o FLiNG e as duas opções ligadas.

## 9. Decisões tomadas

| Decisão | Quem decidiu |
|---|---|
| Nenhum comando escreve no jogo; a escrita existe só na biblioteca, testada fora do jogo | Dono |
| Comandos extras `read`, `fling` e `dump` | Dono |
| Texto por padrão + `--json` | Dono |
| Critério de pronto do roadmap | Dono |
| Desvios do FLiNG: só mostrar o destino, nunca desmontar o código dele | Dono |
| Offsets sempre hexadecimais; atalho `+offset` | IA |
| Opções globais `--pid` e `--module` (permitem testar com processo-alvo próprio) | IA |
| Agrupar diferenças a menos de 16 bytes; limite de 50 ocorrências exibidas no `scan` | IA |
| `dump` em `scratch/dumps/`, nunca sobrescrevendo sem `--force` | IA |
| Bloquear em `disasm`/`dump` o código executável fora dos módulos (6.12) | Dono |
| `fling` escolhe a pasta pelo `TimeDateStamp`; "alterado" = diferença em `[endereço, endereço + 16)` | IA |
| Extensão de módulo por `SizeOfImage`; módulos conhecidos = todas as imagens PE do processo | IA |
| Alvo sem PE: só `read` funciona; testes de integração usam a API do `gt-core` | IA |
| Guarda da 6.12 também no `read` (só para código executável) e para PEs fora do jogo/Proton/Windows | IA, seguindo a decisão do dono |
| Leitor e escritor separados; escritor atrás da feature `write`, que o `gt-cli` não ativa | IA |
| Política de módulos confiáveis: diretórios absolutos comparados por componentes, montados em `gt-grounded2`; sem exceção para `--module` | IA |
| Guarda avaliada por intervalo de bytes (regiões fundidas) | IA |
| `unsafe` do alvo de teste em `gt-core/src/mem/` atrás da feature `test-support` | IA |
| Casamento do processo pelo primeiro argumento, tolerando argumentos juntados com espaço | IA |
| `scan --disk`; tamanho de seção = `VirtualSize`; tamanhos decimais ou `0x` | IA |

## 10. Deixado para quem implementa

- Organização interna dos módulos, tamanho dos blocos de leitura e algoritmo de busca.
- Formato exato do JSON (campos estáveis e documentados).
- Como ler o `ProductVersion` do recurso de versão.
- Como o processo-alvo de teste é construído e iniciado pelos testes.
- Como achar a fronteira de instrução para desmontar "algumas instruções antes" de um trecho no `diff`.
- Como lidar com o lint `print_stdout` no `gt-cli` (ex.: `writeln!` em stdout).
