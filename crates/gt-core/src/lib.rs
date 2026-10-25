//! Núcleo genérico: processos, memória, PE, busca AOB, assembly e patches.

pub mod addr;
pub mod error;
pub mod maps;
pub mod pe;
pub mod scan;

pub use error::{Error, Result};
