//! Topology export for humans.
//!
//! At 3 a.m. the question is "what is this system actually shaped like, and
//! what did the compiler believe when it proved things about it?" These
//! exports answer both from the SAME `Topology` the provers consumed, so the
//! picture cannot drift from the analysis. Anything shown here — edge message
//! types, routing, back-pressure, per-stage latency — is a fact the compiler
//! used, not a redrawing of the source.

use crate::analysis::topology::Topology;
use crate::frontend::ast::{Program, Route, Stmt};
use std::collections::BTreeMap;

/// How a given edge is routed and back-pressured, read off the source.
struct EdgeFacts {
    route: String,
    backpressure: String,
    guarded: bool,
}

fn edge_facts(program: &Program, from: &str, from_handler: &str, to: &str) -> EdgeFacts {
    let mut route = "round-robin".to_string();
    let mut backpressure = "@block".to_string();
    let mut guarded = false;
    for process in program.processes.iter().filter(|p| p.name == from) {
        for handler in process
            .handlers
            .iter()
            .filter(|h| h.msg_name == from_handler)
        {
            for stmt in &handler.body {
                let Stmt::Send {
                    target,
                    route: r,
                    backpressure: bp,
                    guard,
                    ..
                } = stmt
                else {
                    continue;
                };
                if target != to {
                    continue;
                }
                route = match r {
                    Route::RoundRobin => "round-robin".into(),
                    Route::ByKey(_) => "by key".into(),
                    Route::Broadcast => "broadcast".into(),
                };
                backpressure = bp.describe();
                guarded = guard.is_some();
            }
        }
    }
    EdgeFacts {
        route,
        backpressure,
        guarded,
    }
}

/// Worst-case ms a message spends in each process, as Level 2 computed it.
fn stage_latency(program: &Program) -> BTreeMap<String, u64> {
    program
        .processes
        .iter()
        .map(|p| {
            (
                p.name.clone(),
                crate::analysis::level2::process_worst_case_ms_pub(p),
            )
        })
        .collect()
}

/// Mermaid flowchart. Renders in GitHub, most wikis, and many editors.
pub fn to_mermaid(program: &Program, topo: &Topology) -> String {
    let mut out = String::new();
    let lat = stage_latency(program);

    out.push_str("%% Verified process topology, exported by sigilc.\n");
    out.push_str("%% Every fact here was used by the compiler's analysis.\n");
    out.push_str("flowchart LR\n");

    for p in &program.processes {
        let states: Vec<String> = p.states.iter().map(|s| s.name.clone()).collect();
        let handlers: Vec<String> = p
            .handlers
            .iter()
            .map(|h| crate::analysis::types::type_name(&h.msg_ty))
            .collect();
        let ms = lat.get(&p.name).copied().unwrap_or(0);
        let label = format!(
            "{}<br/>on: {}<br/>state: {}<br/>worst case {}ms",
            p.name,
            if handlers.is_empty() {
                "-".into()
            } else {
                handlers.join(", ")
            },
            if states.is_empty() {
                "-".into()
            } else {
                states.join(", ")
            },
            ms
        );
        // Entry processes get a distinct shape: they are fed from outside the
        // system, which is exactly what the FLOW obligation cares about.
        let is_entry = !topo.edges.iter().any(|e| e.to == p.name);
        if is_entry {
            out.push_str(&format!("  {}([\"{}\"])\n", p.name, label));
        } else {
            out.push_str(&format!("  {}[\"{}\"]\n", p.name, label));
        }
    }

    for e in &topo.edges {
        let f = edge_facts(program, &e.from, &e.from_handler, &e.to);
        let guard = if f.guarded { ", conditional" } else { "" };
        out.push_str(&format!(
            "  {} -->|\"{} → on {}<br/>{}, {}{}\"| {}\n",
            e.from, e.msg_type, e.to_handler, f.route, f.backpressure, guard, e.to
        ));
    }

    if topo.edges.is_empty() && program.processes.len() == 1 {
        out.push_str("  %% single process: no inter-process edges\n");
    }
    out
}

/// Graphviz DOT, for pipelines that already render DOT in CI.
pub fn to_dot(program: &Program, topo: &Topology) -> String {
    let mut out = String::new();
    let lat = stage_latency(program);
    out.push_str("// Verified process topology, exported by sigilc.\n");
    out.push_str("digraph sigil {\n  rankdir=LR;\n  node [shape=box, fontname=\"monospace\"];\n");
    for p in &program.processes {
        let ms = lat.get(&p.name).copied().unwrap_or(0);
        let is_entry = !topo.edges.iter().any(|e| e.to == p.name);
        let shape = if is_entry { ", shape=oval" } else { "" };
        out.push_str(&format!(
            "  \"{}\" [label=\"{}\\nworst case {}ms\"{}];\n",
            p.name, p.name, ms, shape
        ));
    }
    for e in &topo.edges {
        let f = edge_facts(program, &e.from, &e.from_handler, &e.to);
        let guard = if f.guarded { ", conditional" } else { "" };
        out.push_str(&format!(
            "  \"{}\" -> \"{}\" [label=\"{} → on {}\\n{}, {}{}\"];\n",
            e.from, e.to, e.msg_type, e.to_handler, f.route, f.backpressure, guard
        ));
    }
    out.push_str("}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::topology::derive_topology;
    use crate::frontend::ast::parse;

    const SRC: &str = r#"
schema M { id: String, n: Int }
transform ext(m: M) -> M {}
transform pure_f(m: M) -> M { m }
process Up {
  state seen: Int = 0
  on m: M {
    let z = m ~> ext @timeout(30.ms) @retry(1) @recover(with: pure_f)
    seen := seen + 1
    send z to Down by z.id @deadline(5.ms) when z.n > 0
  }
}
process Down {
  state got: Int = 0
  on m: M { got := got + 1 }
}
"#;

    #[test]
    fn mermaid_shows_what_the_prover_saw() {
        let program = parse(SRC).expect("parse");
        let topo = derive_topology(&program).expect("topology");
        let m = to_mermaid(&program, &topo);
        assert!(m.contains("flowchart LR"));
        // Edge carries the resolved type AND destination handler.
        assert!(m.contains("M → on m"), "{m}");
        // Routing and back-pressure are proof-relevant facts.
        assert!(m.contains("by key") && m.contains("@deadline(5.ms)"), "{m}");
        assert!(
            m.contains("conditional"),
            "guarded sends must be visible: {m}"
        );
        // Latency shown is the Level-2 per-process worst case: (1+1)*30.
        assert!(m.contains("worst case 60ms"), "{m}");
        // Entry processes are visually distinct (FLOW cares about them).
        assert!(m.contains("Up([") && m.contains("Down["), "{m}");
    }

    #[test]
    fn dot_export_is_wellformed() {
        let program = parse(SRC).expect("parse");
        let topo = derive_topology(&program).expect("topology");
        let d = to_dot(&program, &topo);
        assert!(d.starts_with("// Verified process topology"));
        assert!(d.contains("digraph sigil {") && d.trim_end().ends_with('}'));
        assert_eq!(d.matches("->").count(), 1, "one edge");
    }

    #[test]
    fn single_process_programs_export_cleanly() {
        let src =
            "schema M { id: String }\nprocess P { state c: Int = 0\n on m: M { c := c + 1 } }\n";
        let program = parse(src).expect("parse");
        let topo = derive_topology(&program).expect("topology");
        let m = to_mermaid(&program, &topo);
        assert!(m.contains("single process"));
        assert!(!m.contains("-->"));
    }
}
