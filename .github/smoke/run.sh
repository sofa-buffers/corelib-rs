#!/usr/bin/env bash
# Consume the crate the way a stranger would: from outside the repository, as an
# ordinary dependency, through nothing but its public API.
#
#   CRATE=sofa-buffers-corelib .github/smoke/run.sh 'path = "target/package/sofa-buffers-corelib-0.11.0"'
#   CRATE=sofa-buffers-corelib .github/smoke/run.sh 'version = "=0.11.0"'
#
# The argument is the right-hand side of the dependency line, so the same
# script covers both halves of a release: the packaged artifact before the
# upload, and what crates.io actually serves after it. Used by
# `.github/workflows/release.yml`; runnable by hand for the same reason.
set -euo pipefail

DEP_SPEC="${1:?usage: run.sh '<cargo dependency spec>'}"
CRATE="${CRATE:?set CRATE to the crates.io package name}"
SMOKE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/src"
cp "$SMOKE_DIR/roundtrip.rs" "$WORK/src/main.rs"

cat > "$WORK/Cargo.toml" <<EOF
[package]
name = "sofab-smoke"
version = "0.0.0"
edition = "2021"
publish = false

[dependencies]
$CRATE = { $DEP_SPEC }
EOF

echo "== smoke: $CRATE { $DEP_SPEC }"
cat "$WORK/Cargo.toml"
cargo run --release --manifest-path "$WORK/Cargo.toml"
