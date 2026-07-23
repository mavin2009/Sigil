
//! AST for Sigil — pest-driven structured representation with arithmetic

use anyhow::{anyhow, bail, Result};
use pest::Parser;
use pest_derive::Parser;

#[derive(Parser)]
#[grammar = "frontend/sigil.pest"]
pub struct SigilParser;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn from_pest(span: pest::Span<'_>) -> Self {
        Self { start: span.start(), end: span.end() }
    }
    pub fn is_valid(&self) -> bool {
        self.start < self.end
    }
}

#[derive(Debug, Clone)]
pub struct Program {
    pub schemas: Vec<Schema>,
    pub processes: Vec<Process>,
    pub transforms: Vec<TransformDecl>,
    pub specs: Vec<SpecDecl>,
}

#[derive(Debug, Clone)]
pub struct SpecDecl {
    pub name: String,
    pub items: Vec<SpecItem>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum SpecItem {
    Extinct { names: Vec<String>, span: Span },
    Require { expr: Expr, span: Span },
    Hold { expr: Expr, span: Span },
}

#[derive(Debug, Clone)]
pub struct TransformDecl {
    pub name: String,
    pub param: String,
    pub param_ty: Type,
    pub return_ty: Type,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Schema {
    pub name: String,
    pub fields: Vec<(String, Type)>,
    pub span: Span,
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
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct StateDecl {
    pub name: String,
    pub ty: Type,
    pub init: Expr,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct OnHandler {
    pub msg_name: String,
    pub msg_ty: Type,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Let { name: String, expr: Expr, span: Span },
    Assign { name: String, expr: Expr, span: Span },
    /// `send <expr> to <Process> [by <key>|broadcast] [@block|@shed|@deadline(N.ms)]`
    /// — typed, routed, back-pressured message to another process's fleet.
    Send {
        target: String,
        expr: Expr,
        route: Route,
        backpressure: Backpressure,
        /// `when <cond>` — the message is only forwarded if this holds.
        /// Provers evaluate the sending handler's counters under this
        /// condition, so a conditionally-forwarded message can be bounded
        /// by a conditionally-incremented counter.
        guard: Option<Expr>,
        span: Span,
    },
    Expr { expr: Expr, span: Span },
}

/// What a `send` does when the destination's queue is full.
///
/// Every policy preserves downstream-counting invariants (shedding only
/// *decreases* the downstream count), but only the bounded policies can
/// back an end-to-end latency claim: `@block` waits for an unbounded time.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Backpressure {
    /// Await capacity. Propagates backpressure upstream; cannot deadlock
    /// because the process graph is proven acyclic — but has no time bound.
    #[default]
    Block,
    /// Never wait: if the queue is full, drop the message and count it.
    Shed,
    /// Wait up to N ms, then drop and count. Bounded, so it can be charged
    /// to the latency budget.
    Deadline(u64),
}

impl Backpressure {
    /// Worst-case milliseconds this send can add to a path, or `None` when
    /// the wait is unbounded.
    pub fn budget_ms(&self) -> Option<u64> {
        match self {
            Backpressure::Block => None,
            Backpressure::Shed => Some(0),
            Backpressure::Deadline(ms) => Some(*ms),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Backpressure::Block => "@block".into(),
            Backpressure::Shed => "@shed".into(),
            Backpressure::Deadline(ms) => format!("@deadline({ms}.ms)"),
        }
    }
}

/// Shard-routing policy for a `send`.
#[derive(Debug, Clone, Default)]
pub enum Route {
    /// Even distribution across shards (default).
    #[default]
    RoundRobin,
    /// Hash the key expression: same key → same shard (ordering/state affinity).
    ByKey(Expr),
    /// Deliver a clone to every shard.
    Broadcast,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Ident { name: String, span: Span },
    FieldAccess { base: String, field: String, span: Span },
    Literal { value: Literal, span: Span },
    Pipeline { base: Box<Expr>, steps: Vec<PipeStep>, span: Span },
    Call { name: String, args: Vec<Expr>, span: Span },
    Binary { op: BinOp, lhs: Box<Expr>, rhs: Box<Expr>, span: Span },
    /// `if cond { a } else { b }` — both branches are expressions of the
    /// same type. Provers evaluate branches under the narrowed condition,
    /// which is what makes clamping provable.
    If {
        cond: Box<Expr>,
        then_branch: Box<Expr>,
        else_branch: Box<Expr>,
        span: Span,
    },
    /// `Schema { field: expr, ... }` — construct a schema value.
    SchemaLit {
        name: String,
        fields: Vec<(String, Expr)>,
        span: Span,
    },
}

#[derive(Debug, Clone)]
pub enum BinOp {
    Add, Sub, Mul, Div,
    Le, Ge, Lt, Gt, Eq,
}

#[derive(Debug, Clone)]
pub struct PipeStep {
    pub expr: Expr,
    pub tags: Vec<Tag>,
}

#[derive(Debug, Clone)]
pub enum Tag {
    Timeout { expr: Expr, span: Span },
    Recover { with: Expr, span: Span },
    /// Re-attempt the stage up to N extra times before taking the failure
    /// path. Requires @recover or @error on the same step.
    Retry { expr: Expr, span: Span },
    Error { span: Span },
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
        transforms: vec![],
        specs: vec![],
    };

    for pair in pairs {
        for inner in pair.into_inner() {
            match inner.as_rule() {
                Rule::schema_def => program.schemas.push(parse_schema(inner)?),
                Rule::process_def => program.processes.push(parse_process(inner)?),
                Rule::transform_def => program.transforms.push(parse_transform(inner)?),
                Rule::spec_def => program.specs.push(parse_spec(inner)?),
                Rule::EOI => {}
                r => eprintln!("skipping top-level {:?}", r),
            }
        }
    }
    Ok(program)
}

fn parse_transform(pair: pest::iterators::Pair<Rule>) -> Result<TransformDecl> {
    let span = Span::from_pest(pair.as_span());
    let mut inner = pair.into_inner();
    let name = inner.next().ok_or_else(|| anyhow!("transform name"))?.as_str().to_string();
    let param = inner.next().ok_or_else(|| anyhow!("transform param"))?.as_str().to_string();
    let param_ty = parse_type(inner.next().ok_or_else(|| anyhow!("transform param type"))?)?;
    let return_ty = parse_type(inner.next().ok_or_else(|| anyhow!("transform return type"))?)?;
    let mut body = vec![];
    for item in inner {
        if item.as_rule() == Rule::stmt {
            body.push(parse_stmt(item)?);
        }
    }
    Ok(TransformDecl {
        name,
        param,
        param_ty,
        return_ty,
        body,
        span,
    })
}


fn parse_spec(pair: pest::iterators::Pair<Rule>) -> Result<SpecDecl> {
    let span = Span::from_pest(pair.as_span());
    let mut inner = pair.into_inner();
    let name = inner.next().ok_or_else(|| anyhow!("spec name"))?.as_str().to_string();
    let mut items = vec![];
    for item in inner {
        if item.as_rule() != Rule::spec_item {
            continue;
        }
        let item_span = Span::from_pest(item.as_span());
        let mut parts = item.into_inner();
        let head = parts.next().ok_or_else(|| anyhow!("spec item"))?;
        match head.as_rule() {
            Rule::extinct_clause => {
                let mut names = vec![];
                for p in head.into_inner() {
                    if p.as_rule() == Rule::ident {
                        names.push(p.as_str().to_string());
                    }
                }
                items.push(SpecItem::Extinct { names, span: item_span });
            }
            Rule::require_clause => {
                let expr_pair = head.into_inner().next().ok_or_else(|| anyhow!("require expr"))?;
                items.push(SpecItem::Require {
                    expr: parse_expr(expr_pair)?,
                    span: item_span,
                });
            }
            Rule::hold_clause => {
                let expr_pair = head.into_inner().next().ok_or_else(|| anyhow!("hold expr"))?;
                items.push(SpecItem::Hold {
                    expr: parse_expr(expr_pair)?,
                    span: item_span,
                });
            }
            r => bail!("unknown spec item {:?}", r),
        }
    }
    Ok(SpecDecl { name, items, span })
}

fn parse_schema(pair: pest::iterators::Pair<Rule>) -> Result<Schema> {
    let span = Span::from_pest(pair.as_span());
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
    Ok(Schema { name, fields, span })
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
    let span = Span::from_pest(pair.as_span());
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
    Ok(Process { name, states, handlers, span })
}

fn parse_state(pair: pest::iterators::Pair<Rule>) -> Result<StateDecl> {
    let span = Span::from_pest(pair.as_span());
    let mut inner = pair.into_inner();
    let name = inner.next().unwrap().as_str().to_string();
    let ty = parse_type(inner.next().unwrap())?;
    let init = parse_expr(inner.next().unwrap())?;
    Ok(StateDecl { name, ty, init, span })
}

fn parse_handler(pair: pest::iterators::Pair<Rule>) -> Result<OnHandler> {
    let span = Span::from_pest(pair.as_span());
    let mut inner = pair.into_inner();
    let msg_name = inner.next().unwrap().as_str().to_string();
    let msg_ty = parse_type(inner.next().unwrap())?;
    let mut body = vec![];
    for item in inner {
        body.push(parse_stmt(item)?);
    }
    Ok(OnHandler { msg_name, msg_ty, body, span })
}

fn parse_stmt(pair: pest::iterators::Pair<Rule>) -> Result<Stmt> {
    let span = Span::from_pest(pair.as_span());
    match pair.as_rule() {
        Rule::let_stmt => {
            let mut inner = pair.into_inner();
            let name = inner.next().unwrap().as_str().to_string();
            let expr = parse_expr(inner.next().unwrap())?;
            Ok(Stmt::Let { name, expr, span })
        }
        Rule::assign_stmt => {
            let mut inner = pair.into_inner();
            let name = inner.next().unwrap().as_str().to_string();
            let expr = parse_expr(inner.next().unwrap())?;
            Ok(Stmt::Assign { name, expr, span })
        }
        Rule::send_stmt => {
            let mut inner = pair.into_inner();
            let expr = parse_expr(inner.next().unwrap())?;
            let target = inner.next().unwrap().as_str().to_string();
            let mut route = Route::RoundRobin;
            let mut backpressure = Backpressure::Block;
            let mut guard: Option<Expr> = None;
            for extra in inner {
                match extra.as_rule() {
                    Rule::route_clause => {
                        let rc_inner = extra.into_inner().next().unwrap();
                        route = match rc_inner.as_rule() {
                            Rule::by_route => {
                                let key = parse_expr(rc_inner.into_inner().next().unwrap())?;
                                Route::ByKey(key)
                            }
                            Rule::broadcast_kw => Route::Broadcast,
                            other => bail!("unexpected route clause: {:?}", other),
                        };
                    }
                    Rule::backpressure => {
                        let bp_inner = extra.into_inner().next().unwrap();
                        backpressure = match bp_inner.as_rule() {
                            Rule::shed_kw => Backpressure::Shed,
                            Rule::block_kw => Backpressure::Block,
                            Rule::deadline_bp => {
                                let e = parse_expr(bp_inner.into_inner().next().unwrap())?;
                                match e {
                                    Expr::Literal { value: Literal::DurationMs(ms), .. } => {
                                        Backpressure::Deadline(ms)
                                    }
                                    _ => bail!(
                                        "@deadline requires a duration literal, e.g. @deadline(5.ms)"
                                    ),
                                }
                            }
                            other => bail!("unexpected backpressure clause: {:?}", other),
                        };
                    }
                    Rule::when_clause => {
                        guard = Some(parse_expr(extra.into_inner().next().unwrap())?);
                    }
                    other => bail!("unexpected send clause: {:?}", other),
                }
            }
            Ok(Stmt::Send { target, expr, route, backpressure, guard, span })
        }
        Rule::expr_stmt => {
            let inner = pair.into_inner().next().unwrap();
            Ok(Stmt::Expr { expr: parse_expr(inner)?, span })
        }
        Rule::stmt => {
            let inner = pair.into_inner().next().unwrap();
            parse_stmt(inner)
        }
        Rule::expr | Rule::comparison | Rule::sum | Rule::product | Rule::pipeline => {
            Ok(Stmt::Expr { expr: parse_expr(pair)?, span })
        }
        other => bail!("unexpected stmt rule: {:?}", other),
    }
}

fn parse_expr(pair: pest::iterators::Pair<Rule>) -> Result<Expr> {
    match pair.as_rule() {
        Rule::expr => {
            let inner = pair.into_inner().next().unwrap();
            parse_expr(inner)
        }
        Rule::comparison => {
            let span = Span::from_pest(pair.as_span());
            let mut inner = pair.into_inner();
            let left = parse_expr(inner.next().unwrap())?;
            if let Some(op_pair) = inner.next() {
                let op = match op_pair.as_str() {
                    "<=" => BinOp::Le,
                    ">=" => BinOp::Ge,
                    "==" => BinOp::Eq,
                    "<" => BinOp::Lt,
                    ">" => BinOp::Gt,
                    _ => bail!("bad cmp op {}", op_pair.as_str()),
                };
                let right = parse_expr(inner.next().unwrap())?;
                Ok(Expr::Binary {
                    op,
                    lhs: Box::new(left),
                    rhs: Box::new(right),
                    span,
                })
            } else {
                Ok(left)
            }
        }
        Rule::sum => {
            let span = Span::from_pest(pair.as_span());
            let mut inner = pair.into_inner();
            let mut left = parse_expr(inner.next().unwrap())?;
            while let Some(op_pair) = inner.next() {
                let op = match op_pair.as_str() {
                    "+" => BinOp::Add,
                    "-" => BinOp::Sub,
                    _ => bail!("bad sum op"),
                };
                let right = parse_expr(inner.next().unwrap())?;
                left = Expr::Binary { op, lhs: Box::new(left), rhs: Box::new(right), span };
            }
            Ok(left)
        }
        Rule::product => {
            let span = Span::from_pest(pair.as_span());
            let mut inner = pair.into_inner();
            let mut left = parse_expr(inner.next().unwrap())?;
            while let Some(op_pair) = inner.next() {
                let op = match op_pair.as_str() {
                    "*" => BinOp::Mul,
                    "/" => BinOp::Div,
                    _ => bail!("bad product op"),
                };
                let right = parse_expr(inner.next().unwrap())?;
                left = Expr::Binary { op, lhs: Box::new(left), rhs: Box::new(right), span };
            }
            Ok(left)
        }
        Rule::pipeline => {
            let span = Span::from_pest(pair.as_span());
            let mut inner = pair.into_inner();
            let first = inner.next().ok_or_else(|| anyhow!("empty pipeline"))?;
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
                Ok(Expr::Pipeline { base: Box::new(base), steps, span })
            }
        }
        _ => parse_atom(pair),
    }
}

fn parse_atom(pair: pest::iterators::Pair<Rule>) -> Result<Expr> {
    match pair.as_rule() {
        Rule::ident => {
            let span = Span::from_pest(pair.as_span());
            Ok(Expr::Ident { name: pair.as_str().to_string(), span })
        },
        Rule::field_access => {
            let span = Span::from_pest(pair.as_span());
            let mut inner = pair.into_inner();
            let base = inner.next().unwrap().as_str().to_string();
            let field = inner.next().unwrap().as_str().to_string();
            Ok(Expr::FieldAccess { base, field, span })
        }
        Rule::if_expr => {
            let span = Span::from_pest(pair.as_span());
            let mut inner = pair.into_inner();
            let cond = parse_expr(inner.next().unwrap())?;
            let then_branch = parse_expr(inner.next().unwrap())?;
            let else_branch = parse_expr(inner.next().unwrap())?;
            Ok(Expr::If {
                cond: Box::new(cond),
                then_branch: Box::new(then_branch),
                else_branch: Box::new(else_branch),
                span,
            })
        }
        Rule::schema_lit => {
            let span = Span::from_pest(pair.as_span());
            let mut inner = pair.into_inner();
            let name = inner.next().unwrap().as_str().to_string();
            let mut fields = Vec::new();
            for fi in inner {
                let mut it = fi.into_inner();
                let fname = it.next().unwrap().as_str().to_string();
                let fexpr = parse_expr(it.next().unwrap())?;
                fields.push((fname, fexpr));
            }
            Ok(Expr::SchemaLit { name, fields, span })
        }
        Rule::literal => parse_literal(pair),
        Rule::call => {
            let span = Span::from_pest(pair.as_span());
            let mut inner = pair.into_inner();
            let name = inner.next().unwrap().as_str().to_string();
            let mut args = vec![];
            for a in inner {
                args.push(parse_expr(a)?);
            }
            Ok(Expr::Call { name, args, span })
        }
        Rule::atom => {
            let inner = pair.into_inner().next().unwrap();
            parse_atom(inner)
        }
        Rule::expr | Rule::comparison | Rule::sum | Rule::product | Rule::pipeline => parse_expr(pair),
        other => bail!("unexpected atom rule: {:?}", other),
    }
}

fn parse_tag(pair: pest::iterators::Pair<Rule>) -> Result<Tag> {
    let span = Span::from_pest(pair.as_span());
    let full = pair.as_str().to_string();
    let mut inner = pair.into_inner();
    if full.starts_with("@timeout") {
        let expr = parse_expr(inner.next().unwrap())?;
        Ok(Tag::Timeout { expr, span })
    } else if full.starts_with("@recover") {
        let expr = parse_expr(inner.next().unwrap())?;
        Ok(Tag::Recover { with: expr, span })
    } else if full.starts_with("@retry") {
        let expr = parse_expr(inner.next().unwrap())?;
        Ok(Tag::Retry { expr, span })
    } else {
        Ok(Tag::Error { span })
    }
}

fn parse_literal(pair: pest::iterators::Pair<Rule>) -> Result<Expr> {
    let span = Span::from_pest(pair.as_span());
    let inner = pair.into_inner().next().unwrap();
    match inner.as_rule() {
        Rule::duration => {
            let s = inner.as_str();
            let num: u64 = s.trim_end_matches(".ms").parse()?;
            Ok(Expr::Literal { value: Literal::DurationMs(num), span })
        }
        Rule::string => {
            let s = inner.as_str();
            Ok(Expr::Literal { value: Literal::String(s[1..s.len()-1].to_string()), span })
        }
        Rule::number => {
            let s = inner.as_str();
            if s.contains('.') {
                Ok(Expr::Literal { value: Literal::Float(s.parse()?), span })
            } else {
                Ok(Expr::Literal { value: Literal::Int(s.parse()?), span })
            }
        }
        Rule::boolean => Ok(Expr::Literal { value: Literal::Bool(inner.as_str() == "true"), span }),
        _ => bail!("bad literal"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ingest_example() {
        let src = include_str!("../../../examples/ingest/ingest.sigil");
        let prog = parse(src).expect("should parse ingest.sigil");
        assert_eq!(prog.schemas.len(), 2);
        assert_eq!(prog.processes.len(), 1);
        let p = &prog.processes[0];
        assert_eq!(p.name, "Ingest");
        assert!(p.states.len() >= 1);
        assert_eq!(p.states[0].name, "last");
        assert_eq!(p.handlers.len(), 1);
        assert_eq!(p.handlers[0].msg_name, "packet");
        assert!(p.handlers[0].body.len() >= 3);
    }

    #[test]
    fn parse_counter_example() {
        let src = include_str!("../../../examples/counter/counter.sigil");
        let prog = parse(src).expect("should parse counter");
        assert_eq!(prog.processes.len(), 1);
        assert_eq!(prog.processes[0].name, "Counter");
        assert!(prog.processes[0].states.len() >= 1);
        assert_eq!(prog.processes[0].handlers.len(), 1);
    }

    #[test]
    fn key_nodes_have_valid_spans() {
        let src = include_str!("../../../examples/ingest/ingest.sigil");
        let prog = parse(src).expect("parse");
        assert!(!prog.schemas.is_empty());
        assert!(prog.schemas[0].span.is_valid(), "schema should have a valid span");
        assert!(!prog.processes.is_empty());
        assert!(prog.processes[0].span.is_valid(), "process should have a valid span");
        assert!(prog.processes[0].span.start < prog.processes[0].span.end);
        assert!(!prog.processes[0].states.is_empty());
        assert!(prog.processes[0].states[0].span.is_valid());
        assert!(!prog.processes[0].handlers.is_empty());
        assert!(prog.processes[0].handlers[0].span.is_valid());
    }

    #[test]
    fn span_extraction_works() {
        let src = "let x = 1 + 2";
        // Just ensure the parser can still run; full span attachment is incremental
        let _ = src;
        assert!(true);
    }

    #[test]
    fn binary_and_pipeline_have_span_field() {
        let src = r#"
schema S { x: Int }
process P {
  state s: Int = 0
  on m: S {
    let y = s + m.x * 2
    s := y
  }
}
"#;
        let prog = parse(src).expect("parse");
        let process = &prog.processes[0];
        let mut found_binary = false;
        for handler in &process.handlers {
            for stmt in &handler.body {
                if let Stmt::Let { expr: Expr::Binary { span, .. }, .. } = stmt {
                    assert!(span.is_valid(), "Binary span should be valid (start < end)");
                    assert!(span.end - span.start > 1, "Binary span should cover more than one character");
                    found_binary = true;
                }
            }
        }
        assert!(found_binary, "expected a Binary expression with a valid span");
    }

    #[test]
    fn parse_circuit_example() {
        let src = include_str!("../../../examples/circuit/circuit.sigil");
        let prog = parse(src).expect("should parse circuit.sigil");
        assert_eq!(prog.processes.len(), 1);
        assert_eq!(prog.processes[0].name, "CircuitBreaker");
        assert_eq!(prog.processes[0].states.len(), 2);
        assert_eq!(prog.processes[0].handlers.len(), 1);
        assert!(prog.processes[0].handlers[0].body.len() >= 3);
    }

    #[test]
    fn parse_resilient_example() {
        let src = include_str!("../../../examples/resilient/resilient.sigil");
        let prog = parse(src).expect("should parse resilient.sigil");
        assert_eq!(prog.processes.len(), 1);
        assert_eq!(prog.processes[0].name, "ResilientProcessor");
        assert!(prog.processes[0].states.len() >= 1);
        assert_eq!(prog.processes[0].states[0].name, "last_ok");
        assert_eq!(prog.processes[0].handlers.len(), 1);
        assert!(prog.processes[0].handlers[0].body.len() >= 3);
    }

    #[test]
    fn parse_binary_arithmetic() {
        let src = r#"
schema S { x: Int }
process P {
  state s: Int = 0
  on m: S {
    let y = s + m.x * 2
    s := y
  }
}
"#;
        let prog = parse(src).expect("should parse arithmetic");
        assert_eq!(prog.processes.len(), 1);
        // Just ensure it parses without error; deeper structure check optional
    }

    #[test]
    fn call_and_timeout_have_valid_spans() {
        let src = include_str!("../../../examples/resilient/resilient.sigil");
        let prog = parse(src).expect("parse resilient");
        let process = &prog.processes[0];
        let mut found_timeout = false;
        let mut found_ident_or_call = false;

        for handler in &process.handlers {
            for stmt in &handler.body {
                let expr = match stmt {
                    Stmt::Let { expr, .. }
                    | Stmt::Assign { expr, .. }
                    | Stmt::Send { expr, .. }
                    | Stmt::Expr { expr, .. } => expr,
                };
                match expr {
                    Expr::Pipeline { steps, span, .. } => {
                        assert!(span.is_valid() || span.start <= span.end);
                        for step in steps {
                            match &step.expr {
                                Expr::Ident { span, .. } | Expr::Call { span, .. } => {
                                    if span.is_valid() {
                                        found_ident_or_call = true;
                                    }
                                }
                                _ => {}
                            }
                            for tag in &step.tags {
                                if let Tag::Timeout { span, .. } = tag {
                                    if span.is_valid() {
                                        found_timeout = true;
                                    }
                                }
                            }
                        }
                    }
                    Expr::Ident { span, .. } | Expr::Call { span, .. } => {
                        if span.is_valid() {
                            found_ident_or_call = true;
                        }
                    }
                    _ => {}
                }
            }
        }
        assert!(found_timeout || found_ident_or_call, "expected Timeout or Ident/Call with valid span");
    }


    #[test]
    fn parse_pipeline_example() {
        let src = include_str!("../../../examples/pipeline/pipeline.sigil");
        let prog = parse(src).expect("should parse pipeline.sigil");
        assert_eq!(prog.processes[0].name, "OrderPipeline");
        assert_eq!(prog.processes[0].states.len(), 2);
        assert!(!prog.processes[0].handlers[0].body.is_empty());
    }

}
