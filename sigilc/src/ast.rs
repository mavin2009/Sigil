
//! AST for Sigil — pest-driven structured representation

use anyhow::{anyhow, bail, Result};
use pest::Parser;
use pest_derive::Parser;

#[derive(Parser)]
#[grammar = "sigil.pest"]
pub struct SigilParser;

#[derive(Debug, Clone)]
pub struct Program {
    pub schemas: Vec<Schema>,
    pub processes: Vec<Process>,
}

#[derive(Debug, Clone)]
pub struct Schema {
    pub name: String,
    pub fields: Vec<(String, Type)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Int, Float, String, Bool, UUID, Bytes, Duration, Named(String),
}

#[derive(Debug, Clone)]
pub struct Process {
    pub name: String,
    pub states: Vec<StateDecl>,
    pub handlers: Vec<OnHandler>,
}

#[derive(Debug, Clone)]
pub struct StateDecl {
    pub name: String,
    pub ty: Type,
    pub init: Expr,
}

#[derive(Debug, Clone)]
pub struct OnHandler {
    pub msg_name: String,
    pub msg_ty: Type,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Let { name: String, expr: Expr },
    Assign { name: String, expr: Expr },
    Expr(Expr),
}

#[derive(Debug, Clone)]
pub enum Expr {
    Ident(String),
    FieldAccess { base: String, field: String },
    Literal(Literal),
    Pipeline { base: Box<Expr>, steps: Vec<PipeStep> },
    Call { name: String, args: Vec<Expr> },
}

#[derive(Debug, Clone)]
pub struct PipeStep {
    pub expr: Expr,
    pub tags: Vec<Tag>,
}

#[derive(Debug, Clone)]
pub enum Tag {
    Timeout(Expr),
    Recover { with: Expr },
    Error,
}

#[derive(Debug, Clone)]
pub enum Literal {
    Int(i64),
    Float(f64),
    String(String),
    Bool(bool),
    DurationMs(u64),
}

pub fn parse(source: &str) -> Result<Program> {
    let pairs = SigilParser::parse(Rule::file, source)
        .map_err(|e| anyhow!("parse error:\n{}", e))?;

    let mut program = Program {
        schemas: vec![],
        processes: vec![],
    };

    for pair in pairs {
        for inner in pair.into_inner() {
            match inner.as_rule() {
                Rule::schema_def => program.schemas.push(parse_schema(inner)?),
                Rule::process_def => program.processes.push(parse_process(inner)?),
                Rule::EOI | Rule::transform_def | Rule::spec_def => {}
                r => eprintln!("skipping top-level {:?}", r),
            }
        }
    }
    Ok(program)
}

fn parse_schema(pair: pest::iterators::Pair<Rule>) -> Result<Schema> {
    let mut inner = pair.into_inner();
    let name = inner.next().unwrap().as_str().to_string();
    let mut fields = vec![];
    if let Some(fs) = inner.next() {
        for f in fs.into_inner() {
            if f.as_rule() == Rule::field {
                let mut fi = f.into_inner();
                let fname = fi.next().unwrap().as_str().to_string();
                let fty = parse_type(fi.next().unwrap())?;
                fields.push((fname, fty));
            }
        }
    }
    Ok(Schema { name, fields })
}

fn parse_type(pair: pest::iterators::Pair<Rule>) -> Result<Type> {
    Ok(match pair.as_str() {
        "Int" => Type::Int,
        "Float" => Type::Float,
        "String" => Type::String,
        "Bool" => Type::Bool,
        "UUID" => Type::UUID,
        "Bytes" => Type::Bytes,
        "Duration" => Type::Duration,
        other => Type::Named(other.to_string()),
    })
}

fn parse_process(pair: pest::iterators::Pair<Rule>) -> Result<Process> {
    let mut inner = pair.into_inner();
    let name = inner.next().unwrap().as_str().to_string();
    let body = inner.next().unwrap();
    let mut states = vec![];
    let mut handlers = vec![];
    for item in body.into_inner() {
        match item.as_rule() {
            Rule::state_decl => states.push(parse_state(item)?),
            Rule::on_handler => handlers.push(parse_handler(item)?),
            _ => {}
        }
    }
    Ok(Process { name, states, handlers })
}

fn parse_state(pair: pest::iterators::Pair<Rule>) -> Result<StateDecl> {
    let mut inner = pair.into_inner();
    let name = inner.next().unwrap().as_str().to_string();
    let ty = parse_type(inner.next().unwrap())?;
    let init = parse_expr(inner.next().unwrap())?;
    Ok(StateDecl { name, ty, init })
}

fn parse_handler(pair: pest::iterators::Pair<Rule>) -> Result<OnHandler> {
    let mut inner = pair.into_inner();
    let msg_name = inner.next().unwrap().as_str().to_string();
    let msg_ty = parse_type(inner.next().unwrap())?;
    let mut body = vec![];
    for item in inner {
        body.push(parse_stmt(item)?);
    }
    Ok(OnHandler { msg_name, msg_ty, body })
}

fn parse_stmt(pair: pest::iterators::Pair<Rule>) -> Result<Stmt> {
    match pair.as_rule() {
        Rule::let_stmt => {
            let mut inner = pair.into_inner();
            let name = inner.next().unwrap().as_str().to_string();
            let expr = parse_expr(inner.next().unwrap())?;
            Ok(Stmt::Let { name, expr })
        }
        Rule::assign_stmt => {
            let mut inner = pair.into_inner();
            let name = inner.next().unwrap().as_str().to_string();
            let expr = parse_expr(inner.next().unwrap())?;
            Ok(Stmt::Assign { name, expr })
        }
        Rule::expr_stmt => {
            let inner = pair.into_inner().next().unwrap();
            Ok(Stmt::Expr(parse_expr(inner)?))
        }
        Rule::stmt => {
            let inner = pair.into_inner().next().unwrap();
            parse_stmt(inner)
        }
        Rule::expr | Rule::pipeline => Ok(Stmt::Expr(parse_expr(pair)?)),
        other => bail!("unexpected stmt rule: {:?}", other),
    }
}

fn parse_expr(pair: pest::iterators::Pair<Rule>) -> Result<Expr> {
    match pair.as_rule() {
        Rule::expr | Rule::pipeline => {
            let mut inner = pair.into_inner();
            let first = inner.next().ok_or_else(|| anyhow!("empty expr"))?;
            let base = parse_atom(first)?;
            let mut steps = vec![];
            for tail in inner {
                if tail.as_rule() == Rule::pipe_tail {
                    let mut tinner = tail.into_inner();
                    let atom = parse_atom(tinner.next().unwrap())?;
                    let mut tags = vec![];
                    for tg in tinner {
                        if tg.as_rule() == Rule::tag {
                            tags.push(parse_tag(tg)?);
                        }
                    }
                    steps.push(PipeStep { expr: atom, tags });
                }
            }
            if steps.is_empty() {
                Ok(base)
            } else {
                Ok(Expr::Pipeline { base: Box::new(base), steps })
            }
        }
        _ => parse_atom(pair),
    }
}

fn parse_atom(pair: pest::iterators::Pair<Rule>) -> Result<Expr> {
    match pair.as_rule() {
        Rule::ident => Ok(Expr::Ident(pair.as_str().to_string())),
        Rule::field_access => {
            let mut inner = pair.into_inner();
            let base = inner.next().unwrap().as_str().to_string();
            let field = inner.next().unwrap().as_str().to_string();
            Ok(Expr::FieldAccess { base, field })
        }
        Rule::literal => parse_literal(pair),
        Rule::call => {
            let mut inner = pair.into_inner();
            let name = inner.next().unwrap().as_str().to_string();
            let mut args = vec![];
            for a in inner {
                args.push(parse_expr(a)?);
            }
            Ok(Expr::Call { name, args })
        }
        Rule::atom => {
            let inner = pair.into_inner().next().unwrap();
            parse_atom(inner)
        }
        Rule::expr | Rule::pipeline => parse_expr(pair),
        other => bail!("unexpected atom rule: {:?}", other),
    }
}

fn parse_tag(pair: pest::iterators::Pair<Rule>) -> Result<Tag> {
    let full = pair.as_str().to_string();
    let mut inner = pair.into_inner();
    if full.starts_with("@timeout") {
        let expr = parse_expr(inner.next().unwrap())?;
        Ok(Tag::Timeout(expr))
    } else if full.starts_with("@recover") {
        let expr = parse_expr(inner.next().unwrap())?;
        Ok(Tag::Recover { with: expr })
    } else {
        Ok(Tag::Error)
    }
}

fn parse_literal(pair: pest::iterators::Pair<Rule>) -> Result<Expr> {
    let inner = pair.into_inner().next().unwrap();
    match inner.as_rule() {
        Rule::duration => {
            let s = inner.as_str();
            let num: u64 = s.trim_end_matches(".ms").parse()?;
            Ok(Expr::Literal(Literal::DurationMs(num)))
        }
        Rule::string => {
            let s = inner.as_str();
            Ok(Expr::Literal(Literal::String(s[1..s.len()-1].to_string())))
        }
        Rule::number => {
            let s = inner.as_str();
            if s.contains('.') {
                Ok(Expr::Literal(Literal::Float(s.parse()?)))
            } else {
                Ok(Expr::Literal(Literal::Int(s.parse()?)))
            }
        }
        Rule::boolean => Ok(Expr::Literal(Literal::Bool(inner.as_str() == "true"))),
        _ => bail!("bad literal"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ingest_example() {
        let src = include_str!("../../examples/ingest.sigil");
        let prog = parse(src).expect("should parse ingest.sigil");
        assert_eq!(prog.schemas.len(), 2);
        assert_eq!(prog.processes.len(), 1);
        let p = &prog.processes[0];
        assert_eq!(p.name, "Ingest");
        assert_eq!(p.states.len(), 1);
        assert_eq!(p.states[0].name, "last");
        assert_eq!(p.handlers.len(), 1);
        assert_eq!(p.handlers[0].msg_name, "packet");
        // body should have several statements
        assert!(p.handlers[0].body.len() >= 3);
    }
}
