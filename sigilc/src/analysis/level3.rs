//! Level 3 — inductive proof of `hold` invariants.
//!
//! For each `hold <state> <cmp> <literal>` the prover establishes:
//!   BASE:      the declared init value satisfies the predicate;
//!   INDUCTIVE: assuming every held state satisfies its predicate, every
//!              reachable assignment to the state re-establishes it.
//!
//! The abstract domain is intervals over the reals (sound for Int and Float
//! updates built from literals, held states, guarded message fields, +, -,
//! and ×). Anything outside the fragment — values flowing through external
//! transforms, unguarded inputs — is NOT assumed; it is unbounded, and if
//! the proof then fails, the error says exactly which assumption is missing.
//!
//! Input assumptions are written `require <msg>.<field> <cmp> <literal>` in
//! a spec. They are not taken on faith: codegen emits a guard at handler
//! entry that rejects (and counts) any message violating them, so every
//! proof assumption is enforced at runtime and the proof is unconditional.

use crate::frontend::ast::{BinOp, Expr, Literal, Process, Program, SpecItem, Stmt};
use anyhow::{bail, Result};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Interval {
    pub lo: f64, // -inf allowed
    pub hi: f64, // +inf allowed
}

impl Interval {
    pub const TOP: Interval = Interval { lo: f64::NEG_INFINITY, hi: f64::INFINITY };

    fn point(v: f64) -> Self {
        Interval { lo: v, hi: v }
    }

    fn add(self, o: Self) -> Self {
        Interval { lo: self.lo + o.lo, hi: self.hi + o.hi }
    }

    fn sub(self, o: Self) -> Self {
        Interval { lo: self.lo - o.hi, hi: self.hi - o.lo }
    }

    fn mul(self, o: Self) -> Self {
        let mut c: Vec<f64> = Vec::new();
        for a in [self.lo, self.hi] {
            for b in [o.lo, o.hi] {
                let v = if (a == 0.0 && b.is_infinite()) || (b == 0.0 && a.is_infinite()) {
                    0.0 // conservative: 0 × ∞ treated as 0 for corner enumeration
                } else {
                    a * b
                };
                c.push(v);
            }
        }
        Interval {
            lo: c.iter().cloned().fold(f64::INFINITY, f64::min),
            hi: c.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        }
    }
}

/// A predicate `<cmp> bound` over one variable.
#[derive(Debug, Clone)]
pub struct Pred {
    pub op: BinOp,
    pub bound: f64,
}

impl Pred {
    /// The region this predicate admits, as an interval.
    fn region(&self) -> Interval {
        match self.op {
            BinOp::Ge => Interval { lo: self.bound, hi: f64::INFINITY },
            BinOp::Gt => Interval { lo: self.bound, hi: f64::INFINITY }, // sound over-approx of assumption
            BinOp::Le => Interval { lo: f64::NEG_INFINITY, hi: self.bound },
            BinOp::Lt => Interval { lo: f64::NEG_INFINITY, hi: self.bound },
            _ => Interval::TOP,
        }
    }

    fn admits(&self, v: Interval) -> bool {
        match self.op {
            BinOp::Ge => v.lo >= self.bound,
            BinOp::Gt => v.lo > self.bound,
            BinOp::Le => v.hi <= self.bound,
            BinOp::Lt => v.hi < self.bound,
            _ => false,
        }
    }

    fn admits_point(&self, v: f64) -> bool {
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

fn lit_value(l: &Literal) -> Option<f64> {
    match l {
        Literal::Int(i) => Some(*i as f64),
        Literal::Float(f) => Some(*f),
        _ => None,
    }
}

fn cmp_pred(op: &BinOp, bound: &Expr) -> Option<Pred> {
    let Expr::Literal { value, .. } = bound else { return None };
    let bound = lit_value(value)?;
    match op {
        BinOp::Ge | BinOp::Gt | BinOp::Le | BinOp::Lt => Some(Pred { op: op.clone(), bound }),
        _ => None,
    }
}

/// Extract input preconditions from all specs, resolved against handler
/// message names. Used by both the prover and codegen (guards).
pub fn input_preconditions(program: &Program) -> Vec<InputPrecondition> {
    let mut out = Vec::new();
    for spec in &program.specs {
        for item in &spec.items {
            let SpecItem::Require { expr, .. } = item else { continue };
            let Expr::Binary { op, lhs, rhs, .. } = expr else { continue };
            let Expr::FieldAccess { base, field, .. } = lhs.as_ref() else { continue };
            let Some(pred) = cmp_pred(op, rhs) else { continue };
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
    for spec in &program.specs {
        for item in &spec.items {
            let SpecItem::Hold { expr, span } = item else { continue };
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
            let Some(pred) = cmp_pred(op, rhs) else {
                report.residual.push(format!(
                    "spec `{}` hold `{name}` — bound must be a numeric literal",
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
    let decl = owner.states.iter().find(|s| s.name == state).unwrap();
    match &decl.init {
        Expr::Literal { value, .. } => {
            let Some(v) = lit_value(value) else {
                bail!(
                    "Level-3 violation in spec '{spec_name}': state '{state}' init is not numeric"
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

    // INDUCTIVE: every assignment in the owner process
    for handler in &owner.handlers {
        for stmt in &handler.body {
            let Stmt::Assign { name, expr, .. } = stmt else { continue };
            if name != state {
                continue;
            }
            let mut why = Vec::new();
            let v = eval_interval(expr, owner, &handler.msg_name, holds, preconds, &mut why);
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

fn eval_interval(
    expr: &Expr,
    owner: &Process,
    msg_name: &str,
    holds: &BTreeMap<String, (Pred, String)>,
    preconds: &[InputPrecondition],
    why: &mut Vec<String>,
) -> Interval {
    match expr {
        Expr::Literal { value, .. } => lit_value(value)
            .map(Interval::point)
            .unwrap_or(Interval::TOP),
        Expr::Ident { name, .. } => {
            if let Some((p, _)) = holds.get(name) {
                p.region() // inductive hypothesis
            } else if owner.states.iter().any(|s| s.name == *name) {
                why.push(format!("state `{name}` has no hold of its own"));
                Interval::TOP
            } else {
                why.push(format!("`{name}` is a local binding (flows through transforms)"));
                Interval::TOP
            }
        }
        Expr::FieldAccess { base, field, .. } => {
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
                        .fold(Interval::TOP, |acc, pc| {
                            let r = pc.pred.region();
                            Interval { lo: acc.lo.max(r.lo), hi: acc.hi.min(r.hi) }
                        })
                }
            } else {
                why.push(format!(
                    "`{base}.{field}` derives from transform output (external stages are unbounded)"
                ));
                Interval::TOP
            }
        }
        Expr::Binary { op, lhs, rhs, .. } => {
            let l = eval_interval(lhs, owner, msg_name, holds, preconds, why);
            let r = eval_interval(rhs, owner, msg_name, holds, preconds, why);
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
    }
}

fn describe_expr(e: &Expr) -> String {
    match e {
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
schema Payment { id: String, amount: Float, units: Int }
process Ledger {
  state posted: Int = 0
  state total: Float = 0.0
  on payment: Payment {
    posted := posted + payment.units
    total := total + payment.amount
  }
}
spec Safe {
  require payment.amount >= 0.0
  require payment.units >= 0
  hold posted >= 0
  hold total >= 0.0
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
        let src = PROVABLE.replace("  require payment.amount >= 0.0\n", "");
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
        let src = PROVABLE.replace("state total: Float = 0.0", "state total: Float = -1.0");
        let program = parse(&src).expect("parse");
        let err = level3_prove(&program).expect_err("init violates the hold");
        assert!(format!("{err}").contains("BASE CASE fails"));
    }

    #[test]
    fn interval_arithmetic_is_sound_on_corners() {
        let a = Interval { lo: 0.0, hi: f64::INFINITY };
        let b = Interval { lo: 0.0, hi: f64::INFINITY };
        let s = a.add(b);
        assert_eq!(s.lo, 0.0);
        let d = a.sub(b);
        assert_eq!(d.lo, f64::NEG_INFINITY); // [0,∞) - [0,∞) can be anything ≤ ∞
        let m = Interval { lo: -2.0, hi: 3.0 }.mul(Interval { lo: -1.0, hi: 4.0 });
        assert_eq!(m.lo, -8.0);
        assert_eq!(m.hi, 12.0);
    }
}
