# grounded-trainer

> **Projeto de estudos de engenharia reversa (ER).** Um trainer nativo de Linux para o
> **Grounded 2** (Steam/Proton), escrito em Rust, feito para aprender na prática como um jogo
> Windows funciona por dentro: memória de processo, formato PE, x86-64, Unreal Engine 5 e
> escrita segura em código em execução.

O objetivo principal é **aprender**. O trainer é o pretexto: cada cheat é um problema de ER
completo (achar o código ou o dado, entender o que ele faz, alterar sem quebrar o jogo e
desfazer tudo ao sair). Todo achado fica documentado em [`docs/RESEARCH.md`](docs/RESEARCH.md),
com versão do jogo, endereço, bytes, desmontagem e como foi descoberto, inclusive as hipóteses
que não deram certo.

---

## Sumário

- [Aviso e limites](#aviso-e-limites)
- [O que o projeto estuda](#o-que-o-projeto-estuda)
- [Como funciona](#como-funciona)
- [Estado atual](#estado-atual)
- [Estrutura do repositório](#estrutura-do-repositório)
- [Requisitos](#requisitos)
- [Compilando e testando](#compilando-e-testando)
- [Ferramentas](#ferramentas)
- [Segurança: como o trainer evita estragar o jogo](#segurança-como-o-trainer-evita-estragar-o-jogo)
- [Metodologia de engenharia reversa](#metodologia-de-engenharia-reversa)
- [Fluxo de desenvolvimento](#fluxo-de-desenvolvimento)
- [Documentação](#documentação)
- [Licença](#licença)

---

## Aviso e limites

- **Fins educacionais.** Este repositório existe para estudo de engenharia reversa e
  programação de sistemas. Não há binários publicados.
- **Somente solo/offline.** Nada de multiplayer, rede, servidores ou contorno de anti-cheat.
  Isso está fora do escopo e não será aceito.
- **Nada é alterado em disco.** O trainer só mexe na memória do processo em execução; nunca nos
  arquivos do jogo nem nos saves.
- **Código de terceiros.** Outros trainers (como o FLiNG) foram desmontados para *entender* o que
  fazem, e o que se aprendeu está em `docs/RESEARCH.md`. Nenhum código ou byte deles é copiado
  ou redistribuído: a implementação é toda própria, escrita a partir desse entendimento.
- Grounded 2 é marca da Obsidian Entertainment / Xbox Game Studios. Este projeto não tem
  nenhuma relação com eles.
- Use por sua conta e risco. Faça backup do seu save antes de experimentar.

---

## O que o projeto estuda

| Tema | Onde aparece |
|---|---|
| Processos Linux, `/proc/<pid>/maps`, `process_vm_readv`, `/proc/<pid>/mem` | `gt-core` (`process`, `maps`, `mem`) |
| Como o Wine/Proton mapeia um `.exe` Windows num processo Linux | `gt-core` (`modules`, `pe`) |
| Formato PE: cabeçalhos, seções, `TimeDateStamp`, `SizeOfImage` | `gt-core::pe`, `gt diff` |
| Busca de padrões AOB com curingas e unicidade de assinatura | `gt-core::scan`, `memscan` |
| Desmontagem e montagem x86-64, fronteiras de instrução, relocação RIP-relativa | `gt-core::asm` (via `iced-x86`) |
| Escrita segura em código em execução: pausa de threads, conferência de bytes, diário | `gt-core` (`engine`, `suspend`, `journal`, `recovery`) |
| Reflexão do Unreal Engine 5: `FNamePool`, `GUObjectArray`, `UClass`, `FProperty` | `gt-ue` |
| Análise estática de outro trainer (Ghidra) e comparação memória × disco | `docs/RESEARCH.md`, `gt diff`, `gt fling` |

---

## Como funciona

O trainer é **externo**: um binário nativo de Linux que não roda dentro do Wine e não injeta
nada no jogo. Isso funciona porque o jogo, mesmo rodando pelo Proton, é um processo Linux comum,
e o Wine carrega o `.exe` no mesmo endereço virtual que o Windows usaria.

```
┌─────────────── Linux (host) ────────────────┐
│                                             │
│  gt-tui / gt-cli  ──/proc/<pid>/mem──►  ┌───┴──────────────────────────┐
│  (Rust nativo)    ◄─process_vm_readv──  │ Grounded2Steam-Win64-        │
│                                         │ Shipping.exe (Wine/Proton,   │
│                                         │ container pressure-vessel)   │
│                                         └───┬──────────────────────────┘
└─────────────────────────────────────────────┘
```

- **Achar o alvo:** varre `/proc/*/cmdline` atrás de `Grounded2Steam-Win64-Shipping.exe`,
  ignorando `reaper`, `proton`, `steam.exe` e o launcher. A base do módulo vem do
  `/proc/<pid>/maps`, nunca de um endereço fixo.
- **Ler:** `process_vm_readv` para leituras em massa (varrer os ~128 MB da `.text`).
- **Escrever:** `pwrite` em `/proc/<pid>/mem`, que permite escrever em páginas de código
  somente leitura quando há permissão de ptrace (`process_vm_writev` falharia nelas).
- **Versão do jogo:** identificada pelo `TimeDateStamp` + `SizeOfImage` do PE (ex.:
  `c935e799-0b27c000`). As assinaturas ficam em TOML, uma genérica e uma por build conferida.
- **Valores do jogo (Fase 5):** em vez de gerar um SDK, os offsets são lidos da própria reflexão
  do Unreal em execução, então continuam valendo depois de uma atualização do jogo.

Os detalhes e o porquê de cada decisão estão em [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

---

## Estado atual

O projeto avança por fases; cada uma só começa quando a anterior cumpre seu critério de pronto
(ver [`docs/ROADMAP.md`](docs/ROADMAP.md)).

| Fase | Conteúdo | Estado |
|---|---|---|
| 0 | Fundação: documentos, toolchain, workspace | ✅ |
| 1 | Núcleo de memória e ferramenta de pesquisa `gt` | ✅ |
| 2 | Motor de patches seguro (`gt-patch`) | ✅ |
| 3 | Interface no terminal (`gt-tui`) | ✅ |
| 4 | Primeiros cheats por troca de bytes | ✅ |
| 5 | Estruturas do Unreal Engine pela reflexão (vida, stamina, fome, sede…) | 🚧 em andamento |
| 6 | Code caves: lógica nova montada com `iced-x86` | ⏳ |
| 7 | (opcional) DLL interna carregada pelo Proton | ⏳ |

Cheats que já funcionam no jogo (Fase 4):

| Categoria | Cheat | Técnica |
|---|---|---|
| Itens | Durabilidade infinita | anular a subtração do desgaste |
| Itens | Tocha infinita | anular a soma da energia gasta |
| Itens | Recarga instantânea | a recarga restante termina no quadro seguinte |

---

## Estrutura do repositório

```
grounded-trainer/
├── crates/
│   ├── gt-core/        # genérico, não sabe nada do Grounded 2: processo, memória, PE, AOB,
│   │                   # assembly, motor de patches, diário e recuperação
│   ├── gt-ue/          # Unreal Engine 5: nomes, tabela de objetos, índice de classes e campos
│   ├── gt-grounded2/   # conhecimento do jogo: catálogo de cheats e assinaturas (signatures/*.toml)
│   └── gt-testkit/     # processo-alvo de teste e harness para testes de integração sem o jogo
├── apps/
│   ├── gt-cli/         # `gt`: ferramentas de pesquisa, somente leitura
│   ├── gt-patch/       # aplica e desfaz patches pela linha de comando
│   └── gt-tui/         # interface do jogador no terminal
└── docs/               # roadmap, arquitetura, pesquisa, specs e planos
```

**Regra de dependência:** `apps` → `gt-grounded2` → `gt-ue` → `gt-core`, nunca o contrário. O
`gt-core` deveria servir para outro jogo sem mudanças.

**Escrita isolada:** só `gt-patch` e `gt-tui` ativam a feature `write` do `gt-core`. O `gt` de
pesquisa é somente leitura por construção. Todo `unsafe` fica em `crates/gt-core/src/mem/`, com
comentário `// SAFETY:`.

---

## Requisitos

- Linux x86-64 (desenvolvido em Fedora 44, kernel 7.2, Wayland).
- Rust stable recente (edition 2024; o `rust-toolchain.toml` escolhe o canal `stable` com
  `rustfmt` e `clippy`).
- Grounded 2 pela Steam com Proton (AppID `2661300`) — só para os testes com o jogo e para usar.
- Permissão de ptrace sobre o processo do jogo: `kernel.yama.ptrace_scope = 0` ou a capability
  `CAP_SYS_PTRACE`. O trainer detecta a falta de permissão e explica o erro; ele **não** muda essa
  configuração sozinho.

```bash
cat /proc/sys/kernel/yama/ptrace_scope
```

---

## Compilando e testando

```bash
cargo build --workspace
```

```bash
cargo fmt --all --check
```

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

```bash
cargo test --workspace
```

Os testes que precisam do jogo aberto estão marcados com `#[ignore]`:

```bash
cargo test --workspace -- --ignored
```

| Nível | Como | Exige o jogo? |
|---|---|---|
| Unitário | busca AOB, parse de padrões, montagem de patches sobre buffers sintéticos | Não |
| Integração | um processo-alvo de teste (`gt-testtarget`) que o trainer lê, altera, pausa e restaura | Não |
| Jogo | `cargo test -- --ignored` com o jogo aberto | Sim |
| Manual | testar o efeito no jogo | Sim |

---

## Ferramentas

### `gt` — pesquisa (somente leitura)

```bash
cargo run -p gt-cli -- inspect
```

| Comando | O que faz |
|---|---|
| `gt inspect` | processo, base do módulo, seções do PE e chave de versão do jogo |
| `gt scan "<padrão>"` | onde um padrão AOB aparece (ex.: `"48 8B ?? ?? 05"`) |
| `gt disasm <endereço\|módulo+offset>` | desmonta x86-64 a partir de um endereço |
| `gt diff` | compara o código na memória com o executável em disco e revela patches de outros trainers |
| `gt read` / `gt dump` | hexdump e cópia de memória para arquivo |
| `gt fling` | estado dos pontos que o trainer FLiNG alterou |

### `gt-patch` — patches pela linha de comando

| Comando | O que faz |
|---|---|
| `gt-patch list` | versão do jogo, estado de cada patch e se há diário pendente (não escreve) |
| `gt-patch run <ID>...` | aplica os patches e desfaz ao receber Ctrl+C ou quando o jogo fecha |
| `gt-patch toggle <ID> --times N` | liga e desliga várias vezes e mede a pausa do jogo |
| `gt-patch restore` | desfaz o que ficou pendente no diário (ex.: depois de um SIGKILL) |
| `gt-patch profile` | amostra em que ponto as threads do jogo param durante as pausas |

### `gt-tui` — interface do jogador

```bash
cargo run -p gt-tui
```

Roda num terminal ao lado do jogo. Lista os cheats por categoria com estado `[ON ]`, `[OFF]`,
`?` (não verificado) ou `indisponível (<motivo>)`, mostra a conexão e a versão do jogo e reconecta
sozinha se o jogo reiniciar.

| Tecla | Ação |
|---|---|
| `↑↓` / `jk` | mover |
| `espaço` / `Enter` | ligar e desligar |
| `←→` / `hl` | ajustar o valor |
| `r` | tentar de novo (retomar o jogo, recuperar o diário, refazer a busca) |
| `q` / `Esc` / Ctrl+C | desligar tudo e sair |

Os comandos são lidos no terminal, sem atalhos globais (o Wayland não permite capturar o teclado
de outra janela). O log vai para `$XDG_STATE_HOME/grounded-trainer/gt-tui.log`.

---

## Segurança: como o trainer evita estragar o jogo

Escrever em código de um processo em execução é a parte delicada. Regras do motor:

1. **Conferir antes de escrever.** Se os bytes atuais não forem exatamente os esperados, o patch
   é recusado. Isso protege contra uma versão diferente do jogo ou contra outro trainer ativo.
2. **Assinaturas únicas.** Um padrão com 0 ou 2+ ocorrências é tratado como indisponível, nunca
   como "pega a primeira".
3. **Diário antes da escrita.** A intenção é gravada de forma atômica em
   `$XDG_STATE_HOME/grounded-trainer/journal.json` antes de tocar no jogo, com uma trava que
   garante uma instância por vez.
4. **Pausa conferida.** O jogo é pausado com `SIGSTOP` (pelo `pidfd`) e o trainer confere, thread
   por thread, que nenhuma está parada no meio das instruções que vão mudar. Se alguma estiver,
   retoma e tenta de novo. A pausa típica é de ~15 ms.
5. **Identidade do processo.** PID + horário de início + `boot_id`: um PID reaproveitado nunca
   recebe sinal nem escrita.
6. **Restauração garantida.** Tudo é desfeito ao sair, com Ctrl+C, SIGTERM, SIGHUP ou pânico. Se o
   trainer for morto com SIGKILL, a próxima execução lê o diário e restaura os bytes originais
   onde eles ainda são os nossos.
7. **Escrita restrita.** Só nas seções executáveis do módulo do jogo, com a extensão do módulo
   confirmada pelo arquivo em disco.

---

## Metodologia de engenharia reversa

- **Endereços nunca vão para o código.** Cada ponto vira uma assinatura AOB em
  `crates/gt-grounded2/signatures/*.toml`, com os bytes esperados e os de substituição.
  `generic.toml` vale para qualquer versão; `<chave>.toml` fixa o `rva` conferido de uma build.
- **Todo achado é registrado** em `docs/RESEARCH.md`: versão do jogo, data, `módulo+offset`,
  bytes, desmontagem, como foi descoberto e o que foi descartado.
- **Ferramentas usadas:** o próprio `gt` (inspect, scan, disasm, diff), Ghidra para análise
  estática e a reflexão do Unreal para achar classes e campos pelo nome.
- **Comparar memória e disco** (`gt diff`) é a forma mais rápida de ver o que outro trainer mudou
  e, a partir daí, entender a lógica do jogo naquele ponto.

---

## Fluxo de desenvolvimento

Todo recurso passa por `spec → plano → código`:

1. **Spec** em `docs/specs/NNN-nome.md`: o que a funcionalidade faz e como saber que funciona.
2. **Plano** em `docs/plans/NNN-nome.md`: passos pequenos, cada um com sua verificação.
3. **Código**, seguindo o plano.
4. **Validação:** `fmt`, `clippy` e testes e, quando envolve o jogo, teste manual no jogo.

Grande parte do código é escrita com assistência de IA (Claude Code), guiada pelas regras de
[`AGENTS.md`](AGENTS.md) e pelos documentos acima. Commits seguem Conventional Commits em pt-BR.

---

## Documentação

| Documento | Conteúdo |
|---|---|
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | fases, ordem e critério de pronto |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | decisões técnicas e o porquê de cada uma |
| [`docs/RESEARCH.md`](docs/RESEARCH.md) | diário de engenharia reversa do jogo |
| [`docs/specs/`](docs/specs) | especificações por funcionalidade |
| [`docs/plans/`](docs/plans) | planos de implementação |
| [`AGENTS.md`](AGENTS.md) | regras do projeto (segurança, código, git) |

---

## Licença

Distribuído sob a licença MIT. Veja [`LICENSE`](LICENSE).
