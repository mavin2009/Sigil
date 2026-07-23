//! Sigilc CLI — compile a .sigil file to an ownership-safe Rust crate.

use anyhow::{Context, Result};
use std::env;
use std::fs;
use std::path::PathBuf;

use sigilc::{
    emit, emit_cargo_toml, emit_demo_main, level_banner, lower, parse, relative_sigil_rt_path,
    residual_risk_report, run_checks, AssuranceLevel,
};

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!(
            "Usage: sigilc <file.sigil> [out_dir] [--emit-main] [--level 0|1|2]\n\
             \n\
             Assurance levels:\n\
             \x20 0 | sketch     exploratory; no safety checks, everything residual\n\
             \x20 1 | safe       default; extinct-by-design + signature checks\n\
             \x20 2 | contracts  spec obligations (require / hold / extinct)"
        );
        std::process::exit(1);
    }

    let mut input: Option<PathBuf> = None;
    let mut out = PathBuf::from("generated");
    let mut emit_main_flag = false;
    let mut level = AssuranceLevel::default();
    let mut args_iter = args.iter().skip(1).peekable();
    while let Some(arg) = args_iter.next() {
        if arg == "--emit-main" {
            emit_main_flag = true;
        } else if let Some(v) = arg.strip_prefix("--level=") {
            level = AssuranceLevel::from_arg(v)
                .with_context(|| format!("invalid assurance level '{v}' (expected 0, 1, or 2)"))?;
        } else if arg == "--level" {
            let v = args_iter
                .next()
                .context("--level requires a value (0, 1, or 2)")?;
            level = AssuranceLevel::from_arg(v)
                .with_context(|| format!("invalid assurance level '{v}' (expected 0, 1, or 2)"))?;
        } else if input.is_none() {
            input = Some(PathBuf::from(arg));
        } else if !arg.starts_with("--") {
            out = PathBuf::from(arg);
        }
    }
    let input = input.expect("input file");

    let source = fs::read_to_string(&input)
        .with_context(|| format!("failed to read {}", input.display()))?;

    println!("=== Sigilc ===");
    println!("Input: {}", input.display());

    let program = parse(&source).context("parsing")?;
    println!(
        "Parsed {} schema(s), {} process(es), {} transform(s), {} spec(s)",
        program.schemas.len(),
        program.processes.len(),
        program.transforms.len(),
        program.specs.len()
    );

    let graph = lower(&program).context("lowering to Graph IR")?;
    let outcome = run_checks(&program, &graph, level)
        .with_context(|| format!("checks at {}", level.name()))?;
    println!("Assurance: {} — checks passed.", level.name());
    for note in &outcome.notes {
        println!("[note] {note}");
    }
    if let Some(l2) = &outcome.level2 {
        if l2.path_timeout_sum_ms > 0 {
            println!("path_timeout_sum = {}ms", l2.path_timeout_sum_ms);
        }
    }

    fs::create_dir_all(out.join("src"))?;

    let rust = emit(&program, &graph);
    fs::write(out.join("src/lib.rs"), &rust)?;
    println!("[codegen] Wrote {}", out.join("src/lib.rs").display());

    if emit_main_flag {
        let main_rs = emit_demo_main(&program);
        fs::write(out.join("src/main.rs"), main_rs)?;
        println!("[codegen] Wrote {}", out.join("src/main.rs").display());
    }

    let pkg_name = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("sigil_out")
        .replace('-', "_");
    let rt_path = relative_sigil_rt_path(&out);
    fs::write(
        out.join("Cargo.toml"),
        emit_cargo_toml(&pkg_name, &rt_path, emit_main_flag),
    )?;
    println!(
        "[codegen] Wrote {} (sigil_rt path: {})",
        out.join("Cargo.toml").display(),
        rt_path
    );

    let base_risk = residual_risk_report(
        &program,
        &graph,
        outcome.level2.as_ref(),
        level >= AssuranceLevel::Safe,
    );
    let risk = format!("{}{}", level_banner(&outcome), base_risk);
    fs::write(out.join("RESIDUAL_RISK.md"), &risk)?;
    println!("[risk]    Wrote {}", out.join("RESIDUAL_RISK.md").display());

    println!("Generated crate is ready in {}", out.display());
    if emit_main_flag {
        println!("Demo binary target: cargo run -p {pkg_name} --bin demo");
    }
    Ok(())
}

#[cfg(test)]
mod integration {
    use sigilc::{
        check_failure_paths, check_transform_signatures, emit, emit_demo_main, level1_check,
        level2_check, lower, parse, residual_risk_report, GraphIR,
    };

    /// Full pipeline: parse → lower → L1 → signatures → L2 → emit → residual.
    fn compile_source(source: &str) -> (String, String, GraphIR) {
        let program = parse(source).expect("parse");
        let graph = lower(&program).expect("lower");
        level1_check(&graph).expect("level1");
        check_transform_signatures(&program).expect("signatures");
        check_failure_paths(&program).expect("failure paths");
        let l2 = level2_check(&program, &graph).expect("level2");
        let rust = emit(&program, &graph);
        let risk = residual_risk_report(&program, &graph, Some(&l2), true);
        (rust, risk, graph)
    }

    fn expect_l1_or_sig_reject(source: &str, needle: &str) {
        let program = parse(source).expect("parse should succeed");
        let graph = lower(&program).expect("lower");
        let ir_err = level1_check(&graph).err();
        let sig_err = check_transform_signatures(&program).err();
        let msg = format!(
            "{}{}",
            ir_err.map(|e| format!("{e}")).unwrap_or_default(),
            sig_err.map(|e| format!("{e}")).unwrap_or_default()
        );
        assert!(!msg.is_empty(), "expected Level-1 or signature rejection");
        assert!(
            msg.contains(needle) || msg.contains("Level-1"),
            "expected diagnostic containing '{needle}', got: {msg}"
        );
    }

    fn expect_l2_reject(source: &str, needle: &str) {
        let program = parse(source).expect("parse");
        let graph = lower(&program).expect("lower");
        level1_check(&graph).expect("level1 should pass for L2-only failures");
        let _ = check_transform_signatures(&program);
        let err = level2_check(&program, &graph).expect_err("level2 must fail");
        let msg = format!("{err}");
        assert!(msg.contains("Level-2"), "{msg}");
        assert!(
            msg.contains(needle) || msg.contains("path_timeout") || msg.contains("initial"),
            "expected '{needle}' in: {msg}"
        );
    }

    // ---------- Level-1 proofs (negative) ----------

    #[test]
    fn proof_rejects_unhandled_timeout() {
        let source = include_str!("../../examples/proofs/unhandled_timeout.sigil");
        expect_l1_or_sig_reject(source, "@timeout");
    }

    #[test]
    fn proof_rejects_type_mismatch() {
        let source = include_str!("../../examples/proofs/type_mismatch.sigil");
        expect_l1_or_sig_reject(source, "needs_receipt");
    }

    // ---------- Level-2 proofs (negative) ----------

    #[test]
    fn proof_rejects_hold_bad_init() {
        expect_l2_reject(
            include_str!("../../examples/proofs/hold_bad_init.sigil"),
            "initial",
        );
    }

    #[test]
    fn proof_rejects_timeout_without_step_recover() {
        expect_l2_reject(
            include_str!("../../examples/proofs/timeout_without_step_recover.sigil"),
            "@timeout",
        );
    }

    #[test]
    fn proof_rejects_timeout_sum_exceeded() {
        expect_l2_reject(
            include_str!("../../examples/proofs/timeout_sum_exceeded.sigil"),
            "path_timeout_sum",
        );
    }

    // ---------- Positive examples (full pipeline) ----------

    #[test]
    fn compile_ingest() {
        let (rust, risk, graph) = compile_source(include_str!("../../examples/ingest/ingest.sigil"));
        assert!(graph.has_timeout() && graph.has_recover());
        assert!(rust.contains("Ingest"));
        assert!(risk.contains("Level-1"));
    }

    #[test]
    fn compile_counter() {
        let (rust, risk, _) = compile_source(include_str!("../../examples/counter/counter.sigil"));
        assert!(rust.contains("fn add") || rust.contains("Counter"));
        assert!(risk.contains("Level-1") || risk.contains("hold") || risk.contains("Compiled"));
    }

    #[test]
    fn compile_resilient() {
        let (rust, risk, graph) =
            compile_source(include_str!("../../examples/resilient/resilient.sigil"));
        assert!(graph.has_timeout() && graph.has_recover());
        assert!(rust.contains("ResilientProcessor") || rust.contains("normalize"));
        assert!(risk.contains("enrich") || risk.contains("external") || risk.contains("Level"));
    }

    #[test]
    fn compile_circuit() {
        let (rust, risk, graph) =
            compile_source(include_str!("../../examples/circuit/circuit.sigil"));
        assert!(graph.has_timeout() && graph.has_recover());
        assert!(rust.contains("CircuitBreaker"));
        assert!(risk.contains("Level-1"));
    }

    #[test]
    fn compile_pipeline() {
        let (rust, risk, graph) =
            compile_source(include_str!("../../examples/pipeline/pipeline.sigil"));
        assert_eq!(graph.process_name, "OrderPipeline");
        assert!(rust.contains("from_millis(120)") && rust.contains("from_millis(200)"));
        assert!(risk.contains("Level-2") || risk.contains("path_timeout") || risk.contains("320") || risk.contains("discharged"));
        assert!(risk.contains("confirm") || risk.contains("Declared") || risk.contains("Order"));
    }

    #[test]
    fn compile_level2_example() {
        let (rust, risk, graph) =
            compile_source(include_str!("../../examples/level2/slo_and_hold.sigil"));
        assert!(graph.has_timeout() && graph.has_recover());
        assert!(rust.contains("Service") || rust.contains("on_event"));
        assert!(risk.contains("Level-2") || risk.contains("discharged") || risk.contains("hold"));
    }

    #[test]
    fn compile_runnable_counter_and_demo_main() {
        let source = include_str!("../../examples/runnable/counter/counter.sigil");
        let (rust, risk, graph) = compile_source(source);
        assert!(!graph.has_timeout());
        assert!(rust.contains("fn add"));
        assert!(risk.contains("body present") || risk.contains("Compiled") || risk.contains("hold"));
        let program = parse(source).unwrap();
        let main_rs = emit_demo_main(&program);
        assert!(main_rs.contains("Counter::new") && main_rs.contains("total"));
    }

    #[test]
    fn all_positive_examples_pass_full_pipeline() {
        let files = [
            include_str!("../../examples/ingest/ingest.sigil"),
            include_str!("../../examples/counter/counter.sigil"),
            include_str!("../../examples/resilient/resilient.sigil"),
            include_str!("../../examples/circuit/circuit.sigil"),
            include_str!("../../examples/pipeline/pipeline.sigil"),
            include_str!("../../examples/level2/slo_and_hold.sigil"),
            include_str!("../../examples/runnable/counter/counter.sigil"),
            include_str!("../../examples/concurrent/ledger/ledger.sigil"),
        ];
        for (i, src) in files.iter().enumerate() {
            let (rust, risk, _) = compile_source(src);
            assert!(rust.len() > 50, "example {i} empty codegen");
            assert!(risk.contains("Level-1"), "example {i} missing residual L1 section");
        }
    }

    /// Level-1 must reject external stages with no declared failure path.
    #[test]
    fn rejects_unrecovered_external_stage() {
        let src = include_str!("../../examples/proofs/unrecovered_external.sigil");
        let program = parse(src).expect("parse");
        let err = check_failure_paths(&program).expect_err("must reject");
        let msg = format!("{err}");
        assert!(msg.contains("no failure path") && msg.contains("fetch"), "got: {msg}");
    }

    /// @error acknowledges a drop; @recover without @timeout is now legal.
    #[test]
    fn error_ack_and_untimed_recover_pass() {
        let src = include_str!("../../examples/concurrent/ledger/ledger.sigil");
        let program = parse(src).expect("parse");
        check_failure_paths(&program).expect("fully covered pipeline must pass");
        let (rust, _, _) = compile_source(src);
        // Untimed @recover emits a match on the stage result with a recovery note.
        assert!(rust.contains("note_recovery(\"validate\")"), "untimed recover path missing");
        assert!(rust.contains("note_recovery(\"post\")"));
    }

    /// Every process must compile to a shared-nothing actor: state moves into
    /// an isolated task, reachable only through a Clone-able message handle.
    /// No lock or shared-ownership machinery may appear in generated code.
    #[test]
    fn emitted_process_is_a_lock_free_actor() {
        let source = include_str!("../../examples/concurrent/ledger/ledger.sigil");
        let (rust, _, _) = compile_source(source);
        assert!(rust.contains("pub struct LedgerHandle"), "missing actor handle");
        assert!(
            rust.contains("tokio::sync::mpsc::channel::<Payment>"),
            "actor must own a typed channel"
        );
        assert!(
            rust.contains("pub fn spawn(mut self"),
            "spawn must take state by move — isolation by construction"
        );
        assert!(!rust.contains("Mutex"), "generated code must not use locks");
        assert!(!rust.contains("Arc<"), "generated code must not share ownership");
        assert!(!rust.contains("unsafe"), "generated code must not use unsafe");
    }

    /// The demo driver must exercise real concurrency: a fleet of shards fed
    /// by many producer tasks on a multi-threaded runtime, with aggregate
    /// invariants printed for verification.
    #[test]
    fn demo_main_is_a_concurrent_stress_driver() {
        let source = include_str!("../../examples/concurrent/ledger/ledger.sigil");
        let program = parse(source).unwrap();
        let main_rs = emit_demo_main(&program);
        assert!(main_rs.contains("multi_thread"));
        assert!(main_rs.contains("SIGIL_DEMO_SHARDS"));
        assert!(main_rs.contains("SIGIL_DEMO_PRODUCERS"));
        assert!(main_rs.contains("agg_posted") && main_rs.contains("agg_total_amount"));
        assert!(!main_rs.contains("Mutex") && !main_rs.contains("Arc<"));
    }
}
