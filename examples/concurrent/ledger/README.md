# Concurrent Ledger

A payment pipeline compiled into a **fleet of shared-nothing actors** with
**total failure-path coverage** — verified at build time, demonstrated under
fault injection at run time.

## Run it

```
cargo run -p sigilc -- examples/concurrent/ledger/ledger.sigil generated/ledger --emit-main --level 2
cd generated/ledger

# Calm run: 16,000 payments, 8 actors, 64 producers
cargo run --bin demo

# Chaos run: 15% injected faults, latency spikes past both timeouts
SIGIL_CHAOS_FAIL_PCT=15 SIGIL_CHAOS_LATENCY_MS=150 cargo run --bin demo
```

Demo load is tunable: `SIGIL_DEMO_SHARDS`, `SIGIL_DEMO_PRODUCERS`,
`SIGIL_DEMO_MSGS`. Chaos knobs: `SIGIL_CHAOS_FAIL_PCT`,
`SIGIL_CHAOS_LATENCY_MS`, `SIGIL_CHAOS_SLOW_PCT`.

## Measured result (identical chaos, before/after the failure-path rule)

| Pipeline                                   | faults injected | recoveries | dropped | aggregates |
| ------------------------------------------ | --------------- | ---------- | ------- | ---------- |
| v1: untimed stages unprotected             | 1,749           | 1,330      | **1,031** | exact conservation (posted + dropped = total) |
| v2: every stage `@recover`, pure fallbacks | 1,807           | 2,529      | **0**     | posted = total exactly |

v1 no longer compiles: the Level-1 failure-path check rejects any external
stage lacking `@recover` or an explicit `@error`. The design that survives
chaos is the only design the compiler accepts.

## Why recovery paths are pure transforms

`quarantine`, `release`, `refund` have bodies, so they compile into the crate:
infallible, un-slowable, exempt from chaos. A fallible fallback reintroduces
exactly the loss it exists to prevent — v1's drops included recover stubs
failing mid-recovery.

## Why this is hard to make safe in regular Rust

| Hazard in hand-written Rust                          | Sigil                                        |
| ---------------------------------------------------- | -------------------------------------------- |
| Shared `f64` needs `Arc<Mutex<f64>>` (no float atomics) | State is task-local by construction       |
| Forgetting a timeout fallback → hung request         | `@timeout` without `@recover` fails the build |
| Forgetting error handling on one RPC → silent loss   | Untagged external stage fails the build      |
| Holding a lock across `.await`                       | No locks exist to hold                       |
| Message loss invisible until reconciliation          | Actors count handled/dropped; demo asserts conservation |

Generated code contains **zero** `Mutex`, `Arc`, atomics, or `unsafe`
(enforced by test `emitted_process_is_a_lock_free_actor`).
