//! Backend: code generation to Rust.
pub mod codegen;

pub use codegen::{emit, emit_cargo_toml, emit_demo_main, relative_sigil_rt_path};
