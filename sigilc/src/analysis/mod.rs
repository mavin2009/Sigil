
//! Analysis: Graph IR lowering, Level-1 checks, and residual risk.
pub mod ir;
pub mod check;
pub mod residual;
pub mod types;

pub use ir::{lower, GraphIR, Node, Edge, EffectSet};
pub use check::{level1_check, check_transform_signatures};
pub use residual::residual_risk_report;

pub use types::{infer_program, type_name, TypeEnv, TransformTypes};
