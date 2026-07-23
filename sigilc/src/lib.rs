//! Sigilc — compiler library for the Sigil language.
//!
//! Pipeline: parse → lower → level1_check → check_transform_signatures
//!         → level2_check → residual_risk_report → emit

pub mod analysis;
pub mod backend;
pub mod diagnostics;
pub mod frontend;

pub use analysis::{
    check_failure_paths, check_handler_wellformedness, check_numeric_types,
    check_recover_signatures, check_transform_purity, check_transform_signatures, derive_topology,
    fallible_fallbacks, input_preconditions, level1_check, level2_check, level3_prove,
    level4_prove, level_banner, lower, residual_risk_report, run_checks, AssuranceLevel,
    CheckOutcome, GraphIR, Level2Report, Level3Report, Level4Report, Topology,
};
pub use backend::{
    emit, emit_cargo_toml, emit_cargo_toml_with_deps, emit_demo_main, relative_sigil_rt_path,
    to_dot, to_mermaid,
};
pub use diagnostics::{line_col, render as render_diagnostic};
pub use frontend::{parse, Program, Span};
