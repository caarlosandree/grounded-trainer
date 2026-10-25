# CLAUDE.md

Complementa o `AGENTS.md` com orientações específicas para Claude Code. As regras do projeto ficam em `AGENTS.md`.

## Primeiro passo

1. Leia `AGENTS.md`.
2. Leia `docs/ROADMAP.md` e identifique a fase atual.
3. Leia `docs/ARCHITECTURE.md` e, se a tarefa envolver o jogo, `docs/RESEARCH.md`.
4. Procure a spec e o plano da tarefa em `docs/specs/` e `docs/plans/`.

## Comandos frequentes

```bash
cargo build --workspace
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test --workspace -- --ignored   # exige o jogo aberto
cargo run -p gt-cli -- inspect
cargo run -p gt-tui
```

## Gotchas operacionais

- O jogo roda dentro do container da Steam (pressure-vessel), mas o processo é visível no `/proc` do host. O PID do Linux não é o PID que o Wine mostra (ex.: o FLiNG registra `Pid=364`).
- Há vários processos com o nome do jogo na linha de comando (`reaper`, `proton`, `steam.exe`, `Grounded2.exe` launcher). O alvo é **`Grounded2Steam-Win64-Shipping.exe`**.
- O executável é carregado em `0x140000000`, mas leia a base de `/proc/<pid>/maps`; não assuma.
- `process_vm_writev` falha em páginas de código (somente leitura). Para patches de código, escreva via `/proc/<pid>/mem`.
- `kernel.yama.ptrace_scope` está em `0` nesta máquina. Se as leituras falharem com `EPERM`, verifique isso antes de depurar o código.
- Atualizações do jogo (Early Access) quebram assinaturas com frequência. Rode `gt inspect` depois de cada atualização.
- Para rodar ferramentas Windows no prefixo do jogo: `protontricks-launch --appid 2661300 <exe>`.

## Subagentes

Siga o mapeamento de modelos do CLAUDE.md global. Engenharia reversa e decisões de arquitetura ficam com o modelo principal; transcrição de planos detalhados pode ir para modelos menores.
