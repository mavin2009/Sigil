//! Deterministic, allocation-bounded codecs for generated Sigil schemas.
//!
//! The format is deliberately small and fixed:
//! - integers and floats are little-endian;
//! - booleans are exactly `0` or `1`;
//! - strings and byte arrays are a `u32` little-endian length followed by data;
//! - durations are a `u64` seconds value followed by `u32` nanoseconds;
//! - nested schemas are encoded inline in declaration order.
//!
//! Schema identity and version are carried by [`WireEnvelope`], while the
//! session also pins the codec's structural fingerprint. Payloads do not repeat
//! that metadata, and decoders never guess at a layout.

use super::{
    DeliverySemantics, MessageId, NegotiatedTransport, OwnershipError, OwnershipPermit,
    RemoteAdmission, SchemaFingerprint, ShardAddress, ShardLease, Transport, TransportError,
    TransportOutcome, WireEnvelope,
};
use crate::StableRouteKey;
use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::str;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CodecError {
    #[error("codec limit must be greater than zero")]
    ZeroLimit,
    #[error("field limit {field} exceeds payload limit {payload}")]
    FieldLimitExceedsPayload { field: usize, payload: usize },
    #[error("field limit {actual} exceeds the wire-format maximum of {maximum}")]
    FieldLimitTooLarge { actual: usize, maximum: usize },
    #[error("payload is {actual} bytes, exceeding the codec limit of {maximum}")]
    PayloadTooLarge { actual: usize, maximum: usize },
    #[error("field is {actual} bytes, exceeding the codec limit of {maximum}")]
    FieldTooLarge { actual: usize, maximum: usize },
    #[error("encoded length overflows the platform address space")]
    LengthOverflow,
    #[error("allocation for a bounded wire value failed")]
    AllocationFailed,
    #[error("wire payload ended at byte {offset}; {needed} more bytes were required")]
    Truncated { offset: usize, needed: usize },
    #[error("wire payload contains {remaining} trailing bytes")]
    TrailingBytes { remaining: usize },
    #[error("wire boolean at byte {offset} has invalid value {actual}")]
    InvalidBool { offset: usize, actual: u8 },
    #[error("wire float at byte {offset} is NaN or infinite")]
    NonFiniteFloat { offset: usize },
    #[error("wire string at byte {offset} is not valid UTF-8")]
    InvalidUtf8 { offset: usize },
    #[error("wire duration at byte {offset} has invalid nanoseconds value {nanoseconds}")]
    InvalidDuration { offset: usize, nanoseconds: u32 },
}

pub type CodecResult<T> = std::result::Result<T, CodecError>;

/// Explicit total-payload and individual-field allocation ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodecLimits {
    max_payload_bytes: usize,
    max_field_bytes: usize,
}

impl CodecLimits {
    pub fn new(max_payload_bytes: usize, max_field_bytes: usize) -> CodecResult<Self> {
        if max_payload_bytes == 0 || max_field_bytes == 0 {
            return Err(CodecError::ZeroLimit);
        }
        if max_field_bytes > max_payload_bytes {
            return Err(CodecError::FieldLimitExceedsPayload {
                field: max_field_bytes,
                payload: max_payload_bytes,
            });
        }
        let wire_maximum = u32::MAX as usize;
        if max_field_bytes > wire_maximum {
            return Err(CodecError::FieldLimitTooLarge {
                actual: max_field_bytes,
                maximum: wire_maximum,
            });
        }
        Ok(Self {
            max_payload_bytes,
            max_field_bytes,
        })
    }

    pub fn max_payload_bytes(self) -> usize {
        self.max_payload_bytes
    }

    pub fn max_field_bytes(self) -> usize {
        self.max_field_bytes
    }

    fn bounded_by(self, maximum: usize) -> CodecResult<Self> {
        let payload = self.max_payload_bytes.min(maximum);
        let field = self.max_field_bytes.min(payload);
        Self::new(payload, field)
    }
}

/// Checked encoder used by generated [`WireCodec`] implementations.
#[derive(Debug)]
pub struct WireEncoder {
    bytes: Vec<u8>,
    limits: CodecLimits,
}

impl WireEncoder {
    pub fn new(limits: CodecLimits) -> Self {
        Self {
            bytes: Vec::new(),
            limits,
        }
    }

    fn reserve(&mut self, additional: usize) -> CodecResult<()> {
        let required = self
            .bytes
            .len()
            .checked_add(additional)
            .ok_or(CodecError::LengthOverflow)?;
        if required > self.limits.max_payload_bytes {
            return Err(CodecError::PayloadTooLarge {
                actual: required,
                maximum: self.limits.max_payload_bytes,
            });
        }
        if required > self.bytes.capacity() {
            self.bytes
                .try_reserve_exact(required - self.bytes.len())
                .map_err(|_| CodecError::AllocationFailed)?;
        }
        Ok(())
    }

    fn write_fixed(&mut self, bytes: &[u8]) -> CodecResult<()> {
        self.reserve(bytes.len())?;
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn write_length_delimited(&mut self, value: &[u8]) -> CodecResult<()> {
        if value.len() > self.limits.max_field_bytes {
            return Err(CodecError::FieldTooLarge {
                actual: value.len(),
                maximum: self.limits.max_field_bytes,
            });
        }
        let length = u32::try_from(value.len()).map_err(|_| CodecError::LengthOverflow)?;
        let additional = 4usize
            .checked_add(value.len())
            .ok_or(CodecError::LengthOverflow)?;
        self.reserve(additional)?;
        self.bytes.extend_from_slice(&length.to_le_bytes());
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    pub fn write_i64(&mut self, value: i64) -> CodecResult<()> {
        self.write_fixed(&value.to_le_bytes())
    }

    pub fn write_f64(&mut self, value: f64) -> CodecResult<()> {
        if !value.is_finite() {
            return Err(CodecError::NonFiniteFloat {
                offset: self.bytes.len(),
            });
        }
        self.write_fixed(&value.to_bits().to_le_bytes())
    }

    pub fn write_bool(&mut self, value: bool) -> CodecResult<()> {
        self.write_fixed(&[u8::from(value)])
    }

    pub fn write_string(&mut self, value: &str) -> CodecResult<()> {
        self.write_length_delimited(value.as_bytes())
    }

    pub fn write_bytes(&mut self, value: &[u8]) -> CodecResult<()> {
        self.write_length_delimited(value)
    }

    pub fn write_duration(&mut self, value: Duration) -> CodecResult<()> {
        self.reserve(12)?;
        self.bytes.extend_from_slice(&value.as_secs().to_le_bytes());
        self.bytes
            .extend_from_slice(&value.subsec_nanos().to_le_bytes());
        Ok(())
    }

    pub fn write_nested<T: WireCodec>(&mut self, value: &T) -> CodecResult<()> {
        value.encode_wire(self)
    }

    pub fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

/// Checked decoder that validates lengths before allocating owned fields.
#[derive(Debug)]
pub struct WireDecoder<'a> {
    bytes: &'a [u8],
    offset: usize,
    limits: CodecLimits,
}

impl<'a> WireDecoder<'a> {
    pub fn new(bytes: &'a [u8], limits: CodecLimits) -> CodecResult<Self> {
        if bytes.len() > limits.max_payload_bytes {
            return Err(CodecError::PayloadTooLarge {
                actual: bytes.len(),
                maximum: limits.max_payload_bytes,
            });
        }
        Ok(Self {
            bytes,
            offset: 0,
            limits,
        })
    }

    fn read_fixed<const N: usize>(&mut self) -> CodecResult<[u8; N]> {
        let start = self.offset;
        let end = start.checked_add(N).ok_or(CodecError::LengthOverflow)?;
        let source = self.bytes.get(start..end).ok_or(CodecError::Truncated {
            offset: start,
            needed: end.saturating_sub(self.bytes.len()),
        })?;
        let mut bytes = [0_u8; N];
        bytes.copy_from_slice(source);
        self.offset = end;
        Ok(bytes)
    }

    fn read_length_delimited(&mut self) -> CodecResult<&'a [u8]> {
        let length_offset = self.offset;
        let length = u32::from_le_bytes(self.read_fixed::<4>()?) as usize;
        if length > self.limits.max_field_bytes {
            return Err(CodecError::FieldTooLarge {
                actual: length,
                maximum: self.limits.max_field_bytes,
            });
        }
        let start = self.offset;
        let end = start
            .checked_add(length)
            .ok_or(CodecError::LengthOverflow)?;
        let value = self.bytes.get(start..end).ok_or(CodecError::Truncated {
            offset: start,
            needed: end.saturating_sub(self.bytes.len()),
        })?;
        self.offset = end;
        debug_assert_eq!(length_offset + 4, start);
        Ok(value)
    }

    pub fn read_i64(&mut self) -> CodecResult<i64> {
        Ok(i64::from_le_bytes(self.read_fixed::<8>()?))
    }

    pub fn read_f64(&mut self) -> CodecResult<f64> {
        let offset = self.offset;
        let value = f64::from_bits(u64::from_le_bytes(self.read_fixed::<8>()?));
        if !value.is_finite() {
            return Err(CodecError::NonFiniteFloat { offset });
        }
        Ok(value)
    }

    pub fn read_bool(&mut self) -> CodecResult<bool> {
        let offset = self.offset;
        match self.read_fixed::<1>()?[0] {
            0 => Ok(false),
            1 => Ok(true),
            actual => Err(CodecError::InvalidBool { offset, actual }),
        }
    }

    pub fn read_string(&mut self) -> CodecResult<String> {
        let offset = self.offset;
        let bytes = self.read_length_delimited()?;
        let value = str::from_utf8(bytes).map_err(|_| CodecError::InvalidUtf8 { offset })?;
        let mut owned = String::new();
        owned
            .try_reserve_exact(value.len())
            .map_err(|_| CodecError::AllocationFailed)?;
        owned.push_str(value);
        Ok(owned)
    }

    pub fn read_bytes(&mut self) -> CodecResult<Vec<u8>> {
        let bytes = self.read_length_delimited()?;
        let mut owned = Vec::new();
        owned
            .try_reserve_exact(bytes.len())
            .map_err(|_| CodecError::AllocationFailed)?;
        owned.extend_from_slice(bytes);
        Ok(owned)
    }

    pub fn read_duration(&mut self) -> CodecResult<Duration> {
        let offset = self.offset;
        let seconds = u64::from_le_bytes(self.read_fixed::<8>()?);
        let nanoseconds = u32::from_le_bytes(self.read_fixed::<4>()?);
        if nanoseconds >= 1_000_000_000 {
            return Err(CodecError::InvalidDuration {
                offset,
                nanoseconds,
            });
        }
        Ok(Duration::new(seconds, nanoseconds))
    }

    pub fn read_nested<T: WireCodec>(&mut self) -> CodecResult<T> {
        T::decode_wire(self)
    }

    pub fn finish(self) -> CodecResult<()> {
        let remaining = self.bytes.len().saturating_sub(self.offset);
        if remaining == 0 {
            Ok(())
        } else {
            Err(CodecError::TrailingBytes { remaining })
        }
    }
}

/// Versioned deterministic payload contract implemented by generated schemas.
pub trait WireCodec: Sized {
    const SCHEMA: &'static str;
    const VERSION: u32;
    const FINGERPRINT: SchemaFingerprint;

    fn encode_wire(&self, encoder: &mut WireEncoder) -> CodecResult<()>;

    fn decode_wire(decoder: &mut WireDecoder<'_>) -> CodecResult<Self>;

    fn encode_bounded(&self, limits: CodecLimits) -> CodecResult<Vec<u8>> {
        let mut encoder = WireEncoder::new(limits);
        self.encode_wire(&mut encoder)?;
        Ok(encoder.finish())
    }

    fn decode_bounded(bytes: &[u8], limits: CodecLimits) -> CodecResult<Self> {
        let mut decoder = WireDecoder::new(bytes, limits)?;
        let value = Self::decode_wire(&mut decoder)?;
        decoder.finish()?;
        Ok(value)
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RemoteMessageError {
    #[error(transparent)]
    Codec(#[from] CodecError),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error(transparent)]
    Ownership(#[from] OwnershipError),
    #[error("wire envelope contains schema '{actual}', expected '{expected}'")]
    UnexpectedSchema {
        expected: &'static str,
        actual: String,
    },
    #[error(
        "wire codec for schema '{schema}' is version {codec}, but the session negotiated {negotiated}"
    )]
    CodecVersionMismatch {
        schema: &'static str,
        codec: u32,
        negotiated: u32,
    },
}

pub type RemoteMessageResult<T> = std::result::Result<T, RemoteMessageError>;

fn validate_typed_envelope<T: WireCodec>(
    session: &NegotiatedTransport,
    envelope: &WireEnvelope,
) -> RemoteMessageResult<()> {
    session.validate_envelope(envelope)?;
    if envelope.schema() != T::SCHEMA {
        return Err(RemoteMessageError::UnexpectedSchema {
            expected: T::SCHEMA,
            actual: envelope.schema().to_owned(),
        });
    }
    if envelope.schema_version() != T::VERSION {
        return Err(RemoteMessageError::CodecVersionMismatch {
            schema: T::SCHEMA,
            codec: T::VERSION,
            negotiated: envelope.schema_version(),
        });
    }
    let negotiated_fingerprint = session
        .schema_fingerprint(T::SCHEMA)
        .ok_or_else(|| TransportError::UnknownSchema(T::SCHEMA.to_owned()))?;
    if negotiated_fingerprint != T::FINGERPRINT {
        return Err(TransportError::SchemaFingerprintMismatch {
            schema: T::SCHEMA.to_owned(),
            version: T::VERSION,
        }
        .into());
    }
    Ok(())
}

/// Encode a typed value and attach the exact negotiated envelope metadata.
pub fn encode_message<T: WireCodec>(
    session: &NegotiatedTransport,
    destination: ShardAddress,
    message_id: MessageId,
    delivery: DeliverySemantics,
    value: &T,
    limits: CodecLimits,
) -> RemoteMessageResult<WireEnvelope> {
    let negotiated = session
        .schemas()
        .get(T::SCHEMA)
        .copied()
        .ok_or_else(|| TransportError::UnknownSchema(T::SCHEMA.to_owned()))?;
    if negotiated != T::VERSION {
        return Err(RemoteMessageError::CodecVersionMismatch {
            schema: T::SCHEMA,
            codec: T::VERSION,
            negotiated,
        });
    }
    let fingerprint = session
        .schema_fingerprint(T::SCHEMA)
        .ok_or_else(|| TransportError::UnknownSchema(T::SCHEMA.to_owned()))?;
    if fingerprint != T::FINGERPRINT {
        return Err(TransportError::SchemaFingerprintMismatch {
            schema: T::SCHEMA.to_owned(),
            version: T::VERSION,
        }
        .into());
    }
    let limits = limits.bounded_by(session.max_payload_bytes())?;
    let payload = value.encode_bounded(limits)?;
    Ok(session.envelope(destination, message_id, T::SCHEMA, delivery, payload)?)
}

/// Validate session metadata and decode one typed payload.
pub fn decode_message<T: WireCodec>(
    session: &NegotiatedTransport,
    envelope: &WireEnvelope,
    limits: CodecLimits,
) -> RemoteMessageResult<T> {
    validate_typed_envelope::<T>(session, envelope)?;
    let limits = limits.bounded_by(session.max_payload_bytes())?;
    Ok(T::decode_bounded(envelope.payload(), limits)?)
}

/// A decoded message paired with the non-cloneable epoch permit that must
/// remain live through its state mutation.
#[derive(Debug)]
pub struct AuthorizedMessage<T> {
    message: T,
    message_id: MessageId,
    destination: ShardAddress,
    schema_version: u32,
    delivery: DeliverySemantics,
    permit: OwnershipPermit,
}

impl<T> AuthorizedMessage<T> {
    pub fn message(&self) -> &T {
        &self.message
    }

    pub fn message_mut(&mut self) -> &mut T {
        &mut self.message
    }

    pub fn message_id(&self) -> &MessageId {
        &self.message_id
    }

    pub fn destination(&self) -> &ShardAddress {
        &self.destination
    }

    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    pub fn delivery(&self) -> DeliverySemantics {
        self.delivery
    }

    pub fn into_parts(
        self,
    ) -> (
        T,
        MessageId,
        ShardAddress,
        u32,
        DeliverySemantics,
        OwnershipPermit,
    ) {
        (
            self.message,
            self.message_id,
            self.destination,
            self.schema_version,
            self.delivery,
            self.permit,
        )
    }
}

/// Validate the negotiated session and current ownership epoch before
/// returning a decoded message. The returned permit fences shard handoff until
/// the caller finishes applying the message.
pub fn authorize_and_decode<T: WireCodec>(
    session: &NegotiatedTransport,
    lease: &ShardLease,
    envelope: &WireEnvelope,
    limits: CodecLimits,
) -> RemoteMessageResult<AuthorizedMessage<T>> {
    validate_typed_envelope::<T>(session, envelope)?;
    let permit = lease.authorize_envelope(envelope)?;
    let limits = limits.bounded_by(session.max_payload_bytes())?;
    let message = T::decode_bounded(envelope.payload(), limits)?;
    Ok(AuthorizedMessage {
        message,
        message_id: envelope.message_id().clone(),
        destination: envelope.destination().clone(),
        schema_version: envelope.schema_version(),
        delivery: envelope.delivery(),
        permit,
    })
}

/// Whether a message-identity source survives process and host loss.
///
/// `Durable` is an adapter contract: the deployment owns evidence that an
/// acknowledged sequence cannot be reused after recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageIdDurability {
    Volatile,
    Durable,
}

pub type MessageIdFuture<'a> =
    Pin<Box<dyn Future<Output = super::TransportResult<MessageId>> + Send + 'a>>;

/// Source of globally unique producer-session sequence numbers.
///
/// At-least-once endpoints require [`MessageIdDurability::Durable`]. The
/// implementation must durably advance or reserve the sequence before
/// returning an identity.
pub trait MessageIdSource: Send + Sync {
    fn durability(&self) -> MessageIdDurability;

    fn next_id(&self) -> MessageIdFuture<'_>;
}

/// Fast in-memory identity source for at-most-once traffic and tests.
///
/// It is deliberately marked volatile and is rejected by an at-least-once
/// endpoint. Start a fresh producer identity after every process restart.
#[derive(Debug)]
pub struct VolatileMessageIds {
    producer: String,
    next: AtomicU64,
}

impl VolatileMessageIds {
    pub fn new(producer: impl Into<String>, first_sequence: u64) -> super::TransportResult<Self> {
        let producer = producer.into();
        let _ = MessageId::new(producer.clone(), first_sequence)?;
        Ok(Self {
            producer,
            next: AtomicU64::new(first_sequence),
        })
    }
}

impl MessageIdSource for VolatileMessageIds {
    fn durability(&self) -> MessageIdDurability {
        MessageIdDurability::Volatile
    }

    fn next_id(&self) -> MessageIdFuture<'_> {
        Box::pin(async move {
            let sequence = self
                .next
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                    value.checked_add(1)
                })
                .map_err(|_| {
                    TransportError::Unavailable(format!(
                        "message sequence for producer '{}' is exhausted",
                        self.producer
                    ))
                })?;
            MessageId::new(self.producer.clone(), sequence)
        })
    }
}

/// Typed, negotiated route from one generated schema to a bounded transport.
///
/// The endpoint owns no socket and performs no retry. It turns a compiler-owned
/// schema into a validated [`WireEnvelope`], selects a fenced destination
/// shard with the same stable hash used by local routers, obtains a unique
/// message identity, and delegates bounded admission to [`Transport`].
pub struct RemoteEndpoint<T> {
    transport: Arc<dyn Transport>,
    destinations: Arc<[ShardAddress]>,
    message_ids: Arc<dyn MessageIdSource>,
    delivery: DeliverySemantics,
    limits: CodecLimits,
    round_robin: Arc<AtomicU64>,
    marker: PhantomData<fn(T)>,
}

impl<T> fmt::Debug for RemoteEndpoint<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteEndpoint")
            .field("destinations", &self.destinations)
            .field("delivery", &self.delivery)
            .field("limits", &self.limits)
            .field("message_id_durability", &self.message_ids.durability())
            .finish_non_exhaustive()
    }
}

impl<T> Clone for RemoteEndpoint<T> {
    fn clone(&self) -> Self {
        Self {
            transport: self.transport.clone(),
            destinations: self.destinations.clone(),
            message_ids: self.message_ids.clone(),
            delivery: self.delivery,
            limits: self.limits,
            round_robin: self.round_robin.clone(),
            marker: PhantomData,
        }
    }
}

impl<T: WireCodec> RemoteEndpoint<T> {
    pub fn new(
        transport: Arc<dyn Transport>,
        destinations: Vec<ShardAddress>,
        message_ids: Arc<dyn MessageIdSource>,
        delivery: DeliverySemantics,
        limits: CodecLimits,
    ) -> RemoteMessageResult<Self> {
        if destinations.is_empty() {
            return Err(TransportError::Configuration(
                "a remote endpoint requires at least one destination shard".into(),
            )
            .into());
        }
        let session = transport.session();
        if !session.delivery().contains(&delivery) {
            return Err(TransportError::DeliverySemanticsMismatch(delivery).into());
        }
        let negotiated = session
            .schemas()
            .get(T::SCHEMA)
            .copied()
            .ok_or_else(|| TransportError::UnknownSchema(T::SCHEMA.to_owned()))?;
        if negotiated != T::VERSION {
            return Err(RemoteMessageError::CodecVersionMismatch {
                schema: T::SCHEMA,
                codec: T::VERSION,
                negotiated,
            });
        }
        let fingerprint = session
            .schema_fingerprint(T::SCHEMA)
            .ok_or_else(|| TransportError::UnknownSchema(T::SCHEMA.to_owned()))?;
        if fingerprint != T::FINGERPRINT {
            return Err(TransportError::SchemaFingerprintMismatch {
                schema: T::SCHEMA.to_owned(),
                version: T::VERSION,
            }
            .into());
        }
        let limits = limits.bounded_by(session.max_payload_bytes())?;
        if delivery == DeliverySemantics::AtLeastOnce
            && message_ids.durability() != MessageIdDurability::Durable
        {
            return Err(TransportError::Configuration(
                "at-least-once delivery requires a durable message-identity source".into(),
            )
            .into());
        }

        let first = destinations
            .first()
            .expect("non-empty destination set was checked");
        let first_key = first.key();
        let mut shards = BTreeSet::new();
        for destination in &destinations {
            let key = destination.key();
            if key.deployment() != first_key.deployment()
                || key.placement_group() != first_key.placement_group()
                || key.process() != first_key.process()
            {
                return Err(TransportError::Configuration(
                    "remote endpoint destinations must belong to one deployment, placement \
                     group, and process"
                        .into(),
                )
                .into());
            }
            if !shards.insert(key.shard()) {
                return Err(TransportError::Configuration(format!(
                    "remote endpoint contains duplicate shard {}",
                    key.shard()
                ))
                .into());
            }
        }

        Ok(Self {
            transport,
            destinations: destinations.into(),
            message_ids,
            delivery,
            limits,
            round_robin: Arc::new(AtomicU64::new(0)),
            marker: PhantomData,
        })
    }

    pub fn destinations(&self) -> &[ShardAddress] {
        &self.destinations
    }

    pub fn delivery(&self) -> DeliverySemantics {
        self.delivery
    }

    pub fn limits(&self) -> CodecLimits {
        self.limits
    }

    pub fn session(&self) -> &NegotiatedTransport {
        self.transport.session()
    }

    async fn prepare_index(&self, index: usize, value: &T) -> RemoteMessageResult<WireEnvelope> {
        let destination = self
            .destinations
            .get(index)
            .ok_or_else(|| {
                TransportError::Configuration(format!(
                    "remote destination index {index} is out of range"
                ))
            })?
            .clone();
        let message_id = self.message_ids.next_id().await?;
        encode_message(
            self.transport.session(),
            destination,
            message_id,
            self.delivery,
            value,
            self.limits,
        )
    }

    /// Build, but do not admit, the next round-robin envelope. Durable
    /// outboxes use this to persist the exact bytes before transport I/O.
    pub async fn prepare_round_robin(&self, value: &T) -> RemoteMessageResult<WireEnvelope> {
        let sequence = self.round_robin.fetch_add(1, Ordering::Relaxed);
        let index = sequence % self.destinations.len() as u64;
        self.prepare_index(index as usize, value).await
    }

    pub async fn prepare_by_key<K: StableRouteKey + ?Sized>(
        &self,
        key: &K,
        value: &T,
    ) -> RemoteMessageResult<WireEnvelope> {
        let index = key.stable_route_hash() % self.destinations.len() as u64;
        self.prepare_index(index as usize, value).await
    }

    pub async fn prepare_broadcast(&self, value: &T) -> RemoteMessageResult<Vec<WireEnvelope>> {
        let mut envelopes = Vec::new();
        envelopes
            .try_reserve_exact(self.destinations.len())
            .map_err(|_| CodecError::AllocationFailed)?;
        for index in 0..self.destinations.len() {
            envelopes.push(self.prepare_index(index, value).await?);
        }
        Ok(envelopes)
    }

    pub async fn admit_round_robin(
        &self,
        value: &T,
        admission: RemoteAdmission,
    ) -> RemoteMessageResult<TransportOutcome> {
        admission.validate()?;
        let envelope = self.prepare_round_robin(value).await?;
        Ok(self.transport.admit(envelope, admission).await?)
    }

    pub async fn admit_by_key<K: StableRouteKey + ?Sized>(
        &self,
        key: &K,
        value: &T,
        admission: RemoteAdmission,
    ) -> RemoteMessageResult<TransportOutcome> {
        admission.validate()?;
        let envelope = self.prepare_by_key(key, value).await?;
        Ok(self.transport.admit(envelope, admission).await?)
    }

    /// Admit one independently identified envelope to every destination.
    pub async fn admit_broadcast(
        &self,
        value: &T,
        admission: RemoteAdmission,
    ) -> RemoteMessageResult<Vec<TransportOutcome>> {
        let mut outcomes = Vec::new();
        outcomes
            .try_reserve_exact(self.destinations.len())
            .map_err(|_| CodecError::AllocationFailed)?;
        admission.validate()?;
        for envelope in self.prepare_broadcast(value).await? {
            outcomes.push(self.transport.admit(envelope, admission).await?);
        }
        Ok(outcomes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::distributed::{
        NodeId, OwnershipEpoch, ShardKey, TransportFuture, TransportManifest, VersionRange,
        DISTRIBUTED_PROTOCOL_VERSION,
    };
    use std::sync::Mutex;

    #[derive(Debug, PartialEq)]
    struct AllFields {
        integer: i64,
        float: f64,
        boolean: bool,
        string: String,
        bytes: Vec<u8>,
        duration: Duration,
    }

    impl WireCodec for AllFields {
        const SCHEMA: &'static str = "AllFields";
        const VERSION: u32 = 1;
        const FINGERPRINT: SchemaFingerprint = SchemaFingerprint::new([7; 32]);

        fn encode_wire(&self, encoder: &mut WireEncoder) -> CodecResult<()> {
            encoder.write_i64(self.integer)?;
            encoder.write_f64(self.float)?;
            encoder.write_bool(self.boolean)?;
            encoder.write_string(&self.string)?;
            encoder.write_bytes(&self.bytes)?;
            encoder.write_duration(self.duration)
        }

        fn decode_wire(decoder: &mut WireDecoder<'_>) -> CodecResult<Self> {
            Ok(Self {
                integer: decoder.read_i64()?,
                float: decoder.read_f64()?,
                boolean: decoder.read_bool()?,
                string: decoder.read_string()?,
                bytes: decoder.read_bytes()?,
                duration: decoder.read_duration()?,
            })
        }
    }

    fn limits() -> CodecLimits {
        CodecLimits::new(1024, 128).expect("limits")
    }

    fn manifest() -> TransportManifest {
        let mut manifest = TransportManifest::new(
            VersionRange::exact(DISTRIBUTED_PROTOCOL_VERSION).expect("protocol"),
            1024,
        )
        .expect("manifest");
        manifest
            .register_schema("AllFields", 1, AllFields::FINGERPRINT)
            .expect("register schema");
        manifest.enable_delivery(DeliverySemantics::AtMostOnce);
        manifest
    }

    fn address(epoch: u64) -> ShardAddress {
        ShardAddress::new(
            ShardKey::new("prod", "workers", "P", 0).expect("key"),
            NodeId::new("node-a").expect("node"),
            OwnershipEpoch::new(epoch).expect("epoch"),
        )
    }

    #[test]
    fn deterministic_round_trip_covers_every_primitive() {
        let value = AllFields {
            integer: -42,
            float: 1.5,
            boolean: true,
            string: "sigil".into(),
            bytes: vec![0, 1, 255],
            duration: Duration::new(7, 9),
        };
        let encoded = value.encode_bounded(limits()).expect("encode");
        assert_eq!(&encoded[0..8], &(-42_i64).to_le_bytes());
        assert_eq!(&encoded[8..16], &1.5_f64.to_bits().to_le_bytes());
        assert_eq!(encoded[16], 1);
        assert_eq!(&encoded[17..21], &5_u32.to_le_bytes());
        assert_eq!(
            AllFields::decode_bounded(&encoded, limits()).expect("decode"),
            value
        );
        assert_eq!(
            value.encode_bounded(limits()).expect("encode again"),
            encoded,
            "the same value must always have the same wire bytes"
        );
    }

    #[test]
    fn decoder_rejects_adversarial_lengths_and_noncanonical_values() {
        let tiny = CodecLimits::new(32, 8).expect("limits");
        assert!(matches!(
            WireDecoder::new(&[9, 0, 0, 0], tiny)
                .expect("decoder")
                .read_bytes(),
            Err(CodecError::FieldTooLarge {
                actual: 9,
                maximum: 8
            })
        ));
        assert!(matches!(
            WireDecoder::new(&[8, 0, 0, 0, 1], tiny)
                .expect("decoder")
                .read_bytes(),
            Err(CodecError::Truncated { .. })
        ));
        assert!(matches!(
            WireDecoder::new(&[2], tiny).expect("decoder").read_bool(),
            Err(CodecError::InvalidBool { actual: 2, .. })
        ));
        assert!(matches!(
            WireDecoder::new(&f64::NAN.to_bits().to_le_bytes(), tiny)
                .expect("decoder")
                .read_f64(),
            Err(CodecError::NonFiniteFloat { .. })
        ));
        let mut invalid_duration = 0_u64.to_le_bytes().to_vec();
        invalid_duration.extend_from_slice(&1_000_000_000_u32.to_le_bytes());
        assert!(matches!(
            WireDecoder::new(&invalid_duration, tiny)
                .expect("decoder")
                .read_duration(),
            Err(CodecError::InvalidDuration { .. })
        ));
        assert!(matches!(
            WireDecoder::new(&[1, 0, 0, 0, 0xff], tiny)
                .expect("decoder")
                .read_string(),
            Err(CodecError::InvalidUtf8 { .. })
        ));
    }

    #[test]
    fn complete_decode_rejects_trailing_bytes_and_payload_overflow() {
        let value = AllFields {
            integer: 1,
            float: 2.0,
            boolean: false,
            string: String::new(),
            bytes: Vec::new(),
            duration: Duration::ZERO,
        };
        let mut encoded = value.encode_bounded(limits()).expect("encode");
        encoded.push(0);
        assert_eq!(
            AllFields::decode_bounded(&encoded, limits()),
            Err(CodecError::TrailingBytes { remaining: 1 })
        );
        assert!(matches!(
            value.encode_bounded(CodecLimits::new(8, 8).expect("limits")),
            Err(CodecError::PayloadTooLarge { .. })
        ));
    }

    #[test]
    fn typed_envelopes_hold_the_epoch_permit_until_application_finishes() {
        let session = NegotiatedTransport::negotiate(&manifest(), &manifest()).expect("session");
        let destination = address(1);
        let value = AllFields {
            integer: 1,
            float: 2.0,
            boolean: true,
            string: "bounded".into(),
            bytes: vec![1, 2],
            duration: Duration::from_millis(5),
        };
        let envelope = encode_message(
            &session,
            destination.clone(),
            MessageId::new("producer", 1).expect("message id"),
            DeliverySemantics::AtMostOnce,
            &value,
            limits(),
        )
        .expect("envelope");
        let lease = ShardLease::serving(destination);
        let authorized = authorize_and_decode::<AllFields>(&session, &lease, &envelope, limits())
            .expect("authorized");
        assert_eq!(authorized.message(), &value);
        assert_eq!(lease.in_flight(), 1);
        drop(authorized);
        assert_eq!(lease.in_flight(), 0);
    }

    #[derive(Debug)]
    struct RecordingTransport {
        session: NegotiatedTransport,
        received: Mutex<Vec<WireEnvelope>>,
    }

    impl Transport for RecordingTransport {
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
                self.received
                    .lock()
                    .map_err(|_| TransportError::Unavailable("recording lock poisoned".into()))?
                    .push(envelope);
                Ok(TransportOutcome::Accepted)
            })
        }
    }

    #[tokio::test]
    async fn typed_remote_endpoint_routes_and_allocates_unique_identities() {
        let session = NegotiatedTransport::negotiate(&manifest(), &manifest()).expect("session");
        let transport = Arc::new(RecordingTransport {
            session,
            received: Mutex::new(Vec::new()),
        });
        let endpoint = RemoteEndpoint::<AllFields>::new(
            transport.clone(),
            vec![
                address(1),
                ShardAddress::new(
                    ShardKey::new("prod", "workers", "P", 1).expect("key"),
                    NodeId::new("node-b").expect("node"),
                    OwnershipEpoch::new(4).expect("epoch"),
                ),
            ],
            Arc::new(VolatileMessageIds::new("fresh-process-session", 10).expect("message ids")),
            DeliverySemantics::AtMostOnce,
            limits(),
        )
        .expect("endpoint");
        let value = AllFields {
            integer: 1,
            float: 2.0,
            boolean: true,
            string: "route".into(),
            bytes: vec![],
            duration: Duration::ZERO,
        };

        endpoint
            .admit_round_robin(&value, RemoteAdmission::Shed)
            .await
            .expect("first");
        endpoint
            .admit_round_robin(&value, RemoteAdmission::Shed)
            .await
            .expect("second");
        endpoint
            .admit_by_key("stable-key", &value, RemoteAdmission::Shed)
            .await
            .expect("affinity");
        let broadcast = endpoint
            .admit_broadcast(&value, RemoteAdmission::Shed)
            .await
            .expect("broadcast");
        assert_eq!(broadcast.len(), 2);

        let received = transport.received.lock().expect("recording lock");
        assert_eq!(received.len(), 5);
        assert_eq!(received[0].destination().key().shard(), 0);
        assert_eq!(received[1].destination().key().shard(), 1);
        assert_eq!(
            received
                .iter()
                .map(|envelope| envelope.message_id().sequence())
                .collect::<Vec<_>>(),
            vec![10, 11, 12, 13, 14]
        );
    }

    #[test]
    fn at_least_once_endpoint_rejects_volatile_identity_state() {
        let mut at_least_once = manifest();
        at_least_once.enable_delivery(DeliverySemantics::AtLeastOnce);
        let session = NegotiatedTransport::negotiate(&at_least_once, &at_least_once)
            .expect("at-least-once session");
        let transport = Arc::new(RecordingTransport {
            session,
            received: Mutex::new(Vec::new()),
        });
        assert!(matches!(
            RemoteEndpoint::<AllFields>::new(
                transport,
                vec![address(1)],
                Arc::new(VolatileMessageIds::new("volatile", 1).expect("ids")),
                DeliverySemantics::AtLeastOnce,
                limits(),
            ),
            Err(RemoteMessageError::Transport(
                TransportError::Configuration(message)
            )) if message.contains("durable message-identity")
        ));
    }
}
