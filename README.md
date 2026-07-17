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

### String validity (strict UTF-8)

A `string` field is UTF-8. Rust's `str`/`String` is a **Unicode string type**,
so this port is **always strict** — the `SOFAB_STRICT_UTF8` option
(CORELIB_PLAN §6.4) is a **no-op here, pinned ON**, and there is no primitive to
expose (only byte-container targets need one):

- **Encode is strict by construction.** `OStream::write_str` takes `&str`, which
  is already guaranteed valid UTF-8 by the type system, so a `string` field can
  never carry invalid bytes — no runtime check is possible or needed. Put
  arbitrary bytes in a `blob` (`write_blob`).
- **Decode strictness lives in generated code.** The corelib hands a `string`
  field's **raw bytes** to `Visitor::string` and never builds a `String`;
  generated code materializes it with `core::str::from_utf8`, turning invalid
  bytes into `Error::InvalidMsg` (the `INVALID` decode outcome). Invalid UTF-8 is
  **rejected, never replaced** with `U+FFFD` or truncated. Embedded `U+0000` is
  valid UTF-8 and round-trips byte-exact. This matches `corelib-rs-no-std`
  exactly (subsumes generator #80).

The shared `invalid_utf8` negative vectors in `assets/test_vectors.json`
(tracking corelib-c-cpp#97) are exercised by `tests/utf8_tests.rs`.

## Usage

The codec has four use cases — serialize a message that fits in one buffer,
serialize one too large for the buffer (streamed out in chunks), deserialize a
whole message, and deserialize one arriving in chunks — plus the generated-code
path that wraps them.

### Serialize

`OStream::new` wraps a caller-owned buffer big enough for the whole message. Each
`write_*` returns `Result<()>`, never allocates, and `bytes_used()` reports the
byte count:

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

### Serialize stream

Attach a `Flush` sink (any `FnMut(&[u8])`) with `OStream::with_flush`. When the
scratch buffer fills, its bytes drain to the sink and writing resumes at the start;
`flush()` pushes the tail — so the message can far exceed the buffer:

```rust
use sofab::OStream;

let mut scratch = [0u8; 16];
let mut out = Vec::new();                  // or a socket / file
{
    let mut os = OStream::with_flush(&mut scratch, 0, |chunk: &[u8]| {
        out.extend_from_slice(chunk);
    });
    for i in 0..1000u32 { os.write_unsigned(i, i as u64).unwrap(); }
    os.flush();                            // push the tail
}
```

### Deserialize

Decoding is **push-based**: implement `Visitor` and the decoder calls one method
per field. `decode` runs the zero-copy fast path over a complete message:

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

### Deserialize stream

`IStream::feed` takes chunks of any size, suspends/resumes at any byte boundary,
and drives the same `Visitor` — so it decodes whatever the transport hands you,
wherever it comes from. It reports one of three outcomes for the bytes seen so
far (MESSAGE_SPEC §7), with **no separate finalize step**: `Ok(())` when the
stream ends exactly at a field boundary, `Err(Error::Incomplete)` when a chunk
ends mid-field (feed more — this is not an error), and `Err(Error::InvalidMsg)`
for malformed bytes. There is no separate finalize step; the caller owns
end-of-input, so a truncated tail is `Incomplete`, never promoted to a rejection.
To probe whether the stream ended cleanly, feed an empty chunk: `feed(&[], …)`
returns `Ok(())` iff the last byte landed on a field boundary:

```rust
use sofab::{Error, IStream, Visitor};

#[derive(Default)] struct Sink;
impl Visitor for Sink { /* override the callbacks you care about */ }

let some_byte_stream: Vec<u8> = Vec::new();  // bytes from your transport

let mut sink = Sink::default();
let mut is = IStream::new();
for chunk in some_byte_stream.chunks(7) {  // 7 bytes at a time, or 1, or 64k
    match is.feed(chunk, &mut sink) {
        Ok(()) | Err(Error::Incomplete) => {}   // more may follow
        Err(e) => panic!("malformed: {e}"),
    }
}
is.feed(&[], &mut sink).unwrap();  // Ok only if the stream ended at a clean boundary
```

### Code generator

Usually you never touch the raw API: the
[`generator`](https://github.com/sofa-buffers/generator) (sofabgen) turns a schema
into typed structs with `marshal()` (stream out through any `OStream`, sparse:
fields at their default are omitted), one-shot `encode()`, best-effort `decode()`
and error-checked `try_decode()`. A hand-written stand-in in exactly that shape
(real output decodes through a private `Visitor` module), encoded then decoded:

```rust
use sofab::{OStream, IStream, Visitor, Id, Signed};

// generated by: sofabgen --lang rust --in point.yaml --out src/
#[derive(Default)]
struct Point { x: i32, y: i32 }

impl Point {
    const MAX_SIZE: usize = 32;
    fn marshal(&self, os: &mut OStream) {
        let _ = os.write_signed(1, self.x as Signed);
        let _ = os.write_signed(2, self.y as Signed);
    }
    fn encode(&self) -> Vec<u8> {
        let mut buf = vec![0u8; Self::MAX_SIZE];
        let used = { let mut os = OStream::new(&mut buf); self.marshal(&mut os); os.bytes_used() };
        buf.truncate(used);
        buf
    }
    fn decode(data: &[u8]) -> Self {          // best-effort; unknown fields are skipped
        let mut m = Self::default();
        let _ = IStream::new().feed(data, &mut m);
        m
    }
    fn try_decode(data: &[u8]) -> Result<Self, sofab::Error> {  // error-checked
        let mut m = Self::default();
        IStream::new().feed(data, &mut m)?;
        Ok(m)
    }
}

impl Visitor for Point {
    fn signed(&mut self, id: Id, v: Signed) { match id { 1 => self.x = v as i32, 2 => self.y = v as i32, _ => {} } }
}

let wire = Point { x: 3, y: 4 }.encode();
let got = Point::try_decode(&wire).unwrap();   // got.x == 3, got.y == 4
```

Streaming out a generated object through a small buffer is the same `marshal()`
over an `OStream::with_flush` sink (see [Serialize stream](#serialize-stream));
streaming *in* goes through the raw `IStream::feed` path shown above.

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

Three tools mirror the other ports' `perf`, `bench` and `run_callgrind.sh`
tooling — same workloads (a 1000-element `u64` array and a mixed message) and
output format, so results are comparable across languages:

```bash
cargo bench --bench perf          # cycles/op + CPU ns/op + throughput
cargo bench --bench bench         # practical MB/s (encode + decode)
bash benches/run_callgrind.sh     # instructions/op (Callgrind Ir), machine-independent
RUSTFLAGS="-C target-cpu=native" cargo bench   # last few percent
```

`perf` reports per-op cost from the hardware cycle counter plus throughput;
`bench` is the CPU-time MB/s speedtest; `run_callgrind.sh` (requires
`valgrind`) counts instructions retired per operation — deterministic and
independent of clock speed or scheduling, so the numbers compare across
machines.

## Choosing between the two Rust corelibs

SofaBuffers ships **two** Rust corelibs with the same API and the same wire
format, each built for its target:

- **`corelib-rs` (this crate)** — `std`, heap-using, `opt-level = 3` + fat LTO.
  For desktop and server workloads where throughput is the goal. Golden reference
  for the benchmark tools; roughly **1.4× prost throughput** in the arena.
- **[`corelib-rs-no-std`](https://github.com/sofa-buffers/corelib-rs-no-std)** —
  `#![no_std]`, no allocator, `opt-level = "z"` + LTO, size-tuned for bare-metal
  firmware. About **1.3× micropb throughput** at a Cortex-M flash footprint of
  roughly **7.0 KB vs ~8.5 KB**.

| | `corelib-rs` (this crate) | `corelib-rs-no-std` |
|---|---|---|
| Target | desktop / server | microcontroller / firmware |
| `std` / allocator | requires `std`, uses the heap | `#![no_std]`, no allocation |
| Release profile | `opt-level = 3`, fat LTO | `opt-level = "z"`, LTO |
| Optimized for | raw throughput | small `.text` footprint |
| Configurable format | no — always full | Cargo features trim wire types / value width |
| Arena reference | ~1.4× prost | ~1.3× micropb; Cortex-M ~7.0 KB vs ~8.5 KB |

Pick **this crate** for servers and throughput; pick **`corelib-rs-no-std`** for
embedded and footprint. The public API mirrors between them, so moving code across
is at most a profile change. Arena figures are approximate (best-of-5, comparable
only within a language).
