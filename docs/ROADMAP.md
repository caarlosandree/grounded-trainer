# Roadmap — grounded-trainer

Trainer **nativo de Linux** para o Grounded 2 (Steam/Proton), para uso pessoal em partidas solo/offline.

Escopo de funcionalidades definido: **Sobrevivência, Itens, Progressão e Movimento**.
Interface: **TUI no terminal**. Estratégia técnica: **externo primeiro, DLL injetada como fase opcional posterior**.

Cada fase só começa quando a anterior cumpre seu critério de pronto. Cada funcionalidade passa por
`spec → plano → código` (ver `AGENTS.md`).

---

## Fase 0 — Fundação

**Objetivo:** projeto pronto para receber código, com as regras e decisões documentadas.

- Documentos: `ROADMAP.md`, `AGENTS.md`/`CLAUDE.md`, `ARCHITECTURE.md`, `RESEARCH.md`.
- Toolchain Rust instalado via `rustup` (stable).
- Workspace Cargo vazio compilando, com `fmt`, `clippy` e `test` passando.

**Pronto quando:** `cargo build`, `cargo clippy -- -D warnings` e `cargo test` passam no workspace vazio.
**Você faz:** revisar e aprovar os documentos.

---

## Fase 1 — Núcleo de memória e ferramenta de pesquisa

**Objetivo:** enxergar o jogo de fora: achar o processo, ler memória, buscar padrões e desmontar código.

- Localizar o processo do jogo e o módulo `Grounded2Steam-Win64-Shipping.exe`.
- Ler os cabeçalhos PE direto da memória (seções `.text`, `.rdata`, `.data`).
- Leitura/escrita de memória.
- Busca AOB com curingas (`??`).
- Desmontagem x86-64 de qualquer endereço.
- CLI de pesquisa `gt`:
  - `gt inspect`: processo, base, seções, chave de versão do jogo.
  - `gt scan "<padrão>"`: onde o padrão aparece.
  - `gt disasm <endereço|módulo+offset>`: código desmontado.
  - `gt diff`: compara o código na memória com o executável em disco (revela patches de outros trainers).
  - `gt read`, `gt dump` e `gt fling`: hexdump, salvar memória em arquivo e estado dos pontos do FLiNG (spec 001).

**Pronto quando:** `gt inspect` e `gt disasm Grounded2Steam-Win64-Shipping.exe+56FC11C` funcionam com o jogo aberto, e `gt diff` reproduz os achados de `RESEARCH.md`.
**Você faz:** deixar o jogo aberto num save de teste quando eu pedir.

---

## Fase 2 — Motor de patches seguro

**Objetivo:** alterar o jogo de forma **reversível e à prova de crash**.

- `Patch`: substituir bytes, guardando os originais.
- Verificação dos bytes esperados antes de escrever: se não baterem, o patch é recusado (proteção contra versão errada).
- Pausar o jogo por alguns milissegundos durante a escrita (evita uma thread do jogo executar uma instrução pela metade).
- Restauração garantida: ao sair, com Ctrl+C, em pânico do trainer ou se o jogo fechar.
- Detecção de versão do jogo e arquivo de assinaturas por versão (`signatures/*.toml`).

**Pronto quando:** um patch de teste liga e desliga 100 vezes seguidas sem crash, e matar o trainer restaura os bytes.
**Você faz:** confirmar que o jogo continua estável.

---

## Fase 3 — Interface TUI

**Objetivo:** controlar os cheats confortavelmente com o jogo aberto.

- Lista de cheats por categoria com estado `ON/OFF/indisponível`.
- Valores ajustáveis (multiplicadores, velocidade).
- Status: jogo conectado, versão detectada, padrões encontrados/ausentes.
- Reconexão automática se o jogo reiniciar.

**Pronto quando:** dá para ligar e desligar os cheats existentes só pela TUI.
**Você faz:** opinar sobre usabilidade e layout.

---

## Fase 4 — Primeiros cheats (patch de código simples)

**Objetivo:** primeiras funcionalidades reais, usando os pontos já descobertos nos achados de engenharia reversa.

| Categoria | Cheat | Ponto de partida |
|---|---|---|
| Itens | Usos/cargas infinitas | `aobcharges` (FLiNG, funciona na versão atual) |
| Itens | Quantidade ao arrastar (duplicar) | `aobdragitemamount` (FLiNG, funciona) |
| Movimento | Velocidade do jogador | `aobplayerspeed` |
| Movimento | Pulo infinito | `aobinfinitejumps` |
| Sobrevivência | God mode | `aobgodmode`; o da FLiNG não funciona, então precisa de investigação nova |

**Pronto quando:** cada cheat da tabela funciona no jogo e sobrevive a reiniciar o jogo.
**Você faz:** testar cada cheat no jogo e dizer se funcionou.

---

## Fase 5 — Estruturas do Unreal Engine (valores)

**Objetivo:** ler e escrever valores do jogo pelo *nome* da estrutura, não por endereço solto. É a base para os cheats mais robustos.

- Gerar o SDK do jogo com Dumper-7 (uma vez por atualização), para obter os offsets das classes.
- Resolver `GNames`, `GObjects` e `GWorld` por assinatura.
- Navegar: `GWorld → GameInstance → LocalPlayer → PlayerController → Pawn → componentes`.
- Cheats:
  - Sobrevivência: vida, fome, sede e stamina travadas no máximo.
  - Movimento: voar (`MovementMode = Flying`), teleporte (salvar/carregar posição).
  - Itens: editar quantidade de pilhas no inventário.

**Pronto quando:** os cheats de sobrevivência funcionam e continuam funcionando após uma atualização do jogo, só regenerando os offsets.
**Você faz:** rodar o Dumper-7 quando eu pedir (passo guiado).

---

## Fase 6 — Code caves (lógica nova)

**Objetivo:** cheats que mudam *cálculos* do jogo, não só valores.

- Encontrar espaço livre no executável para código novo.
- Montar o código com `iced-x86` e desviar a execução para ele.
- Cheats:
  - Progressão: multiplicador de XP / Brain Power.
  - Itens: multiplicador ao coletar, durabilidade infinita, construção grátis.

**Pronto quando:** o multiplicador de XP funciona com o valor ajustável pela TUI.
**Você faz:** testar ganho de XP no jogo.

---

## Fase 7 (opcional) — DLL interna

**Objetivo:** recursos que um trainer externo não alcança.

- DLL Windows em Rust, carregada pelo jogo via override de DLL do Proton.
- Chamar funções do jogo diretamente (ex.: dar item X).
- Comunicação com a TUI externa.
- Possível overlay dentro do jogo.

**Só começa** se as fases 1–6 estiverem estáveis e houver funcionalidades que realmente precisem disso.

---

## Fora de escopo

- Multiplayer, qualquer interação com outros jogadores ou servidores.
- Burlar anti-cheat.
- Versão Game Pass/Windows.
- Distribuição pública ou reaproveitamento de código de terceiros (FLiNG, WeMod/Wand).
