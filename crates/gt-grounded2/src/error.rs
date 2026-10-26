//! Erros da resolução do alvo e das operações específicas do Grounded 2.

use std::path::PathBuf;

/// Erro do `gt-grounded2`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Nenhum processo do jogo em `/proc`.
    #[error("jogo não encontrado; abra o Grounded 2 e tente de novo")]
    GameNotFound,

    /// Mais de um processo do jogo em `/proc`.
    #[error(
        "mais de um processo do jogo encontrado (PIDs: {}); escolha um com --pid <PID>",
        pids_list(.pids)
    )]
    ManyCandidates { pids: Vec<i32> },

    /// A imagem do módulo não pôde ser aberta no disco.
    #[error("não foi possível abrir {} no disco", .path.display())]
    DiskOpen {
        path: PathBuf,
        #[source]
        source: gt_core::Error,
    },

    /// Arquivo de assinaturas inválido (`signatures/*.toml`).
    #[error("assinatura inválida em {file}, campo {field}: {reason}")]
    Signature {
        file: String,
        field: String,
        reason: String,
    },

    /// `id` pedido que não existe no catálogo.
    #[error("patch desconhecido: {id}")]
    UnknownPatch { id: String },

    /// `id` de um cheat de valor (`[[field]]`) pedido onde só vale patch.
    #[error("{id} é um cheat de valor; use o gt-tui")]
    FieldCheat { id: String },

    /// `id` pedido cujo estado não permite aplicar.
    #[error("patch {id} indisponível: {state}")]
    Unavailable { id: String, state: String },

    /// Erro do núcleo; a mensagem é a do próprio erro.
    #[error(transparent)]
    Core(#[from] gt_core::Error),
}

/// Resultado com o erro do `gt-grounded2`.
pub type Result<T> = std::result::Result<T, Error>;

fn pids_list(pids: &[i32]) -> String {
    let list: Vec<String> = pids.iter().map(i32::to_string).collect();
    list.join(", ")
}
