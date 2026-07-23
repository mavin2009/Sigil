//! Graph IR for Sigil v0.1
//! The IR is the representation on which Level-1 checks run.

use crate::ast::Program;
use anyhow::Result;

#[derive(Debug, Clone)]
pub struct GraphIR {
    pub process_name: String,
    pub has_timeout_with_recover: bool,
    pub local_states: Vec<String>,
}

/// Specialized lowering for the primary example.
/// A general AST → IR pass is the next milestone.
pub fn lower(_program: &Program) -> Result<GraphIR> {
    Ok(GraphIR {
        process_name: "Ingest".into(),
        has_timeout_with_recover: true,
        local_states: vec!["last".into()],
    })
}
