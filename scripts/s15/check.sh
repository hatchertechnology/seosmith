#!/bin/sh
set -eu
cargo build --locked --example s15_controlled
cargo test --locked --test s15_fixtures
S15_BENCH_BINARY="$PWD/target/debug/examples/s15_controlled" python3 -m unittest discover -s tests/s15 -v
if [ -f scripts/check-contract-vectors.mjs ]; then
  node scripts/check-contract-vectors.mjs
else
  printf '%s\n' 'S01 contract vectors unavailable on this branch; run after S01 merges.'
fi
