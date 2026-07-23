# counter

Process-local total updated through a pure compiled transform.

## Compile

```bash
cargo run -p sigilc -- examples/counter/counter.sigil generated/counter
```

## Constructs

| Item | Role |
|------|------|
| `add` | Pure body `x + 1` → compiled into generated Rust |

## Local state

- `total` — running total after each tick

## Residual risk (expected shape)

- Compiled: `add: Int → Int`
- No timed stages
