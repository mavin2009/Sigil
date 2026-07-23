# Sigil Language Reference

Complete surface syntax and semantics. The grammar here matches
`sigilc/src/frontend/sigil.pest`; if they ever disagree, the `.pest` file is
authoritative and the discrepancy is a bug.

- [Lexical](#lexical)
- [Compilation unit](#compilation-unit)
- [Types](#types)
- [Schemas](#schemas)
- [Transforms](#transforms)
- [Processes and handlers](#processes-and-handlers)
- [Statements](#statements)
- [Expressions and pipelines](#expressions-and-pipelines)
- [Effect tags](#effect-tags)
- [Sending between processes](#sending-between-processes)
- [Back-pressure](#back-pressure)
- [Specs](#specs)
- [Reserved names](#reserved-names)

---

## Lexical

```
WHITESPACE   = space | tab | CR | LF
COMMENT      = "//" ~ (!newline ~ ANY)*

ident        = (letter | "_") ~ (letter | digit | "_")*
duration     = digits ~ ".ms"                 // 50.ms
string       = "\"" ~ (!"\"" ~ ANY)* ~ "\""
number       = "-"? ~ digits ~ ("." ~ digits)?
boolean      = "true" | "false"
```

Durations are milliseconds only. There is no `.s` or `.us`; one unit means
budget arithmetic never silently mixes scales.

## Compilation unit

```
file = (schema_def | process_def | transform_def | spec_def)*
```

Order does not matter. Declarations are resolved after the whole file is
parsed.

## Types

```
type = "Int" | "Float" | "String" | "Bool"
     | "UUID" | "Bytes" | "Duration"
     | ident                                  // a named schema
```

`Int` becomes `i64`, `Float` becomes `f64`. There is no null, no optional,
and no union type — a value of type `T` is always a `T`.

## Schemas

```
schema_def = "schema" ~ ident ~ "{" ~ fields? ~ "}"
fields     = field ~ ("," ~ field)* ~ ","?
field      = ident ~ ":" ~ type
```

```
schema Payment {
  id: String,
  amount: Float,
  units: Int
}
```

Schemas generate `#[derive(Clone, Debug, Default)]` structs.

## Transforms

```
transform_def = "transform" ~ ident
              ~ "(" ~ ident ~ ":" ~ type ~ ")"
              ~ "->" ~ type
              ~ "{" ~ stmt* ~ "}"
```

A transform is a one-argument function. **The body's presence is
semantically significant:**

| Body | Meaning | Compiled as |
| ---- | ------- | ----------- |
| non-empty | **pure** — infallible, cannot be slowed | a real Rust function |
| empty | **external** — real I/O lives here | a stub that can fail and hang |

```
transform add_fee(o: Order) -> Order { o }     // pure: compiled
transform authorize(o: Order) -> Order {}      // external: residual
```

This distinction drives most of the compiler:

- external stages must declare a failure path ([effect tags](#effect-tags))
- recovery targets should be pure — a fallback that can itself fail
  reintroduces the loss it exists to prevent (rejected at Level 3+)
- pure bodies may only call other pure transforms; calling an external
  transform from a pure body is a Level-1 error
- external transforms may only appear as pipeline steps, never as bare
  calls, so their failure path is always visible

Declared signatures are authoritative for type checking.

## Processes and handlers

```
process_def  = "process" ~ ident ~ "{" ~ process_body ~ "}"
process_body = (state_decl | on_handler)*

state_decl   = "state" ~ ident ~ ":" ~ type ~ "=" ~ expr
on_handler   = "on" ~ ident ~ ":" ~ type ~ "{" ~ stmt* ~ "}"
```

```
process Ledger {
  state posted: Int = 0
  state total: Float = 0.0

  on payment: Payment {
    posted := posted + 1
  }
}
```

State is **process-local, always**. A write to a slot the process does not
declare is a Level-1 error. Each process compiles to a shared-nothing actor:
`spawn(self, capacity)` moves the state into an isolated task, and the only
way in afterwards is a message.

A process may declare **several handlers** for different message types:

```
process OrderGateway {
  state received: Int = 0

  on new_order: NewOrder { received := received + 1; ... }
  on cancel: Cancel      { received := received + 1; ... }
}
```

Both message **names** and message **types** must be unique within a
process: the name becomes the dispatch enum variant and scopes input guards,
and `send` resolves its destination handler by type. All proof obligations
(ordering, counting, latency budget) are applied to each handler
independently — one compliant handler never excuses another.

## Statements

```
stmt         = let_stmt | assign_stmt | send_stmt | expr_stmt
let_stmt     = "let" ~ ident ~ "=" ~ expr
assign_stmt  = ident ~ ":=" ~ expr            // local state write
send_stmt    = "send" ~ expr ~ "to" ~ ident ~ route_clause? ~ backpressure?
expr_stmt    = expr
```

`let` bindings are immutable. `:=` writes process-local state. Statement
order is significant for proofs: Level 4 requires a counter to be updated
**before** the send it bounds.

## Expressions and pipelines

Precedence: comparisons bind looser than `+ -`, which bind looser than `* /`.

```
expr         = comparison
comparison   = sum ~ (cmp_op ~ sum)?
cmp_op       = "<=" | ">=" | "==" | "<" | ">"
sum          = product ~ (("+" | "-") ~ product)*
product      = pipeline ~ (("*" | "/") ~ pipeline)*

pipeline     = atom ~ pipe_tail*
pipe_tail    = "~>" ~ atom ~ tag*

atom         = if_expr | schema_lit | call | field_access
             | literal | ident | "(" ~ expr ~ ")"

field_access = ident ~ "." ~ ident
call         = ident ~ "(" ~ (expr ~ ("," ~ expr)*)? ~ ")"
schema_lit   = ident ~ "{" ~ field_init ~ ("," ~ field_init)* ~ ","? ~ "}"
field_init   = ident ~ ":" ~ expr
if_expr      = "if" ~ expr ~ "{" ~ expr ~ "}" ~ "else" ~ "{" ~ expr ~ "}"
literal      = duration | string | number | boolean
```

`~>` is the pipeline operator: `x ~> f` applies `f` to `x`. Chained steps
thread the value through, and each step may carry effect tags.

```
let receipt = order ~> authorize @error
                    ~> charge @timeout(200.ms) @retry(1) @recover(with: refund)
```

## Effect tags

```
tag = "@timeout" ~ "(" ~ expr ~ ")"
    | "@recover" ~ "(" ~ "with" ~ ":" ~ expr ~ ")"
    | "@retry"   ~ "(" ~ expr ~ ")"
    | "@error"
```

| Tag | Meaning |
| --- | ------- |
| `@timeout(N.ms)` | abandon the stage after N ms |
| `@recover(with: f)` | on failure or timeout, run `f` instead |
| `@retry(n)` | re-attempt up to `n` extra times before the failure path |
| `@error` | acknowledge that failure here intentionally drops the message |

Rules, all enforced at Level 1 unless noted:

- Every **external** stage must carry `@recover` or `@error`. There is no
  untagged way to call something that can fail.
- Every `@timeout` needs `@recover` or `@error` **on the same step**. A
  recovery elsewhere in the program does not satisfy it.
- `@recover` is legal with or without `@timeout` (plain failure recovery).
- `@retry(n)` requires `@recover` or `@error` on the same step: retries
  delay failure, they do not handle it. `n` must be an integer literal ≥ 1.
- At most one of each tag per step; `@recover` and `@error` together is an
  error — a step either recovers or acknowledges the drop.
- `@timeout` + `@error` is an **acknowledged timed drop**: failure
  propagates honestly and is counted in the actor's `dropped` total.
- Level 2 charges the latency budget `(1 + retries) × timeout` per stage.
- Recovery targets should be pure; an external `@recover` target is reported
  as residual risk and rejected at Level 3+.

## Sending between processes

```
send_stmt    = "send" ~ expr ~ "to" ~ ident ~ route_clause? ~ backpressure?
route_clause = ("by" ~ expr) | "broadcast"
```

```
send ok to RiskEngine by ok.account      // hash affinity
send s to Settlement                     // round-robin (default)
send done to Audit broadcast             // every shard gets a clone
```

| Routing | Behaviour |
| ------- | --------- |
| default | round-robin across the destination's shards |
| `by <key>` | hash the key — the same key always reaches the same shard, so per-key ordering and shard-local state stay coherent |
| `broadcast` | deliver a clone to every shard |

The compiler derives the whole topology from these statements and checks:

- the target is a declared process (self-sends are rejected: they deadlock)
- the sent value's type resolves to exactly one handler on the target
- the process graph is **acyclic** — cycles over bounded channels can
  deadlock
- routing keys are hashable: `Float` keys are rejected, because float
  hashing is not a stable shard function

Types are inferred locally within a handler, never from a program-global
environment, so identically-named bindings in different processes cannot
cause a message to be dispatched to the wrong handler.

## Back-pressure

```
backpressure = "@block" | "@shed" | "@deadline" ~ "(" ~ expr ~ ")"
```

What a `send` does when the destination's queue is full:

| Policy | Wait | Loss | Latency bound |
| ------ | ---- | ---- | ------------- |
| `@block` (default) | until capacity | none | **unbounded** |
| `@shed` | never | drops when full (counted) | O(1) |
| `@deadline(N.ms)` | up to N ms | drops past N (counted) | N |

All three preserve downstream-counting invariants, because shedding only
*decreases* the downstream count. Only the bounded policies can back an
end-to-end latency claim — see [`require path_latency`](#specs).

Blocking sends cannot deadlock: the process graph is proven acyclic, and
handlers terminate (bounded retries over bounded timeouts), so every sink
always drains and back-pressure propagates cleanly upstream.

Shed counts appear per actor in `ActorStats.shed` and in the run report.

## Specs

```
spec_def       = "spec" ~ ident ~ "{" ~ spec_item* ~ "}"
spec_item      = extinct_clause | require_clause | hold_clause
extinct_clause = "extinct" ~ "[" ~ ident ~ ("," ~ ident)* ~ "]"
require_clause = "require" ~ expr
hold_clause    = "hold" ~ expr
```

### `require`

| Form | Meaning | Level |
| ---- | ------- | ----- |
| `require path_timeout_sum <= N.ms` | worst-case **processing** time along the longest topology path | 2 |
| `require path_latency <= N.ms` | processing **plus queue hand-off**; rejected if any send on the path uses `@block` | 2 |
| `require <msg>.<field> <cmp> <literal>` | an input contract — compiled into a runtime guard at handler entry | 3 |

Budgets take the maximum over a process's handlers (a message traverses
exactly one) and the maximum over parallel branches (not the sum).

Input contracts are the reason Level-3 proofs are unconditional: an
assumption is not taken on faith, it is enforced. Messages violating a
`require` are rejected as typed errors and counted as drops.

### `hold`

| Form | Proven at | How |
| ---- | --------- | --- |
| `hold state <cmp> literal` | 3 | induction: init satisfies it, and every reachable update re-establishes it |
| `hold state_a <cmp> state_b` | 3 | per-handler delta argument within one process |
| `hold P.state <cmp> Q.state` | 4 | structural, across the topology |

### `extinct`

```
extinct [null]
```

Lists assumptions to record in the residual-risk report. Parsed and
reported; it does not itself discharge an obligation.

## Reserved names

`path_timeout_sum` and `path_latency` are compiler-provided quantities in
`require` clauses. Everything else is user-defined.

---

## Complete example

```
schema Order   { id: String, amount: Float }
schema Receipt { id: String, status: String }

transform authorize(o: Order) -> Order {}
transform charge(o: Order) -> Order {}
transform confirm(o: Order) -> Receipt {}
transform refund(o: Order) -> Order { o }

process OrderPipeline {
  state last_order: String = "none"
  state total_charged: Float = 0.0

  on order: Order {
    let auth    = order ~> authorize @error
    let charged = auth ~> charge @timeout(200.ms) @retry(1) @recover(with: refund)
    let receipt = charged ~> confirm @error
    last_order    := receipt.id
    total_charged := total_charged + order.amount
  }
}

spec OrderSlo {
  require order.amount >= 0.0
  require path_timeout_sum <= 500.ms
  hold total_charged >= 0.0
  extinct [null]
}
```

See [ASSURANCE.md](ASSURANCE.md) for what each level proves about this, and
[RUNTIME.md](RUNTIME.md) for what it compiles to.
