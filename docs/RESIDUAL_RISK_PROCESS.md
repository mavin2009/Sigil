# Residual Risk: From Artifact to Control

Every build emits `RESIDUAL_RISK.md`. Emitting it is the easy part. This
document is the process that turns it into something an organisation acts on
— a review gate, an owner, and a mapping to controls you already run.

The premise: **a proof narrows what can go wrong; it does not eliminate the
need to say what is still assumed.** The residual report is the list of
assumptions. Assumptions with no owner are just optimism with better
formatting.

- [What the report contains](#what-the-report-contains)
- [The review gate](#the-review-gate)
- [Mapping residuals to controls](#mapping-residuals-to-controls)
- [Review template](#review-template)
- [When an external transform changes](#when-an-external-transform-changes)
- [Versioning the report with the binary](#versioning-the-report-with-the-binary)

---

## What the report contains

| Section | Meaning |
| ------- | ------- |
| Assurance level | which checks ran, and **everything not established at that level** |
| Proven invariants | the holds discharged, and by which argument |
| Proof assumptions | input contracts — each one enforced by a generated runtime guard |
| Process topology | verified edges, message types, acyclicity |
| Back-pressure policies | per send, plus the generated channel-cycle argument |
| External (residual) transforms | the real I/O the proofs do **not** cover |

The last row is the important one. Everything else describes what was
established; that row describes what was assumed.

## The review gate

Make the report a reviewable artifact, and make a **diff to it** the trigger.
A changed residual report means the assurance argument changed — which is
precisely when a human should look, and the only time one needs to.

**Recommended CI wiring**

```bash
cargo run -p sigilc -- component.sigil out --level 4 --emit-graph
diff -u baseline/RESIDUAL_RISK.md out/RESIDUAL_RISK.md || {
  echo "Residual risk changed — requires assurance review"; exit 1;
}
```

**Who reviews.** The same people who review a schema migration or an IAM
policy change: someone who owns the consequence, not only the code. In
practice one engineer plus whoever owns the affected control (security for
the vault path, treasury for the settlement path).

**What blocks a merge**

| Change in the report | Response |
| -------------------- | -------- |
| Assurance level dropped | Block. Nothing else in the diff matters until this is explained. |
| A proven invariant disappeared | Block. Either the property was intentionally abandoned — say so in writing — or a refactor silently broke it. |
| A new external transform appeared | Review. A new residual surface needs an owner and a contract. |
| A new proof assumption appeared | Review. Confirm the generated guard actually matches the upstream contract. |
| Back-pressure changed from `@block` to `@shed` | Review. This converts a latency problem into a data-loss problem. That can be right — it must be deliberate. |
| Topology edge added or removed | Review against `topology.mmd`. |

**What does not block.** Latency numbers moving within budget, message
counts, formatting. Reviewer attention is finite; spend it on the rows above.

## Mapping residuals to controls

Residual items are not abstract — each maps to a control most organisations
already run.

| Residual (as reported) | Concrete control |
| ---------------------- | ---------------- |
| External transform `fetch_secret` (no body) | Contract test against the real KMS in CI; SLO and alert on its error rate; the timeout in the `.sigil` source is the *declared* bound, so alert when p99 approaches it |
| External transform `write_audit` (no body) | Durability requirement on the audit sink; the `@retry`/`@recover` policy in source is the documented degradation path |
| OS and scheduler | Pinned runtime version; container CPU limits; worker-thread count fixed rather than inherited from an unknown host |
| Channel capacity / back-pressure tuning | Capacity recorded in deployment config, not hardcoded; alert on sustained `shed` |
| Cancel-safety of external transforms | Review checklist item when any timed stage's implementation changes (see [PRODUCTION.md](PRODUCTION.md)) |
| Input contracts (`require` clauses) | Guards reject violations at runtime and count them; alert on `dropped` rising, which means an upstream producer broke its contract |
| Compiler itself | Pinned version and hash; see [VERSIONING.md](VERSIONING.md) |

**Mapping to common frameworks.** The report is evidence, not a certificate.
Where it tends to land:

- **Change management** — the diff gate above is a change-control record with
  a machine-generated basis, which is unusually strong evidence.
- **Threat modelling** — the external transform list is a ready-made trust
  boundary inventory: every one is a place data crosses out of proven code.
- **Zero trust** — proven ordering invariants (`served <= recorded`) are
  enforcement evidence for "no access without an audit record", stronger
  than a code-review attestation because it holds in every execution.

Nothing here is an audit opinion. The report is input to your controls, and
the mapping needs someone in your organisation to own it.

## Review template

Copy into the pull request when the residual report changes.

```markdown
## Assurance Review

**Component:** <name>            **Level:** <0-4>
**Compiler:** sigilc <version> (<git sha>)

### What changed in RESIDUAL_RISK.md
<paste the diff>

### Invariants
- [ ] Every previously proven invariant is still proven
- [ ] Any removed invariant is intentional, and named here: ______
- [ ] New invariants reviewed for whether they say what we mean

### Residual surface
- [ ] Every new external transform has a named owner
- [ ] Its failure mode is declared in source (`@recover` / `@error`) and matches
      what the dependency actually does
- [ ] Its declared `@timeout` is consistent with the dependency's SLO
- [ ] Cancel-safety confirmed for any timed stage whose body changed

### Assumptions
- [ ] Every `require` contract matches the upstream producer's real guarantee
- [ ] Guard rejections (`dropped`) are alerted on

### Back-pressure
- [ ] Policy changes are deliberate and the trade-off is stated: ______
- [ ] Capacity values reviewed against expected burst

### Sign-off
Engineer: ______   Control owner: ______   Date: ______
```

## When an external transform changes

This is the most common way a proof quietly stops meaning what it did,
because **the compiler never sees the change** — the body lives in your code,
not the `.sigil` file.

Trigger a review when any of these change:

1. **Its latency profile.** The `@timeout` in source is a declared bound. If
   the dependency's p99 crosses it, the recovery path becomes the common
   path, not the exceptional one. Behaviour is still correct; your capacity
   and SLO assumptions may not be.
2. **Its failure modes.** A transform that starts returning partial success
   instead of an error violates the model where failure is a typed drop.
3. **Its cancel-safety.** Timed stages can be cancelled mid-flight. A body
   that becomes non-idempotent under cancellation breaks an assumption no
   proof covers.
4. **Its purity.** A pure transform that acquires I/O is a Level-1 error if
   declared in Sigil — but if the *Rust body* of a pure transform gains a
   side effect, the compiler cannot see it. Recovery paths depend on pure
   transforms being infallible.

Point 4 is worth a lint in your own review checklist: the bodies of pure
transforms are load-bearing for every recovery path in the component.

## Versioning the report with the binary

The report describes a specific build. Keep them together:

- store `RESIDUAL_RISK.md` and `topology.mmd` as **release artifacts**
  alongside the binary, not only in the repository;
- record the compiler version and git SHA in the release;
- when investigating an incident, read the residual report **for the build
  that was running**, not the one on `main`.

An assurance argument that cannot be tied to a deployed artifact is a
document, not a control.
