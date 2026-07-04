#!/usr/bin/env bash
# Full test pass — the same gate runs as the pre-push hook and in CI:
# formatting, debug tests, release tests, then the Node ABI test against the
# real wasm module.
set -euo pipefail
cd "$(dirname "$0")"

echo "== cargo fmt --check =="
cargo fmt --all --check

echo "== cargo test (debug; builds the wasm module via cli/build.rs) =="
cargo test --workspace

echo "== cargo test (release) =="
cargo test --workspace --release --lib

echo "== node ABI test against the real wasm module =="
if command -v node >/dev/null 2>&1; then
  node --test tests/
else
  echo "SKIP: node not found on PATH — install Node.js 18+ to run the wasm ABI test"
fi
