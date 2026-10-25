# Arquitetura — grounded-trainer

## 1. Visão geral

Trainer **externo**, binário nativo de Linux. Ele não roda dentro do Wine nem injeta nada no jogo (até a Fase 7 opcional). Funciona porque o jogo, mesmo rodando pelo Proton, é um processo Linux comum: o Wine carrega o `.exe` no mesmo endereço virtual que o Windows usaria.

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

## 2. Linguagem: Rust

| Critério | Decisão |
|---|---|
| Leitura/escrita de memória | Empate com Go |
| Montar/desmontar x86-64 (code caves, fronteiras de instrução) | **Rust**: `iced-x86` monta *e* desmonta; em Go só há desmontador |
| DLL Windows futura (Fase 7) | **Rust**: `cdylib` enxuta para `x86_64-pc-windows-gnu`; Go embutiria runtime e GC no processo do jogo |
| Structs com layout C (SDK do Unreal) | **Rust**: `#[repr(C)]` |
| Curva de aprendizado | Irrelevante: o código é escrito pela IA |

## 3. Workspace

```
grounded-trainer/
├── Cargo.toml                    # workspace
├── crates/
│   ├── gt-core/                  # genérico, não sabe nada do Grounded 2
│   │   └── src/
│   │       ├── process.rs        # achar processo, ler /proc/<pid>/maps, módulos
│   │       ├── mem/              # ÚNICO lugar com unsafe: leitura/escrita
│   │       ├── pe.rs             # cabeçalhos PE lidos da memória (seções)
│   │       ├── scan.rs           # padrões AOB com curingas
│   │       ├── asm.rs            # wrappers iced-x86: disasm, montagem, relocação RIP-relative
│   │       ├── patch.rs          # Patch, CodeCave, Freeze + guarda de restauração
│   │       └── suspend.rs        # pausar/retomar o processo durante escritas
│   ├── gt-ue/                    # estruturas do Unreal Engine 5 (Fase 5)
│   │   └── src/                  # FName/GNames, GObjects, GWorld, leitura de UObjects
│   └── gt-grounded2/             # conhecimento específico do jogo
│       ├── src/cheats/           # 1 módulo por cheat (sobrevivência, itens, progressão, movimento)
│       └── signatures/           # *.toml por build do jogo
├── apps/
│   ├── gt-cli/                   # ferramentas de pesquisa: inspect, read, scan, disasm, diff, dump, fling
│   └── gt-tui/                   # interface do jogador
└── docs/
```

**Regra de dependência:** `gt-tui`/`gt-cli` → `gt-grounded2` → `gt-ue` → `gt-core`. Nunca o contrário. `gt-core` deve servir para outro jogo sem mudanças.

## 4. Dependências principais

| Crate | Uso |
|---|---|
| `iced-x86` | Desmontar e montar x86-64 (code caves, cálculo de tamanho de instrução) |
| `goblin` | Interpretar cabeçalhos PE |
| `nix` | `process_vm_readv`, sinais (`SIGSTOP`/`SIGCONT`) |
| `ratatui` + `crossterm` | TUI |
| `clap` | CLI |
| `serde` + `toml` | Arquivos de assinatura e configuração |
| `thiserror` / `anyhow` | Erros (libs / binários) |
| `tracing` + `tracing-subscriber` | Logs (em arquivo enquanto a TUI estiver aberta) |
| `signal-hook` | Restauração ao receber SIGINT/SIGTERM |

As versões são fixadas no `Cargo.toml` na Fase 0, usando a estável mais recente.

## 5. Decisões técnicas

### 5.1 Acesso à memória
- **Leitura em massa** (varrer os ~128 MB da `.text`): `process_vm_readv`, que é rápido e não exige anexar ao processo (`PTRACE_ATTACH`), mas passa pela mesma checagem de permissão de ptrace (afetada pelo Yama).
- **Escrita**: `pwrite` em `/proc/<pid>/mem`. O kernel permite escrever em páginas somente leitura (código) quando o processo tem permissão de ptrace. `process_vm_writev` falharia nelas.
- **Permissão**: exige `kernel.yama.ptrace_scope = 0` (estado atual) ou `CAP_SYS_PTRACE`. O trainer detecta e explica o erro em vez de falhar silenciosamente.

### 5.2 Localização do alvo
- PID: varrer `/proc/*/cmdline` pelo nome `Grounded2Steam-Win64-Shipping.exe`, excluindo os processos `reaper`/`proton`/launcher.
- Base: primeira região do arquivo do executável em `/proc/<pid>/maps` com offset 0.
- Seções: lidas do cabeçalho PE **na memória**, não do disco.

### 5.3 Versão do jogo e assinaturas
- Chave de versão: campo `TimeDateStamp` do PE (hash de build reprodutível, ex. `c935e799`) + `SizeOfImage`.
- `signatures/<chave>.toml` guarda padrões, offsets e bytes esperados por versão. Uma versão desconhecida usa os padrões genéricos (só AOB) e marca como "não verificado".
- Cada cheat informa o próprio estado: disponível, padrão não encontrado ou padrão ambíguo.

### 5.4 Escrita segura de patches
1. Verificar que os bytes atuais são os esperados (se não, recusar).
2. **Pausar o processo** (`SIGSTOP`), escrever e retomar (`SIGCONT`). Isso evita que uma thread do jogo execute uma instrução escrita pela metade. A pausa dura poucos milissegundos.
3. Registrar o patch no `PatchRegistry`, que desfaz tudo em `Drop`, em pânico (`panic hook`) e em SIGINT/SIGTERM.
4. Se o jogo fechar, o registro é descartado sem tentar escrever.

### 5.5 Code caves (Fase 6)
Sem `VirtualAllocEx` de fora do processo, existem duas estratégias:
- **Padrão: espaço livre do próprio executável**, ou seja, sequências de `CC`/`00` (preenchimento entre funções) na `.text`. Ficam perto do código (`jmp rel32`, ±2 GB) e não exigem alocação. O alocador de caves rastreia o que já foi usado.
- **Alternativa, se faltar espaço:** ptrace numa thread do jogo para executar um `mmap` dentro do processo. Funciona por ser um processo Linux, mas é mais arriscado; só se necessário e com spec própria.

As instruções originais copiadas para a cave passam por relocação (endereços relativos ao RIP são recalculados com `iced-x86`).

### 5.6 Valores via Unreal Engine (Fase 5)
- Offsets das classes vêm de um dump do **Dumper-7** (rodado no prefixo do jogo, uma vez por atualização), convertidos para um `offsets/<chave>.toml`.
- `GNames`/`GObjects`/`GWorld` são resolvidos por assinatura AOB.
- "Congelar" valores = um laço de ~10 Hz que reescreve o campo. Ele roda numa thread do trainer, nunca dentro do jogo.

### 5.7 Interface
- `gt-tui`: ratatui num terminal ao lado do jogo. Comandos por teclado **no terminal** (sem hotkeys globais, por causa do Wayland).
- Hotkeys globais via evdev (`/dev/input`, exige o grupo `input`) ficam como melhoria futura, com spec própria.

## 6. Estratégia de testes

| Nível | Como | Exige jogo? |
|---|---|---|
| Unitário | Busca AOB, parse de padrão, montagem e relocação de patches sobre buffers sintéticos | Não |
| Integração local | Processo-alvo de teste (binário Rust simples) que o trainer modifica: valida leitura, escrita, pausa e restauração | Não |
| Jogo | `#[ignore]` + `cargo test -- --ignored` com o jogo aberto | Sim |
| Manual | Dono testa no jogo e confirma o efeito | Sim |

## 7. Ambiente de referência

- Fedora 44, kernel 7.2, Wayland, GPU RTX 3060 Laptop.
- Steam nativa (`~/.local/share/Steam`), Proton Experimental, Steam Linux Runtime 4.
- Grounded 2: AppID `2661300`, executável em `steamapps/common/Grounded2/Augusta/Binaries/Win64/`.
- Toolchain: `rustup` stable (Fase 0). Target `x86_64-pc-windows-gnu` + `mingw64-gcc` só na Fase 7.

## 8. Registro de decisões

| Data | Decisão | Motivo |
|---|---|---|
| 2026-10-22 | Rust em vez de Go | `iced-x86` para code caves; caminho para DLL na Fase 7 |
| 2026-10-22 | Trainer externo primeiro | Mais simples e seguro; nada é injetado no jogo |
| 2026-10-22 | TUI em vez de GUI | Simples, funciona bem no Wayland |
| 2026-10-22 | Assinaturas em TOML por versão | O jogo atualiza com frequência (Early Access) |
