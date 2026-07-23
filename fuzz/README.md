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

## Note on scope

This is randomized, not coverage-guided. It exercises the parser, the
checkers, and codegen shape; it does not explore the provers' state space
systematically. Both harnesses are deterministic given a seed, so any failure
reproduces exactly.
