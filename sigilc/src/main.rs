//! Sigilc v0.1.0 — Compiler for the Sigil language
//! Level-1 extinct-by-design properties mapped to safe Rust.

use anyhow::{Context, Result};
use clap::Parser;
use std::fs;
use std::path::PathBuf;

mod ast;
mod check;
mod codegen;
mod ir;

#[derive(Parser, Debug)]
#[command(name = "sigilc", about = "Sigil compiler — fault-tolerant systems by construction")]
struct Args {
    /// Input .sigil file
    input: PathBuf,

    /// Output directory for generated Rust + residual risk report
    #[arg(short, long, default_value = "generated")]
    out: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let source = fs::read_to_string(&args.input)
        .with_context(|| format!("failed to read {}", args.input.display()))?;

    println!("=== Sigilc v0.1.0 ===");
    println!("Input: {}", args.input.display());

    // v0.1 uses a specialized path for the primary example while the
    // general pest pair-walker is completed in subsequent iterations.
    let program = ast::parse_example(&source)
        .context("parsing")?;

    let graph = ir::lower(&program)
        .context("lowering to Graph IR")?;

    check::level1_check(&graph)
        .context("Level-1 extinct-by-design checks")?;

    fs::create_dir_all(&args.out)?;

    let rust_code = codegen::emit(&graph);
    let rust_path = args.out.join("main.rs");
    fs::write(&rust_path, &rust_code)?;
    println!("[codegen] Wrote {}", rust_path.display());

    let risk = codegen::residual_risk_report();
    let risk_path = args.out.join("RESIDUAL_RISK.md");
    fs::write(&risk_path, risk)?;
    println!("[risk]    Wrote {}", risk_path.display());

    // Minimal Cargo.toml so the output is immediately buildable
    let cargo_toml = r#"[package]
name = "sigil_generated"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1", features = ["full"] }
sigil_rt = { path = "../sigil_rt" }
"#;
    fs::write(args.out.join("Cargo.toml"), cargo_toml)?;

    println!();
    println!("Level-1 checks passed.");
    println!("Generated project is ready in {}", args.out.display());
    Ok(())
}
