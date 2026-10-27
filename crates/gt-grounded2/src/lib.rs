//! Conhecimento específico do Grounded 2: constantes do jogo e regra de casamento do processo.

pub mod catalog;
pub mod caves;
pub mod error;
#[cfg(feature = "write")]
pub mod fields;
pub mod fling;
#[cfg(feature = "write")]
pub mod patching;
pub mod player;
pub mod policy;
pub mod profile;
pub mod resolve;
pub mod signatures;
pub mod state;
pub mod target;
pub mod trust;
pub mod ue;

pub use error::{Error, Result};

/// Nome do executável do jogo, como aparece no `cmdline` (formato Windows).
pub const GAME_EXE: &str = "Grounded2Steam-Win64-Shipping.exe";

/// AppID da Steam do Grounded 2.
pub const APP_ID: u32 = 2661300;

/// Pasta do trainer FLiNG usada para localizar o `symbols.ini` (Tarefa 9).
pub const FLING_GAME_DIR: &str = "grounded-2-trainer";
