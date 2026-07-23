# Sigil

A language that makes whole classes of production failures extinct by construction.

Programs are causal graphs: data, effects, time, and failure are first-class. The compiler checks explicit recovery paths, local state discipline, transform signatures, and effect tags, then reports residual risk outside the model.

## Extinct by Design (Level 1)

These failure modes are rejected or unrepresentable at the default safety level:

- Data races and shared mutable state
- Null or undefined values
- `@timeout` without a matching `@recover`
- State writes to non-local slots
- Pipeline stages whose types disagree with declared transform signatures

Residual risk outside the model (external transforms, OS, scheduler) is always reported explicitly.

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

| Directory | Focus |
|-----------|--------|
| `examples/ingest/` | validate → timed decompress → extract → store |
| `examples/counter/` | pure `transform add(x: Int) -> Int { x + 1 }` |
| `examples/resilient/` | compiled `normalize` + residual `enrich` / `store` |
| `examples/circuit/` | timeout + recover + local status state |
| `examples/pipeline/` | dual timed stages, `Order → Receipt` signatures |

## Compiler Pipeline

```
parse → lower (Graph IR) → level1_check → check_transform_signatures
      → residual_risk_report → emit (Rust + Cargo.toml)
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

### Statements

```
stmt         = let_stmt | assign_stmt | expr_stmt
let_stmt     = "let" ~ ident ~ "=" ~ expr
assign_stmt  = ident ~ ":=" ~ expr            // local state write
expr_stmt    = expr
```

### Expressions

Precedence: `+` `-` below `*` `/`; pipelines bind to atoms.

```
expr         = sum
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
             | "@error"
```

Rules:

- Every `@timeout` must have a matching `@recover` on the same program (Level-1).
- `@recover(with: f)` names a fallback transform or expression used when the timed step fails or times out.

Example:

```
let reserved = auth ~> reserve @timeout(120.ms) @recover(with: release)
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
    let reserved = auth ~> reserve @timeout(120.ms) @recover(with: release)
    let charged = reserved ~> charge @timeout(200.ms) @recover(with: refund)
    let receipt = charged ~> confirm
    last_order := receipt.id
    total_charged := total_charged + order.amount
  }
}
```

See `examples/pipeline/pipeline.sigil` and the other directories under `examples/` for full programs.

## Status

v0.2 — Modular compiler (frontend / analysis / backend), declared transform signatures, pure transform bodies, signature-checked pipelines, residual risk reporting, multi-stage examples in dedicated subdirectories.
