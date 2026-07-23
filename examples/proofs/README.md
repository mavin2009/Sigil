# proofs

Negative and positive programs that document Level-1 bug prevention.

## Negative (must fail to compile)

| File | Bug class | Expected diagnostic |
|------|-----------|---------------------|
| `unhandled_timeout.sigil` | `@timeout` without `@recover` | Level-1 violation … matching @recover |
| `type_mismatch.sigil` | Pipeline stage type mismatch | expects input type Receipt … has type Order |

```bash
# These should exit non-zero:
cargo run -p sigilc -- examples/proofs/unhandled_timeout.sigil /tmp/should_not_emit
cargo run -p sigilc -- examples/proofs/type_mismatch.sigil /tmp/should_not_emit
```

## Guarantees exercised

1. Unhandled timeout paths cannot be emitted as Rust.
2. Declared transform signatures reject wrong-stage wiring before codegen.
3. State writes are restricted to process-local slots (IR Level-1).

Positive runnable programs live under `examples/runnable/` and the main example directories.
