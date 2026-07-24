//! Total static type and local-name checking for executable Sigil programs.
//!
//! This pass is deliberately independent of Rust code generation. If it
//! accepts a Level-1 program, every source expression has exactly one Sigil
//! type and every generated identifier has a resolved declaration.

use crate::frontend::ast::{
    Backpressure, BinOp, BindKind, Cancellation, Expr, Idempotency, Literal, PipeStep, Program,
    Route, SideEffect, SpecItem, Stmt, Tag, Type,
};
use anyhow::{bail, Result};
use std::collections::{BTreeMap, BTreeSet};

type Env = BTreeMap<String, Type>;
type Schemas<'a> = BTreeMap<&'a str, BTreeMap<&'a str, &'a Type>>;
type Transforms<'a> = BTreeMap<&'a str, (&'a Type, &'a Type)>;

fn display_type(ty: &Type) -> &str {
    match ty {
        Type::Int => "Int",
        Type::Float => "Float",
        Type::String => "String",
        Type::Bool => "Bool",
        Type::UUID => "UUID",
        Type::Bytes => "Bytes",
        Type::Duration => "Duration",
        Type::Named(name) => name,
    }
}

fn literal_type(value: &Literal) -> Type {
    match value {
        Literal::Int(_) => Type::Int,
        Literal::Float(_) => Type::Float,
        Literal::String(_) => Type::String,
        Literal::Bool(_) => Type::Bool,
        Literal::DurationMs(_) => Type::Duration,
    }
}

fn expect_type(actual: &Type, expected: &Type, context: &str) -> Result<()> {
    if actual != expected {
        bail!(
            "Level-1 type violation: {context} expects {}, found {}",
            display_type(expected),
            display_type(actual)
        );
    }
    Ok(())
}

fn is_numeric(ty: &Type) -> bool {
    matches!(ty, Type::Int | Type::Float)
}

fn check_step<'a>(
    step: &PipeStep,
    input: Type,
    env: &Env,
    schemas: &Schemas<'a>,
    transforms: &Transforms<'a>,
    context: &str,
) -> Result<Type> {
    let (name, args) = match &step.expr {
        Expr::Ident { name, .. } => (name.as_str(), &[][..]),
        Expr::Call { name, args, .. } => (name.as_str(), args.as_slice()),
        _ => {
            bail!("Level-1 type violation: {context} pipeline stage must name a declared transform")
        }
    };
    if !args.is_empty() {
        bail!(
            "Level-1 type violation: {context} pipeline stage '{name}' receives its one \
             argument from the pipeline and cannot declare additional call arguments"
        );
    }
    let Some((param, output)) = transforms.get(name) else {
        bail!("Level-1 type violation: {context} calls unknown transform '{name}'");
    };
    expect_type(
        &input,
        param,
        &format!("{context} input to transform '{name}'"),
    )?;

    for tag in &step.tags {
        match tag {
            Tag::Timeout { expr, .. } => {
                let ty = infer_expr(expr, env, schemas, transforms, context)?;
                expect_type(&ty, &Type::Duration, &format!("{context} @timeout"))?;
            }
            Tag::Retry { expr, .. } => {
                let ty = infer_expr(expr, env, schemas, transforms, context)?;
                expect_type(&ty, &Type::Int, &format!("{context} @retry"))?;
            }
            Tag::Recover { with, .. } => {
                let (fallback, fallback_args) = match with {
                    Expr::Ident { name, .. } => (name.as_str(), &[][..]),
                    Expr::Call { name, args, .. } => (name.as_str(), args.as_slice()),
                    _ => bail!(
                        "Level-1 type violation: {context} @recover target must name a \
                         declared transform"
                    ),
                };
                if !fallback_args.is_empty() {
                    bail!(
                        "Level-1 type violation: {context} recovery transform '{fallback}' \
                         receives the failed stage input automatically"
                    );
                }
                let Some((fallback_in, fallback_out)) = transforms.get(fallback) else {
                    bail!(
                        "Level-1 type violation: {context} names unknown recovery transform \
                         '{fallback}'"
                    );
                };
                expect_type(
                    fallback_in,
                    param,
                    &format!("{context} recovery input for '{fallback}'"),
                )?;
                expect_type(
                    fallback_out,
                    output,
                    &format!("{context} recovery output for '{fallback}'"),
                )?;
            }
            Tag::Error { .. } => {}
        }
    }
    Ok((*output).clone())
}

fn infer_expr<'a>(
    expr: &Expr,
    env: &Env,
    schemas: &Schemas<'a>,
    transforms: &Transforms<'a>,
    context: &str,
) -> Result<Type> {
    match expr {
        Expr::Literal { value, span } => {
            if matches!(value, Literal::Float(value) if !value.is_finite()) {
                bail!(
                    "Level-1 type violation: {context} has a non-finite Float literal at \
                     bytes {}..{}",
                    span.start,
                    span.end
                );
            }
            Ok(literal_type(value))
        }
        Expr::Ident { name, span } => env.get(name).cloned().ok_or_else(|| {
            anyhow::anyhow!(
                "Level-1 name violation: {context} reads unknown name '{name}' at bytes {}..{}",
                span.start,
                span.end
            )
        }),
        Expr::FieldAccess { base, field, span } => {
            let Some(base_ty) = env.get(base) else {
                bail!(
                    "Level-1 name violation: {context} reads field '{base}.{field}' from an \
                     unknown name at bytes {}..{}",
                    span.start,
                    span.end
                );
            };
            let Type::Named(schema) = base_ty else {
                bail!(
                    "Level-1 type violation: {context} reads field '{field}' from {}, which \
                     is not a schema",
                    display_type(base_ty)
                );
            };
            schemas
                .get(schema.as_str())
                .and_then(|fields| fields.get(field.as_str()))
                .map(|ty| (*ty).clone())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Level-1 type violation: {context} schema '{schema}' has no field '{field}'"
                    )
                })
        }
        Expr::Call { name, args, .. } => {
            let Some((param, output)) = transforms.get(name.as_str()) else {
                bail!("Level-1 type violation: {context} calls unknown transform '{name}'");
            };
            if args.len() != 1 {
                bail!(
                    "Level-1 type violation: {context} transform '{name}' expects exactly one \
                     argument, found {}",
                    args.len()
                );
            }
            let actual = infer_expr(&args[0], env, schemas, transforms, context)?;
            expect_type(
                &actual,
                param,
                &format!("{context} argument to transform '{name}'"),
            )?;
            Ok((*output).clone())
        }
        Expr::Pipeline { base, steps, .. } => {
            let mut current = infer_expr(base, env, schemas, transforms, context)?;
            for step in steps {
                current = check_step(step, current, env, schemas, transforms, context)?;
            }
            Ok(current)
        }
        Expr::Binary { op, lhs, rhs, span } => {
            let left = infer_expr(lhs, env, schemas, transforms, context)?;
            let right = infer_expr(rhs, env, schemas, transforms, context)?;
            match op {
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => {
                    if !is_numeric(&left) || !is_numeric(&right) {
                        bail!(
                            "Level-1 type violation: {context} arithmetic at bytes {}..{} is \
                             defined only on Int and Float, found {} and {}",
                            span.start,
                            span.end,
                            display_type(&left),
                            display_type(&right)
                        );
                    }
                    if left != right {
                        bail!(
                            "Level-1 type violation: {context} arithmetic at bytes {}..{} \
                             mixes numeric types {} and {}; Sigil performs no implicit coercion",
                            span.start,
                            span.end,
                            display_type(&left),
                            display_type(&right)
                        );
                    }
                    Ok(left)
                }
                BinOp::Le | BinOp::Ge | BinOp::Lt | BinOp::Gt => {
                    if !is_numeric(&left) || !is_numeric(&right) {
                        bail!(
                            "Level-1 type violation: {context} ordering at bytes {}..{} is \
                             defined only on Int and Float, found {} and {}",
                            span.start,
                            span.end,
                            display_type(&left),
                            display_type(&right)
                        );
                    }
                    if left != right {
                        bail!(
                            "Level-1 type violation: {context} ordering at bytes {}..{} mixes \
                             numeric types {} and {}; Sigil performs no implicit coercion",
                            span.start,
                            span.end,
                            display_type(&left),
                            display_type(&right)
                        );
                    }
                    Ok(Type::Bool)
                }
                BinOp::Eq => {
                    expect_type(&right, &left, &format!("{context} equality"))?;
                    Ok(Type::Bool)
                }
            }
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            let cond_ty = infer_expr(cond, env, schemas, transforms, context)?;
            expect_type(&cond_ty, &Type::Bool, &format!("{context} if condition"))?;
            let then_ty = infer_expr(then_branch, env, schemas, transforms, context)?;
            let else_ty = infer_expr(else_branch, env, schemas, transforms, context)?;
            expect_type(&else_ty, &then_ty, &format!("{context} if branch result"))?;
            Ok(then_ty)
        }
        Expr::SchemaLit { name, fields, .. } => {
            let Some(declared) = schemas.get(name.as_str()) else {
                bail!("Level-1 type violation: {context} constructs unknown schema '{name}'");
            };
            let mut seen = BTreeSet::new();
            for (field, value) in fields {
                if !seen.insert(field.as_str()) {
                    bail!(
                        "Level-1 name violation: {context} initializes schema '{name}' field \
                         '{field}' more than once"
                    );
                }
                let Some(expected) = declared.get(field.as_str()) else {
                    bail!(
                        "Level-1 type violation: {context} initializes unknown field \
                         '{name}.{field}'"
                    );
                };
                let actual = infer_expr(value, env, schemas, transforms, context)?;
                expect_type(
                    &actual,
                    expected,
                    &format!("{context} field '{name}.{field}'"),
                )?;
            }
            let missing: Vec<&str> = declared
                .keys()
                .copied()
                .filter(|field| !seen.contains(field))
                .collect();
            if !missing.is_empty() {
                bail!(
                    "Level-1 type violation: {context} schema literal '{name}' is missing \
                     field(s): {}",
                    missing.join(", ")
                );
            }
            Ok(Type::Named(name.clone()))
        }
    }
}

fn check_transform_bodies<'a>(
    program: &Program,
    schemas: &Schemas<'a>,
    transforms: &Transforms<'a>,
) -> Result<()> {
    for transform in &program.transforms {
        if transform.body.is_empty() {
            continue;
        }
        let context = format!("transform '{}'", transform.name);
        let mut env = Env::new();
        env.insert(transform.param.clone(), transform.param_ty.clone());
        let mut returned = false;
        for (index, stmt) in transform.body.iter().enumerate() {
            match stmt {
                Stmt::Let { name, expr, .. } => {
                    if returned {
                        bail!("Level-1 type violation: {context} has code after its result");
                    }
                    if env.contains_key(name) {
                        bail!("Level-1 name violation: {context} redeclares local name '{name}'");
                    }
                    let ty = infer_expr(expr, &env, schemas, transforms, &context)?;
                    env.insert(name.clone(), ty);
                }
                Stmt::Expr { expr, .. } => {
                    if index + 1 != transform.body.len() {
                        bail!(
                            "Level-1 type violation: {context} result expression must be the \
                             final statement"
                        );
                    }
                    let actual = infer_expr(expr, &env, schemas, transforms, &context)?;
                    expect_type(&actual, &transform.return_ty, &format!("{context} return"))?;
                    returned = true;
                }
                Stmt::Assign { .. } | Stmt::Send { .. } => bail!(
                    "Level-1 type violation: {context} may contain local lets and one final \
                     result expression, but not state assignment or send"
                ),
            }
        }
        if !returned {
            bail!(
                "Level-1 type violation: {context} has a body but no final expression of \
                 return type {}",
                display_type(&transform.return_ty)
            );
        }
    }
    Ok(())
}

fn check_processes<'a>(
    program: &Program,
    schemas: &Schemas<'a>,
    transforms: &Transforms<'a>,
) -> Result<()> {
    for process in &program.processes {
        let context = format!("process '{}'", process.name);
        let mut state_env = Env::new();
        for state in &process.states {
            let init_ty = infer_expr(
                &state.init,
                &Env::new(),
                schemas,
                transforms,
                &format!("{context} state '{}' initializer", state.name),
            )?;
            expect_type(
                &init_ty,
                &state.ty,
                &format!("{context} state '{}' initializer", state.name),
            )?;
            state_env.insert(state.name.clone(), state.ty.clone());
        }

        for handler in &process.handlers {
            let handler_context = format!("{context} handler '{}'", handler.msg_name);
            if state_env.contains_key(&handler.msg_name) {
                bail!(
                    "Level-1 name violation: {handler_context} message name collides with a \
                     process state"
                );
            }
            let mut env = state_env.clone();
            env.insert(handler.msg_name.clone(), handler.msg_ty.clone());
            let mut locals = BTreeSet::new();

            for stmt in &handler.body {
                match stmt {
                    Stmt::Let { name, expr, .. } => {
                        if env.contains_key(name) || !locals.insert(name.as_str()) {
                            bail!(
                                "Level-1 name violation: {handler_context} redeclares local \
                                 name '{name}'"
                            );
                        }
                        let ty = infer_expr(expr, &env, schemas, transforms, &handler_context)?;
                        env.insert(name.clone(), ty);
                    }
                    Stmt::Assign { name, expr, .. } => {
                        let Some(expected) = state_env.get(name) else {
                            bail!(
                                "Level-1 name violation: {handler_context} assigns unknown or \
                                 non-state name '{name}'"
                            );
                        };
                        let actual = infer_expr(expr, &env, schemas, transforms, &handler_context)?;
                        expect_type(
                            &actual,
                            expected,
                            &format!("{handler_context} assignment to '{name}'"),
                        )?;
                    }
                    Stmt::Expr { expr, .. } => {
                        infer_expr(expr, &env, schemas, transforms, &handler_context)?;
                    }
                    Stmt::Send {
                        target,
                        expr,
                        route,
                        backpressure,
                        guard,
                        ..
                    } => {
                        let sent = infer_expr(expr, &env, schemas, transforms, &handler_context)?;
                        let Some(destination) = program
                            .processes
                            .iter()
                            .find(|candidate| candidate.name == *target)
                        else {
                            bail!(
                                "Level-1 name violation: {handler_context} sends to unknown \
                                 process '{target}'"
                            );
                        };
                        let matching: Vec<_> = destination
                            .handlers
                            .iter()
                            .filter(|candidate| candidate.msg_ty == sent)
                            .collect();
                        if matching.len() != 1 {
                            bail!(
                                "Level-1 type violation: {handler_context} sends {} to \
                                 '{target}', which has {} matching handler(s)",
                                display_type(&sent),
                                matching.len()
                            );
                        }
                        if let Some(condition) = guard {
                            let ty =
                                infer_expr(condition, &env, schemas, transforms, &handler_context)?;
                            expect_type(
                                &ty,
                                &Type::Bool,
                                &format!("{handler_context} send guard"),
                            )?;
                        }
                        if let Route::ByKey(key) = route {
                            let ty = infer_expr(key, &env, schemas, transforms, &handler_context)?;
                            if !matches!(
                                ty,
                                Type::Int
                                    | Type::String
                                    | Type::Bool
                                    | Type::UUID
                                    | Type::Bytes
                                    | Type::Duration
                            ) {
                                bail!(
                                    "Level-1 type violation: {handler_context} route key has \
                                     type {}, which has no stable generated hash",
                                    display_type(&ty)
                                );
                            }
                        }
                        if let Backpressure::Deadline(_) = backpressure {
                            // The parser already restricts this to a Duration literal.
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn spec_env(program: &Program) -> Env {
    let mut env = Env::new();
    env.insert("path_timeout_sum".into(), Type::Duration);
    env.insert("path_latency".into(), Type::Duration);
    for state in program.processes.iter().flat_map(|process| &process.states) {
        env.insert(state.name.clone(), state.ty.clone());
    }
    for handler in program
        .processes
        .iter()
        .flat_map(|process| &process.handlers)
    {
        match env.get(&handler.msg_name) {
            None => {
                env.insert(handler.msg_name.clone(), handler.msg_ty.clone());
            }
            Some(existing) if existing == &handler.msg_ty => {}
            Some(_) => {
                // Keep it absent below: an ambiguous name must be qualified by
                // a future syntax extension rather than silently picking one.
                env.remove(&handler.msg_name);
            }
        }
    }
    env
}

fn check_specs<'a>(
    program: &Program,
    schemas: &Schemas<'a>,
    transforms: &Transforms<'a>,
) -> Result<()> {
    let base = spec_env(program);
    for spec in &program.specs {
        let context = format!("spec '{}'", spec.name);
        let mut env = base.clone();
        for process in &program.processes {
            env.insert(process.name.clone(), Type::Named(process.name.clone()));
        }
        for item in &spec.items {
            let expr = match item {
                SpecItem::Require { expr, .. } | SpecItem::Hold { expr, .. } => expr,
                SpecItem::Extinct { .. } => continue,
            };
            // Process.state is a proof-only qualified access. Resolve it
            // before the ordinary schema expression checker.
            let ty = match expr {
                Expr::Binary { op, lhs, rhs, span } => {
                    let infer_side = |side: &Expr| -> Result<Type> {
                        if let Expr::FieldAccess { base, field, .. } = side {
                            if let Some(process) = program
                                .processes
                                .iter()
                                .find(|process| process.name == *base)
                            {
                                return process
                                    .states
                                    .iter()
                                    .find(|state| state.name == *field)
                                    .map(|state| state.ty.clone())
                                    .ok_or_else(|| {
                                        anyhow::anyhow!(
                                            "Level-1 type violation: {context} process '{base}' \
                                             has no state '{field}'"
                                        )
                                    });
                            }
                        }
                        infer_expr(side, &base, schemas, transforms, &context)
                    };
                    let left = infer_side(lhs)?;
                    let right = infer_side(rhs)?;
                    match op {
                        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => {
                            if !is_numeric(&left) || left != right {
                                bail!(
                                    "Level-1 type violation: {context} arithmetic at bytes \
                                     {}..{} requires equal numeric operands",
                                    span.start,
                                    span.end
                                );
                            }
                            left
                        }
                        BinOp::Le | BinOp::Ge | BinOp::Lt | BinOp::Gt => {
                            if is_numeric(&left) && is_numeric(&right) && left != right {
                                bail!(
                                    "Level-1 type violation: {context} ordering mixes numeric \
                                     types {} and {}; Sigil performs no implicit coercion",
                                    display_type(&left),
                                    display_type(&right)
                                );
                            }
                            if !(left == right && (is_numeric(&left) || left == Type::Duration)) {
                                bail!(
                                    "Level-1 type violation: {context} ordering requires equal \
                                     numeric or Duration operands, found {} and {}",
                                    display_type(&left),
                                    display_type(&right)
                                );
                            }
                            Type::Bool
                        }
                        BinOp::Eq => {
                            expect_type(&right, &left, &format!("{context} equality"))?;
                            Type::Bool
                        }
                    }
                }
                other => infer_expr(other, &base, schemas, transforms, &context)?,
            };
            expect_type(&ty, &Type::Bool, &format!("{context} obligation"))?;
        }
    }
    Ok(())
}

/// Validate every expression, transform result, call, field, schema literal,
/// send value, guard, route key, and local declaration before code generation.
pub fn check_types(program: &Program) -> Result<()> {
    let schemas: Schemas<'_> = program
        .schemas
        .iter()
        .map(|schema| {
            (
                schema.name.as_str(),
                schema
                    .fields
                    .iter()
                    .map(|(name, ty)| (name.as_str(), ty))
                    .collect(),
            )
        })
        .collect();
    let transforms: Transforms<'_> = program
        .transforms
        .iter()
        .map(|transform| {
            (
                transform.name.as_str(),
                (&transform.param_ty, &transform.return_ty),
            )
        })
        .collect();

    check_transform_bodies(program, &schemas, &transforms)?;
    check_processes(program, &schemas, &transforms)?;
    check_specs(program, &schemas, &transforms)
}

fn check_effect_expr(expression: &Expr, program: &Program, owner: &str) -> Result<()> {
    match expression {
        Expr::Pipeline { base, steps, .. } => {
            check_effect_expr(base, program, owner)?;
            for step in steps {
                let name = match &step.expr {
                    Expr::Ident { name, .. } | Expr::Call { name, .. } => name,
                    other => {
                        check_effect_expr(other, program, owner)?;
                        continue;
                    }
                };
                let Some(transform) = program
                    .transforms
                    .iter()
                    .find(|transform| transform.name == *name)
                else {
                    continue;
                };
                let Some(binding) = &transform.binding else {
                    continue;
                };
                let retries = step.tags.iter().any(|tag| matches!(tag, Tag::Retry { .. }));
                let timeout = step
                    .tags
                    .iter()
                    .any(|tag| matches!(tag, Tag::Timeout { .. }));
                if retries && binding.effect.idempotency != Idempotency::Idempotent {
                    bail!(
                        "Level-1 effect violation in {owner}: bound transform '{name}' is \
                         non_idempotent but the call retries; retry could duplicate its {:?} \
                         effect",
                        binding.effect.side_effect
                    );
                }
                if timeout
                    && binding.kind == BindKind::Blocking
                    && binding.effect.cancellation != Cancellation::CompletionTracked
                {
                    bail!(
                        "Level-1 effect violation in {owner}: timed blocking transform '{name}' \
                         must declare completion_tracked cancellation semantics"
                    );
                }
            }
            Ok(())
        }
        Expr::Call { args, .. } => {
            for argument in args {
                check_effect_expr(argument, program, owner)?;
            }
            Ok(())
        }
        Expr::Binary { lhs, rhs, .. } => {
            check_effect_expr(lhs, program, owner)?;
            check_effect_expr(rhs, program, owner)
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            check_effect_expr(cond, program, owner)?;
            check_effect_expr(then_branch, program, owner)?;
            check_effect_expr(else_branch, program, owner)
        }
        Expr::SchemaLit { fields, .. } => {
            for (_, value) in fields {
                check_effect_expr(value, program, owner)?;
            }
            Ok(())
        }
        Expr::Ident { .. } | Expr::FieldAccess { .. } | Expr::Literal { .. } => Ok(()),
    }
}

/// Validate foreign-effect metadata and every retry/cancellation use site.
pub fn check_effect_contracts(program: &Program) -> Result<()> {
    for transform in &program.transforms {
        let Some(binding) = &transform.binding else {
            continue;
        };
        match binding.kind {
            BindKind::Async if binding.effect.cancellation != Cancellation::CancelSafe => {
                bail!(
                    "Level-1 effect violation: async binding '{}' must declare cancel_safe; \
                     internally detached async work is unsupported",
                    transform.name
                );
            }
            BindKind::Blocking
                if binding.effect.cancellation != Cancellation::CompletionTracked =>
            {
                bail!(
                    "Level-1 effect violation: blocking binding '{}' must declare \
                     completion_tracked",
                    transform.name
                );
            }
            BindKind::Infallible
                if binding.effect.idempotency != Idempotency::Idempotent
                    || binding.effect.cancellation != Cancellation::CancelSafe
                    || binding.effect.side_effect != SideEffect::None =>
            {
                bail!(
                    "Level-1 effect violation: infallible binding '{}' must declare \
                     @effect(idempotent, cancel_safe, none)",
                    transform.name
                );
            }
            _ => {}
        }
    }
    for process in &program.processes {
        for handler in &process.handlers {
            for statement in &handler.body {
                let expression = match statement {
                    Stmt::Let { expr, .. }
                    | Stmt::Assign { expr, .. }
                    | Stmt::Send { expr, .. }
                    | Stmt::Expr { expr, .. } => expr,
                };
                check_effect_expr(
                    expression,
                    program,
                    &format!("process '{}' handler '{}'", process.name, handler.msg_name),
                )?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::ast::parse;

    fn reject(source: &str) -> String {
        let program = parse(source).expect("test input parses");
        check_types(&program)
            .expect_err("input must be rejected")
            .to_string()
    }

    #[test]
    fn rejects_transform_return_and_call_argument_mismatches() {
        let bad_return = r#"
schema M { value: Int }
transform wrong(m: M) -> Int { m }
process P { on m: M {} }
"#;
        assert!(reject(bad_return).contains("return expects Int, found M"));

        let bad_call = r#"
schema M { value: Int }
transform id(m: M) -> M { m }
process P { on m: M { let x = id(1) } }
"#;
        assert!(reject(bad_call).contains("argument to transform 'id' expects M, found Int"));
    }

    #[test]
    fn rejects_unknown_fields_and_incomplete_schema_literals() {
        let missing = r#"
schema M { value: Int, ok: Bool }
process P { on m: M { let x = M { value: 1 } } }
"#;
        assert!(reject(missing).contains("missing field(s): ok"));

        let extra = r#"
schema M { value: Int }
process P { on m: M { let x = M { value: 1, nope: true } } }
"#;
        assert!(reject(extra).contains("unknown field 'M.nope'"));

        let access = r#"
schema M { value: Int }
process P { on m: M { let x = m.nope } }
"#;
        assert!(reject(access).contains("has no field 'nope'"));
    }

    #[test]
    fn rejects_bad_guards_route_keys_and_send_values() {
        let guard = r#"
schema M { value: Int }
process A { on m: M { send m to B when m.value } }
process B { on m: M {} }
"#;
        assert!(reject(guard).contains("send guard expects Bool, found Int"));

        let route = r#"
schema M { value: Float }
process A { on m: M { send m to B by m.value } }
process B { on m: M {} }
"#;
        assert!(reject(route).contains("route key has type Float"));

        let send = r#"
schema M { value: Int }
schema N { value: Int }
process A { on m: M { send m to B } }
process B { on n: N {} }
"#;
        assert!(reject(send).contains("has 0 matching handler"));
    }

    #[test]
    fn rejects_name_holes_and_non_state_assignment() {
        let unknown = r#"
schema M { value: Int }
process P { on m: M { let x = missing } }
"#;
        assert!(reject(unknown).contains("unknown name 'missing'"));

        let local_assign = r#"
schema M { value: Int }
process P { on m: M { let x = 1 x := 2 } }
"#;
        assert!(reject(local_assign).contains("non-state name 'x'"));
    }

    #[test]
    fn validates_bound_effect_and_retry_contracts() {
        let non_idempotent_retry = r#"
schema M { value: Int }
transform write(m: M) -> M = service::write @effect(non_idempotent, cancel_safe, write)
transform fallback(m: M) -> M { m }
process P {
  on m: M {
    let x = m ~> write @timeout(5.ms) @retry(1) @recover(with: fallback)
  }
}
"#;
        let program = parse(non_idempotent_retry).expect("effect syntax parses");
        assert!(check_effect_contracts(&program)
            .expect_err("non-idempotent retry must fail")
            .to_string()
            .contains("could duplicate"));

        let untracked_blocking = r#"
schema M { value: Int }
transform read(m: M) -> M = blocking service::read @effect(idempotent, cancel_safe, read)
process P { on m: M {} }
"#;
        let program = parse(untracked_blocking).expect("effect syntax parses");
        assert!(check_effect_contracts(&program)
            .expect_err("blocking work must be tracked")
            .to_string()
            .contains("completion_tracked"));
    }
}
