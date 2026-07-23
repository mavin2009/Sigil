//! Analysis: Graph IR lowering, Level-1/2 checks, types, residual risk.
pub mod ir;
pub mod check;
pub mod residual;
pub mod types;
pub mod level2;
pub mod level3;
pub mod level4;
pub mod levels;
pub mod topology;

pub use ir::{lower, GraphIR, Node, Edge, EffectSet};
pub use levels::{level_banner, run_checks, AssuranceLevel, CheckOutcome};
pub use topology::{derive_topology, Topology, TopologyEdge};
pub use level3::{input_preconditions, level3_prove, Level3Report};
pub use level4::{level4_prove, Level4Report};
pub use check::{check_failure_paths, check_transform_purity, check_transform_signatures, level1_check};
pub use residual::residual_risk_report;
pub use types::{infer_program, type_name, TypeEnv, TransformTypes};
pub use level2::{level2_check, Level2Report};
