# Production Maturity and Release Gate

Sigil-generated Rust is used in production. This document records the
engineering evidence required to keep that use disciplined: every **P0** item
must remain closed and release candidates must pass the release gate from a
clean checkout. A passing example or fuzz campaign is evidence, not a
substitute for a soundness argument.

Status marks:

- `[x]` implemented and covered by an automated regression;
- `[ ]` open;
- `[~]` partially addressed, with the remaining acceptance criteria stated.

## P0 — proof and language soundness

- [x] **Define the numeric semantics and make the prover implement them.**
  The Level 3/4 proof fragment is now exact `Int` only. Its interval endpoints
  are `i128`, per-shard assertions compare `i64` directly (aggregate checks
  use `i128`), strict bounds are discrete, and each emitted arithmetic
  operation is intersected with the successful `i64` range. Generated crates
  enable overflow checks in every profile, so an overflowing operation fails
  before wrapped state can be installed. Tests cover `i64::{MIN,MAX}`, values
  on both sides of `2^53`,
  mixed proof operands, strict comparisons, and overflow. `Float` remains an
  executable type, with non-finite input rejected, but every Float hold fails
  closed at Level 3/4. No IEEE-754 theorem is emitted.

- [x] **Write reviewable soundness arguments for every Level 3/4 rule.**
  [`SOUNDNESS.md`](SOUNDNESS.md) states the formal preconditions and
  preservation arguments for scalar induction, relational deltas, guard
  correlation, topology flow, ordering, multiplicity, shedding, effects, and
  panic cut-points. Its evidence table maps every premise to its isolated
  analysis module and adversarial regression.

- [x] **Complete the static type and name system.**
  `check_types` covers every transform return/call argument, initializer,
  assignment, condition, spec operand, field access, exact schema literal,
  send target/value, and route key before codegen. Negative tests cover
  missing/extra/duplicate fields and name/type holes. A shrinkable typed-AST
  strategy plus a complete primitive-schema fixture enforce
  Level-1 acceptance ⇒ generated Rust type-checks.

- [x] **Eliminate compiler panics and resource-exhaustion paths.**
  Production Clippy denies `unwrap`, `expect`, and `panic`; parser/lowering/
  topology/codegen paths return typed diagnostics. Source bytes, declarations,
  statements, expressions, identifier/string lengths, and nesting are
  bounded. Budget/multiplicity arithmetic is checked. Six cargo-fuzz targets
  cover all public pipeline stages, and the process harness treats panic,
  abort, signal, and timeout as crashes.

- [x] **Specify panic and partial-handler semantics in the proof model.**
  The fail-stop boundary matrix in [`SOUNDNESS.md`](SOUNDNESS.md) identifies
  what survives every injected cut-point and when assurance terminates.
  Generated code injects before/after state writes, sends, retries, and
  foreign calls. Panics produce incomplete supervisor records; automatic
  replay is forbidden and reconciliation is documented.

- [x] **Close the external-effect and cancellation gap.**
  Every binding declares idempotency, cancellation, and side-effect class in
  `SIGIL_EFFECTS.json`; retries and binding kinds are statically constrained.
  Tests cover async cancellation and completion-tracked blocking work.
  Residual JSON records each exact application-owned contract.

- [x] **Define durability guarantees.**
  Sigil proves in-process executions only. Tokio inboxes and actor state are
  explicitly volatile across process/host failure, `@block` is not a
  durability acknowledgement, all “zero loss” claims were narrowed, and
  rollback/reconciliation runbooks require an external durable source.

## P0 — runtime lifecycle and failure containment

- [x] Reject zero/overlarge actor capacities before Tokio can panic.
- [x] Reject empty router shard sets before modulo/index operations.
- [x] Distinguish stopped, panicked, and cancelled actors with typed errors.
- [x] Generated demos propagate producer and actor task failures.
- [x] **Provide a production supervisor API.** `Supervisor` exposes live
  snapshots and termination events, rejects duplicate registration, owns
  every `must_use` `ActorTask`, and enforces deadline shutdown with undrained
  reports. Dropping a task or its owning supervisor aborts rather than
  detaches.
- [x] **Test shutdown under every queue policy and failure point.**
  Paused-time tests cover block/shed/deadline, full and closed queues, blocked
  sender cancellation, actor panic, shutdown timeout, producer disappearance,
  clean stop, and repeated registration/stop without wall-clock thresholds.
- [x] **Harden accounting overflow and snapshots.**
  Actor/chaos/external-work counters saturate, snapshots are live, accepted
  messages are counted only after channel acceptance, and every abnormal
  termination carries explicit incomplete accounting.

## P1 — verification suite

- [x] Fast compiler suite covers parsing, checks, provers, topology, and
  codegen.
- [x] Runtime has direct regression tests for routing, configuration,
  back-pressure closure, and panic conversion.
- [x] The three end-to-end chaos cases run in the default test gate and use
  race-free output directories.
- [x] Fuzz scripts resolve the current checkout instead of a developer's
  absolute path, write under `target/`, fail their exit status, and classify
  aborts/timeouts/signals as crashes.
- [x] Add coverage-guided `cargo-fuzz` targets for parser, checker pipeline,
  topology, Level 3, Level 4, and codegen; seed/minimized corpora are tracked.
- [x] Add property generators for well-typed ASTs plus shrinkers. Required
  properties: parse/print stability, accepted⇒type-checks, proof-preserving
  alpha-renaming, independent-statement permutation where legal, and
  proven⇒not refuted by the reference interpreter.
- [x] A codegen-independent executable reference semantics differentially
  checks generated Rust state and ordered sends, in addition to proof
  non-refutation properties.
- [x] Extended manual cargo-mutants runs cover Level 3, Level 4, and topology;
  [`SOUNDNESS.md`](SOUNDNESS.md) maps every proof premise to its negative
  example, so removing a premise is a test failure.
- [x] Loom models exhaust lifecycle/accounting, concurrent ingress cursor,
  shard ownership admission/drain, and panic/shutdown races; CI runs Miri on
  runtime unit tests.
- [x] A bounded manually dispatched soak publishes latency percentiles, memory
  estimate, queue peak, retries, and shutdown duration with exact accounting.
- [x] CI tests Linux, macOS, and Windows on MSRV and latest stable and compiles
  every positive generated example with no-default and all features while
  denying Rust warnings.

## P1 — release, supply chain, and operations

- [x] Establish a clean verification workflow. Formatting, strict production
  panic lints, docs, tests, property/differential/chaos/fuzz/model/Miri gates,
  pinned Actions, dependency policy, platform/MSRV matrix, coverage, and
  reproducibility are configured. The workflow is manually dispatched after
  selected commits and for release candidates rather than on every push.
- [x] Declare and test an MSRV. Workspace and generated crates declare Rust
  1.85; CI also runs latest stable and pinned verification Rust 1.97.
  `SIGIL_BUILD.json` records compiler/language/runtime, ABI schemas, MSRV,
  verification toolchain, lockfile SHA, source SHA, and runtime path.
- [x] `cargo-audit`/RustSec and `cargo-deny` enforce advisories, explicit
  licenses, registries/git sources, wildcards, and unique critical crates.
- [x] Manually dispatched release builds generate CycloneDX SBOMs,
  deterministic archives, SHA-256, keyless Sigstore bundles, and GitHub
  provenance; CI verifies generated artifacts reproduce byte-for-byte.
- [x] Code generation uses a sibling lock, fully populated staging tree,
  same-filesystem publish rename, rollback, and deliberate stale-file removal.
- [x] Generated ABI is version 2 and the residual-risk schema is version 1,
  embedded in crate metadata and guarded by immutable compatibility fixtures.
  Migration rules are in [`ABI.md`](ABI.md).
- [x] [`RUNBOOKS.md`](RUNBOOKS.md) covers panic, stuck transforms, overload,
  partial shutdown, dependency outage, rollback, reconciliation, and upgrade.
- [ ] Complete an independent security review and an independent proof-core
  review; track findings to closure.
- [x] Production deployments exercise generated Rust in real systems.
  Operational owners remain responsible for rehearsing deploy, rollback,
  overload, dependency degradation, and recovery for each component.

## Completed maturity feature — generated component assembly

The compiler already owns and verifies the process graph. Application code
previously had to repeat that graph when it spawned actors, connected
outboxes, retained ingress handles, registered tasks, and shut stages down.
The generated production component API makes the verified topology itself
executable:

- [x] Emit a typed `ComponentConfig` with shard count and inbox capacity for
  every process.
- [x] Emit a `Component::start(config)` that validates configuration, creates
  sinks first, connects every route, registers every task, exposes only
  declared ingress handles, and owns the supervisor.
- [x] Emit topological graceful shutdown so application code cannot strand a
  downstream stage or reproduce the graph incorrectly.
- [x] Differentially test generated assembly against the compiler topology,
  including zero/overlarge capacity, multiple shards, startup failure, panic,
  and deadline shutdown.

This removes the largest remaining piece of difficult hand-written wiring
without inventing a second runtime or hiding the generated Rust.

## Completed scalability feature — production ingress and health

- [x] Entry processes expose cloneable concurrent ingress routers with one
  shared atomic round-robin sequence across producer tasks.
- [x] External affinity routing uses the same versioned stable-key contract as
  compiler-generated internal edges.
- [x] Generated handles expose blocking, immediate-shed, and duration-deadline
  admission without requiring callers to manipulate raw channels.
- [x] Generated component health reports expected and running actors plus live
  accounting snapshots; readiness fails when any supervised shard is no
  longer running.
- [x] Concurrent distribution, queue saturation, deadline admission,
  actor-count overflow, panic degradation, and generated-crate execution are
  regression-tested.

The ingress cursor is specifically an atomic runtime mechanism; this is not a
blanket claim that Tokio, allocation, or the component is universally
lock-free. Shard count is static for one component run, and changing it
remaps affinity keys.

## Completed distributed foundation — placement and state handoff

Massive cross-host scaling now has an explicit compiler/runtime boundary
instead of treating a remote queue as if it were an in-memory handle:

- [x] `placement` groups declare co-location; the compiler rejects incomplete,
  unknown, duplicate, or multiply assigned processes and emits only verified
  cross-group edges as remote-capable boundaries.
- [x] The runtime `Transport` contract carries durable message identity,
  bounded admission, version/routing/schema negotiation, payload ceilings,
  and explicit at-most-once or at-least-once semantics. Exactly-once is not
  claimed.
- [x] `ShardLease` combines phase and live ownership-permit count in one atomic
  state word. Drain rejects new work, handoff cannot complete with a live
  permit, the old owner retires before the successor can activate, and stale
  epochs are rejected.
- [x] `ShardCheckpoint` moves application state and the deduplication frontier
  together; `DedupWindow` provides bounded exact duplicate suppression.
- [x] Type/acyclic topology checks and process-local invariants remain valid,
  while Level-2 end-to-end latency and Level-4 system conservation fail closed
  across remote admission. Generated residual risks assign loss, duplication,
  reordering, partition, rolling-version skew, durable identity, and global
  epoch coordination.
- [x] A deterministic multi-node harness exercises accepted-but-lost,
  duplicate, reordered, partitioned, capacity-shed, stale-epoch, and
  checkpoint-handoff behavior. Loom exhaustively checks the ownership
  admission/drain race; it found and prevented a two-atomic weak-memory bug.
- [x] Every finite compiler-owned schema receives a deterministic, versioned
  wire codec and typed envelope helpers. Total payload and individual field
  allocation are bounded before allocation; malformed lengths, truncation,
  trailing bytes, invalid UTF-8/booleans/durations, and non-finite floats fail
  closed. Remote edges with foreign or incomplete schemas are rejected.
- [x] Each exact schema version carries a compiler-derived SHA-256 structural
  fingerprint covering schema/field names, order, primitive kinds, and nested
  fingerprints. Negotiation selects the highest version whose fingerprints
  also match and rejects same-version layout skew.
- [x] ABI v4 fixtures pin the generated codec/helper surface, and an
  end-to-end generated-crate test round-trips nested schemas through
  negotiation and retains the ownership permit until application processing
  finishes.
- [x] `RemoteEndpoint<T>` provides typed round-robin, stable-key, and broadcast
  routing over fenced destination shards. Generated `RemoteEndpoints`
  configuration validates each endpoint against the declared placement and
  target process and rejects cross-deployment mixtures. Remote `@block` sends
  fail compilation; at-least-once endpoints reject volatile message-ID state.
- [x] ABI v5 adds a durable producer outbox and typed durable endpoint.
  Exact envelope bytes and attempt increments are persisted before transport
  I/O; retry exhaustion retains pending work; only a matching durable receiver
  receipt can remove it, and repeated acknowledgements are idempotent.
- [x] Ownership permits are owned capabilities that can cross the bounded
  actor inbox. Generated remote receivers retain the permit through handler
  execution and an atomic application-state/dedup commit, fail-stop on apply
  or commit failure, and suppress committed replays.
- [x] `PlacementComponent::start` starts and supervises only one declared
  placement, wires same-placement edges locally, requires durable endpoints
  for outbound boundaries, restores one durable receiver shard transaction
  before spawn, and exposes typed receiver handoff methods.
- [x] A generated two-placement executable test covers persist-before-send,
  bounded transport admission, receiver permit lifetime, durable ack removal,
  receiver restart/restore, duplicate suppression, and subsequent state
  continuity. Runtime tests separately cover partitions, retry exhaustion,
  and idempotent repeated acknowledgement.

## Remaining distributed infrastructure tranche

Sigil now supplies the language/runtime durability and placement assembly.
The following deployment-owned infrastructure remains intentionally outside
the language runtime:

- [x] a transport-neutral durable outbox/ack/retry adapter layered over
  `Transport`, with explicit storage contracts and typed generated routing;
- [x] remote component assembly that starts only the processes assigned to the
  local placement group and routes cross-group edges through durable endpoints;
- [ ] an authenticated/encrypted network `Transport`, service discovery,
  durable store implementations, and deployment-specific telemetry;
- [ ] integration with a strongly consistent epoch allocator and durable,
  authenticated checkpoint store; and
- [ ] a real multi-process/host test environment in addition to the
  deterministic fault model.

## Release gate

A release candidate is admissible only when:

1. every P0 box is closed with linked tests and review evidence;
2. the full CI matrix passes from a clean, offline-capable checkout;
3. generated artifacts reproduce byte-for-byte from the recorded inputs;
4. the residual-risk report has no unowned item or unreviewed change;
5. an operational owner signs the panic, durability, capacity, and external
   transform contracts; and
6. the project status and public claims match the evidence above.

Describe Sigil as a production-used language and compiler that generates
ordinary Rust and checks the claims inside its documented model. Keep the
scope precise: Sigil-emitted process state is shared-nothing, but the whole
runtime is not universally lock-free; panics are contained and reported, not
impossible; in-memory queues are not durable; and foreign effect contracts
remain application-owned.
