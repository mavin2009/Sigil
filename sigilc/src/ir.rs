
//! Graph IR for Sigil
//! Built from the structured AST so Level-1 checks have real information.

use crate::ast::{Program, Expr, Stmt, Tag};
use anyhow::Result;

#[derive(Debug, Clone)]
pub struct GraphIR {
    pub process_name: String,
    pub local_states: Vec<String>,
    pub has_timeout: bool,
    pub has_recover: bool,
    pub external_calls: Vec<String>,
}

pub fn lower(program: &Program) -> Result<GraphIR> {
    let mut ir = GraphIR {
        process_name: String::new(),
        local_states: vec![],
        has_timeout: false,
        has_recover: false,
        external_calls: vec![],
    };

    for proc in &program.processes {
        ir.process_name = proc.name.clone();
        for st in &proc.states {
            ir.local_states.push(st.name.clone());
        }
        for handler in &proc.handlers {
            for stmt in &handler.body {
                walk_stmt(stmt, &mut ir);
            }
        }
    }
    Ok(ir)
}

fn walk_stmt(stmt: &Stmt, ir: &mut GraphIR) {
    match stmt {
        Stmt::Let { expr, .. } | Stmt::Assign { expr, .. } | Stmt::Expr(expr) => {
            walk_expr(expr, ir);
        }
    }
}

fn walk_expr(expr: &Expr, ir: &mut GraphIR) {
    match expr {
        Expr::Pipeline { base, steps } => {
            walk_expr(base, ir);
            for step in steps {
                walk_expr(&step.expr, ir);
                for tag in &step.tags {
                    match tag {
                        Tag::Timeout(_) => ir.has_timeout = true,
                        Tag::Recover { .. } => ir.has_recover = true,
                        Tag::Error => {}
                    }
                }
            }
        }
        Expr::Call { name, args } => {
            ir.external_calls.push(name.clone());
            for a in args {
                walk_expr(a, ir);
            }
        }
        Expr::Ident(name) => {
            // treat bare idents that look like transforms as calls for residual
            if !["packet", "v", "d", "m", "last"].contains(&name.as_str()) {
                ir.external_calls.push(name.clone());
            }
        }
        Expr::FieldAccess { .. } | Expr::Literal(_) => {}
    }
}
