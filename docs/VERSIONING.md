# Versioning and Proof Stability

Once a component's build gates on `--level 4`, the compiler is part of your
trusted computing base and the meaning of a proof is part of your interface.
This document states what may change, what may not, and how a proof-affecting
change is communicated.

- [Semantic versioning](#semantic-versioning)
- [Proof-breaking changes](#proof-breaking-changes)
- [The stability tiers](#the-stability-tiers)
- [Supply chain](#supply-chain)
- [Current status](#current-status)

---

## Semantic versioning

`sigilc` and the language version together. For `MAJOR.MINOR.PATCH`:

| Change | Bump |
| ------ | ---- |
| A program that compiled no longer compiles | MAJOR |
| **A proof that was discharged is no longer discharged** | MAJOR |
| **A proof means something weaker than it did** | MAJOR |
| New syntax, new checks that only reject previously-unsound programs | MINOR |
| A previously-rejected program is now accepted (prover got more precise) | MINOR |
| Diagnostics, codegen shape, performance, docs | PATCH |

The two bold rows are the ones that matter. They are treated as breaking
even though they break no *compilation*, because they change what the
artifact means.

## Proof-breaking changes

A change is **proof-breaking** if either holds:

1. A spec clause that was discharged in version *N* is not discharged in
   *N+1* (your build starts failing — loud, and safe).
2. A spec clause discharged in both versions **proves a weaker property** in
   *N+1* (your build still passes — silent, and dangerous).

Category 2 is the one this policy exists for. A user who wrote
`hold Vault.served <= Audit.recorded` at v0.5 must be able to rely on that
sentence meaning the same thing at v0.6, or be told loudly that it does not.

**Commitments**

- Category 2 changes require a MAJOR bump and an entry in `CHANGELOG.md`
  naming the affected clause form and stating precisely what changed.
- Every change to a proof obligation ships with a program in
  `examples/proofs/` demonstrating the new boundary.
- Soundness fixes are exempt from the MAJOR requirement **only** when they
  make the prover stricter — refusing something previously accepted is a
  build failure, never a silent weakening. Such fixes ship as MINOR with a
  `SOUNDNESS` entry in the changelog.

That exemption has been used. In `11b6714`, guard correlation was found to
prove a false invariant when the guard read state the handler mutated
(`examples/proofs/guard_mutated_state.sigil`). The fix made the prover
stricter: programs relying on the unsound correlation now fail to build. A
user's build breaking is the correct outcome — the alternative was continuing
to hold a proof that the running program contradicted.

## The stability tiers

Not all of the surface deserves the same guarantee.

| Tier | Surface | Guarantee |
| ---- | ------- | --------- |
| **Stable** | Level 1 checks; `schema`, `transform`, `process`, `on`, `send`, effect tags; `SigilError`, `ActorStats` | semver as above |
| **Stable** | Level 2 budgets; Level 3/4 obligations as documented in [ASSURANCE.md](ASSURANCE.md) | semver, plus the proof-breaking policy |
| **Provisional** | The provers' *precision* — which valid programs they can discharge | may improve in MINOR; a program that failed may start passing |
| **Unstable** | Generated code shape, diagnostic wording, `topology.mmd` layout, demo driver | may change in PATCH |

"Provisional precision" is deliberate: the interval domain and the counting
argument will get sharper over time. Getting *more* provable is safe. Getting
*less* provable is a MAJOR bump.

## Supply chain

For high-assurance use, `sigilc` is in the trusted computing base — a
compromised or buggy compiler invalidates every proof downstream.

**What you should do**

- Pin the exact version **and** git SHA; record both in the release artifact
  next to `RESIDUAL_RISK.md`.
- Build the compiler from source in your own pipeline rather than consuming
  a binary, if your environment requires it. The workspace has no build
  script and no proc-macro of its own.
- Vendor and review dependencies. The compiler's runtime dependency surface
  is deliberately small: `pest` (parsing), `anyhow` (errors), `thiserror`.
  Generated crates depend on `tokio`, `sigil_rt`, `thiserror`, and
  optionally `tracing`.
- Diff generated output across compiler upgrades. Because codegen is
  Unstable tier, output *will* change; because it is readable Rust, the diff
  is reviewable. That is the intended safety net.

**What is not yet done**

- Reproducible builds of the compiler are not verified.
- Release artifacts are not signed.
- No SBOM is published.

These are real gaps for a regulated environment and are listed rather than
glossed.

## Current status

**v0.7 — pre-1.0. The language surface is NOT frozen.**

Nothing above is a promise yet, because there is no 1.0. The policy is
published now so it can be argued with before it constrains anything, and so
the shape of the commitment is visible to anyone evaluating adoption.

Before a 1.0 that would carry these guarantees:

- [ ] the ORDERING / counting core reviewed as an isolated, separately
      testable unit with explicit stated preconditions
- [ ] a soundness argument for each Level 3/4 obligation written out well
      enough to be attacked
- [ ] `CHANGELOG.md` with proof-affecting entries, from 1.0 onward
- [ ] the language surface frozen, with a migration story for changes
- [ ] at least one component running in production long enough to have
      survived real operational chaos

The last item cannot be manufactured. Sigil has been exercised by fuzzing,
fault injection, and runtime invariant checking — it has **not** been run in
production by anyone, and no claim in this repository should be read as
implying otherwise.
