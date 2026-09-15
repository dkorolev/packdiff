#!/usr/bin/env bash
# Stage the compiled wasm comment engine inside the `packdiff` crate, where
# `cargo package` and `cargo publish` pick it up as `engine/packdiff_wasm.wasm`.
# The crates.io tarball ships this file instead of compiling the engine, so
# installing packdiff — or depending on it — needs no wasm target (see
# cli/build.rs). Run it right before packaging or publishing; the staged file
# is gitignored and never committed.
set -euo pipefail
cd "$(dirname "$0")"

cargo build -p packdiff-wasm --release --target wasm32-unknown-unknown --target-dir target-wasm
mkdir -p cli/engine
cp target-wasm/wasm32-unknown-unknown/release/packdiff_wasm.wasm cli/engine/packdiff_wasm.wasm
echo "staged cli/engine/packdiff_wasm.wasm ($(wc -c < cli/engine/packdiff_wasm.wasm | tr -d ' ') bytes)"
