#!/usr/bin/env bash
set -euo pipefail

# Every Level-3/4 premise is paired with a negative regression in
# docs/SOUNDNESS.md. A surviving mutation in either prover fails this command.
cargo mutants \
  --package sigilc \
  --file sigilc/src/analysis/level3.rs \
  --file sigilc/src/analysis/level4.rs \
  --file sigilc/src/analysis/topology.rs \
  --timeout 180 \
  --jobs 2
