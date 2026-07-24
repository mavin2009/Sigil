#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SIGIL_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
SIGILC="$SIGIL_ROOT/target/debug/sigilc"
OUT_ROOT="$SIGIL_ROOT/generated"

cargo build --manifest-path "$SIGIL_ROOT/Cargo.toml" -p sigilc --locked
mkdir -p "$OUT_ROOT"

examples=(
  avionics/attitude_control.sigil
  circuit/circuit.sigil
  clearinghouse/clearing.sigil
  concurrent/ledger/ledger.sigil
  concurrent/orderflow/orderflow.sigil
  counter/counter.sigil
  finance/clearing.sigil
  ingest/ingest.sigil
  level2/slo_and_hold.sigil
  level3/proven_ledger.sigil
  level4/conservation.sigil
  pipeline/pipeline.sigil
  resilient/resilient.sigil
  runnable/counter/counter.sigil
  security/vault.sigil
  trading/order_gateway.sigil
)

for relative in "${examples[@]}"; do
  name="${relative//\//_}"
  name="${name%.sigil}"
  output="$OUT_ROOT/.sigil-ci-$name"
  log="$SIGIL_ROOT/target/generated-example-matrix-$name.log"
  "$SIGILC" "$SIGIL_ROOT/examples/$relative" "$output" --emit-main --emit-graph --level 1 \
    >"$log" 2>&1
  cargo generate-lockfile --manifest-path "$output/Cargo.toml" --offline >>"$log" 2>&1
  if ! CARGO_TARGET_DIR="$SIGIL_ROOT/target/generated-example-matrix" \
    cargo check --manifest-path "$output/Cargo.toml" --locked --offline \
      --no-default-features >>"$log" 2>&1; then
    cat "$log"
    exit 1
  fi
  if ! CARGO_TARGET_DIR="$SIGIL_ROOT/target/generated-example-matrix" \
    cargo check --manifest-path "$output/Cargo.toml" --locked --offline \
      --all-features >>"$log" 2>&1; then
    cat "$log"
    exit 1
  fi
  echo "generated example passed: $relative"
done
