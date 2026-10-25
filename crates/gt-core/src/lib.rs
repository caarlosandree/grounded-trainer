//! Núcleo genérico: processos, memória, PE, busca AOB, assembly e patches.

pub mod addr;
pub mod error;
pub mod guard;
pub mod maps;
pub mod mem;
pub mod modules;
pub mod pe;
pub mod process;
pub mod scan;

pub use error::{Error, Result};
