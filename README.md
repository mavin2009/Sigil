# Sigil

**A small language for high-assurance concurrent components.** You write the
pipeline; the compiler proves the properties, generates lock-free Rust, and
tells you exactly what it could not prove.

```
process Audit {
  state recorded: Int = 0

  on request: Request {
    let logged = request ~> write_audit @timeout(60.ms) @retry(2) @recover(with: deny_unaudited)
    recorded := recorded + 1
    send logged to Vault @deadline(5.ms)
  }
}

spec ZeroTrust {
  hold Vault.served <= Audit.recorded      // no secret served without an audit record
  require path_latency <= 700.ms           // end-to-end, including queue hand-off
}
```

Those two spec lines are checked, not aspirational. Move
`recorded := recorded + 1` below the `send` — the natural refactor, the one
that passes code review — and the build fails:

```
error[Level 4 (system)]: ORDERING fails — the `request` handler of `Audit`
sends toward `Vault` BEFORE updating `recorded`; a message could arrive
uncounted. Move the update above the send.
```

---

## Seriously… another language?

Yes. I'm as sorry about it as you are. Here's the honest case; you can decide
whether it clears the bar.

**"Just write careful Rust."** You can. Rust already gives you memory safety
and `Send`/`Sync`. What it does not give you is a compiler that knows your
audit write must happen *before* your secret release, or that your risk check
must cover cancels and not just new orders. Those are properties of your
*system*, not of your memory. Today they live in a design doc, a code review,
and someone's memory of the last incident.

**"So it's a framework."** A framework can hand you actors and channels. It
cannot reject your program because a counter moved three lines down. These
checks need the whole statement order, the whole message graph, and every
failure path visible at once — that's a compiler's job, and this one is
~5,500 lines of Rust (plus its own test suite), not a moonshot.

**"Another DSL, another syntax to learn."** The surface is deliberately tiny:
schemas, transforms, processes, four effect tags, one `send` statement. No
generics, no traits, no lifetimes, no macros, no package manager, no build
system. You can read [the entire language reference](docs/LANGUAGE.md) in
about fifteen minutes, which is roughly how long `cargo build` takes on a bad
day.

**"What about my ecosystem?"** Fine, untouched. Sigil compiles *to* Rust — a
normal crate with a normal `Cargo.toml` that you call from normal code. The
external transforms are stubs you fill in with your real KMS, your real policy
engine, your real ledger. Sigil owns the concurrency, failure, and proof
structure. It owns none of your business logic.

**"Does it catch anything, or is it a compile-time personality test?"**
Building the two flagship examples found five real bugs *in the compiler
itself* — the kind that only surface when you write real programs against it.
Every one now has a test. And the claim is more specific than "safer": under
20% fault injection, with 1,757 injected faults, the security pipeline held
`served ≤ recorded ≤ granted ≤ verified` exactly — zero messages lost, no
locks anywhere in the generated code.

**"This will just make everything slower and more annoying."** Sometimes yes,
and it should. The first draft of the clearing example declared a 400 ms SLO
over a pipeline that needed 520 ms. The compiler said so. That conversation
happened at build time instead of at 3 a.m., which is the whole pitch in one
sentence.

**Things Sigil is not:** general-purpose, a web framework, pleasant for
prototyping, finished, or a good idea for your CRUD service. It's for the
component in the middle of your system where being wrong is expensive and "we
reviewed it carefully" is not a control.

**What it will never claim:** that your component cannot fail. Every build
emits `RESIDUAL_RISK.md` naming what it assumed rather than proved — your
external I/O, the OS, the scheduler. A `--level 0` build states plainly that
it established nothing at all. If you want a language that promises zero
residual risk, several exist, and they are all lying.

---

## Quick start

```
cargo run -p sigilc -- examples/security/vault.sigil generated/vault --emit-main --level 4
cd generated/vault && cargo run --bin demo

# now break it on purpose
SIGIL_CHAOS_FAIL_PCT=20 SIGIL_CHAOS_LATENCY_MS=120 cargo run --bin demo
```

```
[Authn]  verified = 1920   [Audit]  recorded = 1920
[Authz]  granted  = 1920   [Vault]  served   = 1920
chaos: 10240 external calls, 1757 injected faults, 2560 retries,
       632 recover paths taken
```

## Documentation

| Doc | What's in it |
| --- | ------------ |
| **[Why Sigil](docs/WHY_SIGIL.md)** | one component walked from 51 lines of Sigil to 479 lines of generated Rust, and the seven decisions per handler the compiler checks so a reviewer doesn't have to |
| **[Language Reference](docs/LANGUAGE.md)** | complete surface syntax and semantics |
| **[Assurance Levels](docs/ASSURANCE.md)** | what each level proves, the proof obligations, and all 25 must-fail programs |
| **[Runtime & Generated Code](docs/RUNTIME.md)** | the actor model, topology wiring, `sigil_rt`, fault injection, tuning |

## Examples

| Directory | Focus |
| --------- | ----- |
| [`examples/security/`](examples/security/) | **zero-trust secrets vault** — audit-before-serve proven; fail-closed by construction |
| [`examples/finance/`](examples/finance/) | **clearing & settlement** — 5 proofs, 380 ms budget, an `f64` accumulator with no `Arc<Mutex<>>` |
| [`examples/trading/`](examples/trading/) | **exchange order gateway** — multi-handler; cancels are risk-checked, provably |
| [`examples/level4/`](examples/level4/) | system conservation across a topology |
| [`examples/level3/`](examples/level3/) | inductive invariants with runtime-guarded assumptions |
| [`examples/concurrent/`](examples/concurrent/) | lock-free actor fleets, routing policies, chaos runs |
| [`examples/proofs/`](examples/proofs/) | 25 programs that **must fail to compile** |

## What it rules out

By construction, at the default level: data races, shared mutable state,
null, cross-process state writes, cyclic actor graphs, untagged failure
paths, `@timeout` without recovery, `@retry` without a terminal failure path,
fallible recovery paths, `Float` shard keys, and sends to a type the target
cannot receive.

By proof, when you ask for it: inductive state invariants, relational
invariants, cross-process conservation, and latency budgets that include
queue hand-off.

Full detail in [ASSURANCE.md](docs/ASSURANCE.md).

## Project layout

```
sigilc/src/
  frontend/       AST + pest grammar + parser
  analysis/       Graph IR, Level 1-4 checks and provers, topology, residual risk
  backend/        Rust codegen
  diagnostics.rs  byte spans → line:col with caret snippets
sigil_rt/         runtime: errors, actor stats, router, back-pressure, chaos
examples/         runnable programs, including 25 negative proofs
docs/             language reference, assurance levels, runtime, rationale
```

## Testing

```
cargo test
```

75 tests: parser, per-level checks, both provers, topology and routing,
codegen shape, and end-to-end compilation of every example. Every rule has a
program in `examples/proofs/` asserted to fail *for the right reason*.

## Status

**v0.7** — Five assurance levels; shared-nothing actor codegen (lock-free by
construction, enforced by test); compiler-wired multi-process topologies with
hash / round-robin / broadcast routing; multi-handler processes with
type-directed dispatch; total failure-path coverage via
`@retry`/`@recover`/`@error`; declared back-pressure with provable
end-to-end latency; inductive and system-level provers with runtime-enforced
assumptions; fault injection with exact message accounting.

Not production-ready. It is a working compiler with real proofs and honest
limits, looking for the two or three components in your system that deserve
this treatment.
