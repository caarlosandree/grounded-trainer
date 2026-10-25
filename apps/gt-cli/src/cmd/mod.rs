//! Subcomandos do `gt`.

pub mod disasm;
pub mod dump;
pub mod inspect;
pub mod read;
pub mod scan;

/// Opções globais (`--pid`, `--module`, `--json`).
#[derive(Debug, Clone)]
pub struct Global {
    pub pid: Option<i32>,
    pub module: String,
    pub json: bool,
}
