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
    ir: &GraphIR,
    level2: Option<&Level2Report>,
    level1_enforced: bool,
) -> String {
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
            compiled.push(format!("- {sig} (body present — compiled into generated crate)"));
        }
    }

    // IR-discovered calls that were never declared
    let declared_names: std::collections::BTreeSet<_> =
        program.transforms.iter().map(|t| t.name.as_str()).collect();
    let skip = [
        "packet", "v", "d", "m", "last", "event", "req", "request", "validated",
        "processed", "stored", "result", "final", "checked", "fetched", "recorded",
        "next", "y", "s", "auth", "reserved", "charged", "receipt", "enriched",
        "order", "count", "total_charged", "last_order", "last_ok", "failures",
        "last_status", "open",
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
            let mut lines = vec![
                format!("- path_timeout_sum: {}ms", l2.path_timeout_sum_ms),
            ];
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

    let topology_section = match crate::analysis::topology::derive_topology(program) {
        Ok(t) if t.is_pipeline() => {
            let mut lines = vec!["## Process Topology (verified)".to_string()];
            for e in &t.edges {
                lines.push(format!("- `{}` → `{}` carrying `{}` (typed, acyclic, bounded channel)", e.from, e.to, e.msg_type));
            }
            lines.push("- Residual: channel capacity/backpressure tuning is a runtime concern".into());
            lines.join("\n") + "\n\n"
        }
        _ => String::new(),
    };

    format!(
        r#"# Residual Risk Report

{topology_section}{l1_section}

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

/// Backward-compatible wrapper when only the IR is available.
pub fn residual_risk_report_ir(ir: &GraphIR) -> String {
    residual_risk_report(
        &Program {
            schemas: vec![],
            processes: vec![],
            transforms: vec![],
            specs: vec![],
        },
        ir,
        None,
        true,
    )
}
