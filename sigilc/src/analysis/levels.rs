//! Stratified assurance levels.
//!
//! Level 0 — Sketch:    exploratory; parse + lower only, everything is residual.
//! Level 1 — Safe:      default; extinct-by-design checks + signature agreement.
//! Level 2 — Contracts: spec obligations (require / hold / extinct) on top of L1.
//!
//! Higher levels include everything below them. Contagion is explicit, not
//! automatic: skipping a level never fails the build silently — every skipped
//! guarantee is surfaced in the residual-risk report.

use crate::analysis::check::{
    check_failure_paths, check_handler_wellformedness, check_numeric_types,
    check_recover_signatures, check_transform_purity, check_transform_signatures,
    fallible_fallbacks, level1_check,
};
use crate::analysis::ir::GraphIR;
use crate::analysis::level2::{level2_check, Level2Report};
use crate::analysis::topology::derive_topology;
use crate::frontend::ast::Program;
use anyhow::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum AssuranceLevel {
    /// Level 0 — exploratory sketch. Parses and lowers; no safety checks run.
    Sketch = 0,
    /// Level 1 — safe default. Extinct-by-design checks + transform signatures.
    #[default]
    Safe = 1,
    /// Level 2 — contracts. Spec obligations checked on a Level-1-legal graph.
    Contracts = 2,
    /// Level 3 — proofs. hold invariants proven inductively; assumptions are
    /// runtime-guarded input preconditions. Undischargeable holds fail.
    Proofs = 3,
    /// Level 4 — system. Cross-process invariants proven structurally from
    /// the topology (ordering + multiplicity + reachability).
    System = 4,
}

impl AssuranceLevel {
    pub fn from_arg(s: &str) -> Option<Self> {
        match s.trim() {
            "0" | "sketch" => Some(Self::Sketch),
            "1" | "safe" => Some(Self::Safe),
            "2" | "contracts" => Some(Self::Contracts),
            "3" | "proofs" => Some(Self::Proofs),
            "4" | "system" => Some(Self::System),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Sketch => "Level 0 (sketch)",
            Self::Safe => "Level 1 (safe)",
            Self::Contracts => "Level 2 (contracts)",
            Self::Proofs => "Level 3 (proofs)",
            Self::System => "Level 4 (system)",
        }
    }
}

/// Result of running checks at a chosen assurance level.
#[derive(Debug)]
pub struct CheckOutcome {
    pub level: AssuranceLevel,
    /// Present only when Level-2 checks actually ran.
    pub level2: Option<Level2Report>,
    /// Present only when Level-3 proofs actually ran.
    pub level3: Option<crate::analysis::level3::Level3Report>,
    /// Present only when Level-4 system proofs actually ran.
    pub level4: Option<crate::analysis::level4::Level4Report>,
    /// Guarantees that were NOT established at this level.
    pub skipped: Vec<String>,
    /// Human-readable notes (e.g. specs parsed but unchecked).
    pub notes: Vec<String>,
}

/// Run all checks appropriate for `level`. Failing a check at or below the
/// chosen level fails the build; guarantees above the chosen level are
/// recorded as skipped so the residual report can surface them.
pub fn run_checks(
    program: &Program,
    irs: &[GraphIR],
    level: AssuranceLevel,
) -> Result<CheckOutcome> {
    let mut skipped = Vec::new();
    let mut notes = Vec::new();
    let mut level2 = None;
    let mut level3 = None;
    let mut level4 = None;

    if level >= AssuranceLevel::Safe {
        for ir in irs {
            level1_check(ir)?;
        }
        check_handler_wellformedness(program)?;
        check_numeric_types(program)?;
        check_recover_signatures(program)?;
        check_transform_signatures(program)?;
        check_failure_paths(program)?;
        check_transform_purity(program)?;
        derive_topology(program)?;
    } else {
        skipped.push("shared-mutability / local-state discipline (Level-1 not run)".into());
        skipped.push("@timeout ↔ @recover pairing (Level-1 not run)".into());
        skipped.push("pipeline ↔ transform signature agreement (not checked)".into());
        skipped.push("handler well-formedness (unique message names/types — not checked)".into());
        skipped.push("failure-path coverage of external stages (not checked)".into());
        skipped.push("process topology (targets, types, acyclicity — not checked)".into());
        notes.push(
            "SKETCH MODE: no safety guarantees are established. \
             Do not deploy artifacts built at Level 0."
                .into(),
        );
    }

    if level >= AssuranceLevel::Contracts {
        level2 = Some(level2_check(program, irs)?);
    } else if !program.specs.is_empty() {
        skipped.push("spec obligations (require / hold / extinct) — not checked".into());
        notes.push(format!(
            "{} spec(s) parsed but NOT checked at {} — rerun with --level 2",
            program.specs.len(),
            level.name()
        ));
    }

    if level >= AssuranceLevel::Proofs {
        // Proofs assume recovery paths cannot fail; enforce that here.
        let ff = fallible_fallbacks(program)?;
        if !ff.is_empty() {
            anyhow::bail!(
                "Level-3 requires infallible recovery: {} used as a @recover target but \
                 declared external (empty body). A fallback that can fail or hang \
                 reintroduces the loss it exists to prevent — give it a pure body.",
                ff.iter()
                    .map(|f| format!("`{f}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        let l3 = crate::analysis::level3::level3_prove(program)?;
        for p in &l3.proven {
            notes.push(format!("PROVEN: {p}"));
        }
        level3 = Some(l3);
    }
    if level >= AssuranceLevel::System {
        let l4 = crate::analysis::level4::level4_prove(program)?;
        for p in &l4.proven {
            notes.push(format!("PROVEN (system): {p}"));
        }
        level4 = Some(l4);
    }
    if level < AssuranceLevel::Proofs {
        // Below Level 3 a fallible recovery path is reported, not rejected.
        let ff = fallible_fallbacks(program).unwrap_or_default();
        if !ff.is_empty() {
            notes.push(format!(
                "fallible recovery paths (external @recover targets): {} — these can fail \
                 or hang; Level 3 rejects them",
                ff.iter()
                    .map(|f| format!("`{f}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            skipped
                .push("infallible-recovery guarantee (some @recover targets are external)".into());
        }
        let has_holds = program.specs.iter().any(|sp| {
            sp.items
                .iter()
                .any(|i| matches!(i, crate::frontend::ast::SpecItem::Hold { .. }))
        });
        if has_holds {
            skipped.push(
                "inductive hold proofs (Level 3 not run — holds are heuristic/residual at Level 2)"
                    .into(),
            );
        }
    }

    Ok(CheckOutcome {
        level,
        level2,
        level3,
        level4,
        skipped,
        notes,
    })
}

/// Markdown section describing the assurance level of this build, for
/// inclusion at the top of RESIDUAL_RISK.md.
pub fn level_banner(outcome: &CheckOutcome) -> String {
    let mut out = String::new();
    out.push_str(&format!("## Assurance Level: {}\n\n", outcome.level.name()));
    if outcome.skipped.is_empty() {
        out.push_str("All guarantees available at this level were established.\n\n");
    } else {
        out.push_str("**Guarantees NOT established by this build:**\n\n");
        for s in &outcome.skipped {
            out.push_str(&format!("- {s}\n"));
        }
        out.push('\n');
    }
    if let Some(l4) = &outcome.level4 {
        if !l4.proven.is_empty() {
            out.push_str(
                "**Proven SYSTEM invariants (Level 4, structural over the topology):**\n\n",
            );
            for p in &l4.proven {
                out.push_str(&format!("- {p}\n"));
            }
            out.push('\n');
        }
    }
    if let Some(l3) = &outcome.level3 {
        if !l3.proven.is_empty() {
            out.push_str("**Proven invariants (Level 3, inductive):**\n\n");
            for p in &l3.proven {
                out.push_str(&format!("- {p}\n"));
            }
            out.push('\n');
        }
        if !l3.guarded_assumptions.is_empty() {
            out.push_str("**Proof assumptions — every one runtime-enforced:**\n\n");
            for a in &l3.guarded_assumptions {
                out.push_str(&format!("- {a}\n"));
            }
            out.push('\n');
        }
        for r in &l3.residual {
            out.push_str(&format!("> ⚠ {r}\n\n"));
        }
    }
    for n in &outcome.notes {
        if !n.starts_with("PROVEN:") {
            out.push_str(&format!("> ⚠ {n}\n\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::ir::lower;
    use crate::frontend::ast::parse;

    /// A program that must fail Level-1 (unhandled @timeout)…
    const L1_VIOLATION: &str = r#"
schema M { v: Int }
transform slow(m: M) -> M {}
process P {
  state last: Int = 0
  on m: M {
    let out = m ~> slow @timeout(50.ms)
    last := out.v
  }
}
"#;

    #[test]
    fn sketch_mode_accepts_l1_violations_but_reports_them() {
        let program = parse(L1_VIOLATION).expect("parse");
        let irs = lower(&program).expect("lower");
        let outcome =
            run_checks(&program, &irs, AssuranceLevel::Sketch).expect("sketch must not reject");
        assert!(outcome.level2.is_none());
        assert!(
            outcome.skipped.iter().any(|s| s.contains("@timeout")),
            "sketch build must surface skipped timeout pairing"
        );
        let banner = level_banner(&outcome);
        assert!(banner.contains("Level 0"));
        assert!(banner.contains("NOT established"));
    }

    #[test]
    fn safe_level_still_rejects_l1_violations() {
        let program = parse(L1_VIOLATION).expect("parse");
        let irs = lower(&program).expect("lower");
        let err = run_checks(&program, &irs, AssuranceLevel::Safe)
            .expect_err("level 1 must reject unhandled timeout");
        assert!(format!("{err}").contains("Level-1"));
    }

    #[test]
    fn specs_are_skipped_below_contracts_level_with_note() {
        let src = r#"
schema M { v: Int }
process P {
  state total: Int = 0
  on m: M {
    total := total + 1
  }
}
spec S {
  hold total >= 0
}
"#;
        let program = parse(src).expect("parse");
        let irs = lower(&program).expect("lower");
        let outcome = run_checks(&program, &irs, AssuranceLevel::Safe).expect("l1 ok");
        assert!(outcome.level2.is_none());
        assert!(outcome
            .notes
            .iter()
            .any(|n| n.contains("NOT checked") && n.contains("--level 2")));

        let outcome2 = run_checks(&program, &irs, AssuranceLevel::Contracts).expect("l2 ok");
        assert!(outcome2.level2.is_some());
        // At Level 2, the only remaining skip is the Level-3 proof of the hold.
        assert!(outcome2.skipped.iter().all(|s| s.contains("Level 3")));

        let outcome3 = run_checks(&program, &irs, AssuranceLevel::Proofs)
            .expect("hold total >= 0 is provable");
        assert!(outcome3.skipped.is_empty());
        assert!(outcome3.level3.is_some());
    }

    #[test]
    fn level_arg_parsing() {
        assert_eq!(AssuranceLevel::from_arg("0"), Some(AssuranceLevel::Sketch));
        assert_eq!(
            AssuranceLevel::from_arg("sketch"),
            Some(AssuranceLevel::Sketch)
        );
        assert_eq!(AssuranceLevel::from_arg("1"), Some(AssuranceLevel::Safe));
        assert_eq!(
            AssuranceLevel::from_arg("contracts"),
            Some(AssuranceLevel::Contracts)
        );
        assert_eq!(AssuranceLevel::from_arg("9"), None);
    }
}
