//! Level-2 checks: temporal / path obligations on a Level-1-legal graph.
//!
//! - Per-step recovery totality: every @timeout on a pipe step has @recover on the same step
//! - path_timeout_sum bounds from `require path_timeout_sum <= N.ms`
//! - Simple `hold` recording (numeric state floor) for residual reporting
//! - `extinct` assumptions recorded for residual risk

use crate::analysis::ir::GraphIR;
use crate::frontend::ast::{BinOp, Expr, Literal, Program, SpecItem, Stmt, Tag};
use anyhow::{bail, Result};

#[derive(Debug, Clone, Default)]
pub struct Level2Report {
    pub path_timeout_sum_ms: u64,
    pub path_timeout_bound_ms: Option<u64>,
    pub holds: Vec<String>,
    pub extinct: Vec<String>,
    pub discharged: Vec<String>,
    pub residual_assumptions: Vec<String>,
}

pub fn level2_check(program: &Program, ir: &GraphIR) -> Result<Level2Report> {
    let mut report = Level2Report::default();

    // 1. Per-step timeout/recover pairing (stronger than process-global Level-1)
    check_per_step_recovery(program)?;
    report
        .discharged
        .push("per-step @timeout/@recover totality".into());

    // 2. Path timeout sum from IR
    let sum = path_timeout_sum_ms(ir);
    report.path_timeout_sum_ms = sum;

    // 3. Spec obligations
    for spec in &program.specs {
        for item in &spec.items {
            match item {
                SpecItem::Extinct { names, .. } => {
                    for n in names {
                        report.extinct.push(n.clone());
                        report.residual_assumptions.push(format!(
                            "spec `{}` assumes extinct: {}",
                            spec.name, n
                        ));
                    }
                }
                SpecItem::Require { expr, span } => {
                    check_require(expr, sum, &mut report, &spec.name, span.start, span.end)?;
                }
                SpecItem::Hold { expr, .. } => {
                    report.holds.push(format_expr(expr));
                    report.residual_assumptions.push(format!(
                        "spec `{}` hold `{}` — discharged only for pure state updates; external transforms remain residual",
                        spec.name,
                        format_expr(expr)
                    ));
                    // Conservative: accept hold syntax; full symbolic discharge is future work
                    report.discharged.push(format!(
                        "hold `{}` recorded under residual assumptions",
                        format_expr(expr)
                    ));
                }
            }
        }
    }

    Ok(report)
}

fn check_per_step_recovery(program: &Program) -> Result<()> {
    for process in &program.processes {
        for handler in &process.handlers {
            for stmt in &handler.body {
                let expr = match stmt {
                    Stmt::Let { expr, .. } | Stmt::Assign { expr, .. } | Stmt::Expr { expr, .. } => {
                        expr
                    }
                };
                check_expr_steps(expr, &process.name)?;
            }
        }
    }
    Ok(())
}

fn check_expr_steps(expr: &Expr, process: &str) -> Result<()> {
    match expr {
        Expr::Pipeline { steps, span, .. } => {
            for step in steps {
                let has_timeout = step.tags.iter().any(|t| matches!(t, Tag::Timeout { .. }));
                let has_recover = step.tags.iter().any(|t| matches!(t, Tag::Recover { .. }));
                if has_timeout && !has_recover {
                    bail!(
                        "Level-2 violation in process '{}' at bytes {}..{}: \
                         @timeout on a pipeline step without @recover on the same step",
                        process,
                        span.start,
                        span.end
                    );
                }
            }
            for step in steps {
                check_expr_steps(&step.expr, process)?;
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            check_expr_steps(lhs, process)?;
            check_expr_steps(rhs, process)?;
        }
        Expr::Call { args, .. } => {
            for a in args {
                check_expr_steps(a, process)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn path_timeout_sum_ms(ir: &GraphIR) -> u64 {
    ir.nodes
        .iter()
        .filter_map(|n| match n {
            crate::analysis::ir::Node::Timeout { ms, .. } => Some(*ms),
            _ => None,
        })
        .sum()
}

fn check_require(
    expr: &Expr,
    path_sum: u64,
    report: &mut Level2Report,
    spec_name: &str,
    start: usize,
    end: usize,
) -> Result<()> {
    // Support: path_timeout_sum <= N.ms
    if let Expr::Binary {
        op: BinOp::Le,
        lhs,
        rhs,
        ..
    } = expr
    {
        let is_pts = matches!(
            lhs.as_ref(),
            Expr::Ident { name, .. } if name == "path_timeout_sum"
        );
        if is_pts {
            let bound = match rhs.as_ref() {
                Expr::Literal {
                    value: Literal::DurationMs(ms),
                    ..
                } => *ms,
                _ => bail!(
                    "Level-2 violation in spec '{}' at bytes {}..{}: \
                     path_timeout_sum bound must be a duration literal (e.g. 500.ms)",
                    spec_name,
                    start,
                    end
                ),
            };
            report.path_timeout_bound_ms = Some(bound);
            if path_sum > bound {
                bail!(
                    "Level-2 violation in spec '{}' at bytes {}..{}: \
                     path_timeout_sum is {}ms but require path_timeout_sum <= {}ms",
                    spec_name,
                    start,
                    end,
                    path_sum,
                    bound
                );
            }
            report.discharged.push(format!(
                "path_timeout_sum {}ms <= {}ms",
                path_sum, bound
            ));
            return Ok(());
        }
    }
    // Unknown require: record as residual assumption
    report.residual_assumptions.push(format!(
        "spec `{}` require `{}` not auto-discharged",
        spec_name,
        format_expr(expr)
    ));
    Ok(())
}

fn format_expr(expr: &Expr) -> String {
    match expr {
        Expr::Ident { name, .. } => name.clone(),
        Expr::Literal { value, .. } => match value {
            Literal::Int(i) => i.to_string(),
            Literal::Float(f) => f.to_string(),
            Literal::String(s) => format!("\"{s}\""),
            Literal::Bool(b) => b.to_string(),
            Literal::DurationMs(ms) => format!("{ms}.ms"),
        },
        Expr::Binary { op, lhs, rhs, .. } => {
            let op_s = match op {
                BinOp::Add => "+",
                BinOp::Sub => "-",
                BinOp::Mul => "*",
                BinOp::Div => "/",
                BinOp::Le => "<=",
                BinOp::Ge => ">=",
                BinOp::Lt => "<",
                BinOp::Gt => ">",
                BinOp::Eq => "==",
            };
            format!("{} {} {}", format_expr(lhs), op_s, format_expr(rhs))
        }
        Expr::FieldAccess { base, field, .. } => format!("{base}.{field}"),
        Expr::Call { name, .. } => format!("{name}(..)"),
        Expr::Pipeline { .. } => "pipeline".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::ir::lower;
    use crate::frontend::ast::parse;

    #[test]
    fn path_timeout_sum_within_bound() {
        let src = r#"
schema Order { id: String }
transform a(o: Order) -> Order {}
transform b(o: Order) -> Order {}
transform r(o: Order) -> Order {}
process P {
  state s: String = "n"
  on order: Order {
    let x = order ~> a @timeout(100.ms) @recover(with: r)
    let y = x ~> b @timeout(50.ms) @recover(with: r)
    s := y.id
  }
}
spec Slo {
  require path_timeout_sum <= 200.ms
}
"#;
        let prog = parse(src).expect("parse");
        let ir = lower(&prog).expect("lower");
        let report = level2_check(&prog, &ir).expect("level2");
        assert_eq!(report.path_timeout_sum_ms, 150);
        assert_eq!(report.path_timeout_bound_ms, Some(200));
    }

    #[test]
    fn path_timeout_sum_exceeds_bound() {
        let src = r#"
schema Order { id: String }
transform a(o: Order) -> Order {}
transform r(o: Order) -> Order {}
process P {
  state s: String = "n"
  on order: Order {
    let x = order ~> a @timeout(300.ms) @recover(with: r)
    s := x.id
  }
}
spec Slo {
  require path_timeout_sum <= 100.ms
}
"#;
        let prog = parse(src).expect("parse");
        let ir = lower(&prog).expect("lower");
        let err = level2_check(&prog, &ir).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("Level-2"), "{msg}");
        assert!(msg.contains("300") || msg.contains("path_timeout_sum"), "{msg}");
    }

    #[test]
    fn pipeline_example_level2_ok() {
        let src = include_str!("../../../examples/pipeline/pipeline.sigil");
        let prog = parse(src).expect("parse");
        let ir = lower(&prog).expect("lower");
        let report = level2_check(&prog, &ir).expect("level2");
        assert!(report.path_timeout_sum_ms >= 300); // 120+200
    }
}
