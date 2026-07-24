# Running Sigil Components in Production

A generated crate is meant to be boring: an ordinary Rust library that your
existing monorepo, CI, and deployment pipeline treat like any other. This
document covers the parts that are not obvious.

- [What you get](#what-you-get)
- [Wiring external transforms](#wiring-external-transforms)
- [Capacity and back-pressure tuning](#capacity-and-back-pressure-tuning)
- [Lifecycle: startup and graceful shutdown](#lifecycle-startup-and-graceful-shutdown)
- [Concurrent ingress and readiness](#concurrent-ingress-and-readiness)
- [Distributed placement and shard handoff](#distributed-placement-and-shard-handoff)
- [Panics and task supervision](#panics-and-task-supervision)
- [Observability](#observability)
- [Performance characterization](#performance-characterization)
- [CI integration](#ci-integration)
- [What is still on you](#what-is-still-on-you)

---

## What you get

```
out_dir/
  Cargo.toml          standalone crate; no build script, no proc macro
  src/lib.rs          schemas, transforms, processes, actors
  src/main.rs         demo driver (only with --emit-main)
  topology.mmd/.dot   the verified graph (only with --emit-graph)
  RESIDUAL_RISK.md    what was proven, assumed, and skipped
  RESIDUAL_RISK.json  versioned, owned residual items
  SIGIL_EFFECTS.json  bound-transform effect contracts
  SIGIL_BUILD.json    compiler/runtime/lock/source provenance
```

Dependencies are `tokio`, `sigil_rt`, `thiserror`, and optionally `tracing`.
No build system, no code generation at build time, no magic. Vendor the
output or generate it in CI — both work, and the choice is discussed under
[CI integration](#ci-integration).

## Wiring external transforms

Empty-bodied transforms compile to stubs. Prefer a bound transform for real
deployments so regeneration never overwrites integration code:

```
transform fetch_secret(r: Request) -> Request {}

transform fetch_secret(r: Request) -> Request =
  kms::fetch
    @effect(idempotent, cancel_safe, read)
```

```rust
async fn fetch_secret(r: Request) -> Result<Request> { ... }
```

The effect declaration is checked at every timeout/retry use and emitted for
review, but its truth remains application-owned. Four properties matter:

1. **It must be cancel-safe.** Timed stages run inside `tokio::time::timeout`,
   so the future can be dropped at any await point. If your implementation
   holds a half-completed write when cancelled, the compiler's failure model
   does not cover that. Prefer idempotent operations, transactional APIs, or
   an operation-specific idempotency key. Detaching work in a spawned task
   merely to evade cancellation also detaches its outcome from Sigil's
   failure accounting and is not a general fix.
2. **It must terminate.** Untimed stages have no bound. A transform that
   blocks forever stalls one actor permanently; the Level-2 budget only
   covers stages you actually annotated with `@timeout`.
3. **It must not block the runtime.** Use async I/O or a `blocking` binding.
   Codegen routes blocking work through completion tracking; a synchronous
   call on a runtime worker violates the declared contract.
4. **Its errors must be `SigilError`.** Map your failures at the boundary;
   the actor counts them as drops, which is what the counting arguments
   depend on.

Pure transforms (non-empty bodies) are compiled and must stay pure — the
compiler rejects an external call inside one, because recovery paths depend
on their infallibility.

## Capacity and back-pressure tuning

`ComponentConfig` sets the shard count and per-actor inbox size independently
for every process. `Component::start` validates all of them before spawning
the first actor. The lower-level `spawn(self, capacity)` API performs the same
capacity validation when manual assembly is required. The number of queued
message slots is bounded:

```
queued slots  =  sum over actors(capacity)
```

This is not a byte-exact memory ceiling: heap-owned fields such as `String`,
in-flight handler values, producer tasks, runtime bookkeeping, foreign
libraries, and allocator overhead are additional. Sigil emits no unbounded
channel.

Choosing the policy per edge matters more than choosing the number:

| Situation | Policy |
| --------- | ------ |
| Losing a message is worse than being slow | `@block` (default) |
| Being slow is worse than losing a message (audit, telemetry, metrics) | `@shed` |
| There is a real deadline and you want the bound to be provable | `@deadline(N.ms)` |

`require path_latency <= N.ms` is rejected if any send on the path uses
`@block`, because a blocking send has no time bound. That refusal is the
feature: you cannot claim a latency SLO you have not made provable.

**Sizing rule of thumb.** Capacity should absorb a burst, not a sustained
overload. If a stage is persistently slower than its input, no capacity
saves you — back-pressure will propagate to your producers (with `@block`)
or the system will shed (with `@shed`/`@deadline`). Both are correct; decide
which one your product wants, per edge, and write it down in the source.

## Lifecycle: startup and graceful shutdown

Use the generated component API for production assembly:

```rust
let mut config = ComponentConfig::default();
config.gateway = ProcessConfig {
    shards: 8,
    inbox_capacity: 1_024,
};
config.settlement = ProcessConfig {
    shards: 4,
    inbox_capacity: 2_048,
};

let mut component = Component::start(config)?;
component
    .ingress_gateway()
    .round_robin()
    .send(request)
    .await?;
export_snapshots(component.snapshots());
```

`Component::start` requires an active Tokio runtime and returns a typed
configuration error when there is none. It preflights every shard count,
capacity, and handle-vector reservation before startup, creates sinks first,
wires verified routes, and registers shards under unique operational names
such as `Gateway[3]`. Only zero-indegree processes expose
`ingress_<process>()` methods.

```rust
let reports = component.shutdown(Duration::from_secs(10)).await;
for report in reports { export(report); }
```

Shutdown releases component-owned handles in `Component::SHUTDOWN_ORDER`,
which is generated directly from the verified acyclic graph. Upstream actors
drain and release their outboxes before downstream actors stop. Any external
ingress clone retained by application code can delay closure, so the hard
deadline still applies and produces explicit `ShutdownDeadline` reports.

Manual `new` / `connect_*` / `spawn` remains available for specialized
integration. Register multiple manual shards with
`Supervisor::register_as("Gateway[0]", task)` so operational names are
unique. `ActorTask` is `must_use`; dropping it aborts rather than silently
detaching work. `Component::next_event` exposes panic/cancellation while the
rest of the component is still running and is intended to be selected
alongside the application's other control-plane events.

## Concurrent ingress and readiness

Each entry process exposes a cloneable `IngressRouter<Handle>`. All clones
share one atomic round-robin cursor, so producer tasks distribute admission
across the fleet instead of each restarting at shard zero:

```rust
let ingress = component.ingress_gateway().clone();
for producer in producers {
    let ingress = ingress.clone();
    tokio::spawn(async move {
        while let Some(request) = producer.next().await {
            ingress.round_robin().send(request).await?;
        }
        Ok::<_, sigil_rt::SigilError>(())
    });
}
```

For affinity, `by_key` uses the same versioned stable hash as internal
`send ... by key` routing:

```rust
ingress.by_key(&account_id).send(request).await?;
```

Changing a stateful process's shard count changes key placement. It is a
deployment migration, not an in-place configuration edit. Cross-host
migration uses the epoch-fenced protocol described under
[Distributed placement and shard handoff](#distributed-placement-and-shard-handoff).

Ingress admission is explicit:

| Method | Full inbox behavior | Result |
| ------ | ------------------- | ------ |
| `send(message)` | waits | `Result<()>` |
| `admit_shed(message)` | returns immediately | `SendOutcome::Shed` |
| `admit_deadline(message, duration)` | waits only to the deadline | `SendOutcome::Shed` on expiry |

For a multi-handler entry, construct its generated dispatch enum when using
the bounded methods, for example
`GatewayMsg::NewOrder(order)`. Admission shedding happens before actor
acceptance; the caller must export the returned outcome as boundary
telemetry.

`component.health()` returns the expected actor count, currently running
shard names, and live accounting snapshots. `is_ready()` means every
generated actor is running:

```rust
let health = component.health();
if !health.is_ready() {
    mark_not_ready(health.unavailable_actors(), health.snapshots);
}
```

This is component readiness, not dependency or business-level health.
Applications must combine it with their foreign-service and durability
checks.

## Distributed placement and shard handoff

Declare co-location explicitly:

```
placement edge { Gateway }
placement core { RiskEngine, Settlement }
```

When any placement exists, every process must belong to exactly one group.
The compiler emits `COMPONENT_PLACEMENT`; only verified edges crossing group
boundaries appear in `remote_boundaries`. It also emits
`transport_manifest(max_payload_bytes)`, including the boundary schemas and a
protocol-compatible at-most-once and at-least-once delivery contract.

`Component::start` runs the complete verified reference topology inside one
Tokio runtime. `PlacementComponent::start(...).await` instead starts only one
declared placement: same-placement edges remain typed channels and every
cross-placement output requires its generated `DurableRemoteEndpoint<T>`.
Sigil does not ship a network driver or choose service discovery; a deployment
adapter implements `Transport` at each advertised boundary.

Before traffic is admitted, peers must negotiate:

- the distributed protocol version;
- the routing-hash version;
- the highest common version of each message schema whose compiler-derived
  structural SHA-256 fingerprint also matches;
- at-most-once or at-least-once delivery; and
- a maximum payload size.

Remote admission is always bounded:

```rust
let outcome = transport
    .admit(envelope, RemoteAdmission::deadline(Duration::from_millis(20))?)
    .await?;
```

There is no remote equivalent of an indefinitely blocking send.
The compiler rejects `@block` on every cross-placement edge; use `@shed` or a
finite `@deadline`.
`TransportOutcome::Accepted` means accepted by the bounded transport, not
handled or committed by the actor. `Shed` is expected policy output.
Exactly-once is intentionally absent. At-most-once can lose; at-least-once can
duplicate and therefore requires durable producer sequences plus
receiver-side deduplication.

Generated schemas expose `encode_remote`, `decode_remote`, and
`authorize_remote`. The codec limit is always intersected with the negotiated
payload ceiling:

```rust
let limits = CodecLimits::new(64 * 1024, 16 * 1024)?;
let envelope = order.encode_remote(
    &session,
    destination,
    message_id,
    DeliverySemantics::AtMostOnce,
    limits,
)?;
let authorized =
    Order::authorize_remote(&session, &lease, &envelope, limits)?;
apply(authorized.message()).await?;
drop(authorized); // releases the ownership permit after state mutation
```

The wire layout is declaration-order, little-endian, and strict. Strings and
bytes have `u32` lengths checked before allocation; booleans accept only
`0`/`1`; strings must be UTF-8; floats must be finite; durations must have
valid nanoseconds; truncation and trailing bytes are errors. A remote message
must be a finite locally declared schema. Bound foreign types, including
foreign types nested inside a local schema, require an explicit adapter and
otherwise fail compilation.

The fingerprint covers the schema name, ordered field names and primitive
kinds, and nested schema fingerprints. Editing any of those while retaining
schema version 1 therefore makes rolling peers reject negotiation instead of
decoding the new bytes with an old layout. Future compatible evolution can
advertise multiple explicit versions; the current generated codec advertises
one exact version and fails closed on every structural change.

`RemoteEndpoint<T>` combines one typed schema, a negotiated `Transport`, a
consistent set of fenced destination shards, and a `MessageIdSource`. It
provides round-robin, stable-key, and broadcast admission. Generated
`RemoteEndpoints` gathers every outbound boundary and verifies the declared
target placement/process and one deployment identity. At-least-once endpoints
refuse `VolatileMessageIds`; a production adapter must return `Durable` only
after it has durably advanced or reserved the producer sequence.

`DurableRemoteEndpoint<T>` combines that typed endpoint with a
`DurableOutbox`. It persists the exact envelope before the first admission,
durably records every attempt before transport I/O, and retains accepted,
failed, shed, partitioned, and exhausted work until a matching durable
receiver acknowledgement arrives. Retry exhaustion is visible but never
deletes the record.

Generated placement startup requires one `StateCommitter<Process>` for each
remote receiver shard. Startup restores application state and its
deduplication frontier before spawning the actor. The generated
`deliver_<process>_<handler>` method moves an owned `AuthorizedMessage<T>`
through the bounded receiver inbox; its ownership permit remains live through
the handler and atomic state/identity commit. At-least-once acknowledgement is
therefore emitted only after durable commit. A committed replay skips the
handler and returns an idempotent duplicate receipt.

Stateful shard movement is fenced by a monotonically increasing
`OwnershipEpoch`:

```rust
let plan = source.begin_drain(node_b, next_epoch)?;
// Stop new old-epoch admissions and retain every OwnershipPermit until its
// message has finished mutating state.
let bundle = source.complete_handoff(plan, checkpoint)?;
let (successor, checkpoint) = ShardLease::receive_handoff(bundle, &local_node)?;
restore_and_verify(checkpoint)?;
successor.activate()?;
```

`complete_handoff` fails while any ownership permit is live. The source is
permanently retired before the successor can be activated, and stale epochs
are rejected. State and the deduplication frontier travel together in
`ShardCheckpoint`.

The local fence cannot elect a global owner. Production deployments still
need a strongly consistent coordinator to allocate epochs, durable checkpoint
storage, authentication/encryption, service discovery, a codec adapter for
any deliberately bound foreign type, and a rule that one non-cloneable
handoff bundle reaches only one successor. These items appear in generated
residual-risk output for programs with placement declarations.

Level-1 type/acyclic topology checks and Level-3 process-local invariants
remain available. `require path_latency` and Level-4 cross-process
conservation fail closed when a remote boundary exists: transport timing,
loss, and duplication are not silently imported into an in-process theorem.

## Panics and task supervision

Rust and Tokio do handle task panics: Tokio catches a panic at the spawned
task boundary. Sigil's supervisor converts it to an
`ActorTermination::Panicked` report without panicking again.

The policy is deliberately **fail-stop**, not restart-and-continue. A panic
may occur after part of a handler changed state or sent a message, so
continuing the same actor or automatically replaying the input could violate
an invariant or duplicate an effect. After a panic, final state and complete
`ActorStats` are unavailable. The report includes the last live snapshot and
explicit undrained count. Fail the component or isolate affected traffic,
then reconcile from a durable source before restarting. Automatic replay is
forbidden: the handler may already have sent or committed a foreign effect.

Ordinary transform errors, validation failures, timeouts, and declared
shedding are not panics; those remain typed and accounted for. Panic freedom
of foreign Rust code and arithmetic is not currently proven.

## Observability

**Tracing** is opt-in so the crate forces no dependency on you:

```
cargo build --features tracing
```

Each handler invocation gets a span carrying the process and message type,
which is what lets you correlate a trace with `topology.mmd`.

**Actor statistics** are available live and in terminal reports:

```rust
let snapshots = supervisor.snapshots();
let event = supervisor.next_event().await;
// accepted — messages admitted to the actor inbox
// handled  — messages processed
// dropped  — failed with no recovery path, or rejected by an input guard
// shed     — outbound messages dropped by back-pressure policy
```

Counters saturate at `u64::MAX` rather than wrapping. On normal completion,
accounting is `Complete`. Panic, cancellation, and deadline termination are
`Incomplete { last_snapshot, undrained }`; never turn those into zero or omit
them from conservation reports.

**Suggested alerts**

| Signal | Why it matters |
| ------ | -------------- |
| `dropped` rising | inputs violating a `require` contract, or a stage failing past its recovery path |
| `shed` sustained non-zero | a downstream stage is persistently slower than its input |
| retries per message climbing | an external dependency degrading before it fails |

**Runtime invariant checks.** Generated demos assert the invariants the
compiler proved. That harness exists to test the compiler, not to run in
production — the proofs already cover every execution, and re-checking them
on the hot path buys nothing. It is, however, an excellent thing to run in
staging against real traffic shapes.

## Performance characterization

Measured on the `examples/clearinghouse` component — 4 stages, 8 shards
each, multi-handler, full failure-path coverage — on a **single vCPU**
sandbox, release build. Treat these as shape, not as a benchmark for your
hardware.

| Scenario | Messages | Wall time | Notes |
| -------- | -------- | --------- | ----- |
| Calm | 16,000 × 4 stages | ~42 ms | ≈ 380k messages/sec end-to-end |
| Calm, larger | 64,000 × 4 stages | ~210 ms | ≈ 306k messages/sec, linear |
| 20% faults, 50 ms latency injection | 16,000 × 4 stages | ~17.9 s | 15,109 faults absorbed by 14,601 retries and 3,039 recoveries; no policy shedding in that in-process run, all 6 invariants held |

The fault-injected time is dominated by deliberate sleeps, not by the
runtime — it shows the resilience machinery working, not throughput.

**What these numbers do not tell you:** latency percentiles, behaviour on
many cores, or behaviour when an external service is degraded for minutes
rather than milliseconds. Sustained multi-minute degradation is the case
most worth measuring against your own dependencies before committing, and
it is not covered here.

## CI integration

```bash
# Fail the build if the source no longer meets its own spec.
cargo run -p sigilc -- component.sigil out --level 4 --emit-graph

# The generated crate is an ordinary crate.
cd out && cargo build && cargo test
```

Sigil's repository Actions are deliberately manual. Dispatch `ci` after a
commit when a hosted OS/MSRV/Miri/fuzz matrix is useful; select its extended
evidence input when mutation testing and the bounded soak are warranted.
Dispatch `release-artifacts` explicitly for signed release evidence. Neither
workflow runs merely because a commit or tag was pushed.

**Generate in CI or vendor the output?** Both are defensible. Vendoring
makes the compiled artifact reviewable in a pull request — which matters,
because the generated code *is* the thing that runs. Generating in CI keeps
source and output from drifting. If you vendor, add a CI step that
regenerates and diffs, so drift is caught.

**Treat `RESIDUAL_RISK.md` as a reviewable artifact.** A diff to it means the
assurance argument changed, which is exactly when a human should look. See
[RESIDUAL_RISK_PROCESS.md](RESIDUAL_RISK_PROCESS.md).

**Pin the compiler.** For high-assurance environments `sigilc` is part of
your trusted computing base; see [VERSIONING.md](VERSIONING.md).

## What is still on you

Sigil owns concurrency, failure structure, and the proofs. It does not own:

- the correctness of your external transforms;
- the OS, scheduler, and runtime it executes on;
- capacity numbers chosen for your traffic;
- a network transport, durable inbox/outbox, global epoch coordinator, or
  reconciliation across process and host failure;
- whether the invariant you wrote is the invariant your business needs.

That last one deserves emphasis. The compiler proves that
`Settlement.settled <= RiskEngine.cleared` holds in every execution. It has
no opinion on whether that is the property that keeps you solvent. Choosing
the right invariants remains engineering judgement, and the residual-risk
review is where that judgement gets recorded.
