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

## Hipóteses descartadas

- (2026-10-25) "Os patches em `+57BD6D0`/`+5BBFE8C` indicam que as opções de cargas e arrastar item estavam
  ligadas no FLiNG": os desvios aparecem com todas as opções desligadas.
