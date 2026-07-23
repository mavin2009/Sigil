//! AST for Sigil v0.1

use anyhow::{anyhow, Result};

#[derive(Debug, Clone)]
pub struct Program {
    pub schemas: Vec<Schema>,
    pub processes: Vec<Process>,
}

#[derive(Debug, Clone)]
pub struct Schema {
    pub name: String,
    pub fields: Vec<(String, Type)>,
}

#[derive(Debug, Clone)]
pub enum Type {
    String,
    Float,
    Named(String),
}

#[derive(Debug, Clone)]
pub struct Process {
    pub name: String,
    pub states: Vec<StateDecl>,
    pub handlers: Vec<OnHandler>,
}

#[derive(Debug, Clone)]
pub struct StateDecl {
    pub name: String,
    pub ty: Type,
    pub init: String,
}

#[derive(Debug, Clone)]
pub struct OnHandler {
    pub msg_name: String,
    pub msg_ty: String,
    pub steps: Vec<String>,
}

/// Specialized parser path for the primary example.
/// A general pest-based pair walker is the next milestone.
pub fn parse_example(source: &str) -> Result<Program> {
    if !source.contains("process Ingest") {
        return Err(anyhow!(
            "v0.1 currently supports the Ingest example. General parser coming next."
        ));
    }

    Ok(Program {
        schemas: vec![
            Schema {
                name: "Telemetry".into(),
                fields: vec![
                    ("id".into(), Type::String),
                    ("payload".into(), Type::String),
                ],
            },
            Schema {
                name: "Metrics".into(),
                fields: vec![
                    ("id".into(), Type::String),
                    ("value".into(), Type::Float),
                ],
            },
        ],
        processes: vec![Process {
            name: "Ingest".into(),
            states: vec![StateDecl {
                name: "last".into(),
                ty: Type::String,
                init: "00000000-0000-0000-0000-000000000000".into(),
            }],
            handlers: vec![OnHandler {
                msg_name: "packet".into(),
                msg_ty: "Telemetry".into(),
                steps: vec![
                    "validate".into(),
                    "decompress @timeout(50.ms) @recover(with: empty)".into(),
                    "extract".into(),
                    "store".into(),
                    "last := id".into(),
                ],
            }],
        }],
    })
}
