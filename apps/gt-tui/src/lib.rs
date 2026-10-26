//! Interface do jogador no terminal (biblioteca do `gt-tui`).

pub mod backend;
pub mod game;
pub mod keys;
pub mod model;
#[cfg(feature = "test-backend")]
pub mod raw;
pub mod ui;
