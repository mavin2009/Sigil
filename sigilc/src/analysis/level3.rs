//! Level 3 — inductive proof of `hold` invariants.
//!
//! For each `hold <state> <cmp> <literal>` the prover establishes:
//!   BASE:      the declared init value satisfies the predicate;
//!   INDUCTIVE: assuming every held state satisfies its predicate, every
//!              reachable assignment to the state re-establishes it.
//!
//! The abstract domain is exact integer intervals for Rust `i64` operations
//! that complete without overflow. Generated crates enable overflow checks in
//! every profile, so an overflowing assignment fails the actor before it can
//! install a wrapped value. `Float` remains an executable language type but is
//! deliberately outside the proof fragment: accepting an IEEE-754 theorem
//! requires explicit rounding, NaN, infinity, and signed-zero semantics.
//! Anything else — values flowing through external transforms or unguarded
//! inputs — is the full `i64` range, so an insufficient assumption fails
//! closed instead of being guessed.
//!
//! Input assumptions are written `require <msg>.<field> <cmp> <literal>` in
//! a spec. They are not taken on faith: codegen emits a guard at handler
//! entry that rejects (and counts) any message violating them, so every
//! proof assumption is enforced at runtime and the proof is unconditional.

use crate::frontend::ast::{BinOp, Expr, Literal, Process, Program, SpecItem, Stmt, Type};
use anyhow::{bail, Result};
use std::collections::BTreeMap;

const I64_MIN: i128 = i64::MIN as i128;
const I64_MAX: i128 = i64::MAX as i128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interval {
    pub lo: i128,
    pub hi: i128,
}

impl Interval {
    pub const TOP: Interval = Interval {
        lo: I64_MIN,
        hi: I64_MAX,
    };
    const EMPTY: Interval = Interval { lo: 1, hi: 0 };

    fn point(v: i64) -> Self {
        let v = i128::from(v);
        Interval { lo: v, hi: v }
    }

    /// Range of successful Rust `i64` additions. Values outside the `i64`
    /// range panic because generated crates enable overflow checks, so they
    /// cannot become post-state values.
    fn add(self, o: Self) -> Self {
        Self::runtime_range(self.lo + o.lo, self.hi + o.hi)
    }

    /// Range of successful Rust `i64` subtractions.
    fn sub(self, o: Self) -> Self {
        Self::runtime_range(self.lo - o.hi, self.hi - o.lo)
    }

    /// Mathematical interval subtraction used for proof deltas, not for an
    /// emitted arithmetic expression. Delta endpoints can exceed `i64` but
    /// remain within `i128`.
    fn math_sub(self, o: Self) -> Self {
        if self.is_empty() || o.is_empty() {
            Self::EMPTY
        } else {
            Interval {
                lo: self.lo - o.hi,
                hi: self.hi - o.lo,
            }
        }
    }

    fn negate_delta(self) -> Self {
        if self.is_empty() {
            Self::EMPTY
        } else {
            Interval {
                lo: -self.hi,
                hi: -self.lo,
            }
        }
    }

    /// Smallest interval containing both — the join of two branches.
    fn hull(self, o: Self) -> Self {
        if self.is_empty() {
            return o;
        }
        if o.is_empty() {
            return self;
        }
        Interval {
            lo: self.lo.min(o.lo),
            hi: self.hi.max(o.hi),
        }
    }

    fn meet(self, o: Self) -> Self {
        let result = Interval {
            lo: self.lo.max(o.lo),
            hi: self.hi.min(o.hi),
        };
        if result.is_empty() {
            Self::EMPTY
        } else {
            result
        }
    }

    fn mul(self, o: Self) -> Self {
        if self.is_empty() || o.is_empty() {
            return Self::EMPTY;
        }
        let mut lo = i128::MAX;
        let mut hi = i128::MIN;
        for a in [self.lo, self.hi] {
            for b in [o.lo, o.hi] {
                let v = a * b;
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        Self::runtime_range(lo, hi)
    }

    fn runtime_range(lo: i128, hi: i128) -> Self {
        let lo = lo.max(I64_MIN);
        let hi = hi.min(I64_MAX);
        if lo > hi {
            Self::EMPTY
        } else {
            Interval { lo, hi }
        }
    }

    pub(crate) fn is_empty(self) -> bool {
        self.lo > self.hi
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NumericBound {
    Int(i64),
    Float(f64),
}

impl std::fmt::Display for NumericBound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Int(value) => write!(f, "{value}"),
            Self::Float(value) => write!(f, "{value}"),
        }
    }
}

/// A predicate `<cmp> bound` over one variable.
#[derive(Debug, Clone)]
pub struct Pred {
    pub op: BinOp,
    pub bound: NumericBound,
}

impl Pred {
    /// The region this predicate admits, as an interval.
    fn region(&self) -> Option<Interval> {
        let NumericBound::Int(bound) = self.bound else {
            return None;
        };
        let bound = i128::from(bound);
        Some(match self.op {
            BinOp::Ge => Interval {
                lo: bound,
                hi: I64_MAX,
            },
            BinOp::Gt if bound == I64_MAX => Interval::EMPTY,
            BinOp::Gt => Interval {
                lo: bound + 1,
                hi: I64_MAX,
            },
            BinOp::Le => Interval {
                lo: I64_MIN,
                hi: bound,
            },
            BinOp::Lt if bound == I64_MIN => Interval::EMPTY,
            BinOp::Lt => Interval {
                lo: I64_MIN,
                hi: bound - 1,
            },
            _ => Interval::TOP,
        })
    }

    fn admits(&self, v: Interval) -> bool {
        if v.is_empty() {
            return false;
        }
        let NumericBound::Int(bound) = self.bound else {
            return false;
        };
        let bound = i128::from(bound);
        match self.op {
            BinOp::Ge => v.lo >= bound,
            BinOp::Gt => v.lo > bound,
            BinOp::Le => v.hi <= bound,
            BinOp::Lt => v.hi < bound,
            _ => false,
        }
    }

    fn admits_point(&self, v: i64) -> bool {
        self.admits(Interval::point(v))
    }

    pub fn describe(&self) -> String {
        let op = match self.op {
            BinOp::Ge => ">=",
            BinOp::Gt => ">",
            BinOp::Le => "<=",
            BinOp::Lt => "<",
            _ => "?",
        };
        format!("{op} {}", self.bound)
    }
}

/// A runtime-guarded input assumption: `<msg>.<field> <cmp> <literal>`.
#[derive(Debug, Clone)]
pub struct InputPrecondition {
    pub process: String,
    pub msg_name: String,
    pub field: String,
    pub pred: Pred,
    pub spec: String,
}

#[derive(Debug, Default)]
pub struct Level3Report {
    pub proven: Vec<String>,
    pub guarded_assumptions: Vec<String>,
    pub residual: Vec<String>,
}

pub(crate) fn int_literal_pub(l: &Literal) -> Option<i64> {
    int_literal(l)
}

fn numeric_bound(l: &Literal) -> Option<NumericBound> {
    match l {
        Literal::Int(i) => Some(NumericBound::Int(*i)),
        Literal::Float(f) if f.is_finite() => Some(NumericBound::Float(*f)),
        _ => None,
    }
}

fn int_literal(l: &Literal) -> Option<i64> {
    match l {
        Literal::Int(value) => Some(*value),
        _ => None,
    }
}

fn cmp_pred(op: &BinOp, bound: &Expr) -> Option<Pred> {
    let Expr::Literal { value, .. } = bound else {
        return None;
    };
    let bound = numeric_bound(value)?;
    match op {
        BinOp::Ge | BinOp::Gt | BinOp::Le | BinOp::Lt => Some(Pred {
            op: op.clone(),
            bound,
        }),
        _ => None,
    }
}

/// Extract input preconditions from all specs, resolved against handler
/// message names. Used by both the prover and codegen (guards).
pub fn input_preconditions(program: &Program) -> Vec<InputPrecondition> {
    let mut out = Vec::new();
    for spec in &program.specs {
        for item in &spec.items {
            let SpecItem::Require { expr, .. } = item else {
                continue;
            };
            let Expr::Binary { op, lhs, rhs, .. } = expr else {
                continue;
            };
            let Expr::FieldAccess { base, field, .. } = lhs.as_ref() else {
                continue;
            };
            let Some(pred) = cmp_pred(op, rhs) else {
                continue;
            };
            for process in &program.processes {
                for handler in &process.handlers {
                    if handler.msg_name == *base {
                        out.push(InputPrecondition {
                            process: process.name.clone(),
                            msg_name: base.clone(),
                            field: field.clone(),
                            pred: pred.clone(),
                            spec: spec.name.clone(),
                        });
                    }
                }
            }
        }
    }
    out
}

/// Prove every hold in every spec, or fail with an actionable message.
pub fn level3_prove(program: &Program) -> Result<Level3Report> {
    // Keep this public entry point sound even when callers bypass the
    // stratified `run_checks` pipeline.
    crate::analysis::check::check_numeric_types(program)?;
    let mut report = Level3Report::default();
    let preconds = input_preconditions(program);
    for pc in &preconds {
        report.guarded_assumptions.push(format!(
            "`{}.{}` {} — enforced by a generated guard at `{}`'s handler entry (violations are counted drops)",
            pc.msg_name, pc.field, pc.pred.describe(), pc.process
        ));
    }

    // Held-state predicates (the inductive hypotheses).
    let mut holds: BTreeMap<String, (Pred, String)> = BTreeMap::new();
    let mut relational: Vec<(String, BinOp, String, String)> = Vec::new();
    for spec in &program.specs {
        for item in &spec.items {
            let SpecItem::Hold { expr, span } = item else {
                continue;
            };
            let Expr::Binary { op, lhs, rhs, .. } = expr else {
                report.residual.push(format!(
                    "spec `{}` hold at bytes {}..{} — not in the provable fragment (need `state <cmp> literal`)",
                    spec.name, span.start, span.end
                ));
                continue;
            };
            let Expr::Ident { name, .. } = lhs.as_ref() else {
                report.residual.push(format!(
                    "spec `{}` hold — left side must be a state name",
                    spec.name
                ));
                continue;
            };
            if let (Expr::FieldAccess { base: lb, .. }, Expr::FieldAccess { base: rb, .. }) =
                (lhs.as_ref(), rhs.as_ref())
            {
                let is_proc = |n: &str| program.processes.iter().any(|p| p.name == n);
                if is_proc(lb) && is_proc(rb) {
                    // System invariant across processes: proven at Level 4.
                    report.residual.push(format!(
                        "spec `{}` hold spans processes — proven at Level 4 (--level 4)",
                        spec.name
                    ));
                    continue;
                }
            }
            if let Expr::Ident {
                name: rhs_state, ..
            } = rhs.as_ref()
            {
                // Relational hold within a process: proven separately.
                relational.push((
                    name.clone(),
                    op.clone(),
                    rhs_state.clone(),
                    spec.name.clone(),
                ));
                continue;
            }
            let Some(pred) = cmp_pred(op, rhs) else {
                report.residual.push(format!(
                    "spec `{}` hold `{name}` — bound must be a numeric literal or a state name",
                    spec.name
                ));
                continue;
            };
            holds.insert(name.clone(), (pred, spec.name.clone()));
        }
    }

    for (state, (pred, spec_name)) in &holds {
        prove_one(program, state, pred.clone(), spec_name, &holds, &preconds)?;
        report.proven.push(format!(
            "hold `{state} {}` (spec `{spec_name}`): init satisfies; every reachable update re-establishes it",
            pred.describe()
        ));
    }
    for (a, op, b, spec_name) in &relational {
        prove_relational(program, a, op, b, spec_name, &holds, &preconds)?;
        let op_s = match op {
            BinOp::Le => "<=",
            BinOp::Lt => "<",
            BinOp::Ge => ">=",
            BinOp::Gt => ">",
            _ => "?",
        };
        report.proven.push(format!(
            "hold `{a} {op_s} {b}` (spec `{spec_name}`): init ordering + per-handler delta argument \
             (sound at handler boundaries because actors are shared-nothing: no interleaving \
             is observable mid-handler)"
        ));
    }
    Ok(report)
}

fn prove_one(
    program: &Program,
    state: &str,
    pred: Pred,
    spec_name: &str,
    holds: &BTreeMap<String, (Pred, String)>,
    preconds: &[InputPrecondition],
) -> Result<()> {
    let owner: &Process = program
        .processes
        .iter()
        .find(|p| p.states.iter().any(|s| s.name == state))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Level-3 violation in spec '{spec_name}': hold refers to unknown state '{state}'"
            )
        })?;

    // BASE
    let decl = owner
        .states
        .iter()
        .find(|candidate| candidate.name == state)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Level-3 internal consistency error: state '{state}' lost its declaration"
            )
        })?;
    if !matches!(decl.ty, Type::Int) {
        bail!(
            "Level-3 violation in spec '{spec_name}': state '{state}' has type Float. \
             Float is executable but outside the proof fragment until Sigil has an \
             explicit IEEE-754 abstract domain. Represent exact quantities as Int \
             (for example, monetary minor units) or keep this hold residual below Level 3."
        );
    }
    if !matches!(pred.bound, NumericBound::Int(_)) {
        bail!(
            "Level-3 violation in spec '{spec_name}': hold on Int state '{state}' \
             requires an Int literal bound; Sigil does not coerce proof operands"
        );
    }
    match &decl.init {
        Expr::Literal { value, .. } => {
            let Some(v) = int_literal(value) else {
                bail!(
                    "Level-3 violation in spec '{spec_name}': state '{state}' init must be an Int literal"
                );
            };
            if !pred.admits_point(v) {
                bail!(
                    "Level-3 violation in spec '{spec_name}': BASE CASE fails — \
                     state '{state}' initialises to {v}, which does not satisfy `{state} {}`",
                    pred.describe()
                );
            }
        }
        _ => bail!(
            "Level-3 violation in spec '{spec_name}': state '{state}' init is not a literal; \
             the base case cannot be established"
        ),
    }

    // INDUCTIVE: every assignment in the owner process, evaluated in
    // statement order with a let-binding environment so simple bindings
    // (`let x = payment.units`) keep their guarded intervals.
    for handler in &owner.handlers {
        let mut lets: BTreeMap<String, Interval> = BTreeMap::new();
        for stmt in &handler.body {
            if let Stmt::Let { name, expr, .. } = stmt {
                let mut scratch = Vec::new();
                let v = eval_interval(
                    expr,
                    owner,
                    &handler.msg_name,
                    holds,
                    preconds,
                    &lets,
                    &mut scratch,
                );
                lets.insert(name.clone(), v);
                continue;
            }
            let Stmt::Assign { name, expr, .. } = stmt else {
                continue;
            };
            if name != state {
                continue;
            }
            let mut why = Vec::new();
            let v = eval_interval(
                expr,
                owner,
                &handler.msg_name,
                holds,
                preconds,
                &lets,
                &mut why,
            );
            if !pred.admits(v) {
                let hints = if why.is_empty() {
                    String::new()
                } else {
                    format!("\n  unbounded because: {}", why.join("; "))
                };
                bail!(
                    "Level-3 violation in spec '{spec_name}': INDUCTIVE STEP fails — \
                     in process '{}', update `{state} := {}` yields [{}, {}] which can \
                     escape `{state} {}`{hints}\n  fix: constrain the inputs with \
                     `require <msg>.<field> {}` in the spec, or restructure the update",
                    owner.name,
                    describe_expr(expr),
                    v.lo,
                    v.hi,
                    pred.describe(),
                    pred.describe(),
                );
            }
        }
    }
    Ok(())
}

/// Per-handler net delta of a state, in the self-additive fragment:
///   unassigned            → delta = [0, 0]
///   x := x + e (once)     → delta = interval(e)
///   x := x - e (once)     → delta = -interval(e)
/// Anything else → None (outside the fragment).
pub(crate) fn handler_delta(
    state: &str,
    handler: &crate::frontend::ast::OnHandler,
    owner: &Process,
    holds: &BTreeMap<String, (Pred, String)>,
    preconds: &[InputPrecondition],
    lets: &BTreeMap<String, Interval>,
    why: &mut Vec<String>,
) -> Option<Interval> {
    handler_delta_under(state, handler, owner, holds, preconds, lets, None, why)
}

/// Per-handler delta of a state, optionally assuming a guard condition
/// holds. Assuming the guard is what lets a conditional `send ... when G`
/// be bounded by a counter that is also incremented only when `G`.
/// Structural equality on expressions, used to correlate a `when` guard with
/// an `if` that tests the same condition.
fn same_expr(a: &Expr, b: &Expr) -> bool {
    match (a, b) {
        (Expr::Ident { name: x, .. }, Expr::Ident { name: y, .. }) => x == y,
        (
            Expr::FieldAccess {
                base: b1,
                field: f1,
                ..
            },
            Expr::FieldAccess {
                base: b2,
                field: f2,
                ..
            },
        ) => b1 == b2 && f1 == f2,
        (Expr::Literal { value: v1, .. }, Expr::Literal { value: v2, .. }) => {
            format!("{v1:?}") == format!("{v2:?}")
        }
        (
            Expr::Binary {
                op: o1,
                lhs: l1,
                rhs: r1,
                ..
            },
            Expr::Binary {
                op: o2,
                lhs: l2,
                rhs: r2,
                ..
            },
        ) => format!("{o1:?}") == format!("{o2:?}") && same_expr(l1, l2) && same_expr(r1, r2),
        _ => false,
    }
}

/// Free names read by an expression (identifiers and field-access bases).
fn free_names(e: &Expr, out: &mut std::collections::BTreeSet<String>) {
    match e {
        Expr::Ident { name, .. } => {
            out.insert(name.clone());
        }
        Expr::FieldAccess { base, .. } => {
            out.insert(base.clone());
        }
        Expr::Binary { lhs, rhs, .. } => {
            free_names(lhs, out);
            free_names(rhs, out);
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            free_names(cond, out);
            free_names(then_branch, out);
            free_names(else_branch, out);
        }
        Expr::SchemaLit { fields, .. } => {
            for (_, fe) in fields {
                free_names(fe, out);
            }
        }
        Expr::Call { args, .. } => {
            for a in args {
                free_names(a, out);
            }
        }
        Expr::Pipeline { base, steps, .. } => {
            free_names(base, out);
            for st in steps {
                free_names(&st.expr, out);
            }
        }
        Expr::Literal { .. } => {}
    }
}

/// Is a `when` guard stable across the whole handler body?
///
/// Correlating a guard with an earlier `if` is only valid when the guard has
/// the SAME value at both points. A guard that reads state the handler also
/// assigns does not: the `if` sees the old value and the guard sees the new
/// one. Treating them as the same condition proved a false invariant — see
/// examples/proofs/guard_mutated_state.sigil, which is exactly that shape.
fn guard_is_stable(guard: &Expr, handler: &crate::frontend::ast::OnHandler) -> bool {
    let mut names = std::collections::BTreeSet::new();
    free_names(guard, &mut names);
    let assigned: std::collections::BTreeSet<&str> = handler
        .body
        .iter()
        .filter_map(|st| match st {
            Stmt::Assign { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    !names.iter().any(|n| assigned.contains(n.as_str()))
}

/// Under an assumed condition, an `if` testing that same condition can only
/// take its then-branch. Interval arithmetic alone cannot see this (a closed
/// domain cannot represent a strict bound), but the correlation is exactly
/// what `send ... when G` relies on, so it is resolved syntactically.
fn simplify_under_guard(expr: &Expr, guard: &Expr) -> Expr {
    match expr {
        Expr::If {
            cond,
            then_branch,
            else_branch,
            span,
        } => {
            if same_expr(cond, guard) {
                simplify_under_guard(then_branch, guard)
            } else {
                Expr::If {
                    cond: cond.clone(),
                    then_branch: Box::new(simplify_under_guard(then_branch, guard)),
                    else_branch: Box::new(simplify_under_guard(else_branch, guard)),
                    span: *span,
                }
            }
        }
        Expr::Binary { op, lhs, rhs, span } => Expr::Binary {
            op: op.clone(),
            lhs: Box::new(simplify_under_guard(lhs, guard)),
            rhs: Box::new(simplify_under_guard(rhs, guard)),
            span: *span,
        },
        other => other.clone(),
    }
}

#[allow(clippy::too_many_arguments)] // the proof environment is intentionally explicit
pub(crate) fn handler_delta_under(
    state: &str,
    handler: &crate::frontend::ast::OnHandler,
    owner: &Process,
    holds: &BTreeMap<String, (Pred, String)>,
    preconds: &[InputPrecondition],
    lets: &BTreeMap<String, Interval>,
    guard: Option<&Expr>,
    why: &mut Vec<String>,
) -> Option<Interval> {
    // A guard may only be assumed if it cannot change between the point the
    // conditional counter is evaluated and the point the send is reached.
    let guard = guard.filter(|g| guard_is_stable(g, handler));
    let narrowed;
    let lets = match guard {
        Some(g) => {
            let (t, _e) = narrow(g, lets, owner, &handler.msg_name, holds, preconds);
            narrowed = t;
            &narrowed
        }
        None => lets,
    };
    let mut delta: Option<Interval> = None;
    for stmt in &handler.body {
        let Stmt::Assign { name, expr, .. } = stmt else {
            continue;
        };
        if name != state {
            continue;
        }
        if delta.is_some() {
            why.push(format!(
                "state `{state}` assigned more than once in a handler"
            ));
            return None;
        }
        let simplified;
        let expr = match guard {
            Some(g) => {
                simplified = simplify_under_guard(expr, g);
                &simplified
            }
            None => expr,
        };
        match expr {
            Expr::Binary { op, lhs, rhs, .. } if matches!(lhs.as_ref(), Expr::Ident { name: n, .. } if n == state) =>
            {
                let e = eval_interval(rhs, owner, &handler.msg_name, holds, preconds, lets, why);
                delta = match op {
                    BinOp::Add => Some(e),
                    BinOp::Sub => Some(e.negate_delta()),
                    _ => {
                        why.push(format!("`{state}` update is not additive"));
                        return None;
                    }
                };
            }
            _ => {
                why.push(format!(
                    "`{state}` update is not of the form `{state} := {state} ± e`"
                ));
                return None;
            }
        }
    }
    Some(delta.unwrap_or(Interval::point(0)))
}

/// Prove `a <op> b` for two states of the same process:
///   BASE:      init_a <op> init_b (literals)
///   INDUCTIVE: in every handler, interval(delta_b − delta_a) keeps the gap
///              (≥ 0 for `a <= b`, etc.). Induction at handler boundaries is
///              sound because process state is shared-nothing: handlers run
///              to completion with no observable interleaving.
fn prove_relational(
    program: &Program,
    a: &str,
    op: &BinOp,
    b: &str,
    spec_name: &str,
    holds: &BTreeMap<String, (Pred, String)>,
    preconds: &[InputPrecondition],
) -> Result<()> {
    let owner = program
        .processes
        .iter()
        .find(|p| p.states.iter().any(|s| s.name == a))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Level-3 violation in spec '{spec_name}': hold refers to unknown state '{a}'"
            )
        })?;
    if !owner.states.iter().any(|s| s.name == b) {
        bail!(
            "Level-3 violation in spec '{spec_name}': relational hold `{a} .. {b}` spans \
             processes — same-process only at Level 3 (cross-process relations are Level 4)"
        );
    }
    let a_decl = owner
        .states
        .iter()
        .find(|state| state.name == a)
        .ok_or_else(|| {
            anyhow::anyhow!("Level-3 internal consistency error: state '{a}' disappeared")
        })?;
    let b_decl = owner
        .states
        .iter()
        .find(|state| state.name == b)
        .ok_or_else(|| {
            anyhow::anyhow!("Level-3 internal consistency error: state '{b}' disappeared")
        })?;
    if !matches!(a_decl.ty, Type::Int) || !matches!(b_decl.ty, Type::Int) {
        bail!(
            "Level-3 violation in spec '{spec_name}': relational Float holds are outside \
             the proof fragment until Sigil has explicit IEEE-754 semantics"
        );
    }
    let lit_init = |st: &str| -> Result<i64> {
        let d = owner
            .states
            .iter()
            .find(|state| state.name == st)
            .ok_or_else(|| {
                anyhow::anyhow!("Level-3 internal consistency error: state '{st}' disappeared")
            })?;
        match &d.init {
            Expr::Literal { value, .. } => int_literal(value).ok_or_else(|| {
                anyhow::anyhow!(
                    "Level-3 violation in spec '{spec_name}': `{st}` init is not an Int literal"
                )
            }),
            _ => bail!("Level-3 violation in spec '{spec_name}': `{st}` init not a literal"),
        }
    };
    let (ia, ib) = (lit_init(a)?, lit_init(b)?);
    let base_ok = match op {
        BinOp::Le => ia <= ib,
        BinOp::Lt => ia < ib,
        BinOp::Ge => ia >= ib,
        BinOp::Gt => ia > ib,
        _ => bail!("Level-3 violation in spec '{spec_name}': unsupported relational op"),
    };
    if !base_ok {
        bail!(
            "Level-3 violation in spec '{spec_name}': BASE CASE fails — inits {a}={ia}, {b}={ib}"
        );
    }

    for handler in &owner.handlers {
        // Track lets for this handler (same as scalar proofs).
        let mut lets: BTreeMap<String, Interval> = BTreeMap::new();
        for stmt in &handler.body {
            if let Stmt::Let { name, expr, .. } = stmt {
                let mut scratch = Vec::new();
                let v = eval_interval(
                    expr,
                    owner,
                    &handler.msg_name,
                    holds,
                    preconds,
                    &lets,
                    &mut scratch,
                );
                lets.insert(name.clone(), v);
            }
        }
        let mut why = Vec::new();
        let da = handler_delta(a, handler, owner, holds, preconds, &lets, &mut why);
        let db = handler_delta(b, handler, owner, holds, preconds, &lets, &mut why);
        let (Some(da), Some(db)) = (da, db) else {
            bail!(
                "Level-3 violation in spec '{spec_name}': relational hold `{a}` vs `{b}` — \
                 handler in '{}' leaves the additive fragment: {}",
                owner.name,
                why.join("; ")
            );
        };
        // Gap change: (b + db) - (a + da) - (b - a) = db - da
        let gap = db.math_sub(da);
        let ok = match op {
            BinOp::Le | BinOp::Lt => gap.lo >= 0,
            BinOp::Ge | BinOp::Gt => gap.hi <= 0,
            _ => false,
        };
        if !ok {
            let hints = if why.is_empty() {
                String::new()
            } else {
                format!("\n  unbounded because: {}", why.join("; "))
            };
            bail!(
                "Level-3 violation in spec '{spec_name}': INDUCTIVE STEP fails — in process \
                 '{}', per-message deltas allow d({b})−d({a}) in [{}, {}], which can shrink \
                 the `{a}` vs `{b}` gap{hints}\n  fix: guard the inputs so every message \
                 changes `{b}` at least as much as `{a}`",
                owner.name,
                gap.lo,
                gap.hi
            );
        }
    }
    Ok(())
}

pub(crate) fn eval_interval(
    expr: &Expr,
    owner: &Process,
    msg_name: &str,
    holds: &BTreeMap<String, (Pred, String)>,
    preconds: &[InputPrecondition],
    lets: &BTreeMap<String, Interval>,
    why: &mut Vec<String>,
) -> Interval {
    match expr {
        Expr::Literal { value, .. } => int_literal(value)
            .map(Interval::point)
            .unwrap_or(Interval::TOP),
        Expr::Ident { name, .. } => {
            if let Some(v) = lets.get(name) {
                *v // tracked let binding
            } else if let Some((p, _)) = holds.get(name) {
                p.region().unwrap_or_else(|| {
                    why.push(format!(
                        "state `{name}` has a Float hold outside the proof fragment"
                    ));
                    Interval::TOP
                }) // inductive hypothesis
            } else if owner.states.iter().any(|s| s.name == *name) {
                why.push(format!("state `{name}` has no hold of its own"));
                Interval::TOP
            } else {
                why.push(format!(
                    "`{name}` is an untracked binding (flows through transforms)"
                ));
                Interval::TOP
            }
        }
        Expr::FieldAccess { base, field, .. } => {
            // A narrowed branch may have refined this exact field.
            if let Some(v) = lets.get(&format!("{base}.{field}")) {
                return *v;
            }
            if base == msg_name {
                let bounds: Vec<&InputPrecondition> = preconds
                    .iter()
                    .filter(|pc| {
                        pc.process == owner.name && pc.msg_name == *base && pc.field == *field
                    })
                    .collect();
                if bounds.is_empty() {
                    why.push(format!(
                        "input `{base}.{field}` is unguarded (no `require {base}.{field} ...`)"
                    ));
                    Interval::TOP
                } else {
                    bounds
                        .iter()
                        .filter_map(|pc| pc.pred.region())
                        .fold(Interval::TOP, Interval::meet)
                }
            } else {
                why.push(format!(
                    "`{base}.{field}` derives from transform output (external stages are unbounded)"
                ));
                Interval::TOP
            }
        }
        Expr::Binary { op, lhs, rhs, .. } => {
            let l = eval_interval(lhs, owner, msg_name, holds, preconds, lets, why);
            let r = eval_interval(rhs, owner, msg_name, holds, preconds, lets, why);
            match op {
                BinOp::Add => l.add(r),
                BinOp::Sub => l.sub(r),
                BinOp::Mul => l.mul(r),
                _ => Interval::TOP,
            }
        }
        Expr::Pipeline { .. } | Expr::Call { .. } => {
            why.push("value flows through transforms (not in the linear fragment)".into());
            Interval::TOP
        }
        Expr::SchemaLit { .. } => {
            why.push("schema literals are not numeric values".into());
            Interval::TOP
        }
        // `if` is where real code enforces its own invariants (clamping,
        // flooring, capping). Evaluating each branch under the NARROWED
        // condition — rather than taking a blind hull — is what makes those
        // patterns provable.
        Expr::If {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            let (then_env, else_env) = narrow(cond, lets, owner, msg_name, holds, preconds);
            let t = eval_interval(
                then_branch,
                owner,
                msg_name,
                holds,
                preconds,
                &then_env,
                why,
            );
            let e = eval_interval(
                else_branch,
                owner,
                msg_name,
                holds,
                preconds,
                &else_env,
                why,
            );
            t.hull(e)
        }
    }
}

/// Refine the environment for each branch of `if <var> <cmp> <literal>`.
///
/// In the then-branch the variable is known to satisfy the comparison; in
/// the else-branch it satisfies the negation. Any other condition shape
/// leaves both environments unchanged (sound, just less precise).
pub(crate) fn narrow(
    cond: &Expr,
    lets: &BTreeMap<String, Interval>,
    owner: &Process,
    msg_name: &str,
    holds: &BTreeMap<String, (Pred, String)>,
    preconds: &[InputPrecondition],
) -> (BTreeMap<String, Interval>, BTreeMap<String, Interval>) {
    let mut then_env = lets.clone();
    let mut else_env = lets.clone();

    let Expr::Binary { op, lhs, rhs, .. } = cond else {
        return (then_env, else_env);
    };
    let Some(bound) = (match rhs.as_ref() {
        Expr::Literal { value, .. } => int_literal(value),
        _ => None,
    }) else {
        return (then_env, else_env);
    };
    // Only simple named values can be narrowed.
    let key = match lhs.as_ref() {
        Expr::Ident { name, .. } => name.clone(),
        Expr::FieldAccess { base, field, .. } => format!("{base}.{field}"),
        _ => return (then_env, else_env),
    };

    let mut scratch = Vec::new();
    let current = eval_interval(lhs, owner, msg_name, holds, preconds, lets, &mut scratch);

    // Regions admitted by the comparison and by its negation.
    let bound = i128::from(bound);
    let (t_region, e_region) = match op {
        BinOp::Gt => (
            if bound == I64_MAX {
                Interval::EMPTY
            } else {
                Interval {
                    lo: bound + 1,
                    hi: I64_MAX,
                }
            },
            Interval {
                lo: I64_MIN,
                hi: bound,
            },
        ),
        BinOp::Ge => (
            Interval {
                lo: bound,
                hi: I64_MAX,
            },
            if bound == I64_MIN {
                Interval::EMPTY
            } else {
                Interval {
                    lo: I64_MIN,
                    hi: bound - 1,
                }
            },
        ),
        BinOp::Lt => (
            if bound == I64_MIN {
                Interval::EMPTY
            } else {
                Interval {
                    lo: I64_MIN,
                    hi: bound - 1,
                }
            },
            Interval {
                lo: bound,
                hi: I64_MAX,
            },
        ),
        BinOp::Le => (
            Interval {
                lo: I64_MIN,
                hi: bound,
            },
            if bound == I64_MAX {
                Interval::EMPTY
            } else {
                Interval {
                    lo: bound + 1,
                    hi: I64_MAX,
                }
            },
        ),
        _ => return (then_env, else_env),
    };
    then_env.insert(key.clone(), current.meet(t_region));
    else_env.insert(key, current.meet(e_region));
    (then_env, else_env)
}

pub(crate) fn describe_expr_pub(e: &Expr) -> String {
    describe_expr(e)
}

fn describe_expr(e: &Expr) -> String {
    match e {
        Expr::If {
            cond,
            then_branch,
            else_branch,
            ..
        } => format!(
            "if {} {{ {} }} else {{ {} }}",
            describe_expr(cond),
            describe_expr(then_branch),
            describe_expr(else_branch)
        ),
        Expr::SchemaLit { name, .. } => format!("{name} {{ .. }}"),
        Expr::Ident { name, .. } => name.clone(),
        Expr::FieldAccess { base, field, .. } => format!("{base}.{field}"),
        Expr::Literal { value, .. } => format!("{value:?}"),
        Expr::Binary { op, lhs, rhs, .. } => {
            let op_s = match op {
                BinOp::Add => "+",
                BinOp::Sub => "-",
                BinOp::Mul => "*",
                BinOp::Div => "/",
                BinOp::Ge => ">=",
                BinOp::Gt => ">",
                BinOp::Le => "<=",
                BinOp::Lt => "<",
                BinOp::Eq => "==",
            };
            format!("{} {op_s} {}", describe_expr(lhs), describe_expr(rhs))
        }
        Expr::Pipeline { .. } => "<pipeline>".into(),
        Expr::Call { name, .. } => format!("{name}(..)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::ast::parse;

    const PROVABLE: &str = r#"
schema Payment { id: String, amount: Int, units: Int }
process Ledger {
  state posted: Int = 0
  state total: Int = 0
  on payment: Payment {
    posted := posted + payment.units
    total := total + payment.amount
  }
}
spec Safe {
  require payment.amount >= 0
  require payment.units >= 0
  hold posted >= 0
  hold total >= 0
}
"#;

    #[test]
    fn proves_guarded_monotone_accumulators() {
        let program = parse(PROVABLE).expect("parse");
        let report = level3_prove(&program).expect("must prove");
        assert_eq!(report.proven.len(), 2);
        assert_eq!(report.guarded_assumptions.len(), 2);
    }

    #[test]
    fn unguarded_input_fails_with_actionable_fix() {
        let src = PROVABLE.replace("  require payment.amount >= 0\n", "");
        let program = parse(&src).expect("parse");
        let err = level3_prove(&program).expect_err("unbounded input must fail");
        let msg = format!("{err}");
        assert!(msg.contains("INDUCTIVE STEP fails"), "got: {msg}");
        assert!(msg.contains("unguarded"), "got: {msg}");
        assert!(msg.contains("require"), "must suggest the fix: {msg}");
    }

    #[test]
    fn subtraction_escapes_and_fails() {
        let src = PROVABLE.replace(
            "total := total + payment.amount",
            "total := total - payment.amount",
        );
        let program = parse(&src).expect("parse");
        let err = level3_prove(&program).expect_err("subtraction can go negative");
        assert!(format!("{err}").contains("INDUCTIVE STEP fails"));
    }

    #[test]
    fn bad_init_fails_base_case() {
        let src = PROVABLE.replace("state total: Int = 0", "state total: Int = -1");
        let program = parse(&src).expect("parse");
        let err = level3_prove(&program).expect_err("init violates the hold");
        assert!(format!("{err}").contains("BASE CASE fails"));
    }

    #[test]
    fn let_bindings_keep_guarded_intervals() {
        let src = r#"
schema Payment { id: String, amount: Int }
process P {
  state total: Int = 0
  on payment: Payment {
    let amt = payment.amount
    let doubled = amt + amt
    total := total + doubled
  }
}
spec S {
  require payment.amount >= 0
  hold total >= 0
}
"#;
        let program = parse(src).expect("parse");
        let report = level3_prove(&program).expect("bindings must carry guards");
        assert_eq!(report.proven.len(), 1);
    }

    const RELATIONAL: &str = r#"
schema Tx { id: String, charge: Int, refund: Int }
process Book {
  state charged: Int = 0
  state refunded: Int = 0
  on tx: Tx {
    charged := charged + tx.charge
    refunded := refunded + tx.refund
  }
}
spec Rel {
  require tx.charge >= 0
  require tx.refund >= 0
  require tx.refund <= 0
  hold refunded <= charged
}
"#;

    #[test]
    fn relational_hold_proves_when_gap_cannot_shrink() {
        // refund guarded to exactly 0 → delta(charged) − delta(refunded) ≥ 0.
        let program = parse(RELATIONAL).expect("parse");
        let report = level3_prove(&program).expect("gap cannot shrink");
        assert!(report
            .proven
            .iter()
            .any(|p| p.contains("refunded <= charged")));
    }

    #[test]
    fn relational_hold_fails_when_gap_can_shrink() {
        // Remove the upper guard: refund can exceed charge → gap can shrink.
        let src = RELATIONAL.replace(
            "  require tx.refund <= 0
",
            "",
        );
        let program = parse(&src).expect("parse");
        let err = level3_prove(&program).expect_err("gap can shrink");
        let msg = format!("{err}");
        assert!(
            msg.contains("INDUCTIVE STEP fails") && msg.contains("gap"),
            "got: {msg}"
        );
    }

    #[test]
    fn relational_hold_fails_bad_init() {
        let src = RELATIONAL.replace("state refunded: Int = 0", "state refunded: Int = 1");
        let program = parse(&src).expect("parse");
        let err = level3_prove(&program).expect_err("init ordering violated");
        assert!(format!("{err}").contains("BASE CASE fails"));
    }

    #[test]
    fn interval_arithmetic_is_sound_on_corners() {
        let a = Interval { lo: 0, hi: I64_MAX };
        let b = Interval { lo: 0, hi: I64_MAX };
        let s = a.add(b);
        assert_eq!(s, Interval { lo: 0, hi: I64_MAX });
        let d = a.math_sub(b);
        assert_eq!(d.lo, -I64_MAX);
        let m = Interval { lo: -2, hi: 3 }.mul(Interval { lo: -1, hi: 4 });
        assert_eq!(m.lo, -8);
        assert_eq!(m.hi, 12);
    }

    #[test]
    fn integers_above_f64_precision_are_compared_exactly() {
        let src = r#"
process P {
  state low: Int = 9007199254740991
  state value: Int = 9007199254740993
}
spec Exact {
  hold low < 9007199254740992
  hold value > 9007199254740992
}
"#;
        let program = parse(src).expect("parse");
        level3_prove(&program).expect("i64 comparison must not round through f64");

        let bad = src.replace(
            "hold value > 9007199254740992",
            "hold value <= 9007199254740992",
        );
        let program = parse(&bad).expect("parse");
        let error = level3_prove(&program).expect_err("exactly larger value must be rejected");
        assert!(error.to_string().contains("BASE CASE fails"));
    }

    #[test]
    fn checked_i64_overflow_cannot_install_a_wrapped_post_state() {
        let near_max = Interval {
            lo: I64_MAX - 1,
            hi: I64_MAX,
        };
        assert_eq!(
            near_max.add(Interval::point(1)),
            Interval {
                lo: I64_MAX,
                hi: I64_MAX
            }
        );
        assert!(Interval::point(i64::MAX).add(Interval::point(1)).is_empty());

        let near_min = Interval {
            lo: I64_MIN,
            hi: I64_MIN + 1,
        };
        assert_eq!(
            near_min.sub(Interval::point(1)),
            Interval {
                lo: I64_MIN,
                hi: I64_MIN
            }
        );
        assert!(Interval::point(i64::MIN).sub(Interval::point(1)).is_empty());
    }

    #[test]
    fn float_holds_fail_closed_until_ieee_semantics_exist() {
        let src = r#"
process P {
  state value: Float = 0.0
}
spec Unsupported {
  hold value >= 0.0
}
"#;
        let program = parse(src).expect("parse");
        let error = level3_prove(&program).expect_err("Float theorem must not be emitted");
        assert!(error.to_string().contains("outside the proof fragment"));
    }
}
