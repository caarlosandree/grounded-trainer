# Pesquisa — engenharia reversa do Grounded 2

Achados sobre o jogo, sempre com a versão em que foram observados. Hipóteses descartadas também ficam registradas.

## Versão observada: build Steam `26054455` (2026-10-22)

| Campo | Valor |
|---|---|
| Atualizado na Steam | 2026-10-20 20:44 |
| `ProductVersion` | `Augusta-CL-3207493` |
| PE `TimeDateStamp` (hash de build) | `c935e799` |
| `SizeOfImage` | `0x0B27C000` |
| Tamanho do arquivo | 182.806.048 bytes |
| Base na memória | `0x140000000` |
| Processo | `Grounded2Steam-Win64-Shipping.exe Augusta` |

### Seções

| Seção | VMA | Tamanho |
|---|---|---|
| `.text` | `0x140001000` | `0x07A8E4F1` |
| `.rdata` | `0x147A90000` | `0x026F326C` |
| `.data` | `0x14A184000` | `0x00298400` (*) |
| `.pdata` | `0x14A840000` | `0x0069E790` |
| `.rsrc` | `0x14AF3E000` | `0x00011B40` |
| `.reloc` | `0x14AF50000` | `0x0032B428` |

(*) Correção em 2026-10-24: o valor da `.data` é o `SizeOfRawData`; o `VirtualSize` é `0x006BBECC`. As
demais são `VirtualSize`. O PE tem 8 seções; esta tabela omitiu `.msvcjmc` e `_RDATA`.

## Trainer FLiNG "Early Access Plus 38" (2026-08-17) na versão atual

**Como foi obtido:** o trainer grava no prefixo do jogo
`drive_c/users/steamuser/AppData/Local/FLiNGTrainer/trainer-logs/grounded-2-trainer/version/3375753113/symbols.ini`
os padrões que encontrou. Só **11 de ~38** opções acharam o padrão na versão atual.

| Símbolo FLiNG | Endereço | Estado observado |
|---|---|---|
| `aobgodmode` | `+56FC11C` | Encontrado, **mas o God Mode não funciona no jogo** (relato do dono) |
| `aobdefmultiplier` | `+5943EC8` | Encontrado |
| `aobcharges` | `+57BD6D0` | Encontrado e **patch aplicado** |
| `aobdragitemamount` | `+5BBFE8C` | Encontrado e **patch aplicado** |
| `aobgamemodesettings` | `+593AF1C` | Encontrado (em 2026-10-25: **patch aplicado**) |
| `aobbuildingreg` | `+57689DB` | Encontrado (em 2026-10-25: **patch aplicado**) |
| `aobspeed` | `+37EEE9B` | Encontrado |
| `aobplayerspeed` | `+3785DF2` | Encontrado |
| `aobinfinitejumps` | `+378553E` | Encontrado |
| `aobdaytime` | `+58C8A2A` | Encontrado (em 2026-10-25: **patch aplicado**) |
| `aoblocation` | `+35FB632` | Encontrado |

Correção em 2026-10-25: "patch aplicado" **não** significa "opção ligada". Ver a sessão de 2026-10-25 abaixo.

O trainer contém uma DLL embutida, `TrSpeedHack_x64.dll` (injetada para velocidade do jogo, hooks em `WINMM`/`KERNEL32`).

### Detalhes por ponto

**Comparação feita:** bytes na memória × bytes no arquivo em disco (script de diff, só leitura).

#### `+57BD6D0` — cargas/usos (patch ativo)
```
original: f3 0f 10 89 74 09 00 00     movss xmm1, [rcx+0x974]
          f3 0f 58 c9                 addss xmm1, xmm1
          f3 0f 5c 0d ...             subss xmm1, [rip+...]
          f3 0f 2d c1                 cvtss2si eax, xmm1
FLiNG:    e9 4b 29 ac 86 0f 1f 00     jmp <cave do FLiNG> ; nop
```
Hipótese: o campo `+0x974` do objeto em `rcx` é um float relacionado a cargas e usos. O FLiNG desvia a execução para um código próprio (*hook* de 8 bytes: `jmp rel32` + `nop` de 3 bytes).

#### `+5BBFE8C` — quantidade ao arrastar item (patch ativo)
```
original: 48 8b f9           mov rdi, rcx
          48 85 c9           test rcx, rcx
          74 09              je  +9
          e8 ...             call 0x1434b68e0
          3b c6              cmp eax, esi
          75 11              jne ...
FLiNG:    e9 9f 01 6c 86 90  jmp <cave do FLiNG> ; nop
```

#### `+56FC11C` — god mode (não funciona)
```
e8 ef ab 2e fe           call 0x1439e6d10
48 83 be b0 00 00 00 00  cmp qword ptr [rsi+0xb0], 0
48 8b d8                 mov rbx, rax
0f 57 ff                 xorps xmm7, xmm7
0f 84 cc 0c 00 00        je ...
```
- Nenhum byte alterado num raio de ±1 KB no momento da leitura.
- **Pendente:** confirmar se o God Mode estava ligado no trainer durante a leitura. Se estava, o FLiNG não aplicou o patch. Se não estava, repetir a leitura com ele ligado.
- Hipótese: `[rsi+0xb0]` é um ponteiro de componente verificado antes do cálculo de dano. A lógica de dano pode ter mudado de lugar na atualização de 20/10.

## Ambiente

- `kernel.yama.ptrace_scope = 0`: leitura de `/proc/<pid>/mem` funciona como usuário comum.
- O FLiNG roda com `protontricks-launch --appid 2661300` e enxerga o jogo (PID do Wine: `364`).
- Os avisos `Fontconfig ... out of memory` do container são inofensivos.
- (2026-10-24) `cmdline` do jogo: `S:\steamapps\common\Grounded2\Augusta\Binaries\Win64\Grounded2Steam-Win64-Shipping.exe`, `Augusta` (separados por NUL). O launcher aparece como `S:\steamapps\common\Grounded2\Grounded2.exe`.
- (2026-10-24) No `maps`, `.text` (`140001000-147a90000 r-xp`) e `.rdata` são anônimas; só cabeçalho, `.data` e `.reloc` aparecem com o nome do exe. 129 imagens PE carregadas. Com o FLiNG fechado, há 6 regiões executáveis anônimas fora de qualquer imagem PE (`0x7700000` 64 KB `rwxp` e cinco páginas `r-xp` isoladas perto de `0x7f43ec98d000`).
- (2026-10-24) Com o FLiNG sem patch ativo, a `.text` na memória é idêntica ao arquivo em disco.
- (2026-10-24) `symbols.ini` do FLiNG: UTF-16LE com BOM, CRLF, seção `[default]`, linhas `nome=Grounded2Steam-Win64-Shipping.exe+OFFSET`. A pasta `version/<id>` é o `TimeDateStamp` do PE em decimal (`0xc935e799` = `3375753113`).

## Sessão de 2026-10-25: confirmação com o `gt` (fim da Fase 1)

Mesmo build (`c935e799-0b27c000`, `Augusta-CL-3207493`). Jogo aberto num save, FLiNG aberto e conectado,
**nenhuma opção ligada** (relato do dono). Comandos: `gt inspect`, `disasm`, `diff`, `fling`, `scan`, `read`, `dump`.

- `gt inspect`: base, `SizeOfImage`, `ProductVersion` e as 8 seções iguais às tabelas acima.
- `gt disasm +56FC11C`: mesmas instruções registradas acima, sem patch.
- `gt diff` (`.text` na memória × disco) achou **5** trechos alterados, todos `jmp rel32` (+ `nop`) para
  uma região executável fora de qualquer módulo (código do FLiNG, que o `gt` se recusa a ler):

  | Endereço | Símbolo FLiNG | Bytes | Original | Desvio |
  |---|---|---|---|---|
  | `+57689DB` | `aobbuildingreg` | 6 | `88 9f d1 05 00 00` (`mov [rdi+0x5d1], bl`) | `0xDCB10050` |
  | `+57BD6D0` | `aobcharges` | 7 | `f3 0f 10 89 74 09 00` | `0xDCB10020` |
  | `+58C8A2A` | `aobdaytime` | 7 | `f2 0f 58 83 70 01 00` (`addsd xmm0, [rbx+0x170]`) | `0xDCB10090` |
  | `+593AF1C` | `aobgamemodesettings` | 7 | `48 8b 88 48 01 00 00` (`mov rcx, [rax+0x148]`) | `0xDCB10040` |
  | `+5BBFE8C` | `aobdragitemamount` | 6 | `48 8b f9 48 85 c9` | `0xDCB10030` |

- **O FLiNG instala os desvios ao se conectar, mesmo sem nenhuma opção ligada.** Hipótese: cada opção só
  liga uma chave dentro do código do FLiNG. Então os patches vistos em 2026-10-22 não provam que as opções
  estavam ligadas. Por que só 2 desvios apareceram em 2026-10-22: não verificado.
- O destino do `jmp` muda a cada sessão (`0xCC28xxxx` em 2026-10-22/24, `0xDCB1xxxx` agora). O FLiNG aloca
  a área do código dele em lugar novo a cada vez, então só o **local** do patch é estável, não os bytes.
- O padrão original das cargas (`f3 0f 10 89 74 09 00 00 f3 0f 58 c9`) aparece **2 vezes** no executável
  em disco (`+57BD6D0` e `+57C2C10`). A assinatura do nosso patch (Fase 3) precisa de mais contexto para
  ser única.
- O padrão do god mode (`e8 ?? ?? ?? ?? 48 83 be b0 00 00 00 00 48 8b d8`) é único em memória e em disco.

## Sessão de 2026-10-25 (Fase 2, sessão A): portão da pausa e reconferência do `gt scan`

Mesmo build (`c935e799-0b27c000`, `Augusta-CL-3207493`), Proton 11.0. Jogo num save, FLiNG aberto e conectado,
nenhuma opção ligada (relato do dono).

### Pausa por SIGSTOP/SIGCONT (portão da spec 002 6.5)

- Teste `pause_gate` (`crates/gt-grounded2/tests/game_pause.rs`): **100 pausas** a cada 300 ms, sem escrita.
  Nenhuma nova tentativa por prazo; **148 threads** por pausa em média; tempo até o processo inteiro parar:
  mín 10,1 ms, médio 15,4 ms, máx 24,2 ms. Todas as posições legíveis em `/proc/<tid>/syscall`; depois de cada
  SIGCONT, nenhuma thread ficou parada. O dono confirmou que o jogo não engasgou.
- Uma thread (`tid 150444`) foi vista uma vez em estado `D` antes de uma pausa (não atrasou a parada).
- O processo pai do jogo é o `pv-adverb` do pressure-vessel. O efeito do `SIGCHLD` das pausas nele não foi
  medido; o jogo seguiu normal.
- **Achado:** o `wineserver` do Proton se anexa por ptrace à thread principal (`GameThread`, tid = pid) por
  instantes (`TracerPid` = pid do `wineserver`), provavelmente para atender às leituras de memória que o FLiNG faz
  pela API do Windows. Enquanto anexada, a thread aparece como `running` no `syscall` e até `R` no `stat`, mesmo com
  o processo pausado (56 de 300 leituras seguidas). O motor relê essa posição (até 50×2 ms) em vez de tratá-la como
  thread rodando. Com o FLiNG fechado o efeito não foi medido.
- `pc` mais frequentes com o jogo pausado: três endereços fora do executável (`0x7f25ff6ca2fd` 9544×,
  `0x7f25ff6cc3bd` 2436×, `0x7f25ff653352` 2263×; bibliotecas do Wine/sistema, threads esperando em syscall).
  No executável, os mais vistos foram `+112c464` (17×), `+26109c5` (10×), `+2cb577b` e `+4a59225` (6× cada).

### `gt scan` com o FLiNG conectado (spec 002 7.1, commit `85bfa2f`)

| Padrão | Memória | Disco |
|---|---|---|
| God mode `e8 ?? ?? ?? ?? 48 83 be b0 00 00 00 00 48 8b d8` | único (`+56fc11c`) | único (`+56fc11c`) |
| Cargas `f3 0f 10 89 74 09 00 00 f3 0f 58 c9` | 1 (`+57c2c10`) | 2 (`+57bd6d0`, `+57c2c10`) |

- O FLiNG alterou a ocorrência de `+57bd6d0` na memória, e a outra passou a parecer "única": é o caso que o
  `--disk` (e a exigência de unicidade em disco da spec 002 6.4) evita.
- `gt scan --all` com o padrão do god mode: "único", sem bloqueio da guarda e sem veredito `inconclusive` (nenhuma
  página ilegível nas seções).
- A DLL do FLiNG (`TrSpeedHack_x64.dll`) **aparece no `maps`** com o FLiNG conectado e nenhuma opção ligada;
  `gt scan --module TrSpeedHack_x64.dll` é recusado pela guarda (código de terceiros).
- Extensão dos módulos (spec 002 6.12): 131 módulos confiáveis, **0** com `SizeOfImage` diferente entre memória e
  disco; `gt inspect` mostra "Extensão do módulo: conferida com o disco".

## Sessão de 2026-10-25 (Fase 2, sessão B): patch de teste e critério de pronto

Mesmo build (`c935e799-0b27c000`). Jogo reiniciado, FLiNG **fechado** (`gt diff`: nenhuma alteração na `.text`
antes de começar), dono andando pelo mapa, backup do save feito.

### Perfil por amostragem (`gt-patch profile`, 200 pausas a cada 300 ms)

| Função (`.pdata`) | Amostras | `pc` mais frequente |
|---|---|---|
| `+112c3f3`–`+112c4f6` | 44 | `+112c464` (43×) |
| `+2610831`–`+2610df0` | 41 | `+26109c5` (16×) |
| `+4a85057`–`+4a86953` | 21 | espalhados |
| `+4a8e611`–`+4a8e9c0` | 17 | `+4a8e78a` (3×) |

A função `+112c3f3` é uma espera ativa de um pool de threads (`pause; rdtsc; shl rdx,0x20; or rax,rdx;
sub rax,rdi; cmp rax,rbx; jb` de volta a `+112c460`), executada o tempo todo por várias threads.

### Patch de teste `teste-codificacao`

- Local: `Grounded2Steam-Win64-Shipping.exe+112c468`, dentro do laço acima, no mesmo bloco básico da amostra
  `+112c464` (o bloco vai de `+112c460`, alvo do `jb`, até o `jb` em `+112c471`; nenhum desvio ou alvo entre
  `+112c464` e `+112c468`). Fora dos 5 locais do FLiNG.
- Troca: `or rax, rdx` `48 0B C2` → `48 09 D0` (a mesma instrução na outra codificação; mesmo tamanho, mesmo
  efeito e mesmas flags). Mesmas fronteiras de instrução (nenhuma fronteira perdida).
- Assinatura: `f3 90 0f 31 48 c1 e2 20 48 0b c2 48 2b c7 48 3b c3 72 ed`, `offset = 8`; **única** na memória e no
  disco (`gt scan` e `gt scan --disk`, ocorrência em `+112c460`). Registrada em `signatures/generic.toml` e
  `signatures/c935e799-0b27c000.toml` (`rva = 0x112c468`).

### Critério de pronto (spec 002, seção 9)

1. `gt-patch list`: `teste-codificacao  disponível (verificado)  …+112c468`.
2. `gt-patch toggle teste-codificacao --times 100`: 100 ciclos, **0 novas tentativas** (perigo e prazo), pausa
   média **14,1 ms**, máxima **16,3 ms**; ~35 s no total. O dono confirmou que o jogo não engasgou. `gt diff`
   depois: nenhuma alteração.
3. `gt-patch run` + SIGINT e + SIGTERM: com o patch ativo o `gt diff` mostra 1 trecho; depois de cada sinal,
   `restaurado: teste-codificacao`, código 0, e `gt diff` sem alteração.
4. `gt-patch run` + SIGKILL: o patch continua no jogo, o `list` mostra `pendente` e "Diário pendente: sim";
   `gt-patch restore` imprime `restaurado: teste-codificacao` e o `gt diff` volta a "nenhuma alteração".
   (Na primeira tentativa o `kill -9` atingiu o subshell que lançou o `gt-patch`, não ele; o `restore` então
   recusou corretamente com "outro gt-patch já está em execução".)
5. `gt-patch run` e o dono fecha o jogo: "o jogo fechou; nada a restaurar", código 0, diário limpo.

Observação: um `gt-patch list` rodado no instante em que o `run` aplicava mostrou `bytes diferentes` uma vez
(a busca viu os bytes originais e a releitura já viu os novos); na consulta seguinte, `ativo`. Transitório.

Os testes `#[ignore]` de `apps/gt-patch/tests/game.rs` não foram rodados nesta sessão (o jogo foi fechado no
item 5); os passos manuais acima cobrem o mesmo comportamento.

## Sessão de 2026-10-25 (Fase 3): análise estática do trainer FLiNG

Mesmo build do jogo (`c935e799-0b27c000`). Alvo: `Grounded 2 Early Access Plus 38 Trainer Updated 2026.08.17.exe`
(PE32+ x86-64, 1.823.232 bytes, SHA-256 `33104962d39aa0ee74a8cea857bfb8a01f775b435af8b3be6c3584ea6353bc16`).
Sem jogo aberto. Nada do código do FLiNG foi copiado para o projeto (`AGENTS.md` 3); a extração e as ferramentas
ficam em `scratch/fling/` (ignorado pelo git).

**Como foi feito:** entropia das seções (nada empacotado: `.text` 6,4; só a `.rsrc` alta, 7,6), `strings` ASCII e
UTF-16, e o REA 6.7.0 (`rea analyze/search/xrefs/decompile --provider ghidra`, Ghidra 12.1.4) para o código nativo.

### Arquitetura do trainer

- **Os cheats são scripts no formato do auto-assembler do Cheat Engine**, guardados em texto puro na `.rdata`
  (`[ENABLE]`/`[DISABLE]`, `aobscanmodule`, `alloc`, `label`, `registersymbol`, `db`). O trainer traz o próprio
  interpretador desses scripts, montado sobre o **AsmJit** (mensagens de erro do AsmJit nas strings).
- Sintaxe própria nos padrões: `s1.N` é um curinga de N bytes cujo valor é **capturado** em `s1` (o offset do
  campo); `s1.2.*.0x800.0xF00` limita o valor capturado a uma faixa. O `[DISABLE]` restaura `db … s1 …` com o
  valor capturado, ou seja, o original é reconstruído, não guardado.
- Cada script tem um alvo `aobscanmodule(...)` + uma cave alocada perto do alvo (`alloc(newmem,$1000,alvo)`), com
  variáveis de controle no fim da cave (`newmem+200`, `+400`). Ligar uma opção no menu **só escreve 1 numa
  variável da cave**; o desvio é instalado ao conectar. Isso explica a observação de 2026-10-25 (desvios com
  todas as opções desligadas).
- Há uma lista de nomes de executável (`Grounded2Steam-Win64`, `Grounded2-WinGRTS`, `-WinGDK`, `-Win64`) e o
  módulo do script é trocado por ela: scripts marcados `WinGRTS` também valem na versão Steam.
- Existe um modo por linha de comando (`--godmode`, `--speed_f` etc.) e as teclas `Num`/`Ctrl+Num`/`Alt+Num`.
- A velocidade do jogo (`Set Game Speed`) usa a DLL embutida `TrSpeedHack_x64.dll`, não um script.

### Leitor de reflexão do Unreal embutido

- O trainer lê a **reflexão do Unreal em tempo real**: acha `GUObjectArray` e o `FNamePool` por padrões AOB
  genéricos de UE4/UE5, percorre classes e propriedades (`IntProperty`, `FloatProperty`, `StructProperty`, …) e
  preenche uma tabela `Classe_Campo = offset` antes de instalar os scripts. Strings de progresso: "All unreal
  engine structures and enums processed!".
- Tabela usada (`//Maine.<Classe>=<Apelido>` + `Apelido_Campo:`), em `FUN_14007f070` (`UE_OFFSETS_CHECKED_`):
  `SurvivalCharacter` → `HealthComponent`, `BlockComponent`, `ReactionComponent`, `StaminaComponent`,
  `BodyTemperatureComponent`, `HaulingComponent`, `SurvivalComponent`; `HealthComponent.MaxHealth`/`CurrentDamage`;
  `StaminaComponent.CurrentStamina`/`MaxStamina`; `BlockComponent.PerfectBlockWindow(Mounted)`;
  `ReactionComponent.CurrentStunValue`; `BodyTemperatureComponent.CurrentBodyTemperature`;
  `HaulingComponent.Capacity`; `SurvivalComponent.CurrentFood`/`CurrentWater`/`CurrentBreath`; `Item.StackSize`;
  `PartyComponent.ScienceFound`/`PartyUpgradePoints`/`PersonalUpgradePoints`;
  `SurvivalGameModeSettings.bAutoCompleteBuildings`/`bRecipesRequireIngredients`/`bCreaturesIgnorePlayers`/
  `FallDamageMultiplier`; `ResourceAnalyzerBuilding.MaxPowerCapacity`; enum `EBuildingState` (`Built`,
  `UnderConstruction`). O módulo do jogo no Unreal se chama **`Maine`**.
- Os scripts que usam essa tabela só agem quando `UE_OFFSETS_CHECKED = 1`.
- `GetPlayerDamageMultiplier` é resolvido por um padrão que acha uma referência à função (`FUN_140089080`).
- Consequência para a Fase 5: os offsets podem vir da reflexão do jogo em execução, sem gerar SDK com o
  Dumper-7 a cada atualização. **Proposta**, não decisão; ver `ARCHITECTURE.md` quando a Fase 5 começar.

### Os padrões contra o executável atual

`scratch/fling/scan_fling.py` converte os padrões do FLiNG e busca na `.text` do exe em disco. Resultado: os
mesmos **11** endereços do `symbols.ini`, o que confirma a leitura dos scripts. Ausentes (0 ocorrências):
`aobplayerpawn` (as duas variantes), `aobplayerstats`, `aobdmg`, `aobdmgmul`, `aobpartycomponent`, `aobitemcd`,
`aobitemdurability`, `aobitempower`, `aobresearchlevelup`, `aobgravity`, `aoboffsetcheck`.

### Por que o God Mode não funciona (causa provável)

- O script `aobplayerpawn` é um *hook* que grava o ponteiro do pawn do jogador numa variável `player`. **Ele não
  acha o padrão na versão atual**, então `player` nunca existe.
- Dependem de `player`: god mode, estatísticas (vida, stamina, comida, água, oxigênio, temperatura), multiplicador
  de defesa, velocidade de movimento, velocidade do jogador/IA, pulo infinito, gravidade e localização.
- Com o FLiNG conectado, os 6 encontrados que dependem de `player` (`godmode`, `defmultiplier`, `speed`,
  `playerspeed`, `infinitejumps`, `location`) **não** tinham desvio; os 5 com desvio são exatamente os que não usam
  `player` (`buildingreg`, `charges`, `daytime`, `gamemodesettings`, `dragitemamount`). Hipótese forte: o script
  falha ao montar por causa do símbolo `player` não registrado. Mesmo que montasse, compararia com zero.
- **Mecanismo do god mode** em `+56FC11C`: depois do `call`, o jogo faz `cmp qword [rsi+0xb0], 0` / `je`. O FLiNG
  troca o `cmp` por uma comparação do retorno do `call` (`rax`) com o pawn do jogador; se for o jogador, `ZF = 1`
  e o `je` seguinte é tomado, como se o componente fosse nulo: o golpe é ignorado ("God Mode/Ignore Hits").
- Para o nosso god mode basta saber qual é o pawn do jogador: pela cadeia `GWorld → … → Pawn` (Fase 5) ou por um
  novo ponto de captura do pawn no build atual.

### O que cada ponto encontrado faz (lido dos scripts)

| Ponto | O que o FLiNG faz |
|---|---|
| `+57BD6D0` cargas | Antes de ler `[rcx+s1]` (cargas atuais, float), copia `ResourceAnalyzerBuilding.MaxPowerCapacity` (int → float) para ele |
| `+5BBFE8C` arrastar item | Ao arrastar, escreve o valor escolhido em `Item.StackSize` do item em `rcx` e zera o pedido (uso único) |
| `+593AF1C` modo de jogo | Lê `SurvivalGameModeSettings` em `[rax+s1]` e força `bRecipesRequireIngredients`, `FallDamageMultiplier` e `bCreaturesIgnorePlayers` |
| `+57689DB` construção | Ao gravar o estado da construção (`mov [rdi+s1], bl`), troca `UnderConstruction` por `Built` |
| `+58C8A2A` hora do dia | No `addsd xmm0, [rbx+s1]` multiplica o incremento de tempo (ou zera, para congelar) |
| `+56FC11C` god mode | Ver acima (depende de `player`) |
| `+5943EC8` defesa | Divide o dano em `[r15]` pelo multiplicador quando `[rsi+0x20]` é o jogador |
| `+37EEE9B` velocidade | Depois de um `call [rax+s1]` virtual, multiplica o retorno (`xmm0`) se `[rbx+0x20]` é o jogador |
| `+3785DF2` velocidade do ator | Escreve um float em `[rcx+0x68]` do ator (jogador ou IA); provável `CustomTimeDilation` |
| `+378553E` pulo infinito | Para o jogador, zera o contador de pulos `[rbx+s1]` e garante máximo ≥ 2 em `[rbx+s1-4]` |
| `+35FB632` localização | Guarda o endereço da posição (`[rax+s1]`, 3 floats) quando `[rax+0x20]` é o jogador; usado pelo teleporte |

Os offsets "`[x+0x20]` é o jogador" sugerem que esses objetos são componentes cujo dono (`Outer`/`Owner`) fica em
`+0x20`. Não verificado.

## Sessão de 2026-10-26 (Fase 3): o pawn do jogador e as tabelas do Unreal

Mesmo build (`c935e799-0b27c000`). Jogo aberto, FLiNG **fechado** (`gt diff`: nenhuma alteração), dono num save.
Tudo **só leitura** (`gt scan`/`gt disasm` e scripts em `scratch/` que leem `/proc/<pid>/mem`).

**O jogo crashou uma vez** logo depois de uma varredura de ~6 GB de regiões anônimas `rw` (37 s lendo
`/proc/<pid>/mem` em blocos de 16 MB). Nenhum OOM ou segfault no kernel e nenhum coredump do jogo; a causa não foi
determinada (ler `/proc/<pid>/mem` não altera o processo). As varreduras seguintes, com blocos de 1 MB, `nice 19` e
1 ms de pausa entre regiões, rodaram 5 vezes sem problema. Manter esse cuidado.

### Substituto do `aobplayerpawn`

- `Grounded2Steam-Win64-Shipping.exe+3e078fe`: `41 8b 87 c8 02 00 00` `mov eax, [r15+0x2c8]` / `c1 e8 04`
  `shr eax, 4` / `a8 01` `test al, 1` / `0f 84 …` `je`. Mesma forma da segunda variante do FLiNG; o padrão deles
  quebrou porque o compilador pôs `mov [rsp+0x98], r12` logo depois de `mov rbx, [r15+0x180]`.
- Função: `+3e07870` (entrada na `.pdata`; os fragmentos `+3e0787b` e `+3e07889` são *chained unwind*). É método
  virtual, sem `call` direto, no slot 460 da vtable `+9051a80` de `BP_SurvivalPlayerController_Augusta_C`.
- `rcx` = o controller; `r15 = [controller+0x318]` (ou `[controller+0x6e8]` se o byte em `+a538889` for ≠ 0; com o
  jogo rodando ele vale 0). **`[controller+0x318]` é o pawn do jogador**: com o dono no save, apontava para
  `BP_SurvivalPlayerCharacter_Gellarde_C`, e `[pawn+0x2c8] = 0x52` (bit 4 ligado, passa no teste).
- Outros campos do controller que apontam para o pawn: `+0x328`, `+0x380`, `+0xfb8`. O pawn aponta de volta para o
  controller em `+0x160`, `+0x2f0`, `+0x2f8`. Nomes dos campos (`Pawn`, `Character`, `AcknowledgedPawn`…) não
  verificados; virão da reflexão.
- Não medido: com que frequência `+3e07870` roda (se por quadro, serve como ponto de captura).

### Tabelas globais do Unreal (achadas na memória)

| Global | Endereço | Como foi achado |
|---|---|---|
| `GUObjectArray` | `+a53bc30` | `InternalIndex` de um CDO levou ao bloco 0; a lista de blocos é lida no padrão de acesso `48 8b 05 ?? ?? ?? ?? 48 8b 0c c8 48 8d 04 d1` (281 ocorrências na `.text`, ex.: `+1374f1e`) |
| `FNamePool` (blocos) | `+a499e50` | Busca de `?? 01 "None" ?? 03 "ByteProperty"` no heap (bloco 0) e do ponteiro para ele na `.data` |

- `GUObjectArray`: `+0x0` ponteiro para a lista de blocos, `+0x8` pré-alocado, `+0x10` `MaxElements` (`0x250000`),
  `+0x14` `NumElements` (~506 mil), `+0x18` `MaxChunks` (`0x25`), `+0x1c` `NumChunks` (10). Blocos de 65.536 itens
  de **24 bytes**; o ponteiro do objeto fica em **`+0x8` do item**.
- `UObject`: `+0x0` vtable, `+0x8` `ObjectFlags` (`0x10` = CDO), `+0xC` `InternalIndex`, `+0x10` `ClassPrivate`,
  `+0x18` `NamePrivate` (`ComparisonIndex`, `Number`).
- `FNamePool`: `+0x0` (antes dos blocos) `CurrentBlock = 0x72`, `CurrentByteCursor = 0xc4b6`; cada bloco é uma
  sequência de entradas alinhadas a 2 bytes; cabeçalho `u16`: tamanho `>> 6`, bit 0 = UTF-16, bits 1–5 = hash.
  Índice → bloco `idx >> 16`, deslocamento `(idx & 0xffff) * 2`.
- Percorrendo a tabela com o dono no save: `BP_SurvivalPlayerController_Augusta_C` e
  `BP_SurvivalPlayerCharacter_Gellarde_C` (instâncias únicas), além de um `PlayerCharacterLODActor`.
- Consequência: o pawn do jogador pode ser obtido **sem hook**, procurando a instância do controller na
  `GUObjectArray` e lendo `+0x318`. Para o god mode, que precisa comparar dentro do código do jogo, o trainer pode
  escrever esse endereço numa variável da cave (Fase 6).

## Sessão de 2026-10-26 (Fase 3): catálogo de todos os cheats do FLiNG

Mesmo build (`c935e799-0b27c000`), jogo aberto, FLiNG fechado, só leitura. Offsets lidos da **reflexão do jogo em
execução** (`scratch/fling/tools/ue_props.py`), não de SDK gerado.

### Layout da reflexão (UE5, este build)

| Estrutura | Campo | Offset |
|---|---|---|
| `UStruct` | `SuperStruct` / `Children` (funções) / `ChildProperties` / `PropertiesSize` | `+0x40` / `+0x48` / `+0x50` / `+0x58` |
| `FField` | `ClassPrivate` (`FFieldClass*`, nome em `+0`) / `Next` / `NamePrivate` | `+0x08` / `+0x18` / `+0x20` |
| `FProperty` | `ArrayDim` / `ElementSize` / `PropertyFlags` / `Offset_Internal` | `+0x30` / `+0x34` / `+0x38` / `+0x44` |
| `UFunction` | ponteiro nativo (`Func`) | `+0xd8` |

### Offsets dos campos usados (lidos da reflexão)

| Classe | Campos |
|---|---|
| `Actor` | `CustomTimeDilation +0x70`, `RootComponent +0x1c0` |
| `Controller` | `Pawn +0x318`, `Character +0x328`; `PlayerController.AcknowledgedPawn +0x380` |
| `SurvivalPlayerController` | `ItemUseCooldownRemaining +0xb90`, `CachedPartyComponent +0xfa8` |
| `Character` | `CharacterMovement +0x348`, `JumpMaxCount +0x488`, `JumpCurrentCount +0x48c` |
| `CharacterMovementComponent` | `GravityScale +0x1b8`, `JumpZVelocity +0x1c0`, `MovementMode +0x251`, `MaxWalkSpeed +0x298`, `bCheatFlying +0x585` |
| `SceneComponent` | `RelativeLocation +0x150`; posição no mundo em `+0x210` (3 `double`, lida pelo jogo em `+35fb645`) |
| `SurvivalCharacter` | componentes `Health +0x13b8`, `Block +0x13c0`, `Reaction +0x13c8`, `Stamina +0x13d0`, `BodyTemperature +0x1420`, `Hauling +0x14d8`, `Survival +0x14e0`; `CustomDamageMultiplier +0x1330`; `bTakeFallDamage +0x1619` |
| `HealthComponent` | `MaxHealth +0x330`, `CurrentDamage +0x334` (vida = máx − dano); `bCanDieFromDamage`/`bCanDieEver` (bits em `+0x11d`) |
| `StaminaComponent` | `CurrentStamina +0x100`, `MaxStamina +0x104` |
| `BlockComponent` | `PerfectBlockWindow +0x14c`, `PerfectBlockWindowMounted +0x150` |
| `ReactionComponent` | `CurrentStunValue +0xf8`, `MaxStunValue +0xfc` |
| `BodyTemperatureComponent` | `CurrentBodyTemperature +0x134` |
| `HaulingComponent` | `Capacity +0xf4` |
| `SurvivalComponent` | `CurrentFood +0x290`, `bAllowCurrentFoodDecrease +0x294`, `CurrentWater +0x298`, `bAllowCurrentWaterDecrease +0x29c`, `CurrentBreath +0x2a0` |
| `Item` | `StackSize +0x198`, `PowerUsed +0x250`, `DurabilityRemaining +0x298` |
| `PartyComponent` | `ScienceFound +0x510`, `ResearchPoints +0x514`, `PartyUpgradePoints +0x51c`, `PersonalUpgradePoints +0x520` |
| `SurvivalGameModeSettings` | `bAutoCompleteBuildings +0x3b`, `bRecipesRequireIngredients +0x3d`, `bCreaturesIgnorePlayers +0x3f`, `FallDamageMultiplier +0x94` |
| `ResourceAnalyzerBuilding` | `MaxPowerCapacity +0x980` |

- A instância viva das configurações de modo é `BP_NormalModeSettings_C` (`outer = Augusta_Main`); os CDOs dos
  modos Criativo mostram `bAutoCompleteBuildings = 1` e `bRecipesRequireIngredients = 0`. **Risco:** essas
  configurações podem ir para o save; ao desligar, restaurar o valor original e testar com backup.
- `SurvivalCharacter.CustomDamageMultiplier` vale `1.0` no jogador; `GetCustomDamageMultiplier` (`+5646bb0`) só o
  devolve. Não verificado se afeta o dano causado ou o recebido. `GetPlayerDamageMultiplier` (usada pelo FLiNG) não
  existe mais como `UFunction` neste build.

### Pontos novos (padrão do FLiNG quebrado, código equivalente achado)

| Script FLiNG | Ponto atual | Instrução | Por que o padrão quebrou |
|---|---|---|---|
| `aobplayerpawn` | `+3e078fe` (função `+3e07870`) | `mov eax,[r15+0x2c8]; shr eax,4; test al,1` | instrução nova (`mov [rsp+0x98],r12`) no meio |
| `aobitemcd` | `+5c3bbff` | `subss xmm0,xmm6; comiss xmm0,xmm1; movss [rbx+0xb90],xmm0; ja` | registrador `xmm8` → `xmm6` |
| `aobresearchlevelup` | `+5970f18` | `cmp eax,[rbx+0x514]; jle` (laço com `add r14,0x30`) | registrador base `r14`/`rsi` → `rbx` |
| `aobgravity` | `+57bcb2b` e `+57c0431` | `mulss xmm0,[rbx+0x1b8]` (`GravityScale`) | contexto mudou; 2 ocorrências |
| `aobitemdurability` | `+5a4e777` (bloco desde `+5a4e76c`) | `movss xmm1,[rbx+0x298]; movaps xmm2,xmm1; subss xmm2,xmm8; minss xmm2,xmm0; maxss xmm2,xmm6; ucomiss; je; …; movss [rbx+0x298],xmm2` | `movaps` novo e destino `xmm1` → `xmm2` |
| `aobitempower` | `+5a5744b` (bloco desde `+5a57440`) | `movss xmm0,[rcx+0x250]; movaps xmm1,xmm0; addss xmm1,xmm3; minss xmm1,xmm2; ucomiss; je; …; movss [rcx+0x250],xmm1` | `movaps` novo; o limite vem de `[dados+0x748]` |

- Os dois últimos viram **patch simples** (Fase 4): anular o `subss` (5 bytes) ou o `addss` (4 bytes) mantém o
  valor, e o `je` do próprio jogo pula a gravação. Efeito colateral provável: reparo não aumenta a durabilidade. O
  `call +1fed760` com `r8d = 0x11`/`0x0e` logo após a gravação marca a propriedade para replicação (índice do campo).
- A escrita em `+0x298` de `+39073a0` (com `minss`/`maxss`) é de outra estrutura: grava `+0x298` e `+0x29c` juntos.
- `aobdmg`/`aobdmgmul` não foram procurados: o dano causado sai por valor (ver abaixo). `aobplayerstats` é
  desnecessário.

### Dano causado e "não morrer" (estático, a confirmar no jogo)

- `+5943892`, na aplicação de dano do `HealthComponent` (mesma função do ponto de defesa `+5943ec8`):
  `rbx` = personagem que **causou** o dano (`call +14f2520`, conferido contra a classe); `movss xmm0,[rbx+0x1330];
  mulss xmm0,[r15]; movss [r15],xmm0`. **`CustomDamageMultiplier` do atacante multiplica o dano causado**; é o único
  uso do campo no executável.
- Logo depois: `test byte [rsi+0x11d], 2` com `rsi` = `HealthComponent` do alvo. Máscaras das `FBoolProperty`
  (`+0x72 ByteMask`, `+0x73 FieldMask`): em `+0x11d`, `PlayDamagedParticlesOnDeath = 0x01`,
  `bCanDieFromDamage = 0x02`, `bCanDieEver = 0x04`; no jogador vale `0x06`. Limpar esses bits é candidato a god mode
  por valor ("não morre", talvez sem ignorar o golpe). Outra opção: `ImmunityFlags` (`+0x110`).
- Leitura completa do personagem pelos offsets (dono no save, 2026-10-26): vida 100/100, stamina 120/100, comida
  54,1, água 4,8, fôlego 100, carga 5, `CustomDamageMultiplier` 1,0, pulos 0/1, gravidade 1,0, `MaxWalkSpeed` 350,
  posição em `double`. O `CachedPartyComponent` (`+0xfa8`) do controller estava **nulo**: a ciência precisa de outro
  caminho até o `PartyComponent`.

### Partes nativas do trainer (REA/Ghidra)

- **Teleporte** (`FUN_140088cb0`, `FUN_1400788a0`): descritor `{tem_z, endereço, tipo (0 = float, 1 = double)}`. O
  endereço é o `player_location` capturado em `+35fb632`. Salvar e desfazer leem 3 `double` com
  `ReadProcessMemory`; teleportar escreve `x + dx, y + dy, z + dz` com `WriteProcessMemory` direto na posição do
  `RootComponent` (`+0x210`). Para o Grounded 2, `dz = 100.0` (folga de altura). Locais salvos em `locations.txt`.
- **Velocidade do jogo:** `TrSpeedHack_x64.dll` (embutida no exe em `0x199100`) intercepta `QueryPerformanceCounter`,
  `GetTickCount`, `GetTickCount64` e `timeGetTime`; sem exports nomeados.
- `instant_item_cooldown`, `infinite_equipment_durability` e `infinite_torch_duration` são registrados em
  `FUN_140089080` na mesma ordem dos três scripts sem variável de controle (`aobitemcd`, `aobitemdurability`,
  `aobitempower`): **`aobitempower` (zerar `Item.PowerUsed`) é a tocha infinita**.

### Catálogo: as 36 opções do menu

Caminho proposto para o nosso trainer: **V** = escrita de valor no objeto (Fase 5, sem patch de código);
**P** = patch simples (Fase 4); **C** = desvio com cave (Fase 6); **D** = DLL (Fase 7).

| Tecla | Opção | Como o FLiNG faz | Estado no build | Nosso caminho |
|---|---|---|---|---|
| Num 1 | God Mode | `+56fc11c`: golpe no jogador é tratado como alvo sem componente | ponto ok, falta `player` | C (com o pawn escrito na cave); testar antes V com os bits `bCanDie…` (`+0x11d`) |
| Num 2 | Vida infinita | `CurrentDamage = -9999` no hook `aobplayerstats` | padrão quebrado | V: `CurrentDamage = 0` |
| Num 3 | Stamina infinita | `CurrentStamina = 999999` | quebrado | V |
| Num 5/6/7 | Comida / água / oxigênio | `CurrentFood/Water/Breath = 999999` | quebrado | V; comida e água também por `bAllow…Decrease = 0` |
| Num 8 | Temperatura estável | `CurrentBodyTemperature = 0` | quebrado | V |
| Num 9 | Furtividade | `bCreaturesIgnorePlayers = 1` (hook `+593af1c`) | ok | V nas configurações de modo (risco de save) |
| Num 0 | Dano máximo | multiplicador do dano em `xmm7` = 9999 | quebrado | V `CustomDamageMultiplier` (multiplica o dano causado, `+5943892`) |
| Num . | Multiplicador de dano | idem, × valor | quebrado | V `CustomDamageMultiplier` |
| Num + | Multiplicador de defesa | divide o dano recebido em `+5943ec8` | ponto ok, falta `player` | C |
| Ctrl+Num 1 | Editar ciência | `ScienceFound` no hook `aobpartycomponent` | quebrado | V (caminho até o `PartyComponent` a achar; `CachedPartyComponent` estava nulo) |
| Ctrl+Num 4 | Quantidade ao arrastar | `Item.StackSize` no hook `+5bbfe8c` | ok | C, ou V se acharmos o item arrastado |
| Ctrl+Num 6 | Recarga instantânea | zera a subtração em `aobitemcd` | ponto novo `+5c3bbff` | V `ItemUseCooldownRemaining = 0` ou P |
| Ctrl+Num 7 | Durabilidade infinita | força o novo valor ao máximo | ponto novo `+5a4e777` | P (anular o `subss`) |
| Ctrl+Num 0 | Sem requisitos de receita | `bRecipesRequireIngredients = 0` | ok | V (risco de save) |
| Ctrl+Num . | Sem requisitos de construção | troca `UnderConstruction` por `Built` em `+57689db` | ok | C, ou V com `bAutoCompleteBuildings` |
| Alt+Num 1 | Brain Power rápido | iguala `ResearchPoints` ao limite do nível | ponto novo `+5970f18` | C |
| Alt+Num 2/3 | Editar vida / stamina máximas | `MaxHealth`, `MaxStamina` | quebrado | V |
| Alt+Num 4 | Velocidade de movimento | multiplica o retorno de `call [rax+0x500]` em `+37eee9b` | ponto ok, falta `player` | V `MaxWalkSpeed` ou C |
| Alt+Num 5/6 | Velocidade do jogador / da IA | `[ator+0x68]` em `+3785df2` | **offset mudou para `0x70`** (o FLiNG recusa) | V `CustomTimeDilation` |
| Alt+Num 7 | Velocidade do jogo | DLL speedhack | — | D (ou outra técnica no Linux) |
| Alt+Num 8 | Pulos infinitos | zera `JumpCurrentCount`, `JumpMaxCount ≥ 2` | ponto ok, falta `player` | V (os dois campos) |
| Alt+Num 9 | Sem dano de queda | `FallDamageMultiplier = 0` | ok | V `bTakeFallDamage = 0` no personagem (sem tocar no modo) |
| Alt+Num 0 | Gravidade | multiplica em `aobgravity` | pontos novos | V `GravityScale` |
| Alt+Num . / + | Congelar hora / velocidade do tempo | multiplica ou zera o incremento em `+58c8a2a` | ok | C |
| Alt+Num / * - | Salvar local / teleporte / desfazer | captura a posição em `+35fb632` e escreve 3 `double` | ponto ok, falta `player` | V (posição do `RootComponent` do pawn) |
| — | Capacidade de carga (`hauling_capacity`) | `HaulingComponent.Capacity` | quebrado | V |
| — | Bloqueio perfeito, sem atordoamento | soma 10000 à janela; `CurrentStunValue = -9999` | quebrado | V |
| — | Cargas infinitas do analisador | copia `MaxPowerCapacity` para as cargas em `+57bd6d0` | ok | C |
| — | Tocha infinita | zera `Item.PowerUsed` (`aobitempower`) | ponto novo `+5a5744b` | P (anular o `addss`) |

**Conclusão:** cerca de 20 das opções viram **escrita de valor** a partir do pawn do jogador, sem patch de código e
sem depender de padrões que quebram a cada atualização. Os offsets vêm da reflexão, então sobrevivem a mudanças de
layout. Patches e caves ficam para o que muda lógica: god mode, defesa, Brain Power, hora do dia, construção e
analisador.

## Sessão de 2026-10-26 (Fase 3): critério de pronto do `gt-tui` no jogo

Build `c935e799-0b27c000` (`Augusta-CL-3207493`). Save de teste, FLiNG fechado, backup do save feito pelo dono.
Binário `target/debug/gt-tui --diagnostico`; conferência por `gt diff` e `gt-patch list` num segundo terminal.

- **Conexão:** com a TUI aberta antes do jogo, ela passou de `aguardando o jogo` a `conectado` sozinha, versão `com
  assinaturas`, `teste-codificacao` em `[OFF]`.
- **Busca dos padrões (`catalog::scan` sobre a `.text` de ~128 MB):** 8,9 s a 11,4 s por busca (log `busca dos
  padrões duracao_ms=…`, seis medições). Bem acima dos ~2 s imaginados, mas a busca roda numa thread separada e a tela
  continuou respondendo. Consequência: a cadência das buscas automáticas é `max(5 s, 2 × duração)` ≈ 20 s.
- **Ligar/desligar:** pausa de 11–15 ms por troca. `gt diff` mostrou `+112c469` `0b c2` → `09 d0` ligado (o `48` do
  início da instrução não muda) e "nenhuma alteração" desligado; `gt-patch list` mostrou `ativo` com a TUI segurando.
- **Saídas:** `q`, fechar a janela do terminal (SIGHUP) e `kill` (SIGTERM) restauraram na hora (`restaurado:
  teste-codificacao` no log; `gt diff` limpo). `kill -9` deixou o patch e o `journal.json`; o `gt-patch list` mostrou
  `pendente`; o `gt-tui` aberto de novo recuperou sozinho e apagou o diário.
- **Reconexão:** com o patch ligado, o jogo fechado deu `o jogo fechou` (motor descartou sem escrever). Jogo aberto de
  novo (PID novo): a TUI reconectou na primeira busca (~34 s depois de fechar, incluindo o carregamento do jogo) e
  religou o patch **já na partida** (relato do dono), sem precisar das buscas automáticas. A pausa com o jogo ainda
  carregando não chegou a ser exercitada.
- **Testes `#[ignore]` com o jogo aberto:** 11 passaram, inclusive os de `apps/gt-patch/tests/game.rs` que estavam
  pendentes da Fase 2 e o `game_backend_reopen_50_times_no_fd_leak` (527 s: 50 buscas de ~10 s).
- **Processo solto achado na sessão:** um `gt-tui` aberto às 00:06 num teste manual da Tarefa 15, antes da correção do
  terminal fechado, girava a 99% de CPU com o pty apagado (laço de `EIO` do crossterm, que a thread `gt-keys` corrige).
  Sem diário nem motor; encerrado à mão.
- **Opinião do dono sobre a TUI:** "achei bom".

## Hipóteses descartadas

- (2026-10-25) "Os patches em `+57BD6D0`/`+5BBFE8C` indicam que as opções de cargas e arrastar item estavam
  ligadas no FLiNG": os desvios aparecem com todas as opções desligadas.
- (2026-10-26) "A função `+3e07870` não roda no solo porque só existem CDOs das classes dela": erro de método. A busca
  pelo início da vtable recuou 5 entradas além do começo real (`+9051a58` em vez de `+9051a80`), e as instâncias
  foram procuradas pela vtable errada. A função é do controller do jogador.
