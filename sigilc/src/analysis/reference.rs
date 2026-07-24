//! Small executable reference semantics for the pure, single-handler subset.
//!
//! It is intentionally direct and independent of Rust code generation. The
//! differential/property suite uses it as an oracle for state and trace order.

use crate::frontend::ast::{BinOp, Expr, Literal, Process, Program, Stmt, Type};
use anyhow::{bail, Result};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum ReferenceValue {
    Int(i64),
    Float(f64),
    String(String),
    Bool(bool),
    Duration(u64),
    Record(BTreeMap<String, ReferenceValue>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum TraceEvent {
    StateWrite {
        state: String,
        value: ReferenceValue,
    },
    Send {
        target: String,
        value: ReferenceValue,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReferenceResult {
    pub state: BTreeMap<String, ReferenceValue>,
    pub trace: Vec<TraceEvent>,
}

fn literal(value: &Literal) -> ReferenceValue {
    match value {
        Literal::Int(value) => ReferenceValue::Int(*value),
        Literal::Float(value) => ReferenceValue::Float(*value),
        Literal::String(value) => ReferenceValue::String(value.clone()),
        Literal::Bool(value) => ReferenceValue::Bool(*value),
        Literal::DurationMs(value) => ReferenceValue::Duration(*value),
    }
}

fn checked_int_binary(op: &BinOp, left: i64, right: i64) -> Result<ReferenceValue> {
    match op {
        BinOp::Add => left
            .checked_add(right)
            .map(ReferenceValue::Int)
            .ok_or_else(|| anyhow::anyhow!("reference Int addition overflow")),
        BinOp::Sub => left
            .checked_sub(right)
            .map(ReferenceValue::Int)
            .ok_or_else(|| anyhow::anyhow!("reference Int subtraction overflow")),
        BinOp::Mul => left
            .checked_mul(right)
            .map(ReferenceValue::Int)
            .ok_or_else(|| anyhow::anyhow!("reference Int multiplication overflow")),
        BinOp::Div => left
            .checked_div(right)
            .map(ReferenceValue::Int)
            .ok_or_else(|| anyhow::anyhow!("reference Int division failed")),
        BinOp::Le => Ok(ReferenceValue::Bool(left <= right)),
        BinOp::Ge => Ok(ReferenceValue::Bool(left >= right)),
        BinOp::Lt => Ok(ReferenceValue::Bool(left < right)),
        BinOp::Gt => Ok(ReferenceValue::Bool(left > right)),
        BinOp::Eq => Ok(ReferenceValue::Bool(left == right)),
    }
}

fn eval(
    expression: &Expr,
    env: &BTreeMap<String, ReferenceValue>,
    program: &Program,
) -> Result<ReferenceValue> {
    match expression {
        Expr::Literal { value, .. } => Ok(literal(value)),
        Expr::Ident { name, .. } => env
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("reference reads unknown name '{name}'")),
        Expr::FieldAccess { base, field, .. } => {
            let Some(ReferenceValue::Record(record)) = env.get(base) else {
                bail!("reference field base '{base}' is not a record");
            };
            record
                .get(field)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("reference record has no field '{field}'"))
        }
        Expr::SchemaLit { fields, .. } => {
            let mut record = BTreeMap::new();
            for (field, value) in fields {
                record.insert(field.clone(), eval(value, env, program)?);
            }
            Ok(ReferenceValue::Record(record))
        }
        Expr::Binary { op, lhs, rhs, .. } => {
            let left = eval(lhs, env, program)?;
            let right = eval(rhs, env, program)?;
            match (left, right) {
                (ReferenceValue::Int(left), ReferenceValue::Int(right)) => {
                    checked_int_binary(op, left, right)
                }
                (ReferenceValue::Float(left), ReferenceValue::Float(right)) => {
                    let value = match op {
                        BinOp::Add => ReferenceValue::Float(left + right),
                        BinOp::Sub => ReferenceValue::Float(left - right),
                        BinOp::Mul => ReferenceValue::Float(left * right),
                        BinOp::Div => ReferenceValue::Float(left / right),
                        BinOp::Le => ReferenceValue::Bool(left <= right),
                        BinOp::Ge => ReferenceValue::Bool(left >= right),
                        BinOp::Lt => ReferenceValue::Bool(left < right),
                        BinOp::Gt => ReferenceValue::Bool(left > right),
                        BinOp::Eq => ReferenceValue::Bool(left == right),
                    };
                    Ok(value)
                }
                (left, right) if matches!(op, BinOp::Eq) => Ok(ReferenceValue::Bool(left == right)),
                _ => bail!("reference encountered ill-typed binary expression"),
            }
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
            ..
        } => match eval(cond, env, program)? {
            ReferenceValue::Bool(true) => eval(then_branch, env, program),
            ReferenceValue::Bool(false) => eval(else_branch, env, program),
            _ => bail!("reference if condition is not Bool"),
        },
        Expr::Call { name, args, .. } => {
            if args.len() != 1 {
                bail!("reference transforms take exactly one argument");
            }
            let input = eval(&args[0], env, program)?;
            eval_transform(name, input, program)
        }
        Expr::Pipeline { base, steps, .. } => {
            let mut value = eval(base, env, program)?;
            for step in steps {
                let name = match &step.expr {
                    Expr::Ident { name, .. } | Expr::Call { name, .. } => name,
                    _ => bail!("reference pipeline stage is not a transform"),
                };
                value = eval_transform(name, value, program)?;
            }
            Ok(value)
        }
    }
}

fn eval_transform(name: &str, input: ReferenceValue, program: &Program) -> Result<ReferenceValue> {
    let transform = program
        .transforms
        .iter()
        .find(|transform| transform.name == name)
        .ok_or_else(|| anyhow::anyhow!("reference calls unknown transform '{name}'"))?;
    if transform.binding.is_some() || transform.body.is_empty() {
        bail!("reference cannot execute external transform '{name}'");
    }
    let mut env = BTreeMap::new();
    env.insert(transform.param.clone(), input);
    let mut result = None;
    for statement in &transform.body {
        match statement {
            Stmt::Let { name, expr, .. } => {
                env.insert(name.clone(), eval(expr, &env, program)?);
            }
            Stmt::Expr { expr, .. } => result = Some(eval(expr, &env, program)?),
            Stmt::Assign { .. } | Stmt::Send { .. } => {
                bail!("reference pure transform contains an effect")
            }
        }
    }
    result.ok_or_else(|| anyhow::anyhow!("reference transform '{name}' has no result"))
}

fn default_state(process: &Process, program: &Program) -> Result<BTreeMap<String, ReferenceValue>> {
    let env = BTreeMap::new();
    process
        .states
        .iter()
        .map(|state| Ok((state.name.clone(), eval(&state.init, &env, program)?)))
        .collect()
}

/// Execute one handler sequentially for the supplied messages. Each statement
/// is one trace boundary; an error stops before subsequent boundaries.
pub fn interpret_handler(
    program: &Program,
    process_name: &str,
    messages: &[ReferenceValue],
) -> Result<ReferenceResult> {
    let process = program
        .processes
        .iter()
        .find(|process| process.name == process_name)
        .ok_or_else(|| anyhow::anyhow!("reference has no process '{process_name}'"))?;
    if process.handlers.len() != 1 {
        bail!("reference handler runner requires exactly one handler");
    }
    let handler = &process.handlers[0];
    let mut state = default_state(process, program)?;
    let mut trace = Vec::new();
    for message in messages {
        let mut env = state.clone();
        env.insert(handler.msg_name.clone(), message.clone());
        for statement in &handler.body {
            match statement {
                Stmt::Let { name, expr, .. } => {
                    env.insert(name.clone(), eval(expr, &env, program)?);
                }
                Stmt::Assign { name, expr, .. } => {
                    let value = eval(expr, &env, program)?;
                    state.insert(name.clone(), value.clone());
                    env.insert(name.clone(), value.clone());
                    trace.push(TraceEvent::StateWrite {
                        state: name.clone(),
                        value,
                    });
                }
                Stmt::Send {
                    target,
                    expr,
                    guard,
                    ..
                } => {
                    let enabled = match guard {
                        None => true,
                        Some(condition) => {
                            matches!(eval(condition, &env, program)?, ReferenceValue::Bool(true))
                        }
                    };
                    if enabled {
                        trace.push(TraceEvent::Send {
                            target: target.clone(),
                            value: eval(expr, &env, program)?,
                        });
                    }
                }
                Stmt::Expr { expr, .. } => {
                    let _ = eval(expr, &env, program)?;
                }
            }
        }
    }
    Ok(ReferenceResult { state, trace })
}

/// Construct a record value for a named schema, checking field completeness.
pub fn record(
    program: &Program,
    schema_name: &str,
    fields: impl IntoIterator<Item = (String, ReferenceValue)>,
) -> Result<ReferenceValue> {
    let schema = program
        .schemas
        .iter()
        .find(|schema| schema.name == schema_name)
        .ok_or_else(|| anyhow::anyhow!("unknown reference schema '{schema_name}'"))?;
    let record: BTreeMap<_, _> = fields.into_iter().collect();
    for (field, field_ty) in &schema.fields {
        let Some(value) = record.get(field) else {
            bail!("reference record is missing '{schema_name}.{field}'");
        };
        let matches = matches!(
            (field_ty, value),
            (Type::Int, ReferenceValue::Int(_))
                | (Type::Float, ReferenceValue::Float(_))
                | (Type::String | Type::UUID, ReferenceValue::String(_))
                | (Type::Bool, ReferenceValue::Bool(_))
                | (Type::Duration, ReferenceValue::Duration(_))
                | (Type::Bytes, ReferenceValue::Record(_))
                | (Type::Named(_), ReferenceValue::Record(_))
        );
        if !matches {
            bail!("reference record field '{schema_name}.{field}' has the wrong type");
        }
    }
    if record.len() != schema.fields.len() {
        bail!("reference record for '{schema_name}' has extra fields");
    }
    Ok(ReferenceValue::Record(record))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lower, parse, run_checks, AssuranceLevel};

    #[test]
    fn proven_counter_is_not_refuted_by_reference_execution() {
        let source = r#"
schema M { value: Int }
process P {
  state total: Int = 0
  on m: M { total := total + m.value }
}
spec S {
  require m.value >= 0
  hold total >= 0
}
"#;
        let program = parse(source).expect("parse");
        let graph = lower(&program).expect("lower");
        run_checks(&program, &graph, AssuranceLevel::Proofs).expect("proof");
        let messages = [0, 1, 7, i64::MAX - 8]
            .into_iter()
            .map(|value| {
                record(
                    &program,
                    "M",
                    [("value".to_string(), ReferenceValue::Int(value))],
                )
                .expect("record")
            })
            .collect::<Vec<_>>();
        let result = interpret_handler(&program, "P", &messages).expect("reference run");
        assert_eq!(
            result.state.get("total"),
            Some(&ReferenceValue::Int(i64::MAX))
        );
    }
}
