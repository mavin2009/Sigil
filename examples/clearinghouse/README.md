# Derivatives Clearing House

The most complex example in the repo, and the one closest to the shape of a
system that is genuinely hard to harden by hand.

```
                     ┌──────────────> AuditTrail      @shed      (best effort)
  Intake ──by acct───┤
                     └──> RiskEngine ──> Settlement   @deadline  (critical)
```

```
cargo run -p sigilc -- examples/clearinghouse/clearing.sigil generated/ch --emit-main --level 4
cd generated/ch
SIGIL_DEMO_CAPACITY=4 SIGIL_CHAOS_FAIL_PCT=15 SIGIL_CHAOS_LATENCY_MS=90 cargo run --bin demo
```

## What makes this hard by hand

| Feature | The hazard it creates |
| ------- | --------------------- |
| **Fan-out with divergent reliability** | audit may shed under load; settlement may not silently vanish. One `send` moved, or one policy copy-pasted, inverts that. |
| **Multi-handler** | trades *and* amendments traverse every stage. Amendments are the path people forget to risk-check. |
| **Conditional acceptance** | `cleared` rises only for accepted trades, `assessed` always. Getting the bound backwards is invisible in review. |
| **Clamped accumulation** | exposure must be bounded on **both** sides. |
| **Two back-pressure policies on one hot path** | the latency claim is only valid because neither blocks. |

## What the build proves

| Property | Level |
| -------- | ----- |
| `RiskEngine.assessed <= Intake.accepted` | 4 |
| `Settlement.settled <= RiskEngine.cleared` | 4 |
| `AuditTrail.recorded <= Intake.accepted` | 4 |
| `cleared <= assessed` (conditional acceptance never exceeds assessment) | 3 (relational) |
| `exposure >= 0`, `settled_value >= 0` | 3 (exact integer minor units, inductive via clamping) |
| end-to-end latency ≤ 500 ms **including queue hand-off** | 2 |
| no data races, no shared accumulators, every failure path declared | 1 |

## Two bugs the compiler caught while this file was being written

**1. Forwarding and counting must agree.** The first draft forwarded *every*
message to Settlement but incremented `cleared` only conditionally, so a
rejected trade still reached Settlement:

```
GAP fails — the `trade` handler of `RiskEngine` forwards up to 1 message(s)
toward `Settlement`, each able to add 1 to `settled`, but only guarantees
+0 to `cleared`.
```

The fix is a conditional send:

```
send checked to Settlement @deadline(5.ms) when checked.lots > 0
```

The prover evaluates the counter's delta **under that same guard**, so the
correlation is proven rather than assumed. Remove the `when` and the build
fails again — the guard is load-bearing, and a test asserts it.

**2. A one-sided clamp.** Capping the maximum and forgetting the minimum is
the classic shipped bug:

```
let bounded = if checked.notional > 1000000 { 1000000 } else { checked.notional }
```

```
INDUCTIVE STEP fails — update `exposure := exposure + bounded` yields
the full `i64` range, which can escape `exposure >= 0`
```

The prover evaluates each branch under the **narrowed** condition, so a
two-sided clamp proves and a one-sided one does not. See
`examples/proofs/one_sided_clamp.sigil`.

## Measured under overload (capacity 4, 15% faults, 90 ms spikes)

```
[Intake]     accepted = 480   shed downstream = 229
[AuditTrail] recorded = 359
[RiskEngine] assessed = 372   cleared = 372   exposure = 372.0   shed = 44
[Settlement] settled  = 328   settled_value = 328.0
```

Every number reconciles:

- Intake issues `2 × 480 = 960` sends, sheds 229 → `731 = 372 + 359` ✓
- RiskEngine sheds 44 → `372 − 44 = 328` ✓
- `328 ≤ 372 ≤ 480` and `359 ≤ 480` — every proven invariant holds ✓

## Hardening

The generated crate forbids `unsafe` outright and enables overflow checks in
every profile. Monetary values use exact integer minor units in both the
generated code and the Level-3 proof domain; wrapped counters cannot be
installed as state.

The audit path degraded under pressure, exactly as declared. The settlement
path stayed fully accounted. Nothing was lost silently, and no invariant
needed to be re-checked at runtime — the proofs already covered every drop
the language admits.
