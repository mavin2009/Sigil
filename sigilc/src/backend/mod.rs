//! Backend: code generation to Rust.
pub mod codegen;
pub mod graph;

pub use graph::{to_dot, to_mermaid};
pub use codegen::{emit, emit_cargo_toml_with_deps, emit_cargo_toml, emit_demo_main, relative_sigil_rt_path};
