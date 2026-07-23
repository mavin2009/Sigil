#!/bin/bash
# THE property: PROVEN(invariant) => the running program never violates it.
# Generated demos assert their own proven holds, so a violation surfaces as
# an assertion failure in a program the compiler blessed.
SIGILC=/home/claude/Sigil/target/debug/sigilc
GEN=/home/claude/Sigil/generated/psound
mkdir -p ps unsound
PROVEN=0; RAN=0; UNSOUND=0
for seed in $(seq $1 $2); do
  python3 prover_gen.py $seed > ps/t.sigil 2>/dev/null || continue
  if ! $SIGILC ps/t.sigil $GEN --emit-main --level 4 >/dev/null 2>&1; then continue; fi
  PROVEN=$((PROVEN+1))
  if ! (cd $GEN && timeout 150 cargo build -q >/dev/null 2>&1); then continue; fi
  RAN=$((RAN+1))
  outp=$(cd $GEN && SIGIL_DEMO_SHARDS=2 SIGIL_DEMO_PRODUCERS=6 SIGIL_DEMO_MSGS=25 \
         SIGIL_CHAOS_FAIL_PCT=20 SIGIL_CHAOS_LATENCY_MS=40 timeout 90 cargo run -q --bin demo 2>&1)
  if echo "$outp" | grep -q "PROVEN INVARIANT VIOLATED"; then
    UNSOUND=$((UNSOUND+1))
    cp ps/t.sigil unsound/${seed}.sigil
    echo "$outp" | grep "VIOLATED" | head -2 > unsound/${seed}.txt
  fi
done
echo "proven=$PROVEN ran=$RAN UNSOUND=$UNSOUND"
