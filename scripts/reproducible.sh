#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SIGIL_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
SIGILC="$SIGIL_ROOT/target/debug/sigilc"
OUT_ROOT="$SIGIL_ROOT/target/reproducibility"

cargo build --manifest-path "$SIGIL_ROOT/Cargo.toml" -p sigilc --locked
rm -rf "$OUT_ROOT"
mkdir -p "$OUT_ROOT"
"$SIGILC" "$SIGIL_ROOT/examples/security/vault.sigil" "$OUT_ROOT/first" \
  --emit-main --emit-graph --level 4 >/dev/null
"$SIGILC" "$SIGIL_ROOT/examples/security/vault.sigil" "$OUT_ROOT/second" \
  --emit-main --emit-graph --level 4 >/dev/null
diff -ru "$OUT_ROOT/first" "$OUT_ROOT/second"
echo "generated artifacts reproduce byte-for-byte"
