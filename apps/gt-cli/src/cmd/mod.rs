//! Subcomandos do `gt`.

pub mod caves;
pub mod compare;
pub mod diff;
pub mod disasm;
pub mod dump;
pub mod fling;
pub mod inspect;
pub mod ours;
pub mod read;
pub mod scan;
pub mod ue;

/// Opções globais (`--pid`, `--module`, `--json`).
#[derive(Debug, Clone)]
pub struct Global {
    pub pid: Option<i32>,
    pub module: String,
    pub json: bool,
}
