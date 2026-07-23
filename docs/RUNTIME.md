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
  src/lib.rs          schemas, transforms, processes, actors, smoke test
  src/main.rs         concurrent demo driver (with --emit-main)
  RESIDUAL_RISK.md    what was proven, assumed, and skipped
```

Generated code contains **no `Mutex`, no `Arc`, no atomics, and no
`unsafe`** — asserted by an integration test, not by convention.

## The actor model

Every process compiles to a shared-nothing actor:

```rust
pub struct LedgerHandle {
    tx: tokio::sync::mpsc::Sender<Payment>,
}

impl Ledger {
    pub fn spawn(mut self, capacity: usize)
        -> (LedgerHandle, tokio::task::JoinHandle<(Self, sigil_rt::ActorStats)>)
    {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Payment>(capacity);
        let join = tokio::spawn(async move {
            let mut stats = sigil_rt::ActorStats::default();
            while let Some(msg) = rx.recv().await {
                match self.on_payment(msg).await {
                    Ok(()) => stats.handled += 1,
                    Err(_) => stats.dropped += 1,
                }
            }
            stats.shed = self.__shed;
            (self, stats)
        });
        (LedgerHandle { tx }, join)
    }
}
```

`spawn` takes `self` **by move**. After the call the state is unreachable
except by message; it comes back at `join()`. Data races are not prevented
by discipline — they are unrepresentable.

`ActorStats` gives exact accounting: `handled + dropped` is every message the
actor received, and `shed` counts outbound messages dropped by policy.

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

The demo driver spawns **sinks first** so upstream stages can be wired to
live handles, then shuts down stage by stage in topological order. Closing a
stage's channels drains its actors, which release their outboxes, cascading
the shutdown downstream. No message is stranded in a closed channel, and
nothing hangs waiting on a stage that will never drain.

Shutdown ordering is where hand-written actor systems deadlock. Here it is
derived from the graph the compiler already proved acyclic.

`Router<H>` lives inside a single actor's task, so round-robin needs no
atomics and hashing needs no locks.

## sigil_rt

The runtime is deliberately small:

| Item | Purpose |
| ---- | ------- |
| `SigilError` | `Timeout`, `Transform(String)`, `Schema` |
| `ActorStats` | `{ handled, dropped, shed }` |
| `Router<H>` | shard ring: `round_robin`, `by_key`, `shards` |
| `SendOutcome` | `Delivered` \| `Shed` |
| `backpressure::{block, shed, deadline}` | the three declared policies |
| `chaos` | fault injection and counters |

## Fault injection

External stubs route through `sigil_rt::chaos`, so the verified
`@timeout` / `@retry` / `@recover` machinery can be exercised under load.
Disabled by default (zero latency, zero faults).

| Variable | Meaning |
| -------- | ------- |
| `SIGIL_CHAOS_FAIL_PCT` | percent of external calls that fail (0–100) |
| `SIGIL_CHAOS_LATENCY_MS` | max injected latency per external call |
| `SIGIL_CHAOS_SLOW_PCT` | percent of calls that get injected latency (default 25) |

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
