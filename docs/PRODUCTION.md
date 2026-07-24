# Running Sigil Components in Production

A generated crate is meant to be boring: an ordinary Rust library that your
existing monorepo, CI, and deployment pipeline treat like any other. This
document covers the parts that are not obvious.

- [What you get](#what-you-get)
- [Wiring external transforms](#wiring-external-transforms)
- [Capacity and back-pressure tuning](#capacity-and-back-pressure-tuning)
- [Lifecycle: startup and graceful shutdown](#lifecycle-startup-and-graceful-shutdown)
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

transform fetch_secret(r: Request) -> Request = kms::fetch @effect(idempotent, cancel_safe, read)
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

`spawn(self, capacity)` validates and sets the per-actor inbox size. The
number of queued message slots is bounded, which is the point of bounded
channels:

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

Wire sinks first, then upstream stages, so every outbox points at a live
handle:

```rust
let mut sinks = Vec::new();
let mut supervisor = sigil_rt::Supervisor::new();
for _ in 0..shards {
    let (h, task) = Settlement::new().spawn(capacity)?;
    sinks.push(h);
    supervisor.register(task)?;
}

let mut gateways = Vec::new();
for _ in 0..shards {
    let mut g = Gateway::new();
    g.connect_settlement(sinks.clone())?;
    let (handle, task) = g.spawn(capacity)?;
    gateways.push(handle);
    supervisor.register(task)?;
}
```

Shut down **in topological order**. Dropping a stage's handles closes its
inboxes; its actors drain, release their own outboxes, and the shutdown
cascades downstream:

```rust
drop(gateways);
drop(sinks);
let reports = supervisor.shutdown(Duration::from_secs(10)).await;
for report in reports { export(report); }
```

`Supervisor::next_event` exposes panic/cancellation while the rest of the
component is still running. `ActorTask` is `must_use`, and dropping one
aborts rather than silently detaching it. Dropping `Supervisor` also aborts
every actor it still owns. Shutting down out of order is where hand-written
actor systems hang. The order is derivable from `topology.mmd`, and the graph
is proven acyclic, so one exists.

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
- durable inbox/outbox or reconciliation across process and host failure;
- whether the invariant you wrote is the invariant your business needs.

That last one deserves emphasis. The compiler proves that
`Settlement.settled <= RiskEngine.cleared` holds in every execution. It has
no opinion on whether that is the property that keeps you solvent. Choosing
the right invariants remains engineering judgement, and the residual-risk
review is where that judgement gets recorded.
