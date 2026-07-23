//! Level 4 — system invariants across the process topology.
//!
//! Proves holds of the form `hold B.sb <= A.sa` (two states in two
//! processes) from a purely structural argument:
//!
//!   BASE        init(sb) <= init(sa)
//!   PER-MESSAGE A's handler adds da to sa, with da.lo >= 0; B's handler
//!               adds db to sb (self-additive fragment, Level-3 deltas)
//!   ORDERING    A updates sa BEFORE its first send — so even if a handler
//!               is cut short (shutdown, drop), sa already counts the
//!               message that may reach B
//!   FLOW        every path by which a message can reach B passes through
//!               A, and the total send multiplicity along A→B paths is a
//!               static constant `mult` (broadcast edges have no static
//!               multiplicity and are rejected)
//!   GAP         mult × max(db.hi, 0) <= da.lo
//!
//! Then: count_B <= mult × count_A, so
//!   sb  =  init(sb) + Σ db  <=  init(sb) + count_B·db.hi
//!       <=  init(sa) + count_A·da.lo  <=  sa.
//!
//! Drops anywhere in the pipeline only DECREASE count_B, so every failure
//! mode the language admits (timeouts, @error drops, guard rejections,
//! staged shutdown) preserves the invariant. This is the payoff of the
//! actor model plus mandatory failure paths: the system-level proof needs
//! no fairness or liveness assumptions at all.

use crate::analysis::level3::{eval_interval, handler_delta, input_preconditions, Interval};
use crate::analysis::topology::derive_topology;
use crate::frontend::ast::{BinOp, Expr, Program, Route, SpecItem, Stmt};
use anyhow::{bail, Result};
use std::collections::BTreeMap;

#[derive(Debug, Default)]
pub struct Level4Report {
    pub proven: Vec<String>,
}

struct SystemHold {
    lo_proc: String,
    lo_state: String,
    hi_proc: String,
    hi_state: String,
    strict: bool,
    spec: String,
}

fn collect_system_holds(program: &Program) -> Result<Vec<SystemHold>> {
    let is_proc = |n: &str| program.processes.iter().any(|p| p.name == n);
    let mut out = Vec::new();
    for spec in &program.specs {
        for item in &spec.items {
            let SpecItem::Hold { expr, .. } = item else { continue };
            let Expr::Binary { op, lhs, rhs, .. } = expr else { continue };
            let (Expr::FieldAccess { base: lb, field: lf, .. }, Expr::FieldAccess { base: rb, field: rf, .. }) =
                (lhs.as_ref(), rhs.as_ref())
            else {
                continue;
            };
            if !is_proc(lb) || !is_proc(rb) {
                continue;
            }
            // Normalise to `lo <= hi`.
            let (lo, hi, strict) = match op {
                BinOp::Le => ((lb, lf), (rb, rf), false),
                BinOp::Lt => ((lb, lf), (rb, rf), true),
                BinOp::Ge => ((rb, rf), (lb, lf), false),
                BinOp::Gt => ((rb, rf), (lb, lf), true),
                _ => bail!(
                    "Level-4 violation in spec '{}': system hold must be an ordering (<=, <, >=, >)",
                    spec.name
                ),
            };
            out.push(SystemHold {
                lo_proc: lo.0.clone(),
                lo_state: lo.1.clone(),
                hi_proc: hi.0.clone(),
                hi_state: hi.1.clone(),
                strict,
                spec: spec.name.clone(),
            });
        }
    }
    Ok(out)
}

pub fn level4_prove(program: &Program) -> Result<Level4Report> {
    let mut report = Level4Report::default();
    let holds = collect_system_holds(program)?;
    if holds.is_empty() {
        return Ok(report);
    }
    let topo = derive_topology(program)?;
    let preconds = input_preconditions(program);

    // Static edge multiplicities: send statements per handler execution.
    // Broadcast has no static multiplicity (shard count is a runtime value).
    let mut edge_mult: BTreeMap<(String, String), Option<u64>> = BTreeMap::new();
    for process in &program.processes {
        for handler in &process.handlers {
            for stmt in &handler.body {
                let Stmt::Send { target, route, .. } = stmt else { continue };
                let e = edge_mult
                    .entry((process.name.clone(), target.clone()))
                    .or_insert(Some(0));
                match route {
                    Route::Broadcast => *e = None,
                    _ => {
                        if let Some(m) = e {
                            *m += 1;
                        }
                    }
                }
            }
        }
    }

    for h in &holds {
        prove_system_hold(program, &topo, &edge_mult, &preconds, h)?;
        let cmp = if h.strict { "<" } else { "<=" };
        report.proven.push(format!(
            "hold `{}.{} {cmp} {}.{}` (spec `{}`): base ordering + update-before-send + \
             all-paths-through-`{}` + static multiplicity bound; robust to every drop the \
             language admits",
            h.lo_proc, h.lo_state, h.hi_proc, h.hi_state, h.spec, h.hi_proc
        ));
    }
    Ok(report)
}

fn prove_system_hold(
    program: &Program,
    topo: &crate::analysis::topology::Topology,
    edge_mult: &BTreeMap<(String, String), Option<u64>>,
    preconds: &[crate::analysis::level3::InputPrecondition],
    h: &SystemHold,
) -> Result<()> {
    let spec = &h.spec;
    let a = program.processes.iter().find(|p| p.name == h.hi_proc).unwrap();
    let b = program.processes.iter().find(|p| p.name == h.lo_proc).unwrap();
    let sa = a
        .states
        .iter()
        .find(|s| s.name == h.hi_state)
        .ok_or_else(|| anyhow::anyhow!(
            "Level-4 violation in spec '{spec}': `{}` has no state `{}`",
            h.hi_proc, h.hi_state
        ))?;
    let sb = b
        .states
        .iter()
        .find(|s| s.name == h.lo_state)
        .ok_or_else(|| anyhow::anyhow!(
            "Level-4 violation in spec '{spec}': `{}` has no state `{}`",
            h.lo_proc, h.lo_state
        ))?;

    // BASE
    let lit = |e: &Expr| -> Option<f64> {
        match e {
            Expr::Literal { value, .. } => crate::analysis::level3::lit_value_pub(value),
            _ => None,
        }
    };
    let (ia, ib) = match (lit(&sa.init), lit(&sb.init)) {
        (Some(x), Some(y)) => (x, y),
        _ => bail!("Level-4 violation in spec '{spec}': inits must be numeric literals"),
    };
    if (h.strict && !(ib < ia)) || (!h.strict && !(ib <= ia)) {
        bail!(
            "Level-4 violation in spec '{spec}': BASE CASE fails — init {}.{} = {ib} vs \
             {}.{} = {ia}",
            h.lo_proc, h.lo_state, h.hi_proc, h.hi_state
        );
    }

    // PER-MESSAGE deltas (single-handler processes; topology already enforces
    // single-handler send targets).
    let empty = BTreeMap::new();
    let per_handler_delta = |p: &crate::frontend::ast::Process, st: &str| -> Result<Interval> {
        let handler = p.handlers.first().ok_or_else(|| anyhow::anyhow!(
            "Level-4 violation in spec '{spec}': process `{}` has no handler",
            p.name
        ))?;
        let mut lets: BTreeMap<String, Interval> = BTreeMap::new();
        for stmt in &handler.body {
            if let Stmt::Let { name, expr, .. } = stmt {
                let mut scratch = Vec::new();
                let v = eval_interval(expr, p, &handler.msg_name, &empty, preconds, &lets, &mut scratch);
                lets.insert(name.clone(), v);
            }
        }
        let mut why = Vec::new();
        handler_delta(st, handler, p, &empty, preconds, &lets, &mut why).ok_or_else(|| {
            anyhow::anyhow!(
                "Level-4 violation in spec '{spec}': `{}` update in `{}` leaves the additive \
                 fragment: {}",
                st, p.name, why.join("; ")
            )
        })
    };
    let da = per_handler_delta(a, &h.hi_state)?;
    let db = per_handler_delta(b, &h.lo_state)?;
    if !(da.lo >= 0.0) || da.lo.is_infinite() && da.lo < 0.0 {
        bail!(
            "Level-4 violation in spec '{spec}': `{}.{}` can decrease per message \
             (delta lo = {}) — the counting argument needs a non-negative lower bound; \
             guard the inputs",
            h.hi_proc, h.hi_state, da.lo
        );
    }

    // ORDERING: sa assigned before A's first send.
    let handler = a.handlers.first().unwrap();
    let mut seen_send = false;
    let mut assigned_before_send = false;
    for stmt in &handler.body {
        match stmt {
            Stmt::Send { .. } => seen_send = true,
            Stmt::Assign { name, .. } if name == &h.hi_state => {
                if !seen_send {
                    assigned_before_send = true;
                } else {
                    bail!(
                        "Level-4 violation in spec '{spec}': ORDERING fails — `{}` updates \
                         `{}` AFTER sending; a message could reach `{}` without being \
                         counted. Move the update above the send.",
                        h.hi_proc, h.hi_state, h.lo_proc
                    );
                }
            }
            _ => {}
        }
    }
    if !assigned_before_send {
        bail!(
            "Level-4 violation in spec '{spec}': `{}` never updates `{}` — nothing bounds \
             `{}.{}`",
            h.hi_proc, h.hi_state, h.lo_proc, h.lo_state
        );
    }

    // FLOW: every path into B passes through A, and multiplicity A→B is static.
    // ways(X) counts A-originating multiplicity; leak(X) flags any way to reach
    // X from an entry without passing through A.
    let mut ways: BTreeMap<&str, Option<u64>> = BTreeMap::new(); // None = non-static (broadcast)
    let mut leak: BTreeMap<&str, bool> = BTreeMap::new();
    for pname in &topo.order {
        let pname = pname.as_str();
        if pname == h.hi_proc {
            ways.insert(pname, Some(1));
            leak.insert(pname, false);
            continue;
        }
        let preds: Vec<_> = topo.edges.iter().filter(|e| e.to == pname).collect();
        if preds.is_empty() {
            // An entry other than A: contributes leak if it can reach B.
            ways.insert(pname, Some(0));
            leak.insert(pname, true);
            continue;
        }
        let mut w: Option<u64> = Some(0);
        let mut l = false;
        for e in &preds {
            let m = edge_mult
                .get(&(e.from.clone(), e.to.clone()))
                .cloned()
                .unwrap_or(Some(0));
            let pw = ways.get(e.from.as_str()).cloned().unwrap_or(Some(0));
            l = l || *leak.get(e.from.as_str()).unwrap_or(&false);
            match (w, pw, m) {
                (Some(acc), Some(pw), Some(m)) => w = Some(acc + pw * m),
                _ => w = None,
            }
        }
        ways.insert(pname, w);
        leak.insert(pname, l);
    }
    if *leak.get(h.lo_proc.as_str()).unwrap_or(&true) {
        bail!(
            "Level-4 violation in spec '{spec}': FLOW fails — `{}` is reachable from an \
             entry that does not pass through `{}`, so `{}.{}` counts messages `{}` never \
             saw",
            h.lo_proc, h.hi_proc, h.lo_proc, h.lo_state, h.hi_proc
        );
    }
    let mult = match ways.get(h.lo_proc.as_str()) {
        Some(Some(m)) => *m,
        _ => bail!(
            "Level-4 violation in spec '{spec}': FLOW fails — a broadcast edge lies on a \
             path `{}` → `{}`; broadcast multiplies by the shard count, which is a runtime \
             value with no static bound",
            h.hi_proc, h.lo_proc
        ),
    };
    if mult == 0 {
        bail!(
            "Level-4 violation in spec '{spec}': `{}` never reaches `{}` in the topology",
            h.hi_proc, h.lo_proc
        );
    }

    // GAP: mult × max(db.hi, 0) <= da.lo
    let db_up = db.hi.max(0.0);
    let need = (mult as f64) * db_up;
    let ok = if h.strict { need < da.lo } else { need <= da.lo };
    if !ok {
        bail!(
            "Level-4 violation in spec '{spec}': GAP fails — along `{}` → `{}` a message \
             can add up to {db_up} to `{}` across multiplicity {mult}, but is only \
             guaranteed to add {} to `{}`. Guard the increments (e.g. use literal +1 \
             counters, or `require` an upper bound on `{}`'s increment).",
            h.hi_proc, h.lo_proc, h.lo_state, da.lo, h.hi_state, h.lo_state
        );
    }
    Ok(())
}
