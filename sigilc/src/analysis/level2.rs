//! Level-2 checks: temporal / path obligations on a Level-1-legal graph.
//!
//! - Per-step recovery totality (AST tags + Graph IR Timeout→Recover edges)
//! - path_timeout_sum bounds from `require path_timeout_sum <= N.ms`
//! - `hold state >= N` discharge for pure Int/Float state
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
    /// Processing + declared hand-off waits, longest path.
    pub path_latency_ms: u64,
    /// Sends whose wait is unbounded (`@block`), blocking a latency proof.
    pub latency_blockers: Vec<String>,
    pub path_timeout_bound_ms: Option<u64>,
    pub holds: Vec<String>,
    pub extinct: Vec<String>,
    pub discharged: Vec<String>,
    pub residual_assumptions: Vec<String>,
}

pub fn level2_check(program: &Program, irs: &[GraphIR]) -> Result<Level2Report> {
    let mut report = Level2Report::default();

    check_per_step_recovery(program)?;
    report
        .discharged
        .push("per-step @timeout/@recover totality (AST)".into());

    for ir in irs {
        check_ir_timeout_recover_edges(ir)?;
    }
    report
        .discharged
        .push("Timeout→Recover edges present in Graph IR (per process)".into());

    // End-to-end worst-case latency: the LONGEST path through the process
    // topology, where each process contributes its own timed-stage sum
    // (already charged (1 + retries) × timeout per stage). For a single
    // process this degenerates to that process's sum; parallel branches
    // take the max, not the sum.
    let per_process: std::collections::BTreeMap<&str, u64> = program
        .processes
        .iter()
        .map(|p| (p.name.as_str(), process_worst_case_ms(p)))
        .collect();
    let sum = longest_path(program, &per_process);
    report.path_timeout_sum_ms = sum;

    // End-to-end latency: processing + declared hand-off waits, longest path.
    let mut latency_blockers: Vec<String> = Vec::new();
    let per_process_latency: std::collections::BTreeMap<&str, u64> = program
        .processes
        .iter()
        .map(|p| {
            let (ms, blocker) = process_worst_case_latency_ms(p).unwrap_or((0, None));
            if let Some(b) = blocker {
                latency_blockers.push(b);
            }
            (p.name.as_str(), ms)
        })
        .collect();
    let latency = longest_path(program, &per_process_latency);
    report.path_latency_ms = latency;
    report.latency_blockers = latency_blockers.clone();
    let blockers_snapshot = latency_blockers;

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
                    check_require(expr, sum, latency, &blockers_snapshot, &mut report, &spec.name, span.start, span.end)?;
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

    // A hold may target state in ANY process (multi-process topologies).
    let owner = program
        .processes
        .iter()
        .find(|p| p.states.iter().any(|s| s.name == state_name));
    let state_decl = owner.and_then(|p| p.states.iter().find(|s| s.name == state_name));
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
    let owner = owner.expect("state owner exists when state_decl matched");
    for handler in &owner.handlers {
        for stmt in &handler.body {
            if let Stmt::Assign { name, expr: rhs, .. } = stmt {
                if *name != state_name {
                    continue;
                }
                match classify_rhs(rhs, pure_transforms, &owner.states) {
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


fn check_ir_timeout_recover_edges(ir: &GraphIR) -> Result<()> {
    use crate::analysis::ir::Node;
    for (idx, node) in ir.nodes.iter().enumerate() {
        if let Node::Timeout { span, ms, .. } = node {
            let has_recover_succ = ir.edges.iter().any(|e| {
                e.from == idx
                    && matches!(ir.nodes.get(e.to), Some(Node::ErrorAck { .. }))
            }) || ir.edges.iter().any(|e| {
                e.from == idx
                    && matches!(ir.nodes.get(e.to), Some(Node::Recover { .. }))
            });
            // Also accept immediate next node Recover (same step lowering)
            let next_is_recover = matches!(
                ir.nodes.get(idx + 1),
                Some(Node::Recover { .. })
            );
            if !has_recover_succ && !next_is_recover {
                let loc = span
                    .map(|s| format!(" at bytes {}..{}", s.start, s.end))
                    .unwrap_or_default();
                bail!(
                    "Level-2 violation in process '{}'{}: Timeout node ({}ms) has no Recover successor in Graph IR",
                    ir.process_name,
                    loc,
                    ms
                );
            }
        }
    }
    Ok(())
}

fn check_per_step_recovery(program: &Program) -> Result<()> {
    for process in &program.processes {
        for handler in &process.handlers {
            for stmt in &handler.body {
                let expr = match stmt {
                    Stmt::Let { expr, .. }
                    | Stmt::Assign { expr, .. }
                    | Stmt::Send { expr, .. }
                    | Stmt::Expr { expr, .. } => {
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
                let has_recover = step.tags.iter().any(|t| matches!(t, Tag::Recover { .. }))
                    || step.tags.iter().any(|t| matches!(t, Tag::Error { .. }));
                if has_timeout && !has_recover {
                    bail!(
                        "Level-2 violation in process '{}' at bytes {}..{}: \
                         @timeout on a pipeline step without @recover or @error on the same step",
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

/// Worst-case time a SINGLE message spends in one process.
///
/// A message is dispatched to exactly one handler, so a process contributes
/// the maximum over its handlers — not the sum. (Summing over handlers
/// inflates the budget of every multi-handler process and would reject
/// programs that comfortably meet their SLO.) Within a handler the timed
/// stages are sequential, so they add, each charged `(1 + retries) × timeout`.
fn process_worst_case_ms(process: &crate::frontend::ast::Process) -> u64 {
    process
        .handlers
        .iter()
        .map(|h| {
            h.body
                .iter()
                .map(|stmt| match stmt {
                    Stmt::Let { expr, .. }
                    | Stmt::Assign { expr, .. }
                    | Stmt::Send { expr, .. }
                    | Stmt::Expr { expr, .. } => expr_timeout_ms(expr),
                })
                .sum::<u64>()
        })
        .max()
        .unwrap_or(0)
}

/// Worst case for one message in a process, INCLUDING time spent handing
/// off downstream under the declared back-pressure policy.
///
/// `None` means unbounded: some handler uses `@block`, whose wait has no
/// time bound. End-to-end latency then cannot be proven, only measured.
fn process_worst_case_latency_ms(
    process: &crate::frontend::ast::Process,
) -> Option<(u64, Option<String>)> {
    let mut worst = 0u64;
    let mut blocker: Option<String> = None;
    for h in &process.handlers {
        let mut total = 0u64;
        for stmt in &h.body {
            match stmt {
                Stmt::Send { target, backpressure, expr, .. } => {
                    total += expr_timeout_ms(expr);
                    match backpressure.budget_ms() {
                        Some(ms) => total += ms,
                        None => {
                            blocker = Some(format!(
                                "`{}` handler of `{}` sends to `{target}` with @block",
                                h.msg_name, process.name
                            ));
                        }
                    }
                }
                Stmt::Let { expr, .. } | Stmt::Assign { expr, .. } | Stmt::Expr { expr, .. } => {
                    total += expr_timeout_ms(expr);
                }
            }
        }
        worst = worst.max(total);
    }
    Some((worst, blocker))
}

/// Longest path through the process topology, where each process
/// contributes its own worst case. Parallel branches take the max.
fn longest_path(
    program: &Program,
    per_process: &std::collections::BTreeMap<&str, u64>,
) -> u64 {
    match crate::analysis::topology::derive_topology(program) {
        Ok(topo) if topo.is_pipeline() => {
            let mut longest: std::collections::BTreeMap<&str, u64> =
                std::collections::BTreeMap::new();
            for pname in &topo.order {
                let own = *per_process.get(pname.as_str()).unwrap_or(&0);
                let best_pred = topo
                    .edges
                    .iter()
                    .filter(|e| e.to == *pname)
                    .filter_map(|e| longest.get(e.from.as_str()).copied())
                    .max()
                    .unwrap_or(0);
                longest.insert(pname.as_str(), best_pred + own);
            }
            longest.values().copied().max().unwrap_or(0)
        }
        _ => per_process.values().copied().max().unwrap_or(0),
    }
}

fn expr_timeout_ms(expr: &Expr) -> u64 {
    match expr {
        Expr::Pipeline { base, steps, .. } => {
            let mut total = expr_timeout_ms(base);
            for step in steps {
                let ms = step.tags.iter().find_map(|t| match t {
                    Tag::Timeout { expr: Expr::Literal { value: Literal::DurationMs(m), .. }, .. } => Some(*m),
                    _ => None,
                });
                let retries = step
                    .tags
                    .iter()
                    .find_map(|t| match t {
                        Tag::Retry { expr: Expr::Literal { value: Literal::Int(n), .. }, .. } => {
                            Some((*n).max(0) as u64)
                        }
                        _ => None,
                    })
                    .unwrap_or(0);
                total += ms.unwrap_or(0) * (1 + retries);
            }
            total
        }
        Expr::Binary { lhs, rhs, .. } => expr_timeout_ms(lhs) + expr_timeout_ms(rhs),
        _ => 0,
    }
}

fn check_require(
    expr: &Expr,
    path_sum: u64,
    latency: u64,
    blockers: &[String],
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
        let is_latency = matches!(
            lhs.as_ref(),
            Expr::Ident { name, .. } if name == "path_latency"
        );
        if is_latency {
            let bound = match rhs.as_ref() {
                Expr::Literal { value: Literal::DurationMs(ms), .. } => *ms,
                _ => bail!(
                    "Level-2 violation in spec '{}' at bytes {}..{}: path_latency bound \
                     must be a duration literal (e.g. 500.ms)",
                    spec_name, start, end
                ),
            };
            if !blockers.is_empty() {
                bail!(
                    "Level-2 violation in spec '{}' at bytes {}..{}: `require path_latency` \
                     claims a bound on END-TO-END latency, but {} — `@block` waits for an \
                     unbounded time when the destination queue is full. Declare a bounded \
                     policy (`@deadline(N.ms)` or `@shed`) on every send, or use \
                     `require path_timeout_sum` which bounds processing time only.",
                    spec_name, start, end, blockers.join("; ")
                );
            }
            if latency > bound {
                bail!(
                    "Level-2 violation in spec '{}' at bytes {}..{}: path_latency is {}ms \
                     but require path_latency <= {}ms (processing + declared hand-off waits)",
                    spec_name, start, end, latency, bound
                );
            }
            report.discharged.push(format!(
                "path_latency {latency}ms <= {bound}ms (processing + hand-off, all sends bounded)"
            ));
            return Ok(());
        }
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
        assert!(report.path_timeout_bound_ms == Some(500) || report.discharged.iter().any(|d| d.contains("path_timeout")));
        // total_charged >= 0.0 is pure arithmetic + msg field
        assert!(
            report.discharged.iter().any(|d| d.contains("total_charged") || d.contains("hold")),
            "expected hold discharge notes: {:?}",
            report.discharged
        );
        assert!(
            report.discharged.iter().any(|d| d.contains("Timeout→Recover") || d.contains("Graph IR")),
            "expected IR recovery discharge: {:?}",
            report.discharged
        );
    }

    #[test]
    fn level2_combined_example() {
        let src = include_str!("../../../examples/level2/slo_and_hold.sigil");
        let prog = parse(src).expect("parse");
        assert!(!prog.specs.is_empty());
        let ir = lower(&prog).expect("lower");
        let report = level2_check(&prog, &ir).expect("level2");
        assert_eq!(report.path_timeout_sum_ms, 80);
        assert!(report.discharged.iter().any(|d| d.contains("hits") || d.contains("hold")));
    }

    #[test]
    fn per_step_recover_required_even_if_process_has_recover() {
        let src = include_str!("../../../examples/proofs/timeout_without_step_recover.sigil");
        let prog = parse(src).expect("parse");
        let ir = lower(&prog).expect("lower");
        // Level-1 may pass (global has recover)
        let _ = ir.iter().map(crate::analysis::check::level1_check).collect::<Vec<_>>();
        let err = level2_check(&prog, &ir).expect_err("level2 must require per-step recover");
        let msg = format!("{err}");
        assert!(msg.contains("Level-2"), "{msg}");
        assert!(msg.contains("@timeout") || msg.contains("same step") || msg.contains("Recover"), "{msg}");
    }

}
