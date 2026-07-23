
//! Level-1 extinct-by-design checks.

use crate::ir::GraphIR;
use anyhow::{bail, Result};

pub fn level1_check(ir: &GraphIR) -> Result<()> {
    if ir.has_timeout && !ir.has_recover {
        bail!("Level-1 violation: @timeout without a matching @recover path");
    }

    // State locality is enforced by the surface language and the IR construction.
    // Additional checks (schema flow, etc.) will be added as the IR grows.

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::GraphIR;

    #[test]
    fn accepts_handled_timeout() {
        let ir = GraphIR {
            process_name: "P".into(),
            local_states: vec!["s".into()],
            has_timeout: true,
            has_recover: true,
            external_calls: vec![],
        };
        assert!(level1_check(&ir).is_ok());
    }

    #[test]
    fn rejects_unhandled_timeout() {
        let ir = GraphIR {
            process_name: "P".into(),
            local_states: vec![],
            has_timeout: true,
            has_recover: false,
            external_calls: vec![],
        };
        assert!(level1_check(&ir).is_err());
    }
}
