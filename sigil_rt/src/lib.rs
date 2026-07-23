//! Minimal Sigil runtime support for generated code.

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SigilError {
    #[error("timeout")]
    Timeout,
    #[error("transform error: {0}")]
    Transform(String),
    #[error("schema or validation failure")]
    Schema,
    #[error("invalid runtime configuration: {0}")]
    Configuration(String),
    #[error("actor stopped before accepting the message")]
    ActorStopped,
    #[error("actor task panicked")]
    ActorPanicked,
    #[error("actor task was cancelled")]
    ActorCancelled,
}

pub type Result<T> = std::result::Result<T, SigilError>;

/// Outcome of a back-pressured send.
///
/// `Shed` is not an error: it is the policy doing exactly what was declared.
/// It is counted so the loss is always visible in the run report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendOutcome {
    Delivered,
    Shed,
}

/// Per-actor message accounting, returned alongside final state after a
/// normal actor exit. If the actor panics, no final state or accounting
/// snapshot is available and `join_actor` returns `ActorPanicked`.
#[derive(Debug, Default, Clone, Copy)]
pub struct ActorStats {
    pub handled: u64,
    pub dropped: u64,
    /// Outbound messages shed because a downstream queue was full.
    pub shed: u64,
}

/// Shard router for typed actor outboxes. Lives inside a single actor's
/// task-local state, so round-robin needs no atomics and hashing no locks.
pub struct Router<H> {
    shards: Vec<H>,
    rr: usize,
}

impl<H> Router<H> {
    /// Construct a router with at least one destination shard.
    ///
    /// Keeping the shard vector non-empty is the invariant that makes
    /// `round_robin` and `by_key` panic-free.
    pub fn new(shards: Vec<H>) -> Result<Self> {
        if shards.is_empty() {
            return Err(SigilError::Configuration(
                "a router requires at least one destination shard".into(),
            ));
        }
        Ok(Self { shards, rr: 0 })
    }

    /// Even distribution: successive calls walk the shard ring.
    pub fn round_robin(&mut self) -> &H {
        let h = &self.shards[self.rr % self.shards.len()];
        self.rr = self.rr.wrapping_add(1);
        h
    }

    /// Key affinity: identical keys always land on the same shard, so
    /// per-key ordering and shard-local state remain coherent.
    pub fn by_key<K: std::hash::Hash + ?Sized>(&self, key: &K) -> &H {
        use std::hash::BuildHasher;
        let state =
            std::hash::BuildHasherDefault::<std::collections::hash_map::DefaultHasher>::default();
        &self.shards[(state.hash_one(key) as usize) % self.shards.len()]
    }

    /// Every shard, for broadcast delivery.
    pub fn shards(&self) -> &[H] {
        &self.shards
    }
}

/// Validate a capacity before calling Tokio's bounded-channel constructor.
///
/// Tokio panics for zero and overlarge capacities. Generated code calls this
/// first so deployment configuration errors are returned as typed errors.
pub fn validate_channel_capacity(capacity: usize) -> Result<()> {
    if capacity == 0 {
        return Err(SigilError::Configuration(
            "actor inbox capacity must be at least 1".into(),
        ));
    }
    if capacity > tokio::sync::Semaphore::MAX_PERMITS {
        return Err(SigilError::Configuration(format!(
            "actor inbox capacity {capacity} exceeds the runtime maximum {}",
            tokio::sync::Semaphore::MAX_PERMITS
        )));
    }
    Ok(())
}

/// Await an actor without turning a task panic into another panic.
///
/// Tokio already catches task panics and reports them through `JoinHandle`.
/// This helper preserves that distinction as a typed Sigil error.
pub async fn join_actor<T>(
    join: tokio::task::JoinHandle<(T, ActorStats)>,
) -> Result<(T, ActorStats)> {
    join_task(join).await
}

/// Await any Tokio task and retain panic versus cancellation in the error.
pub async fn join_task<T>(join: tokio::task::JoinHandle<T>) -> Result<T> {
    match join.await {
        Ok(done) => Ok(done),
        Err(err) if err.is_panic() => Err(SigilError::ActorPanicked),
        Err(_) => Err(SigilError::ActorCancelled),
    }
}

/// Back-pressure helpers over a raw channel sender.
///
/// These wrap the three declared policies so generated code never open-codes
/// queue handling:
///   `block`    — await capacity (unbounded wait, no loss)
///   `shed`     — never wait; drop when full (bounded at O(1), lossy)
///   `deadline` — wait up to N ms, then drop (bounded, lossy only past N)
pub mod backpressure {
    use super::{SendOutcome, SigilError};
    use std::time::Duration;
    use tokio::sync::mpsc::Sender;

    pub async fn block<T>(tx: &Sender<T>, msg: T) -> crate::Result<SendOutcome> {
        tx.send(msg)
            .await
            .map(|_| SendOutcome::Delivered)
            .map_err(|_| SigilError::ActorStopped)
    }

    pub fn shed<T>(tx: &Sender<T>, msg: T) -> crate::Result<SendOutcome> {
        use tokio::sync::mpsc::error::TrySendError;
        match tx.try_send(msg) {
            Ok(()) => Ok(SendOutcome::Delivered),
            Err(TrySendError::Full(_)) => Ok(SendOutcome::Shed),
            Err(TrySendError::Closed(_)) => Err(SigilError::ActorStopped),
        }
    }

    pub async fn deadline<T>(tx: &Sender<T>, msg: T, ms: u64) -> crate::Result<SendOutcome> {
        match tokio::time::timeout(Duration::from_millis(ms), tx.send(msg)).await {
            Ok(Ok(())) => Ok(SendOutcome::Delivered),
            Ok(Err(_)) => Err(SigilError::ActorStopped),
            Err(_) => Ok(SendOutcome::Shed), // deadline expired: declared loss
        }
    }
}

/// Deterministic-seed fault injection for external residual stages.
///
/// Disabled by default (zero latency, zero faults). Configure via env:
///   SIGIL_CHAOS_FAIL_PCT   — percent of external calls that fail (0-100)
///   SIGIL_CHAOS_LATENCY_MS — max injected latency per external call
///
/// Faults injected here exercise the @timeout / @recover paths the compiler
/// verified at build time. Counters are runtime observability only; generated
/// user code remains lock- and atomic-free.
pub mod chaos {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    use std::time::Duration;

    static SEED: AtomicU64 = AtomicU64::new(0x9E37_79B9_7F4A_7C15);
    static CALLS: AtomicU64 = AtomicU64::new(0);
    static FAULTS: AtomicU64 = AtomicU64::new(0);
    static RECOVERIES: AtomicU64 = AtomicU64::new(0);
    static RETRIES: AtomicU64 = AtomicU64::new(0);
    static SHED: AtomicU64 = AtomicU64::new(0);

    /// splitmix64 — deterministic sequence, no external deps.
    fn next() -> u64 {
        let mut z = SEED
            .fetch_add(0x9E37_79B9_7F4A_7C15, Relaxed)
            .wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn env_u64(key: &str, default: u64) -> u64 {
        std::env::var(key)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }

    pub fn fail_pct() -> u64 {
        env_u64("SIGIL_CHAOS_FAIL_PCT", 0).min(100)
    }

    pub fn max_latency_ms() -> u64 {
        env_u64("SIGIL_CHAOS_LATENCY_MS", 0)
    }

    /// Percent of external calls that experience injected latency.
    pub fn slow_pct() -> u64 {
        env_u64("SIGIL_CHAOS_SLOW_PCT", 25).min(100)
    }

    /// Injected behavior for one external residual stage call.
    pub async fn external_stage(name: &'static str) -> crate::Result<()> {
        CALLS.fetch_add(1, Relaxed);
        let lat = max_latency_ms();
        if lat > 0 && next() % 100 < slow_pct() {
            let range = lat.saturating_add(1);
            tokio::time::sleep(Duration::from_millis(next() % range)).await;
        }
        let pct = fail_pct();
        if pct > 0 && next() % 100 < pct {
            FAULTS.fetch_add(1, Relaxed);
            return Err(crate::SigilError::Transform(format!(
                "{name}: injected fault"
            )));
        }
        Ok(())
    }

    /// Called by generated code whenever a send is shed by policy.
    pub fn note_shed(_target: &'static str) {
        SHED.fetch_add(1, Relaxed);
    }

    pub fn shed_total() -> u64 {
        SHED.load(Relaxed)
    }

    /// Called by generated code whenever a stage re-attempt begins.
    pub fn note_retry(_stage: &'static str) {
        RETRIES.fetch_add(1, Relaxed);
    }

    /// Called by generated code whenever a @recover fallback path is taken.
    pub fn note_recovery(_stage: &'static str) {
        RECOVERIES.fetch_add(1, Relaxed);
    }

    pub fn recoveries() -> u64 {
        RECOVERIES.load(Relaxed)
    }

    pub fn report() -> String {
        format!(
            "chaos: external calls={} injected faults={} retries={} recover paths taken={} shed={} (fail_pct={} max_latency={}ms)",
            CALLS.load(Relaxed),
            FAULTS.load(Relaxed),
            RETRIES.load(Relaxed),
            RECOVERIES.load(Relaxed),
            SHED.load(Relaxed),
            fail_pct(),
            max_latency_ms()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_rejects_an_empty_shard_set() {
        let err = Router::<u8>::new(Vec::new())
            .err()
            .expect("an empty router must be rejected");
        assert!(matches!(err, SigilError::Configuration(_)));
    }

    #[test]
    fn router_round_robin_and_affinity_are_stable() {
        let mut router = Router::new(vec![10, 20, 30]).expect("non-empty router");
        assert_eq!(*router.round_robin(), 10);
        assert_eq!(*router.round_robin(), 20);
        assert_eq!(*router.round_robin(), 30);
        assert_eq!(*router.round_robin(), 10);
        assert_eq!(router.by_key("account-7"), router.by_key("account-7"));
    }

    #[test]
    fn channel_capacity_is_checked_without_panicking() {
        assert!(matches!(
            validate_channel_capacity(0),
            Err(SigilError::Configuration(_))
        ));
        assert!(matches!(
            validate_channel_capacity(tokio::sync::Semaphore::MAX_PERMITS + 1),
            Err(SigilError::Configuration(_))
        ));
        assert_eq!(validate_channel_capacity(1), Ok(()));
    }

    #[tokio::test]
    async fn backpressure_reports_closed_channels() {
        let (tx, rx) = tokio::sync::mpsc::channel::<u8>(1);
        drop(rx);
        assert_eq!(
            backpressure::block(&tx, 1).await,
            Err(SigilError::ActorStopped)
        );
        assert_eq!(backpressure::shed(&tx, 1), Err(SigilError::ActorStopped));
        assert_eq!(
            backpressure::deadline(&tx, 1, 1).await,
            Err(SigilError::ActorStopped)
        );
    }

    #[tokio::test]
    async fn actor_panics_are_typed_join_errors() {
        let join = tokio::spawn(async move {
            panic!("test panic");
            #[allow(unreachable_code)]
            ((), ActorStats::default())
        });
        assert!(matches!(
            join_actor(join).await,
            Err(SigilError::ActorPanicked)
        ));
    }
}
