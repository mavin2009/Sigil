//! Sigilc CLI — compile a .sigil file to an ownership-safe Rust crate.

use anyhow::{Context, Result};
use std::env;
use std::fs;
use std::path::PathBuf;

use sigilc::{
    check_transform_signatures, emit, emit_cargo_toml, emit_demo_main, level1_check, level2_check, lower,
    parse, relative_sigil_rt_path, residual_risk_report,
};

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: sigilc <file.sigil> [out_dir] [--emit-main]");
        std::process::exit(1);
    }

    let mut input: Option<PathBuf> = None;
    let mut out = PathBuf::from("generated");
    let mut emit_main_flag = false;
    for arg in args.iter().skip(1) {
        if arg == "--emit-main" {
            emit_main_flag = true;
        } else if input.is_none() {
            input = Some(PathBuf::from(arg));
        } else {
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
        "Parsed {} schema(s), {} process(es), {} transform(s)",
        program.schemas.len(),
        program.processes.len(),
        program.transforms.len()
    );

    let graph = lower(&program).context("lowering to Graph IR")?;
    level1_check(&graph).context("Level-1 checks")?;
    check_transform_signatures(&program).context("transform signature checks")?;
    let l2 = level2_check(&program, &graph).context("Level-2 checks")?;
    println!("Level-1 and Level-2 checks passed.");
    if l2.path_timeout_sum_ms > 0 {
        println!("path_timeout_sum = {}ms", l2.path_timeout_sum_ms);
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

    let risk = residual_risk_report(&program, &graph, Some(&l2));
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
        check_transform_signatures, emit, emit_demo_main, level1_check, level2_check, lower, parse,
        residual_risk_report, GraphIR,
    };

    fn compile_source(source: &str) -> (String, String, GraphIR) {
        let program = parse(source).expect("parse");
        let graph = lower(&program).expect("lower");
        level1_check(&graph).expect("level1");
        check_transform_signatures(&program).expect("signatures");
        let rust = emit(&program, &graph);
        let risk = residual_risk_report(&program, &graph, Some(&l2));
        (rust, risk, graph)
    }

    fn expect_reject(source: &str, needle: &str) {
        let program = parse(source).expect("parse should succeed");
        let graph = lower(&program).expect("lower");
        let ir_err = level1_check(&graph).err();
        let sig_err = check_transform_signatures(&program).err();
        let msg = format!(
            "{}{}",
            ir_err
                .map(|e| format!("{e}"))
                .unwrap_or_default(),
            sig_err
                .map(|e| format!("{e}"))
                .unwrap_or_default()
        );
        assert!(
            !msg.is_empty(),
            "expected Level-1 or signature rejection"
        );
        assert!(
            msg.contains(needle) || msg.contains("Level-1"),
            "expected diagnostic containing '{needle}', got: {msg}"
        );
    }

    #[test]
    fn proof_rejects_unhandled_timeout() {
        let source = include_str!("../../examples/proofs/unhandled_timeout.sigil");
        expect_reject(source, "@timeout");
    }

    #[test]
    fn proof_rejects_hold_bad_init() {
        let source = include_str!("../../examples/proofs/hold_bad_init.sigil");
        let program = parse(source).expect("parse");
        let graph = lower(&program).expect("lower");
        level1_check(&graph).expect("level1");
        let err = level2_check(&program, &graph).expect_err("level2 must fail");
        let msg = format!("{err}");
        assert!(msg.contains("Level-2"), "{msg}");
    }

    #[test]
    fn proof_rejects_timeout_sum_exceeded() {
        let source = include_str!("../../examples/proofs/timeout_sum_exceeded.sigil");
        let program = parse(source).expect("parse");
        let graph = lower(&program).expect("lower");
        level1_check(&graph).expect("level1 should pass");
        check_transform_signatures(&program).expect("signatures should pass");
        let err = level2_check(&program, &graph).expect_err("level2 must fail");
        let msg = format!("{err}");
        assert!(msg.contains("Level-2"), "{msg}");
        assert!(msg.contains("path_timeout_sum") || msg.contains("300"), "{msg}");
    }

    #[test]
    fn proof_rejects_type_mismatch() {
        let source = include_str!("../../examples/proofs/type_mismatch.sigil");
        expect_reject(source, "needs_receipt");
    }

    #[test]
    fn compile_ingest_example() {
        let source = include_str!("../../examples/ingest/ingest.sigil");
        let (rust, risk, graph) = compile_source(source);
        assert!(risk.contains("Level-1"));
        assert!(graph.has_timeout());
        assert!(graph.has_recover());
        assert!(rust.contains("pub struct Ingest"));
        assert!(rust.contains("on_packet"));
    }

    #[test]
    fn compile_resilient_example() {
        let source = include_str!("../../examples/resilient/resilient.sigil");
        let (rust, risk, graph) = compile_source(source);
        assert!(graph.has_timeout());
        assert!(graph.has_recover());
        assert!(rust.contains("ResilientProcessor"));
        assert!(rust.contains("fn normalize"));
        assert!(risk.contains("enrich") && risk.contains("external residual"));
    }

    #[test]
    fn compile_circuit_example() {
        let source = include_str!("../../examples/circuit/circuit.sigil");
        let (rust, risk, graph) = compile_source(source);
        assert!(graph.has_timeout());
        assert!(graph.has_recover());
        assert!(rust.contains("CircuitBreaker"));
        assert!(risk.contains("Level-1"));
    }

    #[test]
    fn compile_pipeline_example() {
        let source = include_str!("../../examples/pipeline/pipeline.sigil");
        let (rust, risk, graph) = compile_source(source);
        assert_eq!(graph.process_name, "OrderPipeline");
        assert!(graph.has_timeout());
        assert!(graph.has_recover());
        assert!(rust.contains("from_millis(120)"));
        assert!(rust.contains("from_millis(200)"));
        assert!(rust.contains("fn authorize") || rust.contains("authorize"));
        assert!(risk.contains("Declared Transforms") || risk.contains("confirm"));
    }

    #[test]
    fn compile_counter_and_demo_main() {
        let source = include_str!("../../examples/runnable/counter/counter.sigil");
        let (rust, risk, graph) = compile_source(source);
        assert!(rust.contains("fn add"));
        assert!(rust.contains("x + 1") || rust.contains("(x + 1)"));
        assert!(risk.contains("body present") || risk.contains("Compiled"));
        let program = parse(source).unwrap();
        let main_rs = emit_demo_main(&program);
        assert!(main_rs.contains("Counter::new"));
        assert!(main_rs.contains("println!"));
        assert!(main_rs.contains("total"));
        assert!(!graph.has_timeout());
    }

    #[test]
    fn compile_legacy_counter_example() {
        let source = include_str!("../../examples/counter/counter.sigil");
        let (rust, risk, _) = compile_source(source);
        assert!(rust.contains("Counter") || rust.contains("total"));
        assert!(risk.contains("Level-1"));
    }
}
