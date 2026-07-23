# Order Flow — Multi-Stage Topology with Routing Policies

`Gateway → Risk → Settlement → Audit`: four processes, each a fleet of
shared-nothing actors, wired together **by the compiler** from `send`
statements — with three shard-routing policies:

```
send ok to Risk by ok.id        // hash affinity: same id → same shard
send s to Settlement            // round-robin (default)
send done to Audit broadcast    // every shard mirrors every message
```

Float routing keys are rejected at compile time (float hashing is not a
stable shard function) — see `examples/proofs/float_route_key.sigil`.

## Run it

```
cargo run -p sigilc -- examples/concurrent/orderflow/orderflow.sigil generated/orderflow --emit-main --level 2
cd generated/orderflow
cargo run --bin demo                                          # calm
SIGIL_CHAOS_FAIL_PCT=15 SIGIL_CHAOS_LATENCY_MS=120 cargo run --bin demo   # chaos
```

## What the compiler verifies about the topology

- every `send` target is a declared process
- the sent value's type matches the target handler's message type
- the graph is acyclic (cycles over bounded channels can deadlock → rejected)
- every stage retains total failure-path coverage (`@recover`/`@error`)

The generated demo spawns sinks first, wires outboxes upstream, feeds the
entry stage from 64 concurrent producers, then shuts down **stage by stage**:
closing a stage's channels drains its actors, which release their outboxes,
cascading a clean shutdown with no message stranded in a closed channel.

## Measured under chaos (15% faults, 120ms spikes, 3,200 orders)

```
[Gateway]    handled + dropped = 3200 + 0 = 3200
[Risk]       handled + dropped = 3200 + 0 = 3200   (~1,700 recoveries fired)
[Settlement] posted = 3200, total_amount = 3200.0
[Audit]      mirrored = 12800  (= 3200 × 4 shards — broadcast exact)
```

Every message accounted for at every stage, exact float totals, zero locks.
Hand-writing this in Rust means: channel wiring, shutdown ordering (a
notorious deadlock source), per-stage timeout/fallback plumbing, and shared
accumulators — none of it compiler-checked.
