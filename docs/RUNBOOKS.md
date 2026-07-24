# Operational Runbooks

These runbooks assume generated artifacts, `SIGIL_BUILD.json`,
`SIGIL_EFFECTS.json`, `RESIDUAL_RISK.json`, topology files, and supervisor
events are retained with the deployment.

## Actor panic

1. Page on `ActorTermination::Panicked`; mark the component failed.
2. Capture the actor's last `ActorSnapshot`, `undrained`, chaos report, build
   manifest, and panic backtrace. Do not report complete conservation.
3. Stop upstream producers and run bounded supervisor shutdown.
4. Do not replay the in-flight message automatically. A send or foreign write
   may already have completed.
5. Reconcile from the system's durable source/idempotency ledger, then deploy
   a fixed or known-good artifact.

## Stuck transform

1. Compare actor snapshots over time and inspect `blocking_active`.
2. For a timed async transform, verify the implementation really satisfies
   its `cancel_safe` declaration.
3. For blocking work, wait for `blocking_active` to return to baseline or
   isolate the dependency; aborting the await cannot stop its OS thread.
4. At the shutdown deadline, accept the explicit incomplete report and
   reconcile. Never extend deadlines indefinitely just to obtain a clean
   counter.

## Sustained overload

1. Alert on rising queue occupancy, `shed`, deadline expiry, and retry rate.
2. If the edge is `@block`, throttle admission before producers accumulate.
3. If it is `@shed`/`@deadline`, confirm the loss is within the product's
   declared policy and that downstream inequalities still hold.
4. Scale shards only after validating affinity/order requirements and repeat
   the bounded soak test with production message sizes.

## Partial shutdown

1. Stop admission, then close actors in topological order from sources to
   sinks.
2. Call `Supervisor::shutdown` with the owned deadline.
3. Record every `ShutdownDeadline`/`Cancelled` report and its `undrained`
   value. Do not discard the report or call the run clean.
4. Reconcile undrained accepted messages from the durable upstream system.

## Dependency outage

1. Identify the transform in `SIGIL_EFFECTS.json`; confirm idempotency,
   cancellation, and side-effect class.
2. Watch retry and recovery counters. Do not raise retry counts without
   re-running the Level 2 latency proof and capacity test.
3. For a non-idempotent operation, never introduce retries. Route to its
   explicit terminal error/reconciliation path.
4. If recovery cannot sustain load, stop admission and degrade the component
   deliberately.

## Rollback

1. Select a signed artifact whose generated ABI and residual schema are
   accepted by the operator.
2. Verify its SHA-256, Sigstore bundle, provenance, `Cargo.lock`, source hash,
   and residual-risk owner approvals.
3. Stop the current component using the partial-shutdown procedure.
4. Deploy the prior artifact and reconcile messages spanning the cutover.
   In-memory inboxes are never assumed to survive it.

## Reconciliation

1. Establish a durable input interval or idempotency-key range.
2. Compare it with accepted/handled/dropped/shed and undrained snapshots.
3. Query foreign write systems before replay; an `after_foreign_call` panic
   can mean the write committed.
4. Replay only operations proven absent or made idempotent by the external
   system. Record exceptions and owner approval.

## Compiler upgrade

1. Read the changelog and ABI/residual-schema migration notes.
2. Regenerate in a clean checkout and compare `SIGIL_BUILD.json`,
   `SIGIL_EFFECTS.json`, `RESIDUAL_RISK.json`, proof output, topology, and
   generated Rust.
3. Require byte-for-byte regeneration from identical inputs.
4. Re-run the full platform/MSRV matrix, chaos/property suite, dependency
   policy, and bounded soak.
5. Obtain fresh owner approval for every changed residual or effect contract
   before promotion.
