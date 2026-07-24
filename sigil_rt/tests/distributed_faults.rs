use sigil_rt::distributed::{
    CodecLimits, CodecResult, DedupDecision, DedupWindow, DeliverySemantics, HandoffBundle,
    LeasePhase, MessageId, NegotiatedTransport, NodeId, OwnershipEpoch, OwnershipError,
    RemoteAdmission, RemoteEndpoint, SchemaFingerprint, ShardAddress, ShardCheckpoint, ShardKey,
    ShardLease, Transport, TransportError, TransportFuture, TransportManifest, TransportOutcome,
    VersionRange, VolatileMessageIds, WireCodec, WireDecoder, WireEncoder, WireEnvelope,
    DISTRIBUTED_PROTOCOL_VERSION,
};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Order {
    sequence: i64,
}

impl WireCodec for Order {
    const SCHEMA: &'static str = "Order";
    const VERSION: u32 = 1;
    const FINGERPRINT: SchemaFingerprint = SchemaFingerprint::new([9; 32]);

    fn encode_wire(&self, encoder: &mut WireEncoder) -> CodecResult<()> {
        encoder.write_i64(self.sequence)
    }

    fn decode_wire(decoder: &mut WireDecoder<'_>) -> CodecResult<Self> {
        Ok(Self {
            sequence: decoder.read_i64()?,
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum Fault {
    Deliver,
    DropAfterAccept,
    Duplicate,
    Hold,
    Partition,
}

#[derive(Debug)]
struct NetworkState {
    faults: VecDeque<Fault>,
    ready: VecDeque<WireEnvelope>,
    held: Vec<WireEnvelope>,
}

/// Deterministic multi-node harness. It is intentionally test-only: production
/// transports must implement the same public `Transport` contract.
#[derive(Debug)]
struct DeterministicTransport {
    session: NegotiatedTransport,
    capacity: usize,
    state: Mutex<NetworkState>,
}

impl DeterministicTransport {
    fn new(session: NegotiatedTransport, capacity: usize, faults: Vec<Fault>) -> Self {
        Self {
            session,
            capacity,
            state: Mutex::new(NetworkState {
                faults: faults.into(),
                ready: VecDeque::new(),
                held: Vec::new(),
            }),
        }
    }

    fn release_held_in_reverse(&self) {
        let mut state = self.state.lock().expect("fault harness lock");
        while let Some(envelope) = state.held.pop() {
            state.ready.push_back(envelope);
        }
    }

    fn drain(&self) -> Vec<WireEnvelope> {
        self.state
            .lock()
            .expect("fault harness lock")
            .ready
            .drain(..)
            .collect()
    }
}

impl Transport for DeterministicTransport {
    fn session(&self) -> &NegotiatedTransport {
        &self.session
    }

    fn admit<'a>(
        &'a self,
        envelope: WireEnvelope,
        admission: RemoteAdmission,
    ) -> TransportFuture<'a> {
        Box::pin(async move {
            admission.validate()?;
            self.session.validate_envelope(&envelope)?;
            let mut state = self.state.lock().map_err(|_| {
                TransportError::Unavailable("fault harness state was poisoned".into())
            })?;
            if state.ready.len().saturating_add(state.held.len()) >= self.capacity {
                return Ok(TransportOutcome::Shed);
            }
            match state.faults.pop_front().unwrap_or(Fault::Deliver) {
                Fault::Deliver => state.ready.push_back(envelope),
                Fault::DropAfterAccept => {}
                Fault::Duplicate => {
                    state.ready.push_back(envelope.clone());
                    state.ready.push_back(envelope);
                }
                Fault::Hold => state.held.push(envelope),
                Fault::Partition => {
                    return Err(TransportError::Unavailable(
                        "deterministic network partition".into(),
                    ));
                }
            }
            Ok(TransportOutcome::Accepted)
        })
    }
}

fn manifest() -> TransportManifest {
    let mut manifest = TransportManifest::new(
        VersionRange::exact(DISTRIBUTED_PROTOCOL_VERSION).expect("protocol"),
        1024,
    )
    .expect("manifest");
    manifest
        .register_schema("Order", 1, Order::FINGERPRINT)
        .expect("register schema");
    manifest.enable_delivery(DeliverySemantics::AtMostOnce);
    manifest.enable_delivery(DeliverySemantics::AtLeastOnce);
    manifest
}

fn address(node: &NodeId, epoch: u64) -> ShardAddress {
    ShardAddress::new(
        ShardKey::new("orders-prod", "workers", "OrderBook", 7).expect("shard key"),
        node.clone(),
        OwnershipEpoch::new(epoch).expect("epoch"),
    )
}

fn envelope(
    session: &NegotiatedTransport,
    destination: ShardAddress,
    sequence: u64,
) -> WireEnvelope {
    session
        .envelope(
            destination,
            MessageId::new("producer-session-a", sequence).expect("message id"),
            "Order",
            DeliverySemantics::AtLeastOnce,
            vec![sequence as u8],
        )
        .expect("valid envelope")
}

#[tokio::test]
async fn deterministic_faults_expose_loss_duplication_reordering_and_partition() {
    let session =
        NegotiatedTransport::negotiate(&manifest(), &manifest()).expect("negotiated session");
    let node = NodeId::new("node-a").expect("node");
    let transport = DeterministicTransport::new(
        session.clone(),
        16,
        vec![
            Fault::Duplicate,
            Fault::Hold,
            Fault::Deliver,
            Fault::DropAfterAccept,
            Fault::Partition,
        ],
    );

    for sequence in 1..=4 {
        assert_eq!(
            transport
                .admit(
                    envelope(&session, address(&node, 1), sequence),
                    RemoteAdmission::Shed,
                )
                .await
                .expect("transport admission"),
            TransportOutcome::Accepted
        );
    }
    assert!(matches!(
        transport
            .admit(
                envelope(&session, address(&node, 1), 5),
                RemoteAdmission::deadline(std::time::Duration::from_millis(10))
                    .expect("bounded deadline"),
            )
            .await,
        Err(TransportError::Unavailable(_))
    ));

    transport.release_held_in_reverse();
    let delivered = transport.drain();
    assert_eq!(
        delivered
            .iter()
            .map(|item| item.message_id().sequence())
            .collect::<Vec<_>>(),
        vec![1, 1, 3, 2],
        "duplicate and reordered delivery must be explicit in the harness"
    );

    let mut dedup = DedupWindow::new(16).expect("dedup window");
    let applied = delivered
        .into_iter()
        .filter(|item| dedup.observe(item.message_id().clone()) == DedupDecision::FirstSeen)
        .map(|item| item.message_id().sequence())
        .collect::<Vec<_>>();
    assert_eq!(applied, vec![1, 3, 2]);
    assert!(
        !dedup.snapshot().iter().any(|id| id.sequence() == 4),
        "transport acceptance is not evidence of remote delivery"
    );
}

#[tokio::test]
async fn remote_admission_sheds_at_the_configured_capacity() {
    let session =
        NegotiatedTransport::negotiate(&manifest(), &manifest()).expect("negotiated session");
    let node = NodeId::new("node-a").expect("node");
    let transport = DeterministicTransport::new(session.clone(), 1, vec![Fault::Deliver]);

    assert_eq!(
        transport
            .admit(
                envelope(&session, address(&node, 1), 1),
                RemoteAdmission::Shed,
            )
            .await
            .expect("first admission"),
        TransportOutcome::Accepted
    );
    assert_eq!(
        transport
            .admit(
                envelope(&session, address(&node, 1), 2),
                RemoteAdmission::Shed,
            )
            .await
            .expect("bounded shed"),
        TransportOutcome::Shed
    );
}

#[tokio::test]
async fn typed_endpoint_survives_duplicate_and_reordered_wire_delivery() {
    let session =
        NegotiatedTransport::negotiate(&manifest(), &manifest()).expect("negotiated session");
    let node = NodeId::new("node-a").expect("node");
    let transport = Arc::new(DeterministicTransport::new(
        session.clone(),
        16,
        vec![Fault::Duplicate, Fault::Hold, Fault::Deliver],
    ));
    let limits = CodecLimits::new(64, 64).expect("limits");
    let endpoint = RemoteEndpoint::<Order>::new(
        transport.clone(),
        vec![address(&node, 1)],
        Arc::new(VolatileMessageIds::new("wire-session", 1).expect("message ids")),
        DeliverySemantics::AtMostOnce,
        limits,
    )
    .expect("typed endpoint");

    for sequence in [10, 20, 30] {
        assert_eq!(
            endpoint
                .admit_round_robin(&Order { sequence }, RemoteAdmission::Shed)
                .await
                .expect("admission"),
            TransportOutcome::Accepted
        );
    }
    transport.release_held_in_reverse();
    let delivered = transport.drain();
    assert_eq!(
        delivered
            .iter()
            .map(|envelope| {
                Order::decode_bounded(envelope.payload(), limits)
                    .expect("bounded typed decode")
                    .sequence
            })
            .collect::<Vec<_>>(),
        vec![10, 10, 30, 20]
    );
    assert_eq!(
        delivered
            .iter()
            .map(|envelope| envelope.message_id().sequence())
            .collect::<Vec<_>>(),
        vec![1, 1, 3, 2],
        "duplicates retain one identity and reordering retains producer order metadata"
    );
}

#[test]
fn epoch_fencing_requires_drain_before_checkpoint_and_activation_after_restore() {
    let node_a = NodeId::new("node-a").expect("node");
    let node_b = NodeId::new("node-b").expect("node");
    let source_address = address(&node_a, 1);
    let source = ShardLease::serving(source_address.clone());

    let permit = source
        .authorize(&source_address)
        .expect("current epoch is serving");
    let plan = source
        .begin_drain(node_b.clone(), OwnershipEpoch::new(2).expect("epoch"))
        .expect("begin drain");
    assert_eq!(source.phase(), LeasePhase::Draining);
    assert!(matches!(
        source.authorize(&source_address),
        Err(OwnershipError::NotServing(LeasePhase::Draining))
    ));
    let checkpoint = ShardCheckpoint::new(
        1,
        "checkpoint-42",
        vec![4, 2],
        vec![MessageId::new("producer-session-a", 9).expect("message")],
    )
    .expect("checkpoint");
    assert!(matches!(
        source.complete_handoff(plan.clone(), checkpoint.clone()),
        Err(OwnershipError::InFlight(1))
    ));

    drop(permit);
    let bundle = source
        .complete_handoff(plan, checkpoint)
        .expect("source is fully drained");
    assert_eq!(source.phase(), LeasePhase::Retired);
    assert!(matches!(
        source.authorize(&source_address),
        Err(OwnershipError::NotServing(LeasePhase::Retired))
    ));

    let (persisted_source, persisted_destination, persisted_checkpoint) =
        bundle.into_persisted_parts();
    let bundle = HandoffBundle::from_persisted_parts(
        persisted_source,
        persisted_destination,
        persisted_checkpoint,
    )
    .expect("authenticated persisted handoff");
    let (successor, restored) =
        ShardLease::receive_handoff(bundle, &node_b).expect("receive handoff");
    assert_eq!(successor.phase(), LeasePhase::Pending);
    assert_eq!(restored.state(), &[4, 2]);
    assert!(matches!(
        successor.authorize(successor.address()),
        Err(OwnershipError::NotServing(LeasePhase::Pending))
    ));

    // The application restores state and the dedup frontier before opening
    // the data-plane gate.
    let _dedup = DedupWindow::restore(16, restored.deduplication().to_vec())
        .expect("restore deduplication frontier");
    successor.activate().expect("activate restored shard");
    let _new_epoch_permit = successor
        .authorize(successor.address())
        .expect("new epoch serves");

    let stale_for_new_owner = ShardAddress::new(
        successor.address().key().clone(),
        node_b,
        OwnershipEpoch::new(1).expect("old epoch"),
    );
    assert!(matches!(
        successor.authorize(&stale_for_new_owner),
        Err(OwnershipError::StaleEpoch {
            expected: 2,
            actual: 1,
        })
    ));
}

#[test]
fn cancelled_drain_reopens_only_the_original_epoch() {
    let node = NodeId::new("node-a").expect("node");
    let lease = ShardLease::serving(address(&node, 4));
    let plan = lease
        .begin_drain(node.clone(), OwnershipEpoch::new(5).expect("epoch"))
        .expect("begin drain");
    lease.cancel_drain(&plan).expect("cancel drain");
    assert_eq!(lease.phase(), LeasePhase::Serving);
    let _permit = lease
        .authorize(lease.address())
        .expect("original epoch resumes");
}
