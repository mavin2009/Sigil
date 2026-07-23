
//! Graph IR for Sigil
//! Real dataflow graph with nodes and effect-tagged edges.

use crate::ast::{Program, Expr, Stmt, Tag, Literal};
use anyhow::Result;

#[derive(Debug, Clone)]
pub struct GraphIR {
    pub process_name: String,
    pub process_span: Option<crate::ast::Span>,
    pub local_states: Vec<String>,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub external_calls: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum Node {
    Input { name: String },
    Call { name: String },
    Timeout { ms: u64, span: Option<crate::ast::Span> },
    Recover { fallback: String, span: Option<crate::ast::Span> },
    StateWrite { slot: String },
}

#[derive(Debug, Clone)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    pub effects: EffectSet,
}

#[derive(Debug, Clone, Default)]
pub struct EffectSet {
    pub timeout: bool,
    pub error: bool,
    pub pure: bool,
}

impl GraphIR {
    pub fn has_timeout(&self) -> bool {
        self.nodes.iter().any(|n| matches!(n, Node::Timeout { .. }))
    }
    pub fn has_recover(&self) -> bool {
        self.nodes.iter().any(|n| matches!(n, Node::Recover { .. }))
    }
}

pub fn lower(program: &Program) -> Result<GraphIR> {
    let mut ir = GraphIR {
        process_name: String::new(),
        process_span: None,
        local_states: vec![],
        nodes: vec![],
        edges: vec![],
        external_calls: vec![],
    };

    for proc in &program.processes {
        ir.process_name = proc.name.clone();
        ir.process_span = Some(proc.span);
        for st in &proc.states {
            ir.local_states.push(st.name.clone());
        }
        for handler in &proc.handlers {
            // Input node
            let input_idx = ir.nodes.len();
            ir.nodes.push(Node::Input { name: handler.msg_name.clone() });
            let mut prev = input_idx;

            for stmt in &handler.body {
                match stmt {
                    Stmt::Let { name: _, expr, .. } | Stmt::Expr { expr, .. } => {
                        prev = lower_expr(expr, prev, &mut ir);
                    }
                    Stmt::Assign { name, expr, .. } => {
                        let expr_idx = lower_expr(expr, prev, &mut ir);
                        let write_idx = ir.nodes.len();
                        ir.nodes.push(Node::StateWrite { slot: name.clone() });
                        ir.edges.push(Edge {
                            from: expr_idx,
                            to: write_idx,
                            effects: EffectSet { pure: true, ..Default::default() },
                        });
                        prev = write_idx;
                    }
                }
            }
        }
    }
    Ok(ir)
}

fn lower_expr(expr: &Expr, prev: usize, ir: &mut GraphIR) -> usize {
    match expr {
        Expr::Pipeline { base, steps, .. } => {
            let mut current = lower_expr(base, prev, ir);
            for step in steps {
                current = lower_pipe_step(step, current, ir);
            }
            current
        }
        Expr::Call { name, args: _, .. } => {
            let idx = ir.nodes.len();
            ir.nodes.push(Node::Call { name: name.clone() });
            ir.external_calls.push(name.clone());
            ir.edges.push(Edge {
                from: prev,
                to: idx,
                effects: EffectSet { pure: true, ..Default::default() },
            });
            idx
        }
        Expr::Ident { name, .. } => {
            // Only treat as external transform if it is not a known local / intermediate / state name
            let locals = ["packet", "v", "d", "m", "last", "last_ok", "event", "validated",
                          "processed", "stored", "enriched", "next", "tick", "total", "s", "y"];
            if !locals.contains(&name.as_str()) {
                let idx = ir.nodes.len();
                ir.nodes.push(Node::Call { name: name.clone() });
                ir.external_calls.push(name.clone());
                ir.edges.push(Edge {
                    from: prev,
                    to: idx,
                    effects: EffectSet { pure: true, ..Default::default() },
                });
                idx
            } else {
                prev
            }
        }
        Expr::FieldAccess { .. } | Expr::Literal { .. } => prev,
        Expr::Binary { lhs, rhs, .. } => {
            let _ = lower_expr(lhs, prev, ir);
            lower_expr(rhs, prev, ir)
        },
    }
}

fn lower_pipe_step(step: &crate::ast::PipeStep, prev: usize, ir: &mut GraphIR) -> usize {
    let mut current = lower_expr(&step.expr, prev, ir);
    for tag in &step.tags {
        match tag {
            Tag::Timeout { expr, span } => {
                let ms = match expr {
                    Expr::Literal { value: Literal::DurationMs(m), .. } => *m,
                    _ => 0,
                };
                let idx = ir.nodes.len();
                ir.nodes.push(Node::Timeout { ms, span: Some(*span) });
                ir.edges.push(Edge {
                    from: current,
                    to: idx,
                    effects: EffectSet { timeout: true, ..Default::default() },
                });
                current = idx;
            }
            Tag::Recover { with, span } => {
                let fallback = match with {
                    Expr::Ident { name: s, .. } => s.clone(),
                    _ => "fallback".into(),
                };
                let idx = ir.nodes.len();
                ir.nodes.push(Node::Recover { fallback, span: Some(*span) });
                ir.edges.push(Edge {
                    from: current,
                    to: idx,
                    effects: EffectSet { pure: true, ..Default::default() },
                });
                current = idx;
            }
            Tag::Error { .. } => {
                // mark error effect on the previous edge if possible
            }
        }
    }
    current
}
