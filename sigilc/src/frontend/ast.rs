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
        Self {
            start: span.start(),
            end: span.end(),
        }
    }
    pub fn is_valid(&self) -> bool {
        self.start < self.end
    }
}

#[derive(Debug, Clone)]
pub struct Program {
    pub extern_crates: Vec<ExternCrate>,
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

/// Where a generated crate should get a Rust dependency from.
#[derive(Debug, Clone, PartialEq)]
pub enum CrateSource {
    Version(String),
    Path(String),
}

/// A Rust dependency declared in the .sigil source, so the generated crate
/// is complete rather than something you must hand-edit afterwards.
#[derive(Debug, Clone)]
pub struct ExternCrate {
    pub name: String,
    pub source: CrateSource,
    pub span: Span,
}

/// How a bound Rust function must be called.
///
/// The distinction is not cosmetic. A blocking call made directly from an
/// async handler stalls a runtime worker thread — a failure mode invisible to
/// every proof in this compiler, because it degrades the scheduler rather
/// than the program. Declaring it lets codegen place the call correctly.
#[derive(Debug, Clone, PartialEq)]
pub enum BindKind {
    /// `async fn(T) -> Result<U, E>` — awaited directly.
    Async,
    /// `fn(T) -> Result<U, E>` that blocks — dispatched to a blocking pool.
    Blocking,
    /// `fn(T) -> U` — cannot fail, but is still foreign code.
    Infallible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Idempotency {
    Idempotent,
    NonIdempotent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cancellation {
    /// Dropping the async future stops work without leaving an effect in flight.
    CancelSafe,
    /// Work may outlive the caller but its completion remains runtime-accounted.
    CompletionTracked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideEffect {
    None,
    Read,
    Write,
}

/// Machine-readable operational contract for a bound foreign transform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectContract {
    pub idempotency: Idempotency,
    pub cancellation: Cancellation,
    pub side_effect: SideEffect,
}

/// A transform bound to an existing Rust function.
#[derive(Debug, Clone)]
pub struct Binding {
    pub kind: BindKind,
    /// Fully-qualified Rust path, e.g. `sensor_hal::read_imu`.
    pub path: String,
    pub effect: EffectContract,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct TransformDecl {
    pub name: String,
    pub param: String,
    pub param_ty: Type,
    pub return_ty: Type,
    pub body: Vec<Stmt>,
    /// Present when the transform is bound to a real Rust function. A bound
    /// transform is still EXTERNAL for every analysis — it performs real I/O,
    /// so it can fail and hang, and still requires a declared failure path.
    /// Binding removes the hand-editing, not the obligation.
    pub binding: Option<Binding>,
    pub span: Span,
}

impl TransformDecl {
    /// Declared as unable to fail: a compiled pure body, or a binding to a
    /// Rust function that returns a value rather than a Result.
    pub fn is_infallible(&self) -> bool {
        matches!(
            self.binding.as_ref().map(|b| &b.kind),
            Some(BindKind::Infallible)
        )
    }

    /// Performs real I/O: an empty stub, or a binding to fallible code.
    pub fn is_external(&self) -> bool {
        match &self.binding {
            Some(b) => !matches!(b.kind, BindKind::Infallible),
            None => self.body.is_empty(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Schema {
    pub name: String,
    /// When set, this schema IS an existing Rust type rather than a struct
    /// the compiler defines. Required for binding transforms to a crate that
    /// already has its own types — otherwise the generated struct and the
    /// foreign one are different types with the same shape.
    pub binding: Option<String>,
    pub fields: Vec<(String, Type)>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Type {
    Int,
    Float,
    String,
    Bool,
    UUID,
    Bytes,
    Duration,
    Named(String),
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
    Let {
        name: String,
        expr: Expr,
        span: Span,
    },
    Assign {
        name: String,
        expr: Expr,
        span: Span,
    },
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
    Expr {
        expr: Expr,
        span: Span,
    },
}

/// What a `send` does when the destination's queue is full.
///
/// Every policy preserves downstream-counting invariants (shedding only
/// *decreases* the downstream count), but only the bounded policies can
/// back an end-to-end latency claim: `@block` waits for an unbounded time.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Backpressure {
    /// Await capacity. An acyclic graph rules out generated channel-wait
    /// cycles, but external code can still prevent downstream progress.
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
    Ident {
        name: String,
        span: Span,
    },
    FieldAccess {
        base: String,
        field: String,
        span: Span,
    },
    Literal {
        value: Literal,
        span: Span,
    },
    Pipeline {
        base: Box<Expr>,
        steps: Vec<PipeStep>,
        span: Span,
    },
    Call {
        name: String,
        args: Vec<Expr>,
        span: Span,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        span: Span,
    },
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
    Add,
    Sub,
    Mul,
    Div,
    Le,
    Ge,
    Lt,
    Gt,
    Eq,
}

#[derive(Debug, Clone)]
pub struct PipeStep {
    pub expr: Expr,
    pub tags: Vec<Tag>,
}

#[derive(Debug, Clone)]
pub enum Tag {
    Timeout {
        expr: Expr,
        span: Span,
    },
    Recover {
        with: Expr,
        span: Span,
    },
    /// Re-attempt the stage up to N extra times before taking the failure
    /// path. Requires @recover or @error on the same step.
    Retry {
        expr: Expr,
        span: Span,
    },
    Error {
        span: Span,
    },
}

#[derive(Debug, Clone)]
pub enum Literal {
    Int(i64),
    Float(f64),
    String(String),
    Bool(bool),
    DurationMs(u64),
}

/// Maximum nesting of `(`/`{`/`[` accepted in a source file.
///
/// The parser is recursive descent (pest's generated code recurses during
/// parsing, before any AST is built), so sufficiently nested input exhausts
/// the stack and aborts the process. An abort is a denial-of-service, not a
/// diagnostic, so depth is bounded up front and reported cleanly. 64 is far
/// beyond any legible program and far below the observed failure point.
pub const MAX_NESTING_DEPTH: usize = 64;
/// Maximum UTF-8 source size accepted by the public parser entry point.
pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;
/// Maximum number of top-level and process-local declarations.
pub const MAX_DECLARATIONS: usize = 10_000;
/// Maximum number of statements across transforms and handlers.
pub const MAX_STATEMENTS: usize = 100_000;
/// Maximum number of expression nodes across the complete compilation unit.
pub const MAX_EXPRESSION_NODES: usize = 200_000;
/// Maximum identifier and string literal sizes.
pub const MAX_IDENTIFIER_BYTES: usize = 128;
pub const MAX_STRING_BYTES: usize = 64 * 1024;

fn required<'i>(
    pairs: &mut pest::iterators::Pairs<'i, Rule>,
    description: &str,
) -> Result<pest::iterators::Pair<'i, Rule>> {
    pairs
        .next()
        .ok_or_else(|| anyhow!("internal parser contract: missing {description}"))
}

/// Reject pathologically nested input before it reaches the parser.
fn check_nesting_depth(source: &str) -> Result<()> {
    let mut depth: usize = 0;
    let mut max_depth: usize = 0;
    let mut deepest_byte: usize = 0;
    let mut in_string = false;
    let mut in_comment = false;
    let mut prev = '\0';

    for (i, ch) in source.char_indices() {
        if in_comment {
            if ch == '\n' {
                in_comment = false;
            }
            prev = ch;
            continue;
        }
        if in_string {
            if ch == '"' && prev != '\\' {
                in_string = false;
            }
            prev = ch;
            continue;
        }
        match ch {
            '"' => in_string = true,
            '/' if prev == '/' => in_comment = true,
            '(' | '{' | '[' => {
                depth += 1;
                if depth > max_depth {
                    max_depth = depth;
                    deepest_byte = i;
                }
                if depth > MAX_NESTING_DEPTH {
                    bail!(
                        "input nests brackets {depth} deep at bytes {i}..{}, exceeding the \
                         limit of {MAX_NESTING_DEPTH} — deeply nested input would exhaust \
                         the parser stack",
                        i + 1
                    );
                }
            }
            ')' | '}' | ']' => depth = depth.saturating_sub(1),
            _ => {}
        }
        prev = ch;
    }
    let _ = (max_depth, deepest_byte);
    Ok(())
}

pub fn parse(source: &str) -> Result<Program> {
    if source.len() > MAX_SOURCE_BYTES {
        bail!(
            "source is {} bytes, exceeding the compiler limit of {MAX_SOURCE_BYTES} bytes",
            source.len()
        );
    }
    check_nesting_depth(source)?;
    let pairs =
        SigilParser::parse(Rule::file, source).map_err(|e| anyhow!("parse error:\n{}", e))?;

    let mut program = Program {
        extern_crates: vec![],
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
                Rule::extern_crate => {
                    let espan = Span::from_pest(inner.as_span());
                    let mut it = inner.into_inner();
                    let name = it
                        .next()
                        .ok_or_else(|| anyhow!("extern crate name"))?
                        .as_str()
                        .to_string();
                    let src_pair = it.next().ok_or_else(|| anyhow!("extern crate source"))?;
                    let source = match src_pair.as_rule() {
                        Rule::path_dep => {
                            let sp = src_pair
                                .into_inner()
                                .next()
                                .ok_or_else(|| anyhow!("path"))?;
                            CrateSource::Path(sp.as_str().trim_matches('"').to_string())
                        }
                        _ => CrateSource::Version(src_pair.as_str().trim_matches('"').to_string()),
                    };
                    program.extern_crates.push(ExternCrate {
                        name,
                        source,
                        span: espan,
                    });
                }
                Rule::transform_def => program.transforms.push(parse_transform(inner)?),
                Rule::spec_def => program.specs.push(parse_spec(inner)?),
                Rule::EOI => {}
                r => bail!("internal parser contract: unexpected top-level rule {r:?}"),
            }
        }
    }
    check_program_limits(&program)?;
    Ok(program)
}

fn check_program_limits(program: &Program) -> Result<()> {
    let declarations = program
        .extern_crates
        .len()
        .checked_add(program.schemas.len())
        .and_then(|n| n.checked_add(program.processes.len()))
        .and_then(|n| n.checked_add(program.transforms.len()))
        .and_then(|n| n.checked_add(program.specs.len()))
        .and_then(|n| {
            program
                .schemas
                .iter()
                .try_fold(n, |acc, schema| acc.checked_add(schema.fields.len()))
        })
        .and_then(|n| {
            program.processes.iter().try_fold(n, |acc, process| {
                acc.checked_add(process.states.len())?
                    .checked_add(process.handlers.len())
            })
        })
        .ok_or_else(|| anyhow!("declaration count overflow"))?;
    if declarations > MAX_DECLARATIONS {
        bail!(
            "program declares {declarations} items, exceeding the compiler limit of \
             {MAX_DECLARATIONS}"
        );
    }

    let statements = program
        .transforms
        .iter()
        .try_fold(0usize, |acc, transform| {
            acc.checked_add(transform.body.len())
        })
        .and_then(|n| {
            program.processes.iter().try_fold(n, |acc, process| {
                process
                    .handlers
                    .iter()
                    .try_fold(acc, |inner, handler| inner.checked_add(handler.body.len()))
            })
        })
        .ok_or_else(|| anyhow!("statement count overflow"))?;
    if statements > MAX_STATEMENTS {
        bail!(
            "program contains {statements} statements, exceeding the compiler limit of \
             {MAX_STATEMENTS}"
        );
    }

    let check_name = |name: &str, kind: &str| -> Result<()> {
        if name.len() > MAX_IDENTIFIER_BYTES {
            bail!(
                "{kind} identifier is {} bytes, exceeding the compiler limit of \
                 {MAX_IDENTIFIER_BYTES}",
                name.len()
            );
        }
        Ok(())
    };
    for schema in &program.schemas {
        check_name(&schema.name, "schema")?;
        for (field, _) in &schema.fields {
            check_name(field, "field")?;
        }
    }
    for process in &program.processes {
        check_name(&process.name, "process")?;
        for state in &process.states {
            check_name(&state.name, "state")?;
        }
        for handler in &process.handlers {
            check_name(&handler.msg_name, "handler message")?;
        }
    }
    for transform in &program.transforms {
        check_name(&transform.name, "transform")?;
        check_name(&transform.param, "transform parameter")?;
    }
    for spec in &program.specs {
        check_name(&spec.name, "spec")?;
    }
    for dependency in &program.extern_crates {
        check_name(&dependency.name, "dependency")?;
    }

    fn check_expr_limits(expr: &Expr, nodes: &mut usize) -> Result<()> {
        *nodes = nodes
            .checked_add(1)
            .ok_or_else(|| anyhow!("expression count overflow"))?;
        if *nodes > MAX_EXPRESSION_NODES {
            bail!(
                "program contains more than {MAX_EXPRESSION_NODES} expression nodes, \
                 exceeding the compiler limit"
            );
        }
        match expr {
            Expr::Ident { name, .. } | Expr::Call { name, .. }
                if name.len() > MAX_IDENTIFIER_BYTES =>
            {
                bail!(
                    "expression identifier is {} bytes, exceeding the compiler limit of \
                     {MAX_IDENTIFIER_BYTES}",
                    name.len()
                )
            }
            Expr::FieldAccess { base, field, .. }
                if base.len() > MAX_IDENTIFIER_BYTES || field.len() > MAX_IDENTIFIER_BYTES =>
            {
                bail!(
                    "field access identifier exceeds the compiler limit of \
                     {MAX_IDENTIFIER_BYTES} bytes"
                )
            }
            Expr::Literal {
                value: Literal::String(value),
                ..
            } if value.len() > MAX_STRING_BYTES => bail!(
                "string literal is {} bytes, exceeding the compiler limit of {MAX_STRING_BYTES}",
                value.len()
            ),
            Expr::Pipeline { base, steps, .. } => {
                check_expr_limits(base, nodes)?;
                for step in steps {
                    check_expr_limits(&step.expr, nodes)?;
                    for tag in &step.tags {
                        match tag {
                            Tag::Timeout { expr, .. } | Tag::Retry { expr, .. } => {
                                check_expr_limits(expr, nodes)?;
                            }
                            Tag::Recover { with, .. } => check_expr_limits(with, nodes)?,
                            Tag::Error { .. } => {}
                        }
                    }
                }
                Ok(())
            }
            Expr::Call { args, .. } => {
                for argument in args {
                    check_expr_limits(argument, nodes)?;
                }
                Ok(())
            }
            Expr::Binary { lhs, rhs, .. } => {
                check_expr_limits(lhs, nodes)?;
                check_expr_limits(rhs, nodes)
            }
            Expr::If {
                cond,
                then_branch,
                else_branch,
                ..
            } => {
                check_expr_limits(cond, nodes)?;
                check_expr_limits(then_branch, nodes)?;
                check_expr_limits(else_branch, nodes)
            }
            Expr::SchemaLit { name, fields, .. } => {
                if name.len() > MAX_IDENTIFIER_BYTES {
                    bail!(
                        "schema literal identifier is {} bytes, exceeding the compiler limit \
                         of {MAX_IDENTIFIER_BYTES}",
                        name.len()
                    );
                }
                for (field, value) in fields {
                    if field.len() > MAX_IDENTIFIER_BYTES {
                        bail!(
                            "schema literal field identifier is {} bytes, exceeding the \
                             compiler limit of {MAX_IDENTIFIER_BYTES}",
                            field.len()
                        );
                    }
                    check_expr_limits(value, nodes)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    let mut expression_nodes = 0usize;
    for process in &program.processes {
        for state in &process.states {
            check_expr_limits(&state.init, &mut expression_nodes)?;
        }
        for handler in &process.handlers {
            for statement in &handler.body {
                let expression = match statement {
                    Stmt::Let { name, expr, .. } | Stmt::Assign { name, expr, .. } => {
                        check_name(name, "local")?;
                        expr
                    }
                    Stmt::Send { expr, .. } | Stmt::Expr { expr, .. } => expr,
                };
                check_expr_limits(expression, &mut expression_nodes)?;
                if let Stmt::Send {
                    target,
                    route,
                    guard,
                    ..
                } = statement
                {
                    check_name(target, "send target")?;
                    if let Route::ByKey(key) = route {
                        check_expr_limits(key, &mut expression_nodes)?;
                    }
                    if let Some(condition) = guard {
                        check_expr_limits(condition, &mut expression_nodes)?;
                    }
                }
            }
        }
    }
    for transform in &program.transforms {
        for statement in &transform.body {
            let expression = match statement {
                Stmt::Let { name, expr, .. } | Stmt::Assign { name, expr, .. } => {
                    check_name(name, "local")?;
                    expr
                }
                Stmt::Send { expr, .. } | Stmt::Expr { expr, .. } => expr,
            };
            check_expr_limits(expression, &mut expression_nodes)?;
        }
    }
    for spec in &program.specs {
        for item in &spec.items {
            match item {
                SpecItem::Extinct { names, .. } => {
                    for name in names {
                        check_name(name, "extinct property")?;
                    }
                }
                SpecItem::Require { expr, .. } | SpecItem::Hold { expr, .. } => {
                    check_expr_limits(expr, &mut expression_nodes)?;
                }
            }
        }
    }
    Ok(())
}

fn parse_transform(pair: pest::iterators::Pair<Rule>) -> Result<TransformDecl> {
    let span = Span::from_pest(pair.as_span());
    let mut inner = pair.into_inner();
    let name = inner
        .next()
        .ok_or_else(|| anyhow!("transform name"))?
        .as_str()
        .to_string();
    let param = inner
        .next()
        .ok_or_else(|| anyhow!("transform param"))?
        .as_str()
        .to_string();
    let param_ty = parse_type(
        inner
            .next()
            .ok_or_else(|| anyhow!("transform param type"))?,
    )?;
    let return_ty = parse_type(
        inner
            .next()
            .ok_or_else(|| anyhow!("transform return type"))?,
    )?;
    let mut body = vec![];
    let mut binding = None;
    for item in inner {
        if item.as_rule() == Rule::binding {
            let bspan = Span::from_pest(item.as_span());
            let mut kind = BindKind::Async;
            let mut path = String::new();
            let mut effect = None;
            for part in item.into_inner() {
                match part.as_rule() {
                    Rule::bind_kind => {
                        kind = match part.as_str() {
                            "blocking" => BindKind::Blocking,
                            "infallible" => BindKind::Infallible,
                            other => bail!("unknown binding kind '{other}'"),
                        };
                    }
                    Rule::rust_path => path = part.as_str().to_string(),
                    Rule::effect_contract => {
                        let mut parts = part.into_inner();
                        let idempotency = match required(&mut parts, "effect idempotency")?.as_str()
                        {
                            "idempotent" => Idempotency::Idempotent,
                            "non_idempotent" => Idempotency::NonIdempotent,
                            other => bail!("unknown idempotency contract '{other}'"),
                        };
                        let cancellation =
                            match required(&mut parts, "effect cancellation")?.as_str() {
                                "cancel_safe" => Cancellation::CancelSafe,
                                "completion_tracked" => Cancellation::CompletionTracked,
                                other => bail!("unknown cancellation contract '{other}'"),
                            };
                        let side_effect = match required(&mut parts, "effect class")?.as_str() {
                            "none" => SideEffect::None,
                            "read" => SideEffect::Read,
                            "write" => SideEffect::Write,
                            other => bail!("unknown side-effect contract '{other}'"),
                        };
                        effect = Some(EffectContract {
                            idempotency,
                            cancellation,
                            side_effect,
                        });
                    }
                    other => bail!("unexpected binding part: {other:?}"),
                }
            }
            binding = Some(Binding {
                kind,
                path,
                effect: effect
                    .ok_or_else(|| anyhow!("bound transform requires an @effect contract"))?,
                span: bspan,
            });
            continue;
        }
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
        binding,
        span,
    })
}

fn parse_spec(pair: pest::iterators::Pair<Rule>) -> Result<SpecDecl> {
    let span = Span::from_pest(pair.as_span());
    let mut inner = pair.into_inner();
    let name = inner
        .next()
        .ok_or_else(|| anyhow!("spec name"))?
        .as_str()
        .to_string();
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
                items.push(SpecItem::Extinct {
                    names,
                    span: item_span,
                });
            }
            Rule::require_clause => {
                let expr_pair = head
                    .into_inner()
                    .next()
                    .ok_or_else(|| anyhow!("require expr"))?;
                items.push(SpecItem::Require {
                    expr: parse_expr(expr_pair)?,
                    span: item_span,
                });
            }
            Rule::hold_clause => {
                let expr_pair = head
                    .into_inner()
                    .next()
                    .ok_or_else(|| anyhow!("hold expr"))?;
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
    let name = required(&mut inner, "schema name")?.as_str().to_string();
    let mut binding = None;
    let mut fields = vec![];
    for part in inner {
        if part.as_rule() == Rule::rust_path {
            binding = Some(part.as_str().to_string());
            continue;
        }
        let fs = part;
        for f in fs.into_inner() {
            if f.as_rule() == Rule::field {
                let mut fi = f.into_inner();
                let fname = required(&mut fi, "schema field name")?.as_str().to_string();
                let fty = parse_type(required(&mut fi, "schema field type")?)?;
                fields.push((fname, fty));
            }
        }
    }
    Ok(Schema {
        name,
        binding,
        fields,
        span,
    })
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
    let name = required(&mut inner, "process name")?.as_str().to_string();
    let body = required(&mut inner, "process body")?;
    let mut states = vec![];
    let mut handlers = vec![];
    for item in body.into_inner() {
        match item.as_rule() {
            Rule::state_decl => states.push(parse_state(item)?),
            Rule::on_handler => handlers.push(parse_handler(item)?),
            _ => {}
        }
    }
    Ok(Process {
        name,
        states,
        handlers,
        span,
    })
}

fn parse_state(pair: pest::iterators::Pair<Rule>) -> Result<StateDecl> {
    let span = Span::from_pest(pair.as_span());
    let mut inner = pair.into_inner();
    let name = required(&mut inner, "state name")?.as_str().to_string();
    let ty = parse_type(required(&mut inner, "state type")?)?;
    let init = parse_expr(required(&mut inner, "state initializer")?)?;
    Ok(StateDecl {
        name,
        ty,
        init,
        span,
    })
}

fn parse_handler(pair: pest::iterators::Pair<Rule>) -> Result<OnHandler> {
    let span = Span::from_pest(pair.as_span());
    let mut inner = pair.into_inner();
    let msg_name = required(&mut inner, "handler message name")?
        .as_str()
        .to_string();
    let msg_ty = parse_type(required(&mut inner, "handler message type")?)?;
    let mut body = vec![];
    for item in inner {
        body.push(parse_stmt(item)?);
    }
    Ok(OnHandler {
        msg_name,
        msg_ty,
        body,
        span,
    })
}

fn parse_stmt(pair: pest::iterators::Pair<Rule>) -> Result<Stmt> {
    let span = Span::from_pest(pair.as_span());
    match pair.as_rule() {
        Rule::let_stmt => {
            let mut inner = pair.into_inner();
            let name = required(&mut inner, "let name")?.as_str().to_string();
            let expr = parse_expr(required(&mut inner, "let expression")?)?;
            Ok(Stmt::Let { name, expr, span })
        }
        Rule::assign_stmt => {
            let mut inner = pair.into_inner();
            let name = required(&mut inner, "assignment name")?
                .as_str()
                .to_string();
            let expr = parse_expr(required(&mut inner, "assignment expression")?)?;
            Ok(Stmt::Assign { name, expr, span })
        }
        Rule::send_stmt => {
            let mut inner = pair.into_inner();
            let expr = parse_expr(required(&mut inner, "send expression")?)?;
            let target = required(&mut inner, "send target")?.as_str().to_string();
            let mut route = Route::RoundRobin;
            let mut backpressure = Backpressure::Block;
            let mut guard: Option<Expr> = None;
            for extra in inner {
                match extra.as_rule() {
                    Rule::route_clause => {
                        let mut route_parts = extra.into_inner();
                        let rc_inner = required(&mut route_parts, "route clause")?;
                        route = match rc_inner.as_rule() {
                            Rule::by_route => {
                                let mut key_parts = rc_inner.into_inner();
                                let key = parse_expr(required(&mut key_parts, "route key")?)?;
                                Route::ByKey(key)
                            }
                            Rule::broadcast_kw => Route::Broadcast,
                            other => bail!("unexpected route clause: {:?}", other),
                        };
                    }
                    Rule::backpressure => {
                        let mut bp_parts = extra.into_inner();
                        let bp_inner = required(&mut bp_parts, "backpressure clause")?;
                        backpressure = match bp_inner.as_rule() {
                            Rule::shed_kw => Backpressure::Shed,
                            Rule::block_kw => Backpressure::Block,
                            Rule::deadline_bp => {
                                let mut deadline_parts = bp_inner.into_inner();
                                let e = parse_expr(required(
                                    &mut deadline_parts,
                                    "backpressure deadline",
                                )?)?;
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
                        let mut guard_parts = extra.into_inner();
                        guard = Some(parse_expr(required(&mut guard_parts, "send guard")?)?);
                    }
                    other => bail!("unexpected send clause: {:?}", other),
                }
            }
            Ok(Stmt::Send {
                target,
                expr,
                route,
                backpressure,
                guard,
                span,
            })
        }
        Rule::expr_stmt => {
            let mut parts = pair.into_inner();
            let inner = required(&mut parts, "expression statement")?;
            Ok(Stmt::Expr {
                expr: parse_expr(inner)?,
                span,
            })
        }
        Rule::stmt => {
            let mut parts = pair.into_inner();
            let inner = required(&mut parts, "statement")?;
            parse_stmt(inner)
        }
        Rule::expr | Rule::comparison | Rule::sum | Rule::product | Rule::pipeline => {
            Ok(Stmt::Expr {
                expr: parse_expr(pair)?,
                span,
            })
        }
        other => bail!("unexpected stmt rule: {:?}", other),
    }
}

fn parse_expr(pair: pest::iterators::Pair<Rule>) -> Result<Expr> {
    match pair.as_rule() {
        Rule::expr => {
            let mut parts = pair.into_inner();
            let inner = required(&mut parts, "expression")?;
            parse_expr(inner)
        }
        Rule::comparison => {
            let span = Span::from_pest(pair.as_span());
            let mut inner = pair.into_inner();
            let left = parse_expr(required(&mut inner, "comparison left operand")?)?;
            if let Some(op_pair) = inner.next() {
                let op = match op_pair.as_str() {
                    "<=" => BinOp::Le,
                    ">=" => BinOp::Ge,
                    "==" => BinOp::Eq,
                    "<" => BinOp::Lt,
                    ">" => BinOp::Gt,
                    _ => bail!("bad cmp op {}", op_pair.as_str()),
                };
                let right = parse_expr(required(&mut inner, "comparison right operand")?)?;
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
            let mut left = parse_expr(required(&mut inner, "sum left operand")?)?;
            while let Some(op_pair) = inner.next() {
                let op = match op_pair.as_str() {
                    "+" => BinOp::Add,
                    "-" => BinOp::Sub,
                    _ => bail!("bad sum op"),
                };
                let right = parse_expr(required(&mut inner, "sum right operand")?)?;
                left = Expr::Binary {
                    op,
                    lhs: Box::new(left),
                    rhs: Box::new(right),
                    span,
                };
            }
            Ok(left)
        }
        Rule::product => {
            let span = Span::from_pest(pair.as_span());
            let mut inner = pair.into_inner();
            let mut left = parse_expr(required(&mut inner, "product left operand")?)?;
            while let Some(op_pair) = inner.next() {
                let op = match op_pair.as_str() {
                    "*" => BinOp::Mul,
                    "/" => BinOp::Div,
                    _ => bail!("bad product op"),
                };
                let right = parse_expr(required(&mut inner, "product right operand")?)?;
                left = Expr::Binary {
                    op,
                    lhs: Box::new(left),
                    rhs: Box::new(right),
                    span,
                };
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
                    let atom = parse_atom(required(&mut tinner, "pipeline stage")?)?;
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
                Ok(Expr::Pipeline {
                    base: Box::new(base),
                    steps,
                    span,
                })
            }
        }
        _ => parse_atom(pair),
    }
}

fn parse_atom(pair: pest::iterators::Pair<Rule>) -> Result<Expr> {
    match pair.as_rule() {
        Rule::ident => {
            let span = Span::from_pest(pair.as_span());
            Ok(Expr::Ident {
                name: pair.as_str().to_string(),
                span,
            })
        }
        Rule::field_access => {
            let span = Span::from_pest(pair.as_span());
            let mut inner = pair.into_inner();
            let base = required(&mut inner, "field base")?.as_str().to_string();
            let field = required(&mut inner, "field name")?.as_str().to_string();
            Ok(Expr::FieldAccess { base, field, span })
        }
        Rule::if_expr => {
            let span = Span::from_pest(pair.as_span());
            let mut inner = pair.into_inner();
            let cond = parse_expr(required(&mut inner, "if condition")?)?;
            let then_branch = parse_expr(required(&mut inner, "if then branch")?)?;
            let else_branch = parse_expr(required(&mut inner, "if else branch")?)?;
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
            let name = required(&mut inner, "schema literal name")?
                .as_str()
                .to_string();
            let mut fields = Vec::new();
            for fi in inner {
                let mut it = fi.into_inner();
                let fname = required(&mut it, "schema literal field")?
                    .as_str()
                    .to_string();
                let fexpr = parse_expr(required(&mut it, "schema literal value")?)?;
                fields.push((fname, fexpr));
            }
            Ok(Expr::SchemaLit { name, fields, span })
        }
        Rule::literal => parse_literal(pair),
        Rule::call => {
            let span = Span::from_pest(pair.as_span());
            let mut inner = pair.into_inner();
            let name = required(&mut inner, "call name")?.as_str().to_string();
            let mut args = vec![];
            for a in inner {
                args.push(parse_expr(a)?);
            }
            Ok(Expr::Call { name, args, span })
        }
        Rule::atom => {
            let mut parts = pair.into_inner();
            let inner = required(&mut parts, "atom")?;
            parse_atom(inner)
        }
        Rule::expr | Rule::comparison | Rule::sum | Rule::product | Rule::pipeline => {
            parse_expr(pair)
        }
        other => bail!("unexpected atom rule: {:?}", other),
    }
}

fn parse_tag(pair: pest::iterators::Pair<Rule>) -> Result<Tag> {
    let span = Span::from_pest(pair.as_span());
    let full = pair.as_str().to_string();
    let mut inner = pair.into_inner();
    if full.starts_with("@timeout") {
        let expr = parse_expr(required(&mut inner, "timeout expression")?)?;
        Ok(Tag::Timeout { expr, span })
    } else if full.starts_with("@recover") {
        let expr = parse_expr(required(&mut inner, "recover expression")?)?;
        Ok(Tag::Recover { with: expr, span })
    } else if full.starts_with("@retry") {
        let expr = parse_expr(required(&mut inner, "retry expression")?)?;
        Ok(Tag::Retry { expr, span })
    } else {
        Ok(Tag::Error { span })
    }
}

fn parse_literal(pair: pest::iterators::Pair<Rule>) -> Result<Expr> {
    let span = Span::from_pest(pair.as_span());
    let mut parts = pair.into_inner();
    let inner = required(&mut parts, "literal value")?;
    match inner.as_rule() {
        Rule::duration => {
            let s = inner.as_str();
            let num: u64 = s.trim_end_matches(".ms").parse()?;
            Ok(Expr::Literal {
                value: Literal::DurationMs(num),
                span,
            })
        }
        Rule::string => {
            let s = inner.as_str();
            Ok(Expr::Literal {
                value: Literal::String(s[1..s.len() - 1].to_string()),
                span,
            })
        }
        Rule::number => {
            let s = inner.as_str();
            if s.contains('.') {
                Ok(Expr::Literal {
                    value: Literal::Float(s.parse()?),
                    span,
                })
            } else {
                Ok(Expr::Literal {
                    value: Literal::Int(s.parse()?),
                    span,
                })
            }
        }
        Rule::boolean => Ok(Expr::Literal {
            value: Literal::Bool(inner.as_str() == "true"),
            span,
        }),
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
        assert!(!p.states.is_empty());
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
        assert!(!prog.processes[0].states.is_empty());
        assert_eq!(prog.processes[0].handlers.len(), 1);
    }

    #[test]
    fn key_nodes_have_valid_spans() {
        let src = include_str!("../../../examples/ingest/ingest.sigil");
        let prog = parse(src).expect("parse");
        assert!(!prog.schemas.is_empty());
        assert!(
            prog.schemas[0].span.is_valid(),
            "schema should have a valid span"
        );
        assert!(!prog.processes.is_empty());
        assert!(
            prog.processes[0].span.is_valid(),
            "process should have a valid span"
        );
        assert!(prog.processes[0].span.start < prog.processes[0].span.end);
        assert!(!prog.processes[0].states.is_empty());
        assert!(prog.processes[0].states[0].span.is_valid());
        assert!(!prog.processes[0].handlers.is_empty());
        assert!(prog.processes[0].handlers[0].span.is_valid());
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
                if let Stmt::Let {
                    expr: Expr::Binary { span, .. },
                    ..
                } = stmt
                {
                    assert!(span.is_valid(), "Binary span should be valid (start < end)");
                    assert!(
                        span.end - span.start > 1,
                        "Binary span should cover more than one character"
                    );
                    found_binary = true;
                }
            }
        }
        assert!(
            found_binary,
            "expected a Binary expression with a valid span"
        );
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
        assert!(!prog.processes[0].states.is_empty());
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
                                Expr::Ident { span, .. } | Expr::Call { span, .. }
                                    if span.is_valid() =>
                                {
                                    found_ident_or_call = true;
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
                    Expr::Ident { span, .. } | Expr::Call { span, .. } if span.is_valid() => {
                        found_ident_or_call = true;
                    }
                    _ => {}
                }
            }
        }
        assert!(
            found_timeout || found_ident_or_call,
            "expected Timeout or Ident/Call with valid span"
        );
    }

    #[test]
    fn parse_pipeline_example() {
        let src = include_str!("../../../examples/pipeline/pipeline.sigil");
        let prog = parse(src).expect("should parse pipeline.sigil");
        assert_eq!(prog.processes[0].name, "OrderPipeline");
        assert_eq!(prog.processes[0].states.len(), 2);
        assert!(!prog.processes[0].handlers[0].body.is_empty());
    }

    #[test]
    fn rejects_oversized_sources_and_identifiers_before_compilation() {
        let oversized = " ".repeat(MAX_SOURCE_BYTES + 1);
        let error = parse(&oversized)
            .expect_err("oversized source must be rejected")
            .to_string();
        assert!(error.contains("source is") && error.contains("compiler limit"));

        let long_name = "x".repeat(MAX_IDENTIFIER_BYTES + 1);
        let source =
            format!("schema M {{ value: Int }}\nprocess P {{ on m: M {{ let {long_name} = 1 }} }}");
        let error = parse(&source)
            .expect_err("oversized identifier must be rejected")
            .to_string();
        assert!(error.contains("identifier") && error.contains("compiler limit"));
    }

    #[test]
    fn rejects_oversized_strings_and_excessive_nesting() {
        let string = "x".repeat(MAX_STRING_BYTES + 1);
        let source = format!(
            "schema M {{ value: String }}\nprocess P {{ on m: M {{ let x = \"{string}\" }} }}"
        );
        let error = parse(&source)
            .expect_err("oversized string must be rejected")
            .to_string();
        assert!(error.contains("string literal") && error.contains("compiler limit"));

        let nested = format!(
            "{}1{}",
            "(".repeat(MAX_NESTING_DEPTH + 1),
            ")".repeat(MAX_NESTING_DEPTH + 1)
        );
        let error = parse(&nested)
            .expect_err("excessive nesting must be rejected")
            .to_string();
        assert!(error.contains("nests brackets"));
    }
}
