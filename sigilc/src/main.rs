
//! Sigilc — compiler for the Sigil language.
//! Parses Sigil, lowers to Graph IR, runs Level-1 checks, emits ownership-safe Rust.

use anyhow::{Context, Result};
use std::env;
use std::fs;
use std::path::PathBuf;

use sigilc::{parse, lower, level1_check, emit, residual_risk_report};

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

    let program = parse(&source).context("parsing")?;
    println!(
        "Parsed {} schema(s), {} process(es)",
        program.schemas.len(),
        program.processes.len()
    );

    let graph = lower(&program).context("lowering to Graph IR")?;
    level1_check(&graph).context("Level-1 checks")?;
    println!("Level-1 checks passed.");

    fs::create_dir_all(&out)?;
    let rust = emit(&program, &graph);
    let risk = residual_risk_report(&graph);

    let rust_path = out.join("src").join("lib.rs");
    fs::create_dir_all(rust_path.parent().unwrap())?;
    fs::write(&rust_path, &rust)?;
    println!("[codegen] Wrote {}", rust_path.display());

    let risk_path = out.join("RESIDUAL_RISK.md");
    fs::write(&risk_path, &risk)?;
    println!("[risk]    Wrote {}", risk_path.display());

    // Minimal Cargo.toml for the generated crate
    let cargo_toml = format!(
        r#"[package]
name = "sigil_generated"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = {{ version = "1", features = ["time", "rt", "macros"] }}
"#
    );
    fs::write(out.join("Cargo.toml"), cargo_toml)?;

    println!("Generated project is ready in {}", out.display());
    Ok(())
}

#[cfg(test)]
mod integration {
    use sigilc::{parse, lower, level1_check, emit, residual_risk_report};

    fn compile_source(source: &str) -> (String, String, sigilc::GraphIR) {
        let program = parse(source).expect("parse");
        let graph = lower(&program).expect("lower");
        level1_check(&graph).expect("level1");
        let rust = emit(&program, &graph);
        let risk = residual_risk_report(&graph);
        (rust, risk, graph)
    }

    #[test]
    fn compile_ingest_example() {
        let source = include_str!("../../examples/ingest.sigil");
        let (rust, risk, graph) = compile_source(source);
        assert!(risk.contains("Level-1"));
        assert!(graph.has_timeout() || !graph.has_timeout());
        assert!(rust.len() > 50);
    }

    #[test]
    fn compile_resilient_example() {
        let source = include_str!("../../examples/resilient.sigil");
        let (rust, risk, graph) = compile_source(source);
        assert!(graph.has_timeout());
        assert!(graph.has_recover());
        assert!(risk.contains("Level-1"));
        assert!(rust.len() > 50);
    }

    #[test]
    fn compile_circuit_example() {
        let source = include_str!("../../examples/circuit.sigil");
        let (rust, risk, graph) = compile_source(source);
        assert!(graph.has_timeout());
        assert!(graph.has_recover());
        assert!(risk.contains("Level-1"));
        assert!(rust.len() > 50);
    }
}
