
//! Sigilc — Compiler for the Sigil language
//! Level-1 extinct-by-design properties mapped to safe Rust.

use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;
use std::env;

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

    let program = ast::parse(&source)
        .context("parsing")?;

    println!("Parsed {} schema(s), {} process(es)", program.schemas.len(), program.processes.len());

    let graph = ir::lower(&program)
        .context("lowering to Graph IR")?;

    check::level1_check(&graph)
        .context("Level-1 extinct-by-design checks")?;

    fs::create_dir_all(&out)?;

    let rust_code = codegen::emit(&graph);
    let rust_path = out.join("main.rs");
    fs::write(&rust_path, &rust_code)?;
    println!("[codegen] Wrote {}", rust_path.display());

    let risk = codegen::residual_risk_report(&graph);
    let risk_path = out.join("RESIDUAL_RISK.md");
    fs::write(&risk_path, risk)?;
    println!("[risk]    Wrote {}", risk_path.display());

    println!();
    println!("Level-1 checks passed.");
    println!("Generated project is ready in {}", out.display());
    Ok(())
}








#[cfg(test)]
mod integration {
    use crate::ast;
    use crate::ir;
    use crate::check;
    use crate::codegen;

    fn compile_source(source: &str) -> (String, String, ir::GraphIR) {
        let program = ast::parse(source).expect("parse");
        let graph = ir::lower(&program).expect("lower");
        check::level1_check(&graph).expect("level1");
        let rust = codegen::emit(&graph);
        let risk = codegen::residual_risk_report(&graph);
        (rust, risk, graph)
    }

    #[test]
    fn compile_ingest_example() {
        let source = include_str!("../../examples/ingest.sigil");
        let (rust, risk, graph) = compile_source(source);

        assert!(rust.contains("Ingest") || rust.contains("on_packet"));
        assert!(risk.contains("Level-1"));
        assert!(risk.contains("Timeout") || risk.contains("timeout") || risk.contains("50ms"));
        assert!(graph.has_timeout());
        assert!(graph.has_recover());
        assert!(!graph.local_states.is_empty());
    }

    #[test]
    
    #[test]
    
    #[test]
    fn compile_circuit_example() {
        let source = include_str!("../../examples/circuit.sigil");
        let (rust, risk, graph) = compile_source(source);
        assert!(graph.has_timeout(), "circuit example must have a timeout");
        assert!(graph.has_recover(), "circuit example must recover the timeout");
        assert!(risk.contains("Level-1"));
        assert!(graph.local_states.iter().any(|s| s == "last_status" || s == "failures"));
        assert!(rust.len() > 50);
    }

    fn compile_resilient_example() {
        let source = include_str!("../../examples/resilient.sigil");
        let (rust, risk, graph) = compile_source(source);
        assert!(graph.has_timeout(), "resilient example must have a timeout");
        assert!(graph.has_recover(), "resilient example must recover the timeout");
        assert!(risk.contains("Level-1"));
        assert!(graph.local_states.iter().any(|s| s == "last_ok"));
        assert!(rust.len() > 50);
    }

    fn compile_counter_example() {
        let source = include_str!("../../examples/counter.sigil");
        let (rust, risk, graph) = compile_source(source);

        // Counter has no timeouts — still must pass Level-1
        assert!(!graph.has_timeout() || graph.has_recover());
        assert!(risk.contains("Level-1"));
        assert!(!graph.local_states.is_empty() || graph.process_name == "Counter");
        // Generated code should mention the process or state
        assert!(rust.contains("Ingest") || rust.contains("Counter") || rust.contains("last") || rust.contains("total") || rust.len() > 100);
    }
}
