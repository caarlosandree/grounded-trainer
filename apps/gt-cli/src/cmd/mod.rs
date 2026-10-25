//! Subcomandos do `gt`.

pub mod inspect;
pub mod read;

/// Opções globais (`--pid`, `--module`, `--json`).
#[derive(Debug, Clone)]
pub struct Global {
    pub pid: Option<i32>,
    pub module: String,
    pub json: bool,
}
