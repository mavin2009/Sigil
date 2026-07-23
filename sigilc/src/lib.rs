//! Sigilc — compiler library for the Sigil language.
//!
//! Pipeline: parse → lower → level1_check → check_transform_signatures
//!         → residual_risk_report → emit

pub mod frontend;
pub mod analysis;
pub mod backend;

pub use frontend::{parse, Program, Span};
pub use analysis::{
    check_transform_signatures, lower, level1_check, residual_risk_report, GraphIR,
};
pub use backend::{emit, emit_cargo_toml, emit_demo_main, relative_sigil_rt_path};
