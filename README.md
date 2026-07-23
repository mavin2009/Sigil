# Sigil

A language for systems that are not allowed to die.

Sigil treats programs as living causal graphs. Data, effects, time, and failure are first-class citizens. The compiler explores possible behaviors under concurrency, latency, and faults, then certifies which classes of bugs have been made extinct.

## Extinct by Design (Level 1)

These failure modes are impossible to express at the default safety level:

- Data races and any form of shared mutable state
- Null or undefined values
- Unhandled timeouts or effects
- Silent mutation of state across time
- Forgotten or untyped failure paths

Higher levels add stronger temporal and functional guarantees. Residual risk that remains outside the system boundary is always reported explicitly.

## Quick Start

```bash
cargo run -p sigilc -- examples/ingest.sigil
```

The compiler emits ownership-safe Rust that preserves the Level-1 guarantees, together with a residual risk report.

## Core Ideas

- **Schemas** are sacred shapes. Every value that flows has a known structure.
- **Processes** hold isolated, versioned state and react to messages.
- **Pipelines** (`~>`) compose transformations. Effect tags (`@timeout`, `@recover`, `@error`) make intent and failure modes visible.
- **Failure is algebraic.** Recovery paths are part of the graph, not an afterthought.
- **Residual Risk** is a first-class build artifact.

## Project Layout

- `sigilc` — the compiler (pest-driven parser, Graph IR, Level-1 checks, codegen)
- `sigil_rt` — minimal runtime helpers for generated code
- `examples` — living examples of the language

## Status

v0.1.1 — Real pest-driven frontend with structured AST (pipelines, tags, field access), Graph IR that inspects effect tags, Level-1 checker that enforces timeout/recover pairing, unit tests for parser and checks, dynamic residual risk report. The foundation is now general enough for the core subset and fully tested for the primary example.
