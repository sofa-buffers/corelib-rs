<p align="center"><img src="assets/sofabuffers_logo.png" alt="SofaBuffers" height="140"></p>

# SofaBuffers

<b>Structured Objects For Anyone</b><br>
<i>... so optimized, feels amazing.</i>

[Would you like to know more?](https://github.com/sofa-buffers)

## SofaBuffers Rust library

[![CI](https://github.com/sofa-buffers/corelib-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/sofa-buffers/corelib-rs/actions/workflows/ci.yml)
[![Coverage](https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2Fsofa-buffers%2Fcorelib-rs%2Fbadges%2Fcoverage.json)](https://github.com/sofa-buffers/corelib-rs/actions/workflows/ci.yml)
[![Docs](https://img.shields.io/badge/docs-GitHub%20Pages-1f7feb)](https://sofa-buffers.github.io/corelib-rs/)

[GitHub repository](https://github.com/sofa-buffers/corelib-rs)

A **high-speed, streaming** Rust implementation of the SofaBuffers (*Sofab*)
serialization format, tuned for **throughput on big machines**. The decoder
**advances a cursor over a contiguous buffer** with zero copies — the technique
from the C++ high-speed port and Protocol Buffers — while still supporting true
chunked streaming on both sides. It is wire-compatible, byte-for-byte, with every
other `corelib-*` port.

> Need the embedded build instead? The sibling crate
> [`corelib-rs-no-std`](https://github.com/sofa-buffers/corelib-rs-no-std) is
> `#![no_std]`, heap-free and size-optimized for microcontrollers. **This** crate
> is the opposite trade-off: `std`, allocate freely, go as fast as possible. The
> public API is the same, so code moves between them with at most a profile change.

**Minimum Rust version:** 1.70. **Install:**

```bash
cargo add SofaBuffers        # the registry package name…
```

```rust
use sofab::{OStream, decode}; // …the importable namespace is `sofab`
```

The wire format is specified, language-neutrally, in the
[SofaBuffers documentation](https://github.com/sofa-buffers/documentation). For
byte-for-byte interoperability across every language port, the test suite replays
the **shared** cross-language test vectors
([`assets/test_vectors.json`](assets/test_vectors.json), copied verbatim from the
`corelib-c-cpp` repository — the single source of truth) and asserts the
encoder's output and the decoder's recovered fields match for all of them, on
both the fast and the streaming paths.

This library implements SofaBuffers **API version 1** (exposed as
`sofab::API_VERSION`).

## Why this design

| Goal | How |
|------|-----|
| Streaming **out** | [`OStream`] writes into a caller buffer and calls a [`Flush`] sink whenever it fills, so a message can far exceed the buffer; `buffer_set` swaps the buffer mid-stream. |
| Streaming **in** | [`IStream::feed`] takes arbitrarily small chunks and suspends/resumes at *any* byte boundary; string/blob payloads are delivered incrementally so they too can exceed RAM. |
| Zero unnecessary copies | The one-shot [`decode`] path parses straight from the input buffer and hands string/blob fields back as **borrowed slices** (no copy). `feed` only ever copies the few bytes of a field that genuinely straddles a chunk boundary. |
| Low allocation on the hot path | Per-field encode/decode allocates nothing; the encoder writes into a caller buffer, and the decoder dispatches into a monomorphized [`Visitor`] (no `dyn`, no boxing). |
| Raw speed | `unsafe` pointer-advancing varint decode with an unchecked fast region, bulk `copy_from_slice`, native little-endian loads, `#[inline]` hot path / `#[cold]` error path, and an `-O3` + fat-LTO release profile. |
| Type safety | Wire types and value widths are encoded in the Rust type system; array element widths are generic, so an invalid element size is unrepresentable. |
| Cross-language compatibility | The shared `assets/test_vectors.json` is replayed by the test suite — the same bytes every other port produces. |

## API documentation

Full API docs are published to **GitHub Pages** on every push to `main` (see the
**Docs** badge above): <https://sofa-buffers.github.io/corelib-rs/>.

## Usage

```rust
use sofab::{OStream, decode, Visitor, Id, Unsigned, Signed};

// ---- encode (into a caller buffer) ----
let mut buf = [0u8; 64];
let used = {
    let mut os = OStream::new(&mut buf);
    os.write_unsigned(1, 42).unwrap();
    os.write_signed(2, -7).unwrap();
    os.write_str(3, "hi").unwrap();
    os.bytes_used()
};

// ---- decode (one-shot, zero-copy: push to your Visitor) ----
#[derive(Default)]
struct My { a: Unsigned, b: Signed, s: String }
impl Visitor for My {
    fn unsigned(&mut self, id: Id, v: Unsigned) { if id == 1 { self.a = v; } }
    fn signed(&mut self, id: Id, v: Signed)     { if id == 2 { self.b = v; } }
    fn string(&mut self, id: Id, _total: usize, _off: usize, c: &[u8]) {
        if id == 3 { self.s.push_str(std::str::from_utf8(c).unwrap()); }
    }
    // blob(), fp32(), fp64(), array_begin(), sequence_begin(), ... as needed
}
let mut sink = My::default();
decode(&buf[..used], &mut sink).unwrap();
assert_eq!((sink.a, sink.b, sink.s.as_str()), (42, -7, "hi"));
```

### Streaming a message larger than the buffer

```rust
use sofab::OStream;
let mut scratch = [0u8; 16];                 // tiny buffer
let mut out = Vec::new();                     // or a socket / file
let mut os = OStream::with_flush(&mut scratch, 0, |chunk: &[u8]| out.extend_from_slice(chunk));
for i in 0..1000u32 { os.write_unsigned(i, i as u64).unwrap(); }
os.flush();                                   // push the tail
```

The decode side is symmetric — feed [`IStream`] chunks of any size:

```rust
use sofab::{IStream, Visitor};
# #[derive(Default)] struct Sink; impl Visitor for Sink {}
let mut sink = Sink::default();
let mut is = IStream::new();
for chunk in some_byte_stream().chunks(7) {   // 7 bytes at a time, or 1, or 64k
    is.feed(chunk, &mut sink).unwrap();
}
is.finish().unwrap();                          // assert a clean message boundary
# fn some_byte_stream() -> Vec<u8> { vec![] }
```

### Generated objects

In the common case you never touch the raw API: the
[`generator`](https://github.com/sofa-buffers/generator) turns a schema into
plain typed objects with a dead-simple `serialize()` / `deserialize()` — that
also stream in chunks. [`examples/person.rs`](examples/person.rs) is a hand-written
stand-in showing the generated layer is buildable purely from these primitives:

```bash
cargo run --example person
```

## API summary

**Encoder — [`OStream`]** (writes into a caller buffer):

| Operation | Purpose |
|-----------|---------|
| `new` / `with_offset` / `with_flush` | construct over a buffer; reserve a header offset; attach a flush sink |
| `write_unsigned` / `write_signed` / `write_boolean` | scalar integers (varint / zig-zag) and booleans |
| `write_fp32` / `write_fp64` / `write_str` / `write_blob` / `write_fixlen` | fixed-length values (LE floats, UTF-8 text, raw bytes) |
| `write_array_unsigned` / `write_array_signed` / `write_array_fp32` / `write_array_fp64` | arrays with a single shared descriptor |
| `write_sequence_begin` / `write_sequence_end` | open / close a nested sequence |
| `flush` / `buffer_set` / `bytes_used` | drain pending bytes; swap the output buffer mid-stream; bytes written |

**Decoder — [`decode`] / [`IStream`] + [`Visitor`]:**

| Operation | Purpose |
|-----------|---------|
| `decode(bytes, visitor)` | one-shot, zero-copy decode of a complete message |
| `IStream::new` / `feed` / `finish` / `reset` | streaming decode: feed any-size chunks; assert a clean end; reuse the decoder |
| `Visitor::unsigned` / `signed` / `fp32` / `fp64` | scalar fields and array elements |
| `Visitor::string` / `blob` | fixed-length payloads (`total` / `offset` / `chunk`; one call on the fast path) |
| `Visitor::array_begin` | start of an array (`kind`, `count`); elements follow via the scalar/float callbacks |
| `Visitor::sequence_begin` / `sequence_end` | nested-sequence framing |

A `Visitor` method left at its default (empty) implementation transparently skips
that field — the equivalent of the C decoder's auto-skip.

## No build-time configuration

There are **no Cargo feature flags**. Every wire type — unsigned/signed
integers, fp32, fp64, string, blob, integer arrays, float arrays, and nested
sequences — is always compiled in, and the scalar value type is always 64-bit
(`u64`/`i64`). This is the high-speed build: it does not trade wire-type
granularity or value range for footprint. (If you need the trimmable, 32-bit,
`#![no_std]` build, use [`corelib-rs-no-std`](https://github.com/sofa-buffers/corelib-rs-no-std).)

```toml
SofaBuffers = "0.1"   # nothing to configure
```

## Build & test

```bash
cargo build                      # debug
cargo build --release            # optimized
cargo test                       # unit + integration + doctests (incl. shared vectors)
./coverage.sh                    # llvm-cov: terminal summary + HTML + lcov.info
```

Tests live in `tests/` as separate integration files:

- `vectors_tests.rs` — replays the shared `assets/test_vectors.json` (encode,
  chunked-encode through 1/3/7-byte flush buffers, decode, chunked-decode, and
  `skip_ids` auto-skip).
- `reader_tests.rs` — the fast [`decode`] path: matches the streaming path on
  every shared vector, asserts zero-copy single-call string/blob delivery, and
  rejects truncated input.
- `ostream_tests.rs` — encoder, byte-exact vs. reference vectors.
- `istream_tests.rs` — decoder over the same vectors + malformed-input errors.
- `roundtrip_tests.rs` — encode → decode value preservation.
- `api_tests.rs` — offset reserve, buffer swap, large chunked streaming, API version.
- `config_tests.rs` — per-wire-type encode → decode smoke tests.
- `tests/common/mod.rs` — shared recording [`Visitor`].

## Benchmarks

Two tools mirror the cross-language benchmark suite
([`BENCH_SPEC.md`](https://github.com/sofa-buffers/documentation/blob/main/BENCH_SPEC.md))
and run the **same** reference workloads (a 1000-element `u64` array and a
typical composite message), printing the exact shared output grammar so results
are comparable across ports. This repo's tools are the **golden reference** for
that format.

`perf` — CPU-speed-independent per-operation cost: hardware cycles/op (x86 TSC /
AArch64 counter) plus CPU ns/op and throughput, over a ~1 s CPU-time loop:

```bash
cargo bench --bench perf
```

`bench` — practical throughput in **MB/s** (MB = 1,000,000 bytes), against
process CPU time, for encode and decode of each workload:

```bash
cargo bench --bench bench
```

For the last few percent of throughput, build with native codegen:

```bash
RUSTFLAGS="-C target-cpu=native" cargo bench
```

### `std` vs `no_std`: how the two Rust ports compare

`corelib-rs` (this crate, built on `std`) and the freestanding
[`corelib-rs-no-std`](https://github.com/sofa-buffers/corelib-rs-no-std) port
implement the **same SofaBuffers API** and run the **identical** `perf` and
`bench` tools, so every difference below comes purely from the two encode/decode
implementations — not from the benchmark.

Average of 7 runs on a single 6-core x86-64 host (stable `rustc`, default
`[profile.bench]`, no `target-cpu=native`). Numbers are machine-specific;
`cycles/op` is the more host-independent figure (lower is better), MB/s is this
machine's throughput (higher is better).

| Workload | `std` cycles/op | `no_std` cycles/op | `std` MB/s | `no_std` MB/s |
| --- | ---: | ---: | ---: | ---: |
| serialize — typical message (170 B)   |  3,471 |  3,670 | 138.8 | 131.6 |
| deserialize — typical message (170 B) |  4,383 |  5,116 | 112.7 |  95.4 |
| encode — `u64` array ×1000 (9,491 B)  | 41,486 | 38,958 | 642.0 | 684.7 |
| decode — `u64` array ×1000 (9,491 B)  | 34,159 | 95,217 | 782.3 | 280.5 |

**In plain terms:** the two ports are close on most workloads — within a few
percent on the small typical message, and effectively tied on bulk `u64`-array
*encode* (`no_std` is even marginally ahead there). The one big, consistent gap
is bulk-array **decode**, where `std` is roughly **2.8×** faster (≈780 vs ≈280
MB/s). So for small mixed messages or array encoding the choice barely matters;
for high-volume array *decoding* the `std` crate is the clear pick, while
`no_std` trades that decode throughput for running in freestanding / embedded
targets that have no allocator or operating system.
