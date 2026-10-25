//! Núcleo genérico: processos, memória, PE, busca AOB, assembly e patches.

pub mod addr;
pub mod asm;
pub mod diff;
#[cfg(feature = "write")]
pub mod engine;
pub mod error;
pub mod guard;
pub mod journal;
pub mod maps;
pub mod mem;
pub mod memscan;
pub mod modules;
pub mod patch;
pub mod pe;
pub mod process;
pub mod procinfo;
#[cfg(feature = "write")]
pub mod recovery;
pub mod scan;
#[cfg(feature = "write")]
pub mod session;
pub mod suspend;

pub use error::{Error, Result};
