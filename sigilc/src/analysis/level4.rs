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
use std::collections::{BTreeMap, BTreeSet};

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

    for h in &holds {
        prove_system_hold(program, &topo, &preconds, h)?;
        let cmp = if h.strict { "<" } else { "<=" };
        report.proven.push(format!(
            "hold `{}.{} {cmp} {}.{}` (spec `{}`): base ordering + update-before-send in \
             every sending handler + all-paths-through-`{}` + static multiplicity bound; \
             robust to every drop the language admits",
            h.lo_proc, h.lo_state, h.hi_proc, h.hi_state, h.spec, h.hi_proc
        ));
    }
    Ok(report)
}

/// Sends to `target` performed by ONE handler execution.
/// `None` means "no static bound" (a broadcast fans out to a runtime shard count).
fn sends_to(handler: &crate::frontend::ast::OnHandler, target: &str) -> Option<u64> {
    let mut n = 0u64;
    for stmt in &handler.body {
        let Stmt::Send { target: t, route, .. } = stmt else { continue };
        if t != target {
            continue;
        }
        if matches!(route, Route::Broadcast) {
            return None;
        }
        n += 1;
    }
    Some(n)
}

/// Multiplicity of messages arriving at `dest` per message handled by each
/// process, maximised over that process's handlers (exactly one handler runs
/// per message, so the max is both sound and tight).
///
/// `None` = no static bound (a broadcast on a path that reaches `dest`).
/// Computed in reverse topological order; the graph is proven acyclic first.
fn multiplicity_to(
    program: &Program,
    topo: &crate::analysis::topology::Topology,
    dest: &str,
) -> BTreeMap<String, Option<u64>> {
    let mut m: BTreeMap<String, Option<u64>> = BTreeMap::new();
    m.insert(dest.to_string(), Some(1));

    for pname in topo.order.iter().rev() {
        if pname == dest {
            continue;
        }
        let Some(p) = program.processes.iter().find(|x| x.name == *pname) else {
            m.insert(pname.clone(), Some(0));
            continue;
        };
        // Distinct successor PROCESSES: a multi-handler target contributes
        // several edges, but `sends_to` already counts sends per process, so
        // iterating edges would double-count.
        let succs: BTreeSet<&str> = topo
            .edges
            .iter()
            .filter(|e| e.from == *pname)
            .map(|e| e.to.as_str())
            .collect();
        let mut best: Option<u64> = Some(0); // max over handlers
        for handler in &p.handlers {
            let mut acc: Option<u64> = Some(0); // this handler's multiplicity
            for succ in &succs {
                let downstream = m.get(*succ).cloned().unwrap_or(Some(0));
                if matches!(downstream, Some(0)) {
                    continue; // this successor never reaches dest
                }
                match sends_to(handler, succ) {
                    None => acc = None, // broadcast onto a path that reaches dest
                    Some(0) => {}
                    Some(c) => {
                        acc = match (acc, downstream) {
                            (Some(a), Some(d)) => Some(a + c * d),
                            _ => None,
                        };
                    }
                }
                if acc.is_none() {
                    break;
                }
            }
            best = match (best, acc) {
                (Some(b), Some(a)) => Some(b.max(a)),
                _ => None,
            };
        }
        m.insert(pname.clone(), best);
    }
    m
}

fn prove_system_hold(
    program: &Program,
    topo: &crate::analysis::topology::Topology,
    preconds: &[crate::analysis::level3::InputPrecondition],
    h: &SystemHold,
) -> Result<()> {
    let spec = &h.spec;
    let a = program
        .processes
        .iter()
        .find(|p| p.name == h.hi_proc)
        .ok_or_else(|| anyhow::anyhow!(
            "Level-4 violation in spec '{spec}': unknown process `{}`", h.hi_proc
        ))?;
    let b = program
        .processes
        .iter()
        .find(|p| p.name == h.lo_proc)
        .ok_or_else(|| anyhow::anyhow!(
            "Level-4 violation in spec '{spec}': unknown process `{}`", h.lo_proc
        ))?;
    if a.name == b.name {
        bail!(
            "Level-4 violation in spec '{spec}': `{}` and `{}` are the same process — \
             use a Level-3 relational hold (`hold {} <= {}`)",
            h.hi_proc, h.lo_proc, h.lo_state, h.hi_state
        );
    }
    let sa = a.states.iter().find(|s| s.name == h.hi_state).ok_or_else(|| {
        anyhow::anyhow!(
            "Level-4 violation in spec '{spec}': `{}` has no state `{}`",
            h.hi_proc, h.hi_state
        )
    })?;
    let sb = b.states.iter().find(|s| s.name == h.lo_state).ok_or_else(|| {
        anyhow::anyhow!(
            "Level-4 violation in spec '{spec}': `{}` has no state `{}`",
            h.lo_proc, h.lo_state
        )
    })?;

    // ---- BASE ----
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
            "Level-4 violation in spec '{spec}': BASE CASE fails — init {}.{} = {ib} vs {}.{} = {ia}",
            h.lo_proc, h.lo_state, h.hi_proc, h.hi_state
        );
    }

    // Per-handler delta of a state, with that handler's let-bindings resolved.
    let empty = BTreeMap::new();
    let delta_of = |p: &crate::frontend::ast::Process,
                    handler: &crate::frontend::ast::OnHandler,
                    st: &str|
     -> Result<Interval> {
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
                "Level-4 violation in spec '{spec}': `{st}` update in `{}`'s `{}` handler \
                 leaves the additive fragment: {}",
                p.name, handler.msg_name, why.join("; ")
            )
        })
    };

    // ---- FLOW: every path into B must pass through A ----
    // R = processes that can reach B WITHOUT passing through A (A absorbs:
    // messages entering A are counted, so paths through it are accounted for).
    let mut reaches_b_avoiding_a: BTreeSet<&str> = BTreeSet::new();
    reaches_b_avoiding_a.insert(b.name.as_str());
    // Reverse topological order guarantees successors are settled first.
    for pname in topo.order.iter().rev() {
        if pname == &a.name || pname == &b.name {
            continue;
        }
        let hits = topo
            .edges
            .iter()
            .any(|e| e.from == *pname && reaches_b_avoiding_a.contains(e.to.as_str()));
        if hits {
            reaches_b_avoiding_a.insert(pname.as_str());
        }
    }
    // Anything with no inbound edges is fed from outside the system.
    let inbound = |name: &str| topo.edges.iter().any(|e| e.to == name);
    for p in &program.processes {
        if p.name == a.name || inbound(&p.name) {
            continue;
        }
        if reaches_b_avoiding_a.contains(p.name.as_str()) {
            bail!(
                "Level-4 violation in spec '{spec}': FLOW fails — `{}` is fed from outside \
                 the system and reaches `{}` without passing through `{}`, so `{}.{}` \
                 counts messages `{}` never saw",
                p.name, h.lo_proc, h.hi_proc, h.lo_proc, h.lo_state, h.hi_proc
            );
        }
    }
    let mult_map = multiplicity_to(program, topo, &b.name);
    if !inbound(&b.name) {
        bail!(
            "Level-4 violation in spec '{spec}': FLOW fails — `{}` has no inbound edges; \
             it is fed from outside the system and is not bounded by `{}`",
            h.lo_proc, h.hi_proc
        );
    }

    // ---- db_max: the most B's counter can grow per message it handles ----
    // Only handlers that some inbound edge actually dispatches to can run.
    let reachable_b: Vec<&crate::frontend::ast::OnHandler> = b
        .handlers
        .iter()
        .filter(|hh| topo.edges.iter().any(|e| e.to == b.name && e.to_handler == hh.msg_name))
        .collect();
    if reachable_b.is_empty() {
        bail!(
            "Level-4 violation in spec '{spec}': no inbound edge dispatches to any handler \
             of `{}`",
            h.lo_proc
        );
    }
    let mut db_max: f64 = 0.0;
    for hh in &reachable_b {
        let d = delta_of(b, hh, &h.lo_state)?;
        if d.hi.is_infinite() {
            bail!(
                "Level-4 violation in spec '{spec}': `{}.{}` has no upper bound per message \
                 in the `{}` handler — guard the increment (e.g. `require` an upper bound, \
                 or use a literal counter)",
                h.lo_proc, h.lo_state, hh.msg_name
            );
        }
        db_max = db_max.max(d.hi.max(0.0));
    }

    // ---- Per-handler obligations on A ----
    let mut any_sends = false;
    for ha in &a.handlers {
        let da = delta_of(a, ha, &h.hi_state)?;
        // No handler may decrease the bounding counter.
        if da.lo < 0.0 {
            bail!(
                "Level-4 violation in spec '{spec}': `{}.{}` can DECREASE in the `{}` \
                 handler (delta lo = {}) — the counting argument needs every handler to be \
                 non-decreasing",
                h.hi_proc, h.hi_state, ha.msg_name, da.lo
            );
        }

        // How many messages reach B per execution of THIS handler.
        // Distinct target processes only — a multi-handler target yields one
        // edge per destination handler, but sends are counted per process.
        let a_succs: BTreeSet<&str> = topo
            .edges
            .iter()
            .filter(|e| e.from == a.name)
            .map(|e| e.to.as_str())
            .collect();
        let mut m_h: Option<u64> = Some(0);
        for succ in &a_succs {
            let downstream = mult_map.get(*succ).cloned().unwrap_or(Some(0));
            if matches!(downstream, Some(0)) {
                continue;
            }
            match sends_to(ha, succ) {
                None => m_h = None,
                Some(0) => {}
                Some(c) => {
                    m_h = match (m_h, downstream) {
                        (Some(acc), Some(d)) => Some(acc + c * d),
                        _ => None,
                    }
                }
            }
        }
        let Some(m_h) = m_h else {
            bail!(
                "Level-4 violation in spec '{spec}': FLOW fails — the `{}` handler of `{}` \
                 broadcasts onto a path reaching `{}`; broadcast multiplies by the runtime \
                 shard count, which has no static bound",
                ha.msg_name, h.hi_proc, h.lo_proc
            );
        };
        if m_h == 0 {
            continue; // this handler produces nothing that reaches B
        }
        any_sends = true;

        // A handler that forwards but never counts is unbounded — say so
        // plainly rather than reporting it as an ordering problem.
        if !ha
            .body
            .iter()
            .any(|st| matches!(st, Stmt::Assign { name, .. } if name == &h.hi_state))
        {
            bail!(
                "Level-4 violation in spec '{spec}': the `{}` handler of `{}` sends toward \
                 `{}` but never updates `{}` — those messages are unbounded",
                ha.msg_name, h.hi_proc, h.lo_proc, h.hi_state
            );
        }

        // ORDERING: the counter must be updated before the first send that
        // can reach B, so a message can never arrive uncounted.
        let mut counted = false;
        for stmt in &ha.body {
            match stmt {
                Stmt::Assign { name, .. } if name == &h.hi_state => counted = true,
                Stmt::Send { target, .. } => {
                    let reaches = mult_map.get(target.as_str()).cloned().unwrap_or(Some(0));
                    if !matches!(reaches, Some(0)) && !counted {
                        bail!(
                            "Level-4 violation in spec '{spec}': ORDERING fails — the `{}` \
                             handler of `{}` sends toward `{}` BEFORE updating `{}`; a \
                             message could arrive uncounted. Move the update above the send.",
                            ha.msg_name, h.hi_proc, h.lo_proc, h.hi_state
                        );
                    }
                }
                _ => {}
            }
        }
        if !counted {
            bail!(
                "Level-4 violation in spec '{spec}': the `{}` handler of `{}` sends toward \
                 `{}` but never updates `{}` — those messages are unbounded",
                ha.msg_name, h.hi_proc, h.lo_proc, h.hi_state
            );
        }

        // GAP: this handler's contribution downstream must be covered.
        let need = (m_h as f64) * db_max;
        let ok = if h.strict { need < da.lo } else { need <= da.lo };
        if !ok {
            bail!(
                "Level-4 violation in spec '{spec}': GAP fails — the `{}` handler of `{}` \
                 forwards up to {m_h} message(s) toward `{}`, each able to add {db_max} to \
                 `{}`, but only guarantees +{} to `{}`. Guard the increments (literal \
                 counters, or `require` an upper bound).",
                ha.msg_name, h.hi_proc, h.lo_proc, h.lo_state, da.lo, h.hi_state
            );
        }
    }
    if !any_sends {
        bail!(
            "Level-4 violation in spec '{spec}': no handler of `{}` reaches `{}` in the \
             topology — the bound is vacuous",
            h.hi_proc, h.lo_proc
        );
    }
    Ok(())
}
