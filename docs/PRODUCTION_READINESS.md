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

- [ ] **Write reviewable soundness arguments for every Level 3/4 rule.**
  Acceptance: a document states formal preconditions and preservation
  arguments for scalar induction, relational deltas, guard correlation,
  topology flow, ordering, multiplicity, shedding, and failure cut-points;
  each premise maps to one checker and adversarial negative test; the
  ORDERING/counting core is isolated from CLI/codegen.

- [ ] **Complete the static type and name system.** `[~]`
  Duplicate declarations, Rust keywords, unknown schema types, generated
  type collisions, ambiguous state names, actor-field collisions, invalid
  dependency requirements, and TOML injection are now rejected/tested.
  Numeric initializers, assignments, conditionals, spec operands, and spec
  field access are now checked too. Remaining acceptance: type-check every
  transform return, call argument, non-numeric assignment, field access,
  schema literal (missing/extra/duplicate fields), and route key before codegen.
  The property `accepted at Level 1 => generated crate type-checks` must be
  exhaustive over a typed AST generator, not only sampled text.

- [ ] **Eliminate compiler panics and resource-exhaustion paths.** `[~]`
  Missing/invalid CLI arguments and known parser nesting attacks are typed
  diagnostics. Remaining acceptance: remove or prove every production
  `unwrap`/`expect`, use checked arithmetic in graph multiplicity and budget
  calculations, bound source bytes/declaration counts/identifier and string
  lengths, fuzz all public compiler entry points, and treat abort, signal,
  timeout, and panic as failures.

- [ ] **Specify panic and partial-handler semantics in the proof model.**
  The runtime is now fail-stop and exposes `ActorPanicked`; it does not
  pretend a panic is a counted drop. Acceptance: prove which invariants
  survive a panic at every statement boundary, or generate transactional
  state/effect staging; define recovery/replay rules; add panic injection
  before/after state writes, sends, retries, and foreign calls.

- [ ] **Close the external-effect and cancellation gap.**
  Acceptance: every bound transform declares idempotency, cancellation, and
  side-effect semantics in machine-readable metadata; timeout cancellation
  tests exercise each supported class; detached work cannot silently escape
  accounting; residual reports identify the exact unproved contract.

- [ ] **Define durability guarantees.**
  Tokio MPSC queues are in-memory. Process/host failure can lose queued
  messages even when `@block` succeeded. Acceptance: either state clearly
  that Sigil proves only in-process executions and removes “zero loss”
  language outside that scope, or add durable inbox/outbox protocols with
  crash/restart and duplicate-delivery proofs.

## P0 — runtime lifecycle and failure containment

- [x] Reject zero/overlarge actor capacities before Tokio can panic.
- [x] Reject empty router shard sets before modulo/index operations.
- [x] Distinguish stopped, panicked, and cancelled actors with typed errors.
- [x] Generated demos propagate producer and actor task failures.
- [ ] **Provide a production supervisor API.** Retaining joins until shutdown
  is too late for detection. Acceptance: health/failure notification is
  observable while running; every actor is registered; a dropped join cannot
  silently detach; shutdown has a deadline and reports undrained messages.
- [ ] **Test shutdown under every queue policy and failure point.**
  Acceptance: deterministic tests cover blocked senders, full queues,
  downstream panic, cancellation, timeout, producer disappearance, and
  repeated start/stop; no test relies on wall-clock luck.
- [ ] **Harden accounting overflow and snapshots.**
  Acceptance: stats and chaos counters have defined saturating/checked
  semantics; live snapshots are available; a failed actor produces an
  explicit incomplete-accounting record rather than no telemetry.

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
- [ ] Add coverage-guided `cargo-fuzz` targets for parser, checker pipeline,
  topology, Level 3, Level 4, and codegen; preserve a minimized corpus.
- [ ] Add property generators for well-typed ASTs plus shrinkers. Required
  properties: parse/print stability, accepted⇒type-checks, proof-preserving
  alpha-renaming, independent-statement permutation where legal, and
  proven⇒not refuted by the reference interpreter.
- [ ] Build a small executable reference semantics and differential-test
  generated Rust against it. The current demo assertions only sample
  end-state predicates and can miss trace/order discrepancies.
- [ ] Add mutation testing for every proof premise and negative example; the
  suite must fail when a premise check is removed.
- [ ] Add concurrency model tests (Loom or an equivalent) for lifecycle and
  channel/supervisor interactions; run Miri on runtime unit tests.
- [ ] Run long soak/degradation tests with bounded resources and publish
  latency, memory, queue, retry, and shutdown distributions—not only totals.
- [ ] Test Linux, macOS, and Windows on the declared MSRV and latest stable
  Rust; compile every generated example with all feature combinations.

## P1 — release, supply chain, and operations

- [~] Establish a mandatory clean CI gate. Formatting, strict Clippy, docs
  with warnings denied, all tests, generated-crate/chaos runs, and three fuzz
  smoke campaigns are configured. Remaining acceptance: protect the branch,
  pin Actions by commit, and add dependency policy, the platform/MSRV matrix,
  coverage, and artifact reproducibility.
- [~] Declare and test an MSRV. The verification toolchain is pinned to
  Rust 1.97.0. Remaining acceptance: declare a distinct minimum supported
  Rust version, test both it and the pinned verification toolchain, and
  record compiler, language, runtime, dependency lockfile, and source SHA in
  generated artifacts.
- [ ] Add `cargo-audit`/RustSec and `cargo-deny` policy for advisories,
  licenses, duplicate critical crates, and allowed registries/git sources.
- [ ] Generate an SBOM, sign release artifacts and provenance, and verify
  reproducible builds.
- [ ] Make code generation transactional and concurrency-safe. A failed or
  concurrent invocation must never leave a mixed/stale output directory;
  obsolete `src/main.rs` and graph files must be removed deliberately.
- [ ] Version the generated-code ABI and residual-risk schema. Add golden
  compatibility fixtures and migrations before freezing 1.0.
- [ ] Provide operational runbooks for panic, stuck transform, overload,
  partial shutdown, dependency outage, rollback, reconciliation, and
  compiler upgrade.
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
