//! Ferramentas de pesquisa: inspect, read, scan, disasm, diff, dump, fling, ue.
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
    /// Busca um padrão AOB (`48 8B ?? 05`) nas seções do módulo e diz se é único.
    Scan {
        /// Padrão: bytes em hexadecimal separados por espaço; `??` ou `?` é curinga.
        pattern: String,
        /// Só esta seção (ex.: `.text`); o padrão são as seções executáveis.
        #[arg(long, value_name = "NOME", conflicts_with = "all")]
        section: Option<String>,
        /// Todas as seções do módulo.
        #[arg(long)]
        all: bool,
        /// Busca no arquivo em disco (confere a versão), não na memória.
        #[arg(long)]
        disk: bool,
        /// Máximo de ocorrências exibidas (o total é sempre contado por inteiro).
        #[arg(long, value_name = "N", default_value = cmd::scan::default_limit())]
        limit: String,
    },
    /// Desmonta instruções x86-64 (sintaxe Intel) a partir de um endereço.
    Disasm {
        /// Endereço: `0x1456FC11C`, `modulo+56FC11C` ou `+56FC11C` (hexadecimal).
        address: String,
        /// Quantidade de instruções (decimal, ou hexadecimal com `0x`; máximo 10000).
        #[arg(short = 'n', long = "count", value_name = "N", default_value = cmd::disasm::default_count())]
        count: String,
    },
    /// Compara o código na memória com o executável em disco (acha patches de outros trainers).
    Diff {
        /// Só esta seção (ex.: `.rdata`); o padrão são as seções executáveis.
        #[arg(long, value_name = "NOME")]
        section: Option<String>,
    },
    /// Lê os pontos do `symbols.ini` do FLiNG e mostra o estado de cada um.
    Fling {
        /// Biblioteca da Steam (`steamapps`); padrão `$HOME/.local/share/Steam/steamapps`.
        #[arg(long, value_name = "DIR")]
        steam_library: Option<std::path::PathBuf>,
        /// Usa este `symbols.ini` em vez de procurar pela versão do jogo (ignora --steam-library).
        #[arg(long, value_name = "ARQUIVO")]
        ini: Option<std::path::PathBuf>,
    },
    /// Salva memória em arquivo: uma seção, ou um endereço e um tamanho.
    Dump {
        /// Nome de uma seção (ex.: `.text`) ou, com TAMANHO, o endereço inicial.
        target: String,
        /// Tamanho em bytes (decimal, ou hexadecimal com `0x`; máximo 1 GiB).
        size: Option<String>,
        /// Arquivo `.bin` de saída; o `.json` fica ao lado, com o mesmo nome.
        #[arg(short, long, value_name = "ARQUIVO")]
        output: Option<std::path::PathBuf>,
        /// Sobrescreve arquivos que já existem.
        #[arg(long)]
        force: bool,
    },
    /// Lê a reflexão do Unreal: campos de uma classe e o personagem do jogador.
    Ue {
        #[command(subcommand)]
        command: cmd::ue::UeCommand,
    },
}

/// Erro do núcleo dentro de `err`, direto ou embrulhado em `gt_grounded2::Error`.
fn core_error(err: &anyhow::Error) -> Option<&gt_core::Error> {
    if let Some(core) = err.downcast_ref::<gt_core::Error>() {
        return Some(core);
    }
    match err.downcast_ref::<gt_grounded2::Error>()? {
        gt_grounded2::Error::Core(core) | gt_grounded2::Error::DiskOpen { source: core, .. } => {
            Some(core)
        }
        _ => None,
    }
}

/// Sugestão de ação para o erro, quando há uma.
fn hint(err: &anyhow::Error) -> Option<String> {
    use gt_core::Error;
    match core_error(err)? {
        Error::PermissionDenied { ptrace_scope, .. } => {
            Some(gt_grounded2::resolve::permission_hint(
                "gt",
                gt_core::process::ptrace_scope().or(*ptrace_scope),
            ))
        }
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
        Command::Scan {
            pattern,
            section,
            all,
            disk,
            limit,
        } => cmd::scan::run(
            &global,
            &cmd::scan::Options {
                pattern: &pattern,
                section: section.as_deref(),
                all,
                disk,
                limit: Some(&limit),
            },
        ),
        Command::Disasm { address, count } => cmd::disasm::run(&global, &address, Some(&count)),
        Command::Diff { section } => cmd::diff::run(
            &global,
            &cmd::diff::Options {
                section: section.as_deref(),
            },
        ),
        Command::Fling { steam_library, ini } => cmd::fling::run(
            &global,
            &cmd::fling::Options {
                steam_library: steam_library.as_deref(),
                ini: ini.as_deref(),
            },
        ),
        Command::Dump {
            target,
            size,
            output,
            force,
        } => cmd::dump::run(
            &global,
            &cmd::dump::Options {
                target: &target,
                size: size.as_deref(),
                output: output.as_deref(),
                force,
            },
        ),
        Command::Ue { command } => cmd::ue::run(&global, &command),
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
    fn hint_finds_core_error_inside_grounded2_error() {
        let err = anyhow::Error::from(gt_grounded2::Error::from(gt_core::Error::ProcessGone {
            pid: 1,
        }));
        let hint = hint(&err).expect("deveria haver dica");
        assert!(hint.contains("o processo terminou"), "{hint}");
    }

    #[test]
    fn hint_finds_core_error_inside_disk_open() {
        let err = anyhow::Error::from(gt_grounded2::Error::DiskOpen {
            path: "/x".into(),
            source: gt_core::Error::ProcessGone { pid: 1 },
        });
        assert!(hint(&err).is_some());
    }

    #[test]
    fn hint_is_none_for_errors_without_a_suggestion() {
        assert!(hint(&anyhow::Error::from(gt_grounded2::Error::GameNotFound)).is_none());
    }
}
