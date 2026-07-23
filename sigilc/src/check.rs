//! Level-1 extinct-by-design checks.

use crate::ir::GraphIR;
use anyhow::{bail, Result};

pub fn level1_check(ir: &GraphIR) -> Result<()> {
    if !ir.has_timeout_with_recover {
        bail!("Level-1 violation: @timeout without a matching @recover path");
    }

    // Additional structural checks (state locality, schema closedness, etc.)
    // are enforced by construction in the current specialized path and will
    // be expanded as the general IR lowering lands.

    Ok(())
}
