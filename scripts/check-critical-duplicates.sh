#!/usr/bin/env bash
set -euo pipefail

metadata="$(cargo metadata --format-version 1 --locked)"
for crate in tokio pest pest_derive thiserror anyhow sha2; do
  versions="$(
    jq -r --arg crate "$crate" \
      '.packages[] | select(.name == $crate) | .version' <<<"$metadata" | sort -u
  )"
  count="$(wc -l <<<"$versions" | tr -d ' ')"
  if [[ "$count" -gt 1 ]]; then
    echo "critical crate '$crate' resolves to multiple versions:"
    echo "$versions"
    exit 1
  fi
done
echo "critical dependency versions are unique"
