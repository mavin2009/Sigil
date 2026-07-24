//! Transport and shard-ownership contracts for deployments that cross a
//! process or host boundary.
//!
//! This module deliberately does not ship a network transport. It defines the
//! invariants a transport adapter must preserve so generated components do not
//! confuse "accepted by a local queue" with "durably handled by a remote
//! actor".

use crate::ROUTING_HASH_VERSION;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

mod codec;
mod reliable;

pub use codec::{
    authorize_and_decode, decode_message, encode_message, AuthorizedMessage, CodecError,
    CodecLimits, CodecResult, MessageIdDurability, MessageIdFuture, MessageIdSource,
    RemoteEndpoint, RemoteMessageError, RemoteMessageResult, VolatileMessageIds, WireCodec,
    WireDecoder, WireEncoder,
};
pub use reliable::{
    AcknowledgeOutcome, ApplyDurability, ApplyReceipt, AttemptReservation, CommitLookup,
    CommitLookupFuture, DeliveryAck, DeliveryContext, DispatchOutcome, DispatchReport,
    DurableCommit, DurableOutbox, DurableRemoteEndpoint, OutboxRecord, OutboxStore, PersistOutcome,
    ReceiverOutcome, RetryPolicy, StateCommitFuture, StateCommitter, StateRestoreFuture,
    StoreFuture,
};

/// Version of Sigil's transport-level envelope and negotiation contract.
pub const DISTRIBUTED_PROTOCOL_VERSION: u32 = 1;

const LEASE_PENDING: u8 = 0;
const LEASE_SERVING: u8 = 1;
const LEASE_DRAINING: u8 = 2;
const LEASE_RETIRED: u8 = 3;
const LEASE_PHASE_SHIFT: u32 = 62;
const LEASE_COUNT_MASK: u64 = (1_u64 << LEASE_PHASE_SHIFT) - 1;
const MAX_ID_BYTES: usize = 255;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TransportError {
    #[error("invalid distributed configuration: {0}")]
    Configuration(String),
    #[error(
        "no common transport protocol (local {local_min}..={local_max}, remote {remote_min}..={remote_max})"
    )]
    NoCommonProtocol {
        local_min: u32,
        local_max: u32,
        remote_min: u32,
        remote_max: u32,
    },
    #[error("routing-hash mismatch (local {local}, remote {remote})")]
    RoutingHashMismatch { local: u32, remote: u32 },
    #[error("no common delivery semantics")]
    NoCommonDeliverySemantics,
    #[error("transport schema '{0}' was not negotiated")]
    UnknownSchema(String),
    #[error("transport schema '{0}' has no mutually supported version")]
    IncompatibleSchema(String),
    #[error(
        "transport schema '{schema}' has common version {version}, but its structural fingerprints differ"
    )]
    SchemaFingerprintMismatch { schema: String, version: u32 },
    #[error("schema '{schema}' version {actual} does not match negotiated version {expected}")]
    SchemaVersionMismatch {
        schema: String,
        expected: u32,
        actual: u32,
    },
    #[error("transport envelope protocol {actual} does not match negotiated protocol {expected}")]
    ProtocolMismatch { expected: u32, actual: u32 },
    #[error("delivery semantics {0:?} were not negotiated")]
    DeliverySemanticsMismatch(DeliverySemantics),
    #[error("payload is {actual} bytes, exceeding the negotiated limit of {maximum}")]
    PayloadTooLarge { actual: usize, maximum: usize },
    #[error("remote transport is unavailable: {0}")]
    Unavailable(String),
}

pub type TransportResult<T> = std::result::Result<T, TransportError>;

fn validate_identifier(kind: &str, value: &str) -> TransportResult<()> {
    if value.is_empty() {
        return Err(TransportError::Configuration(format!(
            "{kind} must not be empty"
        )));
    }
    if value.len() > MAX_ID_BYTES {
        return Err(TransportError::Configuration(format!(
            "{kind} is {} bytes, exceeding the limit of {MAX_ID_BYTES}",
            value.len()
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(TransportError::Configuration(format!(
            "{kind} must not contain control characters"
        )));
    }
    Ok(())
}

/// Inclusive version interval advertised during transport negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionRange {
    min: u32,
    max: u32,
}

impl VersionRange {
    pub fn new(min: u32, max: u32) -> TransportResult<Self> {
        if min == 0 || min > max {
            return Err(TransportError::Configuration(format!(
                "invalid version range {min}..={max}"
            )));
        }
        Ok(Self { min, max })
    }

    pub fn exact(version: u32) -> TransportResult<Self> {
        Self::new(version, version)
    }

    pub fn min(self) -> u32 {
        self.min
    }

    pub fn max(self) -> u32 {
        self.max
    }

    fn highest_common(self, other: Self) -> Option<u32> {
        let minimum = self.min.max(other.min);
        let maximum = self.max.min(other.max);
        (minimum <= maximum).then_some(maximum)
    }
}

/// SHA-256 structural identity of one exact schema version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SchemaFingerprint([u8; 32]);

impl SchemaFingerprint {
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Delivery properties a transport adapter can actually provide.
///
/// Exactly-once is intentionally absent. At-least-once delivery can duplicate
/// messages and therefore requires receiver-side deduplication or idempotent
/// effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeliverySemantics {
    AtMostOnce,
    AtLeastOnce,
}

/// Capabilities exchanged before a transport session accepts traffic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportManifest {
    protocol: VersionRange,
    routing_hash_version: u32,
    schemas: BTreeMap<String, BTreeMap<u32, SchemaFingerprint>>,
    delivery: BTreeSet<DeliverySemantics>,
    max_payload_bytes: usize,
}

impl TransportManifest {
    pub fn new(protocol: VersionRange, max_payload_bytes: usize) -> TransportResult<Self> {
        if max_payload_bytes == 0 {
            return Err(TransportError::Configuration(
                "transport payload limit must be at least one byte".into(),
            ));
        }
        Ok(Self {
            protocol,
            routing_hash_version: ROUTING_HASH_VERSION,
            schemas: BTreeMap::new(),
            delivery: BTreeSet::new(),
            max_payload_bytes,
        })
    }

    pub fn register_schema(
        &mut self,
        schema: impl Into<String>,
        version: u32,
        fingerprint: SchemaFingerprint,
    ) -> TransportResult<()> {
        let schema = schema.into();
        validate_identifier("transport schema name", &schema)?;
        if version == 0 {
            return Err(TransportError::Configuration(
                "transport schema version must be greater than zero".into(),
            ));
        }
        if fingerprint.as_bytes().iter().all(|byte| *byte == 0) {
            return Err(TransportError::Configuration(
                "transport schema fingerprint must not be all zeroes".into(),
            ));
        }
        if self
            .schemas
            .entry(schema.clone())
            .or_default()
            .insert(version, fingerprint)
            .is_some()
        {
            return Err(TransportError::Configuration(format!(
                "transport schema '{schema}' version {version} is registered more than once"
            )));
        }
        Ok(())
    }

    pub fn enable_delivery(&mut self, semantics: DeliverySemantics) {
        self.delivery.insert(semantics);
    }

    pub fn protocol(&self) -> VersionRange {
        self.protocol
    }

    pub fn routing_hash_version(&self) -> u32 {
        self.routing_hash_version
    }

    pub fn schemas(&self) -> &BTreeMap<String, BTreeMap<u32, SchemaFingerprint>> {
        &self.schemas
    }

    pub fn delivery(&self) -> &BTreeSet<DeliverySemantics> {
        &self.delivery
    }

    pub fn max_payload_bytes(&self) -> usize {
        self.max_payload_bytes
    }

    #[cfg(test)]
    fn with_routing_hash_version(mut self, version: u32) -> Self {
        self.routing_hash_version = version;
        self
    }
}

/// Immutable result of a successful two-sided capability negotiation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegotiatedTransport {
    protocol: u32,
    schemas: BTreeMap<String, u32>,
    schema_fingerprints: BTreeMap<String, SchemaFingerprint>,
    delivery: BTreeSet<DeliverySemantics>,
    max_payload_bytes: usize,
}

impl NegotiatedTransport {
    pub fn negotiate(
        local: &TransportManifest,
        remote: &TransportManifest,
    ) -> TransportResult<Self> {
        if local.routing_hash_version != remote.routing_hash_version {
            return Err(TransportError::RoutingHashMismatch {
                local: local.routing_hash_version,
                remote: remote.routing_hash_version,
            });
        }
        let protocol = local.protocol.highest_common(remote.protocol).ok_or(
            TransportError::NoCommonProtocol {
                local_min: local.protocol.min,
                local_max: local.protocol.max,
                remote_min: remote.protocol.min,
                remote_max: remote.protocol.max,
            },
        )?;
        let delivery = local
            .delivery
            .intersection(&remote.delivery)
            .copied()
            .collect::<BTreeSet<_>>();
        if delivery.is_empty() {
            return Err(TransportError::NoCommonDeliverySemantics);
        }
        let schema_names = local
            .schemas
            .keys()
            .chain(remote.schemas.keys())
            .collect::<BTreeSet<_>>();
        let mut schemas = BTreeMap::new();
        let mut schema_fingerprints = BTreeMap::new();
        for name in schema_names {
            let (local_versions, remote_versions) = local
                .schemas
                .get(name)
                .zip(remote.schemas.get(name))
                .ok_or_else(|| TransportError::IncompatibleSchema(name.clone()))?;
            let common_versions = local_versions
                .keys()
                .filter(|version| remote_versions.contains_key(version))
                .copied()
                .collect::<BTreeSet<_>>();
            let compatible = common_versions.iter().rev().find_map(|version| {
                let local_fingerprint = local_versions.get(version)?;
                let remote_fingerprint = remote_versions.get(version)?;
                (local_fingerprint == remote_fingerprint).then_some((*version, *local_fingerprint))
            });
            let Some((version, fingerprint)) = compatible else {
                if let Some(version) = common_versions.last().copied() {
                    return Err(TransportError::SchemaFingerprintMismatch {
                        schema: name.clone(),
                        version,
                    });
                }
                return Err(TransportError::IncompatibleSchema(name.clone()));
            };
            schemas.insert(name.clone(), version);
            schema_fingerprints.insert(name.clone(), fingerprint);
        }
        Ok(Self {
            protocol,
            schemas,
            schema_fingerprints,
            delivery,
            max_payload_bytes: local.max_payload_bytes.min(remote.max_payload_bytes),
        })
    }

    pub fn protocol(&self) -> u32 {
        self.protocol
    }

    pub fn schemas(&self) -> &BTreeMap<String, u32> {
        &self.schemas
    }

    pub fn schema_fingerprint(&self, schema: &str) -> Option<SchemaFingerprint> {
        self.schema_fingerprints.get(schema).copied()
    }

    pub fn delivery(&self) -> &BTreeSet<DeliverySemantics> {
        &self.delivery
    }

    pub fn max_payload_bytes(&self) -> usize {
        self.max_payload_bytes
    }

    pub fn validate_envelope(&self, envelope: &WireEnvelope) -> TransportResult<()> {
        if envelope.protocol != self.protocol {
            return Err(TransportError::ProtocolMismatch {
                expected: self.protocol,
                actual: envelope.protocol,
            });
        }
        let expected = self
            .schemas
            .get(&envelope.schema)
            .copied()
            .ok_or_else(|| TransportError::UnknownSchema(envelope.schema.clone()))?;
        if envelope.schema_version != expected {
            return Err(TransportError::SchemaVersionMismatch {
                schema: envelope.schema.clone(),
                expected,
                actual: envelope.schema_version,
            });
        }
        if !self.delivery.contains(&envelope.delivery) {
            return Err(TransportError::DeliverySemanticsMismatch(envelope.delivery));
        }
        if envelope.payload.len() > self.max_payload_bytes {
            return Err(TransportError::PayloadTooLarge {
                actual: envelope.payload.len(),
                maximum: self.max_payload_bytes,
            });
        }
        Ok(())
    }

    /// Construct an envelope using this session's exact negotiated versions.
    pub fn envelope(
        &self,
        destination: ShardAddress,
        message_id: MessageId,
        schema: impl Into<String>,
        delivery: DeliverySemantics,
        payload: Vec<u8>,
    ) -> TransportResult<WireEnvelope> {
        let schema = schema.into();
        let schema_version = self
            .schemas
            .get(&schema)
            .copied()
            .ok_or_else(|| TransportError::UnknownSchema(schema.clone()))?;
        self.receive_envelope(
            self.protocol,
            destination,
            message_id,
            schema,
            schema_version,
            delivery,
            payload,
        )
    }

    /// Reconstruct and validate fields decoded by a network adapter. The
    /// adapter should enforce its byte-level allocation limit before creating
    /// `payload`; this method enforces the negotiated semantic limit.
    #[allow(clippy::too_many_arguments)]
    pub fn receive_envelope(
        &self,
        protocol: u32,
        destination: ShardAddress,
        message_id: MessageId,
        schema: impl Into<String>,
        schema_version: u32,
        delivery: DeliverySemantics,
        payload: Vec<u8>,
    ) -> TransportResult<WireEnvelope> {
        let schema = schema.into();
        validate_identifier("transport schema name", &schema)?;
        let envelope = WireEnvelope {
            protocol,
            destination,
            message_id,
            schema,
            schema_version,
            delivery,
            payload,
        };
        self.validate_envelope(&envelope)?;
        Ok(envelope)
    }
}

/// Stable node identity. Deployments must assign a new identity when a
/// producer loses the durable sequence state behind its message IDs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(String);

impl NodeId {
    pub fn new(value: impl Into<String>) -> TransportResult<Self> {
        let value = value.into();
        validate_identifier("node identity", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Monotonically increasing fencing token for one logical shard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OwnershipEpoch(u64);

impl OwnershipEpoch {
    pub fn new(value: u64) -> TransportResult<Self> {
        if value == 0 {
            return Err(TransportError::Configuration(
                "shard ownership epoch must be greater than zero".into(),
            ));
        }
        Ok(Self(value))
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

/// Logical shard identity, independent of its current owner.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ShardKey {
    deployment: String,
    placement_group: String,
    process: String,
    shard: u64,
}

impl ShardKey {
    pub fn new(
        deployment: impl Into<String>,
        placement_group: impl Into<String>,
        process: impl Into<String>,
        shard: u64,
    ) -> TransportResult<Self> {
        let deployment = deployment.into();
        let placement_group = placement_group.into();
        let process = process.into();
        validate_identifier("deployment identity", &deployment)?;
        validate_identifier("placement-group name", &placement_group)?;
        validate_identifier("process name", &process)?;
        Ok(Self {
            deployment,
            placement_group,
            process,
            shard,
        })
    }

    pub fn deployment(&self) -> &str {
        &self.deployment
    }

    pub fn placement_group(&self) -> &str {
        &self.placement_group
    }

    pub fn process(&self) -> &str {
        &self.process
    }

    pub fn shard(&self) -> u64 {
        self.shard
    }
}

/// Fully fenced destination placed on every remote envelope.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ShardAddress {
    key: ShardKey,
    owner: NodeId,
    epoch: OwnershipEpoch,
}

impl ShardAddress {
    pub fn new(key: ShardKey, owner: NodeId, epoch: OwnershipEpoch) -> Self {
        Self { key, owner, epoch }
    }

    pub fn key(&self) -> &ShardKey {
        &self.key
    }

    pub fn owner(&self) -> &NodeId {
        &self.owner
    }

    pub fn epoch(&self) -> OwnershipEpoch {
        self.epoch
    }
}

/// Producer-session identity plus a durable, monotonically increasing
/// sequence number.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MessageId {
    producer: String,
    sequence: u64,
}

impl MessageId {
    pub fn new(producer: impl Into<String>, sequence: u64) -> TransportResult<Self> {
        let producer = producer.into();
        validate_identifier("message producer identity", &producer)?;
        Ok(Self { producer, sequence })
    }

    pub fn producer(&self) -> &str {
        &self.producer
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }
}

/// Transport-neutral wire payload and all metadata required to validate it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireEnvelope {
    protocol: u32,
    destination: ShardAddress,
    message_id: MessageId,
    schema: String,
    schema_version: u32,
    delivery: DeliverySemantics,
    payload: Vec<u8>,
}

impl WireEnvelope {
    pub fn protocol(&self) -> u32 {
        self.protocol
    }

    pub fn destination(&self) -> &ShardAddress {
        &self.destination
    }

    pub fn message_id(&self) -> &MessageId {
        &self.message_id
    }

    pub fn schema(&self) -> &str {
        &self.schema
    }

    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    pub fn delivery(&self) -> DeliverySemantics {
        self.delivery
    }

    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    pub fn into_payload(self) -> Vec<u8> {
        self.payload
    }
}

/// Remote admission is always bounded. An unbounded cross-host wait would
/// let a partition consume producers indefinitely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteAdmission {
    Shed,
    Deadline(Duration),
}

impl RemoteAdmission {
    pub fn deadline(duration: Duration) -> TransportResult<Self> {
        if duration.is_zero() {
            return Err(TransportError::Configuration(
                "remote admission deadline must be greater than zero".into(),
            ));
        }
        Ok(Self::Deadline(duration))
    }

    pub fn validate(self) -> TransportResult<()> {
        match self {
            Self::Shed => Ok(()),
            Self::Deadline(duration) if !duration.is_zero() => Ok(()),
            Self::Deadline(_) => Err(TransportError::Configuration(
                "remote admission deadline must be greater than zero".into(),
            )),
        }
    }
}

/// `Accepted` means accepted by the bounded transport, not handled or
/// durably committed by the destination actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportOutcome {
    Accepted,
    Shed,
    Duplicate,
}

pub type TransportFuture<'a> =
    Pin<Box<dyn Future<Output = TransportResult<TransportOutcome>> + Send + 'a>>;

/// Network implementations live outside `sigil_rt`; this is the contract
/// they implement and generated integrations consume.
pub trait Transport: Send + Sync {
    fn session(&self) -> &NegotiatedTransport;

    fn admit<'a>(
        &'a self,
        envelope: WireEnvelope,
        admission: RemoteAdmission,
    ) -> TransportFuture<'a>;
}

/// Bounded exact duplicate window. Eviction permits an older message ID to be
/// accepted again, so the configured capacity and producer retry horizon are
/// one operational contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DedupWindow {
    capacity: usize,
    order: VecDeque<MessageId>,
    seen: BTreeSet<MessageId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupDecision {
    FirstSeen,
    Duplicate,
}

impl DedupWindow {
    pub fn new(capacity: usize) -> TransportResult<Self> {
        if capacity == 0 {
            return Err(TransportError::Configuration(
                "deduplication window must hold at least one message identity".into(),
            ));
        }
        Ok(Self {
            capacity,
            order: VecDeque::new(),
            seen: BTreeSet::new(),
        })
    }

    pub fn observe(&mut self, id: MessageId) -> DedupDecision {
        if self.seen.contains(&id) {
            return DedupDecision::Duplicate;
        }
        if self.order.len() == self.capacity {
            if let Some(evicted) = self.order.pop_front() {
                self.seen.remove(&evicted);
            }
        }
        self.seen.insert(id.clone());
        self.order.push_back(id);
        DedupDecision::FirstSeen
    }

    pub fn snapshot(&self) -> Vec<MessageId> {
        self.order.iter().cloned().collect()
    }

    pub fn restore(capacity: usize, ordered_ids: Vec<MessageId>) -> TransportResult<Self> {
        let mut window = Self::new(capacity)?;
        if ordered_ids.len() > capacity {
            return Err(TransportError::Configuration(format!(
                "deduplication snapshot contains {} identities but capacity is {capacity}",
                ordered_ids.len()
            )));
        }
        for id in ordered_ids {
            if window.observe(id) == DedupDecision::Duplicate {
                return Err(TransportError::Configuration(
                    "deduplication snapshot contains duplicate identities".into(),
                ));
            }
        }
        Ok(window)
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum OwnershipError {
    #[error("delivery targets a different shard or owner")]
    WrongDestination,
    #[error("stale shard epoch {actual}; current epoch is {expected}")]
    StaleEpoch { expected: u64, actual: u64 },
    #[error("shard is not serving (phase {0:?})")]
    NotServing(LeasePhase),
    #[error("invalid ownership transition from {actual:?}; expected {expected:?}")]
    InvalidTransition {
        expected: LeasePhase,
        actual: LeasePhase,
    },
    #[error("next ownership epoch {next} must be greater than current epoch {current}")]
    NonIncreasingEpoch { current: u64, next: u64 },
    #[error("handoff plan does not belong to this shard lease")]
    WrongHandoff,
    #[error("{0} ownership permits are still live; the shard is not drained")]
    InFlight(u64),
    #[error("ownership permit count overflow")]
    PermitOverflow,
}

pub type OwnershipResult<T> = std::result::Result<T, OwnershipError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeasePhase {
    Pending,
    Serving,
    Draining,
    Retired,
}

fn decode_phase(phase: u8) -> LeasePhase {
    match phase {
        LEASE_PENDING => LeasePhase::Pending,
        LEASE_SERVING => LeasePhase::Serving,
        LEASE_DRAINING => LeasePhase::Draining,
        _ => LeasePhase::Retired,
    }
}

fn lease_state(phase: u8, in_flight: u64) -> u64 {
    (u64::from(phase) << LEASE_PHASE_SHIFT) | in_flight
}

fn state_phase(state: u64) -> u8 {
    (state >> LEASE_PHASE_SHIFT) as u8
}

fn state_in_flight(state: u64) -> u64 {
    state & LEASE_COUNT_MASK
}

/// Local data-plane fence for one shard owner.
///
/// A returned [`OwnershipPermit`] must stay alive until the accepted message
/// has completed all state mutation. Draining first rejects new permits, then
/// waits for the live count to reach zero before checkpoint handoff.
#[derive(Debug)]
struct ShardLeaseInner {
    address: ShardAddress,
    /// Phase and permit count share one modification order. Keeping them in
    /// separate atomics would let a drainer observe the new phase with a
    /// stale zero count on weakly ordered hardware.
    state: AtomicU64,
}

/// Cloneable control-plane handle for one local shard lease.
///
/// Ownership permits retain the same inner allocation, so a permit may cross
/// an async inbox without borrowing the coordinator's handle. Atomic phase
/// transitions still serialize all clones.
#[derive(Debug, Clone)]
pub struct ShardLease {
    inner: Arc<ShardLeaseInner>,
}

impl ShardLease {
    pub fn serving(address: ShardAddress) -> Self {
        Self {
            inner: Arc::new(ShardLeaseInner {
                address,
                state: AtomicU64::new(lease_state(LEASE_SERVING, 0)),
            }),
        }
    }

    pub fn address(&self) -> &ShardAddress {
        &self.inner.address
    }

    pub fn phase(&self) -> LeasePhase {
        decode_phase(state_phase(self.inner.state.load(Ordering::Acquire)))
    }

    pub fn in_flight(&self) -> u64 {
        state_in_flight(self.inner.state.load(Ordering::Acquire))
    }

    pub fn authorize(&self, destination: &ShardAddress) -> OwnershipResult<OwnershipPermit> {
        if destination.key != self.inner.address.key
            || destination.owner != self.inner.address.owner
        {
            return Err(OwnershipError::WrongDestination);
        }
        if destination.epoch != self.inner.address.epoch {
            return Err(OwnershipError::StaleEpoch {
                expected: self.inner.address.epoch.get(),
                actual: destination.epoch.get(),
            });
        }
        let mut state = self.inner.state.load(Ordering::Acquire);
        loop {
            let phase = decode_phase(state_phase(state));
            if phase != LeasePhase::Serving {
                return Err(OwnershipError::NotServing(phase));
            }
            let in_flight = state_in_flight(state);
            if in_flight == LEASE_COUNT_MASK {
                return Err(OwnershipError::PermitOverflow);
            }
            let next = lease_state(LEASE_SERVING, in_flight + 1);
            match self.inner.state.compare_exchange_weak(
                state,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Ok(OwnershipPermit {
                        inner: self.inner.clone(),
                    })
                }
                Err(observed) => state = observed,
            }
        }
    }

    pub fn authorize_envelope(&self, envelope: &WireEnvelope) -> OwnershipResult<OwnershipPermit> {
        self.authorize(envelope.destination())
    }

    pub fn begin_drain(
        &self,
        next_owner: NodeId,
        next_epoch: OwnershipEpoch,
    ) -> OwnershipResult<HandoffPlan> {
        if next_epoch <= self.inner.address.epoch {
            return Err(OwnershipError::NonIncreasingEpoch {
                current: self.inner.address.epoch.get(),
                next: next_epoch.get(),
            });
        }
        self.transition(LeasePhase::Serving, LeasePhase::Draining)?;
        Ok(HandoffPlan {
            source: self.inner.address.clone(),
            destination: ShardAddress::new(self.inner.address.key.clone(), next_owner, next_epoch),
        })
    }

    pub fn cancel_drain(&self, plan: &HandoffPlan) -> OwnershipResult<()> {
        self.validate_plan(plan)?;
        self.transition(LeasePhase::Draining, LeasePhase::Serving)
    }

    pub fn complete_handoff(
        &self,
        plan: HandoffPlan,
        checkpoint: ShardCheckpoint,
    ) -> OwnershipResult<HandoffBundle> {
        self.validate_plan(&plan)?;
        self.retire_if_drained()?;
        Ok(HandoffBundle { plan, checkpoint })
    }

    /// Create the successor's fenced lease in `Pending`. Restore and verify
    /// the returned checkpoint before calling [`activate`](Self::activate).
    pub fn receive_handoff(
        bundle: HandoffBundle,
        local_owner: &NodeId,
    ) -> OwnershipResult<(Self, ShardCheckpoint)> {
        if &bundle.plan.destination.owner != local_owner {
            return Err(OwnershipError::WrongDestination);
        }
        Ok((
            Self {
                inner: Arc::new(ShardLeaseInner {
                    address: bundle.plan.destination,
                    state: AtomicU64::new(lease_state(LEASE_PENDING, 0)),
                }),
            },
            bundle.checkpoint,
        ))
    }

    pub fn activate(&self) -> OwnershipResult<()> {
        self.transition(LeasePhase::Pending, LeasePhase::Serving)
    }

    fn validate_plan(&self, plan: &HandoffPlan) -> OwnershipResult<()> {
        if plan.source != self.inner.address || plan.destination.key != self.inner.address.key {
            return Err(OwnershipError::WrongHandoff);
        }
        Ok(())
    }

    fn transition(&self, expected: LeasePhase, next: LeasePhase) -> OwnershipResult<()> {
        let next_raw = match next {
            LeasePhase::Pending => LEASE_PENDING,
            LeasePhase::Serving => LEASE_SERVING,
            LeasePhase::Draining => LEASE_DRAINING,
            LeasePhase::Retired => LEASE_RETIRED,
        };
        let mut state = self.inner.state.load(Ordering::Acquire);
        loop {
            let actual = decode_phase(state_phase(state));
            if actual != expected {
                return Err(OwnershipError::InvalidTransition { expected, actual });
            }
            let next_state = lease_state(next_raw, state_in_flight(state));
            match self.inner.state.compare_exchange_weak(
                state,
                next_state,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(observed) => state = observed,
            }
        }
    }

    fn retire_if_drained(&self) -> OwnershipResult<()> {
        let expected = lease_state(LEASE_DRAINING, 0);
        match self.inner.state.compare_exchange(
            expected,
            lease_state(LEASE_RETIRED, 0),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(()),
            Err(actual) => {
                let phase = decode_phase(state_phase(actual));
                let in_flight = state_in_flight(actual);
                if phase == LeasePhase::Draining && in_flight != 0 {
                    Err(OwnershipError::InFlight(in_flight))
                } else {
                    Err(OwnershipError::InvalidTransition {
                        expected: LeasePhase::Draining,
                        actual: phase,
                    })
                }
            }
        }
    }
}

/// Live proof that the receiver admitted work under the current serving
/// epoch. It is intentionally non-cloneable.
#[derive(Debug)]
pub struct OwnershipPermit {
    inner: Arc<ShardLeaseInner>,
}

impl Drop for OwnershipPermit {
    fn drop(&mut self) {
        let previous = self.inner.state.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(
            state_in_flight(previous) > 0,
            "ownership permit counter underflow"
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffPlan {
    source: ShardAddress,
    destination: ShardAddress,
}

impl HandoffPlan {
    pub fn source(&self) -> &ShardAddress {
        &self.source
    }

    pub fn destination(&self) -> &ShardAddress {
        &self.destination
    }
}

/// Application-defined state image plus the deduplication frontier needed to
/// prevent acknowledged retries from being re-applied after handoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardCheckpoint {
    format_version: u32,
    checkpoint_id: String,
    state: Vec<u8>,
    deduplication: Vec<MessageId>,
}

impl ShardCheckpoint {
    pub fn new(
        format_version: u32,
        checkpoint_id: impl Into<String>,
        state: Vec<u8>,
        deduplication: Vec<MessageId>,
    ) -> TransportResult<Self> {
        if format_version == 0 {
            return Err(TransportError::Configuration(
                "checkpoint format version must be greater than zero".into(),
            ));
        }
        let checkpoint_id = checkpoint_id.into();
        validate_identifier("checkpoint identity", &checkpoint_id)?;
        let unique = deduplication.iter().collect::<BTreeSet<_>>();
        if unique.len() != deduplication.len() {
            return Err(TransportError::Configuration(
                "checkpoint deduplication frontier contains duplicate identities".into(),
            ));
        }
        Ok(Self {
            format_version,
            checkpoint_id,
            state,
            deduplication,
        })
    }

    pub fn format_version(&self) -> u32 {
        self.format_version
    }

    pub fn checkpoint_id(&self) -> &str {
        &self.checkpoint_id
    }

    pub fn state(&self) -> &[u8] {
        &self.state
    }

    pub fn deduplication(&self) -> &[MessageId] {
        &self.deduplication
    }

    pub fn into_parts(self) -> (Vec<u8>, Vec<MessageId>) {
        (self.state, self.deduplication)
    }
}

/// Non-cloneable source-to-successor handoff proof.
#[derive(Debug)]
pub struct HandoffBundle {
    plan: HandoffPlan,
    checkpoint: ShardCheckpoint,
}

impl HandoffBundle {
    pub fn source(&self) -> &ShardAddress {
        &self.plan.source
    }

    pub fn destination(&self) -> &ShardAddress {
        &self.plan.destination
    }

    pub fn checkpoint(&self) -> &ShardCheckpoint {
        &self.checkpoint
    }

    /// Split a completed handoff for durable encoding. The deployment must
    /// authenticate the persisted representation before reconstructing it.
    pub fn into_persisted_parts(self) -> (ShardAddress, ShardAddress, ShardCheckpoint) {
        (self.plan.source, self.plan.destination, self.checkpoint)
    }

    /// Reconstruct an authenticated durable handoff at the successor.
    ///
    /// This verifies structural fencing invariants, not the authenticity or
    /// global uniqueness of the record; those belong to the coordinator and
    /// checkpoint store.
    pub fn from_persisted_parts(
        source: ShardAddress,
        destination: ShardAddress,
        checkpoint: ShardCheckpoint,
    ) -> OwnershipResult<Self> {
        if source.key != destination.key {
            return Err(OwnershipError::WrongHandoff);
        }
        if destination.epoch <= source.epoch {
            return Err(OwnershipError::NonIncreasingEpoch {
                current: source.epoch.get(),
                next: destination.epoch.get(),
            });
        }
        Ok(Self {
            plan: HandoffPlan {
                source,
                destination,
            },
            checkpoint,
        })
    }
}

/// Static placement group emitted by the compiler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacementGroupDescriptor<'a> {
    pub name: &'a str,
    pub processes: &'a [&'a str],
}

/// Verified process edge whose endpoints belong to different placement
/// groups and may therefore require a transport adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteBoundaryDescriptor<'a> {
    pub from_group: &'a str,
    pub from_process: &'a str,
    pub to_group: &'a str,
    pub to_process: &'a str,
    pub schema: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacementManifest<'a> {
    pub groups: &'a [PlacementGroupDescriptor<'a>],
    pub remote_boundaries: &'a [RemoteBoundaryDescriptor<'a>],
}

impl PlacementManifest<'_> {
    pub fn validate(&self) -> TransportResult<()> {
        if self.groups.is_empty() {
            return Err(TransportError::Configuration(
                "placement manifest must contain at least one group".into(),
            ));
        }
        let mut groups = BTreeMap::<&str, BTreeSet<&str>>::new();
        let mut all_processes = BTreeSet::new();
        for group in self.groups {
            validate_identifier("placement-group name", group.name)?;
            if group.processes.is_empty() {
                return Err(TransportError::Configuration(format!(
                    "placement group '{}' contains no processes",
                    group.name
                )));
            }
            if groups.contains_key(group.name) {
                return Err(TransportError::Configuration(format!(
                    "placement group '{}' is declared more than once",
                    group.name
                )));
            }
            let mut members = BTreeSet::new();
            for process in group.processes {
                validate_identifier("placed process name", process)?;
                if !members.insert(*process) {
                    return Err(TransportError::Configuration(format!(
                        "process '{process}' appears twice in placement group '{}'",
                        group.name
                    )));
                }
                if !all_processes.insert(*process) {
                    return Err(TransportError::Configuration(format!(
                        "process '{process}' belongs to more than one placement group"
                    )));
                }
            }
            groups.insert(group.name, members);
        }
        for boundary in self.remote_boundaries {
            let from = groups.get(boundary.from_group).ok_or_else(|| {
                TransportError::Configuration(format!(
                    "remote boundary references unknown group '{}'",
                    boundary.from_group
                ))
            })?;
            let to = groups.get(boundary.to_group).ok_or_else(|| {
                TransportError::Configuration(format!(
                    "remote boundary references unknown group '{}'",
                    boundary.to_group
                ))
            })?;
            if boundary.from_group == boundary.to_group {
                return Err(TransportError::Configuration(
                    "remote boundary endpoints must use different placement groups".into(),
                ));
            }
            if !from.contains(boundary.from_process) || !to.contains(boundary.to_process) {
                return Err(TransportError::Configuration(
                    "remote boundary process is not a member of its declared group".into(),
                ));
            }
            validate_identifier("remote boundary schema", boundary.schema)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORDER_FINGERPRINT: SchemaFingerprint = SchemaFingerprint::new([1; 32]);

    fn manifest(protocol: VersionRange) -> TransportManifest {
        let mut manifest = TransportManifest::new(protocol, 1024).expect("manifest");
        for version in 1..=3 {
            manifest
                .register_schema("Order", version, ORDER_FINGERPRINT)
                .expect("register schema");
        }
        manifest.enable_delivery(DeliverySemantics::AtLeastOnce);
        manifest
    }

    #[test]
    fn negotiation_selects_highest_common_versions_and_smallest_limit() {
        let local = manifest(VersionRange::new(1, 3).expect("local versions"));
        let mut remote = manifest(VersionRange::new(2, 4).expect("remote versions"));
        remote.max_payload_bytes = 64;
        remote.schemas.insert(
            "Order".into(),
            (2..=4)
                .map(|version| (version, ORDER_FINGERPRINT))
                .collect(),
        );
        let session = NegotiatedTransport::negotiate(&local, &remote).expect("negotiate");
        assert_eq!(session.protocol(), 3);
        assert_eq!(session.schemas().get("Order"), Some(&3));
        assert_eq!(session.max_payload_bytes(), 64);
    }

    #[test]
    fn negotiation_rejects_version_hash_and_delivery_skew() {
        let local = manifest(VersionRange::exact(1).expect("local"));
        let remote = manifest(VersionRange::exact(2).expect("remote"));
        assert!(matches!(
            NegotiatedTransport::negotiate(&local, &remote),
            Err(TransportError::NoCommonProtocol { .. })
        ));

        let remote = manifest(VersionRange::exact(1).expect("remote"))
            .with_routing_hash_version(ROUTING_HASH_VERSION + 1);
        assert!(matches!(
            NegotiatedTransport::negotiate(&local, &remote),
            Err(TransportError::RoutingHashMismatch { .. })
        ));

        let mut remote = TransportManifest::new(VersionRange::exact(1).expect("remote"), 1024)
            .expect("manifest");
        remote
            .register_schema("Order", 1, ORDER_FINGERPRINT)
            .expect("schema");
        remote.enable_delivery(DeliverySemantics::AtMostOnce);
        assert_eq!(
            NegotiatedTransport::negotiate(&local, &remote),
            Err(TransportError::NoCommonDeliverySemantics)
        );

        let mut remote = TransportManifest::new(VersionRange::exact(1).expect("remote"), 1024)
            .expect("manifest");
        remote
            .register_schema("Other", 1, SchemaFingerprint::new([2; 32]))
            .expect("schema");
        remote.enable_delivery(DeliverySemantics::AtLeastOnce);
        assert_eq!(
            NegotiatedTransport::negotiate(&local, &remote),
            Err(TransportError::IncompatibleSchema("Order".into()))
        );

        let mut remote = manifest(VersionRange::exact(1).expect("remote"));
        remote
            .schemas
            .get_mut("Order")
            .expect("schema")
            .insert(3, SchemaFingerprint::new([3; 32]));
        assert!(matches!(
            NegotiatedTransport::negotiate(&local, &remote),
            Ok(session) if session.schemas().get("Order") == Some(&2)
        ));

        for fingerprint in remote
            .schemas
            .get_mut("Order")
            .expect("schema")
            .values_mut()
        {
            *fingerprint = SchemaFingerprint::new([3; 32]);
        }
        assert_eq!(
            NegotiatedTransport::negotiate(&local, &remote),
            Err(TransportError::SchemaFingerprintMismatch {
                schema: "Order".into(),
                version: 3,
            })
        );
    }

    #[test]
    fn deduplication_snapshot_is_bounded_and_restorable() {
        let mut window = DedupWindow::new(2).expect("window");
        let first = MessageId::new("producer-a", 1).expect("message");
        let second = MessageId::new("producer-a", 2).expect("message");
        let third = MessageId::new("producer-a", 3).expect("message");
        assert_eq!(window.observe(first.clone()), DedupDecision::FirstSeen);
        assert_eq!(window.observe(first.clone()), DedupDecision::Duplicate);
        assert_eq!(window.observe(second), DedupDecision::FirstSeen);
        assert_eq!(window.observe(third), DedupDecision::FirstSeen);
        assert_eq!(window.observe(first), DedupDecision::FirstSeen);

        let restored = DedupWindow::restore(2, window.snapshot()).expect("restore");
        assert_eq!(restored.snapshot(), window.snapshot());
    }

    #[test]
    fn decoded_envelopes_must_match_the_negotiated_session() {
        let session = NegotiatedTransport::negotiate(
            &manifest(VersionRange::exact(1).expect("local")),
            &manifest(VersionRange::exact(1).expect("remote")),
        )
        .expect("session");
        let destination = ShardAddress::new(
            ShardKey::new("prod", "workers", "OrderBook", 0).expect("key"),
            NodeId::new("node-a").expect("node"),
            OwnershipEpoch::new(1).expect("epoch"),
        );
        let id = MessageId::new("producer-a", 1).expect("message id");
        assert!(matches!(
            session.receive_envelope(
                2,
                destination.clone(),
                id.clone(),
                "Order",
                3,
                DeliverySemantics::AtLeastOnce,
                vec![1],
            ),
            Err(TransportError::ProtocolMismatch {
                expected: 1,
                actual: 2,
            })
        ));
        assert!(matches!(
            session.receive_envelope(
                1,
                destination,
                id,
                "Order",
                3,
                DeliverySemantics::AtLeastOnce,
                vec![0; 1025],
            ),
            Err(TransportError::PayloadTooLarge {
                actual: 1025,
                maximum: 1024,
            })
        ));
    }

    #[test]
    fn placement_manifest_rejects_ambiguous_membership() {
        let manifest = PlacementManifest {
            groups: &[
                PlacementGroupDescriptor {
                    name: "edge",
                    processes: &["Gateway"],
                },
                PlacementGroupDescriptor {
                    name: "core",
                    processes: &["Gateway"],
                },
            ],
            remote_boundaries: &[],
        };
        assert!(matches!(
            manifest.validate(),
            Err(TransportError::Configuration(message))
                if message.contains("more than one placement group")
        ));
    }
}
