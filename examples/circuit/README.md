# circuit

Outbound call with timeout recovery and process-local status.

## Compile

```bash
cargo run -p sigilc -- examples/circuit/circuit.sigil generated/circuit
```

## Constructs

| Item | Role |
|------|------|
| `validate` | Residual preprocess |
| `call_service` | Residual, `@timeout(50.ms)` |
| `open_circuit` | Recover fallback |
| `record` | Residual postprocess |

## Local state

- `failures` — reserved for failure counting
- `last_status` — last response status (e.g. from recover path)

## Residual risk (expected shape)

- All transforms external residual (empty bodies)
- Timed stage: 50ms with recover `open_circuit`
