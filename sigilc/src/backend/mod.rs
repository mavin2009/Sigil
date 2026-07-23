//! Backend: code generation to Rust.
pub mod codegen;

pub use codegen::{emit, emit_cargo_toml, relative_sigil_rt_path};
