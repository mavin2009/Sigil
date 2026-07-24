# Assurance Levels

Sigil is stratified. You choose how much you are claiming, and the compiler
holds you to exactly that — no more, no less.

```
sigilc <file.sigil> [out_dir] [--emit-main] [--level 0|1|2|3|4]
```

| Level | Name | What it establishes |
| ----- | ---- | ------------------- |
| 0 | `sketch` | nothing — parses and lowers, every skipped guarantee is listed |
| 1 | `safe` **(default)** | extinct-by-design checks, signatures, failure paths, topology |
| 2 | `contracts` | spec obligations: latency budgets, spec bookkeeping |
| 3 | `proofs` | `hold` invariants proven inductively, assumptions runtime-enforced |
| 4 | `system` | cross-process invariants proven structurally over the topology |

Each level includes every level below it. **A build never claims a property
it did not establish**: at Level 0 the residual report leads with
"Guarantees NOT established by this build", and at every level the skipped
obligations are named.

---

## Level 1 — Safe (default)

These are rejected or unrepresentable:

**Concurrency**
- data races and shared mutable state (state is task-local by construction)
- cross-process state writes
- cyclic process graphs (bounded channels + cycles can deadlock)
- self-sends

**Failure handling**
- external stages with no declared failure path
- `@timeout` without `@recover`/`@error` **on the same step**
- `@retry` without a terminal failure path
- duplicate effect tags, or `@recover` together with `@error`

**Types and structure**
- null or undefined values (not representable)
- pipeline stages disagreeing with declared transform signatures
- sends to undeclared processes, type-mismatched edges, or a type the target
  cannot receive
- `Float` shard-routing keys
- bare calls to external transforms
- external calls inside pure transform bodies
- duplicate handler message names or types within a process

Residual risk — external transforms, the OS, the scheduler — is always
reported, never hidden.

## Level 2 — Contracts

Adds spec obligations on a Level-1-legal graph.

| Clause | Meaning |
| ------ | ------- |
| `require path_timeout_sum <= N.ms` | worst-case **processing** time on the longest path |
| `require path_latency <= N.ms` | processing **plus queue hand-off**; rejects `@block` on the path |

Budget arithmetic:

- each stage is charged `(1 + retries) × timeout`
- a process contributes the **max over its handlers** (one message, one
  handler), not the sum
- the path total is the **longest path** through the topology; parallel
  branches take the max
- `@deadline(N.ms)` sends add N; `@shed` adds 0; `@block` is unbounded
- `path_latency` is an in-process theorem and fails closed across an explicit
  placement boundary; network queueing, retry, acknowledgement, and partition
  time are not represented by a local send deadline

Holds are residual at this level; Level 3 proves them.

## Level 3 — Proofs

A built-in inductive prover over an exact `i64` interval domain — no external
SMT dependency. For each `hold`:

- **BASE** the declared init satisfies the predicate
- **INDUCTIVE** assuming every hold, each reachable update re-establishes it

Also proves **relational holds** within a process (`hold refunded <=
charged`) by a per-handler delta argument. That is sound at handler
boundaries precisely because actors are shared-nothing: no interleaving is
observable mid-handler.

`Float` is executable but deliberately outside Levels 3–4. IEEE-754
rounding, signed zero, NaN, and infinity require a different abstract domain;
until one exists, a Float hold is a compile error rather than a guessed
theorem. Proven monetary quantities use integer minor units.

**Assumptions are never taken on faith.** A `require msg.field <cmp> lit`
compiles into a guard at handler entry; out-of-contract messages are
rejected as typed errors and counted. The proof is therefore unconditional
over what the process actually executes.

Values flowing through external transforms are unbounded by construction,
and failures say so. When the cause is a missing input contract the error
also names it (`unbounded because: input \`payment.amount\` is unguarded`):

```
Level-3 violation in spec 'Broken': INDUCTIVE STEP fails — in process
'Refunds', update `total := total - payment.amount` yields
[-9223372036854775807, 9223372036854775807]
which can escape `total >= 0`
  fix: constrain the inputs with `require <msg>.<field> >= 0` in the spec,
  or restructure the update
```

Level 3 additionally **rejects fallible recovery paths**: a `@recover`
target that is an external transform can itself fail or hang, reintroducing
the loss it exists to prevent. (Reported as residual at Levels 1–2.)

## Level 4 — System

Cross-process invariants like `hold Settlement.settled <= Ingest.admitted`,
proven from five structural obligations:

| Obligation | Meaning |
| ---------- | ------- |
| **BASE** | `init(lo) <= init(hi)` |
| **COUNTING** | per-message deltas are in the additive fragment; no handler may decrease the bounding counter |
| **ORDERING** | every handler that forwards updates the counter **before** the send that can reach downstream |
| **FLOW** | every path into the downstream process passes through the upstream one |
| **MULT** | send multiplicity along the path is a static constant (broadcast is a runtime shard count — rejected) |
| **GAP** | `mult × max(downstream delta) <= min(upstream delta)`, per handler |

The proof requires **no fairness or liveness assumptions.** Every failure
mode the language admits — timeouts, `@error` drops, guard rejections, shed
sends, staged shutdown — only *decreases* the downstream count, so the
inequality survives all of them. That is the payoff of mandatory failure
paths: they make the system-level argument monotone.

Level 4 fails closed when the topology contains a cross-placement edge.
Remote at-least-once delivery can duplicate, at-most-once can lose, and
network acknowledgement/deduplication are not currently inputs to the
structural prover. Process-local Level-3 invariants and Level-1 type/acyclic
checks still apply.

---

## Negative proofs

Every rule above has at least one program in `examples/proofs/` that **must
fail to compile**. The integration tests assert each is rejected, and for the
right reason.

| Program | Rejects |
| ------- | ------- |
| `unhandled_timeout.sigil` | `@timeout` with no recovery |
| `timeout_without_step_recover.sigil` | recovery on a different step |
| `unrecovered_external.sigil` | external stage with no failure path |
| `retry_without_recover.sigil` | `@retry` with no terminal failure path |
| `conflicting_tags.sigil` | `@recover` and `@error` on one step |
| `acknowledged_timeout.sigil` | *(must PASS)* `@timeout @retry @error` is legal |
| `bare_external_call.sigil` | external transform called outside a pipeline |
| `impure_pure_transform.sigil` | pure transform calling an external one |
| `nonlocal_state.sigil` | write to an undeclared state slot |
| `cross_process_state.sigil` | one process writing another's state |
| `type_mismatch.sigil` | pipeline vs declared signature |
| `float_route_key.sigil` | `Float` shard-routing key |
| `mh_duplicate_msg_name.sigil` | two handlers sharing a message name |
| `mh_duplicate_msg_type.sigil` | two handlers sharing a message type |
| `mh_no_handler_for_type.sigil` | sending a type the target cannot receive |
| `mh_uncounted_handler.sigil` | a handler forwarding without counting |
| `timeout_sum_exceeded.sigil` | latency budget overrun |
| `retry_budget_overflow.sigil` | `(1 + retries) × timeout` over budget |
| `latency_unbounded_block.sigil` | `path_latency` claimed with a `@block` send |
| `latency_budget_overflow.sigil` | hand-off deadline pushes the path over budget |
| `hold_bad_init.sigil` | init violates its own invariant |
| `hold_not_inductive.sigil` | update can escape the invariant |
| `one_sided_clamp.sigil` | clamping only the upper bound |
| `guard_mutated_state.sigil` | correlating a send guard with state mutated in the same handler |
| `mixed_numeric_types.sigil` | `Int` and `Float` mixed in arithmetic |
| `system_ordering.sigil` | counting after forwarding |
| `system_leak.sigil` | a second entry bypassing the counter |
| `system_broadcast.sigil` | broadcast on a counted path |

Run one:

```
cargo run -p sigilc -- examples/proofs/system_ordering.sigil /tmp/nope --level 4
```

```
error[Level 4 (system)]: Level-4 violation in spec 'Broken': ORDERING fails
— the `order` handler of `Gateway` sends toward `Settlement` BEFORE
updating `admitted`; a message could arrive uncounted. Move the update
above the send.
```

## The residual-risk report

Every successful build writes `RESIDUAL_RISK.md` next to the generated
crate, containing:

- the assurance level, and everything **not** established at it
- proven invariants, and which assumptions are runtime-enforced
- the verified topology, with message types per edge
- back-pressure policies per send, and the generated channel-cycle argument
- external (residual) transforms — the real I/O the proofs do not cover

This is the honest core of the design. Sigil does not claim your component
cannot fail. It claims that specific failure classes are unrepresentable,
that specific properties are proven, and that **everything else is named**.
