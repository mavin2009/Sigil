//! Sigilc — compiler for the Sigil language.
//! Parses Sigil, lowers to Graph IR, runs Level-1 checks, emits ownership-safe Rust.

use anyhow::{Context, Result};
use std::env;
use std::fs;
use std::path::PathBuf;

mod ast;
mod check;
mod codegen;
mod ir;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: sigilc <file.sigil> [out_dir]");
        std::process::exit(1);
    }
    let input = PathBuf::from(&args[1]);
    let out = if args.len() > 2 {
        PathBuf::from(&args[2])
    } else {
        PathBuf::from("generated")
    };

    let source = fs::read_to_string(&input)
        .with_context(|| format!("failed to read {}", input.display()))?;

    println!("=== Sigilc ===");
    println!("Input: {}", input.display());

    let program = ast::parse(&source).context("parsing")?;
    println!(
        "Parsed {} schema(s), {} process(es)",
        program.schemas.len(),
        program.processes.len()
    );

    let graph = ir::lower(&program).context("lowering to Graph IR")?;
    check::level1_check(&graph).context("Level-1 checks")?;

    fs::create_dir_all(out.join("src"))?;

    let rust_code = codegen::emit(&program, &graph);
    let lib_path = out.join("src/lib.rs");
    fs::write(&lib_path, &rust_code)?;
    println!("[codegen] Wrote {}", lib_path.display());

    let pkg_name = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("sigil_out")
        .replace('-', "_");
    let cargo = codegen::emit_cargo_toml(&pkg_name);
    let cargo_path = out.join("Cargo.toml");
    fs::write(&cargo_path, cargo)?;
    println!("[codegen] Wrote {}", cargo_path.display());

    let risk = codegen::residual_risk_report(&graph);
    let risk_path = out.join("RESIDUAL_RISK.md");
    fs::write(&risk_path, risk)?;
    println!("[risk]    Wrote {}", risk_path.display());

    println!();
    println!("Level-1 checks passed.");
    println!("Generated crate is ready in {}", out.display());
    Ok(())
}

#[cfg(test)]
mod integration {
    use crate::ast;
    use crate::check;
    use crate::codegen;
    use crate::ir;

    fn compile_source(source: &str) -> (String, String, ir::GraphIR) {
        let program = ast::parse(source).expect("parse");
        let graph = ir::lower(&program).expect("lower");
        check::level1_check(&graph).expect("level1");
        let rust = codegen::emit(&program, &graph);
        let risk = codegen::residual_risk_report(&graph);
        (rust, risk, graph)
    }

    #[test]
    fn compile_ingest_example() {
        let source = include_str!("../../examples/ingest.sigil");
        let (rust, risk, graph) = compile_source(source);
        assert!(rust.contains("struct Ingest") || rust.contains("pub struct Ingest"));
        assert!(rust.contains("on_packet") || rust.contains("timeout"));
        assert!(risk.contains("Level-1"));
        assert!(graph.has_timeout());
        assert!(graph.has_recover());
        assert!(!graph.local_states.is_empty());
    }

    #[test]
    fn compile_resilient_example() {
        let source = include_str!("../../examples/resilient.sigil");
        let (rust, risk, graph) = compile_source(source);
        assert!(graph.has_timeout());
        assert!(graph.has_recover());
        assert!(risk.contains("Level-1"));
        assert!(graph.local_states.iter().any(|s| s == "last_ok"));
        assert!(rust.contains("ResilientProcessor") || rust.contains("on_event"));
        assert!(rust.contains("timeout") || rust.contains("Duration"));
    }

    #[test]
    fn compile_counter_example() {
        let source = include_str!("../../examples/counter.sigil");
        let (rust, risk, graph) = compile_source(source);
        assert!(!graph.has_timeout() || graph.has_recover());
        assert!(risk.contains("Level-1"));
        assert!(rust.contains("Counter") || rust.contains("total") || rust.len() > 50);
    }

    #[test]
    fn compile_circuit_example() {
        let source = include_str!("../../examples/circuit.sigil");
        let (rust, risk, graph) = compile_source(source);
        assert_eq!(graph.process_name, "CircuitBreaker");
        assert!(graph.has_timeout());
        assert!(graph.has_recover());
        assert!(risk.contains("Level-1"));
        assert!(
            graph
                .local_states
                .iter()
                .any(|s| s == "failures" || s == "last_status" || s == "open")
        );
        assert!(rust.contains("CircuitBreaker") || rust.contains("on_req"));
    }
}
