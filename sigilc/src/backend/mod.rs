//! Backend: code generation to Rust.
pub mod codegen;
pub mod graph;

pub use codegen::{
    emit, emit_cargo_toml, emit_cargo_toml_with_deps, emit_demo_main, relative_sigil_rt_path,
};
pub use graph::{to_dot, to_mermaid};
