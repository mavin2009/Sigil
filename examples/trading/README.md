# Exchange Order Gateway — Multi-Handler

`OrderGateway → RiskEngine → MatchingEngine`, where **every process handles
both `NewOrder` and `Cancel`**.

```
cargo run -p sigilc -- examples/trading/order_gateway.sigil generated/trading --emit-main --level 4
cd generated/trading
SIGIL_CHAOS_FAIL_PCT=20 SIGIL_CHAOS_LATENCY_MS=100 cargo run --bin demo
```

## Why multi-handler matters here

In real trading systems the new-order path gets scrutinised and the cancel
path quietly does not. A cancel that skips risk is how you get an unhedged
position, or an algo you cannot stop.

Sigil applies the proof obligations **per handler**:

```
hold RiskEngine.cleared       <= OrderGateway.received   PROVEN
hold MatchingEngine.processed <= RiskEngine.cleared      PROVEN
```

Composed: nothing reaches the book without clearing risk — orders *and*
cancels. Delete `cleared := cleared + 1` from just the cancel handler:

```
error[Level 4 (system)]: the `cancel` handler of `RiskEngine` sends toward
`MatchingEngine` but never updates `cleared` — those messages are unbounded
```

One compliant handler never excuses another.

## Dispatch is resolved by type

`send ok to RiskEngine by ok.account` compiles to the right variant because
the compiler infers the sent value's type **locally** (a program-global
environment would let same-named bindings in different processes
cross-contaminate):

```rust
pub enum RiskEngineMsg {
    NewOrder(NewOrder),
    Cancel(Cancel),
}
// ...
Some(out) => out.by_key(&ok.account).send_new_order(ok).await?,   // NewOrder path
Some(out) => out.by_key(&ok.account).send_cancel(ok).await?,      // Cancel path
```

Sending a type the target cannot receive is a compile error, as is declaring
two handlers with the same message name or the same message type.

## Latency budgets are per handler, not summed

A message is dispatched to exactly one handler, so a process contributes the
**maximum** over its handlers to the path budget — not the sum:

```
OrderGateway   max(40, 40)   =  40ms
RiskEngine     max(120, 60)  = 120ms
MatchingEngine max(100, 100) = 100ms
                        total  260ms  <= 400ms SLO
```

## Measured under 20% fault injection

```
[OrderGateway]   received  = 16000
[RiskEngine]     cleared   = 16000   notional_at_risk = 8000.0
[MatchingEngine] processed = 16000
```

`notional_at_risk` is exactly half the message count — only `NewOrder`
carries notional, and the demo drives both handlers evenly. Both message
types flow, both invariants hold, zero loss.
