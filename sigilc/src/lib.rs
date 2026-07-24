//! Sigilc — compiler library for the Sigil language.
//!
//! Pipeline: parse → lower → level1_check → check_transform_signatures
//!         → level2_check → residual_risk_report → emit

pub mod analysis;
pub mod backend;
pub mod diagnostics;
pub mod frontend;

pub use analysis::{
    check_budget_arithmetic, check_effect_contracts, check_failure_paths,
    check_handler_wellformedness, check_numeric_types, check_recover_signatures,
    check_transform_purity, check_transform_signatures, check_types, derive_topology,
    fallible_fallbacks, input_preconditions, interpret_handler, level1_check, level2_check,
    level3_prove, level4_prove, level_banner, lower, reference_record, residual_risk_report,
    run_checks, AssuranceLevel, CheckOutcome, GraphIR, Level2Report, Level3Report, Level4Report,
    ReferenceResult, ReferenceValue, Topology, TraceEvent,
};
pub use backend::{
    emit, emit_cargo_toml, emit_cargo_toml_with_deps, emit_demo_main, emit_effect_contracts,
    relative_sigil_rt_path, to_dot, to_mermaid, write_generated_crate, GeneratedCrate,
    DISTRIBUTED_PROTOCOL_VERSION, GENERATED_ABI_VERSION, RESIDUAL_RISK_SCHEMA_VERSION,
    ROUTING_HASH_VERSION,
};
pub use diagnostics::{line_col, render as render_diagnostic};
pub use frontend::{format_program, parse, Program, Span};
