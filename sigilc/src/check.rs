
//! Level-1 extinct-by-design checks on the Graph IR.

use crate::ir::{GraphIR, Node};
use anyhow::{bail, Result};

pub fn level1_check(ir: &GraphIR) -> Result<()> {
    let has_timeout = ir.has_timeout();
    let has_recover = ir.has_recover();

    if has_timeout && !has_recover {
        // Prefer the first Timeout node's span when available; fall back to process span
        let loc = ir.nodes.iter()
            .find_map(|n| match n {
                Node::Timeout { span: Some(s), .. } => Some(format!(" at bytes {}..{}", s.start, s.end)),
                _ => None,
            })
            .or_else(|| ir.process_span.map(|s| format!(" at bytes {}..{}", s.start, s.end)))
            .unwrap_or_default();
        bail!(
            "Level-1 violation in process '{}'{}: @timeout without a matching @recover path",
            ir.process_name, loc
        );
    }

    // StateWrite only to local slots
    for node in &ir.nodes {
        if let Node::StateWrite { slot } = node {
            if !ir.local_states.contains(slot) {
                let loc = ir.process_span.map(|s| format!(" at bytes {}..{}", s.start, s.end)).unwrap_or_default();
                bail!(
                    "Level-1 violation in process '{}'{}: state write to non-local slot '{}'",
                    ir.process_name, loc, slot
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{GraphIR, Node};

    #[test]
    fn accepts_handled_timeout() {
        let ir = GraphIR {
            process_name: "P".into(),
            process_span: None,
            local_states: vec!["s".into()],
            nodes: vec![
                Node::Timeout { ms: 50, span: None },
                Node::Recover { fallback: "f".into(), span: None },
            ],
            edges: vec![],
            external_calls: vec![],
        };
        assert!(level1_check(&ir).is_ok());
    }

    #[test]
    fn rejects_unhandled_timeout() {
        let ir = GraphIR {
            process_name: "P".into(),
            process_span: None,
            local_states: vec![],
            nodes: vec![Node::Timeout { ms: 50, span: None }],
            edges: vec![],
            external_calls: vec![],
        };
        let err = level1_check(&ir).unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("Level-1 violation"));
        assert!(msg.contains("@timeout"));
    }

    #[test]
    fn rejects_nonlocal_state_write() {
        let ir = GraphIR {
            process_name: "P".into(),
            process_span: None,
            local_states: vec!["s".into()],
            nodes: vec![Node::StateWrite { slot: "other".into() }],
            edges: vec![],
            external_calls: vec![],
        };
        let err = level1_check(&ir).unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("non-local slot"));
    }
}
