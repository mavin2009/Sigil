#!/bin/bash
# Metamorphic property: ACCEPTED(sigil) => COMPILES(generated rust).
# A program the compiler blesses must never produce a crate rustc rejects.
SIGILC=/home/claude/Sigil/target/debug/sigilc
mkdir -p mm bad_codegen
ACC=0; BAD=0
for seed in $(seq $1 $2); do
  python3 gen.py valid $seed > mm/t.sigil 2>/dev/null || continue
  if $SIGILC mm/t.sigil /home/claude/Sigil/generated/mm --emit-main --level 4 >/dev/null 2>&1; then
    ACC=$((ACC+1))
    if ! (cd /home/claude/Sigil/generated/mm && timeout 120 cargo build -q >/dev/null 2>&1); then
      BAD=$((BAD+1)); cp mm/t.sigil bad_codegen/${seed}.sigil
      (cd /home/claude/Sigil/generated/mm && cargo build -q 2>&1 | grep -E "^error" -A 3 | head -8) > bad_codegen/${seed}.err
    fi
  fi
done
echo "accepted=$ACC bad_codegen=$BAD"
