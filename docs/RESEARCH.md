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
| `.data` | `0x14A184000` | `0x00298400` |
| `.pdata` | `0x14A840000` | `0x0069E790` |
| `.rsrc` | `0x14AF3E000` | `0x00011B40` |
| `.reloc` | `0x14AF50000` | `0x0032B428` |

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
| `aobgamemodesettings` | `+593AF1C` | Encontrado |
| `aobbuildingreg` | `+57689DB` | Encontrado |
| `aobspeed` | `+37EEE9B` | Encontrado |
| `aobplayerspeed` | `+3785DF2` | Encontrado |
| `aobinfinitejumps` | `+378553E` | Encontrado |
| `aobdaytime` | `+58C8A2A` | Encontrado |
| `aoblocation` | `+35FB632` | Encontrado |

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

## Hipóteses descartadas

- (nenhuma ainda)
