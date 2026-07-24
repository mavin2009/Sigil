# Runtime & Generated Code

What `sigilc` emits, how it executes, and how to exercise it.

- [Compiler pipeline](#compiler-pipeline)
- [What gets generated](#what-gets-generated)
- [The actor model](#the-actor-model)
- [Topology wiring and shutdown](#topology-wiring-and-shutdown)
- [sigil_rt](#sigil_rt)
- [Fault injection](#fault-injection)
- [Tuning the demo](#tuning-the-demo)

---

## Compiler pipeline

```
parse
  → lower                      (one Graph IR per process)
  → level1_check               (extinct-by-design, per process)
  → check_types                (all names, expressions, literals, routes)
  → check_effect_contracts     (idempotency/cancellation/side effects)
  → check_handler_wellformedness
  → check_transform_signatures
  → check_failure_paths
  → check_transform_purity
  → derive_topology            (targets, types, acyclicity, routing keys)
  → level2_check               (budgets, spec bookkeeping)
  → level3_prove               (inductive holds + input guards)
  → level4_prove               (system invariants)
  → residual_risk_report
  → emit                       (Rust crate)
```

The Graph IR is **per process**. An aggregated graph made Level-1 unsound
for multi-process programs: a state write in A to B's slot passed the
locality check, and a timeout in A could be "paired" by a recover in B.

## What gets generated

```
out_dir/
  Cargo.toml          standalone [workspace], depends on sigil_rt
  src/lib.rs          schemas, transforms, actors, component assembly, smoke test
  src/main.rs         concurrent demo driver (with --emit-main)
  RESIDUAL_RISK.md    what was proven, assumed, and skipped
  RESIDUAL_RISK.json  versioned, owned residual items
  SIGIL_EFFECTS.json  bound-transform effect contracts
  SIGIL_BUILD.json    compiler/runtime/lock/source provenance
```

Sigil-emitted code contains **no `Mutex`, no `RwLock`, no `Arc`, no atomics,
and no `unsafe`** — asserted by an integration test, not by convention. This
is a source-level shared-nothing guarantee, not a claim that Tokio's bounded
channel, the allocator, or the operating system is lock-free internally.

## The actor model

Every process compiles to a shared-nothing actor:

```rust
pub struct LedgerHandle {
    tx: sigil_rt::ActorSender<Payment>,
}

impl Ledger {
    pub fn spawn(mut self, capacity: usize)
        -> Result<(LedgerHandle, sigil_rt::ActorTask<Self>)>
    {
        sigil_rt::validate_channel_capacity(capacity)?;
        let telemetry = self.__telemetry.clone();
        let task_telemetry = telemetry.clone();
        let (raw_tx, mut rx) = tokio::sync::mpsc::channel::<Payment>(capacity);
        let tx = sigil_rt::ActorSender::new(raw_tx, telemetry.clone());
        let join = tokio::spawn(async move {
            let mut stats = sigil_rt::ActorStats::default();
            while let Some(msg) = rx.recv().await {
                match self.on_payment(msg).await {
                    Ok(()) => {
                        stats.record_handled();
                        self.__telemetry.note_handled();
                    }
                    Err(_) => {
                        stats.record_dropped();
                        self.__telemetry.note_dropped();
                    }
                }
            }
            stats.shed = self.__shed;
            self.__telemetry.mark_stopped();
            (self, stats)
        });
        Ok((LedgerHandle { tx }, sigil_rt::ActorTask::new("Ledger", join, task_telemetry)))
    }
}
```

`spawn` takes `self` **by move**. After the call the state is unreachable
except by message; it comes back at `join()`. Data races on Sigil-owned
process state are excluded by construction. External Rust and dependencies
remain outside that source-level guarantee.

On normal exit, `ActorStats` gives complete accounting. Live
`ActorSnapshot` includes accepted messages. All counters saturate rather than
wrap. A panic is fail-stop: `Supervisor` reports it while the component runs
as incomplete accounting with the last snapshot and undrained count. State is
unavailable, so callers must treat the run as failed rather than claiming
conservation.

**Multi-handler processes** get a typed dispatch enum:

```rust
pub enum RiskEngineMsg {
    NewOrder(NewOrder),
    Cancel(Cancel),
}
```

and the handle exposes `send_new_order` / `send_cancel`. Which variant a
`send` constructs is decided by the compiler from the verified topology, so
codegen and the checker can never disagree.

## Topology wiring and shutdown

Processes gain an outbox per target, wired before spawn:

```rust
pub struct Gateway {
    pub admitted: i64,
    pub __shed: u64,
    pub risk_out: Option<sigil_rt::Router<RiskHandle>>,
}
```

Generated crates also expose `ProcessConfig`, `ComponentConfig`, and
`Component`. `Component::start` validates the complete configuration before
startup, reserves handle storage, spawns **sinks first**, connects every
verified route, and registers each shard with its supervisor:

```rust
let mut config = ComponentConfig::default();
config.gateway.shards = 8;
config.gateway.inbox_capacity = 1_024;
config.risk.shards = 16;

let component = Component::start(config)?;
component.ingress_gateway().round_robin().send(order).await?;
let reports = component.shutdown(Duration::from_secs(10)).await;
```

Only zero-indegree processes expose `ingress_<process>()`; internal-stage
handles cannot accidentally become an undeclared external entry point.
Each ingress is a cloneable `IngressRouter` whose clones share an atomic
round-robin cursor. `by_key` uses the same routing-hash version as actor
outboxes. Handles also expose immediate-shed and duration-deadline admission
alongside their existing blocking `send`.
`Component::START_ORDER`, `SHUTDOWN_ORDER`, and `EDGES` make the generated
assembly auditable against the exported graph.

Explicit `placement` declarations additionally emit `COMPONENT_PLACEMENT`.
Only edges between different groups appear as `RemoteBoundaryDescriptor`
values. Generated `transport_manifest(max_payload_bytes)` advertises the
component's protocol, routing-hash, boundary schemas, payload limit, and
at-most-once/at-least-once protocol capabilities. Generated
`PlacementComponent::start(...).await` starts only one placement, connects
local edges directly, and requires typed durable endpoints for remote output.

Shutdown closes component-owned handles stage by stage in topological order.
Each actor drains and releases its outboxes, cascading closure downstream.
The supervisor applies a hard deadline and reports every undrained accepted
message if an application-retained ingress clone or stuck handler prevents
draining.

Shutdown ordering is where hand-written actor systems deadlock. Here it is
derived from the graph the compiler already proved acyclic.

`Router<H>` lives inside a single actor's task, so Sigil's round-robin and
hashing logic needs no atomics or locks. Key routing uses the versioned
`StableRouteKey` encoding, so a key and shard count select the same shard
across supported platforms. `Router::new` rejects an empty shard set as a
typed configuration error.

## sigil_rt

The runtime is deliberately small:

| Item | Purpose |
| ---- | ------- |
| generated `Component` | validated assembly, concurrent routed ingress, readiness, topology-owned shutdown |
| `SigilError` | typed timeout, transform, schema, configuration, distributed, stopped, panicked, and cancelled failures |
| `ActorStats` / `ActorSnapshot` | complete terminal counters / live accepted, handled, dropped, shed |
| `ActorTask` / `Supervisor` | no silent detach; shard-qualified registration, live termination events, bounded shutdown |
| `Router<H>` | shard ring: `round_robin`, `by_key`, `shards` |
| `IngressRouter<H>` | cloneable producer router with one shared atomic cursor and stable-key affinity |
| `distributed::Transport` | bounded remote admission under a negotiated protocol/schema/delivery session |
| `distributed::WireEnvelope` | message identity, fenced shard address, schema version, delivery semantics, and opaque bytes |
| `distributed::WireCodec` | deterministic generated schema encoding with explicit total/field ceilings |
| `distributed::{WireEncoder, WireDecoder}` | checked little-endian primitives, length validation before allocation, and canonical-value rejection |
| `distributed::AuthorizedMessage` | typed decoded payload plus the live, non-cloneable shard ownership permit |
| `distributed::RemoteEndpoint<T>` | typed round-robin, stable-key, and broadcast admission to fenced remote shards |
| `distributed::DurableOutbox` | persist-before-send, durable attempts, bounded retry, and checked idempotent acknowledgement |
| `distributed::DurableRemoteEndpoint<T>` | generated typed routing through a durable at-least-once outbox |
| `distributed::StateCommitter<S>` | per-shard restore, dedup lookup, and atomic application-state/message-ID commit |
| generated `PlacementComponent` | placement-local startup, typed receiver permit handoff, health, and bounded shutdown |
| `distributed::MessageIdSource` | adapter contract for unique producer sequences; at-least-once requires declared durable state |
| `distributed::ShardLease` | one-atomic epoch/phase/permit fence for drain, checkpoint, and handoff |
| `distributed::DedupWindow` | bounded exact duplicate suppression with a checkpointable frontier |
| `SendOutcome` | `Delivered` \| `Shed` |
| `validate_channel_capacity` | rejects capacities that would make Tokio panic |
| `join_actor` / `join_task` | converts Tokio panic/cancellation joins to typed Sigil errors |
| `tracked_spawn_blocking` | retains accounting for blocking work after timeout cancellation |
| `backpressure::{block, shed, deadline}` | the three declared policies |
| `chaos` | fault injection and counters |

## Distributed runtime contract

`sigil_rt::distributed` is a transport integration boundary, not a bundled
network stack. Peers exchange `TransportManifest` values and
`NegotiatedTransport::negotiate` selects the highest common protocol and
schema versions with identical structural fingerprints, intersects delivery
semantics, requires an identical routing hash, and chooses the smaller payload
ceiling. Every received envelope must be validated against that immutable
session.

Generated schemas implement `WireCodec` at schema version 1 and expose typed
envelope helpers. Their `SchemaFingerprint` is a compiler-derived SHA-256 over
the ordered recursive layout; a same-version layout mismatch cannot negotiate.
`CodecLimits` requires nonzero total-payload and
individual-field ceilings. Encoding uses checked capacity arithmetic and
fallible reservation. Decoding checks the complete payload ceiling before
reading, validates every length prefix before allocating, and rejects
truncation, trailing bytes, invalid UTF-8 and booleans, non-finite floats, and
invalid duration nanoseconds. `authorize_and_decode` validates the negotiated
schema and current shard epoch, then returns an `AuthorizedMessage`; its
ownership permit stays live until the caller finishes applying the message.

Remote admission deliberately supports only immediate shed and a finite
deadline. `TransportOutcome::Accepted` means the bounded transport accepted
the envelope; it does not mean that the remote actor handled or durably
committed it. Exactly-once is not offered. At-least-once adapters must persist
producer sequence state and use `DedupWindow` or an equivalent durable,
bounded duplicate ledger.

`RemoteEndpoint<T>` validates that its destinations all belong to one
deployment, placement group, and process and contain no duplicate shard. It
uses the same `StableRouteKey` hash as local routing and obtains a new
`MessageId` for each envelope, including each broadcast destination.
`VolatileMessageIds` is suitable only for at-most-once sessions with a fresh
producer identity after restart. At-least-once construction fails unless the
supplied source declares durable sequence state.

`DurableRemoteEndpoint<T>` additionally requires a negotiated at-least-once
session and the same session as its `DurableOutbox`. The outbox persists exact
bytes and attempt counts before I/O and removes a record only for a matching
`DeliveryAck` backed by `DurableCommit` evidence. Generated receiver delivery
uses an owned permit, bounded actor-inbox admission, and
`StateCommitter<Process>`; startup restores state and deduplication together,
then each successful handler result is committed with its message identity
before acknowledgement.

`ShardLease` fences the data plane by `ShardAddress { key, owner, epoch }`.
Its phase and live permit count share one atomic word, so a weak-memory race
cannot complete handoff while a serving permit is live. The migration
sequence is:

1. acquire and retain an ownership permit through every state mutation;
2. `begin_drain`, which closes new admissions at the old epoch;
3. wait for the permit count to reach zero and create a `ShardCheckpoint`
   containing state plus the deduplication frontier;
4. `complete_handoff`, permanently retiring the source lease;
5. `receive_handoff` on the named successor, restore and verify the checkpoint
   while its lease is `Pending`, then `activate`.

The runtime fence is local. A strongly consistent external coordinator must
allocate monotonically increasing epochs, durably store checkpoints, and
ensure a handoff bundle is delivered to only one successor. The generated
in-process `Component` remains useful as the reference execution and does not
itself schedule processes across hosts.

## Fault injection

External stubs route through `sigil_rt::chaos`, so the verified
`@timeout` / `@retry` / `@recover` machinery can be exercised under load.
Disabled by default (zero latency, zero faults).

| Variable | Meaning |
| -------- | ------- |
| `SIGIL_CHAOS_FAIL_PCT` | percent of external calls that fail (0–100) |
| `SIGIL_CHAOS_LATENCY_MS` | max injected latency per external call |
| `SIGIL_CHAOS_SLOW_PCT` | percent of calls that get injected latency (default 25) |
| `SIGIL_CHAOS_PANIC_AT` | comma-separated proof cut-points for deterministic panic injection |

```
SIGIL_CHAOS_FAIL_PCT=20 SIGIL_CHAOS_LATENCY_MS=120 cargo run --bin demo
```

```
chaos: external calls=10240 injected faults=1757 retries=2560
       recover paths taken=632 shed=0 (fail_pct=20 max_latency=120ms)
```

The generator uses a deterministic splitmix64 sequence, so runs are
reproducible.

## Tuning the demo

| Variable | Default | Purpose |
| -------- | ------- | ------- |
| `SIGIL_DEMO_SHARDS` | 8 | actors per stage |
| `SIGIL_DEMO_PRODUCERS` | 64 | concurrent producer tasks |
| `SIGIL_DEMO_MSGS` | 250 | messages per producer |
| `SIGIL_DEMO_CAPACITY` | 1024 | channel capacity per actor |

Lower `SIGIL_DEMO_CAPACITY` to force real queue pressure and watch
back-pressure policies engage:

```
SIGIL_DEMO_CAPACITY=4 SIGIL_CHAOS_LATENCY_MS=80 cargo run --bin demo
```

```
[Src]  sent = 960   shed downstream = 453
[Sink] got  = 507
```

`507 + 453 = 960` — exact, with `Sink.got <= Src.sent` holding throughout,
which is what the Level-4 proof predicted: shedding only ever *decreases*
the downstream count.
