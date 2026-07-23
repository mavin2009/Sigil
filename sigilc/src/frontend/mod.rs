
//! Frontend: parsing and AST construction.
pub mod ast;

pub use ast::{parse, Program, Schema, Process, Expr, Stmt, Tag, Span, BinOp, Literal, Type};
