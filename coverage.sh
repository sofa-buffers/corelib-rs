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
set -euo pipefail
cd "$(dirname "$0")"

# Cover all feature-gated code paths.
FEATURES="--all-features"

echo ">> Running tests with coverage instrumentation ($FEATURES) ..."
cargo llvm-cov clean --workspace
cargo llvm-cov $FEATURES --html        # detailed HTML report
cargo llvm-cov $FEATURES --summary-only # text summary to stdout

# Machine-readable LCOV for CI upload (Codecov/Coveralls/etc.).
cargo llvm-cov $FEATURES --no-run >/dev/null 2>&1 || true
cargo llvm-cov report $FEATURES --lcov --output-path lcov.info
echo ">> HTML report: target/llvm-cov/html/index.html"
echo ">> LCOV:        lcov.info"

if [[ "${1:-}" == "--open" ]]; then
  cargo llvm-cov $FEATURES --open
fi
