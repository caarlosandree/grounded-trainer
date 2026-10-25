//! Ferramentas de pesquisa: inspect, read, scan, disasm, diff, dump, fling.
//!
//! O `gt` só lê a memória do processo: nunca escreve (spec 6.11).

mod cmd;
mod context;
mod output;

use std::io::Write;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use cmd::Global;

/// Ferramentas de pesquisa para o Grounded 2 (somente leitura).
#[derive(Debug, Parser)]
#[command(name = "gt", version, about)]
struct Cli {
    /// Processo alvo; sem isto, procura o Grounded 2 em /proc.
    #[arg(long, global = true, value_name = "PID", value_parser = clap::value_parser!(i32).range(1..))]
    pid: Option<i32>,

    /// Módulo usado nos endereços `+offset` e nos comandos que operam no executável.
    #[arg(long, global = true, value_name = "NOME", default_value = gt_grounded2::GAME_EXE)]
    module: String,

    /// Saída em JSON (um único objeto por execução).
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Mostra o processo, o módulo, a versão do build e as seções.
    Inspect,
    /// Hexdump da memória: 16 bytes por linha, `??` nas páginas ilegíveis.
    Read {
        /// Endereço: `0x1456FC11C`, `modulo+56FC11C` ou `+56FC11C` (hexadecimal).
        address: String,
        /// Tamanho em bytes (decimal, ou hexadecimal com `0x`; padrão 64; máximo 1 MiB).
        size: Option<String>,
    },
}

/// Texto de ajuda para o erro de permissão (spec 6.10). Não sugere `sudo` como primeira opção.
fn permission_hint(ptrace_scope: Option<u8>) -> String {
    match ptrace_scope {
        Some(0) => "kernel.yama.ptrace_scope está em 0, que já permite a leitura; confira se o \
                    processo é de outro usuário ou se o SELinux/um container bloqueia o acesso."
            .to_string(),
        Some(value) => format!(
            "kernel.yama.ptrace_scope está em {value}, o que restringe a leitura da memória de \
             outro processo (com 1, só descendentes do processo que lê). Confira se o gt e o jogo \
             são do mesmo usuário; se aceitar mudar a política, baixe o valor com `sudo sysctl \
             kernel.yama.ptrace_scope=0` (afeta o sistema todo; o gt nunca altera isso sozinho)."
        ),
        None => "o arquivo kernel.yama.ptrace_scope não existe (Yama ausente); a recusa vem de \
                 outra política, como SELinux, um container ou um processo de outro usuário."
            .to_string(),
    }
}

/// Erro de vários candidatos ao processo do jogo.
fn many_candidates_message(pids: &[i32]) -> String {
    let list: Vec<String> = pids.iter().map(i32::to_string).collect();
    format!(
        "mais de um processo do jogo encontrado (PIDs: {}); escolha um com --pid <PID>",
        list.join(", ")
    )
}

/// Sugestão de ação para o erro, quando há uma.
fn hint(err: &anyhow::Error) -> Option<String> {
    use gt_core::Error;
    match err.downcast_ref::<Error>()? {
        Error::PermissionDenied { ptrace_scope, .. } => Some(permission_hint(
            gt_core::process::ptrace_scope().or(*ptrace_scope),
        )),
        Error::ProcessGone { .. } => {
            Some("o processo terminou; abra o jogo de novo e tente outra vez.".to_string())
        }
        Error::Unmapped { .. } => Some(
            "confira o endereço e o módulo; `gt inspect` mostra a base e as seções do executável."
                .to_string(),
        ),
        Error::ThirdPartyCode { .. } => Some(
            "o gt não lê código de outros trainers; dados comuns (sem permissão de execução) \
             continuam liberados."
                .to_string(),
        ),
        Error::ModuleNotFound { .. } => Some(
            "use --module com o nome de um módulo carregado ou --pid para escolher o processo."
                .to_string(),
        ),
        Error::NotPe { .. } => Some(
            "esta operação precisa de uma imagem PE; endereços absolutos funcionam em qualquer \
             processo."
                .to_string(),
        ),
        _ => None,
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    let global = Global {
        pid: cli.pid,
        module: cli.module,
        json: cli.json,
    };
    match cli.command {
        Command::Inspect => cmd::inspect::run(&global),
        Command::Read { address, size } => cmd::read::run(&global, &address, size.as_deref()),
    }
}

/// Escreve o erro, a cadeia de causas e a sugestão em stderr.
fn report(err: &anyhow::Error) {
    let mut out = std::io::stderr().lock();
    let _ = writeln!(out, "erro: {err}");
    for cause in err.chain().skip(1) {
        let _ = writeln!(out, "  causa: {cause}");
    }
    if let Some(hint) = hint(err) {
        let _ = writeln!(out, "dica: {hint}");
    }
}

fn main() -> ExitCode {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            report(&err);
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_hint_mentions_scope_and_value_without_sudo_first() {
        let hint = permission_hint(Some(1));
        assert!(hint.contains("ptrace_scope"));
        assert!(hint.contains('1'));
        assert!(!hint.trim_start().starts_with("sudo"));
        let sudo = hint.find("sudo");
        let scope = hint.find("ptrace_scope").unwrap();
        assert!(sudo.is_none_or(|s| s > scope), "{hint}");
    }

    #[test]
    fn permission_hint_without_yama_says_so() {
        assert!(permission_hint(None).contains("Yama ausente"));
    }

    #[test]
    fn many_candidates_lists_pids_and_mentions_pid_flag() {
        let msg = many_candidates_message(&[10, 20]);
        assert!(msg.contains("10"));
        assert!(msg.contains("20"));
        assert!(msg.contains("--pid"));
    }
}
