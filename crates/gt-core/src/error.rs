//! Erros comuns do núcleo.

use std::io;

/// Erro de qualquer operação do `gt-core`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// O processo não existe mais (saiu, ou é um zumbi sem mapas).
    #[error("o processo {pid} terminou")]
    ProcessGone { pid: i32 },

    /// O kernel recusou o acesso ao processo.
    #[error("sem permissão para ler o processo {pid}")]
    PermissionDenied {
        pid: i32,
        /// Valor de `kernel.yama.ptrace_scope`, quando conhecido.
        ptrace_scope: Option<u8>,
    },

    /// O intervalo pedido não está todo dentro de regiões mapeadas.
    #[error("endereço {addr:#x} fora de qualquer região mapeada")]
    Unmapped { addr: u64, len: usize },

    /// O endereço cai em código que não pertence ao jogo nem ao Wine.
    #[error(
        "código fora dos módulos do jogo/Wine (pode ser de outro trainer): bloqueado pela regra do projeto"
    )]
    ThirdPartyCode { addr: u64 },

    /// Nenhum módulo com esse nome foi encontrado no processo.
    #[error("módulo {name} não encontrado no processo")]
    ModuleNotFound { name: String },

    /// O módulo existe, mas não é uma imagem PE.
    #[error("o módulo {module} não é uma imagem PE")]
    NotPe { module: String },

    /// Cabeçalho PE malformado.
    #[error("cabeçalho PE inválido: {0}")]
    BadPe(String),

    /// Um token do padrão AOB não é hexadecimal nem curinga.
    #[error("token {index} inválido no padrão: {token}")]
    BadPattern { token: String, index: usize },

    /// O padrão AOB não tem nenhum byte fixo.
    #[error("padrão sem bytes fixos")]
    EmptyPattern,

    /// Endereço ou tamanho digitado pelo usuário não pôde ser lido.
    #[error("endereço ou tamanho inválido '{input}': {reason}")]
    BadAddress { input: String, reason: String },

    /// Linha de `/proc/<pid>/maps` fora do formato esperado.
    #[error("linha inválida no mapa de memória: {line}")]
    BadMaps { line: String },

    /// A versão do módulo na memória difere da versão no disco.
    #[error("versão do módulo na memória ({memory}) difere da do disco ({disk})")]
    VersionMismatch { memory: String, disk: String },

    /// Arquivo de `/proc` fora do formato esperado.
    #[error("formato inesperado em {file}: {reason}")]
    BadProc { file: String, reason: String },

    /// O alvo não parou no prazo (SIGSTOP não chegou a todas as threads).
    #[error("o jogo não pausou no prazo depois de {attempts} tentativas")]
    PauseTimeout { attempts: u32 },

    /// O patch não passou na validação (tamanho, instruções ou regra do `call`).
    #[error("patch {id} inválido: {reason}")]
    BadPatch { id: String, reason: String },

    /// Outro `gt-patch` já segura a trava do diário.
    #[error("outro gt-patch já está em execução")]
    AlreadyRunning,

    /// Falha do diário que não é de E/S comum (serialização, trava).
    #[error("diário: {0}")]
    Journal(String),

    /// Erro de entrada e saída do sistema operacional.
    #[error("erro de E/S: {0}")]
    Io(#[from] io::Error),
}

/// Resultado com o erro do `gt-core`.
pub type Result<T> = std::result::Result<T, Error>;
