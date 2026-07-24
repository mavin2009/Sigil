# Production-Readiness Gate

Sigil is not production-ready until every **P0** item below is closed and
the release gate passes from a clean checkout. A passing example or fuzz
campaign is evidence, not a substitute for a soundness argument.

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
- [x] Scheduled cargo-mutants runs cover Level 3, Level 4, and topology;
  [`SOUNDNESS.md`](SOUNDNESS.md) maps every proof premise to its negative
  example, so removing a premise is a test failure.
- [x] Loom models exhaust lifecycle/accounting and panic/shutdown races; CI
  runs Miri on runtime unit tests.
- [x] A bounded scheduled soak publishes latency percentiles, memory
  estimate, queue peak, retries, and shutdown duration with exact accounting.
- [x] CI tests Linux, macOS, and Windows on MSRV and latest stable and compiles
  every positive generated example with no-default and all features.

## P1 — release, supply chain, and operations

- [~] Establish a mandatory clean CI gate. Formatting, strict production
  panic lints, docs, tests, property/differential/chaos/fuzz/model/Miri gates,
  pinned Actions, dependency policy, platform/MSRV matrix, coverage, and
  reproducibility are configured. **External remaining gate:** enable hosting
  branch protection requiring these checks.
- [x] Declare and test an MSRV. Workspace and generated crates declare Rust
  1.85; CI also runs latest stable and pinned verification Rust 1.97.
  `SIGIL_BUILD.json` records compiler/language/runtime, ABI schemas, MSRV,
  verification toolchain, lockfile SHA, source SHA, and runtime path.
- [x] `cargo-audit`/RustSec and `cargo-deny` enforce advisories, explicit
  licenses, registries/git sources, wildcards, and unique critical crates.
- [x] Tag builds generate CycloneDX SBOMs, deterministic archives, SHA-256,
  keyless Sigstore bundles, and GitHub provenance; CI verifies generated
  artifacts reproduce byte-for-byte.
- [x] Code generation uses a sibling lock, fully populated staging tree,
  same-filesystem publish rename, rollback, and deliberate stale-file removal.
- [x] Generated ABI and residual-risk schema are version 1, embedded in crate
  metadata, and guarded by golden compatibility fixtures. Migration rules are
  in [`ABI.md`](ABI.md).
- [x] [`RUNBOOKS.md`](RUNBOOKS.md) covers panic, stuck transforms, overload,
  partial shutdown, dependency outage, rollback, reconciliation, and upgrade.
- [ ] Complete an independent security review and an independent proof-core
  review; track findings to closure.
- [ ] Run at least one bounded-scope production pilot long enough to exercise
  deploy, rollback, overload, dependency degradation, and recovery.

## Release gate

A release candidate is admissible only when:

1. every P0 box is closed with linked tests and review evidence;
2. the full CI matrix passes from a clean, offline-capable checkout;
3. generated artifacts reproduce byte-for-byte from the recorded inputs;
4. the residual-risk report has no unowned item or unreviewed change;
5. an operational owner signs the panic, durability, capacity, and external
   transform contracts; and
6. the project status and public claims match the evidence above.

Until then, Sigil should be described as a research/prototype compiler with
promising executable evidence—not as a lock-free, panic-free, lossless, or
production-proven system.
