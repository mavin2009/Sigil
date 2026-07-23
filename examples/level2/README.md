# level2

Combined Level-2 obligations: SLO bound + pure hold.

## Compile

```bash
cargo run -p sigilc -- examples/level2/slo_and_hold.sigil generated/level2
```

## Specs

```sigil
spec ServiceL2 {
  require path_timeout_sum <= 250.ms   // enrich is 80ms
  hold hits >= 0                       // pure bump
  extinct [null]
}
```

| Obligation | Outcome |
|------------|---------|
| `path_timeout_sum <= 250.ms` | Discharged (80 ≤ 250) |
| `hold hits >= 0` | Discharged (init 0, pure `bump`) |
| `extinct [null]` | Residual assumption |

## Residual

- Compiled: `normalize`, `bump`
- External: `enrich`, `fallback`, `ack`
