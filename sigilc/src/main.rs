
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
