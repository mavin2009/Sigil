# Zero-Trust Secrets Vault

`Authn → Authz → Audit → Vault`, where the security properties are proven
rather than reviewed.

```
cargo run -p sigilc -- examples/security/vault.sigil generated/vault --emit-main --level 4
cd generated/vault
SIGIL_CHAOS_FAIL_PCT=20 SIGIL_CHAOS_LATENCY_MS=120 cargo run --bin demo
```

## The proven chain

```
hold Vault.served   <= Audit.recorded     PROVEN
hold Audit.recorded <= Authz.granted      PROVEN
hold Authz.granted  <= Authn.verified     PROVEN
```

Composed: **a secret cannot be served unless an authenticated, authorized
request was audited first.** No fairness or liveness assumptions are needed —
every failure mode the language admits (timeouts, `@error` drops, guard
rejections, staged shutdown) only *decreases* the downstream count, so the
inequalities survive all of them.

## Two mistakes that pass code review and fail the build

**1. Audit after forwarding.** Moving `recorded := recorded + 1` below the
`send` is the most natural refactor in the world, and it means a secret can
reach the vault with no audit record:

```
error[Level 4 (system)]: ORDERING fails — the `request` handler of `Audit`
sends toward `Vault` BEFORE updating `recorded`; a message could arrive
uncounted. Move the update above the send.
```

**2. A deny path that can itself fail.** If `deny_unauthorized` is an
external transform, the policy engine timing out means the *denial* can also
time out:

```
error[Level 4 (system)]: Level-3 requires infallible recovery:
`deny_unauthorized` used as a @recover target but declared external (empty
body). A fallback that can fail or hang reintroduces the loss it exists to
prevent — give it a pure body.
```

## Fail-closed by construction

Every external stage (token verification, policy evaluation, audit write,
KMS fetch) carries `@recover(with: deny_*)`, and every deny path is a **pure**
transform: compiled, infallible, immune to injected latency. There is no
code path in which a timing-out policy engine results in a served secret,
because the only compiled alternative to `allow` is `deny`.

## Measured under 20% fault injection

```
[Authn]  verified = 1920   [Audit]  recorded = 1920
[Authz]  granted  = 1920   [Vault]  served   = 1920
chaos: 10240 external calls, 1757 injected faults, 2560 retries, 632 recoveries
```

1,757 faults hit the security path. The chain held exactly.
