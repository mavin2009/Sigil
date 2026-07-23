# Fuzzing sigilc

Two properties, both of which found real defects.

## 1. The compiler never panics

```
./run.sh 1 200
```

Generates programs in four modes — `valid`, `mutated` (byte-level
corruption), `nested` (deep bracket nesting), `truncated` — and asserts the
compiler always exits with a diagnostic rather than a panic or abort.

**Found:** deeply nested expressions overflowed the parser stack and aborted
the process (SIGABRT) at nesting depth ~200. A crash is a denial of service,
not a diagnostic. Fixed by bounding nesting depth *before* the source reaches
the parser, since pest's generated code recurses during parsing itself. Real
programs in this repo nest 4 deep; the limit is 64.

## 2. Accepted ⇒ compiles

```
./metamorphic.sh 1 40
```

For every generated program the compiler **accepts**, the emitted Rust crate
must compile. This is the core promise, and it was being violated.

**Found (5 of the first 8 accepted programs):**

- arithmetic and ordering were permitted on non-numeric operands, so
  `true * true` and `19.7 * "s"` were accepted and produced Rust that `rustc`
  rejected;
- `@recover(with: f)` did not check that `f`'s signature matched the stage it
  recovers, so the mismatch surfaced as a type error in the generated crate
  instead of in the source.

Both are now Level-1 checks with negative proofs.

## 3. Proven ⇒ never violated at runtime

```
./prover_soundness.sh 1 40
```

The strongest property, and the one that closes the loop. A proof is a claim
about *every* execution; a demo is *one* execution. Generated demos now
**assert the invariants the compiler proved** — per shard for same-process
holds, on aggregates for cross-process ones — so every run, including every
chaos run, tests the prover.

`prover_gen.py` generates programs shaped to stress exactly where the provers
reason: conditional counters, guarded and unguarded sends, guards over
immutable bindings *and* over mutated state, clamping, fan-out. Any program
the compiler proves is then built and executed under fault injection.

**Validation of the harness itself:** the guard-correlation unsoundness
(`examples/proofs/guard_mutated_state.sigil`) reproduces under it exactly:

```
PROVEN INVARIANT VIOLATED: Down.got <= Up.cnt (40 vs 32)
```

That defect passed 80 unit tests. It does not pass this.

**Found while building it:** assertions were only emitted into the
multi-process demo, so single-process programs — including
`examples/level3/proven_ledger.sigil` — were running with no runtime
checking at all. Both demo shapes now assert.

Campaigns to date: 33 proven programs built and executed under 20% fault
injection, 0 violations.

## Note on scope

These are randomized, not coverage-guided, and deterministic given a seed, so
any failure reproduces exactly.

What they do **not** establish: absence of unsoundness. Property 3 samples
executions of programs the prover accepted; it can only ever refute a proof,
never confirm one. The prover's reasoning — interval arithmetic, the
per-handler delta argument, the counting argument over the topology — has not
been mechanically verified, and its soundness rests on the arguments written
in `docs/ASSURANCE.md` and on review. Every defect found so far was found by
reading the code or by these harnesses, which is evidence that both are worth
continuing, not that the search is finished.
