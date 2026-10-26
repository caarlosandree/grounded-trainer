//! Erros do leitor de reflexão.

/// Erro de qualquer operação do `gt-ue`.
#[derive(Debug, thiserror::Error)]
pub enum UeError {
    /// Leitura em endereço inválido ou não mapeado.
    #[error("não foi possível ler a memória do jogo em {addr:#x}")]
    Read { addr: u64 },

    /// A estrutura lida não tem a forma esperada (ou estoura um teto de segurança).
    #[error("estrutura do Unreal malformada: {0}")]
    Malformed(String),

    /// Nada encontrado para o que foi pedido.
    #[error("não encontrado: {0}")]
    NotFound(String),

    /// Mais de um resultado para o que foi pedido.
    #[error("ambíguo: {0}")]
    Ambiguous(String),

    /// O teto de tempo da varredura estourou.
    #[error("o tempo máximo da varredura estourou")]
    Deadline,

    /// Erro do núcleo (processo morto, sem permissão, ...).
    #[error(transparent)]
    Core(#[from] gt_core::Error),
}
