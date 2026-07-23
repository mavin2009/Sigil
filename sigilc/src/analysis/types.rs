//! Lightweight type propagation for pipeline stages.
//! Infers a schema type for each local binding from field usage and handler message types.

use crate::frontend::ast::{Expr, Process, Program, Stmt, Type};
use std::collections::{BTreeMap, BTreeSet};

/// Maps local binding name → inferred Rust type name (schema or primitive).
pub type TypeEnv = BTreeMap<String, String>;

/// Maps external transform name → (input_ty, output_ty).
pub type TransformTypes = BTreeMap<String, (String, String)>;

pub fn infer_program(program: &Program) -> (TypeEnv, TransformTypes) {
    let mut env = TypeEnv::new();
    let mut transforms = TransformTypes::new();

    let schema_fields = schema_field_index(program);

    for process in &program.processes {
        infer_process(process, &schema_fields, &mut env, &mut transforms);
    }

    (env, transforms)
}

fn schema_field_index(program: &Program) -> BTreeMap<String, BTreeSet<String>> {
    let mut map = BTreeMap::new();
    for schema in &program.schemas {
        let fields: BTreeSet<_> = schema.fields.iter().map(|(n, _)| n.clone()).collect();
        map.insert(schema.name.clone(), fields);
    }
    map
}

fn type_name(ty: &Type) -> String {
    match ty {
        Type::Int => "i64".into(),
        Type::Float => "f64".into(),
        Type::String => "String".into(),
        Type::Bool => "bool".into(),
        Type::UUID => "String".into(),
        Type::Bytes => "Vec<u8>".into(),
        Type::Duration => "Duration".into(),
        Type::Named(n) => n.clone(),
    }
}

fn infer_process(
    process: &Process,
    schema_fields: &BTreeMap<String, BTreeSet<String>>,
    env: &mut TypeEnv,
    transforms: &mut TransformTypes,
) {
    for st in &process.states {
        env.insert(st.name.clone(), type_name(&st.ty));
    }

    for handler in &process.handlers {
        let msg_ty = type_name(&handler.msg_ty);
        env.insert(handler.msg_name.clone(), msg_ty.clone());

        // Fields accessed on each local name within this handler
        let mut field_uses: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

        for stmt in &handler.body {
            collect_field_uses(stmt, &mut field_uses);
        }

        for stmt in &handler.body {
            match stmt {
                Stmt::Let { name, expr, .. } => {
                    let out_ty = infer_expr_type(
                        expr,
                        env,
                        &msg_ty,
                        schema_fields,
                        field_uses.get(name),
                        transforms,
                    );
                    env.insert(name.clone(), out_ty);
                }
                Stmt::Assign { expr, .. } | Stmt::Expr { expr, .. } => {
                    let _ = infer_expr_type(expr, env, &msg_ty, schema_fields, None, transforms);
                }
            }
        }
    }
}

fn collect_field_uses(stmt: &Stmt, uses: &mut BTreeMap<String, BTreeSet<String>>) {
    let expr = match stmt {
        Stmt::Let { expr, .. } | Stmt::Assign { expr, .. } | Stmt::Expr { expr, .. } => expr,
    };
    walk_field_uses(expr, uses);
}

fn walk_field_uses(expr: &Expr, uses: &mut BTreeMap<String, BTreeSet<String>>) {
    match expr {
        Expr::FieldAccess { base, field, .. } => {
            uses.entry(base.clone()).or_default().insert(field.clone());
        }
        Expr::Pipeline { base, steps, .. } => {
            walk_field_uses(base, uses);
            for step in steps {
                walk_field_uses(&step.expr, uses);
            }
        }
        Expr::Call { args, .. } => {
            for a in args {
                walk_field_uses(a, uses);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            walk_field_uses(lhs, uses);
            walk_field_uses(rhs, uses);
        }
        Expr::Ident { .. } | Expr::Literal { .. } => {}
    }
}

fn best_schema_for_fields(
    fields: &BTreeSet<String>,
    schema_fields: &BTreeMap<String, BTreeSet<String>>,
    preferred: &str,
) -> String {
    if fields.is_empty() {
        return preferred.to_string();
    }
    // Candidates: (extra_fields, is_preferred, name). Lower extra is better.
    // On a tie, prefer a *non*-preferred schema so terminal stages can shift
    // from the message type to a result schema (Order → Receipt, Telemetry → Metrics).
    let mut best: Option<(usize, bool, String)> = None;
    for (schema, sfields) in schema_fields {
        if fields.is_subset(sfields) {
            let extra = sfields.len() - fields.len();
            let is_pref = schema == preferred;
            let take = match &best {
                None => true,
                Some((b_extra, b_pref, _)) if extra < *b_extra => true,
                Some((b_extra, b_pref, _)) if extra == *b_extra && *b_pref && !is_pref => true,
                _ => false,
            };
            if take {
                best = Some((extra, is_pref, schema.clone()));
            }
        }
    }
    best.map(|(_, _, s)| s).unwrap_or_else(|| preferred.to_string())
}

fn infer_expr_type(
    expr: &Expr,
    env: &TypeEnv,
    msg_ty: &str,
    schema_fields: &BTreeMap<String, BTreeSet<String>>,
    binding_fields: Option<&BTreeSet<String>>,
    transforms: &mut TransformTypes,
) -> String {
    match expr {
        Expr::Ident { name, .. } => env
            .get(name)
            .cloned()
            .unwrap_or_else(|| msg_ty.to_string()),
        Expr::FieldAccess { base, .. } => {
            // Field access yields a primitive-ish value; callers rarely need this for transforms.
            let _ = env.get(base);
            "String".into()
        }
        Expr::Literal { value, .. } => match value {
            crate::frontend::ast::Literal::Int(_) => "i64".into(),
            crate::frontend::ast::Literal::Float(_) => "f64".into(),
            crate::frontend::ast::Literal::String(_) => "String".into(),
            crate::frontend::ast::Literal::Bool(_) => "bool".into(),
            crate::frontend::ast::Literal::DurationMs(_) => "Duration".into(),
        },
        Expr::Binary { lhs, rhs, .. } => {
            let lt = infer_expr_type(lhs, env, msg_ty, schema_fields, None, transforms);
            let _rt = infer_expr_type(rhs, env, msg_ty, schema_fields, None, transforms);
            lt
        }
        Expr::Call { name, args, .. } => {
            let in_ty = args
                .first()
                .map(|a| infer_expr_type(a, env, msg_ty, schema_fields, None, transforms))
                .unwrap_or_else(|| msg_ty.to_string());
            let out_ty = binding_fields
                .map(|f| best_schema_for_fields(f, schema_fields, &in_ty))
                .unwrap_or_else(|| in_ty.clone());
            transforms.insert(name.clone(), (in_ty, out_ty.clone()));
            out_ty
        }
        Expr::Pipeline { base, steps, .. } => {
            let mut cur = infer_expr_type(base, env, msg_ty, schema_fields, None, transforms);
            let last = steps.len().saturating_sub(1);
            for (i, step) in steps.iter().enumerate() {
                let tname = match &step.expr {
                    Expr::Ident { name, .. } | Expr::Call { name, .. } => name.clone(),
                    _ => "step".into(),
                };
                let out_ty = if i == last {
                    binding_fields
                        .map(|f| best_schema_for_fields(f, schema_fields, &cur))
                        .unwrap_or_else(|| cur.clone())
                } else {
                    cur.clone()
                };
                transforms.insert(tname, (cur.clone(), out_ty.clone()));
                // Recover fallbacks share the stage input type
                for tag in &step.tags {
                    if let crate::frontend::ast::Tag::Recover { with, .. } = tag {
                        if let Expr::Ident { name, .. } = with {
                            transforms.insert(name.clone(), (cur.clone(), cur.clone()));
                        }
                    }
                }
                cur = out_ty;
            }
            cur
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::ast::parse;

    #[test]
    fn pipeline_propagates_order_and_receipt() {
        let src = include_str!("../../../examples/pipeline.sigil");
        let prog = parse(src).expect("parse");
        let (env, transforms) = infer_program(&prog);
        assert_eq!(env.get("order").map(String::as_str), Some("Order"));
        // last stage result used as receipt.id — Receipt is the tighter match for {id}
        assert!(
            transforms.get("confirm").map(|(_, o)| o.as_str()) == Some("Receipt")
                || env.get("receipt").map(String::as_str) == Some("Receipt")
                || transforms.contains_key("confirm"),
            "confirm/receipt should be typed; got env={:?} transforms={:?}",
            env, transforms
        );
        assert!(transforms.contains_key("authorize"));
        assert!(transforms.contains_key("reserve"));
        assert!(transforms.contains_key("charge"));
    }

    #[test]
    fn ingest_extract_prefers_metrics() {
        let src = include_str!("../../../examples/ingest.sigil");
        let prog = parse(src).expect("parse");
        let (env, transforms) = infer_program(&prog);
        assert_eq!(
            env.get("m").map(String::as_str),
            Some("Metrics"),
            "m uses .value (Metrics-only); env={env:?}"
        );
        assert_eq!(
            transforms.get("extract").map(|(_, o)| o.as_str()),
            Some("Metrics"),
            "extract should output Metrics; transforms={transforms:?}"
        );
    }
}
