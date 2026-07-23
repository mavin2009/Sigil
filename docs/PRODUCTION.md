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
```

Dependencies are `tokio`, `sigil_rt`, `thiserror`, and optionally `tracing`.
No build system, no code generation at build time, no magic. Vendor the
output or generate it in CI — both work, and the choice is discussed under
[CI integration](#ci-integration).

## Wiring external transforms

Empty-bodied transforms compile to stubs. **This is the seam where your real
system attaches, and it is the entire residual risk surface.**

```
transform fetch_secret(r: Request) -> Request {}
```

```rust
async fn fetch_secret(r: Request) -> Result<Request> { ... }
```

Replace the stub with your implementation. Four properties the compiler
assumed and cannot check for you:

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
3. **It must not block the runtime.** Use async I/O or
   `tokio::task::spawn_blocking`. A synchronous call stalls a worker thread,
   which is invisible to every proof in this repo.
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
let mut sink_joins = Vec::new();
for _ in 0..shards {
    let (h, j) = Settlement::new().spawn(capacity)?;
    sinks.push(h);
    sink_joins.push(j);
}

let mut gateways = Vec::new();
for _ in 0..shards {
    let mut g = Gateway::new();
    g.connect_settlement(sinks.clone())?;
    gateways.push(g.spawn(capacity)?);
}
```

Shut down **in topological order**. Dropping a stage's handles closes its
inboxes; its actors drain, release their own outboxes, and the shutdown
cascades downstream:

```rust
drop(gateway_handles);
for j in gateway_joins {
    let (state, stats) = sigil_rt::join_actor(j).await?;
}
drop(sink_handles);
for j in sink_joins {
    let (state, stats) = sigil_rt::join_actor(j).await?;
}
```

Shutting down out of order is where hand-written actor systems hang. The
order is derivable from `topology.mmd`, and the graph is proven acyclic, so
one exists.

## Panics and task supervision

Rust and Tokio do handle task panics: Tokio catches a panic at the spawned
task boundary and returns it as a `JoinError`. Sigil's `join_actor` maps that
to `SigilError::ActorPanicked` without panicking again.

The policy is deliberately **fail-stop**, not restart-and-continue. A panic
may occur after part of a handler changed state or sent a message, so
continuing the same actor or automatically replaying the input could violate
an invariant or duplicate an effect. After a panic, final state and
`ActorStats` are unavailable. Production integration must retain and monitor
every join handle, fail the component or isolate the affected traffic, and
reconcile from a durable source before restarting. Dropping a `JoinHandle`
detaches the task and discards this supervision signal.

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

**Actor statistics** come back with the state at `join()`:

```rust
let (state, stats) = join.await?;
// stats.handled  — messages processed
// stats.dropped  — failed with no recovery path, or rejected by an input guard
// stats.shed     — outbound messages dropped by back-pressure policy
```

On normal actor completion, `handled + dropped` accounts for every completed
or rejected input. Export all three; `shed` in particular is the signal that
a `@shed` edge is doing its job, and a rising `dropped` means input guards are
rejecting traffic. If `join_actor` returns an error there is no final
accounting snapshot; alert on that separately.

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
| 20% faults, 50 ms latency injection | 16,000 × 4 stages | ~17.9 s | 15,109 faults absorbed by 14,601 retries and 3,039 recoveries; **0 messages lost**, all 6 invariants held |

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
- whether the invariant you wrote is the invariant your business needs.

That last one deserves emphasis. The compiler proves that
`Settlement.settled <= RiskEngine.cleared` holds in every execution. It has
no opinion on whether that is the property that keeps you solvent. Choosing
the right invariants remains engineering judgement, and the residual-risk
review is where that judgement gets recorded.
