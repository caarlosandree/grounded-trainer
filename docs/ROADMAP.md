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
**Status:** ✅ concluída.

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
**Status:** ✅ concluída em 2026-10-25 (spec e plano `001-nucleo-memoria-gt`).

---

## Fase 2 — Motor de patches seguro

**Objetivo:** alterar o jogo de forma **reversível e à prova de crash**.

- `Patch`: substituir bytes, guardando os originais.
- Verificação dos bytes esperados antes de escrever: se não baterem, o patch é recusado (proteção contra versão errada).
- Pausar o jogo por alguns milissegundos durante a escrita (evita uma thread do jogo executar uma instrução pela metade).
- Restauração garantida: ao sair, com Ctrl+C, em pânico do trainer ou se o jogo fechar.
- Detecção de versão do jogo e arquivo de assinaturas por versão (`signatures/*.toml`).

**Pronto quando:** um patch de teste liga e desliga 100 vezes seguidas sem crash; Ctrl+C, SIGTERM e pânico restauram os bytes na hora; SIGKILL restaura na próxima execução, pelo diário (`gt-patch restore`).
**Você faz:** confirmar que o jogo continua estável.
**Status:** ✅ concluída em 2026-10-25 (spec e plano `002-motor-patches`; binário `gt-patch`).

---

## Fase 3 — Interface TUI

**Objetivo:** controlar os cheats confortavelmente com o jogo aberto.

- Lista de cheats por categoria com estado `ON/OFF/indisponível`.
- Valores ajustáveis (multiplicadores, velocidade).
- Status: jogo conectado, versão detectada, padrões encontrados/ausentes.
- Reconexão automática se o jogo reiniciar.

**Pronto quando:** dá para ligar e desligar os cheats existentes só pela TUI.
**Você faz:** opinar sobre usabilidade e layout.
**Status:** ✅ concluída em 2026-10-26 (spec e plano `003-interface-tui`; binário `gt-tui`). Critério de pronto
aprovado no jogo pelo dono com o patch de teste. O controle de valor foi provado só contra o alvo de teste, porque
ainda não há cheat real com valor (Fase 4).

---

## Fase 4 — Primeiros cheats (patch de código simples)

**Status:** ✅ concluída em 2026-10-26 (spec e plano `004-cheats-patch-simples`). Os três cheats confirmados no jogo
pelo dono pela TUI, inclusive depois de reiniciar o jogo.

**Objetivo:** primeiras funcionalidades reais, com troca de bytes do mesmo tamanho pelo motor da Fase 2, sem código
novo de motor.

Reorganizada em 2026-10-26 depois da análise do FLiNG (`RESEARCH.md`, catálogo): os cheats que estavam aqui
precisam do pawn do jogador ou de código novo e foram para as Fases 5 e 6.

| Categoria | Cheat | Ponto (build `c935e799`) |
|---|---|---|
| Itens | Durabilidade infinita | `+5a4e777`: anular a subtração do desgaste |
| Itens | Tocha infinita (energia da tocha e de itens com energia) | `+5a5744b`: anular a soma da energia gasta |
| Itens | Recarga instantânea de item | `+5c3bbfc`: a recarga restante termina no quadro seguinte |

**Pronto quando:** cada cheat da tabela funciona no jogo, liga e desliga pela TUI e sobrevive a reiniciar o jogo.
**Você faz:** testar cada cheat no jogo e dizer se funcionou.

---

## Fase 5 — Estruturas do Unreal Engine (valores)

**Objetivo:** ler e escrever valores do jogo pelo *nome* da estrutura, não por endereço solto. É a base para a
maioria dos cheats.

- Offsets lidos da **reflexão do jogo em execução** (`GUObjectArray` + `FNamePool` + `FProperty`), sem gerar SDK
  com o Dumper-7 (decisão de 2026-10-26; achados em `RESEARCH.md`).
- Achar o controller do jogador e o pawn (`Controller.Pawn`), e dele os componentes.
- Cheats (por escrita de valor, com laço de "congelar" quando preciso):
  - Sobrevivência: vida, stamina, comida, água, oxigênio, temperatura estável, sem dano de queda; tentativa de god
    mode pelos bits de morte do `HealthComponent`.
  - Itens: capacidade de carga.
  - Progressão: dano causado (`CustomDamageMultiplier`), ciência.
  - Movimento: velocidade, pulos infinitos, gravidade. **Teleporte** (salvar/carregar posição) e **velocidade do jogo** (speedhack) ficam **fora da Fase 5** (decisão do dono: o jogo tem viagem rápida e o dono quer explorar; a velocidade do jogo é a menor prioridade, sem fase prevista).

**Pronto quando:** os cheats de sobrevivência funcionam e continuam funcionando depois de uma atualização do jogo
sem mudar nada no trainer (os offsets vêm do próprio jogo).
**Você faz:** testar os cheats no jogo.

A fase foi dividida em duas specs:
- **Spec 005 — Sobrevivência por reflexão** (`docs/specs/005-sobrevivencia-por-reflexao.md`): leitor de reflexão
  (`gt-ue`), comandos `gt ue`, escrita em dados no motor e os oito cheats de sobrevivência (vida, stamina, comida,
  água, oxigênio, temperatura, sem dano de queda, não morrer). **Fechada em 2026-10-26**: os oito funcionaram no
  jogo com o dono (branch `fase-5-sobrevivencia`). Pendência: confirmar na próxima atualização do jogo que tudo
  continua funcionando sem mudar o trainer.
- **Spec 006 — o resto da Fase 5:** carga, dano causado, ciência (como ação de somar) e movimento (velocidade, pulos infinitos, gravidade). **Em andamento**: spec aprovada e implementada (`docs/specs/006-progressao-e-movimento.md`); falta o teste no jogo com o dono para fechar a fase.

---

## Fase 6 — Code caves (lógica nova)

**Objetivo:** cheats que mudam *cálculos* do jogo, não só valores.

- Encontrar espaço livre no executável para código novo.
- Montar o código com `iced-x86` e desviar a execução para ele.
- Cheats:
  - Sobrevivência: god mode por desvio (se a versão por valor da Fase 5 não servir), multiplicador de defesa.
  - Progressão: Brain Power rápido, congelar a hora do dia / velocidade do tempo.
  - Itens: cargas infinitas do analisador, quantidade ao arrastar, construção sem requisitos.

**Pronto quando:** o Brain Power rápido funciona no jogo.
**Você faz:** testar os cheats no jogo.

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
