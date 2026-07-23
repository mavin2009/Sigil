//! Minimal Sigil runtime support for generated code.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SigilError {
    #[error("timeout")]
    Timeout,
    #[error("transform error: {0}")]
    Transform(String),
    #[error("schema or validation failure")]
    Schema,
}

pub type Result<T> = std::result::Result<T, SigilError>;
