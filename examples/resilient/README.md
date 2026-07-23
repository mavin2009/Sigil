# resilient

Event processing with one compiled pure stage and timed residual enrichment.

## Compile

```bash
cargo run -p sigilc -- examples/resilient/resilient.sigil generated/resilient
```

## Constructs

| Item | Role |
|------|------|
| `normalize` | Pure body → compiled |
| `enrich` | Residual, `@timeout(80.ms)` |
| `fallback` | Recover path for enrich |
| `store` | Residual, `Event → Result` |

## Local state

- `last_ok` — last stored result id
- `processed` — event count

## Residual risk (expected shape)

- Compiled: `normalize: Event → Event`
- External residual: enrich, fallback, store
