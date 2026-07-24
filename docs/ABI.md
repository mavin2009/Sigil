# Generated ABI and Artifact Schemas

Sigil 0.7 emits generated ABI version **4** and residual-risk schema version
**1**. Generated crates declare both under `[package.metadata.sigil]`.

Every generation transaction publishes:

- `SIGIL_BUILD.json`: compiler/language/runtime versions, ABI versions, MSRV,
  verification toolchain, routing-hash and distributed-protocol versions,
  source SHA-256, workspace lockfile SHA-256, and runtime path;
- `SIGIL_EFFECTS.json`: stable, machine-readable foreign-effect contracts;
- `RESIDUAL_RISK.json`: assurance level and owned residual items;
- `RESIDUAL_RISK.md`: the human review form of the same operational boundary.

Immutable v1/v2/v3 and current v4 fixtures live under
`sigilc/tests/fixtures/abi_v1`, `abi_v2`, `abi_v3`, and `abi_v4`;
`artifact_compatibility.rs` checks the current artifacts and ensures every
earlier representation remains distinctly versioned.

Generated manifests require the exact matching `sigil_rt` release as well as
the recorded local path. Generated Rust also contains a compile-time check of
`sigil_rt::ROUTING_HASH_VERSION`, so a mismatched routing contract fails the
build before it can place messages on different shards. The generated crate
performs the same compile-time equality check for
`distributed::DISTRIBUTED_PROTOCOL_VERSION`.

## Compatibility policy

- Adding an optional JSON field is backward-compatible within a schema
  version.
- A routing-hash version change is a state-placement migration even when the
  JSON schema and generated Rust ABI versions do not change. Mixed versions
  must never serve the same affinity-sharded actor fleet.
- Removing, renaming, changing the type/meaning of a field, or changing
  generated public Rust interfaces requires a version increment.
- Readers must reject an unknown major integer rather than guessing.
- A compiler may read/compare old metadata, but it never silently rewrites an
  approved artifact in place.

## Migration procedure

Before incrementing either version:

1. add an immutable fixture for the old and new representations;
2. document every field/interface mapping and any information loss;
3. add a deterministic migration command or state explicitly that
   regeneration from the recorded source/lockfile is required;
4. test old-reader rejection, new-reader acceptance, and byte-stable
   regeneration;
5. require residual-risk owners to approve semantic changes.

## Version 1 to version 2

Version 2 adds the generated `ProcessConfig`, `ComponentConfig`,
`ComponentHealth`, and `Component` production interfaces, concurrent
`IngressRouter` entry routing, and bounded handle-admission methods. Existing
process, transform, and schema semantics are unchanged, so there is no state
or data conversion. Regenerate the crate from its recorded Sigil source and
lockfile; there is no in-place artifact rewrite. Integrations may keep using
the lower-level `new` / `connect_*` / `spawn` API, or migrate wiring to
`Component::start`, routed ingress, readiness, and `Component::shutdown`.

Readers and deployment gates expecting generated ABI 1 must reject the v2
metadata until explicitly upgraded. The residual-risk schema and routing-hash
contract remain version 1.

## Version 2 to version 3

Version 3 adds source-level placement declarations and the generated
`COMPONENT_PLACEMENT` and `transport_manifest` interfaces. Runtime ABI adds
transport negotiation/envelopes, bounded remote admission, delivery
semantics, deduplication, epoch-fenced `ShardLease`, and checkpoint handoff.
Existing process execution and the generated in-process `Component` are
unchanged.

There is no automatic state or wire conversion. Regenerate from recorded
Sigil source and lockfile. A deployment adopting placement must provide its
own schema codec and `Transport` implementation, and it must not enable
at-least-once delivery until producer message sequences and the receiver
deduplication frontier are durable. Stateful migrations must begin with a
fresh monotonically increasing ownership epoch and checkpoint the
deduplication frontier with actor state.

Readers and deployment gates expecting generated ABI 1 or 2 must reject v3
metadata until explicitly upgraded. Residual-risk schema version 1 and
routing-hash version 1 are unchanged; the independent distributed protocol
starts at version 1.

## Version 3 to version 4

Version 4 adds compiler-generated deterministic `WireCodec` implementations
and typed `encode_remote`, `decode_remote`, and `authorize_remote` helpers for
every finite, compiler-owned schema. Strings and bytes are length-prefixed and
allocation-bounded; integers and finite floats are canonical little-endian;
booleans, UTF-8, durations, truncation, and trailing bytes are validated
strictly. Nested schemas encode in declaration order.

Every exact schema version also carries a compiler-derived SHA-256 structural
fingerprint over its name, ordered fields/types, and nested fingerprints.
Negotiation chooses the highest common version whose fingerprint matches;
same-version layout skew is rejected before traffic. The current generated
codec advertises exact version 1, so any structural edit requires a coordinated
rollout (or a future explicitly versioned compatibility adapter).

The runtime also adds typed `RemoteEndpoint<T>` routing and the
`MessageIdSource` durability contract. Generated `RemoteEndpoints` aggregates
the exact outbound boundary fields and validates their destination placement,
process, and deployment identity. Cross-placement `@block` is now rejected;
remote sends must shed immediately or use a finite deadline.

Remote placement edges now fail compilation when their message is a primitive,
a foreign bound type, recursively contains a foreign type, or otherwise lacks
a complete generated codec. Foreign types require an explicit deployment
adapter instead of an assumed serialization layout.

There is no automatic wire conversion from ABI 3. Regenerate both peers from
the recorded source and lockfile, deploy only after their schema manifests
negotiate the same version and fingerprint, and retain ABI 3 processes as a
separate rollout cohort until drained. Residual-risk schema, routing-hash, and
distributed protocol versions remain 1.

## Version 4 to version 5

Version 5 makes distributed execution an executable generated surface rather
than manifest-only metadata. `DurableOutbox` and `DurableRemoteEndpoint<T>`
enforce persist-before-send, durable attempt accounting, bounded retry, and
metadata-checked idempotent acknowledgement. An exhausted record remains
pending for operator action; transport acceptance is still distinct from
receiver application and durable commit.

Generated `PlacementComponent::start` is asynchronous and starts only the
actors assigned to one declared placement. Local edges use typed bounded
channels; cross-placement edges require the exact generated durable endpoint.
Remote receiver handles accept an owned `AuthorizedMessage<T>` through their
bounded inbox, retaining the shard permit through handler execution and the
atomic state/deduplication commit. One `StateCommitter<Process>` is required
per remote receiver shard and restores state plus its dedup frontier before
the actor starts.

The complete `Component::start` remains available as the in-process reference
assembly. There is no automatic migration from a v4 deployment: drain v4
actors, provision durable producer IDs, outbox stores, receiver committers,
and fenced leases, then start the v5 placement components. Residual-risk
schema, routing-hash, and distributed protocol versions remain 1.
