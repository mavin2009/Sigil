//! Sigilc CLI — compile a .sigil file to an ownership-safe Rust crate.

use anyhow::{Context, Result};
use std::env;
use std::fs;
use std::path::PathBuf;

use sigilc::{emit, emit_cargo_toml, level1_check, lower, parse, relative_sigil_rt_path, residual_risk_report};

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

    fs::create_dir_all(out.join("src"))?;

    let rust = emit(&program, &graph);
    let rust_path = out.join("src/lib.rs");
    fs::write(&rust_path, &rust)?;
    println!("[codegen] Wrote {}", rust_path.display());

    let pkg_name = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("sigil_out")
        .replace('-', "_");
    let rt_path = relative_sigil_rt_path(&out);
    fs::write(out.join("Cargo.toml"), emit_cargo_toml(&pkg_name, &rt_path))?;
    println!("[codegen] Wrote {} (sigil_rt path: {})", out.join("Cargo.toml").display(), rt_path);

    let risk = residual_risk_report(&graph);
    let risk_path = out.join("RESIDUAL_RISK.md");
    fs::write(&risk_path, &risk)?;
    println!("[risk]    Wrote {}", risk_path.display());

    println!("Generated crate is ready in {}", out.display());
    Ok(())
}

#[cfg(test)]
mod integration {
    use sigilc::{emit, level1_check, lower, parse, residual_risk_report, GraphIR};

    fn compile_source(source: &str) -> (String, String, GraphIR) {
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
        assert!(graph.has_timeout());
        assert!(graph.has_recover());
        assert!(rust.contains("pub struct Ingest"));
        assert!(rust.contains("on_packet"));
        assert!(rust.contains("from_millis(50)") || rust.contains("50"));
    }

    #[test]
    fn compile_resilient_example() {
        let source = include_str!("../../examples/resilient.sigil");
        let (rust, risk, graph) = compile_source(source);
        assert!(graph.has_timeout());
        assert!(graph.has_recover());
        assert!(risk.contains("Level-1"));
        assert!(rust.contains("ResilientProcessor"));
        assert!(rust.contains("on_event"));
        assert!(rust.contains("from_millis(80)") || rust.contains("80"));
    }

    #[test]
    fn compile_circuit_example() {
        let source = include_str!("../../examples/circuit.sigil");
        let (rust, risk, graph) = compile_source(source);
        assert!(graph.has_timeout());
        assert!(graph.has_recover());
        assert!(risk.contains("Level-1"));
        assert!(rust.contains("CircuitBreaker"));
    }

    #[test]
    fn compile_pipeline_example() {
        let source = include_str!("../../examples/pipeline.sigil");
        let (rust, risk, graph) = compile_source(source);
        assert_eq!(graph.process_name, "OrderPipeline");
        assert!(graph.has_timeout());
        assert!(graph.has_recover());
        assert!(risk.contains("Level-1"));
        assert!(rust.contains("pub struct OrderPipeline"));
        assert!(rust.contains("on_order"));
        // Dual timed stages
        assert!(rust.contains("from_millis(120)"));
        assert!(rust.contains("from_millis(200)"));
        assert!(rust.contains("reserve") && rust.contains("charge"));
        // Schema-typed external stubs with propagation
        assert!(rust.contains("fn authorize(input: Order)"));
        assert!(
            rust.contains("fn confirm(_input: Order) -> Result<Receipt>")
                || rust.contains("fn confirm(input: Order) -> Result<Receipt>")
                || rust.contains("Result<Receipt>"),
            "confirm should propagate toward Receipt; got snippet missing"
        );
        assert!(rust.contains("pub total_charged"));
        assert!(rust.contains("Process: OrderPipeline"));
        // Generated crate depends on sigil_rt
        assert!(rust.contains("use sigil_rt::Result"));
    }

    #[test]
    fn emit_cargo_toml_points_at_sigil_rt() {
        let toml = sigilc::emit_cargo_toml("demo", "../../sigil_rt");
        assert!(toml.contains("sigil_rt"));
        assert!(toml.contains("path = \"../../sigil_rt\""));
        assert!(toml.contains("tokio"));
    }

    #[test]
    fn compile_counter_example() {
        let source = include_str!("../../examples/counter.sigil");
        let (rust, risk, graph) = compile_source(source);
        assert!(!graph.has_timeout() || graph.has_recover());
        assert!(risk.contains("Level-1"));
        assert!(rust.contains("Counter") || rust.contains("total"));
    }
}
