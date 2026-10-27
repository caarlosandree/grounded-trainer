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
- **Ressalvas das travas de barra** (spec 005, 7): numa trava de barra (vida, stamina, comida, água, oxigênio, temperatura), desligar só para de escrever (a barra fica onde está, decisão do dono), e a conferência antes de escrever é a identidade do objeto e do campo (guarda do objeto, caminho revalidado), não bytes esperados, porque o valor muda o tempo todo. Bits continuam reversíveis: o original vai ao diário antes da escrita.
- **Ressalvas das ações** (spec 006, 6.6 e 7): uma ação (`add`, hoje só "Somar ciência") é uma escrita única **sem desfazer**, só onde o dono decidiu (a ciência vai para o save). Antes de escrever, o caminho é revalidado naquele ciclo e o resultado precisa ficar na faixa sã (`result_max`); o valor é sempre `atual + quantidade`, nunca um número fixo. Uma ação só roda por tecla do dono, no máximo uma vez por segundo, e nunca por religar, recuperação, manutenção ou procura.
- **Ressalvas dos ajustes `set`** (spec 006, 6.4 e 7): o original vai ao diário antes da escrita e volta ao desligar, na saída e na recuperação, mas é o **último valor que o jogo pôs no campo**, e não necessariamente o do momento de ligar. A conferência antes de escrever é a identidade do objeto e do campo mais o valor do jogo ser são, e não bytes esperados, como nas travas de barra. Restaurar é tudo ou nada: o campo inteiro igual ao nosso volta ao original; qualquer outro valor só esquece a entrada, sem escrever.
- **Valores acima da faixa normal do jogo nos `set`** (spec 006, 4): dano ×100, carga 100 e `JumpMaxCount` 1000 são aceitos, porque o original volta ao desligar. Cada cheat continua limitado pela grade dele; o que o jogo faz com esses valores (inclusive no save) é conferido no teste com o dono.
- **Ressalvas dos desvios com code cave** (spec 008, 6.3, 6.4 e 7): um desvio troca **uma instrução** do jogo por um `jmp` para código nosso, escrito em sequências de `CC` da `.text`, com variáveis na folga da `.data` do executável. Isso é uma **exceção explícita** à regra de que a `.data` dos módulos nunca é escrita, só para essa folga: ela é conferida antes (região `rw-` privada dentro do módulo, todos os bytes zero no primeiro uso) e a cave só é escrita onde **todos** os bytes são `CC` conferidos na hora (com 16 bytes `CC` intactos no começo da sequência). O desvio é um patch comum (bytes esperados, original no diário, pausa com o `pc` das threads conferido); a cave é um patch do mesmo motor, também sob pausa. **Desligar devolve só o desvio**: cave e variáveis ficam até sair, porque uma thread ainda pode estar dentro da cave. A cave só é apagada com a pausa e a conferência de que **nenhuma thread tem `pc` dentro dela** (até 5 tentativas); se não der, ela fica no diário, inalcançável, para a próxima execução. As variáveis **não têm diário de valor** (o trainer muda o valor sem pausa); o diário guarda só a região, que é zerada na saída e na recuperação, **só quando nenhuma entrada nossa resta**. Toda cave é segura com as variáveis zeradas (passa direto), nunca chama função do jogo e só desreferencia ponteiros que o próprio jogo desreferencia no ponto. O progresso e os itens que os cheats de Brain Power, quantidade ao arrastar, coleta e analisador criam vão para o save (propósito dos cheats, sem desfazer).
- **Restauração garantida**: ao sair do trainer (normal, Ctrl+C, SIGTERM, pânico), desfaça todos os patches ativos.
- **Nunca altere arquivos do jogo em disco** (`steamapps/common/Grounded2`). Só memória do processo em execução.
- **Saves:** nenhuma funcionalidade mexe em arquivos de save. Antes de testes arriscados, peça ao dono para fazer backup do save.
- **Somente solo/offline**: nada de rede, multiplayer, servidores ou contorno de anti-cheat.
- **Código de terceiros (FLiNG, WeMod/Wand e outros):** é permitido desmontar e decompilar o código que eles injetam no jogo para entender o que fazem, registrando o achado em `RESEARCH.md` (o quê, onde, como foi descoberto). É proibido copiar código ou bytes deles para o trainer, ou redistribuí-los: a implementação é sempre nossa, escrita a partir do entendimento.
- Nunca use `sudo`, nem altere `kernel.yama.ptrace_scope` ou outra configuração do sistema, sem pedir.

## 4. Engenharia reversa

- Todo achado vai para `docs/RESEARCH.md` com: **versão do jogo**, data, endereço (`módulo+offset`), bytes, desmontagem e como foi descoberto.
- Endereços fixos nunca vão para o código: use **assinaturas AOB** em `signatures/*.toml`, versionadas por build do jogo.
- Assinaturas devem ser únicas no módulo. Uma assinatura com 0 ou 2+ ocorrências é tratada como "indisponível", nunca "pega a primeira".
- **Exceção, só para leitura de globais** (spec 005, 6.2): o padrão de uma global do motor (`GUObjectArray`, `FNamePool`) é aceito com várias ocorrências desde que todas resolvam para o mesmo endereço. Patches continuam exigindo ocorrência única.
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
