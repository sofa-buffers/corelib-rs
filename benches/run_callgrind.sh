#!/usr/bin/env bash
#
# SofaBuffers Rust — machine-independent instruction cost.
#
# Runs each benchmark workload once under Callgrind and reports instructions
# retired per operation (Ir/op). Unlike wall-clock or CPU time, instruction
# counts are deterministic and independent of the host's clock speed and
# scheduler, so the numbers compare across machines (and against the C/C++/Go/
# Python/TypeScript tools — the workloads, ids and values are identical).
#
# The bench binary exposes each workload as a `#[inline(never)]`,
# `#[unsafe(no_mangle)]` `run_<workload>` function that performs exactly one op;
# `--collect-atstart=no --toggle-collect=run_<workload>` therefore measures a
# single op's Ir directly (no rep-count subtraction needed — native symbols,
# unlike the JIT/interpreted ports).
#
# Prereqs: valgrind, cargo. This builds the bench binary if missing.
# Usage:   bash benches/run_callgrind.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if ! command -v valgrind >/dev/null 2>&1; then
    echo "error: valgrind not found (needed for instruction counts)." >&2
    echo "       install it, e.g.  apt-get install valgrind" >&2
    exit 1
fi

echo ">> building bench binary (profile: bench) ..." >&2
BIN="$(cargo bench --bench bench --no-run --message-format=json 2>/dev/null \
    | grep -o '"executable":"[^"]*/bench-[^"]*"' | head -1 \
    | sed 's/.*"executable":"//;s/"$//')"
if [ -z "${BIN:-}" ] || [ ! -x "$BIN" ]; then
    echo "error: could not locate the built bench binary." >&2
    exit 1
fi

OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT
# Order follows BENCH_SPEC's table. `encode: blob 1MB passthrough` is its one
# optional row and this port implements no pass-through (CORELIB_PLAN §5.1 makes
# it a MAY), so the row is absent rather than a placeholder.
WORKLOADS=(
    encode_u64_array
    encode_typical
    encode_blob_oneshot
    encode_blob_streaming
    encode_composite
    decode_u64_array
    decode_typical
    decode_blob
    decode_composite
    decode_composite_skip
)

run_cg() { # $1 workload
    valgrind --tool=callgrind --collect-atstart=no --toggle-collect="run_$1" \
        --callgrind-out-file="$OUT/$1.out" "$BIN" "$1" \
        >/dev/null 2>"$OUT/$1.log"
}

ir_of()    { grep -m1 '^summary:' "$OUT/$1.out" 2>/dev/null | awk '{print $2}'; }
bytes_of() { grep -ohE 'BYTES=[0-9]+' "$OUT/$1.log" 2>/dev/null | head -1 | cut -d= -f2; }

label() {
    case "$1" in
        encode_u64_array)      echo "encode: u64 array (1000)";;
        encode_typical)        echo "encode: typical message";;
        encode_blob_oneshot)   echo "encode: blob 1MB one-shot";;
        encode_blob_streaming) echo "encode: blob 1MB streaming";;
        encode_composite)      echo "encode: composite";;
        decode_u64_array)      echo "decode: u64 array (1000)";;
        decode_typical)        echo "decode: typical message";;
        decode_blob)           echo "decode: blob 1MB";;
        decode_composite)      echo "decode: composite";;
        decode_composite_skip) echo "decode: composite skip-all";;
    esac
}

echo ">> Measuring instructions/op under Callgrind (this is slow) ..." >&2
echo
echo "==============================================================================="
echo " SofaBuffers Rust instruction cost   (Callgrind, Ir/op)"
echo " instructions/op: lower is better. Deterministic & machine-independent."
echo "==============================================================================="
printf "%-26s %16s %9s\n" "Workload" "instr/op" "bytes"
printf "%-26s %16s %9s\n" "--------" "--------" "-----"

missing=()
for w in "${WORKLOADS[@]}"; do
    run_cg "$w"
    ir="$(ir_of "$w")"; b="$(bytes_of "$w")"
    [ -n "$ir" ] || missing+=("$w")
    printf "%-26s %16s %9s\n" "$(label "$w")" "${ir:--}" "${b:--}"
done
echo
echo "Ir = instructions retired (Callgrind). Independent of CPU clock and OS"
echo "scheduling; depends only on the executed code, so it compares across machines."
echo
echo "The blob 1MB rows are where Ir/op earns its keep: it takes the machine's"
echo "memory subsystem out of a measurement that is otherwise bandwidth-bound."
echo "Read each of them against the same row on another port, though, and not"
echo "against each other: on x86-64 glibc, Callgrind counts every 'rep movsb'"
echo "iteration as an instruction, and glibc picks that memcpy for the one-shot"
echo "row's contiguous megabyte and a vector loop for the streaming row's 4 KB"
echo "pieces -- ~1.0 Ir/byte against ~0.11. That choice, not the divisible-run"
echo "path (CORELIB_PLAN 5.1), is nearly all of the gap between the two rows."

# A workload whose run produced no summary printed a dash above. That is a tool
# failure, not a measurement: the row is meant to run end to end and report a
# real number, and exiting 0 with a dash in the table is how a broken workload
# survives into a comparison.
if [ ${#missing[@]} -ne 0 ]; then
    echo >&2
    echo "error: no instruction count for: ${missing[*]}" >&2
    echo "       (see the Callgrind logs, or run the bench binary directly:" >&2
    echo "        $BIN ${missing[0]})" >&2
    exit 1
fi
