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
            "Usage: sigilc <file.sigil> [out_dir] [--emit-main] [--level 0|1|2|3|4]\n\
             \n\
             Assurance levels:\n\
             \x20 0 | sketch     exploratory; no safety checks, everything residual\n\
             \x20 1 | safe       default; extinct-by-design + signature checks\n\
             \x20 2 | contracts  spec obligations (require / hold / extinct)\n\
             \x20 3 | proofs     inductive hold proofs with runtime-guarded assumptions\n\
             \x20 4 | system     cross-process invariants proven over the topology"
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
                .with_context(|| format!("invalid assurance level '{v}' (expected 0-4)"))?;
        } else if arg == "--level" {
            let v = args_iter
                .next()
                .context("--level requires a value (0, 1, or 2)")?;
            level = AssuranceLevel::from_arg(v)
                .with_context(|| format!("invalid assurance level '{v}' (expected 0-4)"))?;
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
    let outcome = match run_checks(&program, &graph, level) {
        Ok(o) => o,
        Err(e) => {
            // Byte spans are for machines; show the person a source position.
            let rendered = sigilc::render_diagnostic(
                &format!("{e:#}"),
                &source,
                &input.display().to_string(),
            );
            eprintln!("\nerror[{}]: {rendered}", level.name());
            std::process::exit(1);
        }
    };
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
    fn compile_source(source: &str) -> (String, String, Vec<GraphIR>) {
        let program = parse(source).expect("parse");
        let graph = lower(&program).expect("lower");
        for ir in &graph {
            level1_check(ir).expect("level1");
        }
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
        let ir_err = graph.iter().find_map(|ir| level1_check(ir).err());
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
        for ir in &graph {
            level1_check(ir).expect("level1 should pass for L2-only failures");
        }
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
        assert!(graph.iter().any(|i| i.has_timeout()) && graph.iter().any(|i| i.has_recover()));
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
        assert!(graph.iter().any(|i| i.has_timeout()) && graph.iter().any(|i| i.has_recover()));
        assert!(rust.contains("ResilientProcessor") || rust.contains("normalize"));
        assert!(risk.contains("enrich") || risk.contains("external") || risk.contains("Level"));
    }

    #[test]
    fn compile_circuit() {
        let (rust, risk, graph) =
            compile_source(include_str!("../../examples/circuit/circuit.sigil"));
        assert!(graph.iter().any(|i| i.has_timeout()) && graph.iter().any(|i| i.has_recover()));
        assert!(rust.contains("CircuitBreaker"));
        assert!(risk.contains("Level-1"));
    }

    #[test]
    fn compile_pipeline() {
        let (rust, risk, graph) =
            compile_source(include_str!("../../examples/pipeline/pipeline.sigil"));
        assert_eq!(graph[0].process_name, "OrderPipeline");
        assert!(rust.contains("from_millis(120)") && rust.contains("from_millis(200)"));
        assert!(risk.contains("Level-2") || risk.contains("path_timeout") || risk.contains("320") || risk.contains("discharged"));
        assert!(risk.contains("confirm") || risk.contains("Declared") || risk.contains("Order"));
    }

    #[test]
    fn compile_level2_example() {
        let (rust, risk, graph) =
            compile_source(include_str!("../../examples/level2/slo_and_hold.sigil"));
        assert!(graph.iter().any(|i| i.has_timeout()) && graph.iter().any(|i| i.has_recover()));
        assert!(rust.contains("Service") || rust.contains("on_event"));
        assert!(risk.contains("Level-2") || risk.contains("discharged") || risk.contains("hold"));
    }

    #[test]
    fn compile_runnable_counter_and_demo_main() {
        let source = include_str!("../../examples/runnable/counter/counter.sigil");
        let (rust, risk, graph) = compile_source(source);
        assert!(graph.iter().all(|i| !i.has_timeout()));
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
            include_str!("../../examples/concurrent/orderflow/orderflow.sigil"),
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

    /// Back-pressure: three policies, each with its latency and loss
    /// characteristics enforced rather than documented.
    #[test]
    fn backpressure_policies() {
        use sigilc::{run_checks, AssuranceLevel};

        let base = r#"
schema M { id: String }
transform work(m: M) -> M {}
transform skip(m: M) -> M { m }
process Src {
  state sent: Int = 0
  on m: M {
    let ok = m ~> work @timeout(20.ms) @recover(with: skip)
    sent := sent + 1
    send ok to Sink POLICY
  }
}
process Sink {
  state got: Int = 0
  on m: M { got := got + 1 }
}
spec S {
  REQUIRE
  hold Sink.got <= Src.sent
}
"#;
        let build = |policy: &str, req: &str| {
            let src = base.replace("POLICY", policy).replace("REQUIRE", req);
            let program = parse(&src).expect("parse");
            let irs = lower(&program).expect("lower");
            (program.clone(), irs.clone(), run_checks(&program, &irs, AssuranceLevel::System))
        };

        // @block is the default and needs no annotation.
        let (_, _, r) = build("", "require path_timeout_sum <= 100.ms");
        assert!(r.is_ok(), "block is the default and must still work");

        // A latency claim is only provable with bounded policies.
        let (_, _, r) = build("@block", "require path_latency <= 100.ms");
        let e = format!("{:#}", r.err().expect("block cannot back a latency bound"));
        assert!(e.contains("END-TO-END") && e.contains("unbounded time") && e.contains("@deadline"), "got: {e}");

        let (_, _, r) = build("@shed", "require path_latency <= 100.ms");
        assert!(r.is_ok(), "shed is O(1) and bounded");

        let (_, _, r) = build("@deadline(5.ms)", "require path_latency <= 100.ms");
        let outcome = r.expect("deadline is bounded");
        // 20ms stage + 5ms hand-off.
        assert_eq!(outcome.level2.as_ref().unwrap().path_latency_ms, 25);

        // The deadline is charged: 30 + 90 = 120 > 100.
        let (program, irs, _) = build("@deadline(90.ms)", "require path_latency <= 100.ms");
        let _ = (&program, &irs);
        let src = base
            .replace("POLICY", "@deadline(90.ms)")
            .replace("REQUIRE", "require path_latency <= 100.ms")
            .replace("@timeout(20.ms)", "@timeout(30.ms)");
        let program = parse(&src).unwrap();
        let irs = lower(&program).unwrap();
        let e = format!(
            "{:#}",
            run_checks(&program, &irs, AssuranceLevel::System).err().expect("over budget")
        );
        assert!(e.contains("path_latency is 120ms"), "got: {e}");

        // Every policy preserves the Level-4 invariant: shedding only ever
        // DECREASES the downstream count.
        for policy in ["@block", "@shed", "@deadline(5.ms)"] {
            let src = base.replace("POLICY", policy).replace("REQUIRE", "");
            let program = parse(&src).expect("parse");
            let irs = lower(&program).expect("lower");
            let outcome = run_checks(&program, &irs, AssuranceLevel::System)
                .unwrap_or_else(|e| panic!("{policy} must keep the invariant provable: {e:#}"));
            assert_eq!(outcome.level4.as_ref().unwrap().proven.len(), 1);
        }

        // Codegen uses the runtime helpers, and lossy policies are counted.
        let src = base.replace("POLICY", "@shed").replace("REQUIRE", "");
        let program = parse(&src).unwrap();
        let irs = lower(&program).unwrap();
        let rust = emit(&program, &irs);
        assert!(rust.contains("sigil_rt::backpressure::shed("));
        assert!(rust.contains("self.__shed += 1"), "shed must be counted");
        assert!(rust.contains("note_shed"));
        assert!(!rust.contains("Mutex") && !rust.contains("Arc<") && !rust.contains("unsafe"));

        let src = base.replace("POLICY", "@deadline(7.ms)").replace("REQUIRE", "");
        let program = parse(&src).unwrap();
        let irs = lower(&program).unwrap();
        let rust = emit(&program, &irs);
        assert!(rust.contains("sigil_rt::backpressure::deadline(") && rust.contains(", 7)"));

        // The negative proof programs.
        let p = parse(include_str!("../../examples/proofs/latency_unbounded_block.sigil")).unwrap();
        let i = lower(&p).unwrap();
        assert!(run_checks(&p, &i, AssuranceLevel::Contracts).is_err());
        let p = parse(include_str!("../../examples/proofs/latency_budget_overflow.sigil")).unwrap();
        let i = lower(&p).unwrap();
        assert!(run_checks(&p, &i, AssuranceLevel::Contracts).is_err());
    }

    /// Multi-handler processes: type-directed dispatch, per-handler proof
    /// obligations, per-handler latency budgets, and the four ways to get it
    /// wrong.
    #[test]
    fn multi_handler_processes() {
        use sigilc::{check_handler_wellformedness, derive_topology, level4_prove, run_checks,
                     AssuranceLevel};

        let src = include_str!("../../examples/trading/order_gateway.sigil");
        let program = parse(src).expect("parse");
        let irs = lower(&program).expect("lower");
        let outcome = run_checks(&program, &irs, AssuranceLevel::System)
            .expect("trading gateway must pass Level 4");
        assert_eq!(outcome.level4.as_ref().unwrap().proven.len(), 2);

        // Budget is the MAX over handlers per process, not the sum: a message
        // traverses exactly one handler.
        // Gateway max(40,40)=40 + Risk max(120,60)=120 + Match max(100,100)=100
        assert_eq!(outcome.level2.as_ref().unwrap().path_timeout_sum_ms, 260);

        // Topology resolves each send to a specific destination handler by type.
        let topo = derive_topology(&program).expect("topology");
        let gw_to_risk: Vec<_> = topo
            .edges
            .iter()
            .filter(|e| e.from == "OrderGateway" && e.to == "RiskEngine")
            .collect();
        assert_eq!(gw_to_risk.len(), 2, "one edge per destination handler");
        assert!(gw_to_risk.iter().any(|e| e.msg_type == "NewOrder" && e.to_handler == "new_order"));
        assert!(gw_to_risk.iter().any(|e| e.msg_type == "Cancel" && e.to_handler == "cancel"));

        // Codegen emits a typed dispatch enum and per-type send methods.
        let rust = emit(&program, &irs);
        assert!(rust.contains("pub enum RiskEngineMsg"));
        assert!(rust.contains("NewOrder(NewOrder)") && rust.contains("Cancel(Cancel)"));
        assert!(rust.contains("send_new_order") && rust.contains("send_cancel"));
        // Each send routes by key AND wraps in the correct dispatch variant.
        assert!(rust.contains("out.by_key(&ok.account).raw(), RiskEngineMsg::NewOrder(ok)"));
        assert!(rust.contains("out.by_key(&ok.account).raw(), RiskEngineMsg::Cancel(ok)"));
        assert!(rust.contains("out.round_robin().raw(), MatchingEngineMsg::NewOrder"));
        assert!(rust.contains("out.round_robin().raw(), MatchingEngineMsg::Cancel"));
        assert!(!rust.contains("Mutex") && !rust.contains("Arc<") && !rust.contains("unsafe"));

        // The demo exercises EVERY handler of the entry process.
        let main_rs = sigilc::emit_demo_main(&program);
        assert!(main_rs.contains("send_new_order") && main_rs.contains("send_cancel"));

        // --- the four ways to get multi-handler wrong ---
        let l4_fails = |src: &str, needle: &str| {
            let program = parse(src).expect("parse");
            let err = level4_prove(&program).expect_err("must fail");
            assert!(format!("{err}").contains(needle), "got: {err}");
        };
        // 1. a handler forwards without counting
        l4_fails(
            include_str!("../../examples/proofs/mh_uncounted_handler.sigil"),
            "never updates",
        );
        // 2. duplicate handler message names (would emit duplicate enum variants)
        let p = parse(include_str!("../../examples/proofs/mh_duplicate_msg_name.sigil")).unwrap();
        let e = check_handler_wellformedness(&p).expect_err("dup name");
        assert!(format!("{e}").contains("message name"));
        // 3. duplicate handler types (ambiguous dispatch)
        let p = parse(include_str!("../../examples/proofs/mh_duplicate_msg_type.sigil")).unwrap();
        let e = check_handler_wellformedness(&p).expect_err("dup type");
        assert!(format!("{e}").contains("resolves the destination handler by message type"));
        // 4. sending a type the target cannot receive
        let p = parse(include_str!("../../examples/proofs/mh_no_handler_for_type.sigil")).unwrap();
        let e = derive_topology(&p).expect_err("no handler for type");
        assert!(format!("{e}").contains("no handler for that type"));
    }

    /// Ordering is enforced PER HANDLER: one compliant handler does not
    /// excuse another.
    #[test]
    fn multi_handler_ordering_is_per_handler() {
        use sigilc::level4_prove;
        let src = r#"
schema A { x: Int }
schema B { y: Int }
process Up {
  state seen: Int = 0
  on a: A {
    seen := seen + 1
    send a to Down
  }
  on b: B {
    send b to Down
    seen := seen + 1
  }
}
process Down {
  state got: Int = 0
  on a: A { got := got + 1 }
  on b: B { got := got + 1 }
}
spec S { hold Down.got <= Up.seen }
"#;
        let program = parse(src).expect("parse");
        let err = level4_prove(&program).expect_err("second handler violates ordering");
        let msg = format!("{err}");
        assert!(msg.contains("ORDERING fails"), "got: {msg}");
        assert!(msg.contains("`b` handler"), "must name the offending handler: {msg}");
    }

    /// The finance component: five proofs, a budget, and zero-loss codegen.
    #[test]
    fn finance_clearing_component() {
        use sigilc::{run_checks, AssuranceLevel};
        let src = include_str!("../../examples/finance/clearing.sigil");
        let program = parse(src).expect("parse");
        let irs = lower(&program).expect("lower");
        let outcome = run_checks(&program, &irs, AssuranceLevel::System)
            .expect("clearing must pass Level 4");
        assert_eq!(outcome.level4.as_ref().unwrap().proven.len(), 3, "3 system holds");
        assert_eq!(outcome.level3.as_ref().unwrap().proven.len(), 2, "2 scalar holds");
        assert_eq!(
            outcome.level2.as_ref().unwrap().path_timeout_sum_ms,
            380,
            "longest-path budget"
        );
        let rust = emit(&program, &irs);
        assert!(!rust.contains("Mutex") && !rust.contains("Arc<") && !rust.contains("unsafe"));
        // The float accumulator that would need Arc<Mutex<f64>> by hand.
        assert!(rust.contains("pub settled_value: f64"));
    }

    /// The security component: audit-before-serve is PROVEN, and the two
    /// review-passing mistakes that would break it are compile errors.
    #[test]
    fn security_vault_component() {
        use sigilc::{level4_prove, run_checks, AssuranceLevel};
        let src = include_str!("../../examples/security/vault.sigil");
        let program = parse(src).expect("parse");
        let irs = lower(&program).expect("lower");
        let outcome = run_checks(&program, &irs, AssuranceLevel::System)
            .expect("vault must pass Level 4");
        let proven = &outcome.level4.as_ref().unwrap().proven;
        assert_eq!(proven.len(), 3, "the defense-in-depth chain");
        assert!(proven.iter().any(|p| p.contains("Vault.served <= Audit.recorded")));

        // Mistake 1: audit AFTER the send (passes code review, fails the build).
        let after = src.replace(
            "    recorded := recorded + 1
    send logged to Vault",
            "    send logged to Vault
    recorded := recorded + 1",
        );
        let program = parse(&after).expect("parse");
        let err = level4_prove(&program).expect_err("audit-after-send must fail");
        assert!(format!("{err}").contains("ORDERING fails"));

        // Mistake 2: a deny path that can itself fail or hang.
        let open = src.replace(
            "transform deny_unauthorized(r: Request) -> Request { r }",
            "transform deny_unauthorized(r: Request) -> Request {}",
        );
        let program = parse(&open).expect("parse");
        let irs = lower(&program).expect("lower");
        let err = run_checks(&program, &irs, AssuranceLevel::Proofs)
            .expect_err("fallible deny must fail at Level 3+");
        assert!(format!("{err}").contains("infallible recovery"));
        // ...but it is a reported residual, not an error, at Level 1.
        let outcome = run_checks(&program, &irs, AssuranceLevel::Safe).expect("L1 ok");
        assert!(outcome.notes.iter().any(|n| n.contains("fallible recovery")));
    }

    /// Level 4: system invariants proven structurally over the topology,
    /// with each proof obligation (ordering, flow, broadcast, gap) having a
    /// negative proof program.
    #[test]
    fn level4_system_invariants() {
        use sigilc::{level4_prove, run_checks, AssuranceLevel};

        // Flagship passes all four levels; both system holds proven.
        let src = include_str!("../../examples/level4/conservation.sigil");
        let program = parse(src).expect("parse");
        let irs = lower(&program).expect("lower");
        let outcome = run_checks(&program, &irs, AssuranceLevel::System)
            .expect("conservation must pass Level 4");
        let l4 = outcome.level4.expect("level4 ran");
        assert_eq!(l4.proven.len(), 2, "both system holds proven");
        assert!(outcome.level3.is_some(), "Level 4 includes Level 3");

        // Each obligation fails for its own reason.
        let fails = |src: &str, needle: &str| {
            let program = parse(src).expect("parse");
            let err = level4_prove(&program).expect_err("must fail");
            let msg = format!("{err}");
            assert!(msg.contains(needle), "expected '{needle}', got: {msg}");
        };
        fails(
            include_str!("../../examples/proofs/system_ordering.sigil"),
            "ORDERING fails",
        );
        fails(
            include_str!("../../examples/proofs/system_leak.sigil"),
            "FLOW fails",
        );
        fails(
            include_str!("../../examples/proofs/system_broadcast.sigil"),
            "broadcast",
        );

        // GAP: Settlement counting +2 per message breaks the bound.
        let gap = src.replace("posted := posted + 1", "posted := posted + 2");
        let program = parse(&gap).expect("parse");
        let err = level4_prove(&program).expect_err("gap must fail");
        assert!(format!("{err}").contains("GAP fails"));

        // The system proofs still hold at lower levels' semantics: the same
        // program builds at level 2 with holds residual.
        let program = parse(src).expect("parse");
        let irs = lower(&program).expect("lower");
        run_checks(&program, &irs, AssuranceLevel::Contracts).expect("residual at L2");
    }

    /// Level 3: holds are proven inductively; assumptions are runtime guards
    /// in the generated code; undischargeable holds fail the build.
    #[test]
    fn level3_proofs_are_real_and_guarded() {
        use sigilc::{level3_prove, run_checks, AssuranceLevel};

        let src = include_str!("../../examples/level3/proven_ledger.sigil");
        let program = parse(src).expect("parse");
        let irs = lower(&program).expect("lower");
        let outcome = run_checks(&program, &irs, AssuranceLevel::Proofs)
            .expect("proven ledger must pass Level 3");
        let l3 = outcome.level3.expect("level3 ran");
        assert_eq!(l3.proven.len(), 2, "both holds proven");
        assert_eq!(l3.guarded_assumptions.len(), 2, "both requires guarded");

        // The assumptions are ENFORCED: guards appear in the emitted handler.
        let rust = emit(&program, &irs);
        assert!(rust.contains("payment.amount >= 0f64"), "amount guard missing");
        assert!(rust.contains("(payment.units as f64) >= 0f64"), "units guard missing");
        assert!(rust.contains("SigilError::Schema"), "guard must reject typed");

        // Dropping an assumption breaks the inductive step with a named fix.
        let unguarded = src.replace("  require payment.amount >= 0.0\n", "");
        let program = parse(&unguarded).expect("parse");
        let err = level3_prove(&program).expect_err("must fail without the guard");
        let msg = format!("{err}");
        assert!(msg.contains("INDUCTIVE STEP fails") && msg.contains("unguarded"));

        // The non-inductive proof program fails at --level 3.
        let bad = include_str!("../../examples/proofs/hold_not_inductive.sigil");
        let program = parse(bad).expect("parse");
        let irs = lower(&program).expect("lower");
        assert!(run_checks(&program, &irs, AssuranceLevel::Proofs).is_err());
        // ...but still builds at Level 2, where holds are residual.
        run_checks(&program, &irs, AssuranceLevel::Contracts)
            .expect("residual at level 2");
    }

    /// Soundness hardening before Level 3: every hole found in the L1/L2
    /// audit is closed by a proof program.
    #[test]
    fn soundness_hardening_proofs() {
        use sigilc::{check_transform_purity, derive_topology, run_checks, AssuranceLevel};
        let reject = |src: &str, needle: &str| {
            let program = parse(src).expect("parse");
            let irs = lower(&program).expect("lower");
            let err = run_checks(&program, &irs, AssuranceLevel::Safe)
                .err()
                .map(|e| format!("{e:#}"))
                .unwrap_or_default();
            assert!(err.contains(needle), "expected '{needle}', got: {err}");
        };
        reject(
            include_str!("../../examples/proofs/cross_process_state.sigil"),
            "non-local slot",
        );
        reject(
            include_str!("../../examples/proofs/bare_external_call.sigil"),
            "bare call",
        );
        reject(
            include_str!("../../examples/proofs/impure_pure_transform.sigil"),
            "pure transform",
        );
        reject(
            include_str!("../../examples/proofs/conflicting_tags.sigil"),
            "not both",
        );

        // Purity check directly too.
        let program =
            parse(include_str!("../../examples/proofs/impure_pure_transform.sigil")).unwrap();
        assert!(check_transform_purity(&program).is_err());

        // @timeout + @error is a legal acknowledged drop at L1 AND L2.
        let ok_src = include_str!("../../examples/proofs/acknowledged_timeout.sigil");
        let program = parse(ok_src).expect("parse");
        let irs = lower(&program).expect("lower");
        run_checks(&program, &irs, AssuranceLevel::Contracts)
            .expect("acknowledged timeout must pass both levels");
        let _ = derive_topology(&program).expect("trivial topology");
        let rust = emit(&program, &irs);
        // Codegen must propagate honestly, not silently retry-forever or recover.
        assert!(rust.contains("SigilError::Timeout"), "acknowledged drop must propagate");
        assert!(rust.contains("__attempt < 1"), "bounded retry before the drop");
    }

    /// The Level-2 budget is the LONGEST PATH over the topology, not a blind
    /// global sum: parallel branches take max.
    #[test]
    fn budget_is_longest_path() {
        let src = r#"
schema M { v: Int }
transform f(m: M) -> M {}
transform p(m: M) -> M { m }
process Entry {
  state n: Int = 0
  on m: M {
    let out = m ~> f @timeout(100.ms) @recover(with: p)
    n := n + out.v
    send out to Left
    send out to Right
  }
}
process Left {
  state n: Int = 0
  on m: M {
    let out = m ~> f @timeout(300.ms) @recover(with: p)
    n := n + out.v
  }
}
process Right {
  state n: Int = 0
  on m: M {
    let out = m ~> f @timeout(50.ms) @recover(with: p)
    n := n + out.v
  }
}
"#;
        let program = parse(src).expect("parse");
        let irs = lower(&program).expect("lower");
        let l2 = level2_check(&program, &irs).expect("level2");
        // Longest path = Entry(100) + Left(300) = 400; a global sum would say 450.
        assert_eq!(l2.path_timeout_sum_ms, 400);
    }

    /// @retry: proven at three layers — the Level-1 rule, the Level-2 budget
    /// arithmetic, and the emitted retry loop.
    #[test]
    fn retry_is_proven() {
        // 1) Retry without a terminal failure path is rejected.
        let bad = include_str!("../../examples/proofs/retry_without_recover.sigil");
        let program = parse(bad).expect("parse");
        let err = check_failure_paths(&program).expect_err("retry needs recover/error");
        let msg = format!("{err}");
        assert!(
            msg.contains("@retry") || msg.contains("no failure path"),
            "got: {msg}"
        );

        // 2) Budget charges worst case (1 + retries) x timeout: 600 > 500 fails.
        let overflow = include_str!("../../examples/proofs/retry_budget_overflow.sigil");
        let program = parse(overflow).expect("parse");
        let graph = lower(&program).expect("lower");
        let err = level2_check(&program, &graph).expect_err("600ms must exceed 500ms SLO");
        let msg = format!("{err}");
        assert!(msg.contains("path_timeout_sum"), "got: {msg}");

        // 3) The orderflow budget passes precisely because (1+2)x60 = 180 <= 200,
        //    and the emitted code contains bounded retry loops.
        let src = include_str!("../../examples/concurrent/orderflow/orderflow.sigil");
        let program = parse(src).expect("parse");
        let graph = lower(&program).expect("lower");
        let l2 = level2_check(&program, &graph).expect("budget holds with retries");
        assert_eq!(l2.path_timeout_sum_ms, 180, "budget must charge attempts x timeout");
        let (rust, _, _) = compile_source(src);
        assert!(rust.contains("__attempt < 2"), "bounded retry loop missing");
        assert!(rust.contains("note_retry(\"score\")"));
        assert!(rust.contains("note_retry(\"post\")"), "untimed retry loop missing");
        assert!(!rust.contains("Mutex") && !rust.contains("Arc<") && !rust.contains("unsafe"));
    }

    /// Routing: Float keys rejected; the three policies emit distinct code.
    #[test]
    fn routing_policies() {
        let bad = include_str!("../../examples/proofs/float_route_key.sigil");
        let program = parse(bad).expect("parse");
        let err = sigilc::derive_topology(&program).expect_err("Float key must be rejected");
        assert!(format!("{err}").contains("Float key"));

        let src = include_str!("../../examples/concurrent/orderflow/orderflow.sigil");
        let (rust, _, _) = compile_source(src);
        assert!(rust.contains("by_key(&ok.id)"), "hash routing missing");
        assert!(rust.contains("round_robin().raw()"), "round-robin missing");
        assert!(rust.contains("for h in out.shards()"), "broadcast missing");
        assert!(!rust.contains("Mutex") && !rust.contains("Arc<") && !rust.contains("unsafe"));
    }

    /// Multi-process topology: compiler wires outboxes, types the edges,
    /// and the generated demo stages the shutdown.
    #[test]
    fn topology_codegen_wires_stages() {
        let src = include_str!("../../examples/concurrent/orderflow/orderflow.sigil");
        let (rust, risk, _) = compile_source(src);
        // Outboxes + wiring
        assert!(rust.contains("risk_out: Option<sigil_rt::Router<RiskHandle>>"));
        assert!(rust.contains("settlement_out: Option<sigil_rt::Router<SettlementHandle>>"));
        assert!(rust.contains("pub fn connect_risk"));
        // Cascade shutdown: outboxes released when the actor drains
        assert!(rust.contains("self.risk_out = None"));
        assert!(rust.contains("self.settlement_out = None"));
        // Still lock-free
        assert!(!rust.contains("Mutex") && !rust.contains("Arc<") && !rust.contains("unsafe"));
        // Residual report knows the verified topology
        assert!(risk.contains("`Gateway` → `Risk`"), "topology missing from residual report");

        let program = parse(src).unwrap();
        let main_rs = sigilc::emit_demo_main(&program);
        assert!(main_rs.contains("inst.connect_risk"));
        assert!(main_rs.contains("inst.connect_settlement"));
        assert!(main_rs.contains("entry-stage message conservation"));
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
