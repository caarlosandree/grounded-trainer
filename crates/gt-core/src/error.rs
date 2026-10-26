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

    /// O patch cai fora dos intervalos em que a política permite escrever.
    #[error("patch {id} fora da área permitida para escrita ({addr:#x})")]
    OutsidePolicy { id: String, addr: u64 },

    /// O patch toca um intervalo já ativo (ou outro patch do mesmo lote).
    #[error("patch {id} se sobrepõe a um patch ativo")]
    Overlap { id: String },

    /// Os bytes na memória não são os esperados (hex com espaços).
    #[error(
        "bytes diferentes do esperado no patch {id}: esperados {expected}, encontrados {found}"
    )]
    BytesMismatch {
        id: String,
        expected: String,
        found: String,
    },

    /// Alguma thread continua parada num ponto perigoso do patch.
    #[error("threads do jogo não saem do trecho do patch {id} depois de {attempts} tentativas")]
    ThreadsInPatch { id: String, attempts: u32 },

    /// O kernel recusou a escrita num intervalo permitido pela política.
    #[error("o kernel recusou a escrita em {addr:#x}; confira proc_mem.force_override")]
    WriteRefused { addr: u64 },

    /// Não há patch ativo com esse `id` (troca de valor de um patch que não está ligado).
    #[error("o patch {id} não está ativo")]
    NotActive { id: String },

    /// Os bytes novos da troca de valor decodificam em outras fronteiras de instrução.
    #[error("os bytes novos do patch {id} mudam as fronteiras de instrução")]
    BoundariesChanged { id: String },

    /// Escrita em dados fora da política de dados (região `rw` fora dos módulos).
    #[error("escrita em dados fora da área permitida ({addr:#x})")]
    DataOutsidePolicy { addr: u64 },

    /// A conferência de identidade do objeto falhou imediatamente antes da escrita.
    #[error("o objeto em {addr:#x} mudou; nada foi escrito")]
    GuardMismatch { addr: u64 },

    /// Escrita em dados com um motor sem política de dados.
    #[error("o motor não tem política de escrita em dados")]
    NoDataPolicy,

    /// O cheat tem entrada de dados pendente (restos de uma falha) até a recuperação.
    #[error("o cheat {id} tem restos pendentes no jogo; use a recuperação (r)")]
    DataPending { id: String },

    /// Outro `gt-patch` já segura a trava do diário.
    #[error("outro gt-patch ou gt-tui já está em execução")]
    AlreadyRunning,

    /// Falha do diário: gravação (com o caminho), serialização ou trava.
    #[error("diário: {0}")]
    Journal(String),

    /// Erro de entrada e saída do sistema operacional.
    #[error("erro de E/S: {0}")]
    Io(#[from] io::Error),
}

/// Resultado com o erro do `gt-core`.
pub type Result<T> = std::result::Result<T, Error>;
