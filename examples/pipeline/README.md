# pipeline

Order fulfillment with typed stages and dual recovery paths.

## Compile

```bash
cargo run -p sigilc -- examples/pipeline/pipeline.sigil generated/pipeline
```

## Constructs

| Item | Role |
|------|------|
| `add_fee` | Pure body → compiled into generated Rust |
| `authorize`, `reserve`, `charge`, `confirm` | Empty body → external residual |
| `release`, `refund` | Recover fallbacks for timed stages |
| `@timeout(120.ms)` / `@timeout(200.ms)` | Timed residual stages |
| `Order` → `Receipt` | Declared at `confirm` |

## Local state

- `last_order` — id of the last completed receipt
- `total_charged` — running sum of order amounts

## Residual risk (expected shape)

- Compiled: `add_fee: Order → Order`
- External residual: authorize, reserve, release, charge, refund, confirm
