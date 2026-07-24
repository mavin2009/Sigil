//! Minimal Sigil runtime support for generated code.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;
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
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ActorStats {
    pub handled: u64,
    pub dropped: u64,
    /// Outbound messages shed because a downstream queue was full.
    pub shed: u64,
}

impl ActorStats {
    pub fn record_handled(&mut self) {
        self.handled = self.handled.saturating_add(1);
    }

    pub fn record_dropped(&mut self) {
        self.dropped = self.dropped.saturating_add(1);
    }

    pub fn record_shed(&mut self, count: u64) {
        self.shed = self.shed.saturating_add(count);
    }
}

/// Point-in-time actor counters. Counters saturate at `u64::MAX`; they never
/// wrap and make failures disappear.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ActorSnapshot {
    pub accepted: u64,
    pub handled: u64,
    pub dropped: u64,
    pub shed: u64,
}

impl ActorSnapshot {
    pub fn undrained(self) -> u64 {
        self.accepted
            .saturating_sub(self.handled.saturating_add(self.dropped))
    }
}

const STATUS_RUNNING: u8 = 0;
const STATUS_STOPPED: u8 = 1;
const STATUS_PANICKED: u8 = 2;
const STATUS_CANCELLED: u8 = 3;

#[derive(Debug)]
struct TelemetryInner {
    accepted: AtomicU64,
    handled: AtomicU64,
    dropped: AtomicU64,
    shed: AtomicU64,
    status: AtomicU8,
}

/// Cloneable live telemetry shared by a generated handle, its actor task, and
/// the supervisor. It contains counters only, never actor state.
#[derive(Debug, Clone)]
pub struct ActorTelemetry {
    inner: Arc<TelemetryInner>,
}

impl Default for ActorTelemetry {
    fn default() -> Self {
        Self::new()
    }
}

fn saturating_atomic_add(counter: &AtomicU64, value: u64) {
    let _ = counter.fetch_update(Ordering::Release, Ordering::Relaxed, |current| {
        Some(current.saturating_add(value))
    });
}

impl ActorTelemetry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(TelemetryInner {
                accepted: AtomicU64::new(0),
                handled: AtomicU64::new(0),
                dropped: AtomicU64::new(0),
                shed: AtomicU64::new(0),
                status: AtomicU8::new(STATUS_RUNNING),
            }),
        }
    }

    pub fn snapshot(&self) -> ActorSnapshot {
        // Processing counters are Release-published after channel receipt.
        // Read them before accepted so observing a processed message also
        // carries the acceptance publication that preceded its hand-off.
        let handled = self.inner.handled.load(Ordering::Acquire);
        let dropped = self.inner.dropped.load(Ordering::Acquire);
        ActorSnapshot {
            accepted: self.inner.accepted.load(Ordering::Acquire),
            handled,
            dropped,
            shed: self.inner.shed.load(Ordering::Acquire),
        }
    }

    pub fn is_running(&self) -> bool {
        self.inner.status.load(Ordering::Acquire) == STATUS_RUNNING
    }

    pub fn note_accepted(&self) {
        saturating_atomic_add(&self.inner.accepted, 1);
    }

    pub fn note_handled(&self) {
        saturating_atomic_add(&self.inner.handled, 1);
    }

    pub fn note_dropped(&self) {
        saturating_atomic_add(&self.inner.dropped, 1);
    }

    pub fn note_shed(&self, count: u64) {
        saturating_atomic_add(&self.inner.shed, count);
    }

    pub fn mark_stopped(&self) {
        self.inner.status.store(STATUS_STOPPED, Ordering::Release);
    }

    fn mark_panicked(&self) {
        self.inner.status.store(STATUS_PANICKED, Ordering::Release);
    }

    fn mark_cancelled(&self) {
        self.inner.status.store(STATUS_CANCELLED, Ordering::Release);
    }
}

/// Telemetry-aware bounded inbox sender used by generated handles and all
/// back-pressure helpers.
pub struct ActorSender<T> {
    tx: tokio::sync::mpsc::Sender<T>,
    telemetry: ActorTelemetry,
}

impl<T> Clone for ActorSender<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            telemetry: self.telemetry.clone(),
        }
    }
}

impl<T> ActorSender<T> {
    pub fn new(tx: tokio::sync::mpsc::Sender<T>, telemetry: ActorTelemetry) -> Self {
        Self { tx, telemetry }
    }

    pub async fn send(
        &self,
        message: T,
    ) -> std::result::Result<(), tokio::sync::mpsc::error::SendError<T>> {
        let permit = match self.tx.reserve().await {
            Ok(permit) => permit,
            Err(_) => return Err(tokio::sync::mpsc::error::SendError(message)),
        };
        self.telemetry.note_accepted();
        permit.send(message);
        Ok(())
    }

    pub fn try_send(
        &self,
        message: T,
    ) -> std::result::Result<(), tokio::sync::mpsc::error::TrySendError<T>> {
        use tokio::sync::mpsc::error::TrySendError;

        let permit = match self.tx.try_reserve() {
            Ok(permit) => permit,
            Err(TrySendError::Full(())) => return Err(TrySendError::Full(message)),
            Err(TrySendError::Closed(())) => return Err(TrySendError::Closed(message)),
        };
        self.telemetry.note_accepted();
        permit.send(message);
        Ok(())
    }

    pub fn telemetry(&self) -> &ActorTelemetry {
        &self.telemetry
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorTermination {
    Stopped,
    Panicked,
    Cancelled,
    ShutdownDeadline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Accounting {
    Complete(ActorStats),
    Incomplete {
        last_snapshot: ActorSnapshot,
        undrained: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorReport {
    pub name: String,
    pub termination: ActorTermination,
    pub accounting: Accounting,
}

/// Must-use owner of an actor join. Dropping it aborts the task, so a lost
/// join handle can never silently detach work from accounting.
#[must_use = "dropping an actor task aborts it; join it or register it with Supervisor"]
pub struct ActorTask<T> {
    name: String,
    join: Option<tokio::task::JoinHandle<(T, ActorStats)>>,
    telemetry: ActorTelemetry,
}

type ActorTaskParts<T> = (
    String,
    tokio::task::JoinHandle<(T, ActorStats)>,
    ActorTelemetry,
);

impl<T> ActorTask<T> {
    pub fn new(
        name: impl Into<String>,
        join: tokio::task::JoinHandle<(T, ActorStats)>,
        telemetry: ActorTelemetry,
    ) -> Self {
        Self {
            name: name.into(),
            join: Some(join),
            telemetry,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn snapshot(&self) -> ActorSnapshot {
        self.telemetry.snapshot()
    }

    pub fn abort(&self) {
        if let Some(join) = &self.join {
            join.abort();
        }
        self.telemetry.mark_cancelled();
    }

    pub async fn join(mut self) -> Result<(T, ActorStats)> {
        let join = self
            .join
            .take()
            .ok_or_else(|| SigilError::Configuration("actor task was already consumed".into()))?;
        let outcome = join_actor(join).await;
        match &outcome {
            Ok(_) => self.telemetry.mark_stopped(),
            Err(SigilError::ActorPanicked) => self.telemetry.mark_panicked(),
            Err(SigilError::ActorCancelled) => self.telemetry.mark_cancelled(),
            Err(_) => {}
        }
        outcome
    }

    fn into_parts(mut self) -> Result<ActorTaskParts<T>> {
        let join = self
            .join
            .take()
            .ok_or_else(|| SigilError::Configuration("actor task was already consumed".into()))?;
        Ok((self.name.clone(), join, self.telemetry.clone()))
    }
}

impl<T> Drop for ActorTask<T> {
    fn drop(&mut self) {
        if let Some(join) = &self.join {
            join.abort();
            self.telemetry.mark_cancelled();
        }
    }
}

struct SupervisedActor {
    abort: tokio::task::AbortHandle,
    telemetry: ActorTelemetry,
}

/// Runtime registry for observing actor failure as it happens and enforcing a
/// bounded shutdown. Actor state stays task-local and is intentionally not
/// retained by this type-erased operational API.
pub struct Supervisor {
    tasks: tokio::task::JoinSet<ActorReport>,
    actors: BTreeMap<String, SupervisedActor>,
}

impl Default for Supervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl Supervisor {
    pub fn new() -> Self {
        Self {
            tasks: tokio::task::JoinSet::new(),
            actors: BTreeMap::new(),
        }
    }

    pub fn register<T: Send + 'static>(&mut self, task: ActorTask<T>) -> Result<()> {
        if self.actors.contains_key(task.name()) {
            return Err(SigilError::Configuration(format!(
                "actor '{}' is already registered",
                task.name()
            )));
        }
        let (name, join, telemetry) = task.into_parts()?;
        let report_name = name.clone();
        let report_telemetry = telemetry.clone();
        let abort = join.abort_handle();
        self.tasks.spawn(async move {
            match join.await {
                Ok((_state, stats)) => {
                    report_telemetry.mark_stopped();
                    ActorReport {
                        name: report_name,
                        termination: ActorTermination::Stopped,
                        accounting: Accounting::Complete(stats),
                    }
                }
                Err(error) if error.is_panic() => {
                    report_telemetry.mark_panicked();
                    let snapshot = report_telemetry.snapshot();
                    ActorReport {
                        name: report_name,
                        termination: ActorTermination::Panicked,
                        accounting: Accounting::Incomplete {
                            last_snapshot: snapshot,
                            undrained: snapshot.undrained(),
                        },
                    }
                }
                Err(_) => {
                    report_telemetry.mark_cancelled();
                    let snapshot = report_telemetry.snapshot();
                    ActorReport {
                        name: report_name,
                        termination: ActorTermination::Cancelled,
                        accounting: Accounting::Incomplete {
                            last_snapshot: snapshot,
                            undrained: snapshot.undrained(),
                        },
                    }
                }
            }
        });
        self.actors
            .insert(name, SupervisedActor { abort, telemetry });
        Ok(())
    }

    pub fn snapshots(&self) -> BTreeMap<String, ActorSnapshot> {
        self.actors
            .iter()
            .map(|(name, actor)| (name.clone(), actor.telemetry.snapshot()))
            .collect()
    }

    pub fn running(&self) -> Vec<String> {
        self.actors
            .iter()
            .filter(|(_, actor)| actor.telemetry.is_running())
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Wait for the next actor termination while the rest of the system keeps
    /// running. Panics and cancellations are explicit reports.
    pub async fn next_event(&mut self) -> Option<ActorReport> {
        loop {
            match self.tasks.join_next().await {
                Some(Ok(report)) => {
                    self.actors.remove(&report.name);
                    return Some(report);
                }
                Some(Err(_watcher_cancelled)) => continue,
                None => return None,
            }
        }
    }

    /// Wait for all registered actors to drain until `deadline`. Remaining
    /// actors are aborted and reported with their exact last live snapshot.
    pub async fn shutdown(mut self, deadline: Duration) -> Vec<ActorReport> {
        let expires = tokio::time::Instant::now() + deadline;
        let mut reports = Vec::new();
        loop {
            if self.actors.is_empty() {
                break;
            }
            match tokio::time::timeout_at(expires, self.next_event()).await {
                Ok(Some(report)) => reports.push(report),
                Ok(None) => break,
                Err(_) => {
                    let pending: BTreeSet<String> = self.actors.keys().cloned().collect();
                    for name in pending {
                        if let Some(actor) = self.actors.remove(&name) {
                            let snapshot = actor.telemetry.snapshot();
                            actor.telemetry.mark_cancelled();
                            actor.abort.abort();
                            reports.push(ActorReport {
                                name,
                                termination: ActorTermination::ShutdownDeadline,
                                accounting: Accounting::Incomplete {
                                    last_snapshot: snapshot,
                                    undrained: snapshot.undrained(),
                                },
                            });
                        }
                    }
                    break;
                }
            }
        }
        self.tasks.abort_all();
        while self.tasks.join_next().await.is_some() {}
        reports
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        for actor in self.actors.values() {
            actor.telemetry.mark_cancelled();
            actor.abort.abort();
        }
        self.tasks.abort_all();
    }
}

/// Version of the byte-level shard-key encoding and hash algorithm.
///
/// Changing this value is a state-placement migration: deployments must not
/// mix routing hash versions for actors that rely on key affinity.
pub const ROUTING_HASH_VERSION: u32 = 1;

const ROUTING_FNV_OFFSET: u64 = 14_695_981_039_346_656_037;
const ROUTING_FNV_PRIME: u64 = 1_099_511_628_211;

fn routing_hash(tag: u8, bytes: &[u8]) -> u64 {
    let mut hash = ROUTING_FNV_OFFSET;
    for byte in std::iter::once(&tag).chain(bytes) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(ROUTING_FNV_PRIME);
    }
    hash
}

/// Stable encoding for generated shard-routing key types.
///
/// Application-defined implementations must preserve their byte encoding
/// across processes, platforms, and releases. Sigil-generated programs use
/// only the built-in implementations below.
pub trait StableRouteKey {
    fn stable_route_hash(&self) -> u64;
}

impl StableRouteKey for i64 {
    fn stable_route_hash(&self) -> u64 {
        routing_hash(1, &self.to_le_bytes())
    }
}

impl StableRouteKey for bool {
    fn stable_route_hash(&self) -> u64 {
        routing_hash(2, &[u8::from(*self)])
    }
}

impl StableRouteKey for str {
    fn stable_route_hash(&self) -> u64 {
        routing_hash(3, self.as_bytes())
    }
}

impl StableRouteKey for String {
    fn stable_route_hash(&self) -> u64 {
        self.as_str().stable_route_hash()
    }
}

impl StableRouteKey for [u8] {
    fn stable_route_hash(&self) -> u64 {
        routing_hash(4, self)
    }
}

impl StableRouteKey for Vec<u8> {
    fn stable_route_hash(&self) -> u64 {
        self.as_slice().stable_route_hash()
    }
}

impl StableRouteKey for Duration {
    fn stable_route_hash(&self) -> u64 {
        let mut encoded = [0_u8; 12];
        encoded[..8].copy_from_slice(&self.as_secs().to_le_bytes());
        encoded[8..].copy_from_slice(&self.subsec_nanos().to_le_bytes());
        routing_hash(5, &encoded)
    }
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

    /// Versioned key affinity: identical keys and shard counts select the
    /// same shard on every supported platform.
    pub fn by_key<K: StableRouteKey + ?Sized>(&self, key: &K) -> &H {
        let index = key.stable_route_hash() % self.shards.len() as u64;
        &self.shards[index as usize]
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

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ExternalWorkSnapshot {
    pub started: u64,
    pub active: u64,
    pub completed: u64,
    pub panicked: u64,
}

static EXTERNAL_STARTED: AtomicU64 = AtomicU64::new(0);
static EXTERNAL_ACTIVE: AtomicU64 = AtomicU64::new(0);
static EXTERNAL_COMPLETED: AtomicU64 = AtomicU64::new(0);
static EXTERNAL_PANICKED: AtomicU64 = AtomicU64::new(0);

struct ExternalWorkGuard;

impl Drop for ExternalWorkGuard {
    fn drop(&mut self) {
        // Publish completion before removing the final active worker. The
        // AcqRel chain across concurrent decrements ensures that an Acquire
        // snapshot observing `active == 0` also observes every completion.
        saturating_atomic_add(&EXTERNAL_COMPLETED, 1);
        let _ = EXTERNAL_ACTIVE.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            Some(current.saturating_sub(1))
        });
    }
}

/// Spawn blocking foreign work while retaining accounting even if its caller
/// times out and drops the awaiting future. Rust cannot cancel a blocking
/// function already running; `external_work_snapshot` makes that residual
/// work visible until its completion.
pub async fn tracked_spawn_blocking<T, F>(name: &'static str, work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    saturating_atomic_add(&EXTERNAL_STARTED, 1);
    let _ = EXTERNAL_ACTIVE.fetch_update(Ordering::Release, Ordering::Relaxed, |current| {
        Some(current.saturating_add(1))
    });
    let join = tokio::task::spawn_blocking(move || {
        let _guard = ExternalWorkGuard;
        work()
    });
    match join.await {
        Ok(value) => Ok(value),
        Err(error) if error.is_panic() => {
            saturating_atomic_add(&EXTERNAL_PANICKED, 1);
            Err(SigilError::Transform(format!(
                "{name}: blocking foreign function panicked"
            )))
        }
        Err(_) => Err(SigilError::ActorCancelled),
    }
}

pub fn external_work_snapshot() -> ExternalWorkSnapshot {
    let active = EXTERNAL_ACTIVE.load(Ordering::Acquire);
    let started = EXTERNAL_STARTED.load(Ordering::Acquire);
    ExternalWorkSnapshot {
        started,
        active,
        completed: EXTERNAL_COMPLETED.load(Ordering::Acquire),
        panicked: EXTERNAL_PANICKED.load(Ordering::Acquire),
    }
}

/// Fault-injection hook emitted around proof-relevant statement boundaries.
/// Set `SIGIL_CHAOS_PANIC_AT` to a comma-separated list of exact point names.
/// It is disabled when the variable is absent.
pub fn panic_point(point: &'static str) {
    let enabled = std::env::var("SIGIL_CHAOS_PANIC_AT")
        .ok()
        .is_some_and(|configured| {
            configured
                .split(',')
                .map(str::trim)
                .any(|candidate| candidate == point)
        });
    assert!(!enabled, "Sigil injected panic at {point}");
}

/// Back-pressure helpers over a raw channel sender.
///
/// These wrap the three declared policies so generated code never open-codes
/// queue handling:
///   `block`    — await capacity (unbounded wait, no loss)
///   `shed`     — never wait; drop when full (bounded at O(1), lossy)
///   `deadline` — wait up to N ms, then drop (bounded, lossy only past N)
pub mod backpressure {
    use super::{ActorSender, SendOutcome, SigilError};
    use std::time::Duration;

    pub async fn block<T>(tx: &ActorSender<T>, msg: T) -> crate::Result<SendOutcome> {
        tx.send(msg)
            .await
            .map(|_| SendOutcome::Delivered)
            .map_err(|_| SigilError::ActorStopped)
    }

    pub fn shed<T>(tx: &ActorSender<T>, msg: T) -> crate::Result<SendOutcome> {
        use tokio::sync::mpsc::error::TrySendError;
        match tx.try_send(msg) {
            Ok(()) => Ok(SendOutcome::Delivered),
            Err(TrySendError::Full(_)) => Ok(SendOutcome::Shed),
            Err(TrySendError::Closed(_)) => Err(SigilError::ActorStopped),
        }
    }

    pub async fn deadline<T>(tx: &ActorSender<T>, msg: T, ms: u64) -> crate::Result<SendOutcome> {
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
    use super::saturating_atomic_add;
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
        saturating_atomic_add(&CALLS, 1);
        let lat = max_latency_ms();
        if lat > 0 && next() % 100 < slow_pct() {
            let range = lat.saturating_add(1);
            tokio::time::sleep(Duration::from_millis(next() % range)).await;
        }
        let pct = fail_pct();
        if pct > 0 && next() % 100 < pct {
            saturating_atomic_add(&FAULTS, 1);
            return Err(crate::SigilError::Transform(format!(
                "{name}: injected fault"
            )));
        }
        Ok(())
    }

    /// Called by generated code whenever a send is shed by policy.
    pub fn note_shed(_target: &'static str) {
        saturating_atomic_add(&SHED, 1);
    }

    pub fn shed_total() -> u64 {
        SHED.load(Relaxed)
    }

    /// Called by generated code whenever a stage re-attempt begins.
    pub fn note_retry(_stage: &'static str) {
        saturating_atomic_add(&RETRIES, 1);
    }

    /// Called by generated code whenever a @recover fallback path is taken.
    pub fn note_recovery(_stage: &'static str) {
        saturating_atomic_add(&RECOVERIES, 1);
    }

    pub fn recoveries() -> u64 {
        RECOVERIES.load(Relaxed)
    }

    pub fn report() -> String {
        let external = super::external_work_snapshot();
        format!(
            "chaos: external calls={} injected faults={} retries={} recover paths taken={} \
             shed={} blocking_started={} blocking_active={} blocking_completed={} \
             blocking_panicked={} (fail_pct={} max_latency={}ms)",
            CALLS.load(Relaxed),
            FAULTS.load(Relaxed),
            RETRIES.load(Relaxed),
            RECOVERIES.load(Relaxed),
            SHED.load(Relaxed),
            external.started,
            external.active,
            external.completed,
            external.panicked,
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
        assert_eq!("account-7".stable_route_hash(), 3_239_672_280_558_547_525);
        assert_eq!(42_i64.stable_route_hash(), 13_357_854_087_122_531_014);
        assert_eq!(false.stable_route_hash(), 592_597_218_053_142_079);
        assert_eq!(true.stable_route_hash(), 592_596_118_541_513_868);
        assert_eq!(
            [1_u8, 2, 255].as_slice().stable_route_hash(),
            14_162_279_717_579_352_217
        );
        assert_eq!(
            Duration::new(2, 3).stable_route_hash(),
            17_828_535_364_259_542_577
        );
        assert_eq!(
            "account-7".to_owned().stable_route_hash(),
            "account-7".stable_route_hash()
        );
        assert_eq!(
            vec![1_u8, 2, 255].stable_route_hash(),
            [1_u8, 2, 255].as_slice().stable_route_hash()
        );
        assert_eq!(*router.by_key("account-7"), 20);
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
        let tx = ActorSender::new(tx, ActorTelemetry::new());
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

    #[test]
    fn actor_stats_and_live_counters_saturate() {
        let mut stats = ActorStats {
            handled: u64::MAX,
            dropped: u64::MAX,
            shed: u64::MAX,
        };
        stats.record_handled();
        stats.record_dropped();
        stats.record_shed(10);
        assert_eq!(
            stats,
            ActorStats {
                handled: u64::MAX,
                dropped: u64::MAX,
                shed: u64::MAX,
            }
        );

        let telemetry = ActorTelemetry::new();
        telemetry.inner.accepted.store(u64::MAX, Ordering::Relaxed);
        telemetry.inner.handled.store(u64::MAX, Ordering::Relaxed);
        telemetry.inner.dropped.store(u64::MAX, Ordering::Relaxed);
        telemetry.inner.shed.store(u64::MAX, Ordering::Relaxed);
        telemetry.note_accepted();
        telemetry.note_handled();
        telemetry.note_dropped();
        telemetry.note_shed(10);
        assert_eq!(
            telemetry.snapshot(),
            ActorSnapshot {
                accepted: u64::MAX,
                handled: u64::MAX,
                dropped: u64::MAX,
                shed: u64::MAX,
            }
        );
    }

    #[tokio::test]
    async fn actor_sender_tracks_only_accepted_messages() {
        let telemetry = ActorTelemetry::new();
        let (raw, mut receiver) = tokio::sync::mpsc::channel(1);
        let sender = ActorSender::new(raw, telemetry.clone());
        assert_eq!(backpressure::shed(&sender, 1), Ok(SendOutcome::Delivered));
        assert_eq!(backpressure::shed(&sender, 2), Ok(SendOutcome::Shed));
        assert_eq!(telemetry.snapshot().accepted, 1);
        assert_eq!(receiver.recv().await, Some(1));
        drop(receiver);
        assert_eq!(
            backpressure::shed(&sender, 3),
            Err(SigilError::ActorStopped)
        );
        assert_eq!(telemetry.snapshot().accepted, 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn actor_sender_publishes_acceptance_before_message_visibility() {
        const MESSAGE_COUNT: u64 = 1_000;

        let telemetry = ActorTelemetry::new();
        let observed = telemetry.clone();
        let (raw, mut receiver) = tokio::sync::mpsc::channel(1);
        let sender = ActorSender::new(raw, telemetry);
        let consumer = tokio::spawn(async move {
            for received in 1..=MESSAGE_COUNT {
                assert_eq!(receiver.recv().await, Some(received));
                assert!(
                    observed.snapshot().accepted >= received,
                    "message {received} became visible before its acceptance publication"
                );
            }
        });

        for message in 1..=MESSAGE_COUNT {
            sender.send(message).await.expect("receiver remains live");
        }
        consumer.await.expect("consumer task");
    }

    #[tokio::test(start_paused = true)]
    async fn full_queue_policies_and_blocked_sender_cancellation_are_deterministic() {
        let telemetry = ActorTelemetry::new();
        let (raw, mut receiver) = tokio::sync::mpsc::channel(1);
        let sender = ActorSender::new(raw, telemetry.clone());
        assert_eq!(
            backpressure::block(&sender, 1).await,
            Ok(SendOutcome::Delivered)
        );
        assert_eq!(backpressure::shed(&sender, 2), Ok(SendOutcome::Shed));

        let deadline_sender = sender.clone();
        let deadline =
            tokio::spawn(async move { backpressure::deadline(&deadline_sender, 3, 25).await });
        tokio::time::advance(Duration::from_millis(25)).await;
        assert_eq!(
            deadline.await.expect("deadline task"),
            Ok(SendOutcome::Shed)
        );

        let blocked_sender = sender.clone();
        let blocked = tokio::spawn(async move { backpressure::block(&blocked_sender, 4).await });
        tokio::task::yield_now().await;
        assert!(!blocked.is_finished());
        blocked.abort();
        assert!(blocked.await.expect_err("cancelled sender").is_cancelled());
        assert_eq!(receiver.recv().await, Some(1));
        assert_eq!(telemetry.snapshot().accepted, 1);

        assert_eq!(
            backpressure::block(&sender, 5).await,
            Ok(SendOutcome::Delivered)
        );
        assert_eq!(receiver.recv().await, Some(5));
        drop(sender);
        assert_eq!(receiver.recv().await, None);
    }

    #[tokio::test]
    async fn supervisor_reports_panics_while_running_with_incomplete_accounting() {
        let telemetry = ActorTelemetry::new();
        telemetry.note_accepted();
        let join = tokio::spawn(async move {
            panic!("injected actor failure");
            #[allow(unreachable_code)]
            ((), ActorStats::default())
        });
        let task = ActorTask::new("panics", join, telemetry);
        let mut supervisor = Supervisor::new();
        supervisor.register(task).expect("unique actor");

        let report = supervisor.next_event().await.expect("panic event");
        assert_eq!(report.name, "panics");
        assert_eq!(report.termination, ActorTermination::Panicked);
        assert_eq!(
            report.accounting,
            Accounting::Incomplete {
                last_snapshot: ActorSnapshot {
                    accepted: 1,
                    ..ActorSnapshot::default()
                },
                undrained: 1,
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_deadline_aborts_and_reports_undrained_messages() {
        let telemetry = ActorTelemetry::new();
        telemetry.note_accepted();
        telemetry.note_accepted();
        let join = tokio::spawn(async move {
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            ((), ActorStats::default())
        });
        let task = ActorTask::new("stuck", join, telemetry);
        let mut supervisor = Supervisor::new();
        supervisor.register(task).expect("unique actor");

        let shutdown = tokio::spawn(supervisor.shutdown(Duration::from_millis(25)));
        tokio::time::advance(Duration::from_millis(25)).await;
        let reports = shutdown.await.expect("shutdown task");
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].termination, ActorTermination::ShutdownDeadline);
        assert!(matches!(
            reports[0].accounting,
            Accounting::Incomplete { undrained: 2, .. }
        ));
    }

    #[tokio::test]
    async fn dropping_supervisor_aborts_registered_actor_instead_of_detaching_it() {
        struct DropProbe(Arc<std::sync::atomic::AtomicBool>);

        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let task_dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let probe = DropProbe(task_dropped.clone());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let telemetry = ActorTelemetry::new();
        let observed_telemetry = telemetry.clone();
        let join = tokio::spawn(async move {
            let _probe = probe;
            let _ = started_tx.send(());
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            ((), ActorStats::default())
        });
        let task = ActorTask::new("must-not-detach", join, telemetry);
        let mut supervisor = Supervisor::new();
        supervisor.register(task).expect("unique actor");
        started_rx.await.expect("actor started");

        drop(supervisor);
        while !task_dropped.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }

        assert!(!observed_telemetry.is_running());
    }

    #[tokio::test]
    async fn repeated_registration_and_clean_stop_are_deterministic() {
        for iteration in 0..8 {
            let telemetry = ActorTelemetry::new();
            let completed = telemetry.clone();
            let join = tokio::spawn(async move {
                completed.note_handled();
                (
                    iteration,
                    ActorStats {
                        handled: 1,
                        ..ActorStats::default()
                    },
                )
            });
            let task = ActorTask::new(format!("worker-{iteration}"), join, telemetry);
            let mut supervisor = Supervisor::new();
            supervisor.register(task).expect("unique actor");
            let report = supervisor.next_event().await.expect("clean stop event");
            assert_eq!(report.termination, ActorTermination::Stopped);
            assert_eq!(
                report.accounting,
                Accounting::Complete(ActorStats {
                    handled: 1,
                    ..ActorStats::default()
                })
            );
        }
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore = "Miri does not execute the OS blocking thread pool")]
    async fn timed_out_blocking_work_remains_accounted_until_completion() {
        let before = external_work_snapshot();
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let awaiting = tokio::spawn(async move {
            tracked_spawn_blocking("tracked-test", move || {
                started_tx.send(()).expect("signal start");
                release_rx.recv().expect("release work");
                7
            })
            .await
        });
        while started_rx.try_recv().is_err() {
            tokio::task::yield_now().await;
        }
        awaiting.abort();
        assert_eq!(external_work_snapshot().active, before.active + 1);
        release_tx.send(()).expect("release blocking work");
        loop {
            let snapshot = external_work_snapshot();
            if snapshot.active == before.active {
                assert_eq!(snapshot.started, before.started + 1);
                assert_eq!(snapshot.completed, before.completed + 1);
                break;
            }
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn timed_async_work_is_dropped_at_the_cancellation_boundary() {
        use std::sync::atomic::AtomicBool;

        struct CancelProbe(Arc<AtomicBool>);
        impl Drop for CancelProbe {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let cancelled = Arc::new(AtomicBool::new(false));
        let observed = cancelled.clone();
        let timed = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_millis(25), async move {
                let _probe = CancelProbe(observed);
                std::future::pending::<()>().await;
            })
            .await
        });
        tokio::time::advance(Duration::from_millis(25)).await;
        assert!(timed.await.expect("timeout task").is_err());
        assert!(cancelled.load(Ordering::Acquire));
    }
}
