# runnable/counter

Minimal pure counter with Level-2 `hold total >= 0`.

## Compile

```bash
cargo run -p sigilc -- examples/runnable/counter/counter.sigil generated/runnable_counter --emit-main
```

## Level-2

- `hold total >= 0` is **discharged** for pure updates (init 0, pure `add` body).
- Message field `tick.value` is assumed not to break the floor when combined with pure arithmetic (recorded if needed).

## Residual

- Compiled: `add: Int → Int`
- No timed external stages
