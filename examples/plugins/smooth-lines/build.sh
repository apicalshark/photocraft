#!/bin/sh
# Builds the Smooth Lines example plug-ins and refreshes the test fixtures
# built from them. Needs the wasm32-unknown-unknown target:
# `rustup target add wasm32-unknown-unknown`.
# Extra arguments go to cargo (the release workflow passes `--locked`).
set -eu
cd "$(dirname "$0")"
cargo build --release --target wasm32-unknown-unknown "$@"
cp target/wasm32-unknown-unknown/release/stroke_lab_smooth.wasm \
  ../../../crates/plugins/tests/fixtures/smooth-lines.wasm
cp target/wasm32-unknown-unknown/release/stroke_lab_smooth_advanced.wasm \
  ../../../crates/plugins/tests/fixtures/smooth-lines-advanced.wasm
echo "fixtures refreshed in crates/plugins/tests/fixtures/"
