# Generated ABI and Artifact Schemas

Sigil 0.7 emits generated ABI version **1** and residual-risk schema version
**1**. Generated crates declare both under `[package.metadata.sigil]`.

Every generation transaction publishes:

- `SIGIL_BUILD.json`: compiler/language/runtime versions, ABI versions, MSRV,
  verification toolchain, source SHA-256, workspace lockfile SHA-256, and
  runtime path;
- `SIGIL_EFFECTS.json`: stable, machine-readable foreign-effect contracts;
- `RESIDUAL_RISK.json`: assurance level and owned residual items;
- `RESIDUAL_RISK.md`: the human review form of the same operational boundary.

Golden v1 fixtures live under `sigilc/tests/fixtures/abi_v1` and are checked
by `artifact_compatibility.rs`.

## Compatibility policy

- Adding an optional JSON field is backward-compatible within a schema
  version.
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

No v1-to-v2 migration exists yet because version 2 has not been defined.
Regeneration is the only supported way to create v1 artifacts.
