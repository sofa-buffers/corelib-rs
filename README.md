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
serialization format, tuned for throughput. The decoder advances a
Protocol-Buffers-style cursor over contiguous memory with zero copies, while
still supporting true chunked streaming on both sides. It is wire-compatible,
byte-for-byte, with every other `corelib-*` port.

> Need the embedded build instead? The sibling crate
> [`corelib-rs-no-std`](https://github.com/sofa-buffers/corelib-rs-no-std) is
> `#![no_std]`, heap-free and size-optimized for microcontrollers. **This** crate
> is the opposite trade-off: `std`, allocate freely, go fast. The public API
> mirrors the no_std crate, so code moves between them with at most a profile
> change. See [Choosing between the two Rust corelibs](#choosing-between-the-two-rust-corelibs).

### Requirements

**Rust 1.70 or newer**, edition 2021. Builds on `std` only — use
`corelib-rs-no-std` for embedded / `no_std`.

### Dependencies

None at runtime — only the standard library. The lone external crates are
dev-only (`libc` for the benchmark CPU clock, `serde_json` for the test vectors)
and are not pulled into downstream builds.

### Packaging

Published on crates.io as **`sofa-buffers-corelib`**; the importable namespace is
**`sofab`**:

```bash
cargo add sofa-buffers-corelib
```

```rust
use sofab::{OStream, decode};
```

## Why this design

| Goal | How |
|------|-----|
| Streaming **out** | `OStream` writes into a caller buffer and calls a `Flush` sink when it fills, so a message can exceed the buffer; `buffer_set` swaps the buffer mid-stream. |
| Streaming **in** | `IStream::feed` takes arbitrarily small chunks and suspends/resumes at any byte boundary; string/blob payloads are delivered incrementally. |
| Zero unnecessary copies | `decode` parses straight from the input buffer, handing string/blob fields back as borrowed slices; `feed` copies only bytes that straddle a chunk boundary. |
| Low allocation | Per-field encode/decode allocates nothing; the decoder dispatches into a monomorphized `Visitor` (no `dyn`, no boxing). |
| Raw speed | `unsafe` pointer-advancing varint decode, bulk `copy_from_slice`, native little-endian loads, `#[inline]`/`#[cold]` hot/error split, `opt-level = 3` + fat-LTO. |
| Type safety | Wire types and value widths live in the type system; array element widths are generic, so an invalid element size is unrepresentable. |
| Cross-language compatibility | The shared `assets/test_vectors.json` is replayed — the same bytes every other port produces. |

## Usage

### Encode

`OStream::new` wraps a caller-owned buffer. Each `write_*` returns `Result<()>`,
never allocates, and `bytes_used()` reports the byte count.

```rust
use sofab::OStream;

let mut buf = [0u8; 64];
let used = {
    let mut os = OStream::new(&mut buf);
    os.write_unsigned(1, 42).unwrap();
    os.write_signed(2, -7).unwrap();
    os.write_str(3, "hi").unwrap();
    os.bytes_used()
};
let message = &buf[..used];
```

### Decode

Decoding is **push-based**: implement `Visitor` and the decoder calls one method
per field. `decode` runs the zero-copy fast path over a complete message.

```rust
use sofab::{decode, Visitor, Id, Unsigned, Signed};

#[derive(Default)]
struct My { a: Unsigned, b: Signed, s: String }
impl Visitor for My {
    fn unsigned(&mut self, id: Id, v: Unsigned) { if id == 1 { self.a = v; } }
    fn signed(&mut self, id: Id, v: Signed)     { if id == 2 { self.b = v; } }
    fn string(&mut self, id: Id, _total: usize, _off: usize, c: &[u8]) {
        if id == 3 { self.s.push_str(std::str::from_utf8(c).unwrap()); }
    }
    // blob(), fp32(), fp64(), array_begin(), sequence_begin(), … as needed
}

let mut sink = My::default();
decode(message, &mut sink).unwrap();
assert_eq!((sink.a, sink.b, sink.s.as_str()), (42, -7, "hi"));
```

### Streaming out (message larger than the buffer)

Attach a `Flush` sink (any `FnMut(&[u8])`) with `OStream::with_flush`. When the
scratch buffer fills, its bytes drain to the sink and writing resumes at the
start; call `flush()` at the end to push the tail.

```rust
use sofab::OStream;

let mut scratch = [0u8; 16];
let mut out = Vec::new();                  // or a socket / file
{
    let mut os = OStream::with_flush(&mut scratch, 0, |chunk: &[u8]| {
        out.extend_from_slice(chunk);
    });
    for i in 0..1000u32 { os.write_unsigned(i, i as u64).unwrap(); }
    os.flush();
}
```

### Streaming in (IStream)

`IStream` feeds chunks of any size, suspending/resuming at any byte boundary and
driving the same `Visitor` as `decode`. `finish()` asserts a clean message
boundary; `reset()` reuses the decoder (and its carry allocation) for the next
message.

```rust
use sofab::{IStream, Visitor};

#[derive(Default)] struct Sink;
impl Visitor for Sink { /* override the callbacks you care about */ }

let mut sink = Sink::default();
let mut is = IStream::new();
for chunk in some_byte_stream.chunks(7) {  // 7 bytes at a time, or 1, or 64k
    is.feed(chunk, &mut sink).unwrap();
}
is.finish().unwrap();
# let some_byte_stream: Vec<u8> = Vec::new();
```

### Generated objects

Usually you never touch the raw API: the
[`generator`](https://github.com/sofa-buffers/generator) turns a schema into
typed objects with `serialize()` / `deserialize()` that also stream in chunks.
[`examples/person.rs`](examples/person.rs) is a hand-written stand-in showing the
generated layer builds purely from these primitives:

```bash
cargo run --example person
```

## Memory handling

The hot path is allocation-free and **never owns your payload memory** — you own
the buffers on both sides.

- **Encode (`OStream`):** you own the `&mut [u8]`; the library never allocates or
  grows it. With no sink, overflow is `Error::BufferFull`; with a `Flush` sink the
  buffer drains and is reused (`buffer_set` swaps a fresh one). To collect into a
  `Vec`, drive a small scratch buffer with an appending flush closure — *you* own
  the `Vec`.
- **Decode (`decode` / `IStream` + `Visitor`):** you own the input buffer and it
  must outlive the call. On the zero-copy `decode` fast path (and self-contained
  `feed` chunks) string/blob `&[u8]` chunks **borrow** directly from it, valid
  only during the callback — copy them out (`String::push_str`,
  `Vec::extend_from_slice`) to keep them. Scalars/floats arrive by value.

| Buffer | Owner / lifetime |
|--------|------------------|
| **Output buffer** | Caller-owned `&mut [u8]`; library never allocates or grows it. |
| **Input buffer** | Caller-owned; must outlive the call; string/blob slices borrow from it during the callback. |

This is a **push / visitor** model: values are handed to your `Visitor` as they
are parsed, so there is no address-stability requirement. The only memory the
decoder owns is `IStream`'s small internal carry `Vec` — the few bytes of an item
that straddled a chunk boundary.

## Feature flags

**None — always the full format.**

## Build & test

```bash
cargo build --release            # opt-level 3, fat LTO
cargo test                       # unit + integration + doctests (incl. shared vectors)
./coverage.sh                    # llvm-cov: summary + HTML + lcov.info
```

CI runs fmt + clippy (`-D warnings`), the full suite on **stable** and **beta**,
the same suite on a **big-endian** s390x host under QEMU, and llvm-cov coverage.
Integration tests live in `tests/` (shared-vector replay, fast-path decode,
encoder/decoder byte-exact checks, round-trip, and malformed-input errors).

## Benchmarks

Two `cargo bench` tools mirror the other ports' `perf` and `bench` tooling — same
workloads (a 1000-element `u64` array and a mixed message) and output format, so
results are comparable across languages:

```bash
cargo bench --bench perf         # cycles/op + CPU ns/op + throughput
cargo bench --bench bench         # practical MB/s (encode + decode)
RUSTFLAGS="-C target-cpu=native" cargo bench   # last few percent
```

## Choosing between the two Rust corelibs

SofaBuffers ships **two** Rust corelibs with the same API and the same wire
format, each built for its target:

- **`corelib-rs` (this crate)** — `std`, heap-using, `opt-level = 3` + fat LTO.
  For desktop and server workloads where throughput is the goal. Golden reference
  for the benchmark tools; roughly **1.4× prost throughput** in the arena.
- **[`corelib-rs-no-std`](https://github.com/sofa-buffers/corelib-rs-no-std)** —
  `#![no_std]`, no allocator, `opt-level = "z"` + LTO, size-tuned for bare-metal
  firmware. About **1.13× micropb throughput** at a Cortex-M flash footprint of
  roughly **6.0 KB vs ~8.5 KB**.

| | `corelib-rs` (this crate) | `corelib-rs-no-std` |
|---|---|---|
| Target | desktop / server | microcontroller / firmware |
| `std` / allocator | requires `std`, uses the heap | `#![no_std]`, no allocation |
| Release profile | `opt-level = 3`, fat LTO | `opt-level = "z"`, LTO |
| Optimized for | raw throughput | small `.text` footprint |
| Configurable format | no — always full | Cargo features trim wire types / value width |
| Arena reference | ~1.4× prost | ~1.13× micropb; Cortex-M ~6.0 KB vs ~8.5 KB |

Pick **this crate** for servers and throughput; pick **`corelib-rs-no-std`** for
embedded and footprint. The public API mirrors between them, so moving code across
is at most a profile change. Arena figures are approximate (best-of-5, comparable
only within a language).
