# Sigil

A language that makes whole classes of production failures extinct by construction.

Programs are causal graphs: data, effects, time, and failure are first-class. The compiler checks explicit recovery paths, local state discipline, transform signatures, and effect tags, then reports residual risk outside the model.

## Extinct by Design (Level 1)

These failure modes are rejected or unrepresentable at the default safety level:

- Data races and shared mutable state
- Null or undefined values
- `@timeout` without a matching `@recover`
- External stages with no declared failure path (must carry `@recover` or an explicit `@error` acknowledgment)
- `@retry` without a terminal failure path (retries delay failure, they do not handle it)
- State writes to non-local slots
- Pipeline stages whose types disagree with declared transform signatures
- `send` to undeclared processes, type-mismatched topology edges, cyclic process graphs, and Float shard-routing keys
- Duplicate handler message names or types within a process, and sends to a type the target cannot receive
- Cross-process state writes (Graph IR is per-process; locality is checked against the owning process only)
- Bare calls to external transforms (external stages must be pipeline steps with a failure path)
- External calls inside pure transform bodies (pure transforms are the infallibility anchor)
- Duplicate effect tags on a step, and `@recover` combined with `@error` (a step recovers or acknowledges the drop, not both)

Residual risk outside the model (external transforms, OS, scheduler) is always reported explicitly.

## Assurance Levels

| Flag | Level | What runs |
| ---- | ----- | --------- |
| `--level 0` / `sketch` | exploratory | parse + lower only; every skipped guarantee is listed in the residual report |
| `--level 1` / `safe` (default) | extinct-by-design | Level-1 checks, transform signatures, failure paths, topology |
| `--level 2` / `contracts` | spec obligations | `require` / `hold` / `extinct` on a Level-1-legal graph |
| `--level 3` / `proofs` | inductive proofs | every `hold` proven (base + inductive step over all reachable updates); undischargeable holds fail the build |
| `--level 4` / `system` | system proofs | cross-process invariants (`hold Settlement.posted <= Gateway.admitted`) proven structurally over the topology |

A Level-0 build never claims unverified properties: the residual report leads
with everything that was NOT established.

## Level 3: Proven Invariants with Enforced Assumptions

`hold` clauses are proven by a built-in inductive prover over an interval
domain — no external SMT dependency:

```
spec Proven {
  require payment.amount >= 0.0   // input assumption
  require payment.units >= 0
  hold posted >= 0                // PROVEN: base case + every reachable update
  hold total_amount >= 0.0
}
```

The crucial honesty property: **proof assumptions are never taken on faith.**
Every `require msg.field <cmp> literal` compiles into a guard at handler
entry; out-of-contract messages are rejected as typed errors and counted in
the actor's dropped total. The proof is therefore unconditional over what the
process actually executes.

Failures are actionable, not oracular:

```
Level-3 violation in spec 'Broken': INDUCTIVE STEP fails — in process
'Refunds', update `total := total - payment.amount` yields [-inf, inf] which
can escape `total >= 0`
  unbounded because: input `payment.amount` is unguarded ...
  fix: constrain the inputs with `require <msg>.<field> >= 0` in the spec,
  or restructure the update
```

Values that flow through external transforms are never assumed bounded —
they are unbounded by construction, and the prover says so. See
`examples/level3/proven_ledger.sigil` (passes) and
`examples/proofs/hold_not_inductive.sigil` (must fail).

Level 3 also proves **relational holds** within a process (`hold refunded <=
charged`) by a per-handler delta argument — sound at handler boundaries
precisely because actors are shared-nothing: no interleaving is observable
mid-handler. Simple `let` bindings carry their guarded intervals.

## Level 4: System Invariants over the Topology

`hold Settlement.posted <= Gateway.admitted` is proven from four structural
obligations, each with a negative proof program:

| Obligation | Meaning | Must-fail proof |
| ---------- | ------- | --------------- |
| BASE | `init(lo) <= init(hi)` | — |
| ORDERING | the upstream counter updates **before** the send, so a message can never reach downstream uncounted, even mid-shutdown | `system_ordering.sigil` |
| FLOW | every path into the downstream process passes through the upstream one; a second entry breaks the count | `system_leak.sigil` |
| MULT | send multiplicity along the path is a static constant; broadcast multiplies by the runtime shard count and is rejected | `system_broadcast.sigil` |
| GAP | `mult × max(downstream delta) <= min(upstream delta)` | (tested: `+2` counter) |

The proof needs **no fairness or liveness assumptions**: every failure mode
the language admits — timeouts, `@error` drops, guard rejections, staged
shutdown — only decreases the downstream count. This is the payoff of the
actor model plus mandatory failure paths. See
`examples/level4/conservation.sigil`; under 15% fault injection the runtime
witness holds with `posted = scored = admitted` exactly.

## Fault Injection (proving the failure paths)

Generated external stubs route through `sigil_rt::chaos`. Configure via env
and watch the verified `@timeout` / `@retry` / `@recover` machinery fire under
concurrent load while message accounting stays exact:

```
SIGIL_CHAOS_FAIL_PCT=15 SIGIL_CHAOS_LATENCY_MS=120 cargo run --bin demo
```

Measured on `examples/concurrent/orderflow` (3,200 orders, 15% faults):
recoveries fell from ~1,700 (no retries) to 510 with `@retry(2)` — matching
the binomial prediction (0.15³ per retried stage) — with zero messages lost
at any stage and exact float aggregates throughout.

## Quick Start

```bash
cargo run -p sigilc -- examples/pipeline/pipeline.sigil generated/pipeline
```

Emits a Rust crate (`Cargo.toml`, `src/lib.rs`) that depends on `sigil_rt`, plus `RESIDUAL_RISK.md`.

## Project Layout

```
sigilc/
  src/
    frontend/     # AST + pest grammar + parser
    analysis/     # Graph IR, Level-1 checks, types, residual risk
    backend/      # Rust codegen
sigil_rt/         # runtime Result / SigilError for generated code
examples/
  ingest/         # multi-stage telemetry
  counter/        # pure compiled transform body
  resilient/      # pure normalize + timed residual enrich
  circuit/        # circuit-breaker style recovery
  pipeline/       # order flow with dual timeouts + signatures
```

## Examples

Each example lives in its own subdirectory with a `.sigil` program and a short factual README.

| Directory | Focus |
|-----------|--------|
| `examples/ingest/` | validate → timed decompress → extract → store |
| `examples/counter/` | pure `transform add(x: Int) -> Int { x + 1 }` |
| `examples/resilient/` | compiled `normalize` + residual `enrich` / `store` |
| `examples/circuit/` | timeout + recover + local status state |
| `examples/pipeline/` | pure `add_fee` + dual timed residual stages, `Order → Receipt` |
| `examples/runnable/counter/` | demo-oriented counter for printed state output |
| `examples/proofs/` | **negative** programs that must fail Level-1 (bug prevention proofs) |

### Runnable demo

```bash
cargo run -p sigilc -- examples/runnable/counter/counter.sigil generated/runnable_counter --emit-main
```

Generated crate includes `src/main.rs` that constructs the process, runs one handler, and prints local state.

### Proofs (bug prevention)

```bash
# Must fail — unhandled @timeout
cargo run -p sigilc -- examples/proofs/unhandled_timeout.sigil /tmp/nope

# Must fail — Order fed to transform expecting Receipt
cargo run -p sigilc -- examples/proofs/type_mismatch.sigil /tmp/nope

# Must fail — external stage with no @recover / @error
cargo run -p sigilc -- examples/proofs/unrecovered_external.sigil /tmp/nope

# Must fail — @retry with no terminal failure path
cargo run -p sigilc -- examples/proofs/retry_without_recover.sigil /tmp/nope

# Must fail — Float shard-routing key
cargo run -p sigilc -- examples/proofs/float_route_key.sigil /tmp/nope

# Must fail Level-2 — (1 + 2 retries) × 200ms = 600ms > 500ms SLO
cargo run -p sigilc -- examples/proofs/retry_budget_overflow.sigil /tmp/nope --level 2

# Must fail — cross-process state write / bare external call / impure "pure"
# transform / conflicting @recover+@error
cargo run -p sigilc -- examples/proofs/cross_process_state.sigil /tmp/nope
cargo run -p sigilc -- examples/proofs/bare_external_call.sigil /tmp/nope
cargo run -p sigilc -- examples/proofs/impure_pure_transform.sigil /tmp/nope
cargo run -p sigilc -- examples/proofs/conflicting_tags.sigil /tmp/nope

# Must PASS — @timeout @retry(1) @error is a legal acknowledged timed drop
cargo run -p sigilc -- examples/proofs/acknowledged_timeout.sigil /tmp/ok --level 2
```

Integration tests assert both programs are rejected with Level-1 / signature diagnostics.

## Level 2

Temporal / path obligations on a Level-1-legal graph:

| Check | Meaning |
|-------|---------|
| Per-step recovery (AST) | Every `@timeout` has `@recover` on the **same** pipeline step |
| Timeout→Recover (IR) | Every Timeout node in the Graph IR has a Recover successor |
| `require path_timeout_sum <= N.ms` | Worst-case **longest path through the process topology** — `(1 + retries) × timeout` per stage, per-process sums, parallel branches take max — must not exceed N |
| `hold state >= N` | Discharged for pure Int/Float state when init satisfies; residual if externals feed state |
| `extinct [...]` | Assumptions listed in residual risk |

Specs:

```
spec_def     = "spec" ~ ident ~ "{" ~ spec_item* ~ "}"
spec_item    = extinct_clause | require_clause | hold_clause
extinct_clause = "extinct" ~ "[" ~ ident ~ ("," ~ ident)* ~ "]"
require_clause = "require" ~ expr
hold_clause    = "hold" ~ expr
```

See `examples/level2/` for a combined SLO + hold program, and `examples/proofs/` for negative cases.

```sigil
spec OrderSlo {
  require path_timeout_sum <= 500.ms
  extinct [null]
  hold total_charged >= 0.0
}
```

## Compiler Pipeline

```
parse → lower → level1_check → check_transform_signatures → check_failure_paths → derive_topology → level2_check
      → residual_risk_report → emit
```

## Language Grammar

Complete surface grammar (matches `sigilc/src/frontend/sigil.pest`).

### Lexical

```
WHITESPACE   = space | tab | CR | LF
COMMENT      = "//" ~ (!newline ~ ANY)*

ident        = (letter | "_") ~ (letter | digit | "_")*
duration     = digits ~ ".ms"                 // e.g. 50.ms
string       = "\"" ~ (!"\"" ~ ANY)* ~ "\""
number       = "-"? ~ digits ~ ("." ~ digits)?
boolean      = "true" | "false"
```

### Compilation unit

```
file         = (schema_def | process_def | transform_def | spec_def)*
```

### Types

```
type         = "Int" | "Float" | "String" | "Bool"
             | "UUID" | "Bytes" | "Duration"
             | ident                          // named schema
```

### Schemas

```
schema_def   = "schema" ~ ident ~ "{" ~ fields? ~ "}"
fields       = field ~ ("," ~ field)* ~ ","?
field        = ident ~ ":" ~ type
```

### Transforms

Declared signatures are authoritative for type checking and residual risk.
An empty body is an external residual stub; a non-empty pure body is compiled into the generated crate.

```
transform_def = "transform" ~ ident
              ~ "(" ~ ident ~ ":" ~ type ~ ")"
              ~ "->" ~ type
              ~ "{" ~ stmt* ~ "}"
```

Examples:

```
transform add(x: Int) -> Int {
  x + 1
}

transform confirm(o: Order) -> Receipt {}
```

### Processes

```
process_def  = "process" ~ ident ~ "{" ~ process_body ~ "}"
process_body = (state_decl | on_handler)*

state_decl   = "state" ~ ident ~ ":" ~ type ~ "=" ~ expr

on_handler   = "on" ~ ident ~ ":" ~ type ~ "{" ~ stmt* ~ "}"
```

State is process-local only. Handlers receive a typed message and run a sequence of statements.

Processes form a compiler-verified topology via `send`:

```
send ok to Risk by ok.id        // hash affinity: same key → same shard
send s to Settlement            // round-robin (default)
send done to Audit broadcast    // every shard receives a clone
```

Every process compiles to a shared-nothing actor: `spawn(self)` moves state
into an isolated task; a Clone-able typed handle is the only way in; `join()`
returns state + `{handled, dropped}` accounting after the channel drains.
Generated code contains no `Mutex`, `Arc`, atomics, or `unsafe` (enforced by
test). Send targets, edge message types, acyclicity, and shard-key hashability
(Float keys rejected) are all checked at Level 1.

### Statements

```
stmt         = let_stmt | assign_stmt | send_stmt | expr_stmt
let_stmt     = "let" ~ ident ~ "=" ~ expr
assign_stmt  = ident ~ ":=" ~ expr            // local state write
send_stmt    = "send" ~ expr ~ "to" ~ ident ~ route_clause?
route_clause = ("by" ~ expr) | "broadcast"    // default: round-robin
expr_stmt    = expr
```

### Expressions

Precedence: comparisons bind looser than `+` `-`, which bind looser than `*` `/`.

```
expr         = comparison
comparison   = sum ~ (cmp_op ~ sum)?
cmp_op       = "<=" | ">=" | "==" | "<" | ">"
sum          = product ~ (("+" | "-") ~ product)*
product      = pipeline ~ (("*" | "/") ~ pipeline)*

pipeline     = atom ~ pipe_tail*
pipe_tail    = "~>" ~ atom ~ tag*

atom         = if_expr
             | schema_lit
             | call
             | field_access
             | literal
             | ident
             | "(" ~ expr ~ ")"

field_access = ident ~ "." ~ ident
call         = ident ~ "(" ~ (expr ~ ("," ~ expr)*)? ~ ")"

schema_lit   = ident ~ "{" ~ field_init ~ ("," ~ field_init)* ~ ","? ~ "}"
field_init   = ident ~ ":" ~ expr

if_expr      = "if" ~ expr ~ "{" ~ expr ~ "}" ~ "else" ~ "{" ~ expr ~ "}"

literal      = duration | string | number | boolean
```

### Effect tags

Attached to a pipeline step after the transform atom:

```
tag          = "@timeout" ~ "(" ~ expr ~ ")"
             | "@recover" ~ "(" ~ "with" ~ ":" ~ expr ~ ")"
             | "@retry" ~ "(" ~ expr ~ ")"
             | "@error"
```

Rules:

- Every `@timeout` must have a matching `@recover` on the same program (Level-1).
- Every EXTERNAL stage (empty-bodied transform) must carry `@recover` or an explicit `@error` acknowledgment (Level-1). Pure transforms are compiled and infallible — recovery paths should be pure.
- `@recover(with: f)` names a fallback used when the step fails or times out; it is legal with or without `@timeout`.
- `@retry(n)` re-attempts the stage up to `n` extra times before the failure path; it requires `@recover` or `@error` on the same step. Level-2 charges the budget the worst case: `(1 + n) × timeout`.
- `@timeout` + `@error` (no `@recover`) is a legal *acknowledged timed drop*: the failure propagates honestly, is counted in the actor's dropped total, and satisfies both levels.
- Effect pairing is **per step, per process** — a recover elsewhere in the program never satisfies a timeout here.

Example:

```
let reserved = auth ~> reserve @timeout(120.ms) @retry(2) @recover(with: release)
```

### Specs (parsed; reserved for higher assurance levels)

```
spec_def     = "spec" ~ ident ~ "{" ~ spec_item* ~ "}"
spec_item    = extinct_clause | require_clause | hold_clause
extinct_clause = "extinct" ~ "[" ~ ident ~ ("," ~ ident)* ~ "]"
require_clause = "require" ~ expr
hold_clause    = "hold" ~ expr
```

## Concrete Example

```sigil
schema Order {
  id: String,
  sku: String,
  amount: Float
}

schema Receipt {
  id: String,
  status: String
}

transform authorize(o: Order) -> Order {}
transform reserve(o: Order) -> Order {}
transform release(o: Order) -> Order {}
transform charge(o: Order) -> Order {}
transform refund(o: Order) -> Order {}
transform confirm(o: Order) -> Receipt {}

process OrderPipeline {
  state last_order: String = "none"
  state total_charged: Float = 0.0

  on order: Order {
    let auth = order ~> authorize
    let reserved = auth ~> reserve @timeout(120.ms) @retry(2) @recover(with: release)
    let charged = reserved ~> charge @timeout(200.ms) @recover(with: refund)
    let receipt = charged ~> confirm
    last_order := receipt.id
    total_charged := total_charged + order.amount
  }
}
```

See `examples/pipeline/pipeline.sigil` and the other directories under `examples/` for full programs.

## Testing

```bash
cargo test -p sigilc
```

Coverage includes:

- **Level-1:** unhandled timeout, nonlocal state write, transform signature mismatch
- **Level-2:** path_timeout_sum overflow, hold bad init, per-step recover (when L1 process-global pairing is insufficient)
- **Positive examples:** ingest, counter, resilient, circuit, pipeline, level2, runnable/counter (full pipeline)
- **Codegen:** demo main emission, residual report sections

Negative programs live under `examples/proofs/`.

## Status

v0.6 — Multi-handler processes: type-directed dispatch, per-handler proof obligations and latency budgets, exchange-gateway example. Level 4: cross-process system invariants proven structurally over the topology (ordering / flow / multiplicity / gap obligations, each with a negative proof). Level 3 completed: relational holds via per-handler deltas, let-binding interval tracking. Level 3: built-in inductive prover for hold invariants (interval domain, no SMT dependency), runtime-guarded proof assumptions, actionable proof failures. Soundness-hardened: per-process Graph IR, per-step effect discipline, bare-call and purity rules, topology-longest-path budgets. Stratified assurance levels; shared-nothing actor codegen (lock-free by construction, enforced by test); compiler-wired multi-process topologies with hash / round-robin / broadcast routing; total failure-path coverage with `@retry`/`@recover`/`@error`; retry-aware Level-2 timeout budgets; runtime fault injection with exact message accounting; measured zero-loss chaos demos.
