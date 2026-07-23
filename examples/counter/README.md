# counter

Process-local total with pure `add` and Level-2 `hold total >= 0`.

## Compile

```bash
cargo run -p sigilc -- examples/counter/counter.sigil generated/counter --emit-main
```

## Level-2

`hold total >= 0` is discharged when init is non-negative and updates are pure.
