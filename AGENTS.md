# AGENTS.md — Regras do projeto grounded-trainer

Regras para qualquer agente de IA que trabalhe neste repositório. O código é escrito pela IA; o
dono do projeto define **o que** quer e participa dos testes no jogo.

Responda sempre em português brasileiro.

## 1. Documentos são a fonte de verdade

| Documento | Papel |
|---|---|
| `docs/ROADMAP.md` | Fases, ordem e critério de pronto |
| `docs/ARCHITECTURE.md` | Decisões técnicas e o porquê de cada uma |
| `docs/RESEARCH.md` | Achados de engenharia reversa do jogo |
| `docs/specs/NNN-nome.md` | **O que** uma funcionalidade faz e como saber que funciona |
| `docs/plans/NNN-nome.md` | **Como** implementar uma spec, em passos verificáveis |

- Leia `ROADMAP.md` e `ARCHITECTURE.md` antes de qualquer tarefa.
- Se uma decisão mudar, atualize o documento **antes** do código.

## 2. Fluxo obrigatório: spec → plano → código

1. **Spec:** escrever `docs/specs/NNN-nome.md` e pedir aprovação do dono.
2. **Plano:** escrever `docs/plans/NNN-nome.md` com passos pequenos, cada um com verificação.
3. **Código:** implementar seguindo o plano e marcar os passos concluídos.
4. **Validação:** rodar a seção 6 e, quando envolver o jogo, pedir o teste manual ao dono.

Nenhum código de funcionalidade sem spec aprovada. Exceções: correções triviais e ferramentas descartáveis de pesquisa em `scratch/` (ignorado pelo git).

## 3. Segurança do jogo e do usuário (inegociável)

- **Toda escrita na memória do jogo é reversível**: guarde os bytes originais antes de escrever.
- **Verifique antes de escrever**: se os bytes atuais no endereço não forem os esperados, recuse o patch e informe. Isso evita corromper o jogo numa versão diferente.
- **Restauração garantida**: ao sair do trainer (normal, Ctrl+C, SIGTERM, pânico), desfaça todos os patches ativos.
- **Nunca altere arquivos do jogo em disco** (`steamapps/common/Grounded2`). Só memória do processo em execução.
- **Saves:** nenhuma funcionalidade mexe em arquivos de save. Antes de testes arriscados, peça ao dono para fazer backup do save.
- **Somente solo/offline**: nada de rede, multiplayer, servidores ou contorno de anti-cheat.
- **Código de terceiros**: nunca copiar, decompilar para reaproveitar ou redistribuir código do FLiNG, WeMod/Wand ou outros. Observar o *comportamento* (ex.: quais bytes do jogo eles alteram) é permitido e deve ser registrado em `RESEARCH.md`.
- Nunca use `sudo`, nem altere `kernel.yama.ptrace_scope` ou outra configuração do sistema, sem pedir.

## 4. Engenharia reversa

- Todo achado vai para `docs/RESEARCH.md` com: **versão do jogo**, data, endereço (`módulo+offset`), bytes, desmontagem e como foi descoberto.
- Endereços fixos nunca vão para o código: use **assinaturas AOB** em `signatures/*.toml`, versionadas por build do jogo.
- Assinaturas devem ser únicas no módulo. Uma assinatura com 0 ou 2+ ocorrências é tratada como "indisponível", nunca "pega a primeira".
- Ao final de cada sessão de pesquisa, registre também as hipóteses descartadas.

## 5. Código

- Rust stable, edition 2024. Identificadores em inglês; comentários, docs e mensagens ao usuário em pt-BR.
- `unsafe` só em `crates/gt-core/src/mem/` e com comentário `// SAFETY:` explicando a garantia.
- Erros: `thiserror` nas libs, `anyhow` nos binários. Sem `unwrap()`/`expect()` fora de testes, exceto invariantes documentadas.
- Logs com `tracing`; nunca `println!` em libs.
- Funções puras (parse de padrão, busca, montagem de patch) devem ser testáveis sem o jogo aberto.

## 6. Validação antes de dizer "pronto"

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- Testes que precisam do jogo aberto ficam marcados com `#[ignore]` e rodam com `cargo test -- --ignored` com o jogo aberto.
- Funcionalidade que altera o jogo só está pronta depois do **teste manual no jogo** confirmado pelo dono.
- Se alguma validação não puder ser executada, diga qual e por quê.

## 7. Papel do dono do projeto

O dono não precisa ler o código. A IA deve:
- Explicar cada fase em termos de **resultado no jogo**.
- Pedir ações concretas e curtas ("abra o jogo e entre no save de teste", "ative X e diga se a vida parou de cair").
- Pedir aprovação em specs e em mudanças de escopo ou arquitetura.

## 8. Git

- Não faça commit, push, merge, rebase ou amend sem pedido explícito.
- Mensagens no padrão Conventional Commits em pt-BR.
