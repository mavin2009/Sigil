//! Frontend: parsing and AST construction.
pub mod ast;

pub use ast::{
    parse, BinOp, Expr, Literal, Process, Program, Schema, Span, Stmt, Tag, TransformDecl,
    SpecDecl, SpecItem, Type,
};
