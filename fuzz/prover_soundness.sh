#!/usr/bin/env bash
# THE property: PROVEN(invariant) => the running program never violates it.
# Generated demos assert their own proven holds, so a violation surfaces as
# an assertion failure in a program the compiler blessed.
set -uo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SIGIL_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
SIGILC="${SIGILC:-$SIGIL_ROOT/target/debug/sigilc}"
WORK="$SIGIL_ROOT/target/fuzz/prover-soundness"
GEN="$WORK/generated"
mkdir -p "$WORK/ps" "$WORK/unsound"

if [[ ! -x "$SIGILC" ]]; then
  cargo build --manifest-path "$SIGIL_ROOT/Cargo.toml" -p sigilc
fi

PROVEN=0; RAN=0; BUILD_FAILURES=0; UNSOUND=0
for seed in $(seq $1 $2); do
  python3 "$SCRIPT_DIR/prover_gen.py" "$seed" > "$WORK/ps/t.sigil" 2>/dev/null || continue
  if ! "$SIGILC" "$WORK/ps/t.sigil" "$GEN" --emit-main --level 4 >/dev/null 2>&1; then continue; fi
  PROVEN=$((PROVEN+1))
  if ! (cd "$GEN" && timeout 150 cargo build -q >/dev/null 2>&1); then
    BUILD_FAILURES=$((BUILD_FAILURES+1))
    cp "$WORK/ps/t.sigil" "$WORK/unsound/${seed}_build.sigil"
    (cd "$GEN" && cargo build -q 2>&1 | head -20) > "$WORK/unsound/${seed}_build.txt"
    continue
  fi
  RAN=$((RAN+1))
  outp=$(cd "$GEN" && SIGIL_DEMO_SHARDS=2 SIGIL_DEMO_PRODUCERS=6 SIGIL_DEMO_MSGS=25 \
         SIGIL_CHAOS_FAIL_PCT=20 SIGIL_CHAOS_LATENCY_MS=40 timeout 90 cargo run -q --bin demo 2>&1)
  rc=$?
  if [[ $rc -ne 0 ]] || grep -q "PROVEN INVARIANT VIOLATED" <<<"$outp"; then
    UNSOUND=$((UNSOUND+1))
    cp "$WORK/ps/t.sigil" "$WORK/unsound/${seed}.sigil"
    head -20 <<<"$outp" > "$WORK/unsound/${seed}.txt"
  fi
done
echo "proven=$PROVEN ran=$RAN build_failures=$BUILD_FAILURES UNSOUND=$UNSOUND"
[[ "$BUILD_FAILURES" -eq 0 && "$UNSOUND" -eq 0 ]]
