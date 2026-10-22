//! Semantic model and checker for Wild Horse projects (language spec §13).

pub mod check;
pub mod eval;
pub mod hash;
pub mod machine;
pub mod model;
#[doc(hidden)]
pub mod synthetic;
pub mod ty;

pub use check::{Env, Typer, assignable, compatible};
pub use model::*;
pub use ty::{IntTy, NodeId, Ty};
