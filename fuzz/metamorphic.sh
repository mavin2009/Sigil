#!/usr/bin/env bash
# Metamorphic property: ACCEPTED(sigil) => COMPILES(generated rust).
# A program the compiler blesses must never produce a crate rustc rejects.
set -uo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SIGIL_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
SIGILC="${SIGILC:-$SIGIL_ROOT/target/debug/sigilc}"
WORK="$SIGIL_ROOT/target/fuzz/metamorphic"
GEN="$WORK/generated"
mkdir -p "$WORK/mm" "$WORK/bad_codegen"

if [[ ! -x "$SIGILC" ]]; then
  cargo build --manifest-path "$SIGIL_ROOT/Cargo.toml" -p sigilc
fi

ACC=0; BAD=0
for seed in $(seq $1 $2); do
  python3 "$SCRIPT_DIR/gen.py" valid "$seed" > "$WORK/mm/t.sigil" 2>/dev/null || continue
  if "$SIGILC" "$WORK/mm/t.sigil" "$GEN" --emit-main --level 4 >/dev/null 2>&1; then
    ACC=$((ACC+1))
    if ! (cd "$GEN" && timeout 120 cargo build -q >/dev/null 2>&1); then
      BAD=$((BAD+1)); cp "$WORK/mm/t.sigil" "$WORK/bad_codegen/${seed}.sigil"
      (cd "$GEN" && cargo build -q 2>&1 | grep -E "^error" -A 3 | head -8) > "$WORK/bad_codegen/${seed}.err"
    fi
  fi
done
echo "accepted=$ACC bad_codegen=$BAD"
test "$BAD" -eq 0
