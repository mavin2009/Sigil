use proptest::prelude::*;
use sigilc::{
    emit, emit_cargo_toml, format_program, interpret_handler, lower, parse, reference_record,
    run_checks, write_generated_crate, AssuranceLevel, GeneratedCrate, Program, ReferenceValue,
    TraceEvent,
};
use std::process::Command;

fn counter_source(process: &str, state: &str, message: &str, field: &str, initial: i64) -> String {
    format!(
        "schema M {{ {field}: Int }}\n\
         process {process} {{\n\
         state {state}: Int = {initial}\n\
         on {message}: M {{ {state} := {state} + {message}.{field} }}\n\
         }}\n\
         spec S {{ require {message}.{field} >= 0 hold {state} >= {initial} }}\n"
    )
}

fn typed_ast_strategy() -> impl Strategy<Value = Program> {
    prop_oneof![
        Just(("Int", "0")),
        Just(("Float", "0.0")),
        Just(("String", "\"\"")),
        Just(("Bool", "false")),
        Just(("Duration", "0.ms")),
    ]
    .prop_map(|(field_type, initial)| {
        parse(&format!(
            "schema M {{ value: {field_type} }}\n\
             process P {{\n\
             state observed: {field_type} = {initial}\n\
             on m: M {{ observed := m.value }}\n\
             }}\n"
        ))
        .expect("strategy constructs a well-typed AST")
    })
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 128,
        max_shrink_iters: 4096,
        ..ProptestConfig::default()
    })]

    #[test]
    fn proven_programs_are_not_refuted_by_reference_execution(
        initial in 0i64..10_000,
        inputs in prop::collection::vec(0i64..10_000, 0..32),
    ) {
        let source = counter_source("Counter", "total", "m", "value", initial);
        let program = parse(&source).expect("generated source parses");
        let graph = lower(&program).expect("generated source lowers");
        run_checks(&program, &graph, AssuranceLevel::Proofs).expect("generated theorem proves");
        let messages = inputs
            .iter()
            .map(|value| {
                reference_record(
                    &program,
                    "M",
                    [("value".to_string(), ReferenceValue::Int(*value))],
                )
                .expect("typed reference message")
            })
            .collect::<Vec<_>>();
        let result = interpret_handler(&program, "Counter", &messages).expect("reference execution");
        let expected = inputs
            .iter()
            .try_fold(initial, |sum, value| sum.checked_add(*value))
            .expect("strategy stays inside i64");
        let expected_value = ReferenceValue::Int(expected);
        prop_assert_eq!(
            result.state.get("total"),
            Some(&expected_value)
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 10,
        max_shrink_iters: 128,
        ..ProptestConfig::default()
    })]

    #[test]
    fn generated_typed_asts_are_accepted_and_typecheck_in_rust(program in typed_ast_strategy()) {
        let graph = lower(&program).expect("typed AST lowers");
        run_checks(&program, &graph, AssuranceLevel::Safe).expect("typed AST is accepted");
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace");
        let output = workspace
            .join("target")
            .join(format!("typed-ast-strategy-{}", std::process::id()));
        write_generated_crate(
            &output,
            &GeneratedCrate {
                lib_rs: emit(&program, &graph),
                main_rs: None,
                cargo_toml: emit_cargo_toml(
                    "typed_ast_strategy",
                    &workspace.join("sigil_rt").display().to_string(),
                    false,
                ),
                topology_mermaid: None,
                topology_dot: None,
                residual_risk_md: String::new(),
                residual_risk_json: "{}\n".into(),
                effect_contracts_json: "{}\n".into(),
                build_manifest_json: "{}\n".into(),
            },
        )
        .expect("transactional strategy generation");
        let result = Command::new("cargo")
            .args(["check", "--offline", "--all-features"])
            .env(
                "CARGO_TARGET_DIR",
                workspace.join("target/typed-ast-strategy-cache"),
            )
            .current_dir(&output)
            .output()
            .expect("run generated cargo check");
        prop_assert!(
            result.status.success(),
            "accepted typed AST failed generated Rust checking:\n{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[test]
fn alpha_renaming_preserves_proofs() {
    for (process, state, message, field) in [
        ("Counter", "total", "m", "value"),
        ("Renamed", "balance", "event", "amount"),
        ("X", "n", "input", "delta"),
    ] {
        let source = counter_source(process, state, message, field, 0);
        let program = parse(&source).expect("alpha variant parses");
        let graph = lower(&program).expect("alpha variant lowers");
        run_checks(&program, &graph, AssuranceLevel::Proofs).expect("alpha variant proves");
    }
}

#[test]
fn independent_statement_permutation_preserves_state_and_proofs() {
    let template = |body: &str| {
        format!(
            "schema M {{ x: Int, y: Int }}\n\
             process P {{\n\
             state a: Int = 0\n\
             state b: Int = 0\n\
             on m: M {{ {body} }}\n\
             }}\n\
             spec S {{ require m.x >= 0 require m.y >= 0 hold a >= 0 hold b >= 0 }}\n"
        )
    };
    let left = parse(&template("a := a + m.x b := b + m.y")).expect("left parses");
    let right = parse(&template("b := b + m.y a := a + m.x")).expect("right parses");
    for program in [&left, &right] {
        let graph = lower(program).expect("lowers");
        run_checks(program, &graph, AssuranceLevel::Proofs).expect("proves");
    }
    let message = reference_record(
        &left,
        "M",
        [
            ("x".to_string(), ReferenceValue::Int(3)),
            ("y".to_string(), ReferenceValue::Int(5)),
        ],
    )
    .expect("message");
    let left_result = interpret_handler(&left, "P", std::slice::from_ref(&message)).expect("left");
    let right_result = interpret_handler(&right, "P", &[message]).expect("right");
    assert_eq!(left_result.state, right_result.state);
}

#[test]
fn canonical_printing_is_stable_across_language_features() {
    for source in [
        include_str!("../../examples/pipeline/pipeline.sigil"),
        include_str!("../../examples/clearinghouse/clearing.sigil"),
        include_str!("../../examples/avionics/attitude_control.sigil"),
    ] {
        let once = format_program(&parse(source).expect("example parses"));
        let twice = format_program(&parse(&once).expect("canonical source parses"));
        assert_eq!(once, twice);
    }
}

#[test]
fn level1_accepted_typed_ast_compiles_as_generated_rust() {
    let source = r#"
schema M {
  id: UUID,
  text: String,
  bytes: Bytes,
  count: Int,
  ratio: Float,
  enabled: Bool,
  elapsed: Duration,
}
process A {
  state count: Int = 0
  state ratio: Float = 0.0
  state enabled: Bool = false
  on m: M {
    count := m.count
    ratio := m.ratio
    enabled := m.enabled
    send M {
      id: m.id,
      text: m.text,
      bytes: m.bytes,
      count: m.count,
      ratio: m.ratio,
      enabled: m.enabled,
      elapsed: m.elapsed,
    } to B by m.id @shed
  }
}
process B { on m: M {} }
"#;
    let program = parse(source).expect("typed program parses");
    let graph = lower(&program).expect("typed program lowers");
    run_checks(&program, &graph, AssuranceLevel::Safe).expect("Level 1 accepts");

    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace");
    let output = workspace
        .join("target")
        .join(format!("typed-ast-property-{}", std::process::id()));
    let generated = GeneratedCrate {
        lib_rs: emit(&program, &graph),
        main_rs: None,
        cargo_toml: emit_cargo_toml(
            "typed_ast_property",
            &workspace.join("sigil_rt").display().to_string(),
            false,
        ),
        topology_mermaid: None,
        topology_dot: None,
        residual_risk_md: String::new(),
        residual_risk_json: "{}\n".into(),
        effect_contracts_json: "{}\n".into(),
        build_manifest_json: "{}\n".into(),
    };
    write_generated_crate(&output, &generated).expect("transactional generation");
    let result = Command::new("cargo")
        .args(["check", "--offline", "--all-features"])
        .current_dir(&output)
        .output()
        .expect("run cargo check");
    assert!(
        result.status.success(),
        "accepted generated crate failed to type-check:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn generated_rust_matches_reference_state_and_send_trace() {
    let source = r#"
schema M { value: Int }
process Source {
  state total: Int = 0
  on m: M {
    total := total + m.value
    send m to Sink
  }
}
process Sink {
  state received: Int = 0
  on m: M { received := received * 10 + m.value }
}
"#;
    let program = parse(source).expect("differential program parses");
    let graph = lower(&program).expect("differential program lowers");
    run_checks(&program, &graph, AssuranceLevel::Safe).expect("differential program is safe");
    let messages = [3, 5, 8]
        .into_iter()
        .map(|value| {
            reference_record(
                &program,
                "M",
                [("value".to_string(), ReferenceValue::Int(value))],
            )
            .expect("typed message")
        })
        .collect::<Vec<_>>();
    let reference =
        interpret_handler(&program, "Source", &messages).expect("reference execution succeeds");
    let ReferenceValue::Int(expected_source) = reference.state["total"] else {
        panic!("reference state has the declared Int type");
    };
    let expected_sink = reference
        .trace
        .iter()
        .filter_map(|event| match event {
            TraceEvent::Send {
                target,
                value: ReferenceValue::Record(record),
            } if target == "Sink" => match record.get("value") {
                Some(ReferenceValue::Int(value)) => Some(*value),
                _ => panic!("reference send has the declared Int field"),
            },
            _ => None,
        })
        .fold(0i64, |encoded, value| encoded * 10 + value);

    let main_rs = r#"use sigil_gen::{M, Sink, Source};

#[tokio::main]
async fn main() -> sigil_rt::Result<()> {
    let (sink_handle, sink_task) = Sink::new().spawn(8)?;
    let mut source = Source::new();
    source.connect_sink(vec![sink_handle.clone()])?;
    let (source_handle, source_task) = source.spawn(8)?;
    for value in [3, 5, 8] {
        source_handle.send(M { value }).await?;
    }
    drop(source_handle);
    let (source, _) = source_task.join().await?;
    drop(sink_handle);
    let (sink, _) = sink_task.join().await?;
    println!("{},{}", source.total, sink.received);
    Ok(())
}
"#;
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace");
    let output = workspace
        .join("target")
        .join(format!("reference-differential-{}", std::process::id()));
    let generated = GeneratedCrate {
        lib_rs: emit(&program, &graph),
        main_rs: Some(main_rs.into()),
        cargo_toml: emit_cargo_toml(
            "reference_differential",
            &workspace.join("sigil_rt").display().to_string(),
            true,
        ),
        topology_mermaid: None,
        topology_dot: None,
        residual_risk_md: String::new(),
        residual_risk_json: "{}\n".into(),
        effect_contracts_json: "{}\n".into(),
        build_manifest_json: "{}\n".into(),
    };
    write_generated_crate(&output, &generated).expect("transactional generation");
    let result = Command::new("cargo")
        .args(["run", "--offline", "--quiet", "--bin", "demo"])
        .current_dir(&output)
        .output()
        .expect("run generated semantics");
    assert!(
        result.status.success(),
        "generated semantics failed:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&result.stdout).trim(),
        format!("{expected_source},{expected_sink}")
    );
}
