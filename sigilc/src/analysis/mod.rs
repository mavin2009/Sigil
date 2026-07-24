//! Analysis: Graph IR lowering, Level-1/2 checks, types, residual risk.
pub mod check;
pub mod ir;
pub mod level2;
pub mod level3;
pub mod level4;
pub mod levels;
pub mod reference;
pub mod residual;
pub mod topology;
pub mod typecheck;
pub mod types;

pub use check::{
    check_failure_paths, check_handler_wellformedness, check_numeric_types,
    check_recover_signatures, check_transform_purity, check_transform_signatures,
    fallible_fallbacks, level1_check,
};
pub use ir::{lower, Edge, EffectSet, GraphIR, Node};
pub use level2::{check_budget_arithmetic, level2_check, Level2Report};
pub use level3::{input_preconditions, level3_prove, Level3Report};
pub use level4::{level4_prove, Level4Report};
pub use levels::{level_banner, run_checks, AssuranceLevel, CheckOutcome};
pub use reference::{
    interpret_handler, record as reference_record, ReferenceResult, ReferenceValue, TraceEvent,
};
pub use residual::residual_risk_report;
pub use topology::{derive_topology, Topology, TopologyEdge};
pub use typecheck::{check_effect_contracts, check_types};
pub use types::{infer_program, type_name, TransformTypes, TypeEnv};
