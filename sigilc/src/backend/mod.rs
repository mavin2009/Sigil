//! Backend: code generation to Rust.
pub mod codegen;
pub mod graph;
pub mod output;

pub use codegen::{
    emit, emit_cargo_toml, emit_cargo_toml_with_deps, emit_demo_main, emit_effect_contracts,
    relative_sigil_rt_path, GENERATED_ABI_VERSION, RESIDUAL_RISK_SCHEMA_VERSION,
    ROUTING_HASH_VERSION,
};
pub use graph::{to_dot, to_mermaid};
pub use output::{write_generated_crate, GeneratedCrate};
