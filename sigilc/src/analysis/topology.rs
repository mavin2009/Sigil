//! Process topology: derived from `send <expr> to <Process>` statements.
//!
//! The compiler wires actors together, so it must know the shape of the
//! system: which process sends to which, with what message type, and in what
//! order they can be spawned and shut down. Level-1 obligations here:
//!   - every send target is a declared process
//!   - the sent value's type matches the target's handler message type
//!   - the graph is a DAG (cycles over bounded channels can deadlock;
//!     rejected until an explicit async-boundary construct exists)

use crate::analysis::types::type_name;
use crate::frontend::ast::{Expr, Program, Route, Stmt};
use anyhow::{bail, Result};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct TopologyEdge {
    pub from: String,
    pub to: String,
    pub msg_type: String,
    /// Message name of the destination handler this edge dispatches to.
    /// With multi-handler processes the target is resolved BY TYPE, so
    /// codegen knows exactly which variant to construct.
    pub to_handler: String,
}

#[derive(Debug, Clone, Default)]
pub struct Topology {
    pub edges: Vec<TopologyEdge>,
    /// Processes in spawn-safe topological order: entries first, sinks last.
    pub order: Vec<String>,
    /// Processes with no incoming edges (fed by the outside world).
    pub entries: Vec<String>,
}

impl Topology {
    pub fn is_pipeline(&self) -> bool {
        !self.edges.is_empty()
    }

    pub fn targets_of(&self, process: &str) -> Vec<&TopologyEdge> {
        self.edges.iter().filter(|e| e.from == process).collect()
    }
}

/// Static types of the bindings in one handler, resolved locally.
///
/// A program-global environment would let a binding name in one process
/// silently resolve against a same-named binding in another; for `send`
/// dispatch that would mean delivering to the wrong handler. This walks a
/// single handler with declared transform signatures only.
fn local_binding_types(
    program: &Program,
    handler: &crate::frontend::ast::OnHandler,
) -> BTreeMap<String, String> {
    let sigs: BTreeMap<&str, (String, String)> = program
        .transforms
        .iter()
        .map(|t| {
            (
                t.name.as_str(),
                (type_name(&t.param_ty), type_name(&t.return_ty)),
            )
        })
        .collect();

    let mut env: BTreeMap<String, String> = BTreeMap::new();
    env.insert(handler.msg_name.clone(), type_name(&handler.msg_ty));

    // Type of an expression, given what is known so far.
    fn ty_of(
        e: &Expr,
        env: &BTreeMap<String, String>,
        sigs: &BTreeMap<&str, (String, String)>,
    ) -> Option<String> {
        match e {
            Expr::Ident { name, .. } => env.get(name).cloned(),
            Expr::Call { name, .. } => sigs.get(name.as_str()).map(|(_, o)| o.clone()),
            Expr::Pipeline { base, steps, .. } => {
                let mut cur = ty_of(base, env, sigs);
                for step in steps {
                    let target = match &step.expr {
                        Expr::Ident { name, .. } | Expr::Call { name, .. } => Some(name.as_str()),
                        _ => None,
                    };
                    cur = target.and_then(|n| sigs.get(n)).map(|(_, o)| o.clone());
                }
                cur
            }
            _ => None,
        }
    }

    for stmt in &handler.body {
        if let Stmt::Let { name, expr, .. } = stmt {
            if let Some(t) = ty_of(expr, &env, &sigs) {
                env.insert(name.clone(), t);
            }
        }
    }
    env
}

/// Derive and validate the process topology.
pub fn derive_topology(program: &Program) -> Result<Topology> {
    let process_names: BTreeSet<&str> =
        program.processes.iter().map(|p| p.name.as_str()).collect();

    let mut edges: Vec<TopologyEdge> = Vec::new();

    for process in &program.processes {
        for handler in &process.handlers {
            // Types are resolved per handler; a global environment would let
            // same-named bindings in different processes cross-contaminate.
            let local = local_binding_types(program, handler);
            for stmt in &handler.body {
                let Stmt::Send { target, expr, route, .. } = stmt else {
                    continue;
                };
                if let Route::ByKey(key) = route {
                    // Resolve the key's type when statically known; Float keys
                    // are rejected — hashing floats is nondeterministic
                    // production folklore for a reason.
                    let key_ty: Option<String> = match key {
                        Expr::FieldAccess { base, field, .. } => {
                            let base_ty = local.get(base).cloned();
                            base_ty.and_then(|bt| {
                                program.schemas.iter().find(|sc| sc.name == bt).and_then(
                                    |sc| {
                                        sc.fields
                                            .iter()
                                            .find(|(f, _)| f == field)
                                            .map(|(_, ty)| type_name(ty))
                                    },
                                )
                            })
                        }
                        Expr::Ident { name, .. } => local.get(name).cloned(),
                        _ => None,
                    };
                    if key_ty.as_deref() == Some("Float") {
                        bail!(
                            "topology violation in process '{}': `send ... to {target} by <key>` \
                             uses a Float key — float hashing is not a stable shard function; \
                             route by a String, Int, UUID, or Bool field instead",
                            process.name
                        );
                    }
                }
                if !process_names.contains(target.as_str()) {
                    bail!(
                        "topology violation in process '{}': send target '{target}' \
                         is not a declared process",
                        process.name
                    );
                }
                if target == &process.name {
                    bail!(
                        "topology violation in process '{}': self-send would deadlock \
                         on a bounded channel",
                        process.name
                    );
                }
                let dest = program
                    .processes
                    .iter()
                    .find(|p| p.name == *target)
                    .expect("target existence checked above");
                if dest.handlers.is_empty() {
                    bail!("topology violation: send target '{target}' has no handlers");
                }

                // Static type of the sent value, inferred locally.
                let actual: Option<String> = match expr {
                    Expr::Ident { name, .. } => local.get(name).cloned(),
                    Expr::Call { name, .. } => program
                        .transforms
                        .iter()
                        .find(|t| t.name == *name)
                        .map(|t| type_name(&t.return_ty)),
                    _ => None,
                };

                // Resolve the destination handler BY TYPE.
                let (dest_handler, expected) = if dest.handlers.len() == 1 {
                    let h = &dest.handlers[0];
                    (h, type_name(&h.msg_ty))
                } else {
                    let Some(actual_ty) = actual.clone() else {
                        bail!(
                            "topology violation in process '{}': cannot resolve which \
                             handler of '{target}' receives this send — '{target}' has {} \
                             handlers and the sent value's type is not statically known. \
                             Bind it with `let` from a declared transform first.",
                            process.name,
                            dest.handlers.len()
                        );
                    };
                    let matches: Vec<_> = dest
                        .handlers
                        .iter()
                        .filter(|h| type_name(&h.msg_ty) == actual_ty)
                        .collect();
                    match matches.len() {
                        1 => (matches[0], actual_ty),
                        0 => bail!(
                            "topology violation in process '{}': sends `{actual_ty}` to \
                             '{target}', which has no handler for that type (handlers: {})",
                            process.name,
                            dest.handlers
                                .iter()
                                .map(|h| format!("`{}`", type_name(&h.msg_ty)))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        _ => bail!(
                            "topology violation: process '{target}' has {} handlers for \
                             type `{actual_ty}` — message types must uniquely identify a \
                             handler",
                            matches.len()
                        ),
                    }
                };

                if let Some(actual) = actual {
                    if actual != expected {
                        bail!(
                            "topology violation in process '{}': sends `{actual}` to \
                             '{target}' whose handler expects `{expected}`",
                            process.name
                        );
                    }
                }

                if !edges.iter().any(|e| {
                    e.from == process.name && e.to == *target && e.to_handler == dest_handler.msg_name
                }) {
                    edges.push(TopologyEdge {
                        from: process.name.clone(),
                        to: target.clone(),
                        msg_type: expected,
                        to_handler: dest_handler.msg_name.clone(),
                    });
                }
            }
        }
    }

    // Topological order (Kahn). Includes processes with no edges at all.
    let mut indegree: BTreeMap<&str, usize> = BTreeMap::new();
    for p in &program.processes {
        indegree.insert(p.name.as_str(), 0);
    }
    for e in &edges {
        *indegree.get_mut(e.to.as_str()).unwrap() += 1;
    }
    let mut queue: Vec<&str> = program
        .processes
        .iter()
        .map(|p| p.name.as_str())
        .filter(|n| indegree[n] == 0)
        .collect();
    let entries: Vec<String> = queue
        .iter()
        .filter(|n| {
            edges.iter().any(|e| e.from == **n) || edges.is_empty() || {
                // a checked process with no edges at all still counts as an entry
                !edges.iter().any(|e| e.to == **n)
            }
        })
        .map(|n| n.to_string())
        .collect();
    let mut order: Vec<String> = Vec::new();
    while let Some(n) = queue.pop() {
        order.push(n.to_string());
        for e in edges.iter().filter(|e| e.from == n) {
            let d = indegree.get_mut(e.to.as_str()).unwrap();
            *d -= 1;
            if *d == 0 {
                queue.push(e.to.as_str());
            }
        }
    }
    if order.len() != program.processes.len() {
        let stuck: Vec<&str> = indegree
            .iter()
            .filter(|(_, d)| **d > 0)
            .map(|(n, _)| *n)
            .collect();
        bail!(
            "topology violation: cycle detected among processes {:?} — cycles over \
             bounded channels can deadlock and are not yet supported",
            stuck
        );
    }

    Ok(Topology {
        edges,
        order,
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::ast::parse;

    const CHAIN: &str = r#"
schema M { v: Int }
transform f(m: M) -> M {}
transform g(m: M) -> M { m }
process A {
  state n: Int = 0
  on m: M {
    let out = m ~> f @recover(with: g)
    n := n + out.v
    send out to B
  }
}
process B {
  state total: Int = 0
  on m: M {
    total := total + m.v
  }
}
"#;

    #[test]
    fn derives_chain_topology_in_order() {
        let program = parse(CHAIN).expect("parse");
        let topo = derive_topology(&program).expect("valid topology");
        assert!(topo.is_pipeline());
        assert_eq!(topo.edges.len(), 1);
        assert_eq!(topo.edges[0].from, "A");
        assert_eq!(topo.edges[0].to, "B");
        assert_eq!(topo.edges[0].msg_type, "M");
        assert_eq!(topo.order, vec!["A".to_string(), "B".to_string()]);
        assert_eq!(topo.entries, vec!["A".to_string()]);
    }

    #[test]
    fn rejects_send_to_undeclared_process() {
        let src = CHAIN.replace("send out to B", "send out to Nowhere");
        let program = parse(&src).expect("parse");
        let err = derive_topology(&program).expect_err("must reject");
        assert!(format!("{err}").contains("Nowhere"));
    }

    #[test]
    fn rejects_cycles() {
        let src = format!(
            "{CHAIN}\n"
        )
        .replace(
            "  on m: M {\n    total := total + m.v\n  }",
            "  on m: M {\n    total := total + m.v\n    send m to A\n  }",
        );
        let program = parse(&src).expect("parse");
        let err = derive_topology(&program).expect_err("must reject cycle");
        assert!(format!("{err}").contains("cycle"));
    }

    #[test]
    fn rejects_type_mismatched_send() {
        let src = r#"
schema M { v: Int }
schema N { w: Int }
process A {
  state n: Int = 0
  on m: M {
    n := n + m.v
    send m to B
  }
}
process B {
  state t: Int = 0
  on x: N {
    t := t + x.w
  }
}
"#;
        let program = parse(src).expect("parse");
        let err = derive_topology(&program).expect_err("must reject type mismatch");
        let msg = format!("{err}");
        assert!(msg.contains('M') && msg.contains('N'), "got: {msg}");
    }
}
