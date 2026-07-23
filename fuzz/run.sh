#!/usr/bin/env bash
# Property: sigilc never panics, aborts, receives a fatal signal, or hangs.
set -uo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SIGIL_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
SIGILC="${SIGILC:-$SIGIL_ROOT/target/debug/sigilc}"
WORK="$SIGIL_ROOT/target/fuzz/panic"
mkdir -p "$WORK/crashes" "$WORK/out"

if [[ ! -x "$SIGILC" ]]; then
  cargo build --manifest-path "$SIGIL_ROOT/Cargo.toml" -p sigilc
fi

CRASH=0; TOTAL=0
for mode in valid mutated nested truncated; do
  for seed in $(seq $1 $2); do
    python3 "$SCRIPT_DIR/gen.py" "$mode" "$seed" > "$WORK/out/t.sigil" 2>/dev/null || continue
    TOTAL=$((TOTAL+1))
    err=$(timeout 15 "$SIGILC" "$WORK/out/t.sigil" "$WORK/out/gen" --level 4 2>&1 >/dev/null)
    rc=$?
    if [[ $rc -eq 101 || $rc -eq 124 || $rc -ge 128 ]] || grep -q "panicked" <<<"$err"; then
      CRASH=$((CRASH+1))
      cp "$WORK/out/t.sigil" "$WORK/crashes/${mode}_${seed}.sigil"
      head -3 <<<"$err" > "$WORK/crashes/${mode}_${seed}.err"
    fi
  done
done
echo "modes=4 total=$TOTAL crashes=$CRASH"
test "$CRASH" -eq 0
