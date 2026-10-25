//! Processos: valor de `ptrace_scope`, listagem e `cmdline`.

use std::fs;

use nix::errno::Errno;

use crate::error::Result;

/// Valor atual de `kernel.yama.ptrace_scope`, quando conhecido.
pub fn ptrace_scope() -> Option<u8> {
    fs::read_to_string("/proc/sys/kernel/yama/ptrace_scope")
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Separa o `cmdline` de um processo em argumentos.
///
/// Os argumentos vêm separados por NUL. Vazios do fim são descartados; vazios do meio são mantidos.
/// Bytes que não são UTF-8 viram `U+FFFD`.
pub fn parse_cmdline(raw: &[u8]) -> Vec<String> {
    let mut parts: Vec<&[u8]> = raw.split(|&b| b == 0).collect();
    while parts.last().is_some_and(|p| p.is_empty()) {
        parts.pop();
    }
    parts
        .into_iter()
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect()
}

/// Lista os processos com seus argumentos (`/proc/<pid>/cmdline`).
///
/// Processos que terminam durante a varredura são ignorados.
pub fn list_processes() -> Result<Vec<(i32, Vec<String>)>> {
    let mut out = Vec::new();
    for entry in fs::read_dir("/proc")? {
        let name = entry?.file_name();
        let Some(pid) = name
            .to_str()
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|s| s.parse::<i32>().ok())
        else {
            continue;
        };
        match fs::read(format!("/proc/{pid}/cmdline")) {
            Ok(raw) => out.push((pid, parse_cmdline(&raw))),
            Err(e) if is_gone(&e) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(out)
}

/// `true` quando o erro indica que o processo já não existe.
fn is_gone(e: &std::io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(code) if code == Errno::ENOENT as i32 || code == Errno::ESRCH as i32
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cmdline_separa_por_nul_e_descarta_vazios_do_fim() {
        assert_eq!(parse_cmdline(b"a\0b\0\0\0"), ["a", "b"]);
    }

    #[test]
    fn parse_cmdline_mantem_vazios_do_meio() {
        assert_eq!(parse_cmdline(b"a\0\0b"), ["a", "", "b"]);
    }

    #[test]
    fn parse_cmdline_vazio_e_lossy() {
        assert!(parse_cmdline(b"").is_empty());
        assert_eq!(parse_cmdline(b"\xff"), ["\u{FFFD}"]);
    }

    #[test]
    fn list_processes_inclui_o_proprio_processo() {
        let procs = list_processes().unwrap();
        let me = i32::try_from(std::process::id()).unwrap();
        let (_, args) = procs
            .iter()
            .find(|(pid, _)| *pid == me)
            .expect("processo de teste ausente");
        assert!(!args.is_empty());
    }

    #[test]
    fn ptrace_scope_reflete_o_arquivo_do_kernel() {
        let esperado = std::fs::read_to_string("/proc/sys/kernel/yama/ptrace_scope")
            .ok()
            .and_then(|s| s.trim().parse::<u8>().ok());
        assert_eq!(ptrace_scope(), esperado);
    }
}
