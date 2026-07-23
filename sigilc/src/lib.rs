//! Sigilc — compiler library for the Sigil language.
//!
//! Pipeline: parse → lower → level1_check → check_transform_signatures
//!         → level2_check → residual_risk_report → emit

pub mod frontend;
pub mod analysis;
pub mod backend;

pub use frontend::{parse, Program, Span};
pub use analysis::{
    input_preconditions, level3_prove, Level3Report,
    check_failure_paths, check_transform_purity, check_transform_signatures, level2_check, level_banner, level1_check, lower, residual_risk_report,
    run_checks, AssuranceLevel, CheckOutcome, GraphIR, Level2Report, derive_topology, Topology,
};
pub use backend::{emit, emit_cargo_toml, emit_demo_main, relative_sigil_rt_path};
