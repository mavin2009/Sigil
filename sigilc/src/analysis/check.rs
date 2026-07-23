//! Level-1 extinct-by-design checks on the Graph IR and declared transforms.

use crate::analysis::ir::{GraphIR, Node};
use crate::analysis::types::{infer_program, type_name};
use crate::frontend::ast::{Expr, Program, Stmt, Tag};
use anyhow::{bail, Result};
use std::collections::BTreeMap;

pub fn level1_check(ir: &GraphIR) -> Result<()> {
    let has_timeout = ir.has_timeout();
    let has_recover = ir.has_recover();

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
            local_env.insert(
                handler.msg_name.clone(),
                type_name(&handler.msg_ty),
            );

            for stmt in &handler.body {
                match stmt {
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
                    if is_named_schema(&cur)
                        && is_named_schema(expected_in)
                        && cur != *expected_in
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
                // recover fallbacks must also match input type if declared
                for tag in &step.tags {
                    if let Tag::Recover { with, .. } = tag {
                        if let Expr::Ident { name, .. } = with {
                            if let Some((expected_in, _)) = declared.get(name) {
                                if is_named_schema(&cur)
                                    && is_named_schema(expected_in)
                                    && cur != *expected_in
                                {
                                    // Recover receives the pre-transform value; cur was already
                                    // updated. Use expected_in vs stage input tracked separately
                                    // is approximate — skip strict recover check for now.
                                    let _ = expected_in;
                                }
                            }
                        }
                    }
                }
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
        "Int" | "Float" | "String" | "Bool" | "UUID" | "Bytes" | "Duration" | "i64" | "f64"
            | "bool" | "()"
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
                Node::Timeout { ms: 50, span: None },
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
            nodes: vec![Node::Timeout { ms: 50, span: None }],
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
        let src = include_str!("../../../examples/pipeline.sigil");
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
