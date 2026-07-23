
//! Analysis: Graph IR lowering and Level-1 checks.
pub mod ir;
pub mod check;

pub use ir::{lower, GraphIR, Node, Edge, EffectSet};
pub use check::level1_check;
