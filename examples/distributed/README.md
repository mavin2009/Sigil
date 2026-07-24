# Distributed placement execution

`order_fleet.sigil` places `Gateway` separately from the co-located `Risk` and
`Ledger` processes. The compiler emits one remote-capable boundary
(`Gateway -> Risk`) and keeps `Risk -> Ledger` local.

```
cargo run -p sigilc -- \
  examples/distributed/order_fleet.sigil \
  generated/order_fleet \
  --level 3 --emit-graph
```

Inspect `COMPONENT_PLACEMENT`, `transport_manifest`, and the distributed
section of `RESIDUAL_RISK.md` in the generated crate. `Component::start`
remains the in-process reference execution.
`PlacementComponent::start(...).await` starts only `edge` or `core`, routes
the local `Risk -> Ledger` edge as a typed channel, and requires the generated
durable endpoint for `Gateway -> Risk`. A multi-host integration supplies
`Transport`, durable outbox and receiver-commit stores, authenticated service
discovery, and globally fenced epoch/checkpoint coordination.

Level-3 process-local invariants remain provable. Level-2 end-to-end latency
and Level-4 system conservation deliberately fail closed across the remote
boundary until transport delivery semantics are included in those proofs.
