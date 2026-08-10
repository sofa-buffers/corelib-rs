#!/usr/bin/env bash
#
# Generate a test-coverage report for the sofab crate using cargo-llvm-cov.
#
# Prerequisites (one-time):
#   rustup component add llvm-tools-preview
#   cargo install cargo-llvm-cov
#
# Usage:
#   ./coverage.sh            # terminal summary + HTML report under target/llvm-cov/html
#   ./coverage.sh --open     # also open the HTML report in a browser
#
# The instrumented suite runs exactly once (`--no-report`); every rendering
# below replays that profile data with `cargo llvm-cov report`. A bare
# `cargo llvm-cov --html` / `--summary-only` / `--open` would re-run the whole
# suite for the same numbers.
#
set -euo pipefail
cd "$(dirname "$0")"

echo ">> Running tests with coverage instrumentation ..."
cargo llvm-cov clean --workspace
cargo llvm-cov --no-report

echo ">> Rendering reports ..."
cargo llvm-cov report --html         # detailed HTML report
cargo llvm-cov report --summary-only # text summary to stdout

# Machine-readable LCOV for CI upload (Codecov/Coveralls/etc.).
cargo llvm-cov report --lcov --output-path lcov.info
echo ">> HTML report: target/llvm-cov/html/index.html"
echo ">> LCOV:        lcov.info"

if [[ "${1:-}" == "--open" ]]; then
  cargo llvm-cov report --open
fi
