# Soundness Argument for Levels 3 and 4

This document states the argument the implementation is intended to realize.
It is review material, not a claim that `sigilc` has been mechanically
verified. The trusted computing base is the parser/type checker, the isolated
analysis modules, Rust's checked generated code, `sigil_rt`, Tokio, and rustc.

The proof core is independent of the CLI and code generator:

- `analysis/level3.rs` implements scalar induction and same-process
  relational deltas;
- `analysis/level4.rs` implements cross-process ordering/counting;
- `analysis/topology.rs` resolves typed edges and acyclicity;
- `analysis/typecheck.rs` establishes the expression, name, and effect
  premises consumed by both provers.

These modules consume the AST/topology and return reports or typed errors.
They do not inspect generated Rust and do not depend on CLI state.

## Semantic scope

A proved execution is an in-process sequence of successfully completed actor
statement boundaries. Each actor handles one message at a time. State is
task-local. Bounded channels may delay or reject delivery according to the
declared policy. Ordinary transform errors, timeouts, input-guard rejection,
and declared shedding are part of this model.

`Int` is signed 64-bit integer arithmetic. The proof domain uses exact
`i128` interval endpoints, but every arithmetic result is intersected with
the successful `i64` range. Generated code enables overflow checks in every
profile, so an operation outside that range panics before a wrapped value can
be installed. Division by zero and `MIN / -1` likewise do not produce a
successful state transition. `Float` is executable but receives no Level 3
or 4 theorem.

Actor panic, process exit, host loss, memory corruption, compiler
miscompilation, and incorrect foreign-effect declarations are outside a
continued proved execution. Their precise fail-stop treatment is described
under [panic cut-points](#panic-cut-points).

## Level 3: scalar induction

For a scalar clause `hold x R c`, where `R` is a supported comparison:

1. The type checker establishes that `x` and `c` are `Int`.
2. The base checker evaluates the declared initializer exactly and requires
   `x₀ R c`.
3. For every handler that can write `x`, abstract interpretation starts from
   the invariant interval and the runtime-enforced input intervals.
4. Each reachable assignment is evaluated with checked interval arithmetic.
   The resulting successful `i64` interval must be a subset of `x R c`.
5. Handlers with no write preserve `x` by identity.

Induction over completed handler invocations therefore preserves the clause.
The analysis joins branches; unsupported or insufficiently precise
expressions fail closed rather than receiving a theorem.

## Level 3: relational deltas

For `hold low <= high` in one process:

1. Initial values must satisfy `low₀ <= high₀`.
2. Each handler is analyzed from one actor boundary to the next.
3. The checker derives the net successful deltas `Δlow` and `Δhigh`,
   correlated by stable branch/input guards.
4. It requires `Δlow <= Δhigh` on every reachable path. A handler that only
   changes the high side is safe; a handler that can decrease the gap is
   rejected.
5. Since actors expose no interleaving in the middle of a handler, the
   boundary relation follows from
   `low' - high' = (low - high) + (Δlow - Δhigh) <= 0`.

This theorem is explicitly a handler-boundary theorem. Panic cannot turn a
mid-handler relational state into a successful boundary.

## Guard correlation

A conditional counter and conditional send may share a guard only when every
name read by that guard is stable for the handler. The checker rejects
correlation if the handler writes any referenced state. Input fields and
immutable `let` bindings are stable. Both branches are otherwise analyzed
independently and joined.

Without this premise, a handler could change the guard between counting and
sending and make a false proof. The regression
`guard_mutated_state.sigil` is the minimal counterexample.

## Level 4: topology counting

For `hold Down.received <= Up.sent`, the counting proof establishes:

1. **Base:** declared initial states satisfy the inequality.
2. **Typed flow:** every send edge is resolved to exactly one compatible
   destination handler, and the generated graph is acyclic.
3. **All paths:** every route capable of increasing the downstream counter
   passes through the named upstream process; bypass/leak paths are rejected.
4. **Ordering:** in every sending handler, the upstream accounting write is
   before the send. A receiver can therefore never observe a delivered
   message whose source increment has not happened.
5. **Multiplicity:** the maximum sends enabled by a handler, including
   conditional and broadcast multiplicity, is no greater than the proven
   source increment.
6. **Destination delta:** every destination handler's downstream increment
   is bounded by the multiplicity of the accepted input.
7. **Handler coverage:** every handler that can receive or forward the
   counted type is included; adding an uncounted handler invalidates the
   proof.

Induction over send acceptance and handler completion gives the system
inequality. The argument is safety-only and assumes neither fairness nor
eventual delivery.

## Shedding and queue policies

`@shed` and an expired `@deadline` remove an attempted downstream delivery;
they cannot increase a downstream counter. `@block` has no policy shedding,
but may wait without bound. A closed destination returns `ActorStopped`.
Thus all policies preserve upper-bound counting theorems, while only bounded
policies can support `path_latency`.

This says nothing about process durability. A message accepted into Tokio's
in-memory queue may be lost when the process or host stops.

## Failure paths and external effects

Every fallible external stage must terminate in explicit `@recover` or
`@error`; every timeout is local to its stage. Retry arithmetic is checked,
and a retried bound transform must declare `idempotent`. Async bindings must
declare `cancel_safe`. Blocking bindings must declare
`completion_tracked`; `tracked_spawn_blocking` keeps timed-out work visible
until its OS thread completes. Infallible bindings are restricted to
`idempotent, cancel_safe, none`.

These declarations are assumptions owned by the application, emitted in
`SIGIL_EFFECTS.json` and `RESIDUAL_RISK.json`. The compiler verifies their
use, not the foreign implementation's truthfulness.

## Panic cut-points

Generated code provides deterministic injection immediately before and after
state writes, sends, retries, and foreign calls. The runtime policy is
fail-stop: the actor is not restarted, its state is not returned, and the
supervisor emits `Panicked` with incomplete accounting.

| Cut-point | Preserved statement |
| --- | --- |
| before state write | all earlier successful boundaries |
| after state write | every scalar invariant for that assignment; relational holds are not certified until handler completion |
| before send | Level 4's required source increment already exists |
| after send | Level 4 ordering/multiplicity still bounds any accepted delivery |
| before/after retry | Sigil state and sends are unchanged by the retry boundary; the foreign contract remains residual |
| before foreign call | the current foreign operation has not begun |
| after foreign call | the effect may have committed; automatic replay is unsafe |

Once a panic report exists, Sigil makes no claim that the component remains
available or completely accounted. Already-established Level 4 safety
prefixes are not undone, but no operational owner may describe the run as
successfully proved after that point. Reconciliation must use an external
durable source. Automatic replay is forbidden because a send or foreign
write may already have completed.

## Premise-to-checker and adversarial evidence

| Premise | Enforcer | Negative evidence |
| --- | --- | --- |
| exact supported numeric types | `check_types`, Level 3 interval evaluator | `mixed_numeric_types.sigil`; `float_holds_fail_closed_until_ieee_semantics_exist` |
| scalar base | Level 3 base case | `hold_bad_init.sigil`; `bad_init_fails_base_case` |
| scalar preservation | Level 3 abstract step | `hold_not_inductive.sigil`; `subtraction_escapes_and_fails` |
| checked integer range | Level 3 checked interval operations and generated overflow checks | `checked_i64_overflow_cannot_install_a_wrapped_post_state` |
| relational base/delta | Level 3 relational checker | `relational_hold_fails_bad_init`; `one_sided_clamp.sigil` |
| stable guard correlation | Level 3/4 read/write-set check | `guard_mutated_state.sigil` |
| typed, acyclic flow | topology derivation | `type_mismatch.sigil`; topology cycle/unknown-target unit tests |
| all paths through source | Level 4 flow check | `system_leak.sigil` |
| update before send | Level 4 ordering check | `system_ordering.sigil` |
| bounded multiplicity | Level 4 send counting | `system_broadcast.sigil`; `level4_system_invariants` gap mutation |
| all handlers counted | Level 4 handler coverage | `mh_uncounted_handler.sigil` |
| declared shedding semantics | codegen + runtime back-pressure helpers | `routing_policies`; deterministic full-queue runtime test |
| explicit failure terminal | failure-path checker | `unhandled_timeout.sigil`; `retry_without_recover.sigil` |
| timeout/retry budget arithmetic | checked Level 2 arithmetic | `timeout_sum_exceeded.sigil`; `retry_budget_overflow.sigil`; `latency_budget_overflow.sigil` |
| effect cancellation/idempotency | effect-contract checker | `validates_bound_effect_and_retry_contracts`; tracked blocking cancellation test |
| fail-stop panic accounting | generated hooks + `Supervisor` | live panic/incomplete-accounting and shutdown-deadline tests |
| no handoff with live state mutation | packed `ShardLease` phase/permit atomic | Loom ownership admission/drain model; deterministic in-flight handoff rejection |
| distributed theorem boundary | Level 2/4 remote-placement rejection | remote latency and conservation fail-closed tests |

The extended manually dispatched mutation job mutates `level3.rs`,
`level4.rs`, and `topology.rs`; a surviving mutant fails the evidence gate.
Coverage-guided fuzz targets independently exercise parser, checker,
topology, both provers, and codegen. The executable reference semantics
differentially checks state and send traces against generated Rust.

## Review obligations

Before 1.0, an independent proof-core reviewer must challenge this document,
the implementation, and the negative tests. A passing suite is evidence that
known premises are enforced; it is not a substitute for that review.
