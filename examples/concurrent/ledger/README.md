# Concurrent Ledger

A payment pipeline compiled into a **fleet of shared-nothing actors** — the
kind of program that is easy to get subtly wrong in hand-written Rust and is
safe here by construction.

## Run it

```
cargo run -p sigilc -- examples/concurrent/ledger/ledger.sigil generated/ledger --emit-main --level 2
cd generated/ledger && cargo run --bin demo
```

## What the demo does

- Spawns **8 isolated `Ledger` actors** (state moves into each actor's task;
  after `spawn` it is unreachable except via messages)
- Fires **16,000 payments from 64 concurrent producer tasks** on a
  multi-threaded tokio runtime
- Each payment flows through `validate → risk_check @timeout(50.ms)
  @recover(release) → hold_funds @timeout(80.ms) @recover(refund) → post`
- Prints per-shard and aggregate state after all channels drain

## Expected output

```
aggregate posted       = 16000
aggregate total_amount = 16000.0
elapsed = ... (locks used: 0)
```

Both aggregates are **exact**. `total_amount` is an `f64` accumulator mutated
from 16,000 concurrent messages — in plain Rust that requires
`Arc<Mutex<f64>>` (floats have no atomic ops), and every timed stage needs
hand-rolled `tokio::time::timeout` + fallback plumbing that the compiler
cannot check you got right.

## Why this is hard to make safe in regular Rust

| Hazard in hand-written Rust            | Sigil                                          |
| -------------------------------------- | ---------------------------------------------- |
| Shared `f64` needs `Mutex` (deadlocks, contention) | State is task-local by construction   |
| Forgetting a timeout fallback → hung request        | `@timeout` without `@recover` fails the build |
| Accidentally holding a lock across `.await`         | No locks exist to hold             |
| State observable mid-update from another thread     | Only reachable via `join()` after channel close |

The generated crate contains **zero** `Mutex`, `Arc`, atomics, or `unsafe` —
enforced by an integration test (`emitted_process_is_a_lock_free_actor`).
