//! Interface de alvo (spec 003, 5): o que o núcleo da TUI precisa do jogo, sem saber se o
//! outro lado é o Grounded 2 ou o processo-alvo de teste.
//!
//! Também mora aqui a classificação pura dos desfechos de uma operação ([`outcome_of`]) e do
//! relatório de recuperação ([`recovery_outcome`]), usada pelas duas implementações.

use std::any::Any;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gt_core::engine::{BatchReport, RestoreReport};
use gt_core::journal::DiscardReason;
use gt_core::procinfo::Identity;
use gt_core::recovery::RecoveryReport;
use gt_core::value::ValueGrid;
use gt_grounded2::fields::{ActionOutcome, PlayerState, TickReport};
use gt_grounded2::player::Absent;
use gt_grounded2::signatures::Category;

use crate::model::Avail;

/// Resultado da varredura de `cmdline` atrás do processo do jogo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Find {
    /// Nenhum processo do jogo.
    NotRunning,
    /// Um processo (PID no Linux).
    Found(i32),
    /// Mais de um candidato.
    Many(Vec<i32>),
    /// Com `--pid`: o processo não existe.
    PidGone,
    /// Outra falha ao procurar (motivo); a TUI mostra `erro` e tenta de novo.
    Error(String),
}

/// Falha ao abrir o alvo e buscar o catálogo.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    /// O kernel negou o acesso; traz a dica de permissão.
    #[error("{0}")]
    NoAccess(String),
    /// O processo fechou durante a busca (ou não é mais o mesmo).
    #[error("o processo terminou")]
    Gone,
    /// Qualquer outra falha (motivo).
    #[error("{0}")]
    Other(String),
}

/// Outros erros da biblioteca da TUI.
#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    /// Falha com o motivo já em texto para o usuário.
    #[error("{0}")]
    Other(String),
}

/// Um cheat do catálogo, como a busca o viu.
#[derive(Debug, Clone, PartialEq)]
pub struct CheatInfo {
    /// Identificador estável.
    pub id: String,
    /// Nome mostrado.
    pub name: String,
    /// Categoria.
    pub category: Category,
    /// Definição do valor ajustável, se houver.
    pub value: Option<ValueGrid>,
    /// Disponibilidade segundo a busca.
    pub avail: Avail,
    /// Cheat de valor (`[[field]]`, spec 005), não patch.
    pub field: bool,
    /// Ação de uma execução (`rule = "add"`, spec 006 6.6), não um cheat que liga e desliga.
    pub action: bool,
    /// Texto da quantidade na linha de uma ação, no lugar da formatação da grade (spec 008, 6.8.2).
    pub label: Option<String>,
}

/// Resultado de abrir o alvo e buscar o catálogo; `P` é o que cada implementação leva
/// de volta para abrir a sessão e o motor.
#[derive(Debug, Clone)]
pub struct Search<P> {
    /// PID no Linux.
    pub pid: i32,
    /// Identidade do processo no momento da busca.
    pub identity: Identity,
    /// Chave da versão do jogo.
    pub version_key: String,
    /// Se há arquivo de assinaturas para a versão.
    pub has_version_file: bool,
    /// Cabeçalho do disco: `Ok`, ou o motivo de o motor não poder abrir.
    pub disk: Result<(), String>,
    /// Os cheats buscados.
    pub cheats: Vec<CheatInfo>,
    /// `None` na busca completa; `Some(lista)` numa nova busca só desses `id`.
    pub ids: Option<Vec<String>>,
    /// Quanto a busca levou.
    pub took: Duration,
    /// Dados próprios da implementação.
    pub payload: P,
}

/// Abre o alvo e busca o catálogo (roda na thread de busca). Numa nova busca, recebe a
/// identidade da conexão e devolve [`SearchError::Gone`] se o processo não for mais o mesmo.
pub type SearchFn<P> = Arc<
    dyn Fn(i32, Option<Vec<String>>, Option<Identity>) -> Result<Search<P>, SearchError>
        + Send
        + Sync,
>;

/// Resultado da recuperação do diário.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recovered {
    /// Diário resolvido.
    Clean {
        /// Linhas do relatório, para o registro de eventos.
        lines: Vec<String>,
        /// Para onde o diário ilegível foi movido.
        corrupt: Option<PathBuf>,
    },
    /// Sobraram entradas (ou a recuperação falhou): nada é escrito até o `r` resolver.
    Blocked {
        /// Linhas do que foi resolvido.
        lines: Vec<String>,
        /// Aviso fixo.
        text: String,
    },
}

/// Por que o diário foi descartado.
fn discard_text(reason: DiscardReason) -> &'static str {
    match reason {
        DiscardReason::ProcessGone => "o jogo fechou",
        DiscardReason::OtherProcess => "o jogo reiniciou",
        DiscardReason::OtherBoot => "o sistema reiniciou",
    }
}

/// Classifica o resultado da recuperação (textos do `print_recovery` do `gt-patch`).
pub fn recovery_outcome(res: gt_core::Result<RecoveryReport>) -> Recovered {
    let report = match res {
        Ok(report) => report,
        Err(e) => {
            return Recovered::Blocked {
                lines: Vec::new(),
                text: format!("a recuperação do diário falhou: {e}; aperte r para tentar de novo"),
            };
        }
    };
    let mut lines = Vec::new();
    if let Some(reason) = report.discarded {
        lines.push(format!("diário descartado: {}", discard_text(reason)));
    }
    if report.resumed {
        lines.push("retomado o jogo parado".to_string());
    }
    if report.stopped_by_other {
        lines.push("o jogo estava parado por outro processo; não foi retomado".to_string());
    }
    for id in &report.restored {
        lines.push(format!("restaurado: {id}"));
    }
    for id in &report.dropped {
        lines.push(format!("descartado: {id} (bytes já originais)"));
    }
    for id in &report.foreign {
        lines.push(format!("não restaurado: {id} (bytes alterados por outro)"));
    }
    if !report.kept.is_empty() {
        return Recovered::Blocked {
            lines,
            text: format!(
                "não foi possível restaurar {} agora; aperte r para tentar de novo",
                report.kept.join(", ")
            ),
        };
    }
    Recovered::Clean {
        lines,
        corrupt: report.corrupt,
    }
}

/// Desfecho de uma operação do motor (ligar, desligar, trocar valor, restaurar, retomar).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpOutcome {
    /// Feita; quanto o jogo ficou parado.
    Done(Duration),
    /// Recusada sem deixar nada escrito (motivo).
    Refused(String),
    /// Os bytes eram de outro: o patch saiu do registro e nada foi escrito.
    Foreign,
    /// Falhou deixando restos no jogo (causa); só a restauração resolve.
    Residue(String),
    /// O jogo fechou; o motor já descartou o registro.
    Gone,
}

/// Classifica o resultado de um lote do motor para o patch `id` (veja o topo de
/// `gt_core::engine`). `pending` são os `id` pendentes do motor **depois** da operação.
pub fn outcome_of(res: gt_core::Result<BatchReport>, id: &str, pending: &[String]) -> OpOutcome {
    match res {
        Ok(report) if report.restored.iter().any(|r| r == id) => OpOutcome::Done(report.pause),
        Ok(report) if report.foreign.iter().any(|f| f == id) => OpOutcome::Foreign,
        Ok(_) => OpOutcome::Refused(format!("nada foi alterado em {id}")),
        Err(gt_core::Error::ProcessGone { .. }) => OpOutcome::Gone,
        Err(e) if pending.iter().any(|p| p == id) => OpOutcome::Residue(e.to_string()),
        Err(e) => OpOutcome::Refused(e.to_string()),
    }
}

/// Classifica o resultado de `restore_pending` do motor. `pending` são os `id` pendentes
/// depois da operação: entradas fora da política continuam lá mesmo com `Ok`.
pub fn restore_outcome(res: gt_core::Result<RestoreReport>, pending: &[String]) -> OpOutcome {
    match res {
        // Bytes de outro: o motor não escreveu nada e tirou a entrada do diário. Não é
        // "restaurado" (AGENTS.md 3; spec 6.11).
        Ok(report) if !report.foreign.is_empty() => OpOutcome::Foreign,
        Ok(_) if pending.is_empty() => OpOutcome::Done(Duration::ZERO),
        Ok(_) => OpOutcome::Residue(format!(
            "não foi possível restaurar {} agora",
            pending.join(", ")
        )),
        Err(gt_core::Error::ProcessGone { .. }) => OpOutcome::Gone,
        Err(e) if !pending.is_empty() => OpOutcome::Residue(e.to_string()),
        Err(e) => OpOutcome::Refused(e.to_string()),
    }
}

/// Trabalho da tarefa de reflexão (spec 005, 6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobKind {
    /// Índice das classes e campos, e o catálogo dos cheats de valor.
    Index,
    /// Procura do controller do jogador.
    Controller,
}

/// Um trabalho pronto para rodar na thread da tarefa de reflexão. Ele captura cópias do que
/// precisa (leitor, globais, catálogo): nada da sessão é emprestado.
pub struct ReflectionJob {
    pub kind: JobKind,
    pub run: Box<dyn FnOnce() -> ReflectionOutcome + Send>,
}

/// Resultado de um trabalho, que volta por canal ao laço principal.
pub struct ReflectionOutcome {
    pub kind: JobKind,
    /// Quanto o trabalho levou (o intervalo da tarefa é `max(10 s, 2 × took)`).
    pub took: Duration,
    /// Dados próprios da implementação, que só a sessão que criou o trabalho entende.
    pub data: Box<dyn Any + Send>,
}

/// O que um resultado da tarefa de reflexão mudou na sessão.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReflectionNews {
    /// A sessão tem um índice bom (depois deste resultado).
    pub index_ok: bool,
    /// O motivo de agora pede nova tentativa no intervalo da tarefa.
    pub retry: bool,
    /// A procura do controller mudou o estado do personagem.
    pub player_changed: Option<PlayerState>,
}

/// Relatório de um ciclo sem manutenção (sem cheats de valor, sem motor ou com o motor parado).
pub fn idle_tick() -> TickReport {
    TickReport {
        player: PlayerState::Absent(Absent::NoController),
        notes: HashMap::new(),
        events: Vec::new(),
        pawn_null_since: None,
        pawn: None,
        world: None,
    }
}

/// Sessão com um alvo conectado: o monitor de fechamento e, se houver, o motor.
///
/// A trava do diário é um `Arc<JournalLock>` compartilhado com o backend; a implementação
/// declara o motor **antes** do `Arc`, para o motor cair primeiro e a trava só soltar quando
/// a última cópia cair.
pub trait Session<P> {
    /// Se o motor está aberto.
    fn has_engine(&self) -> bool;
    /// Abre o motor se ainda não houver e o disco da busca guardada estiver `Ok`
    /// (sem disco, `Err(causa)`). Chamado quando o diário fica resolvido.
    fn open_engine(&mut self) -> Result<(), TuiError>;
    /// Junta uma nova busca: confere a identidade (diferente → `false`, nada muda), guarda
    /// `disk` e `version_key` e atualiza endereços e definições só dos `id` em `s.ids`
    /// (com `None`, todos), sem tocar nos ativos.
    fn merge_search(&mut self, s: Search<P>) -> bool;
    /// O jogo fechou (o `pidfd` ficou legível ou a identidade mudou).
    fn exited(&self) -> bool;
    /// Liga `(id, k)` num único lote (`k` é ignorado em cheat sem valor).
    fn apply(&mut self, items: &[(String, u64)]) -> OpOutcome;
    /// Desliga um cheat.
    fn undo(&mut self, id: &str) -> OpOutcome;
    /// Troca o valor de um cheat ligado.
    fn set_value(&mut self, id: &str, k: u64) -> OpOutcome;
    /// Restaura os restos de uma falha.
    fn restore_pending(&mut self) -> OpOutcome;
    /// Tenta de novo o SIGCONT que falhou.
    fn retry_resume(&mut self) -> OpOutcome;
    /// O SIGCONT falhou e o jogo pode ter ficado parado por nós.
    fn is_stuck(&self) -> bool;
    /// `id` das entradas pendentes (restos).
    fn pending_ids(&self) -> Vec<String>;
    /// Se o jogo fechou, descarta o registro sem escrever.
    fn discard_gone(&mut self);
    /// Desfaz tudo; devolve (restaurados, alterados por outro). Os bits dos cheats de valor
    /// também voltam (o `undo_all` do motor os restaura).
    fn undo_all(&mut self) -> (Vec<String>, Vec<String>);

    // ---- cheats de valor (spec 005); o padrão é "sem cheats de valor" ----

    /// O trabalho `kind` da tarefa de reflexão, ou `None` se não há o que fazer (globais
    /// indisponíveis, índice que ainda não chegou para procurar o controller...).
    fn reflection_job(&self, _kind: JobKind) -> Option<ReflectionJob> {
        None
    }

    /// Junta o resultado de um trabalho (o controlador já descartou os de outra conexão).
    fn merge_reflection(&mut self, _out: ReflectionOutcome) -> ReflectionNews {
        ReflectionNews::default()
    }

    /// Disponibilidade de cada cheat de valor: disco, globais e conferência barata da busca, e
    /// o índice da reflexão (`Checking` até o primeiro índice).
    fn field_avail(&self) -> Vec<(String, Avail)> {
        Vec::new()
    }

    /// O controller tem de ser procurado: não há um válido, ou o `Pawn` dele está nulo há mais
    /// de 30 s.
    fn needs_controller(&self, _now: Instant) -> bool {
        false
    }

    /// Liga um cheat de valor: a trava é aceita sempre; os bits escrevem já se o personagem
    /// estiver presente. `k` é o ponto da grade do valor, que vira o desejado (ignorado sem
    /// `[field.value]`).
    fn field_on(&mut self, id: &str, _k: u64) -> OpOutcome {
        OpOutcome::Refused(format!("{id} não é um cheat de valor"))
    }

    /// Executa a ação de somar (spec 006, 6.6) com a quantidade do ponto `k` da grade. Sem motor
    /// ou com o motor parado, recusa com a mesma mensagem do ligar; um controller ausente
    /// (`NeedsController`) também marca o pedido de procura.
    fn field_action(&mut self, id: &str, _k: u64) -> ActionOutcome {
        ActionOutcome::Refused(format!("{id} não é uma ação"))
    }

    /// Uma ação pediu a procura do controller e ela ainda não terminou.
    fn action_wants_controller(&self) -> bool {
        false
    }

    /// Resultado de uma procura pedida pela ação, entregue uma vez: `Some(true)` achou o
    /// personagem, `Some(false)` não achou.
    fn take_controller_news(&mut self) -> Option<bool> {
        None
    }

    /// Desliga um cheat de valor: a trava só para; os bits voltam ao original.
    fn field_off(&mut self, id: &str) -> OpOutcome {
        OpOutcome::Refused(format!("{id} não é um cheat de valor"))
    }

    /// Manutenção de ~10 Hz dos cheats de valor ligados (`on`).
    fn field_tick(&mut self, _on: &[String], _now: Instant) -> TickReport {
        idle_tick()
    }
}

/// O alvo: achar o processo, recuperar o diário, buscar o catálogo e abrir a sessão.
pub trait Backend {
    /// Dados próprios da busca que a sessão precisa guardar.
    type Payload: Send + 'static;
    /// Sessão com o alvo conectado.
    type Session: Session<Self::Payload>;

    /// Varre os processos atrás do jogo.
    fn find(&mut self) -> Find;
    /// A função de busca, para rodar na thread de busca.
    fn search_fn(&self) -> SearchFn<Self::Payload>;
    /// Recupera o diário (a trava já está com o backend).
    fn recover(&mut self) -> Recovered;
    /// Abre o `pidfd` de monitoramento (conferido com a identidade da busca) e, com
    /// `engine == true` e o disco `Ok`, o motor.
    fn open(
        &mut self,
        search: Search<Self::Payload>,
        engine: bool,
    ) -> Result<Self::Session, TuiError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use gt_core::Error;

    fn pending(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn recovery_outcome_texts() {
        // Um caso por campo do relatório.
        let cases: Vec<(RecoveryReport, &str)> = vec![
            (
                RecoveryReport {
                    discarded: Some(DiscardReason::ProcessGone),
                    ..Default::default()
                },
                "diário descartado: o jogo fechou",
            ),
            (
                RecoveryReport {
                    discarded: Some(DiscardReason::OtherProcess),
                    ..Default::default()
                },
                "diário descartado: o jogo reiniciou",
            ),
            (
                RecoveryReport {
                    discarded: Some(DiscardReason::OtherBoot),
                    ..Default::default()
                },
                "diário descartado: o sistema reiniciou",
            ),
            (
                RecoveryReport {
                    resumed: true,
                    ..Default::default()
                },
                "retomado o jogo parado",
            ),
            (
                RecoveryReport {
                    stopped_by_other: true,
                    ..Default::default()
                },
                "o jogo estava parado por outro processo; não foi retomado",
            ),
            (
                RecoveryReport {
                    restored: vec!["a".into()],
                    ..Default::default()
                },
                "restaurado: a",
            ),
            (
                RecoveryReport {
                    dropped: vec!["b".into()],
                    ..Default::default()
                },
                "descartado: b (bytes já originais)",
            ),
            (
                RecoveryReport {
                    foreign: vec!["c".into()],
                    ..Default::default()
                },
                "não restaurado: c (bytes alterados por outro)",
            ),
        ];
        for (report, line) in cases {
            assert_eq!(
                recovery_outcome(Ok(report)),
                Recovered::Clean {
                    lines: vec![line.to_string()],
                    corrupt: None
                }
            );
        }

        // Nada a recuperar.
        assert_eq!(
            recovery_outcome(Ok(RecoveryReport::default())),
            Recovered::Clean {
                lines: vec![],
                corrupt: None
            }
        );

        // Diário corrompido: o caminho segue para o aviso fixo.
        let path = PathBuf::from("/x/journal.json.bad");
        assert_eq!(
            recovery_outcome(Ok(RecoveryReport {
                corrupt: Some(path.clone()),
                ..Default::default()
            })),
            Recovered::Clean {
                lines: vec![],
                corrupt: Some(path)
            }
        );

        // `kept`: bloqueado, com as linhas do que foi resolvido.
        assert_eq!(
            recovery_outcome(Ok(RecoveryReport {
                restored: vec!["a".into()],
                kept: vec!["b".into(), "c".into()],
                ..Default::default()
            })),
            Recovered::Blocked {
                lines: vec!["restaurado: a".into()],
                text: "não foi possível restaurar b, c agora; aperte r para tentar de novo".into()
            }
        );

        // Erro.
        assert_eq!(
            recovery_outcome(Err(Error::PauseTimeout { attempts: 3 })),
            Recovered::Blocked {
                lines: vec![],
                text: "a recuperação do diário falhou: o jogo não pausou no prazo depois de 3 \
                       tentativas; aperte r para tentar de novo"
                    .into()
            }
        );
    }

    #[test]
    fn outcome_of_cases() {
        let ms = Duration::from_millis(3);
        let done = BatchReport {
            pause: ms,
            restored: vec!["a".into()],
            ..Default::default()
        };
        assert_eq!(outcome_of(Ok(done), "a", &[]), OpOutcome::Done(ms));

        let foreign = BatchReport {
            foreign: vec!["a".into()],
            ..Default::default()
        };
        assert_eq!(outcome_of(Ok(foreign), "a", &[]), OpOutcome::Foreign);

        // Recusas limpas: o `id` não está entre os pendentes.
        for err in [
            Error::ThreadsInPatch {
                id: "a".into(),
                attempts: 10,
            },
            Error::NotActive { id: "a".into() },
            Error::BoundariesChanged { id: "a".into() },
        ] {
            let text = err.to_string();
            assert_eq!(outcome_of(Err(err), "a", &[]), OpOutcome::Refused(text));
        }

        // Restos: o `id` ficou pendente.
        let err = Error::WriteRefused { addr: 0x10 };
        let text = err.to_string();
        assert_eq!(
            outcome_of(Err(err), "a", &pending(&["a"])),
            OpOutcome::Residue(text)
        );
        // Pendente de outro `id` não é resto deste.
        assert!(matches!(
            outcome_of(
                Err(Error::WriteRefused { addr: 0x10 }),
                "a",
                &pending(&["b"])
            ),
            OpOutcome::Refused(_)
        ));

        assert_eq!(
            outcome_of(Err(Error::ProcessGone { pid: 1 }), "a", &pending(&["a"])),
            OpOutcome::Gone
        );

        // Sucesso que não tocou o `id`.
        assert!(matches!(
            outcome_of(Ok(BatchReport::default()), "a", &[]),
            OpOutcome::Refused(_)
        ));
    }

    #[test]
    fn restore_outcome_cases() {
        let ok = || Ok(RestoreReport::default());
        assert_eq!(restore_outcome(ok(), &[]), OpOutcome::Done(Duration::ZERO));
        assert!(matches!(
            restore_outcome(ok(), &pending(&["a"])),
            OpOutcome::Residue(_)
        ));
        // Bytes de outro: nada foi escrito, então não é `Done`.
        let foreign = || {
            Ok(RestoreReport {
                foreign: vec!["a".to_string()],
                ..RestoreReport::default()
            })
        };
        assert_eq!(restore_outcome(foreign(), &[]), OpOutcome::Foreign);
        assert_eq!(
            restore_outcome(Err(Error::ProcessGone { pid: 1 }), &pending(&["a"])),
            OpOutcome::Gone
        );
        assert!(matches!(
            restore_outcome(Err(Error::PauseTimeout { attempts: 1 }), &pending(&["a"])),
            OpOutcome::Residue(_)
        ));
        assert!(matches!(
            restore_outcome(Err(Error::PauseTimeout { attempts: 1 }), &[]),
            OpOutcome::Refused(_)
        ));
    }
}
