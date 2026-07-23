# runnable/counter

Minimal program used to show generated Rust updating local state.

## Compile and inspect

```bash
cargo run -p sigilc -- examples/runnable/counter/counter.sigil generated/runnable_counter
```

Generated `src/lib.rs` includes a smoke test and process state fields.
With `--emit-main` (CLI flag), a binary `main` prints `total` after one tick.

## What this shows

- Pure `add` is compiled (not residual)
- Process-local `total` only
- No timeout surface → residual risk has no timed external stages
