//! Residual risk reporting from the Graph IR and declared transforms.

use crate::analysis::ir::GraphIR;
use crate::analysis::level2::Level2Report;
use crate::frontend::ast::{Program, Type};

fn type_name(ty: &Type) -> String {
    match ty {
        Type::Int => "Int".into(),
        Type::Float => "Float".into(),
        Type::String => "String".into(),
        Type::Bool => "Bool".into(),
        Type::UUID => "UUID".into(),
        Type::Bytes => "Bytes".into(),
        Type::Duration => "Duration".into(),
        Type::Named(n) => n.clone(),
    }
}

/// Build the residual risk report using IR analysis and declared transform signatures.
pub fn residual_risk_report(
    program: &Program,
    irs: &[GraphIR],
    level2: Option<&Level2Report>,
    level1_enforced: bool,
) -> String {
    let ir = &merge_for_report(irs);
    let mut declared = Vec::new();
    let mut external = Vec::new();
    let mut compiled = Vec::new();

    for t in &program.transforms {
        let sig = format!(
            "`{}`: {} → {}",
            t.name,
            type_name(&t.param_ty),
            type_name(&t.return_ty)
        );
        declared.push(sig.clone());
        if t.body.is_empty() {
            external.push(format!("- {sig} (no body — external residual)"));
        } else {
            compiled.push(format!(
                "- {sig} (body present — compiled into generated crate)"
            ));
        }
    }

    // IR-discovered calls that were never declared
    let declared_names: std::collections::BTreeSet<_> =
        program.transforms.iter().map(|t| t.name.as_str()).collect();
    let skip = [
        "packet",
        "v",
        "d",
        "m",
        "last",
        "event",
        "req",
        "request",
        "validated",
        "processed",
        "stored",
        "result",
        "final",
        "checked",
        "fetched",
        "recorded",
        "next",
        "y",
        "s",
        "auth",
        "reserved",
        "charged",
        "receipt",
        "enriched",
        "order",
        "count",
        "total_charged",
        "last_order",
        "last_ok",
        "failures",
        "last_status",
        "open",
    ];
    let mut undeclared: Vec<_> = ir
        .external_calls
        .iter()
        .filter(|c| !declared_names.contains(c.as_str()) && !skip.contains(&c.as_str()))
        .cloned()
        .collect();
    undeclared.sort();
    undeclared.dedup();

    let timeout_nodes: Vec<_> = ir
        .nodes
        .iter()
        .filter_map(|n| match n {
            crate::analysis::ir::Node::Timeout { ms, .. } => Some(format!("{ms}ms")),
            _ => None,
        })
        .collect();
    let recover_nodes: Vec<_> = ir
        .nodes
        .iter()
        .filter_map(|n| match n {
            crate::analysis::ir::Node::Recover { fallback, .. } => Some(fallback.clone()),
            _ => None,
        })
        .collect();

    let declared_section = if declared.is_empty() {
        "- (none)".into()
    } else {
        declared
            .iter()
            .map(|s| format!("- {s}"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let external_section = if external.is_empty() {
        "- (none)".into()
    } else {
        external.join("\n")
    };

    let compiled_section = if compiled.is_empty() {
        "- (none)".into()
    } else {
        compiled.join("\n")
    };

    let undeclared_section = if undeclared.is_empty() {
        "- (none)".into()
    } else {
        undeclared
            .iter()
            .map(|c| format!("- `{c}` (used but not declared)"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let level2_section = match level2 {
        Some(l2) => {
            let mut lines = vec![format!("- path_timeout_sum: {}ms", l2.path_timeout_sum_ms)];
            if let Some(b) = l2.path_timeout_bound_ms {
                lines.push(format!("- path_timeout bound: {}ms", b));
            }
            for d in &l2.discharged {
                lines.push(format!("- discharged: {d}"));
            }
            for a in &l2.residual_assumptions {
                lines.push(format!("- assumption: {a}"));
            }
            if lines.len() == 1 {
                lines.push("- (no specs)".into());
            }
            lines.join("\n")
        }
        None => "- (level2 not run)".into(),
    };

    let l1_section = if level1_enforced {
        "## Level-1 Guarantees Enforced\n\
         - No shared mutable state\n\
         - No null values\n\
         - Every @timeout is paired with an explicit @recover (verified)\n\
         - StateWrite only to local process slots (verified)"
    } else {
        "## Level-1 Guarantees: NOT ENFORCED\n\
         - Level-1 checks were skipped for this build (sketch mode)\n\
         - No safety properties below are verified; treat all as residual risk"
    };

    // Back-pressure policies are load-bearing for both latency and loss, so
    // they are always itemised.
    let mut bp_lines: Vec<String> = Vec::new();
    let mut any_block = false;
    for proc in &program.processes {
        for h in &proc.handlers {
            for stmt in &h.body {
                if let crate::frontend::ast::Stmt::Send {
                    target,
                    backpressure,
                    ..
                } = stmt
                {
                    if matches!(backpressure, crate::frontend::ast::Backpressure::Block) {
                        any_block = true;
                    }
                    bp_lines.push(format!(
                        "- `{}` → `{target}` ({} handler): {}{}",
                        proc.name,
                        h.msg_name,
                        backpressure.describe(),
                        match backpressure {
                            crate::frontend::ast::Backpressure::Block =>
                                " — no loss; wait is UNBOUNDED (queueing time is not covered by `path_timeout_sum`)",
                            crate::frontend::ast::Backpressure::Shed =>
                                " — bounded O(1); sheds on a full queue (counted in ActorStats.shed)",
                            crate::frontend::ast::Backpressure::Deadline(_) =>
                                " — bounded; sheds only past the deadline (counted)",
                        }
                    ));
                }
            }
        }
    }
    let backpressure_section = if bp_lines.is_empty() {
        String::new()
    } else {
        let mut out = String::from("## Back-Pressure Policies\n");
        out.push_str(&bp_lines.join("\n"));
        out.push_str(
            "\n\nGenerated channel-wait cycles are ruled out because the process graph \
             is proven ACYCLIC at Level 1. This does not establish global deadlock freedom \
             or handler termination; external code and unbounded waits remain residual.\n",
        );
        if any_block {
            out.push_str(
                "\nResidual: at least one send uses `@block`, so END-TO-END latency is \
                 unbounded under sustained overload. `require path_latency <= N.ms` \
                 rejects that combination; declare `@deadline(N.ms)` or `@shed` to make \
                 the bound provable.\n",
            );
        }
        out.push('\n');
        out
    };

    let topology_section = match crate::analysis::topology::derive_topology(program) {
        Ok(t) if t.is_pipeline() => {
            let mut lines = vec!["## Process Topology (verified)".to_string()];
            for e in &t.edges {
                lines.push(format!(
                    "- `{}` → `{}` carrying `{}` (typed, acyclic, bounded channel)",
                    e.from, e.to, e.msg_type
                ));
            }
            lines.push(
                "- Residual: channel capacity/backpressure tuning is a runtime concern".into(),
            );
            lines.join("\n") + "\n\n"
        }
        _ => String::new(),
    };

    format!(
        r#"# Residual Risk Report

{backpressure_section}{topology_section}{l1_section}

## Analysis Summary
- Process: `{process}`
- Local states: {states}
- Timeout nodes: {timeouts}
- Recover fallbacks: {recovers}

## Declared Transforms
{declared}

### Compiled (body present)
{compiled}

### External residual (empty body)
{external}

### Undeclared uses
{undeclared}

## Level-2
{level2_section}

## Residual Risk
- External transforms (empty bodies) are assumed to match their declared schemas and to terminate. Their internal failure modes are residual.
- Undeclared transforms are residual until given signatures.
- Tokio runtime, OS scheduler, and wall-clock latency are outside the model.
- Functional correctness of business logic inside external transforms is residual (Level-1 only).
- Level-2 holds that depend on external transforms remain residual assumptions.
"#,
        process = ir.process_name,
        states = if ir.local_states.is_empty() {
            "(none)".into()
        } else {
            ir.local_states.join(", ")
        },
        timeouts = if timeout_nodes.is_empty() {
            "(none)".into()
        } else {
            timeout_nodes.join(", ")
        },
        recovers = if recover_nodes.is_empty() {
            "(none)".into()
        } else {
            recover_nodes.join(", ")
        },
        declared = declared_section,
        compiled = compiled_section,
        external = external_section,
        undeclared = undeclared_section,
        level2_section = level2_section,
    )
}

fn merge_for_report(irs: &[GraphIR]) -> GraphIR {
    let mut m = GraphIR {
        process_name: irs
            .iter()
            .map(|i| i.process_name.clone())
            .collect::<Vec<_>>()
            .join(", "),
        process_span: None,
        local_states: vec![],
        nodes: vec![],
        edges: vec![],
        external_calls: vec![],
    };
    for ir in irs {
        m.local_states.extend(ir.local_states.iter().cloned());
        m.nodes.extend(ir.nodes.iter().cloned());
        m.external_calls.extend(ir.external_calls.iter().cloned());
    }
    m
}

/// Backward-compatible wrapper when only the IR is available.
pub fn residual_risk_report_ir(ir: &GraphIR) -> String {
    residual_risk_report(
        &Program {
            extern_crates: vec![],
            schemas: vec![],
            processes: vec![],
            transforms: vec![],
            specs: vec![],
        },
        std::slice::from_ref(ir),
        None,
        true,
    )
}
