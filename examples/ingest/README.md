# ingest

Telemetry path with timed decompression and Metrics extraction.

## Compile

```bash
cargo run -p sigilc -- examples/ingest/ingest.sigil generated/ingest
```

## Constructs

| Item | Role |
|------|------|
| `validate` | Residual |
| `decompress` | Residual, `@timeout(50.ms)` |
| `empty` | Recover fallback |
| `extract` | Residual, `Telemetry → Metrics` |
| `store` | Residual, Metrics |

## Local state

- `last` — last metrics id
- `count` — packet count
