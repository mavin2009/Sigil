//! Durable at-least-once delivery contracts.
//!
//! Durability is supplied by deployment adapters. This module defines the
//! ordering those adapters must preserve and provides the retry coordinator:
//!
//! 1. persist the complete envelope before the first transport attempt;
//! 2. durably increment its attempt count before every admission;
//! 3. retain it across acceptance, loss, partitions, and process restart;
//! 4. remove it only for a matching receiver acknowledgement whose
//!    application state and deduplication identity were committed atomically.

use super::{
    AuthorizedMessage, DeliverySemantics, MessageId, RemoteAdmission, RemoteEndpoint,
    RemoteMessageError, RemoteMessageResult, SchemaFingerprint, ShardAddress, Transport,
    TransportError, TransportOutcome, TransportResult, WireCodec, WireEnvelope,
};
use crate::StableRouteKey;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

/// Metadata that a receiver-side state store uses as its deduplication key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryContext {
    message_id: MessageId,
    destination: ShardAddress,
    schema: &'static str,
    schema_version: u32,
    schema_fingerprint: SchemaFingerprint,
    delivery: DeliverySemantics,
}

impl DeliveryContext {
    pub fn from_authorized<T: WireCodec>(message: &AuthorizedMessage<T>) -> Self {
        Self {
            message_id: message.message_id().clone(),
            destination: message.destination().clone(),
            schema: T::SCHEMA,
            schema_version: message.schema_version(),
            schema_fingerprint: T::FINGERPRINT,
            delivery: message.delivery(),
        }
    }

    pub fn message_id(&self) -> &MessageId {
        &self.message_id
    }

    pub fn destination(&self) -> &ShardAddress {
        &self.destination
    }

    pub fn schema(&self) -> &'static str {
        self.schema
    }

    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    pub fn schema_fingerprint(&self) -> SchemaFingerprint {
        self.schema_fingerprint
    }

    pub fn delivery(&self) -> DeliverySemantics {
        self.delivery
    }
}

/// Durable receiver commit evidence returned by a [`StateCommitter`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableCommit {
    checkpoint_id: String,
}

impl DurableCommit {
    pub fn new(checkpoint_id: impl Into<String>) -> TransportResult<Self> {
        let checkpoint_id = checkpoint_id.into();
        if checkpoint_id.is_empty() || checkpoint_id.chars().any(char::is_control) {
            return Err(TransportError::Configuration(
                "durable commit identity must be non-empty and contain no control characters"
                    .into(),
            ));
        }
        Ok(Self { checkpoint_id })
    }

    pub fn checkpoint_id(&self) -> &str {
        &self.checkpoint_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitLookup {
    New,
    Committed(DurableCommit),
}

pub type StateCommitFuture<'a> =
    Pin<Box<dyn Future<Output = TransportResult<DurableCommit>> + Send + 'a>>;
pub type CommitLookupFuture<'a> =
    Pin<Box<dyn Future<Output = TransportResult<CommitLookup>> + Send + 'a>>;
pub type StateRestoreFuture<'a> = Pin<Box<dyn Future<Output = TransportResult<()>> + Send + 'a>>;

/// Receiver-owned durable state and deduplication transaction.
///
/// `restore`, `lookup`, and `commit` must be linearizable for one destination
/// shard. `restore` must recover application state and the deduplication
/// frontier from the same checkpoint before receiver admission begins.
/// `commit` must atomically persist the supplied application state and mark
/// the message identity committed. Returning success for only one of those
/// writes violates the contract and can cause duplicate state mutation.
pub trait StateCommitter<S>: Send + Sync {
    fn restore<'a>(&'a self, state: &'a mut S) -> StateRestoreFuture<'a>;

    fn lookup<'a>(&'a self, context: &'a DeliveryContext) -> CommitLookupFuture<'a>;

    fn commit<'a>(&'a self, context: &'a DeliveryContext, state: &'a S) -> StateCommitFuture<'a>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyDurability {
    AppliedVolatile,
    DurablyCommitted,
}

/// Receiver result returned only after the ownership permit covered handler
/// execution and, when configured, state/deduplication commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyReceipt {
    context: DeliveryContext,
    durability: ApplyDurability,
    duplicate: bool,
    checkpoint_id: Option<String>,
}

impl ApplyReceipt {
    pub fn applied(context: DeliveryContext) -> Self {
        Self {
            context,
            durability: ApplyDurability::AppliedVolatile,
            duplicate: false,
            checkpoint_id: None,
        }
    }

    pub fn committed(context: DeliveryContext, commit: DurableCommit, duplicate: bool) -> Self {
        Self {
            context,
            durability: ApplyDurability::DurablyCommitted,
            duplicate,
            checkpoint_id: Some(commit.checkpoint_id),
        }
    }

    pub fn context(&self) -> &DeliveryContext {
        &self.context
    }

    pub fn durability(&self) -> ApplyDurability {
        self.durability
    }

    pub fn is_duplicate(&self) -> bool {
        self.duplicate
    }

    pub fn checkpoint_id(&self) -> Option<&str> {
        self.checkpoint_id.as_deref()
    }
}

/// Ack accepted by a durable producer outbox.
///
/// The network adapter must authenticate acknowledgements before constructing
/// this value. Structural matching is still rechecked against the pending
/// envelope by [`DurableOutbox`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryAck {
    receipt: ApplyReceipt,
}

impl DeliveryAck {
    pub fn from_receipt(receipt: ApplyReceipt) -> TransportResult<Self> {
        if receipt.context.delivery == DeliverySemantics::AtLeastOnce
            && receipt.durability != ApplyDurability::DurablyCommitted
        {
            return Err(TransportError::Configuration(
                "at-least-once acknowledgement requires durable receiver commit evidence".into(),
            ));
        }
        Ok(Self { receipt })
    }

    pub fn receipt(&self) -> &ApplyReceipt {
        &self.receipt
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxRecord {
    envelope: WireEnvelope,
    attempts: u32,
}

impl OutboxRecord {
    pub fn new(envelope: WireEnvelope) -> Self {
        Self {
            envelope,
            attempts: 0,
        }
    }

    pub fn envelope(&self) -> &WireEnvelope {
        &self.envelope
    }

    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// Rehydrate one record from durable storage.
    pub fn from_persisted(envelope: WireEnvelope, attempts: u32) -> Self {
        Self { envelope, attempts }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistOutcome {
    Inserted,
    AlreadyPending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcknowledgeOutcome {
    Removed,
    AlreadyAcknowledged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptReservation {
    Reserved(u32),
    Exhausted(u32),
    AlreadyAcknowledged,
}

pub type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = TransportResult<T>> + Send + 'a>>;

/// Crash-durable producer outbox storage.
///
/// Implementations must make every method atomic and preserve acknowledged
/// identities long enough to make repeated acknowledgements idempotent.
pub trait OutboxStore: Send + Sync {
    /// Insert exactly once. A repeated identity with identical envelope bytes
    /// returns `AlreadyPending`; reuse for different bytes must fail.
    fn persist(&self, record: OutboxRecord) -> StoreFuture<'_, PersistOutcome>;

    fn load<'a>(&'a self, id: &'a MessageId) -> StoreFuture<'a, Option<OutboxRecord>>;

    fn pending(&self, limit: usize) -> StoreFuture<'_, Vec<OutboxRecord>>;

    /// Atomically compare the persisted attempt count with `maximum`, then
    /// increment it durably before the corresponding transport attempt.
    ///
    /// This comparison must serialize concurrent dispatchers for the same
    /// identity. `AlreadyAcknowledged` prevents an ack race from resurrecting
    /// work after its pending record was removed.
    fn reserve_attempt<'a>(
        &'a self,
        id: &'a MessageId,
        maximum: u32,
    ) -> StoreFuture<'a, AttemptReservation>;

    fn acknowledge(&self, ack: DeliveryAck) -> StoreFuture<'_, AcknowledgeOutcome>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    max_attempts: u32,
    initial_backoff: Duration,
    max_backoff: Duration,
}

impl RetryPolicy {
    pub fn new(
        max_attempts: u32,
        initial_backoff: Duration,
        max_backoff: Duration,
    ) -> TransportResult<Self> {
        if max_attempts == 0 {
            return Err(TransportError::Configuration(
                "retry policy must allow at least one attempt".into(),
            ));
        }
        if initial_backoff.is_zero() || max_backoff < initial_backoff {
            return Err(TransportError::Configuration(
                "retry backoff must be nonzero and no larger than its maximum".into(),
            ));
        }
        Ok(Self {
            max_attempts,
            initial_backoff,
            max_backoff,
        })
    }

    pub fn max_attempts(self) -> u32 {
        self.max_attempts
    }

    pub fn delay_after(self, completed_attempts: u32) -> Duration {
        let shifts = completed_attempts.saturating_sub(1).min(31);
        let factor = 1_u32 << shifts;
        self.initial_backoff
            .checked_mul(factor)
            .unwrap_or(self.max_backoff)
            .min(self.max_backoff)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchOutcome {
    Accepted,
    Shed,
    Duplicate,
    Failed(TransportError),
    Exhausted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchReport {
    message_id: MessageId,
    attempt: u32,
    outcome: DispatchOutcome,
    next_delay: Option<Duration>,
}

impl DispatchReport {
    pub fn message_id(&self) -> &MessageId {
        &self.message_id
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    pub fn outcome(&self) -> &DispatchOutcome {
        &self.outcome
    }

    pub fn next_delay(&self) -> Option<Duration> {
        self.next_delay
    }

    /// Collapse a durable dispatch into the admission vocabulary used by
    /// generated back-pressure accounting. Failed and exhausted attempts stay
    /// errors; their envelopes remain pending in the durable store.
    pub fn transport_outcome(&self) -> TransportResult<TransportOutcome> {
        match &self.outcome {
            DispatchOutcome::Accepted => Ok(TransportOutcome::Accepted),
            DispatchOutcome::Shed => Ok(TransportOutcome::Shed),
            DispatchOutcome::Duplicate => Ok(TransportOutcome::Duplicate),
            DispatchOutcome::Failed(error) => Err(error.clone()),
            DispatchOutcome::Exhausted => Err(TransportError::Unavailable(format!(
                "durable message '{}:{}' exhausted {} attempts and remains pending",
                self.message_id.producer(),
                self.message_id.sequence(),
                self.attempt
            ))),
        }
    }
}

/// Producer-side persist-before-send and retry coordinator.
#[derive(Clone)]
pub struct DurableOutbox {
    transport: Arc<dyn Transport>,
    store: Arc<dyn OutboxStore>,
    policy: RetryPolicy,
}

impl DurableOutbox {
    pub fn new(
        transport: Arc<dyn Transport>,
        store: Arc<dyn OutboxStore>,
        policy: RetryPolicy,
    ) -> TransportResult<Self> {
        if !transport
            .session()
            .delivery()
            .contains(&DeliverySemantics::AtLeastOnce)
        {
            return Err(TransportError::Configuration(
                "durable outbox requires negotiated at-least-once delivery".into(),
            ));
        }
        Ok(Self {
            transport,
            store,
            policy,
        })
    }

    pub async fn persist(&self, envelope: WireEnvelope) -> TransportResult<PersistOutcome> {
        self.transport.session().validate_envelope(&envelope)?;
        if envelope.delivery() != DeliverySemantics::AtLeastOnce {
            return Err(TransportError::Configuration(
                "durable outbox accepts only at-least-once envelopes".into(),
            ));
        }
        self.store.persist(OutboxRecord::new(envelope)).await
    }

    pub async fn dispatch(
        &self,
        id: &MessageId,
        admission: RemoteAdmission,
    ) -> TransportResult<DispatchReport> {
        admission.validate()?;
        let record = self.store.load(id).await?.ok_or_else(|| {
            TransportError::Configuration(format!(
                "message '{}:{}' is not pending in the durable outbox",
                id.producer(),
                id.sequence()
            ))
        })?;
        let attempt = match self
            .store
            .reserve_attempt(id, self.policy.max_attempts)
            .await?
        {
            AttemptReservation::Reserved(attempt) => attempt,
            AttemptReservation::Exhausted(attempts) => {
                return Ok(DispatchReport {
                    message_id: id.clone(),
                    attempt: attempts,
                    outcome: DispatchOutcome::Exhausted,
                    next_delay: None,
                });
            }
            AttemptReservation::AlreadyAcknowledged => {
                return Ok(DispatchReport {
                    message_id: id.clone(),
                    attempt: record.attempts,
                    outcome: DispatchOutcome::Duplicate,
                    next_delay: None,
                });
            }
        };
        let outcome = match self
            .transport
            .admit(record.envelope.clone(), admission)
            .await
        {
            Ok(TransportOutcome::Accepted) => DispatchOutcome::Accepted,
            Ok(TransportOutcome::Shed) => DispatchOutcome::Shed,
            Ok(TransportOutcome::Duplicate) => DispatchOutcome::Duplicate,
            Err(error) => DispatchOutcome::Failed(error),
        };
        let next_delay =
            (attempt < self.policy.max_attempts).then(|| self.policy.delay_after(attempt));
        Ok(DispatchReport {
            message_id: id.clone(),
            attempt,
            outcome,
            next_delay,
        })
    }

    pub async fn dispatch_pending(
        &self,
        limit: usize,
        admission: RemoteAdmission,
    ) -> TransportResult<Vec<DispatchReport>> {
        if limit == 0 {
            return Err(TransportError::Configuration(
                "durable outbox batch limit must be greater than zero".into(),
            ));
        }
        let records = self.store.pending(limit).await?;
        let mut reports = Vec::new();
        reports.try_reserve_exact(records.len()).map_err(|_| {
            TransportError::Unavailable("cannot allocate durable outbox retry batch".into())
        })?;
        for record in records {
            reports.push(
                self.dispatch(record.envelope.message_id(), admission)
                    .await?,
            );
        }
        Ok(reports)
    }

    pub async fn acknowledge(&self, ack: DeliveryAck) -> TransportResult<AcknowledgeOutcome> {
        let receipt = ack.receipt();
        if receipt.durability != ApplyDurability::DurablyCommitted {
            return Err(TransportError::Configuration(
                "durable outbox requires durable commit evidence".into(),
            ));
        }
        let id = receipt.context.message_id();
        let Some(record) = self.store.load(id).await? else {
            return self.store.acknowledge(ack).await;
        };
        let envelope = record.envelope();
        let context = receipt.context();
        if envelope.destination() != context.destination()
            || envelope.schema() != context.schema()
            || envelope.schema_version() != context.schema_version()
            || envelope.delivery() != context.delivery()
        {
            return Err(TransportError::Configuration(
                "acknowledgement metadata does not match the pending envelope".into(),
            ));
        }
        let negotiated_fingerprint = self
            .transport
            .session()
            .schema_fingerprint(envelope.schema())
            .ok_or_else(|| TransportError::UnknownSchema(envelope.schema().to_owned()))?;
        if negotiated_fingerprint != context.schema_fingerprint() {
            return Err(TransportError::SchemaFingerprintMismatch {
                schema: envelope.schema().to_owned(),
                version: envelope.schema_version(),
            });
        }
        self.store.acknowledge(ack).await
    }

    pub fn session(&self) -> &super::NegotiatedTransport {
        self.transport.session()
    }
}

/// Typed at-least-once endpoint whose first attempt obeys the same durable
/// ordering as later retries.
///
/// This deliberately has no at-most-once constructor. Use [`RemoteEndpoint`]
/// directly when loss without retry is the declared contract.
#[derive(Clone)]
pub struct DurableRemoteEndpoint<T> {
    endpoint: RemoteEndpoint<T>,
    outbox: DurableOutbox,
}

impl<T: WireCodec> DurableRemoteEndpoint<T> {
    pub fn new(endpoint: RemoteEndpoint<T>, outbox: DurableOutbox) -> RemoteMessageResult<Self> {
        if endpoint.delivery() != DeliverySemantics::AtLeastOnce {
            return Err(TransportError::Configuration(
                "durable remote endpoint requires at-least-once delivery".into(),
            )
            .into());
        }
        if endpoint.session() != outbox.session() {
            return Err(TransportError::Configuration(
                "remote endpoint and durable outbox use different negotiated sessions".into(),
            )
            .into());
        }
        Ok(Self { endpoint, outbox })
    }

    pub fn destinations(&self) -> &[ShardAddress] {
        self.endpoint.destinations()
    }

    pub fn outbox(&self) -> &DurableOutbox {
        &self.outbox
    }

    async fn persist_and_dispatch(
        &self,
        envelope: WireEnvelope,
        admission: RemoteAdmission,
    ) -> RemoteMessageResult<DispatchReport> {
        let message_id = envelope.message_id().clone();
        self.outbox.persist(envelope).await?;
        Ok(self.outbox.dispatch(&message_id, admission).await?)
    }

    pub async fn admit_round_robin(
        &self,
        value: &T,
        admission: RemoteAdmission,
    ) -> RemoteMessageResult<DispatchReport> {
        let envelope = self.endpoint.prepare_round_robin(value).await?;
        self.persist_and_dispatch(envelope, admission).await
    }

    pub async fn admit_by_key<K: StableRouteKey + ?Sized>(
        &self,
        key: &K,
        value: &T,
        admission: RemoteAdmission,
    ) -> RemoteMessageResult<DispatchReport> {
        let envelope = self.endpoint.prepare_by_key(key, value).await?;
        self.persist_and_dispatch(envelope, admission).await
    }

    pub async fn admit_broadcast(
        &self,
        value: &T,
        admission: RemoteAdmission,
    ) -> RemoteMessageResult<Vec<DispatchReport>> {
        let envelopes = self.endpoint.prepare_broadcast(value).await?;
        let mut reports = Vec::new();
        reports
            .try_reserve_exact(envelopes.len())
            .map_err(|_| RemoteMessageError::Codec(super::CodecError::AllocationFailed))?;
        for envelope in envelopes {
            reports.push(self.persist_and_dispatch(envelope, admission).await?);
        }
        Ok(reports)
    }
}

/// Result of bounded receiver-inbox admission.
// The applied receipt is intentionally inline: receiver admission returns it
// once and avoids a second allocation on every successful delivery.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiverOutcome {
    Applied(ApplyReceipt),
    Shed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::distributed::{
        NodeId, OwnershipEpoch, SchemaFingerprint, ShardKey, TransportFuture, TransportManifest,
        VersionRange, DISTRIBUTED_PROTOCOL_VERSION,
    };
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    use std::sync::Mutex;

    const FINGERPRINT: SchemaFingerprint = SchemaFingerprint::new([11; 32]);

    #[derive(Debug, Default)]
    struct MemoryStoreState {
        pending: BTreeMap<MessageId, OutboxRecord>,
        acknowledged: BTreeSet<MessageId>,
    }

    #[derive(Debug, Default)]
    struct MemoryStore {
        state: Mutex<MemoryStoreState>,
    }

    impl OutboxStore for MemoryStore {
        fn persist(&self, record: OutboxRecord) -> StoreFuture<'_, PersistOutcome> {
            Box::pin(async move {
                let id = record.envelope.message_id().clone();
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| TransportError::Unavailable("store lock poisoned".into()))?;
                if state.acknowledged.contains(&id) {
                    return Err(TransportError::Configuration(
                        "cannot reinsert an acknowledged message identity".into(),
                    ));
                }
                match state.pending.get(&id) {
                    Some(existing) if existing.envelope == record.envelope => {
                        Ok(PersistOutcome::AlreadyPending)
                    }
                    Some(_) => Err(TransportError::Configuration(
                        "message identity maps to different envelope bytes".into(),
                    )),
                    None => {
                        state.pending.insert(id, record);
                        Ok(PersistOutcome::Inserted)
                    }
                }
            })
        }

        fn load<'a>(&'a self, id: &'a MessageId) -> StoreFuture<'a, Option<OutboxRecord>> {
            Box::pin(async move {
                Ok(self
                    .state
                    .lock()
                    .map_err(|_| TransportError::Unavailable("store lock poisoned".into()))?
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
                    .map_err(|_| TransportError::Unavailable("store lock poisoned".into()))?
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
                    .map_err(|_| TransportError::Unavailable("store lock poisoned".into()))?;
                if state.acknowledged.contains(id) {
                    return Ok(AttemptReservation::AlreadyAcknowledged);
                }
                let record = state.pending.get_mut(id).ok_or_else(|| {
                    TransportError::Configuration("attempted message is not pending".into())
                })?;
                if record.attempts >= maximum {
                    return Ok(AttemptReservation::Exhausted(record.attempts));
                }
                record.attempts = record.attempts.checked_add(1).ok_or_else(|| {
                    TransportError::Unavailable("outbox attempt counter exhausted".into())
                })?;
                Ok(AttemptReservation::Reserved(record.attempts))
            })
        }

        fn acknowledge(&self, ack: DeliveryAck) -> StoreFuture<'_, AcknowledgeOutcome> {
            Box::pin(async move {
                let id = ack.receipt.context.message_id.clone();
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| TransportError::Unavailable("store lock poisoned".into()))?;
                if state.pending.remove(&id).is_some() {
                    state.acknowledged.insert(id);
                    Ok(AcknowledgeOutcome::Removed)
                } else if state.acknowledged.contains(&id) {
                    Ok(AcknowledgeOutcome::AlreadyAcknowledged)
                } else {
                    Err(TransportError::Configuration(
                        "acknowledgement does not reference known outbox state".into(),
                    ))
                }
            })
        }
    }

    #[derive(Debug)]
    struct ScriptedTransport {
        session: super::super::NegotiatedTransport,
        outcomes: Mutex<VecDeque<TransportResult<TransportOutcome>>>,
    }

    impl Transport for ScriptedTransport {
        fn session(&self) -> &super::super::NegotiatedTransport {
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
                self.outcomes
                    .lock()
                    .map_err(|_| TransportError::Unavailable("transport lock poisoned".into()))?
                    .pop_front()
                    .unwrap_or(Ok(TransportOutcome::Accepted))
            })
        }
    }

    fn session() -> super::super::NegotiatedTransport {
        let mut manifest = TransportManifest::new(
            VersionRange::exact(DISTRIBUTED_PROTOCOL_VERSION).expect("protocol"),
            1024,
        )
        .expect("manifest");
        manifest
            .register_schema("Order", 1, FINGERPRINT)
            .expect("schema");
        manifest.enable_delivery(DeliverySemantics::AtLeastOnce);
        super::super::NegotiatedTransport::negotiate(&manifest, &manifest).expect("session")
    }

    fn destination() -> ShardAddress {
        ShardAddress::new(
            ShardKey::new("prod", "workers", "OrderBook", 0).expect("key"),
            NodeId::new("node-a").expect("node"),
            OwnershipEpoch::new(1).expect("epoch"),
        )
    }

    fn envelope(session: &super::super::NegotiatedTransport, id: MessageId) -> WireEnvelope {
        session
            .envelope(
                destination(),
                id,
                "Order",
                DeliverySemantics::AtLeastOnce,
                42_i64.to_le_bytes().to_vec(),
            )
            .expect("envelope")
    }

    fn committed_receipt(id: MessageId) -> ApplyReceipt {
        ApplyReceipt::committed(
            DeliveryContext {
                message_id: id,
                destination: destination(),
                schema: "Order",
                schema_version: 1,
                schema_fingerprint: FINGERPRINT,
                delivery: DeliverySemantics::AtLeastOnce,
            },
            DurableCommit::new("checkpoint-1").expect("commit"),
            false,
        )
    }

    #[tokio::test]
    async fn persist_before_send_survives_restart_and_ack_is_idempotent() {
        let session = session();
        let transport = Arc::new(ScriptedTransport {
            session: session.clone(),
            outcomes: Mutex::new(
                vec![
                    Err(TransportError::Unavailable("partition".into())),
                    Ok(TransportOutcome::Accepted),
                ]
                .into(),
            ),
        });
        let store = Arc::new(MemoryStore::default());
        let policy = RetryPolicy::new(4, Duration::from_millis(10), Duration::from_millis(80))
            .expect("policy");
        let outbox = DurableOutbox::new(transport.clone(), store.clone(), policy).expect("outbox");
        let id = MessageId::new("producer", 7).expect("id");
        assert_eq!(
            outbox
                .persist(envelope(&session, id.clone()))
                .await
                .expect("persist"),
            PersistOutcome::Inserted
        );
        let first = outbox
            .dispatch(&id, RemoteAdmission::Shed)
            .await
            .expect("first dispatch");
        assert!(matches!(
            first.outcome(),
            DispatchOutcome::Failed(TransportError::Unavailable(message))
                if message == "partition"
        ));
        assert_eq!(first.attempt(), 1);
        assert_eq!(first.next_delay(), Some(Duration::from_millis(10)));

        // Reconstructing the coordinator over the same durable store models a
        // process restart. The complete envelope and attempt count survive.
        let restarted =
            DurableOutbox::new(transport, store.clone(), policy).expect("restarted outbox");
        let second = restarted
            .dispatch_pending(16, RemoteAdmission::Shed)
            .await
            .expect("retry batch");
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].attempt(), 2);
        assert_eq!(second[0].outcome(), &DispatchOutcome::Accepted);

        let ack = DeliveryAck::from_receipt(committed_receipt(id.clone())).expect("ack");
        assert_eq!(
            restarted.acknowledge(ack.clone()).await.expect("remove"),
            AcknowledgeOutcome::Removed
        );
        assert_eq!(
            restarted.acknowledge(ack).await.expect("repeat ack"),
            AcknowledgeOutcome::AlreadyAcknowledged
        );
        assert!(store.load(&id).await.expect("load").is_none());
    }

    #[test]
    fn at_least_once_ack_rejects_volatile_application_state() {
        let context = DeliveryContext {
            message_id: MessageId::new("producer", 1).expect("id"),
            destination: destination(),
            schema: "Order",
            schema_version: 1,
            schema_fingerprint: FINGERPRINT,
            delivery: DeliverySemantics::AtLeastOnce,
        };
        assert!(matches!(
            DeliveryAck::from_receipt(ApplyReceipt::applied(context)),
            Err(TransportError::Configuration(message))
                if message.contains("durable receiver commit")
        ));
    }

    #[tokio::test]
    async fn retry_exhaustion_never_deletes_pending_work() {
        let session = session();
        let transport = Arc::new(ScriptedTransport {
            session: session.clone(),
            outcomes: Mutex::new(VecDeque::from([Ok(TransportOutcome::Shed)])),
        });
        let store = Arc::new(MemoryStore::default());
        let outbox = DurableOutbox::new(
            transport,
            store.clone(),
            RetryPolicy::new(1, Duration::from_millis(1), Duration::from_millis(1))
                .expect("policy"),
        )
        .expect("outbox");
        let id = MessageId::new("producer", 1).expect("id");
        outbox
            .persist(envelope(&session, id.clone()))
            .await
            .expect("persist");
        assert_eq!(
            outbox
                .dispatch(&id, RemoteAdmission::Shed)
                .await
                .expect("attempt")
                .outcome(),
            &DispatchOutcome::Shed
        );
        assert_eq!(
            outbox
                .dispatch(&id, RemoteAdmission::Shed)
                .await
                .expect("exhausted")
                .outcome(),
            &DispatchOutcome::Exhausted
        );
        assert!(store.load(&id).await.expect("load").is_some());
    }

    #[tokio::test]
    async fn concurrent_dispatchers_cannot_overshoot_retry_budget() {
        let session = session();
        let transport = Arc::new(ScriptedTransport {
            session: session.clone(),
            outcomes: Mutex::new(VecDeque::new()),
        });
        let store = Arc::new(MemoryStore::default());
        let outbox = DurableOutbox::new(
            transport,
            store.clone(),
            RetryPolicy::new(1, Duration::from_millis(1), Duration::from_millis(1))
                .expect("policy"),
        )
        .expect("outbox");
        let id = MessageId::new("producer", 99).expect("id");
        outbox
            .persist(envelope(&session, id.clone()))
            .await
            .expect("persist");

        let mut tasks = Vec::new();
        for _ in 0..16 {
            let outbox = outbox.clone();
            let id = id.clone();
            tasks.push(tokio::spawn(async move {
                outbox
                    .dispatch(&id, RemoteAdmission::Shed)
                    .await
                    .expect("dispatch")
            }));
        }
        let mut accepted = 0;
        let mut exhausted = 0;
        for task in tasks {
            match task.await.expect("join").outcome() {
                DispatchOutcome::Accepted => accepted += 1,
                DispatchOutcome::Exhausted => exhausted += 1,
                outcome => panic!("unexpected dispatch outcome: {outcome:?}"),
            }
        }
        assert_eq!(accepted, 1);
        assert_eq!(exhausted, 15);
        assert_eq!(
            store
                .load(&id)
                .await
                .expect("load")
                .expect("pending")
                .attempts(),
            1
        );
    }
}
