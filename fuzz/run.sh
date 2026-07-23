#!/bin/bash
# Property: sigilc never panics. Exit 101 or "panicked" in stderr = bug.
SIGILC=/home/claude/Sigil/target/debug/sigilc
mkdir -p crashes out
CRASH=0; TOTAL=0
for mode in valid mutated nested truncated; do
  for seed in $(seq $1 $2); do
    python3 gen.py $mode $seed > out/t.sigil 2>/dev/null || continue
    TOTAL=$((TOTAL+1))
    err=$($SIGILC out/t.sigil out/gen --level 4 2>&1 >/dev/null); rc=$?
    if [ $rc -eq 101 ] || echo "$err" | grep -q "panicked"; then
      CRASH=$((CRASH+1))
      cp out/t.sigil crashes/${mode}_${seed}.sigil
      echo "$err" | head -3 > crashes/${mode}_${seed}.err
    fi
  done
done
echo "modes=4 total=$TOTAL crashes=$CRASH"
