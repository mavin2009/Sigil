
//! Analysis: Graph IR lowering, Level-1 checks, and residual risk.
pub mod ir;
pub mod check;
pub mod residual;

pub use ir::{lower, GraphIR, Node, Edge, EffectSet};
pub use check::level1_check;
pub use residual::residual_risk_report;
