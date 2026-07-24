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
fn generated_wire_codec_round_trips_nested_schemas_and_fences_delivery() {
    let source = r#"
schema Child { sequence: Int, label: String }
schema M {
  child: Child,
  ratio: Float,
  enabled: Bool,
  bytes: Bytes,
  elapsed: Duration,
}
placement edge { Source }
placement core { Sink }
process Source { on m: M { send m to Sink @shed } }
process Sink { on m: M {} }
"#;
    let program = parse(source).expect("codec program parses");
    let graph = lower(&program).expect("codec program lowers");
    run_checks(&program, &graph, AssuranceLevel::Safe).expect("codec program is safe");

    let main_rs = r#"use sigil_gen::{transport_manifest, Child, M};
use sigil_rt::distributed::{
    CodecError, CodecLimits, DeliverySemantics, MessageId, NegotiatedTransport, NodeId,
    OwnershipEpoch, ShardAddress, ShardKey, ShardLease, WireCodec,
};
use std::time::Duration;

fn main() {
    let limits = CodecLimits::new(1024, 128).expect("limits");
    let message = M {
        child: Child {
            sequence: -42,
            label: "deterministic".into(),
        },
        ratio: 1.5,
        enabled: true,
        bytes: vec![0, 1, 255],
        elapsed: Duration::new(7, 9),
    };
    let bytes = message.encode_bounded(limits).expect("encode");
    let decoded = M::decode_bounded(&bytes, limits).expect("decode");
    assert_eq!(decoded.child.sequence, -42);
    assert_eq!(decoded.child.label, "deterministic");
    assert_eq!(decoded.ratio, 1.5);
    assert!(decoded.enabled);
    assert_eq!(decoded.bytes, [0, 1, 255]);
    assert_eq!(decoded.elapsed, Duration::new(7, 9));
    assert_eq!(
        message.encode_bounded(limits).expect("encode again"),
        bytes
    );
    assert!(matches!(
        message.encode_bounded(CodecLimits::new(24, 8).expect("small limits")),
        Err(CodecError::FieldTooLarge { .. } | CodecError::PayloadTooLarge { .. })
    ));

    let manifest = transport_manifest(1024).expect("manifest");
    let session = NegotiatedTransport::negotiate(&manifest, &manifest).expect("session");
    let destination = ShardAddress::new(
        ShardKey::new("prod", "core", "Sink", 0).expect("shard"),
        NodeId::new("node-a").expect("node"),
        OwnershipEpoch::new(1).expect("epoch"),
    );
    let envelope = message
        .encode_remote(
            &session,
            destination.clone(),
            MessageId::new("producer-a", 1).expect("message id"),
            DeliverySemantics::AtMostOnce,
            limits,
        )
        .expect("typed envelope");
    let decoded = M::decode_remote(&session, &envelope, limits).expect("typed decode");
    assert_eq!(decoded.child.label, "deterministic");

    let lease = ShardLease::serving(destination);
    let authorized =
        M::authorize_remote(&session, &lease, &envelope, limits).expect("authorize");
    assert_eq!(lease.in_flight(), 1);
    assert_eq!(authorized.message().child.sequence, -42);
    drop(authorized);
    assert_eq!(lease.in_flight(), 0);
}
"#;
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace");
    let output = workspace
        .join("target")
        .join(format!("generated-wire-codec-{}", std::process::id()));
    write_generated_crate(
        &output,
        &GeneratedCrate {
            lib_rs: emit(&program, &graph),
            main_rs: Some(main_rs.into()),
            cargo_toml: emit_cargo_toml(
                "generated_wire_codec",
                &workspace.join("sigil_rt").display().to_string(),
                true,
            ),
            topology_mermaid: None,
            topology_dot: None,
            residual_risk_md: String::new(),
            residual_risk_json: "{}\n".into(),
            effect_contracts_json: "{}\n".into(),
            build_manifest_json: "{}\n".into(),
        },
    )
    .expect("transactional codec generation");
    let result = Command::new("cargo")
        .args(["run", "--offline", "--quiet", "--bin", "demo"])
        .env("RUSTFLAGS", "-D warnings")
        .current_dir(&output)
        .output()
        .expect("run generated codec");
    assert!(
        result.status.success(),
        "generated codec failed:\n{}",
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

#[test]
fn generated_component_and_placement_assembly_cover_failure_restart_and_replay() {
    let source = r#"
schema M { value: Int }
placement ingress { Source }
placement workers { Sink }
process Source {
  state total: Int = 0
  on m: M {
    total := total + m.value
    send m to Sink @deadline(20.ms)
  }
}
process Sink {
  state received: Int = 0
  on m: M { received := received + m.value }
}
"#;

    fn assert_generated_placement_startup_hands_owned_permits_through_durable_receivers() {
        let source = r#"
schema M { value: Int }
placement edge { Source }
placement core { Sink }
process Source { on m: M { send m to Sink @deadline(50.ms) } }
process Sink {
  state received: Int = 0
  on m: M { received := received + m.value }
}
"#;
        let program = parse(source).expect("placement program parses");
        let graph = lower(&program).expect("placement program lowers");
        run_checks(&program, &graph, AssuranceLevel::Safe).expect("placement program is safe");

        let main_rs = r#"use sigil_gen::{
    transport_manifest, ComponentConfig, M, PlacementComponent, RemoteCommitters,
    RemoteEndpoints, Sink,
};
use sigil_rt::distributed::{
    AcknowledgeOutcome, ApplyReceipt, AttemptReservation, CommitLookup, CommitLookupFuture,
    DeliveryAck, DeliveryContext, DeliverySemantics, DurableCommit, DurableOutbox,
    DurableRemoteEndpoint, MessageId, MessageIdDurability, MessageIdFuture, MessageIdSource,
    NegotiatedTransport, NodeId, OutboxRecord, OutboxStore, OwnershipEpoch, PersistOutcome,
    ReceiverOutcome, RemoteAdmission, RetryPolicy, ShardAddress, ShardKey, ShardLease,
    StateCommitFuture, StateCommitter, StateRestoreFuture, StoreFuture, Transport,
    TransportError, TransportFuture, TransportOutcome, TransportResult,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct DurableIds(AtomicU64);

impl MessageIdSource for DurableIds {
    fn durability(&self) -> MessageIdDurability {
        MessageIdDurability::Durable
    }

    fn next_id(&self) -> MessageIdFuture<'_> {
        let sequence = self.0.fetch_add(1, Ordering::Relaxed) + 1;
        Box::pin(async move { MessageId::new("edge-source", sequence) })
    }
}

#[derive(Default)]
struct MemoryOutboxState {
    pending: BTreeMap<MessageId, OutboxRecord>,
    acknowledged: BTreeSet<MessageId>,
}

#[derive(Default)]
struct MemoryOutbox {
    state: Mutex<MemoryOutboxState>,
}

impl OutboxStore for MemoryOutbox {
    fn persist(&self, record: OutboxRecord) -> StoreFuture<'_, PersistOutcome> {
        Box::pin(async move {
            let id = record.envelope().message_id().clone();
            let mut state = self
                .state
                .lock()
                .map_err(|_| TransportError::Unavailable("outbox lock poisoned".into()))?;
            if state.acknowledged.contains(&id) {
                return Ok(PersistOutcome::AlreadyPending);
            }
            let outcome = match state.pending.entry(id) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(record);
                    PersistOutcome::Inserted
                }
                std::collections::btree_map::Entry::Occupied(entry) => {
                    if entry.get().envelope() != record.envelope() {
                        return Err(TransportError::Configuration(
                            "message identity was reused for different bytes".into(),
                        ));
                    }
                    PersistOutcome::AlreadyPending
                }
            };
            Ok(outcome)
        })
    }

    fn load<'a>(&'a self, id: &'a MessageId) -> StoreFuture<'a, Option<OutboxRecord>> {
        Box::pin(async move {
            Ok(self
                .state
                .lock()
                .map_err(|_| TransportError::Unavailable("outbox lock poisoned".into()))?
                .pending
                .get(id)
                .cloned())
        })
    }

    fn pending(&self, limit: usize) -> StoreFuture<'_, Vec<OutboxRecord>> {
        Box::pin(async move {
            Ok(self
                .state
                .lock()
                .map_err(|_| TransportError::Unavailable("outbox lock poisoned".into()))?
                .pending
                .values()
                .take(limit)
                .cloned()
                .collect())
        })
    }

    fn reserve_attempt<'a>(
        &'a self,
        id: &'a MessageId,
        maximum: u32,
    ) -> StoreFuture<'a, AttemptReservation> {
        Box::pin(async move {
            let mut state = self
                .state
                .lock()
                .map_err(|_| TransportError::Unavailable("outbox lock poisoned".into()))?;
            if state.acknowledged.contains(id) {
                return Ok(AttemptReservation::AlreadyAcknowledged);
            }
            let record = state.pending.get_mut(id).ok_or_else(|| {
                TransportError::Configuration("attempted a missing outbox record".into())
            })?;
            if record.attempts() >= maximum {
                return Ok(AttemptReservation::Exhausted(record.attempts()));
            }
            let attempt = record
                .attempts()
                .checked_add(1)
                .ok_or_else(|| TransportError::Configuration("attempt counter overflow".into()))?;
            *record = OutboxRecord::from_persisted(record.envelope().clone(), attempt);
            Ok(AttemptReservation::Reserved(attempt))
        })
    }

    fn acknowledge(
        &self,
        ack: DeliveryAck,
    ) -> StoreFuture<'_, sigil_rt::distributed::AcknowledgeOutcome> {
        Box::pin(async move {
            let id = ack.receipt().context().message_id().clone();
            let mut state = self
                .state
                .lock()
                .map_err(|_| TransportError::Unavailable("outbox lock poisoned".into()))?;
            let removed = state.pending.remove(&id).is_some();
            state.acknowledged.insert(id);
            Ok(if removed {
                AcknowledgeOutcome::Removed
            } else {
                AcknowledgeOutcome::AlreadyAcknowledged
            })
        })
    }
}

struct QueueTransport {
    session: NegotiatedTransport,
    tx: tokio::sync::mpsc::Sender<sigil_rt::distributed::WireEnvelope>,
}

impl Transport for QueueTransport {
    fn session(&self) -> &NegotiatedTransport {
        &self.session
    }

    fn admit<'a>(
        &'a self,
        envelope: sigil_rt::distributed::WireEnvelope,
        admission: RemoteAdmission,
    ) -> TransportFuture<'a> {
        Box::pin(async move {
            match admission {
                RemoteAdmission::Shed => match self.tx.try_send(envelope) {
                    Ok(()) => Ok(TransportOutcome::Accepted),
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                        Ok(TransportOutcome::Shed)
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                        Err(TransportError::Unavailable("wire queue closed".into()))
                    }
                },
                RemoteAdmission::Deadline(deadline) => {
                    match tokio::time::timeout(deadline, self.tx.send(envelope)).await {
                        Ok(Ok(())) => Ok(TransportOutcome::Accepted),
                        Ok(Err(_)) => {
                            Err(TransportError::Unavailable("wire queue closed".into()))
                        }
                        Err(_) => Ok(TransportOutcome::Shed),
                    }
                }
            }
        })
    }
}

#[derive(Default)]
struct SinkDurableState {
    total: i64,
    committed: BTreeMap<MessageId, DurableCommit>,
}

#[derive(Default)]
struct SinkCommitter {
    durable: Mutex<SinkDurableState>,
    restored: AtomicI64,
}

impl StateCommitter<Sink> for SinkCommitter {
    fn restore<'a>(&'a self, state: &'a mut Sink) -> StateRestoreFuture<'a> {
        let result = self
            .durable
            .lock()
            .map(|durable| {
                state.received = durable.total;
                self.restored.store(durable.total, Ordering::Release);
            })
            .map_err(|_| TransportError::Unavailable("state lock poisoned".into()));
        Box::pin(async move { result })
    }

    fn lookup<'a>(&'a self, context: &'a DeliveryContext) -> CommitLookupFuture<'a> {
        let id = context.message_id().clone();
        Box::pin(async move {
            Ok(self
                .durable
                .lock()
                .map_err(|_| TransportError::Unavailable("dedup lock poisoned".into()))?
                .committed
                .get(&id)
                .cloned()
                .map_or(CommitLookup::New, CommitLookup::Committed))
        })
    }

    fn commit<'a>(
        &'a self,
        context: &'a DeliveryContext,
        state: &'a Sink,
    ) -> StateCommitFuture<'a> {
        let id = context.message_id().clone();
        let total = state.received;
        Box::pin(async move {
            let commit = DurableCommit::new(format!("sink-{}", id.sequence()))?;
            let mut durable = self
                .durable
                .lock()
                .map_err(|_| TransportError::Unavailable("state lock poisoned".into()))?;
            durable.total = total;
            durable
                .committed
                .insert(id, commit.clone());
            Ok(commit)
        })
    }
}

fn applied(outcome: ReceiverOutcome) -> ApplyReceipt {
    match outcome {
        ReceiverOutcome::Applied(receipt) => receipt,
        ReceiverOutcome::Shed => panic!("receiver unexpectedly shed"),
    }
}

#[tokio::main]
async fn main() -> TransportResult<()> {
    let manifest = transport_manifest(4096)?;
    let session = NegotiatedTransport::negotiate(&manifest, &manifest)?;
    let destination = ShardAddress::new(
        ShardKey::new("prod", "core", "Sink", 0)?,
        NodeId::new("core-a")?,
        OwnershipEpoch::new(1)?,
    );
    let lease = ShardLease::serving(destination.clone());
    let (wire_tx, mut wire_rx) = tokio::sync::mpsc::channel(8);
    let transport = Arc::new(QueueTransport {
        session: session.clone(),
        tx: wire_tx,
    });
    let store = Arc::new(MemoryOutbox::default());
    let policy = RetryPolicy::new(4, Duration::from_millis(1), Duration::from_millis(8))?;
    let outbox = DurableOutbox::new(transport.clone(), store.clone(), policy)?;
    let endpoint = sigil_rt::distributed::RemoteEndpoint::<M>::new(
        transport,
        vec![destination],
        Arc::new(DurableIds(AtomicU64::new(0))),
        DeliverySemantics::AtLeastOnce,
        sigil_rt::distributed::CodecLimits::new(4096, 1024)
            .map_err(|error| TransportError::Configuration(error.to_string()))?,
    )
    .map_err(|error| TransportError::Configuration(error.to_string()))?;
    let endpoint = DurableRemoteEndpoint::new(endpoint, outbox.clone())
        .map_err(|error| TransportError::Configuration(error.to_string()))?;

    let committer = Arc::new(SinkCommitter::default());
    let core = PlacementComponent::start(
        "core",
        ComponentConfig::default(),
        RemoteEndpoints::default(),
        RemoteCommitters {
            sink: vec![committer.clone()],
        },
    )
    .await
    .map_err(|error| TransportError::Configuration(error.to_string()))?;
    let edge = PlacementComponent::start(
        "edge",
        ComponentConfig::default(),
        RemoteEndpoints {
            source_to_sink_m: Some(endpoint),
        },
        RemoteCommitters::default(),
    )
    .await
    .map_err(|error| TransportError::Configuration(error.to_string()))?;
    assert_eq!(core.health().expected_actors, 1);
    assert!(core.ingress_source().is_none());
    assert!(core.receiver_sink().is_some());
    assert_eq!(edge.health().expected_actors, 1);
    assert!(edge.ingress_source().is_some());
    assert!(edge.receiver_sink().is_none());

    edge.ingress_source()
        .expect("source is local")
        .round_robin()
        .send(M { value: 5 })
        .await
        .map_err(|error| TransportError::Unavailable(error.to_string()))?;
    let envelope = tokio::time::timeout(Duration::from_secs(1), wire_rx.recv())
        .await
        .map_err(|_| TransportError::Unavailable("wire receive timed out".into()))?
        .ok_or_else(|| TransportError::Unavailable("wire queue closed".into()))?;
    let duplicate_envelope = envelope.clone();
    let authorized = M::authorize_remote(
        &session,
        &lease,
        &envelope,
        sigil_rt::distributed::CodecLimits::new(4096, 1024)
            .map_err(|error| TransportError::Configuration(error.to_string()))?,
    )
    .map_err(|error| TransportError::Configuration(error.to_string()))?;
    assert_eq!(lease.in_flight(), 1);
    let receipt = applied(
        core.deliver_sink_m(
            authorized,
            RemoteAdmission::deadline(Duration::from_secs(1))?,
        )
        .await?,
    );
    assert_eq!(lease.in_flight(), 0);
    assert!(!receipt.is_duplicate());
    assert_eq!(committer.durable.lock().expect("total").total, 5);
    assert_eq!(
        outbox
            .acknowledge(DeliveryAck::from_receipt(receipt)?)
            .await?,
        AcknowledgeOutcome::Removed
    );
    assert!(store.pending(10).await?.is_empty());

    let _ = core.shutdown(Duration::from_secs(1)).await;
    let restarted = PlacementComponent::start(
        "core",
        ComponentConfig::default(),
        RemoteEndpoints::default(),
        RemoteCommitters {
            sink: vec![committer.clone()],
        },
    )
    .await
    .map_err(|error| TransportError::Configuration(error.to_string()))?;
    assert_eq!(committer.restored.load(Ordering::Acquire), 5);
    let duplicate = M::authorize_remote(
        &session,
        &lease,
        &duplicate_envelope,
        sigil_rt::distributed::CodecLimits::new(4096, 1024)
            .map_err(|error| TransportError::Configuration(error.to_string()))?,
    )
    .map_err(|error| TransportError::Configuration(error.to_string()))?;
    let duplicate_receipt = applied(
        restarted
            .deliver_sink_m(duplicate, RemoteAdmission::Shed)
            .await?,
    );
    assert!(duplicate_receipt.is_duplicate());
    assert_eq!(committer.durable.lock().expect("total").total, 5);
    assert_eq!(
        outbox
            .acknowledge(DeliveryAck::from_receipt(duplicate_receipt)?)
            .await?,
        AcknowledgeOutcome::AlreadyAcknowledged
    );

    edge.ingress_source()
        .expect("source is local")
        .round_robin()
        .send(M { value: 2 })
        .await
        .map_err(|error| TransportError::Unavailable(error.to_string()))?;
    let envelope = wire_rx
        .recv()
        .await
        .ok_or_else(|| TransportError::Unavailable("wire queue closed".into()))?;
    let authorized = M::authorize_remote(
        &session,
        &lease,
        &envelope,
        sigil_rt::distributed::CodecLimits::new(4096, 1024)
            .map_err(|error| TransportError::Configuration(error.to_string()))?,
    )
    .map_err(|error| TransportError::Configuration(error.to_string()))?;
    let receipt = applied(
        restarted
            .deliver_sink_m(authorized, RemoteAdmission::Shed)
            .await?,
    );
    assert_eq!(committer.durable.lock().expect("total").total, 7);
    outbox
        .acknowledge(DeliveryAck::from_receipt(receipt)?)
        .await?;

    let _ = edge.shutdown(Duration::from_secs(1)).await;
    let _ = restarted.shutdown(Duration::from_secs(1)).await;
    println!("placement-durable-ok");
    Ok(())
}
"#;

        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace");
        let output = workspace
            .join("target")
            .join(format!("placement-durable-{}", std::process::id()));
        let generated = GeneratedCrate {
            lib_rs: emit(&program, &graph),
            main_rs: Some(main_rs.into()),
            cargo_toml: emit_cargo_toml(
                "placement_durable",
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
        write_generated_crate(&output, &generated).expect("transactional placement generation");
        let result = Command::new("cargo")
            .args(["run", "--offline", "--quiet", "--bin", "demo"])
            .env(
                "CARGO_TARGET_DIR",
                workspace.join("target/placement-durable-cache"),
            )
            .current_dir(&output)
            .output()
            .expect("run generated placement component");
        assert!(
            result.status.success(),
            "generated placement component failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&result.stdout).trim(),
            "placement-durable-ok"
        );
    }

    assert_generated_placement_startup_hands_owned_permits_through_durable_receivers();
    let program = parse(source).expect("component program parses");
    let graph = lower(&program).expect("component program lowers");
    run_checks(&program, &graph, AssuranceLevel::Safe).expect("component program is safe");

    let main_rs = r#"use sigil_gen::{
    transport_manifest, Component, ComponentConfig, M, ProcessConfig, COMPONENT_PLACEMENT,
};
use sigil_rt::{Accounting, ActorTermination, SigilError};
use std::time::Duration;

fn expect_configuration_error(result: sigil_rt::Result<Component>, expected: &str) {
    match result {
        Err(SigilError::Configuration(message)) => assert!(
            message.contains(expected),
            "expected configuration error containing {expected:?}, got {message:?}"
        ),
        Err(other) => panic!("expected configuration error, got {other}"),
        Ok(_) => panic!("invalid component configuration unexpectedly started"),
    }
}

fn main() -> sigil_rt::Result<()> {
    COMPONENT_PLACEMENT
        .validate()
        .map_err(|error| SigilError::Configuration(error.to_string()))?;
    assert_eq!(COMPONENT_PLACEMENT.groups.len(), 2);
    assert_eq!(COMPONENT_PLACEMENT.remote_boundaries.len(), 1);
    let transport = transport_manifest(1024)
        .map_err(|error| SigilError::Configuration(error.to_string()))?;
    assert_eq!(
        transport.protocol().min(),
        sigil_rt::distributed::DISTRIBUTED_PROTOCOL_VERSION
    );
    assert!(transport.schemas().contains_key("M"));
    assert!(
        transport
            .delivery()
            .contains(&sigil_rt::distributed::DeliverySemantics::AtMostOnce)
    );

    // Startup outside Tokio is a typed error, not a tokio::spawn panic.
    expect_configuration_error(Component::start(ComponentConfig::default()), "Tokio runtime");

    // A current-thread runtime makes immediate admission saturation
    // deterministic: actors cannot consume until this future yields.
    {
        let admission_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| SigilError::Configuration(error.to_string()))?;
        admission_runtime.block_on(async {
            let mut config = ComponentConfig::default();
            config.source.inbox_capacity = 1;
            let component = Component::start(config)?;
            let ingress = component.ingress_source();
            assert_eq!(
                ingress.round_robin().admit_shed(M { value: 1 })?,
                sigil_rt::SendOutcome::Delivered
            );
            assert_eq!(
                ingress.round_robin().admit_shed(M { value: 2 })?,
                sigil_rt::SendOutcome::Shed
            );
            let reports = component.shutdown(Duration::from_secs(2)).await;
            assert!(
                reports
                    .iter()
                    .all(|report| report.termination == ActorTermination::Stopped)
            );

            let component = Component::start(ComponentConfig::default())?;
            assert_eq!(
                component
                    .ingress_source()
                    .by_key("stable-account")
                    .admit_deadline(M { value: 3 }, Duration::from_secs(1))
                    .await?,
                sigil_rt::SendOutcome::Delivered
            );
            let _ = component.shutdown(Duration::from_secs(2)).await;
            Ok::<(), SigilError>(())
        })?;
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|error| SigilError::Configuration(error.to_string()))?;

    runtime.block_on(async {
        let mut zero_shards = ComponentConfig::default();
        zero_shards.source.shards = 0;
        expect_configuration_error(Component::start(zero_shards), "at least one shard");

        let mut zero_capacity = ComponentConfig::default();
        zero_capacity.sink.inbox_capacity = 0;
        expect_configuration_error(Component::start(zero_capacity), "capacity must be at least 1");

        let mut overlarge_capacity = ComponentConfig::default();
        overlarge_capacity.source.inbox_capacity =
            tokio::sync::Semaphore::MAX_PERMITS.saturating_add(1);
        expect_configuration_error(
            Component::start(overlarge_capacity),
            "exceeds the runtime maximum",
        );

        let mut actor_count_overflow = ComponentConfig::default();
        actor_count_overflow.source.shards = usize::MAX;
        expect_configuration_error(
            Component::start(actor_count_overflow),
            "actor count overflows",
        );

        let config = ComponentConfig {
            source: ProcessConfig {
                shards: 2,
                inbox_capacity: 8,
            },
            sink: ProcessConfig {
                shards: 3,
                inbox_capacity: 8,
            },
        };
        let component = Component::start(config)?;
        assert_eq!(Component::START_ORDER, &["Sink", "Source"]);
        assert_eq!(Component::SHUTDOWN_ORDER, &["Source", "Sink"]);
        assert_eq!(Component::EDGES, &[("Source", "Sink")]);
        assert_eq!(component.ingress_source().len(), 2);
        assert_eq!(component.running().len(), 5);
        assert!(component.is_ready());
        let health = component.health();
        assert!(health.is_ready());
        assert_eq!(health.expected_actors, 5);
        assert_eq!(health.running_actors.len(), 5);
        assert_eq!(health.unavailable_actors(), 0);

        for value in 1..=12 {
            component
                .ingress_source()
                .round_robin()
                .send(M { value })
                .await?;
        }
        let reports = component.shutdown(Duration::from_secs(2)).await;
        assert_eq!(reports.len(), 5);
        assert!(
            reports
                .iter()
                .all(|report| report.termination == ActorTermination::Stopped)
        );
        let handled = reports
            .iter()
            .map(|report| match &report.accounting {
                Accounting::Complete(stats) => stats.handled,
                Accounting::Incomplete { .. } => panic!("clean shutdown lost accounting"),
            })
            .sum::<u64>();
        assert_eq!(handled, 24);

        // A caller-retained ingress clone cannot make shutdown hang forever.
        let deadline_component = Component::start(ComponentConfig::default())?;
        let retained_ingress = deadline_component
            .ingress_source()
            .round_robin()
            .clone();
        let reports = deadline_component
            .shutdown(Duration::from_millis(1))
            .await;
        assert!(
            reports
                .iter()
                .any(|report| report.termination == ActorTermination::ShutdownDeadline)
        );
        drop(retained_ingress);

        // Panics remain observable while the rest of the component is alive.
        let mut panic_component = Component::start(ComponentConfig::default())?;
        std::env::set_var("SIGIL_CHAOS_PANIC_AT", "before_send");
        panic_component
            .ingress_source()
            .round_robin()
            .send(M { value: 1 })
            .await?;
        let panic_report = panic_component
            .next_event()
            .await
            .expect("panicked source must produce a live event");
        std::env::remove_var("SIGIL_CHAOS_PANIC_AT");
        assert_eq!(panic_report.name, "Source[0]");
        assert_eq!(panic_report.termination, ActorTermination::Panicked);
        let degraded = panic_component.health();
        assert!(!degraded.is_ready());
        assert_eq!(degraded.unavailable_actors(), 1);
        let remaining = panic_component.shutdown(Duration::from_secs(2)).await;
        assert!(
            remaining
                .iter()
                .all(|report| report.termination == ActorTermination::Stopped)
        );

        println!("component-assembly-ok");
        Ok::<(), SigilError>(())
    })
}
"#;

    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace");
    let output = workspace
        .join("target")
        .join(format!("component-assembly-{}", std::process::id()));
    let generated = GeneratedCrate {
        lib_rs: emit(&program, &graph),
        main_rs: Some(main_rs.into()),
        cargo_toml: emit_cargo_toml(
            "component_assembly",
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
    write_generated_crate(&output, &generated).expect("transactional component generation");
    let result = Command::new("cargo")
        .args(["run", "--offline", "--quiet", "--bin", "demo"])
        .env(
            "CARGO_TARGET_DIR",
            workspace.join("target/component-assembly-cache"),
        )
        .env_remove("SIGIL_CHAOS_PANIC_AT")
        .current_dir(&output)
        .output()
        .expect("run generated component");
    assert!(
        result.status.success(),
        "generated component failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&result.stdout).trim(),
        "component-assembly-ok"
    );
}
