# Spec 004 — Primeiros cheats (patch de código simples)

| Campo | Valor |
|---|---|
| Fase | 4 (ver `docs/ROADMAP.md`, reorganizada em 2026-10-26) |
| Estado | Escrita sob delegação do dono ("o que for melhor"), revisada por subagente; aprovação final com o teste no jogo |
| Data | 2026-10-26 |

## 1. Intenção

Ter os primeiros cheats reais funcionando no jogo com o que já existe: o motor de patches da Fase 2 e a TUI da
Fase 3. Cada cheat desta fase é **uma troca de bytes do mesmo tamanho** num ponto do código do jogo, descrita só
nas assinaturas. Não há código novo de motor, de leitura de estruturas do Unreal nem de code cave.

Os três pontos saíram da análise do FLiNG (`RESEARCH.md`, sessões de 2026-10-25 e 2026-10-26). Os padrões do
FLiNG para esses três cheats **não funcionam** no build atual. O código equivalente do jogo foi achado por nós, e
as trocas são nossas: instruções neutras escolhidas por nós, sem copiar bytes do FLiNG (`AGENTS.md` 3).

## 2. Objetivos

1. Três cheats na categoria **Itens** da TUI, ligando e desligando pelo motor existente:
   - **Durabilidade infinita** (`durabilidade-infinita`);
   - **Energia infinita** (`energia-infinita`): tocha e outros itens que gastam energia;
   - **Recarga instantânea** (`recarga-instantanea`): a espera para usar item de novo acaba no primeiro quadro em
     que o jogo a confere.
2. Definições em `generic.toml` (valem para qualquer versão, com conferência de padrão único) e entrada verificada
   em `c935e799-0b27c000.toml` (com `rva`).
3. Testes que carregam as assinaturas reais e conferem os três cheats sem o jogo; teste `#[ignore]` com o jogo
   aberto que liga e desliga cada um.
4. Teste manual do dono no jogo, cheat a cheat.

## 3. Fora do objetivo

- Cheats com valor ajustável. Nenhum dos três tem `value`.
- Qualquer cheat que precise do pawn do jogador, de estruturas do Unreal (Fase 5) ou de code cave (Fase 6).
- Restaurar valores que já caíram antes de ligar o cheat: ligar a durabilidade infinita não conserta o item, só
  impede que ele continue gastando.
- Escolher a quais itens o cheat se aplica: vale para todos os itens que passam pelo ponto (ver os efeitos em 6.1–6.3).
- Mudanças na TUI, no motor ou no formato das assinaturas.

## 4. Anti-objetivos (falha mesmo que "funcione")

- Qualquer alteração em arquivo de save ou em arquivo do jogo em disco.
- Um cheat que, ligado, faça o jogo travar, fechar ou se comportar de forma estranha fora do efeito descrito.
- Patch aplicado num lugar que não seja exatamente o descrito: padrão com 0 ou 2+ ocorrências é "indisponível",
  nunca "pega o primeiro" (regra já garantida pelo motor).
- Bytes ou código do FLiNG no repositório.

## 5. Onde cada coisa mora

| O quê | Onde |
|---|---|
| Definições genéricas | `crates/gt-grounded2/signatures/generic.toml` |
| Entrada verificada do build atual | `crates/gt-grounded2/signatures/c935e799-0b27c000.toml` |
| Testes sem o jogo | `crates/gt-grounded2/src/signatures.rs` (testes que já carregam os arquivos embutidos) |
| Teste com o jogo aberto | junto dos testes `#[ignore]` existentes de `gt-patch` ou `gt-tui` (escolha de quem implementa) |
| Achados do teste no jogo | `docs/RESEARCH.md` |

## 6. Comportamento

Os três pontos ficam no build `c935e799-0b27c000` (`Augusta-CL-3207493`). Os padrões foram conferidos como
**únicos no executável em disco** (2026-10-26); na memória, só o da durabilidade (o jogo fechou durante a conferência
dos outros dois). Antes de qualquer escrita, o plano tem um passo só de leitura (`gt scan` dos três padrões e
`gt-patch list`) com o jogo aberto. Se algum der "ambíguo" ou "não encontrado" na memória, o padrão é alongado e a
mudança volta para esta spec antes de seguir.

Em todos os casos a troca mantém as fronteiras de instrução (uma instrução por outra do mesmo tamanho), o que a
regra de perigo do motor exige.

### 6.1 Durabilidade infinita

Código do jogo (`Item.DurabilityRemaining` = `+0x298`, offset lido da reflexão):

```
+5a4e76c  f3 0f 10 8b 98 02 00 00   movss  xmm1, [rbx+0x298]   ; durabilidade atual
+5a4e774  0f 28 d1                  movaps xmm2, xmm1
+5a4e777  f3 41 0f 5c d0            subss  xmm2, xmm8          ; − desgaste
+5a4e77c  f3 0f 5d d0               minss  xmm2, xmm0          ; limita ao máximo
+5a4e780  f3 0f 5f d6               maxss  xmm2, xmm6          ; limita a 0
+5a4e784  0f 2e d1                  ucomiss xmm2, xmm1
+5a4e787  74 27                     je     (não grava se não mudou)
          …                         movss  [rbx+0x298], xmm2
```

| Campo | Valor |
|---|---|
| `id` / `name` | `durabilidade-infinita` / `Durabilidade infinita` |
| `category` | `itens` |
| `pattern` | `f3 0f 10 8b 98 02 00 00 0f 28 d1 f3 41 0f 5c d0 f3 0f 5d d0 f3 0f 5f d6 0f 2e d1 74` |
| `offset` | `11` |
| `expected` | `f3 41 0f 5c d0` (`subss xmm2, xmm8`) |
| `replacement` | `0f 1f 44 00 00` (`nop` de 5 bytes) |
| `rva` (build atual) | `0x5a4e777` |

**Efeito:** o novo valor fica igual ao atual, e o `je` do próprio jogo pula a gravação. A durabilidade de
ferramentas, armas e armaduras para de cair.

**Efeitos colaterais conhecidos ou prováveis:**
- Se o reparo de item passar por esta mesma função com desgaste negativo, ele também não aumenta a durabilidade
  enquanto o cheat estiver ligado. A conferir no teste manual.
- Vale para todos os itens que passam por este ponto, não só os do jogador. Num jogo solo, os itens são os do
  jogador.

### 6.2 Energia infinita

Código do jogo (`Item.PowerUsed` = `+0x250`):

```
+5a57440  f3 0f 10 81 50 02 00 00   movss  xmm0, [rcx+0x250]   ; energia já gasta
+5a57448  0f 28 c8                  movaps xmm1, xmm0
+5a5744b  f3 0f 58 cb               addss  xmm1, xmm3          ; + consumo
+5a5744f  f3 0f 5d ca               minss  xmm1, xmm2          ; limita ao total do item
+5a57453  0f 2e c8                  ucomiss xmm1, xmm0
+5a57456  74 24                     je     (não grava se não mudou)
          …                         movss  [rcx+0x250], xmm1
```

| Campo | Valor |
|---|---|
| `id` / `name` | `energia-infinita` / `Energia infinita` |
| `category` | `itens` |
| `pattern` | `f3 0f 10 81 50 02 00 00 0f 28 c8 f3 0f 58 cb f3 0f 5d ca 0f 2e c8 74` |
| `offset` | `11` |
| `expected` | `f3 0f 58 cb` (`addss xmm1, xmm3`) |
| `replacement` | `0f 1f 40 00` (`nop` de 4 bytes) |
| `rva` (build atual) | `0x5a5744b` |

**Efeito:** a energia gasta não aumenta. A tocha (e outros itens com energia) não se apaga por falta de energia.

**Efeitos colaterais prováveis:** vale para qualquer item que use energia por este caminho. Não muda a energia de
construções, que é outro campo (`Building.PowerValue`).

### 6.3 Recarga instantânea

Código do jogo, no `Tick` do `SurvivalPlayerController` (`ItemUseCooldownRemaining` = `+0xb90`; `xmm6` é o tempo do
quadro, o mesmo passado aos outros `Tick` logo acima):

```
+5c3bbec  f3 0f 10 bb 90 0b 00 00   movss  xmm7, [rbx+0xb90]   ; recarga restante
+5c3bbf4  0f 57 c9                  xorps  xmm1, xmm1          ; 0
+5c3bbf7  0f 2f f9                  comiss xmm7, xmm1
+5c3bbfa  76 36                     jbe    (sem recarga: sai)
+5c3bbfc  0f 28 c7                  movaps xmm0, xmm7
+5c3bbff  f3 0f 5c c6               subss  xmm0, xmm6          ; − tempo do quadro
+5c3bc03  0f 2f c1                  comiss xmm0, xmm1
+5c3bc06  f3 0f 11 83 90 0b 00 00   movss  [rbx+0xb90], xmm0
+5c3bc0e  77 22                     ja     (ainda em recarga: sai)
          …                         fim da recarga (caminho normal do jogo)
```

| Campo | Valor |
|---|---|
| `id` / `name` | `recarga-instantanea` / `Recarga instantânea` |
| `category` | `itens` |
| `pattern` | `f3 0f 10 bb 90 0b 00 00 0f 57 c9 0f 2f f9 76 ?? 0f 28 c7 f3 0f 5c c6 0f 2f c1 f3 0f 11 83 90 0b 00 00` |
| `offset` | `16` |
| `expected` | `0f 28 c7` (`movaps xmm0, xmm7`) |
| `replacement` | `0f 57 c0` (`xorps xmm0, xmm0`) |
| `rva` (build atual) | `0x5c3bbfc` |

**Efeito:** sempre que houver recarga, o valor gravado é `0 − tempo do quadro` (negativo). O `ja` não é tomado e o
jogo segue, **no mesmo quadro**, pelo próprio caminho de fim de recarga; nos quadros seguintes o valor negativo sai
pelo `jbe`. O curinga no `pattern` é o deslocamento do `jbe`.

**A conferir no teste manual:** quais usos de item passam por esta recarga (comida, curativos, itens de arremesso).
O nome do campo (`ItemUseCooldownRemaining`) e os eventos `OnItemUseCooldownBegin/End` do controller indicam que é
a espera entre usos de item.

### 6.4 Comportamento na TUI

Nada muda no comportamento da TUI: os três aparecem em **ITENS**, na ordem do `generic.toml` (durabilidade,
energia, recarga), começam `OFF` e seguem todas as regras da spec 003 (religar depois de reiniciar o jogo, desfazer
ao sair, estados `?`/`indisponível`). As descrições em pt-BR dizem o efeito em uma linha, como no patch de teste.

## 7. Testes

| Teste | Exige jogo? |
|---|---|
| As assinaturas embutidas carregam com os três cheats: `name`, `category = itens`, sem `value`, e a entrada da versão `c935e799-0b27c000` tem o `rva` de cada um | Não |
| Para cada um, `expected` e `replacement` decodificam (iced-x86, 64 bits) em instruções completas com as mesmas fronteiras | Não |
| Com o jogo aberto, os três aparecem como "disponível (verificado)" no endereço `base + rva`, e cada um liga e desliga uma vez, com `gt diff` sem alteração no final | Sim (`#[ignore]`) |

Os testes existentes do patch de teste continuam passando.

## 8. Critério de pronto

Com backup do save feito pelo dono (`AGENTS.md` 3) e o FLiNG fechado:

1. `gt-patch list` (ou a TUI) mostra os três como disponíveis e verificados.
2. **Durabilidade:** com o cheat ligado, usar uma ferramenta ou arma até a durabilidade cair sem ele: com ele, ela
   não cai. Desligado, volta a cair. Anotar se o reparo funciona com o cheat ligado.
3. **Energia:** com a tocha acesa e o cheat ligado, a energia não diminui. Desligado, volta a diminuir.
4. **Recarga:** usar um item com recarga e tentar usar de novo logo em seguida: com o cheat, dá para usar de novo
   sem esperar. Anotar quais itens foram testados.
5. Reiniciar o jogo com a TUI aberta e os três ligados: depois de reconectar, eles voltam ligados sozinhos e
   continuam funcionando.
6. Sair da TUI: `gt diff` mostra a `.text` sem alteração.
7. O dono confirma que o jogo ficou estável durante os testes.

## 9. Documentos a atualizar

- `docs/RESEARCH.md`: resultado do teste no jogo de cada cheat, inclusive os efeitos colaterais observados.
- `docs/ROADMAP.md`: status da Fase 4 ao terminar.

## 10. Decisões tomadas

| Decisão | Quem |
|---|---|
| Reorganizar as Fases 4–6; Fase 4 só com trocas de bytes do mesmo tamanho | Dono (2026-10-26) |
| Os três cheats desta fase: durabilidade, energia e recarga | Proposta da IA, aprovada pelo dono junto com a reorganização |
| Anular o desgaste e o consumo com `nop` do mesmo tamanho, em vez de reescrever a conta | IA: menor troca possível; o `je` do jogo já pula a gravação |
| Recarga por `xorps xmm0, xmm0` (gravar valor negativo) em vez de pular o `ja` | IA: o jogo termina a recarga pelo próprio caminho |
| `id`, nomes e descrições em pt-BR desta spec | IA |

## 11. Deixado para quem implementa

- O texto exato das descrições (uma linha, pt-BR, dizendo o efeito).
- Em qual binário fica o teste `#[ignore]` (seguir o padrão de `apps/gt-patch/tests/game.rs` ou de
  `apps/gt-tui/tests/game.rs`).
- Como conferir as fronteiras de instrução no teste. Sugestão: `gt_core::patch::validate`, conferindo
  `lost_on_apply()` e `lost_on_undo()` vazios (o `gt-grounded2` não depende de `iced-x86` diretamente).
