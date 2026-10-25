//! Núcleo genérico: processos, memória, PE, busca AOB, assembly e patches.

pub mod addr;
pub mod asm;
pub mod diff;
pub mod error;
pub mod guard;
pub mod maps;
pub mod mem;
pub mod memscan;
pub mod modules;
pub mod pe;
pub mod process;
pub mod procinfo;
pub mod scan;
pub mod suspend;

pub use error::{Error, Result};
