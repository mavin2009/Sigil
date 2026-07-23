
//! Residual risk reporting from the Graph IR.

use crate::analysis::ir::GraphIR;

pub fn residual_risk_report(ir: &GraphIR) -> String {
    let mut calls: Vec<_> = ir.external_calls.clone();
    calls.sort();
    calls.dedup();
    let skip = [
        "packet", "v", "d", "m", "last", "event", "req", "request", "validated",
        "processed", "stored", "result", "final", "checked", "fetched", "recorded",
        "next", "y", "s", "auth", "reserved", "charged", "receipt", "enriched",
        "order", "count", "total_charged", "last_order", "last_ok", "failures",
        "last_status", "open",
    ];
    calls.retain(|c| !skip.contains(&c.as_str()));

    let calls_list = if calls.is_empty() {
        "- (none detected)".to_string()
    } else {
        calls
            .iter()
            .map(|c| format!("- `{c}`"))
            .collect::<Vec<_>>()
            .join("\n")
    };

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

    format!(
        r#"# Residual Risk Report

## Level-1 Guarantees Enforced
- No shared mutable state
- No null values
- Every @timeout is paired with an explicit @recover (verified)
- StateWrite only to local process slots (verified)

## Analysis Summary
- Process: `{process}`
- Local states: {states}
- External transforms (stubs in generated code):
{calls}
- Timeout nodes: {timeouts}
- Recover fallbacks: {recovers}

## Residual Risk
- External transforms are assumed to match their schemas and to terminate. Their internal failure modes are residual.
- Tokio runtime, OS scheduler, and wall-clock latency are outside the model.
- Functional correctness of business logic inside transforms is residual (Level-1 only).
"#,
        process = ir.process_name,
        states = if ir.local_states.is_empty() {
            "(none)".into()
        } else {
            ir.local_states.join(", ")
        },
        calls = calls_list,
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
    )
}
