# Clearing & Settlement

A payment clearing house component: `Ingest → RiskGate → Clearing → Settlement`.

```
cargo run -p sigilc -- examples/finance/clearing.sigil generated/clearing --emit-main --level 4
cd generated/clearing
SIGIL_CHAOS_FAIL_PCT=20 SIGIL_CHAOS_LATENCY_MS=120 cargo run --bin demo
```

## What the build proves

| Property | Level | How |
| -------- | ----- | --- |
| `Settlement.settled <= Ingest.admitted` | 4 | structural: count-before-send, all paths through Ingest, static multiplicity |
| `Clearing.netted <= Ingest.admitted` | 4 | same |
| `RiskGate.passed <= Ingest.admitted` | 4 | same |
| `settled_value >= 0` | 3 | exact integer minor units; assumption `payment.amount >= 0` is a **generated runtime guard** |
| `exposure >= 0` | 3 | same |
| worst-case latency ≤ 400ms | 2 | longest path, `(1+retries) × timeout` per stage → **380ms** |
| no data races, no shared accumulators | 1 | by construction — zero `Mutex`/`Arc`/atomics in generated code |
| every external stage has a failure path | 1 | rejected at compile time otherwise |

## The SLO check earns its keep

The first version of this file used 40/60/50/80ms timeouts with a `@retry(2)`
on the risk check. The compiler rejected it:

```
error[Level 2 (contracts)]: Level-2 violation in spec 'ClearingHouse'
at examples/finance/clearing.sigil:100:3: path_timeout_sum is 520ms but
require path_timeout_sum <= 400ms
```

The stage timeouts were tuned to 30/50/40/70ms to fit the declared budget.
That conversation happened at compile time instead of during an incident.

## Why `settled_value` is the interesting field

It is an exact integer minor-unit total accumulated from thousands of
concurrent messages. Values beyond `2^53` remain exact in both the generated
code and the proof domain. Here the value lives inside one actor's task and
is returned at `join()`; shared-accumulator synchronization never arises.

## Measured under 20% fault injection

```
[Ingest]     admitted = 1920   handled + dropped = 1920 + 0
[RiskGate]   passed   = 1920   exposure      = 1920
[Clearing]   netted   = 1920
[Settlement] settled  = 1920   settled_value = 1920
chaos: 10172 external calls, 1769 injected faults, 2492 retries, 838 recoveries
```

1,769 faults, zero settlement discrepancies.
