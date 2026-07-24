//! Canonical Sigil formatter used by round-trip property tests and tooling.

use super::ast::{
    Backpressure, BinOp, BindKind, Cancellation, CrateSource, Expr, Idempotency, Literal, Program,
    Route, SideEffect, SpecItem, Stmt, Tag, Type,
};
use std::fmt::Write;

fn ty(ty: &Type) -> &str {
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

fn expression(expr: &Expr) -> String {
    match expr {
        Expr::Ident { name, .. } => name.clone(),
        Expr::FieldAccess { base, field, .. } => format!("{base}.{field}"),
        Expr::Literal { value, .. } => match value {
            Literal::Int(value) => value.to_string(),
            Literal::Float(value) => {
                let rendered = value.to_string();
                if rendered.contains('.') {
                    rendered
                } else {
                    format!("{rendered}.0")
                }
            }
            Literal::String(value) => format!("\"{value}\""),
            Literal::Bool(value) => value.to_string(),
            Literal::DurationMs(value) => format!("{value}.ms"),
        },
        Expr::Pipeline { base, steps, .. } => {
            let mut out = expression(base);
            for step in steps {
                let _ = write!(out, " ~> {}", expression(&step.expr));
                for tag in &step.tags {
                    match tag {
                        Tag::Timeout { expr, .. } => {
                            let _ = write!(out, " @timeout({})", expression(expr));
                        }
                        Tag::Recover { with, .. } => {
                            let _ = write!(out, " @recover(with: {})", expression(with));
                        }
                        Tag::Retry { expr, .. } => {
                            let _ = write!(out, " @retry({})", expression(expr));
                        }
                        Tag::Error { .. } => out.push_str(" @error"),
                    }
                }
            }
            out
        }
        Expr::Call { name, args, .. } => format!(
            "{name}({})",
            args.iter().map(expression).collect::<Vec<_>>().join(", ")
        ),
        Expr::Binary { op, lhs, rhs, .. } => {
            let operator = match op {
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
            format!("({} {operator} {})", expression(lhs), expression(rhs))
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
            ..
        } => format!(
            "if {} {{ {} }} else {{ {} }}",
            expression(cond),
            expression(then_branch),
            expression(else_branch)
        ),
        Expr::SchemaLit { name, fields, .. } => format!(
            "{name} {{ {} }}",
            fields
                .iter()
                .map(|(field, value)| format!("{field}: {}", expression(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn statement(statement: &Stmt, indent: &str) -> String {
    match statement {
        Stmt::Let { name, expr, .. } => format!("{indent}let {name} = {}\n", expression(expr)),
        Stmt::Assign { name, expr, .. } => format!("{indent}{name} := {}\n", expression(expr)),
        Stmt::Expr { expr, .. } => format!("{indent}{}\n", expression(expr)),
        Stmt::Send {
            target,
            expr,
            route,
            backpressure,
            guard,
            ..
        } => {
            let route = match route {
                Route::RoundRobin => String::new(),
                Route::ByKey(key) => format!(" by {}", expression(key)),
                Route::Broadcast => " broadcast".into(),
            };
            let backpressure = match backpressure {
                Backpressure::Block => " @block".to_string(),
                Backpressure::Shed => " @shed".to_string(),
                Backpressure::Deadline(ms) => format!(" @deadline({ms}.ms)"),
            };
            let guard = guard
                .as_ref()
                .map(|condition| format!(" when {}", expression(condition)))
                .unwrap_or_default();
            format!(
                "{indent}send {} to {target}{route}{backpressure}{guard}\n",
                expression(expr)
            )
        }
    }
}

/// Render an AST into one stable, parseable representation.
pub fn format_program(program: &Program) -> String {
    let mut out = String::new();
    for dependency in &program.extern_crates {
        match &dependency.source {
            CrateSource::Version(version) => {
                let _ = writeln!(out, "extern crate {} = \"{version}\"", dependency.name);
            }
            CrateSource::Path(path) => {
                let _ = writeln!(out, "extern crate {} = path \"{path}\"", dependency.name);
            }
        }
    }
    for schema in &program.schemas {
        let binding = schema
            .binding
            .as_ref()
            .map(|path| format!(" = {path}"))
            .unwrap_or_default();
        let _ = writeln!(out, "schema {}{binding} {{", schema.name);
        for (field, field_ty) in &schema.fields {
            let _ = writeln!(out, "  {field}: {},", ty(field_ty));
        }
        out.push_str("}\n");
    }
    for transform in &program.transforms {
        let _ = write!(
            out,
            "transform {}({}: {}) -> {}",
            transform.name,
            transform.param,
            ty(&transform.param_ty),
            ty(&transform.return_ty)
        );
        if let Some(binding) = &transform.binding {
            let kind = match binding.kind {
                BindKind::Async => "",
                BindKind::Blocking => "blocking ",
                BindKind::Infallible => "infallible ",
            };
            let idempotency = match binding.effect.idempotency {
                Idempotency::Idempotent => "idempotent",
                Idempotency::NonIdempotent => "non_idempotent",
            };
            let cancellation = match binding.effect.cancellation {
                Cancellation::CancelSafe => "cancel_safe",
                Cancellation::CompletionTracked => "completion_tracked",
            };
            let side_effect = match binding.effect.side_effect {
                SideEffect::None => "none",
                SideEffect::Read => "read",
                SideEffect::Write => "write",
            };
            let _ = writeln!(
                out,
                " =\n  {kind}{}\n    @effect({idempotency}, {cancellation}, {side_effect})",
                binding.path
            );
        } else {
            out.push_str(" {\n");
            for body in &transform.body {
                out.push_str(&statement(body, "  "));
            }
            out.push_str("}\n");
        }
    }
    for placement in &program.placements {
        let _ = writeln!(
            out,
            "placement {} {{ {} }}",
            placement.name,
            placement.processes.join(", ")
        );
    }
    for process in &program.processes {
        let _ = writeln!(out, "process {} {{", process.name);
        for state in &process.states {
            let _ = writeln!(
                out,
                "  state {}: {} = {}",
                state.name,
                ty(&state.ty),
                expression(&state.init)
            );
        }
        for handler in &process.handlers {
            let _ = writeln!(out, "  on {}: {} {{", handler.msg_name, ty(&handler.msg_ty));
            for body in &handler.body {
                out.push_str(&statement(body, "    "));
            }
            out.push_str("  }\n");
        }
        out.push_str("}\n");
    }
    for spec in &program.specs {
        let _ = writeln!(out, "spec {} {{", spec.name);
        for item in &spec.items {
            match item {
                SpecItem::Extinct { names, .. } => {
                    let _ = writeln!(out, "  extinct [{}]", names.join(", "));
                }
                SpecItem::Require { expr, .. } => {
                    let _ = writeln!(out, "  require {}", expression(expr));
                }
                SpecItem::Hold { expr, .. } => {
                    let _ = writeln!(out, "  hold {}", expression(expr));
                }
            }
        }
        out.push_str("}\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::ast::parse;

    #[test]
    fn canonical_format_is_parse_stable() {
        let source = include_str!("../../../examples/avionics/attitude_control.sigil");
        let first = format_program(&parse(source).expect("example parses"));
        let second = format_program(&parse(&first).expect("formatted output parses"));
        assert_eq!(first, second);
    }

    #[test]
    fn bound_transforms_format_multiline_but_accept_single_line_input() {
        let source = "transform read(x: Input) -> Output = blocking hal::read \
                      @effect(idempotent, completion_tracked, read)";
        let formatted = format_program(&parse(source).expect("single-line binding parses"));
        assert_eq!(
            formatted,
            "transform read(x: Input) -> Output =\n\
             \x20 blocking hal::read\n\
             \x20   @effect(idempotent, completion_tracked, read)\n"
        );
        parse(&formatted).expect("canonical multiline binding parses");
    }
}
