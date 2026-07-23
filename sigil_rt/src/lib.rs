//! Minimal Sigil runtime support for generated code.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SigilError {
    #[error("timeout")]
    Timeout,
    #[error("transform error: {0}")]
    Transform(String),
    #[error("schema or validation failure")]
    Schema,
}

pub type Result<T> = std::result::Result<T, SigilError>;

/// Outcome of a back-pressured send.
///
/// `Shed` is not an error: it is the policy doing exactly what was declared.
/// It is counted so the loss is always visible in the run report.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SendOutcome {
    Delivered,
    Shed,
}

/// Per-actor message accounting, returned alongside final state at join().
/// handled + dropped equals every message the actor ever received, so
/// system-wide conservation checks stay exact even under fault injection.
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
    pub fn new(shards: Vec<H>) -> Self {
        Self { shards, rr: 0 }
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
        use std::hash::{BuildHasher, Hasher};
        let mut hasher = std::hash::BuildHasherDefault::<std::collections::hash_map::DefaultHasher>::default().build_hasher();
        key.hash(&mut hasher);
        &self.shards[(hasher.finish() as usize) % self.shards.len()]
    }

    /// Every shard, for broadcast delivery.
    pub fn shards(&self) -> &[H] {
        &self.shards
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
            .map_err(|_| SigilError::Transform("actor stopped".into()))
    }

    pub fn shed<T>(tx: &Sender<T>, msg: T) -> crate::Result<SendOutcome> {
        use tokio::sync::mpsc::error::TrySendError;
        match tx.try_send(msg) {
            Ok(()) => Ok(SendOutcome::Delivered),
            Err(TrySendError::Full(_)) => Ok(SendOutcome::Shed),
            Err(TrySendError::Closed(_)) => {
                Err(SigilError::Transform("actor stopped".into()))
            }
        }
    }

    pub async fn deadline<T>(
        tx: &Sender<T>,
        msg: T,
        ms: u64,
    ) -> crate::Result<SendOutcome> {
        match tokio::time::timeout(Duration::from_millis(ms), tx.send(msg)).await {
            Ok(Ok(())) => Ok(SendOutcome::Delivered),
            Ok(Err(_)) => Err(SigilError::Transform("actor stopped".into())),
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
            tokio::time::sleep(Duration::from_millis(next() % (lat + 1))).await;
        }
        let pct = fail_pct();
        if pct > 0 && next() % 100 < pct {
            FAULTS.fetch_add(1, Relaxed);
            return Err(crate::SigilError::Transform(format!("{name}: injected fault")));
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
