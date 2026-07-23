//! Level-2 checks: temporal / path obligations on a Level-1-legal graph.
//!
//! - Per-step recovery totality: every @timeout on a pipe step has @recover on the same step
//! - path_timeout_sum bounds from `require path_timeout_sum <= N.ms`
//! - Simple `hold state >= N` discharge for pure integer state
//! - `extinct` assumptions recorded for residual risk

use crate::analysis::ir::GraphIR;
use crate::frontend::ast::{
    BinOp, Expr, Literal, Program, SpecItem, StateDecl, Stmt, Tag, Type,
};
use anyhow::{bail, Result};
use std::collections::BTreeSet;

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

    check_per_step_recovery(program)?;
    report
        .discharged
        .push("per-step @timeout/@recover totality".into());

    let sum = path_timeout_sum_ms(ir);
    report.path_timeout_sum_ms = sum;

    let pure_transforms: BTreeSet<String> = program
        .transforms
        .iter()
        .filter(|t| !t.body.is_empty())
        .map(|t| t.name.clone())
        .collect();

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
                SpecItem::Hold { expr, span } => {
                    report.holds.push(format_expr(expr));
                    discharge_hold(
                        program,
                        expr,
                        &pure_transforms,
                        &mut report,
                        &spec.name,
                        span.start,
                        span.end,
                    )?;
                }
            }
        }
    }

    Ok(report)
}

fn discharge_hold(
    program: &Program,
    expr: &Expr,
    pure_transforms: &BTreeSet<String>,
    report: &mut Level2Report,
    spec_name: &str,
    start: usize,
    end: usize,
) -> Result<()> {
    // Support: state >= N  or state > N  with integer literal N
    let (state_name, op, bound) = match parse_numeric_hold(expr) {
        Some(v) => v,
        None => {
            report.residual_assumptions.push(format!(
                "spec `{}` hold `{}` — form not auto-discharged (need state >= N)",
                spec_name,
                format_expr(expr)
            ));
            return Ok(());
        }
    };

    let process = match program.processes.first() {
        Some(p) => p,
        None => return Ok(()),
    };

    let state_decl = process.states.iter().find(|s| s.name == state_name);
    let state_decl = match state_decl {
        Some(s) => s,
        None => {
            bail!(
                "Level-2 violation in spec '{}' at bytes {}..{}: hold refers to unknown state '{}'",
                spec_name,
                start,
                end,
                state_name
            );
        }
    };

    if !matches!(state_decl.ty, Type::Int | Type::Float) {
        report.residual_assumptions.push(format!(
            "spec `{}` hold on non-numeric state `{}`",
            spec_name, state_name
        ));
        return Ok(());
    }

    // Init must satisfy the hold
    if let Some(init_v) = literal_number(&state_decl.init) {
        if !cmp_holds(op, init_v, bound) {
            bail!(
                "Level-2 violation in spec '{}' at bytes {}..{}: \
                 initial value of '{}' is {} which falsifies hold `{}`",
                spec_name,
                start,
                end,
                state_name,
                init_v,
                format_expr(expr)
            );
        }
    } else {
        report.residual_assumptions.push(format!(
            "spec `{}` hold `{}` — init not a literal; not fully discharged",
            spec_name,
            format_expr(expr)
        ));
        return Ok(());
    }

    // All assignments to state must be pure
    let mut pure_ok = true;
    let mut uses_msg_fields = false;
    for handler in &process.handlers {
        for stmt in &handler.body {
            if let Stmt::Assign { name, expr: rhs, .. } = stmt {
                if name != &state_name {
                    continue;
                }
                match classify_rhs(rhs, pure_transforms, &process.states) {
                    RhsKind::Pure => {}
                    RhsKind::PureWithMsgFields => uses_msg_fields = true,
                    RhsKind::Impure => pure_ok = false,
                }
            }
        }
    }

    if !pure_ok {
        report.residual_assumptions.push(format!(
            "spec `{}` hold `{}` — state updated via residual/external transforms",
            spec_name,
            format_expr(expr)
        ));
        report.discharged.push(format!(
            "hold `{}` recorded (not discharged — impure updates)",
            format_expr(expr)
        ));
        return Ok(());
    }

    if uses_msg_fields {
        report.residual_assumptions.push(format!(
            "spec `{}` hold `{}` discharged for pure updates assuming message fields respect the bound",
            spec_name,
            format_expr(expr)
        ));
    }

    report.discharged.push(format!(
        "hold `{}` on pure state '{}' (init satisfies; updates pure)",
        format_expr(expr),
        state_name
    ));
    Ok(())
}

#[derive(Debug)]
enum RhsKind {
    Pure,
    PureWithMsgFields,
    Impure,
}

fn classify_rhs(
    expr: &Expr,
    pure_transforms: &BTreeSet<String>,
    states: &[StateDecl],
) -> RhsKind {
    let state_names: BTreeSet<_> = states.iter().map(|s| s.name.as_str()).collect();
    fn walk(
        expr: &Expr,
        pure_transforms: &BTreeSet<String>,
        state_names: &BTreeSet<&str>,
        saw_msg: &mut bool,
        impure: &mut bool,
    ) {
        match expr {
            Expr::Ident { name, .. } => {
                if !state_names.contains(name.as_str())
                    && name != "true"
                    && name != "false"
                {
                    // could be message or local — treat non-state as msg-ish
                    if !pure_transforms.contains(name) {
                        *saw_msg = true;
                    }
                }
            }
            Expr::FieldAccess { .. } => *saw_msg = true,
            Expr::Literal { .. } => {}
            Expr::Binary { lhs, rhs, .. } => {
                walk(lhs, pure_transforms, state_names, saw_msg, impure);
                walk(rhs, pure_transforms, state_names, saw_msg, impure);
            }
            Expr::Call { name, args, .. } => {
                if !pure_transforms.contains(name) {
                    *impure = true;
                }
                for a in args {
                    walk(a, pure_transforms, state_names, saw_msg, impure);
                }
            }
            Expr::Pipeline { base, steps, .. } => {
                walk(base, pure_transforms, state_names, saw_msg, impure);
                for step in steps {
                    match &step.expr {
                        Expr::Ident { name, .. } | Expr::Call { name, .. } => {
                            if !pure_transforms.contains(name) {
                                *impure = true;
                            }
                        }
                        other => walk(other, pure_transforms, state_names, saw_msg, impure),
                    }
                    if step.tags.iter().any(|t| matches!(t, Tag::Timeout { .. })) {
                        *impure = true;
                    }
                }
            }
        }
    }
    let mut saw_msg = false;
    let mut impure = false;
    walk(expr, pure_transforms, &state_names, &mut saw_msg, &mut impure);
    if impure {
        RhsKind::Impure
    } else if saw_msg {
        RhsKind::PureWithMsgFields
    } else {
        RhsKind::Pure
    }
}

fn parse_numeric_hold(expr: &Expr) -> Option<(String, BinOp, f64)> {
    match expr {
        Expr::Binary { op, lhs, rhs, .. }
            if matches!(op, BinOp::Ge | BinOp::Gt | BinOp::Le | BinOp::Lt | BinOp::Eq) =>
        {
            let name = match lhs.as_ref() {
                Expr::Ident { name, .. } => name.clone(),
                _ => return None,
            };
            let bound = literal_number(rhs)?;
            Some((name, op.clone(), bound))
        }
        _ => None,
    }
}

fn literal_number(expr: &Expr) -> Option<f64> {
    match expr {
        Expr::Literal {
            value: Literal::Int(i),
            ..
        } => Some(*i as f64),
        Expr::Literal {
            value: Literal::Float(f),
            ..
        } => Some(*f),
        _ => None,
    }
}

fn cmp_holds(op: BinOp, value: f64, bound: f64) -> bool {
    match op {
        BinOp::Ge => value >= bound,
        BinOp::Gt => value > bound,
        BinOp::Le => value <= bound,
        BinOp::Lt => value < bound,
        BinOp::Eq => (value - bound).abs() < f64::EPSILON,
        _ => false,
    }
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
    }

    #[test]
    fn hold_pure_counter_discharged() {
        let src = include_str!("../../../examples/runnable/counter/counter.sigil");
        let prog = parse(src).expect("parse");
        let ir = lower(&prog).expect("lower");
        let report = level2_check(&prog, &ir).expect("level2");
        assert!(
            report.discharged.iter().any(|d| d.contains("hold") && d.contains("pure")),
            "expected pure hold discharge: {:?}",
            report.discharged
        );
    }

    #[test]
    fn hold_bad_init_rejected() {
        let src = r#"
schema Tick { value: Int }
process Counter {
  state total: Int = -1
  on tick: Tick {
    total := total + tick.value
  }
}
spec Bad {
  hold total >= 0
}
"#;
        let prog = parse(src).expect("parse");
        let ir = lower(&prog).expect("lower");
        let err = level2_check(&prog, &ir).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("Level-2"), "{msg}");
        assert!(msg.contains("initial") || msg.contains("-1"), "{msg}");
    }

    #[test]
    fn pipeline_example_level2_ok() {
        let src = include_str!("../../../examples/pipeline/pipeline.sigil");
        let prog = parse(src).expect("parse");
        let ir = lower(&prog).expect("lower");
        let report = level2_check(&prog, &ir).expect("level2");
        assert!(report.path_timeout_sum_ms >= 300);
    }
}
