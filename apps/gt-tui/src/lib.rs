//! Interface do jogador no terminal (biblioteca do `gt-tui`).

pub mod app;
pub mod backend;
pub mod controller;
pub mod exit;
#[cfg(test)]
pub(crate) mod fake;
pub mod game;
pub mod hooks;
pub mod keys;
pub mod logfile;
pub mod model;
#[cfg(feature = "test-backend")]
pub mod raw;
pub mod term;
pub mod ui;
pub mod values;
