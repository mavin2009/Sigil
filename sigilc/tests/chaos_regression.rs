//! End-to-end regression: compile an example, build the generated crate, and
//! run it under fault injection with its proven invariants asserted.
//!
//! The unit suite checks that the compiler reaches the right verdicts. This
//! checks that a program it blessed actually behaves as promised when
//! external stages fail and stall — which is the claim that matters, and the
//! one that catches prover unsoundness rather than checker bugs.
//!
//! These invoke Cargo and are intentionally part of the default production
//! gate. Each case has its own output directory so parallel test execution
//! cannot race while generating or building a crate.

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn sigilc_bin() -> PathBuf {
    // The integration test binary lives in target/<profile>/deps.
    let mut p = std::env::current_exe().expect("test exe");
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.join("sigilc")
}

/// Compile a `.sigil` source to a crate, build it, and run its demo under the
/// given environment. Returns the demo's combined output.
fn compile_build_run(case: &str, example: &str, level: &str, env: &[(&str, &str)]) -> String {
    let root = repo_root();
    let out = root.join("target/chaos-regression").join(case);

    let status = Command::new(sigilc_bin())
        .arg(root.join(example))
        .arg(&out)
        .arg("--emit-main")
        .arg("--level")
        .arg(level)
        .status()
        .expect("run sigilc");
    assert!(status.success(), "{example} must compile at level {level}");

    let build = Command::new("cargo")
        .arg("build")
        .arg("--quiet")
        .current_dir(&out)
        .output()
        .expect("cargo build");
    assert!(
        build.status.success(),
        "generated crate for {example} must compile:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let mut run = Command::new("cargo");
    run.arg("run")
        .arg("--quiet")
        .arg("--bin")
        .arg("demo")
        .current_dir(&out);
    for (k, v) in env {
        run.env(k, v);
    }
    let res = run.output().expect("cargo run");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&res.stdout),
        String::from_utf8_lossy(&res.stderr)
    );
    assert!(
        res.status.success(),
        "demo for {example} must exit cleanly — a failure here is an invariant \
         violation or a panic, not a flake:\n{combined}"
    );
    combined
}

fn counter(out: &str, key: &str) -> u64 {
    out.split(key)
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
}

/// The flagship component under sustained faults and latency spikes, with
/// every proven invariant asserted at runtime.
#[test]
fn clearinghouse_holds_under_chaos() {
    let out = compile_build_run(
        "clearinghouse-faults",
        "examples/clearinghouse/clearing.sigil",
        "4",
        &[
            ("SIGIL_DEMO_SHARDS", "4"),
            ("SIGIL_DEMO_PRODUCERS", "12"),
            ("SIGIL_DEMO_MSGS", "40"),
            ("SIGIL_CHAOS_FAIL_PCT", "20"),
            ("SIGIL_CHAOS_LATENCY_MS", "60"),
        ],
    );
    assert!(
        !out.contains("PROVEN INVARIANT VIOLATED"),
        "a proven invariant was violated at runtime:\n{out}"
    );
    assert!(
        out.contains("proven invariant(s) verified at runtime"),
        "{out}"
    );
    // The resilience machinery must actually have been exercised, or this run
    // says nothing about behaviour under failure.
    assert!(
        counter(&out, "injected faults=") > 0,
        "chaos injected no faults:\n{out}"
    );
    assert!(
        counter(&out, "recover paths taken=") > 0,
        "no recovery path was taken:\n{out}"
    );
}

/// Overload: tiny queues and a slow downstream, so back-pressure engages and
/// messages are shed by policy. The invariants must survive the shedding —
/// that is exactly what "robust to every drop the language admits" claims.
#[test]
fn invariants_survive_shedding_under_overload() {
    let out = compile_build_run(
        "clearinghouse-overload",
        "examples/clearinghouse/clearing.sigil",
        "4",
        &[
            ("SIGIL_DEMO_SHARDS", "2"),
            ("SIGIL_DEMO_CAPACITY", "4"),
            ("SIGIL_DEMO_PRODUCERS", "10"),
            ("SIGIL_DEMO_MSGS", "40"),
            ("SIGIL_CHAOS_LATENCY_MS", "80"),
            ("SIGIL_CHAOS_SLOW_PCT", "70"),
        ],
    );
    assert!(!out.contains("PROVEN INVARIANT VIOLATED"), "{out}");
    assert!(
        counter(&out, "shed=") > 0,
        "overload triggered no shedding, so this proves nothing:\n{out}"
    );
}

/// The security component: the defence-in-depth chain must hold while the
/// policy engine and KMS are failing.
#[test]
fn vault_chain_holds_under_chaos() {
    let out = compile_build_run(
        "vault-faults",
        "examples/security/vault.sigil",
        "4",
        &[
            ("SIGIL_DEMO_SHARDS", "4"),
            ("SIGIL_DEMO_PRODUCERS", "12"),
            ("SIGIL_DEMO_MSGS", "40"),
            ("SIGIL_CHAOS_FAIL_PCT", "20"),
            ("SIGIL_CHAOS_LATENCY_MS", "60"),
        ],
    );
    assert!(!out.contains("PROVEN INVARIANT VIOLATED"), "{out}");
}
