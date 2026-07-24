//! Level-1 extinct-by-design checks on the Graph IR and declared transforms.

use crate::analysis::ir::{GraphIR, Node};
use crate::analysis::types::{infer_program, type_name};
use crate::frontend::ast::{BinOp, Expr, Literal, Program, SpecItem, Stmt, Tag, Type};
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;

pub fn level1_check(ir: &GraphIR) -> Result<()> {
    // Per-step @timeout pairing lives in check_failure_paths (AST) where
    // @error is visible; the old process-global pairing was both too weak
    // (cross-handler pairing) and too strong (rejected @timeout+@error).
    let has_timeout = ir.has_timeout();
    let has_recover =
        ir.has_recover() || ir.nodes.iter().any(|n| matches!(n, Node::ErrorAck { .. }));

    if has_timeout && !has_recover {
        let loc = ir
            .nodes
            .iter()
            .find_map(|n| match n {
                Node::Timeout { span: Some(s), .. } => {
                    Some(format!(" at bytes {}..{}", s.start, s.end))
                }
                _ => None,
            })
            .or_else(|| {
                ir.process_span
                    .map(|s| format!(" at bytes {}..{}", s.start, s.end))
            })
            .unwrap_or_default();
        bail!(
            "Level-1 violation in process '{}'{}: @timeout without a matching @recover path",
            ir.process_name,
            loc
        );
    }

    for node in &ir.nodes {
        if let Node::StateWrite { slot } = node {
            if !ir.local_states.contains(slot) {
                let loc = ir
                    .process_span
                    .map(|s| format!(" at bytes {}..{}", s.start, s.end))
                    .unwrap_or_default();
                bail!(
                    "Level-1 violation in process '{}'{}: state write to non-local slot '{}'",
                    ir.process_name,
                    loc,
                    slot
                );
            }
        }
    }

    Ok(())
}

/// Check pipeline stages against declared transform signatures.
/// Feeding a value of type A into a transform declared as `(B) -> _` is a Level-1 error
/// when A and B are both named schemas and differ.
pub fn check_transform_signatures(program: &Program) -> Result<()> {
    let declared: BTreeMap<String, (String, String)> = program
        .transforms
        .iter()
        .map(|t| {
            (
                t.name.clone(),
                (type_name(&t.param_ty), type_name(&t.return_ty)),
            )
        })
        .collect();

    if declared.is_empty() {
        return Ok(());
    }

    let (env, _) = infer_program(program);

    for process in &program.processes {
        for handler in &process.handlers {
            let mut local_env = env.clone();
            local_env.insert(handler.msg_name.clone(), type_name(&handler.msg_ty));

            for stmt in &handler.body {
                match stmt {
                    Stmt::Send { expr, span, .. } => {
                        check_expr_signatures(
                            expr,
                            &local_env,
                            &declared,
                            &process.name,
                            span.start,
                            span.end,
                        )?;
                        continue;
                    }
                    Stmt::Let { name, expr, span } => {
                        check_expr_signatures(
                            expr,
                            &local_env,
                            &declared,
                            &process.name,
                            span.start,
                            span.end,
                        )?;
                        // Update local env with inferred/declared output of this binding
                        if let Some(out) = expr_output_type(expr, &local_env, &declared) {
                            local_env.insert(name.clone(), out);
                        }
                    }
                    Stmt::Assign { expr, span, .. } | Stmt::Expr { expr, span } => {
                        check_expr_signatures(
                            expr,
                            &local_env,
                            &declared,
                            &process.name,
                            span.start,
                            span.end,
                        )?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn expr_output_type(
    expr: &Expr,
    env: &BTreeMap<String, String>,
    declared: &BTreeMap<String, (String, String)>,
) -> Option<String> {
    match expr {
        Expr::Pipeline { base, steps, .. } => {
            let mut cur = expr_output_type(base, env, declared)?;
            for step in steps {
                let tname = match &step.expr {
                    Expr::Ident { name, .. } | Expr::Call { name, .. } => name.as_str(),
                    _ => return Some(cur),
                };
                if let Some((_, out)) = declared.get(tname) {
                    cur = out.clone();
                }
            }
            Some(cur)
        }
        Expr::Ident { name, .. } => env.get(name).cloned(),
        Expr::Call { name, .. } => declared.get(name).map(|(_, o)| o.clone()),
        _ => None,
    }
}

fn check_expr_signatures(
    expr: &Expr,
    env: &BTreeMap<String, String>,
    declared: &BTreeMap<String, (String, String)>,
    process: &str,
    start: usize,
    end: usize,
) -> Result<()> {
    match expr {
        Expr::Pipeline { base, steps, .. } => {
            let mut cur = match expr_output_type(base, env, declared) {
                Some(t) => t,
                None => return Ok(()), // cannot determine; skip
            };
            for step in steps {
                let tname = match &step.expr {
                    Expr::Ident { name, .. } | Expr::Call { name, .. } => name.clone(),
                    other => {
                        check_expr_signatures(other, env, declared, process, start, end)?;
                        continue;
                    }
                };
                if let Some((expected_in, out_ty)) = declared.get(&tname) {
                    if is_named_schema(&cur) && is_named_schema(expected_in) && cur != *expected_in
                    {
                        bail!(
                            "Level-1 violation in process '{}' at bytes {}..{}: \
                             transform '{}' expects input type {}, but previous stage has type {}",
                            process,
                            start,
                            end,
                            tname,
                            expected_in,
                            cur
                        );
                    }
                    cur = out_ty.clone();
                }
                // Recovery signatures are checked precisely by
                // `check_recover_signatures`, which retains the stage input.
            }
            Ok(())
        }
        Expr::Call { name, args, .. } => {
            if let Some((expected_in, _)) = declared.get(name) {
                if let Some(arg) = args.first() {
                    if let Some(arg_ty) = expr_output_type(arg, env, declared) {
                        if is_named_schema(&arg_ty)
                            && is_named_schema(expected_in)
                            && arg_ty != *expected_in
                        {
                            bail!(
                                "Level-1 violation in process '{}' at bytes {}..{}: \
                                 transform '{}' expects {}, got {}",
                                process,
                                start,
                                end,
                                name,
                                expected_in,
                                arg_ty
                            );
                        }
                    }
                }
            }
            for a in args {
                check_expr_signatures(a, env, declared, process, start, end)?;
            }
            Ok(())
        }
        Expr::Binary { lhs, rhs, .. } => {
            check_expr_signatures(lhs, env, declared, process, start, end)?;
            check_expr_signatures(rhs, env, declared, process, start, end)
        }
        _ => Ok(()),
    }
}

fn is_named_schema(ty: &str) -> bool {
    !matches!(
        ty,
        "Int"
            | "Float"
            | "String"
            | "Bool"
            | "UUID"
            | "Bytes"
            | "Duration"
            | "i64"
            | "f64"
            | "bool"
            | "()"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::ir::{GraphIR, Node};
    use crate::frontend::ast::parse;

    #[test]
    fn accepts_handled_timeout() {
        let ir = GraphIR {
            process_name: "P".into(),
            process_span: None,
            local_states: vec!["s".into()],
            nodes: vec![
                Node::Timeout {
                    ms: 50,
                    attempts: 1,
                    span: None,
                },
                Node::Recover {
                    fallback: "f".into(),
                    span: None,
                },
            ],
            edges: vec![],
            external_calls: vec![],
        };
        assert!(level1_check(&ir).is_ok());
    }

    #[test]
    fn rejects_unhandled_timeout() {
        let ir = GraphIR {
            process_name: "P".into(),
            process_span: None,
            local_states: vec![],
            nodes: vec![Node::Timeout {
                ms: 50,
                attempts: 1,
                span: None,
            }],
            edges: vec![],
            external_calls: vec![],
        };
        let err = level1_check(&ir).unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("Level-1 violation"));
        assert!(msg.contains("@timeout"));
    }

    #[test]
    fn rejects_nonlocal_state_write() {
        let ir = GraphIR {
            process_name: "P".into(),
            process_span: None,
            local_states: vec!["s".into()],
            nodes: vec![Node::StateWrite {
                slot: "other".into(),
            }],
            edges: vec![],
            external_calls: vec![],
        };
        let err = level1_check(&ir).unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("non-local slot"));
    }

    #[test]
    fn accepts_matching_transform_pipeline() {
        let src = include_str!("../../../examples/pipeline/pipeline.sigil");
        let prog = parse(src).expect("parse");
        assert!(check_transform_signatures(&prog).is_ok());
    }

    #[test]
    fn rejects_mismatched_transform_pipeline() {
        let src = r#"
schema Order { id: String }
schema Receipt { id: String, status: String }
transform confirm(o: Order) -> Receipt {}
transform needs_receipt(r: Receipt) -> Receipt {}
process P {
  state s: String = "none"
  on order: Order {
    let bad = order ~> needs_receipt
    s := bad.id
  }
}
"#;
        let prog = parse(src).expect("parse");
        let err = check_transform_signatures(&prog).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("Level-1 violation"), "{msg}");
        assert!(msg.contains("needs_receipt"), "{msg}");
        assert!(msg.contains("Receipt") && msg.contains("Order"), "{msg}");
    }
}

/// Level-1: no unhandled failure paths.
///
/// Every pipeline stage that invokes an EXTERNAL transform (declared with an
/// empty body, or never declared) can fail at runtime. Each such stage must
/// either declare a recovery path (`@recover(with: f)`) or explicitly
/// acknowledge the failure (`@error`, meaning: on failure this message is
/// intentionally dropped and the drop is accounted for).
///
/// Pure transforms (non-empty bodies) are compiled and infallible, so they
/// need no tag. Recovery fallbacks SHOULD be pure transforms — a fallible
/// fallback reintroduces exactly the loss it was meant to prevent.
pub fn check_failure_paths(program: &Program) -> Result<()> {
    fallible_fallbacks(program).map(|_| ())
}

/// Recovery targets that are themselves external (and so can fail or hang).
/// A fallible fallback reintroduces exactly the loss it exists to prevent;
/// it is reported as residual risk always, and rejected at Level 3+ where
/// proofs are being claimed.
pub fn fallible_fallbacks(program: &Program) -> Result<Vec<String>> {
    use std::collections::BTreeSet;

    // Infallible-bound transforms are declared as unable to fail, so they are
    // legitimate recovery targets alongside compiled pure bodies. Async and
    // blocking bindings perform real I/O and remain external.
    let pure: BTreeSet<&str> = program
        .transforms
        .iter()
        .filter(|t| !t.body.is_empty() || t.is_infallible())
        .map(|t| t.name.as_str())
        .collect();

    let mut fallible_fallbacks: Vec<String> = Vec::new();

    for process in &program.processes {
        for handler in &process.handlers {
            for stmt in &handler.body {
                let expr = match stmt {
                    Stmt::Let { expr, .. }
                    | Stmt::Assign { expr, .. }
                    | Stmt::Send { expr, .. }
                    | Stmt::Expr { expr, .. } => expr,
                };
                walk_failure_paths(expr, &pure, &process.name, &mut fallible_fallbacks)?;
            }
        }
    }
    fallible_fallbacks.sort();
    fallible_fallbacks.dedup();
    Ok(fallible_fallbacks)
}

fn walk_failure_paths(
    expr: &Expr,
    pure: &std::collections::BTreeSet<&str>,
    process: &str,
    fallible_fallbacks: &mut Vec<String>,
) -> Result<()> {
    match expr {
        Expr::Pipeline { base, steps, .. } => {
            walk_failure_paths(base, pure, process, fallible_fallbacks)?;
            for step in steps {
                let step_span = step_span_of(step);
                let target = match &step.expr {
                    Expr::Ident { name, .. } => Some(name.as_str()),
                    Expr::Call { name, .. } => Some(name.as_str()),
                    _ => None,
                };
                if let Some(name) = target {
                    let is_external = !pure.contains(name);
                    let n_timeout = step
                        .tags
                        .iter()
                        .filter(|t| matches!(t, Tag::Timeout { .. }))
                        .count();
                    let n_recover = step
                        .tags
                        .iter()
                        .filter(|t| matches!(t, Tag::Recover { .. }))
                        .count();
                    let n_retry = step
                        .tags
                        .iter()
                        .filter(|t| matches!(t, Tag::Retry { .. }))
                        .count();
                    let n_error = step
                        .tags
                        .iter()
                        .filter(|t| matches!(t, Tag::Error { .. }))
                        .count();
                    if n_timeout > 1 || n_recover > 1 || n_retry > 1 || n_error > 1 {
                        bail!(
                            "Level-1 violation in process '{process}' at bytes {}..{}: \
                             stage '{name}' repeats an effect tag — at most one @timeout, \
                             @recover, @retry, and @error per step",
                            step_span.start,
                            step_span.end
                        );
                    }
                    if n_recover == 1 && n_error == 1 {
                        bail!(
                            "Level-1 violation in process '{process}' at bytes {}..{}: \
                             stage '{name}' declares both @recover and @error — a step \
                             either recovers or acknowledges the drop, not both",
                            step_span.start,
                            step_span.end
                        );
                    }
                    let has_recover = n_recover == 1;
                    let has_error = n_error == 1;
                    if n_timeout == 1 && !has_recover && !has_error {
                        bail!(
                            "Level-1 violation in process '{process}' at bytes {}..{}: \
                             timed stage '{name}' has no failure path on the same step — \
                             add @recover(with: f) or acknowledge the drop with @error",
                            step_span.start,
                            step_span.end
                        );
                    }
                    if let Some(retry) = step.tags.iter().find_map(|t| match t {
                        Tag::Retry { expr, .. } => Some(expr),
                        _ => None,
                    }) {
                        if !has_recover && !has_error {
                            bail!(
                                "Level-1 violation in process '{process}' at bytes {}..{}: \
                                 stage '{name}' declares @retry without a terminal failure \
                                 path — retries delay failure, they do not handle it; add \
                                 @recover or @error",
                                step_span.start,
                                step_span.end
                            );
                        }
                        match retry {
                            crate::frontend::ast::Expr::Literal {
                                value: crate::frontend::ast::Literal::Int(n),
                                ..
                            } if *n >= 1 => {}
                            _ => bail!(
                                "Level-1 violation in process '{process}': @retry on stage \
                                 '{name}' requires an integer literal count >= 1"
                            ),
                        }
                    }
                    if is_external && !has_recover && !has_error {
                        bail!(
                            "Level-1 violation in process '{process}' at bytes {}..{}: \
                             external stage '{name}' has no failure path — add \
                             @recover(with: <pure transform>) or acknowledge the drop \
                             explicitly with @error",
                            step_span.start,
                            step_span.end
                        );
                    }
                    // Advisory: fallible fallback (external recover target)
                    for t in &step.tags {
                        if let Tag::Recover {
                            with: Expr::Ident { name: fb, .. } | Expr::Call { name: fb, .. },
                            ..
                        } = t
                        {
                            if !pure.contains(fb.as_str()) {
                                fallible_fallbacks.push(fb.clone());
                            }
                        }
                    }
                }
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            walk_failure_paths(lhs, pure, process, fallible_fallbacks)?;
            walk_failure_paths(rhs, pure, process, fallible_fallbacks)?;
        }
        Expr::Call { name, args, .. } => {
            if !pure.contains(name.as_str()) {
                bail!(
                    "Level-1 violation in process '{process}': external transform \
                     '{name}' is invoked as a bare call — external stages must be \
                     pipeline steps carrying @recover or @error"
                );
            }
            for a in args {
                walk_failure_paths(a, pure, process, fallible_fallbacks)?;
            }
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            walk_failure_paths(cond, pure, process, fallible_fallbacks)?;
            walk_failure_paths(then_branch, pure, process, fallible_fallbacks)?;
            walk_failure_paths(else_branch, pure, process, fallible_fallbacks)?;
        }
        Expr::SchemaLit { fields, .. } => {
            for (_, e) in fields {
                walk_failure_paths(e, pure, process, fallible_fallbacks)?;
            }
        }
        Expr::Ident { .. } | Expr::Literal { .. } | Expr::FieldAccess { .. } => {}
    }
    Ok(())
}

/// Best-effort span for a pipeline step: the step's own expression span.
fn step_span_of(step: &crate::frontend::ast::PipeStep) -> crate::frontend::ast::Span {
    match &step.expr {
        Expr::Ident { span, .. }
        | Expr::Call { span, .. }
        | Expr::FieldAccess { span, .. }
        | Expr::Literal { span, .. }
        | Expr::Pipeline { span, .. }
        | Expr::If { span, .. }
        | Expr::SchemaLit { span, .. }
        | Expr::Binary { span, .. } => *span,
    }
}

/// A recovery target must be able to stand in for the stage it recovers.
///
/// `@recover(with: f)` substitutes `f` for a failed stage, so `f` must accept
/// what the stage accepted and produce what the stage produced. Without this
/// the mismatch surfaced as a type error in the GENERATED crate rather than
/// in the source — the compiler accepted a program it could not compile.
pub fn check_recover_signatures(program: &Program) -> Result<()> {
    use crate::analysis::types::type_name;
    use std::collections::BTreeMap;

    let sigs: BTreeMap<&str, (String, String)> = program
        .transforms
        .iter()
        .map(|t| {
            (
                t.name.as_str(),
                (type_name(&t.param_ty), type_name(&t.return_ty)),
            )
        })
        .collect();

    for process in &program.processes {
        for handler in &process.handlers {
            for stmt in &handler.body {
                let expr = match stmt {
                    Stmt::Let { expr, .. }
                    | Stmt::Assign { expr, .. }
                    | Stmt::Send { expr, .. }
                    | Stmt::Expr { expr, .. } => expr,
                };
                walk_recover_sigs(expr, &sigs, &process.name)?;
            }
        }
    }
    Ok(())
}

fn walk_recover_sigs(
    expr: &Expr,
    sigs: &std::collections::BTreeMap<&str, (String, String)>,
    process: &str,
) -> Result<()> {
    match expr {
        Expr::Pipeline { base, steps, .. } => {
            walk_recover_sigs(base, sigs, process)?;
            for step in steps {
                let stage = match &step.expr {
                    Expr::Ident { name, .. } | Expr::Call { name, .. } => Some(name.as_str()),
                    _ => None,
                };
                let Some(stage) = stage else { continue };
                let Some((stage_in, stage_out)) = sigs.get(stage) else {
                    continue;
                };
                for tag in &step.tags {
                    let Tag::Recover { with, span } = tag else {
                        continue;
                    };
                    let fb = match with {
                        Expr::Ident { name, .. } | Expr::Call { name, .. } => name.as_str(),
                        _ => continue,
                    };
                    let Some((fb_in, fb_out)) = sigs.get(fb) else {
                        bail!(
                            "Level-1 violation in process '{process}' at bytes {}..{}: \
                             recovery target '{fb}' for stage '{stage}' is not a declared \
                             transform",
                            span.start,
                            span.end
                        );
                    };
                    if fb_in != stage_in || fb_out != stage_out {
                        bail!(
                            "Level-1 violation in process '{process}' at bytes {}..{}: \
                             recovery target '{fb}' has signature `{fb_in} -> {fb_out}` but \
                             must stand in for stage '{stage}', which is \
                             `{stage_in} -> {stage_out}`",
                            span.start,
                            span.end
                        );
                    }
                }
            }
            Ok(())
        }
        Expr::Binary { lhs, rhs, .. } => {
            walk_recover_sigs(lhs, sigs, process)?;
            walk_recover_sigs(rhs, sigs, process)
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            walk_recover_sigs(cond, sigs, process)?;
            walk_recover_sigs(then_branch, sigs, process)?;
            walk_recover_sigs(else_branch, sigs, process)
        }
        Expr::SchemaLit { fields, .. } => {
            for (_, fe) in fields {
                walk_recover_sigs(fe, sigs, process)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Numeric type agreement in arithmetic and comparisons.
///
/// Sigil does not coerce between `Int` and `Float`: silent widening is how
/// rounding bugs enter financial code, and an un-checked mix here previously
/// produced generated Rust that would not compile — the error surfaced in
/// the output crate instead of in the source. Write `100.0` when you mean a
/// float.
pub fn check_numeric_types(program: &Program) -> Result<()> {
    // The total language type checker owns name resolution, calls, transform
    // returns, schema literals, sends, guards, and route keys. Keep this
    // historical entry point as part of the public API: callers that used it
    // now receive the complete Level-1 type guarantee.
    crate::analysis::typecheck::check_types(program)?;

    use crate::analysis::types::type_name;
    use std::collections::BTreeMap;

    let schemas: BTreeMap<&str, BTreeMap<&str, String>> = program
        .schemas
        .iter()
        .map(|sc| {
            (
                sc.name.as_str(),
                sc.fields
                    .iter()
                    .map(|(f, t)| (f.as_str(), type_name(t)))
                    .collect(),
            )
        })
        .collect();
    let sigs: BTreeMap<&str, String> = program
        .transforms
        .iter()
        .map(|t| (t.name.as_str(), type_name(&t.return_ty)))
        .collect();

    for process in &program.processes {
        let mut base: BTreeMap<String, String> = process
            .states
            .iter()
            .map(|st| (st.name.clone(), type_name(&st.ty)))
            .collect();
        for st in &process.states {
            check_expr_numeric(&st.init, &base, &schemas, &sigs, &process.name)?;
            if let Some(init_ty) = expr_ty(&st.init, &base, &schemas, &sigs) {
                let declared = type_name(&st.ty);
                if init_ty != declared {
                    bail!(
                        "Level-1 violation in process '{}': state '{}' is declared {declared} \
                         but its initializer has type {init_ty}",
                        process.name,
                        st.name
                    );
                }
            }
        }
        for handler in &process.handlers {
            let mut env = base.clone();
            env.insert(handler.msg_name.clone(), type_name(&handler.msg_ty));
            for stmt in &handler.body {
                let expr = match stmt {
                    Stmt::Let { name, expr, .. } => {
                        check_expr_numeric(expr, &env, &schemas, &sigs, &process.name)?;
                        if let Some(t) = expr_ty(expr, &env, &schemas, &sigs) {
                            env.insert(name.clone(), t);
                        }
                        continue;
                    }
                    Stmt::Assign { name, expr, .. } => {
                        check_expr_numeric(expr, &env, &schemas, &sigs, &process.name)?;
                        if let (Some(target_ty), Some(value_ty)) =
                            (env.get(name), expr_ty(expr, &env, &schemas, &sigs))
                        {
                            if target_ty != &value_ty {
                                bail!(
                                    "Level-1 violation in process '{}': assignment to '{}' \
                                     expects {target_ty}, found {value_ty}",
                                    process.name,
                                    name
                                );
                            }
                        }
                        continue;
                    }
                    Stmt::Send { expr, .. } | Stmt::Expr { expr, .. } => expr,
                };
                check_expr_numeric(expr, &env, &schemas, &sigs, &process.name)?;
            }
        }
        base.clear();
    }
    check_spec_numeric_types(program, &schemas)?;
    Ok(())
}

type SchemaMap<'a> =
    std::collections::BTreeMap<&'a str, std::collections::BTreeMap<&'a str, String>>;
type SigMap<'a> = std::collections::BTreeMap<&'a str, String>;

fn expr_ty(
    e: &Expr,
    env: &std::collections::BTreeMap<String, String>,
    schemas: &SchemaMap,
    sigs: &SigMap,
) -> Option<String> {
    use crate::frontend::ast::Literal;
    match e {
        Expr::Literal { value, .. } => Some(
            match value {
                Literal::Int(_) => "Int",
                Literal::Float(_) => "Float",
                Literal::String(_) => "String",
                Literal::Bool(_) => "Bool",
                Literal::DurationMs(_) => "Duration",
            }
            .to_string(),
        ),
        Expr::Ident { name, .. } => env.get(name).cloned(),
        Expr::FieldAccess { base, field, .. } => {
            let bt = env.get(base)?;
            schemas.get(bt.as_str())?.get(field.as_str()).cloned()
        }
        Expr::Call { name, .. } => sigs.get(name.as_str()).cloned(),
        Expr::SchemaLit { name, .. } => Some(name.clone()),
        Expr::Pipeline { base, steps, .. } => {
            let mut cur = expr_ty(base, env, schemas, sigs);
            for step in steps {
                cur = match &step.expr {
                    Expr::Ident { name, .. } | Expr::Call { name, .. } => {
                        sigs.get(name.as_str()).cloned()
                    }
                    _ => None,
                };
            }
            cur
        }
        Expr::If {
            then_branch,
            else_branch,
            ..
        } => expr_ty(then_branch, env, schemas, sigs)
            .or_else(|| expr_ty(else_branch, env, schemas, sigs)),
        Expr::Binary { op, lhs, rhs, .. } => match op {
            BinOp::Le | BinOp::Ge | BinOp::Lt | BinOp::Gt | BinOp::Eq => Some("Bool".into()),
            _ => expr_ty(lhs, env, schemas, sigs).or_else(|| expr_ty(rhs, env, schemas, sigs)),
        },
    }
}

fn check_expr_numeric(
    e: &Expr,
    env: &std::collections::BTreeMap<String, String>,
    schemas: &SchemaMap,
    sigs: &SigMap,
    process: &str,
) -> Result<()> {
    match e {
        Expr::Literal {
            value: Literal::Float(value),
            span,
        } if !value.is_finite() => {
            bail!(
                "Level-1 violation in process '{process}' at bytes {}..{}: \
                 Float literal must be finite",
                span.start,
                span.end
            )
        }
        Expr::Binary { op, lhs, rhs, span } => {
            check_expr_numeric(lhs, env, schemas, sigs, process)?;
            check_expr_numeric(rhs, env, schemas, sigs, process)?;
            let (lt, rt) = (
                expr_ty(lhs, env, schemas, sigs),
                expr_ty(rhs, env, schemas, sigs),
            );
            if let (Some(lt), Some(rt)) = (lt.clone(), rt.clone()) {
                let numeric = |t: &str| t == "Int" || t == "Float";
                let arithmetic = matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div);
                let ordering = matches!(op, BinOp::Le | BinOp::Ge | BinOp::Lt | BinOp::Gt);
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
                // Arithmetic and ordering are defined only on numbers.
                if (arithmetic || ordering) && (!numeric(&lt) || !numeric(&rt)) {
                    bail!(
                        "Level-1 violation in process '{process}' at bytes {}..{}: \
                         `{lt} {op_s} {rt}` — `{op_s}` is defined only on Int and Float",
                        span.start,
                        span.end
                    );
                }
                // Equality needs both sides to be the same type.
                if matches!(op, BinOp::Eq) && lt != rt {
                    bail!(
                        "Level-1 violation in process '{process}' at bytes {}..{}: \
                         cannot compare `{lt}` with `{rt}`",
                        span.start,
                        span.end
                    );
                }
                if numeric(&lt) && numeric(&rt) && lt != rt {
                    bail!(
                        "Level-1 violation in process '{process}' at bytes {}..{}: \
                         `{lt} {op_s} {rt}` mixes numeric types — Sigil does not coerce \
                         between Int and Float. Write the literal as a Float (e.g. `1.0`) \
                         or keep both operands the same type.",
                        span.start,
                        span.end
                    );
                }
            }
            Ok(())
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
            span,
        } => {
            check_expr_numeric(cond, env, schemas, sigs, process)?;
            check_expr_numeric(then_branch, env, schemas, sigs, process)?;
            check_expr_numeric(else_branch, env, schemas, sigs, process)?;
            if let Some(cond_ty) = expr_ty(cond, env, schemas, sigs) {
                if cond_ty != "Bool" {
                    bail!(
                        "Level-1 violation in process '{process}' at bytes {}..{}: \
                         if condition must be Bool, found {cond_ty}",
                        span.start,
                        span.end
                    );
                }
            }
            if let (Some(then_ty), Some(else_ty)) = (
                expr_ty(then_branch, env, schemas, sigs),
                expr_ty(else_branch, env, schemas, sigs),
            ) {
                if then_ty != else_ty {
                    bail!(
                        "Level-1 violation in process '{process}' at bytes {}..{}: \
                         if branches have different types ({then_ty} and {else_ty})",
                        span.start,
                        span.end
                    );
                }
            }
            Ok(())
        }
        Expr::SchemaLit { fields, .. } => {
            for (_, fe) in fields {
                check_expr_numeric(fe, env, schemas, sigs, process)?;
            }
            Ok(())
        }
        Expr::Pipeline { base, .. } => check_expr_numeric(base, env, schemas, sigs, process),
        Expr::Call { args, .. } => {
            for a in args {
                check_expr_numeric(a, env, schemas, sigs, process)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn check_spec_numeric_types(program: &Program, schemas: &SchemaMap<'_>) -> Result<()> {
    fn spec_expr_ty(program: &Program, schemas: &SchemaMap<'_>, expr: &Expr) -> Option<String> {
        match expr {
            Expr::Literal { value, .. } => Some(
                match value {
                    Literal::Int(_) => "Int",
                    Literal::Float(_) => "Float",
                    Literal::String(_) => "String",
                    Literal::Bool(_) => "Bool",
                    Literal::DurationMs(_) => "Duration",
                }
                .to_string(),
            ),
            Expr::Ident { name, .. } if name == "path_timeout_sum" || name == "path_latency" => {
                Some("Duration".into())
            }
            Expr::Ident { name, .. } => program
                .processes
                .iter()
                .flat_map(|process| &process.states)
                .find(|state| state.name == *name)
                .map(|state| crate::analysis::types::type_name(&state.ty)),
            Expr::FieldAccess { base, field, .. } => {
                if let Some(process) = program
                    .processes
                    .iter()
                    .find(|process| process.name == *base)
                {
                    return process
                        .states
                        .iter()
                        .find(|state| state.name == *field)
                        .map(|state| crate::analysis::types::type_name(&state.ty));
                }
                let mut found: Option<String> = None;
                for handler in program
                    .processes
                    .iter()
                    .flat_map(|process| &process.handlers)
                    .filter(|handler| handler.msg_name == *base)
                {
                    let Type::Named(schema_name) = &handler.msg_ty else {
                        continue;
                    };
                    let Some(field_ty) = schemas
                        .get(schema_name.as_str())
                        .and_then(|fields| fields.get(field.as_str()))
                    else {
                        continue;
                    };
                    match &found {
                        None => found = Some(field_ty.clone()),
                        Some(existing) if existing == field_ty => {}
                        Some(_) => return None,
                    }
                }
                found
            }
            Expr::Binary { op, lhs, rhs, .. } => match op {
                BinOp::Le | BinOp::Ge | BinOp::Lt | BinOp::Gt | BinOp::Eq => Some("Bool".into()),
                _ => spec_expr_ty(program, schemas, lhs)
                    .or_else(|| spec_expr_ty(program, schemas, rhs)),
            },
            Expr::If {
                then_branch,
                else_branch,
                ..
            } => spec_expr_ty(program, schemas, then_branch)
                .or_else(|| spec_expr_ty(program, schemas, else_branch)),
            _ => None,
        }
    }

    fn walk(program: &Program, schemas: &SchemaMap<'_>, expr: &Expr, spec: &str) -> Result<()> {
        match expr {
            Expr::Literal {
                value: Literal::Float(value),
                span,
            } if !value.is_finite() => bail!(
                "Level-1 violation in spec '{spec}' at bytes {}..{}: Float literal must be finite",
                span.start,
                span.end
            ),
            Expr::Binary { op, lhs, rhs, span } => {
                walk(program, schemas, lhs, spec)?;
                walk(program, schemas, rhs, spec)?;
                let (Some(lhs_ty), Some(rhs_ty)) = (
                    spec_expr_ty(program, schemas, lhs),
                    spec_expr_ty(program, schemas, rhs),
                ) else {
                    return Ok(());
                };
                let numeric = |ty: &str| ty == "Int" || ty == "Float";
                let arithmetic = matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div);
                let ordering = matches!(op, BinOp::Le | BinOp::Ge | BinOp::Lt | BinOp::Gt);
                if (arithmetic || ordering)
                    && (numeric(&lhs_ty) || numeric(&rhs_ty))
                    && lhs_ty != rhs_ty
                {
                    bail!(
                        "Level-1 violation in spec '{spec}' at bytes {}..{}: \
                         numeric operands have different types ({lhs_ty} and {rhs_ty}); \
                         Sigil never coerces Int and Float proof operands",
                        span.start,
                        span.end
                    );
                }
                if matches!(op, BinOp::Eq) && lhs_ty != rhs_ty {
                    bail!(
                        "Level-1 violation in spec '{spec}' at bytes {}..{}: \
                         cannot compare {lhs_ty} with {rhs_ty}",
                        span.start,
                        span.end
                    );
                }
                Ok(())
            }
            Expr::If {
                cond,
                then_branch,
                else_branch,
                ..
            } => {
                walk(program, schemas, cond, spec)?;
                walk(program, schemas, then_branch, spec)?;
                walk(program, schemas, else_branch, spec)
            }
            Expr::SchemaLit { fields, .. } => {
                for (_, field) in fields {
                    walk(program, schemas, field, spec)?;
                }
                Ok(())
            }
            Expr::Call { args, .. } => {
                for argument in args {
                    walk(program, schemas, argument, spec)?;
                }
                Ok(())
            }
            Expr::Pipeline { base, steps, .. } => {
                walk(program, schemas, base, spec)?;
                for step in steps {
                    walk(program, schemas, &step.expr, spec)?;
                }
                Ok(())
            }
            Expr::FieldAccess { base, field, span } => {
                if let Some(process) = program
                    .processes
                    .iter()
                    .find(|process| process.name == *base)
                {
                    if process.states.iter().any(|state| state.name == *field) {
                        return Ok(());
                    }
                    bail!(
                        "Level-1 violation in spec '{spec}' at bytes {}..{}: \
                         process '{base}' has no state '{field}'",
                        span.start,
                        span.end
                    );
                }
                let handlers: Vec<_> = program
                    .processes
                    .iter()
                    .flat_map(|process| &process.handlers)
                    .filter(|handler| handler.msg_name == *base)
                    .collect();
                if handlers.is_empty() {
                    bail!(
                        "Level-1 violation in spec '{spec}' at bytes {}..{}: \
                         unknown message or process '{base}'",
                        span.start,
                        span.end
                    );
                }
                for handler in handlers {
                    let Type::Named(schema_name) = &handler.msg_ty else {
                        bail!(
                            "Level-1 violation in spec '{spec}': '{base}' does not have \
                             a schema field '{field}'"
                        );
                    };
                    let present = schemas
                        .get(schema_name.as_str())
                        .is_some_and(|fields| fields.contains_key(field.as_str()));
                    if !present {
                        bail!(
                            "Level-1 violation in spec '{spec}' at bytes {}..{}: \
                             schema '{schema_name}' has no field '{field}'",
                            span.start,
                            span.end
                        );
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    for spec in &program.specs {
        for item in &spec.items {
            match item {
                SpecItem::Require { expr, .. } | SpecItem::Hold { expr, .. } => {
                    walk(program, schemas, expr, &spec.name)?;
                }
                SpecItem::Extinct { .. } => {}
            }
        }
    }
    Ok(())
}

/// Well-formedness of multi-handler processes.
///
/// Two obligations, both of which produce broken or ambiguous programs if
/// violated:
///   - handler message NAMES must be unique in a process (they name the
///     dispatch variant and scope the Level-3 input guards);
///   - handler message TYPES must be unique in a process (`send` resolves
///     the destination handler by type, so duplicates are ambiguous).
pub fn check_handler_wellformedness(program: &Program) -> Result<()> {
    use crate::analysis::types::type_name;
    use std::collections::{BTreeMap, BTreeSet};

    fn is_rust_keyword(name: &str) -> bool {
        matches!(
            name,
            "as" | "async"
                | "await"
                | "break"
                | "const"
                | "continue"
                | "crate"
                | "dyn"
                | "else"
                | "enum"
                | "extern"
                | "false"
                | "fn"
                | "for"
                | "if"
                | "impl"
                | "in"
                | "let"
                | "loop"
                | "match"
                | "mod"
                | "move"
                | "mut"
                | "pub"
                | "ref"
                | "return"
                | "self"
                | "Self"
                | "static"
                | "struct"
                | "super"
                | "trait"
                | "true"
                | "type"
                | "unsafe"
                | "use"
                | "where"
                | "while"
                | "abstract"
                | "become"
                | "box"
                | "do"
                | "final"
                | "macro"
                | "override"
                | "priv"
                | "try"
                | "typeof"
                | "union"
                | "unsized"
                | "virtual"
                | "yield"
        )
    }

    fn validate_ident(name: &str, kind: &str) -> Result<()> {
        if is_rust_keyword(name) {
            bail!(
                "Level-1 violation: {kind} name '{name}' is a Rust keyword and cannot be \
                 emitted as a Rust identifier"
            );
        }
        Ok(())
    }

    fn reject_duplicates<'a>(names: impl IntoIterator<Item = &'a str>, kind: &str) -> Result<()> {
        let mut seen = BTreeSet::new();
        for name in names {
            if !seen.insert(name) {
                bail!("Level-1 violation: duplicate {kind} name '{name}'");
            }
        }
        Ok(())
    }

    reject_duplicates(
        program.schemas.iter().map(|schema| schema.name.as_str()),
        "schema",
    )?;
    reject_duplicates(
        program
            .processes
            .iter()
            .map(|process| process.name.as_str()),
        "process",
    )?;
    reject_duplicates(
        program
            .transforms
            .iter()
            .map(|transform| transform.name.as_str()),
        "transform",
    )?;
    reject_duplicates(program.specs.iter().map(|spec| spec.name.as_str()), "spec")?;
    reject_duplicates(
        program
            .extern_crates
            .iter()
            .map(|dependency| dependency.name.as_str()),
        "extern crate",
    )?;
    for dependency in &program.extern_crates {
        validate_ident(&dependency.name, "extern crate")?;
        match &dependency.source {
            crate::frontend::ast::CrateSource::Version(requirement) => {
                semver::VersionReq::parse(requirement).with_context(|| {
                    format!(
                        "Level-1 violation: extern crate '{}' has invalid version requirement \
                         '{requirement}'",
                        dependency.name
                    )
                })?;
            }
            crate::frontend::ast::CrateSource::Path(path) => {
                if path.is_empty() || path.chars().any(char::is_control) {
                    bail!(
                        "Level-1 violation: extern crate '{}' has an empty or control-character \
                         path",
                        dependency.name
                    );
                }
            }
        }
    }

    // All declarations below become Rust items. Check the generated type
    // namespace too: `P` also emits `PHandle` and, for multi-handler
    // processes, `PMsg`.
    let mut generated_types: BTreeMap<String, String> = BTreeMap::new();
    let mut add_generated_type = |name: String, origin: String| -> Result<()> {
        if let Some(previous) = generated_types.insert(name.clone(), origin.clone()) {
            bail!(
                "Level-1 violation: generated Rust type name '{name}' collides between \
                 {previous} and {origin}"
            );
        }
        Ok(())
    };
    for schema in &program.schemas {
        validate_ident(&schema.name, "schema")?;
        if matches!(
            schema.name.as_str(),
            "Int" | "Float" | "String" | "Bool" | "UUID" | "Bytes" | "Duration" | "Result"
        ) {
            bail!(
                "Level-1 violation: schema name '{}' collides with a built-in/generated type",
                schema.name
            );
        }
        add_generated_type(schema.name.clone(), format!("schema '{}'", schema.name))?;
        reject_duplicates(
            schema.fields.iter().map(|(name, _)| name.as_str()),
            &format!("field in schema '{}'", schema.name),
        )?;
        for (field, _) in &schema.fields {
            validate_ident(field, &format!("field in schema '{}'", schema.name))?;
        }
    }
    for process in &program.processes {
        validate_ident(&process.name, "process")?;
        let mut generated_names = vec![process.name.clone(), format!("{}Handle", process.name)];
        if process.handlers.len() > 1 {
            generated_names.push(format!("{}Msg", process.name));
        }
        for generated in generated_names {
            add_generated_type(generated, format!("process '{}'", process.name))?;
        }
    }

    // Demo variables and outbox fields are derived by lowercasing process
    // names, so case-only distinctions are not representable.
    reject_duplicates(
        program
            .processes
            .iter()
            .map(|process| process.name.to_lowercase())
            .collect::<Vec<_>>()
            .iter()
            .map(String::as_str),
        "case-insensitive process",
    )?;

    let schemas: BTreeSet<&str> = program
        .schemas
        .iter()
        .map(|schema| schema.name.as_str())
        .collect();
    let validate_type = |ty: &crate::frontend::ast::Type, context: &str| -> Result<()> {
        if let crate::frontend::ast::Type::Named(name) = ty {
            if !schemas.contains(name.as_str()) {
                bail!("Level-1 violation: {context} uses unknown schema type '{name}'");
            }
        }
        Ok(())
    };

    for schema in &program.schemas {
        for (field, ty) in &schema.fields {
            validate_type(ty, &format!("field '{}.{}'", schema.name, field))?;
        }
    }
    for transform in &program.transforms {
        validate_ident(&transform.name, "transform")?;
        validate_ident(
            &transform.param,
            &format!("parameter of transform '{}'", transform.name),
        )?;
        if transform.name == "timeout" {
            bail!(
                "Level-1 violation: transform name 'timeout' collides with generated runtime support"
            );
        }
        validate_type(
            &transform.param_ty,
            &format!("parameter of transform '{}'", transform.name),
        )?;
        validate_type(
            &transform.return_ty,
            &format!("return type of transform '{}'", transform.name),
        )?;
        for statement in &transform.body {
            if let Stmt::Let { name, .. } = statement {
                validate_ident(
                    name,
                    &format!("local binding in transform '{}'", transform.name),
                )?;
            }
        }
    }

    // Unqualified Level-3 holds resolve state names globally. Requiring
    // uniqueness prevents a proof from silently selecting the first owner.
    reject_duplicates(
        program
            .processes
            .iter()
            .flat_map(|process| process.states.iter().map(|state| state.name.as_str())),
        "state across the compilation unit",
    )?;

    for process in &program.processes {
        if process.handlers.is_empty() {
            bail!(
                "Level-1 violation: process '{}' declares no handlers — it can never \
                 receive a message",
                process.name
            );
        }
        let mut by_name: BTreeMap<&str, usize> = BTreeMap::new();
        let mut by_type: BTreeMap<String, usize> = BTreeMap::new();
        let targets: BTreeSet<String> = process
            .handlers
            .iter()
            .flat_map(|handler| handler.body.iter())
            .filter_map(|stmt| match stmt {
                Stmt::Send { target, .. } => Some(target.to_lowercase()),
                _ => None,
            })
            .collect();
        let reserved_states: BTreeSet<String> = ["__shed".to_string(), "__telemetry".to_string()]
            .into_iter()
            .chain(targets.iter().map(|target| format!("{target}_out")))
            .collect();
        reject_duplicates(
            process.states.iter().map(|state| state.name.as_str()),
            &format!("state in process '{}'", process.name),
        )?;
        for state in &process.states {
            validate_ident(&state.name, &format!("state in process '{}'", process.name))?;
            validate_type(
                &state.ty,
                &format!("state '{}.{}'", process.name, state.name),
            )?;
            if reserved_states.contains(&state.name) {
                bail!(
                    "Level-1 violation in process '{}': state name '{}' collides with \
                     generated actor bookkeeping",
                    process.name,
                    state.name
                );
            }
        }
        for h in &process.handlers {
            validate_ident(
                &h.msg_name,
                &format!("handler message in process '{}'", process.name),
            )?;
            validate_type(
                &h.msg_ty,
                &format!("handler '{}' in process '{}'", h.msg_name, process.name),
            )?;
            *by_name.entry(h.msg_name.as_str()).or_insert(0) += 1;
            *by_type.entry(type_name(&h.msg_ty)).or_insert(0) += 1;
            for statement in &h.body {
                if let Stmt::Let { name, .. } = statement {
                    validate_ident(
                        name,
                        &format!(
                            "local binding in handler '{}' of process '{}'",
                            h.msg_name, process.name
                        ),
                    )?;
                }
            }
        }
        for (name, n) in &by_name {
            if *n > 1 {
                bail!(
                    "Level-1 violation in process '{}' at bytes {}..{}: {n} handlers bind \
                     the message name '{name}' — handler message names must be unique \
                     within a process (they name the dispatch variant and scope input \
                     guards)",
                    process.name,
                    process.span.start,
                    process.span.end
                );
            }
        }
        // Dispatch variants are UpperCamelCase versions of the message names;
        // distinct names must stay distinct after that transformation.
        let mut by_variant: BTreeMap<String, Vec<&str>> = BTreeMap::new();
        for h in &process.handlers {
            by_variant
                .entry(crate::backend::codegen::variant_name(&h.msg_name))
                .or_default()
                .push(h.msg_name.as_str());
        }
        for (variant, names) in &by_variant {
            if names.len() > 1 {
                bail!(
                    "Level-1 violation in process '{}' at bytes {}..{}: handler names {} all \
                     map to the dispatch variant `{variant}` — rename so they stay distinct",
                    process.name,
                    process.span.start,
                    process.span.end,
                    names
                        .iter()
                        .map(|n| format!("'{n}'"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        for (ty, n) in &by_type {
            if *n > 1 {
                bail!(
                    "Level-1 violation in process '{}' at bytes {}..{}: {n} handlers accept \
                     type `{ty}` — `send` resolves the destination handler by message type, \
                     so each type may appear at most once per process",
                    process.name,
                    process.span.start,
                    process.span.end
                );
            }
        }
    }
    Ok(())
}

/// Pure transforms are the language's infallibility anchor: their bodies may
/// not invoke external transforms, directly or via pipelines.
pub fn check_transform_purity(program: &Program) -> Result<()> {
    use std::collections::BTreeSet;
    let pure: BTreeSet<&str> = program
        .transforms
        .iter()
        .filter(|t| !t.body.is_empty())
        .map(|t| t.name.as_str())
        .collect();
    for t in program.transforms.iter().filter(|t| !t.body.is_empty()) {
        for stmt in &t.body {
            let expr = match stmt {
                Stmt::Let { expr, .. }
                | Stmt::Assign { expr, .. }
                | Stmt::Send { expr, .. }
                | Stmt::Expr { expr, .. } => expr,
            };
            walk_purity(expr, &pure, &t.name)?;
        }
    }
    Ok(())
}

fn walk_purity(expr: &Expr, pure: &std::collections::BTreeSet<&str>, owner: &str) -> Result<()> {
    match expr {
        Expr::Call { name, args, .. } => {
            if !pure.contains(name.as_str()) {
                bail!(
                    "Level-1 violation: pure transform '{owner}' calls external \
                     transform '{name}' — pure bodies are the infallibility anchor \
                     and may only call other pure transforms"
                );
            }
            for a in args {
                walk_purity(a, pure, owner)?;
            }
            Ok(())
        }
        Expr::Pipeline { base, steps, .. } => {
            walk_purity(base, pure, owner)?;
            for step in steps {
                if let Expr::Ident { name, .. } | Expr::Call { name, .. } = &step.expr {
                    if !pure.contains(name.as_str()) {
                        bail!(
                            "Level-1 violation: pure transform '{owner}' pipelines \
                             into external transform '{name}'"
                        );
                    }
                }
            }
            Ok(())
        }
        Expr::Binary { lhs, rhs, .. } => {
            walk_purity(lhs, pure, owner)?;
            walk_purity(rhs, pure, owner)
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            walk_purity(cond, pure, owner)?;
            walk_purity(then_branch, pure, owner)?;
            walk_purity(else_branch, pure, owner)
        }
        Expr::SchemaLit { fields, .. } => {
            for (_, e) in fields {
                walk_purity(e, pure, owner)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod declaration_tests {
    use super::*;
    use crate::frontend::ast::parse;

    fn rejection(source: &str) -> String {
        let program = parse(source).expect("test source parses");
        check_handler_wellformedness(&program)
            .expect_err("source must be rejected")
            .to_string()
    }

    fn numeric_rejection(source: &str) -> String {
        let program = parse(source).expect("test source parses");
        check_numeric_types(&program)
            .expect_err("source must be rejected")
            .to_string()
    }

    #[test]
    fn rejects_names_that_would_break_generated_rust() {
        let duplicate = r#"
schema M { value: Int }
process P { on m: M {} }
process P { on other: M {} }
"#;
        assert!(rejection(duplicate).contains("duplicate process"));

        let keyword = r#"
schema M { type: Int }
process P { on m: M {} }
"#;
        assert!(rejection(keyword).contains("Rust keyword"));

        let collision = r#"
schema PHandle { value: Int }
process P { on m: PHandle {} }
"#;
        assert!(rejection(collision).contains("generated Rust type"));
    }

    #[test]
    fn rejects_unknown_schema_types_and_ambiguous_state_names() {
        let unknown = "process P { on m: Missing {} }";
        assert!(rejection(unknown).contains("unknown schema type"));

        let ambiguous = r#"
schema M { value: Int }
process A { state count: Int = 0 on m: M {} }
process B { state count: Int = 0 on m: M {} }
"#;
        assert!(rejection(ambiguous).contains("state across the compilation unit"));
    }

    #[test]
    fn rejects_actor_bookkeeping_collisions() {
        let source = r#"
schema M { value: Int }
process A {
  state b_out: Int = 0
  on m: M { send m to B }
}
process B { on m: M {} }
"#;
        assert!(rejection(source).contains("actor bookkeeping"));
    }

    #[test]
    fn rejects_invalid_dependency_metadata() {
        let bad_version = r#"
extern crate dep = "not a version requirement"
schema M { value: Int }
process P { on m: M {} }
"#;
        assert!(rejection(bad_version).contains("invalid version requirement"));

        let control_path = "extern crate dep = path \"../ok\ninjected\"\n\
schema M { value: Int }\nprocess P { on m: M {} }";
        assert!(rejection(control_path).contains("control-character"));
    }

    #[test]
    fn rejects_numeric_type_gaps_before_proof_or_codegen() {
        let bad_init = r#"
schema M { value: Int }
process P {
  state count: Int = 0.0
  on m: M {}
}
"#;
        assert!(numeric_rejection(bad_init).contains("initializer expects Int, found Float"));

        let bad_assign = r#"
schema M { value: Int }
process P {
  state count: Int = 0
  on m: M { count := 1.0 }
}
"#;
        assert!(numeric_rejection(bad_assign).contains("expects Int, found Float"));

        let bad_spec = r#"
schema M { value: Int }
process P {
  state count: Int = 0
  on m: M { count := count + m.value }
}
spec S {
  require m.value >= 0.0
  hold count >= 0
}
"#;
        assert!(numeric_rejection(bad_spec).contains("mixes numeric types"));

        let missing_field = r#"
schema M { value: Int }
process P {
  state count: Int = 0
  on m: M { count := count + m.value }
}
spec S {
  require m.missing >= 0
  hold count >= 0
}
"#;
        assert!(numeric_rejection(missing_field).contains("has no field 'missing'"));

        let non_finite = format!(
            "schema M {{ value: Int }}\nprocess P {{ state x: Float = {}.0 on m: M {{}} }}",
            "9".repeat(400)
        );
        assert!(numeric_rejection(&non_finite).contains("non-finite Float literal"));
    }
}
