
//! Level-1 extinct-by-design checks on the Graph IR.

use crate::ir::{GraphIR, Node};
use anyhow::{bail, Result};

pub fn level1_check(ir: &GraphIR) -> Result<()> {
    let has_timeout = ir.has_timeout();
    let has_recover = ir.has_recover();

    if has_timeout && !has_recover {
        bail!("Level-1 violation: @timeout without a matching @recover path");
    }

    // StateWrite only to local slots
    for node in &ir.nodes {
        if let Node::StateWrite { slot } = node {
            if !ir.local_states.contains(slot) {
                bail!("Level-1 violation: state write to non-local slot '{}'", slot);
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
            local_states: vec!["s".into()],
            nodes: vec![
                Node::Timeout { ms: 50 },
                Node::Recover { fallback: "f".into() },
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
            local_states: vec![],
            nodes: vec![Node::Timeout { ms: 50 }],
            edges: vec![],
            external_calls: vec![],
        };
        assert!(level1_check(&ir).is_err());
    }

    #[test]
    fn rejects_nonlocal_state_write() {
        let ir = GraphIR {
            process_name: "P".into(),
            local_states: vec!["s".into()],
            nodes: vec![Node::StateWrite { slot: "other".into() }],
            edges: vec![],
            external_calls: vec![],
        };
        assert!(level1_check(&ir).is_err());
    }
}
