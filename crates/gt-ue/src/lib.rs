//! Leitor genérico da reflexão do Unreal Engine 5 (nomes, tabela de objetos, classes e campos).
//!
//! Nenhum offset do jogo vive aqui: o [`UeLayout`] vem do `[ue]` do `generic.toml` e os campos de
//! cada classe vêm da própria reflexão, por nome.

pub mod check;
pub mod error;
pub mod layout;
pub mod names;
pub mod objects;
pub mod read;
#[cfg(feature = "synth")]
pub mod synth;

pub use check::cheap_check;
pub use error::UeError;
pub use layout::{
    FBoolLayout, FFieldLayout, FPropLayout, ObjectsLayout, UObjectLayout, UStructLayout, UeLayout,
};
pub use names::{Names, decode_entry};
pub use objects::{ObjHeader, ObjectArray, ScanBudget, ScanEnd};

#[cfg(test)]
mod tests;
