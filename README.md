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
> public API mirrors the no_std crate, so code moves between them with at most a
> profile change. See [Choosing between the two Rust corelibs](#choosing-between-the-two-rust-corelibs).

### Requirements

- **Rust 1.70 or newer** (crate `rust-version`), Rust **edition 2021**.
- Builds on `std`. No embedded / `no_std` target is supported here — use
  `corelib-rs-no-std` for that.

### Dependencies

**None at runtime** — the library depends only on the standard library. The only
external crates are dev-only: `libc` (process CPU clock for the benchmarks) and
`serde_json` (parsing the shared cross-language test vectors). Neither is pulled
into a build that merely depends on this crate.

### Package name

The crate is published on crates.io as **`sofa-buffers-corelib`**, but the
importable namespace is **`sofab`**:

```bash
cargo add sofa-buffers-corelib   # the crates.io package name…
```

```rust
use sofab::{OStream, decode};    // …the importable namespace is `sofab`
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
| Streaming **out** | `OStream` writes into a caller buffer and calls a `Flush` sink whenever it fills, so a message can far exceed the buffer; `buffer_set` swaps the buffer mid-stream. |
| Streaming **in** | `IStream::feed` takes arbitrarily small chunks and suspends/resumes at *any* byte boundary; string/blob payloads are delivered incrementally so they too can exceed RAM. |
| Zero unnecessary copies | The one-shot `decode` path parses straight from the input buffer and hands string/blob fields back as **borrowed slices** (no copy). `feed` only ever copies the few bytes of a field that genuinely straddles a chunk boundary. |
| Low allocation on the hot path | Per-field encode/decode allocates nothing; the encoder writes into a caller buffer, and the decoder dispatches into a monomorphized `Visitor` (no `dyn`, no boxing). |
| Raw speed | `unsafe` pointer-advancing varint decode with an unchecked fast region, bulk `copy_from_slice`, native little-endian loads, `#[inline]` hot path / `#[cold]` error path, and an `opt-level = 3` + fat-LTO release profile. |
| Type safety | Wire types and value widths are encoded in the Rust type system; array element widths are generic, so an invalid element size is unrepresentable. |
| Cross-language compatibility | The shared `assets/test_vectors.json` is replayed by the test suite — the same bytes every other port produces. |

## Usage

Full API docs are published to **GitHub Pages** on every push to `main` (the
**Docs** badge above): <https://sofa-buffers.github.io/corelib-rs/>.

### Simple encode

`OStream::new` wraps a caller-owned buffer. Each typed `write_*` returns
`Result<()>` and never allocates; `bytes_used()` reports how many bytes were
written.

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

### Simple decode

Decoding is **push-based**: you implement `Visitor` and the decoder calls one
method per field. `decode` runs the zero-copy fast path over a complete message.

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

### Streaming a message larger than the buffer

Attach a `Flush` sink (any `FnMut(&[u8])`) with `OStream::with_flush`. When the
scratch buffer fills, its bytes are drained to the sink and writing resumes at
the start — so the message can be arbitrarily larger than the buffer. Call
`flush()` at the end to push the tail.

```rust
use sofab::OStream;

let mut scratch = [0u8; 16];              // tiny buffer
let mut out = Vec::new();                  // or a socket / file
{
    let mut os = OStream::with_flush(&mut scratch, 0, |chunk: &[u8]| {
        out.extend_from_slice(chunk);
    });
    for i in 0..1000u32 { os.write_unsigned(i, i as u64).unwrap(); }
    os.flush();                            // push whatever is still buffered
}
```

### OStream — the streaming output primitive

`OStream<'a, F: Flush>` **is** the encoder; it always writes into a
caller-provided `&mut [u8]`. Three constructors cover the cases:

- `OStream::new(buf)` — no sink; overflow is `Error::BufferFull`.
- `OStream::with_offset(buf, n)` — reserve `n` header bytes for a lower layer.
- `OStream::with_flush(buf, offset, sink)` — attach a drain sink for streaming.

`buffer_set(new_buf, offset)` (typically called from inside the sink) swaps the
active buffer mid-stream, and `flush()` drains any pending bytes. `Flush` is
implemented for every `FnMut(&[u8])` closure, and `NoFlush` is the default
zero-sized sink.

### IStream — the streaming input primitive

`IStream` is the pull decoder: feed it chunks of *any* size and it
suspends/resumes at any byte boundary, driving the same `Visitor` as `decode`.
`finish()` asserts a clean message boundary (no half-read field, no open
sequence); `reset()` reuses the decoder — and its carry allocation — for the
next message.

```rust
use sofab::{IStream, Visitor};

#[derive(Default)] struct Sink;
impl Visitor for Sink { /* override the callbacks you care about */ }

let mut sink = Sink::default();
let mut is = IStream::new();
for chunk in some_byte_stream.chunks(7) {  // 7 bytes at a time, or 1, or 64k
    is.feed(chunk, &mut sink).unwrap();
}
is.finish().unwrap();                       // clean message boundary
# let some_byte_stream: Vec<u8> = Vec::new();
```

### Generated objects

In the common case you never touch the raw API: the
[`generator`](https://github.com/sofa-buffers/generator) turns a schema into
plain typed objects with a dead-simple `serialize()` / `deserialize()` — that
also stream in chunks. [`examples/person.rs`](examples/person.rs) is a
hand-written stand-in showing the generated layer is buildable purely from these
primitives (the generated `serialize()` drives an `OStream` over a small scratch
buffer with a `Vec`-appending flush sink; `deserialize()` runs `decode` into a
field-assembling `Visitor`):

```bash
cargo run --example person
```

## API summary

### Encoding API

The encoder is `OStream`. All writers follow one shape — `write_<kind>(id,
value) -> Result<()>` — and never allocate:

- **Scalars:** `write_unsigned` (`u64`, varint), `write_signed` (`i64`,
  zig-zag varint), `write_boolean`, `write_fp32` / `write_fp64` (little-endian
  IEEE-754).
- **Length-delimited:** `write_str` (`&str`, UTF-8, no NUL on the wire),
  `write_blob` (`&[u8]`). `write_fixlen` is the low-level primitive the four
  float/text writers build on.
- **Arrays:** `write_array_unsigned` / `write_array_signed` are generic over the
  element width; `write_array_fp32` / `write_array_fp64` take float slices. A
  **zero-count array is valid** and encodes as an empty array on the wire.
- **Nested sequences:** `write_sequence_begin(id)` / `write_sequence_end()`,
  balanced and capped at `MAX_DEPTH` (255).

Field ids are `u32` in `0..=ID_MAX` (`i32::MAX`); an out-of-range id, an
over-`MAX_DEPTH` sequence, or a length/count above `i32::MAX` returns
`Error::Argument`. The scalar API is fixed-width — always `u64`/`i64` and
`f32`/`f64`; this build does not parameterize the scalar width. Only the integer
arrays are generic, over the sealed-by-construction `UnsignedElem`
(`u8`/`u16`/`u32`/`u64`) and `SignedElem` (`i8`/`i16`/`i32`/`i64`) traits, so any
other element type is a compile error. Narrow elements are zero-/sign-extended to
64-bit on the wire, so the decode side always reports array elements as
`u64`/`i64` (the original width is not carried). A fixlen array may hold only
`Fp32`/`Fp64` elements; a `Str`/`Blob` element width is rejected on decode
(`Error::InvalidMsg`) — use a nested sequence of string/blob fields instead.

### Decoding API

Decoding is **push / visitor** based — there is no `read_xxx()` that returns a
value. You implement `Visitor` (every method has a default no-op body, so
overriding only the ones you care about **auto-skips** the rest) and drive it
through one of two entry points onto the **same** visitor:

- `decode(bytes, &mut visitor)` — one-shot, zero-copy decode of a complete,
  contiguous message.
- `IStream::new()` + `feed(chunk, &mut visitor)` + `finish()` (+ `reset()`) —
  streaming decode of any-size chunks, with a clean-boundary assertion and reuse.

The callbacks are `unsigned` / `signed` / `fp32` / `fp64` (by value),
`string` / `blob` (a **borrowed** `&[u8]` chunk, with `total` field length and
`offset` within the field), `array_begin(id, kind, count)` followed by the
element values through the scalar/float callbacks under the same `id`, and
`sequence_begin` / `sequence_end`. On the contiguous `decode` path a string/blob
always arrives in a **single** call (`offset == 0`, `chunk.len() == total`); over
`feed` it may be delivered in pieces.

### Memory handling

The high-speed `std` build allocates freely for *speed*, but the encode/decode
hot path is deliberately allocation-free and **never owns your payload memory**.

| Item | Owner / lifetime | Copy vs. borrow |
|------|------------------|-----------------|
| **Input buffer** | The **caller** owns it; it must outlive the `decode` / `feed` call. | Read in place. On the `decode` fast path and self-contained `feed` chunks, string/blob slices borrow directly from it (zero copy). |
| **Output buffer** | The **caller** owns the `&mut [u8]`; the library never allocates or grows it. | Written in place. With no sink, overflow is `Error::BufferFull`; with a `Flush` sink the buffer is drained and reused (`buffer_set` can swap in a fresh one). To collect into a growable `Vec`, drive a small scratch buffer with a flush closure that appends — *you* own the `Vec`. |
| **Message object** | Your **`Visitor`** owns whatever it retains. The library allocates **no** `String`/`Vec` for payloads. | Scalars/floats are passed by value. A `string`/`blob` `&[u8]` chunk is a **borrow valid only during the callback** — copy it out (`String::push_str`, `Vec::extend_from_slice`) to keep it. |

This is a **push / visitor** model, not lazy binding: the decoder hands each
value to your `Visitor` as it is parsed rather than recording a destination
pointer to be filled later, so there is no address-stability requirement beyond
the `&mut Visitor` outliving the call. The only memory the decoder itself owns is
`IStream`'s internal carry `Vec`, which holds just the few bytes of a small item
(header / varint / float) that straddled a chunk boundary; long string/blob
payloads are streamed, never buffered, and `reset` reuses the carry allocation
across messages.

## Feature flags

**No Cargo feature flags — always the full format.** Every wire type
(unsigned/signed integers, fp32, fp64, string, blob, integer arrays, float
arrays, nested sequences) is always compiled in, and the scalar value type is
always 64-bit (`u64`/`i64`). This is the high-speed build: it never trades
wire-type granularity or value range for footprint.

```toml
sofa-buffers-corelib = "0.1"   # nothing to configure (import as `use sofab::…`)
```

The trimmable toggles — drop fixlen / fp64 / array / sequence support, switch to
a 32-bit value type, or disable overflow checks to shrink the footprint — live in
the sibling [`corelib-rs-no-std`](https://github.com/sofa-buffers/corelib-rs-no-std)
crate, whose Cargo features cover them.

## Build & test

```bash
cargo build                      # debug
cargo build --release            # optimized (opt-level 3, fat LTO)
cargo test                       # unit + integration + doctests (incl. shared vectors)
./coverage.sh                    # llvm-cov: terminal summary + HTML + lcov.info
```

CI (`.github/workflows/ci.yml`) runs fmt + clippy (`-D warnings`), the full test
suite on **stable** and **beta**, the same suite on a **big-endian** s390x host
under QEMU (proving the little-endian wire format round-trips off little-endian
hardware), and llvm-cov line coverage (which publishes the coverage badge).

Tests live in `tests/` as separate integration files:

- `vectors_tests.rs` — replays the shared `assets/test_vectors.json` (encode,
  chunked-encode through 1/3/7-byte flush buffers, decode, chunked-decode, and
  `skip_ids` auto-skip).
- `reader_tests.rs` — the fast `decode` path: matches the streaming path on every
  shared vector, asserts zero-copy single-call string/blob delivery, and rejects
  truncated input.
- `ostream_tests.rs` — encoder, byte-exact vs. reference vectors.
- `istream_tests.rs` — decoder over the same vectors + malformed-input errors.
- `roundtrip_tests.rs` — encode → decode value preservation.
- `api_tests.rs` — offset reserve, buffer swap, large chunked streaming, API version.
- `config_tests.rs` — per-wire-type encode → decode smoke tests.
- `tests/common/mod.rs` — shared recording `Visitor`.

## Benchmarks

Two `cargo bench` tools mirror the cross-language benchmark suite
([`BENCH_SPEC.md`](https://github.com/sofa-buffers/documentation/blob/main/BENCH_SPEC.md))
and run the **same** reference workloads (a 1000-element `u64` array and a typical
composite message), printing the exact shared output grammar so results are
comparable across ports. This repo's tools are the **golden reference** for that
format.

`perf` — CPU-speed-independent per-operation cost: hardware cycles/op (x86 TSC via
`_rdtsc`, AArch64 virtual count register) plus CPU ns/op and throughput, over a
~1 s CPU-time loop:

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

## Choosing between the two Rust corelibs

SofaBuffers ships **two** Rust corelibs with the **same API** and the **same wire
format**, each built the way it is meant to ship:

- **`corelib-rs` (this crate)** — `std`, heap-using, `opt-level = 3` + fat LTO.
  For **desktop and server** workloads where throughput is the goal and an
  allocator is a given. It is the golden reference for the benchmark tools and,
  in the multi-language benchmark arena, runs at roughly **1.4× prost
  throughput**.
- **[`corelib-rs-no-std`](https://github.com/sofa-buffers/corelib-rs-no-std)** —
  `#![no_std]`, **no allocator**, `opt-level = "z"` + LTO, size-tuned for
  **bare-metal firmware** on microcontrollers where `std` cannot build at all. In
  the arena it runs at about **1.13× micropb throughput** at a bare-metal
  Cortex-M flash footprint of roughly **6.0 KB vs micropb's ~8.5 KB**.

Because both implement the identical API and run the identical `perf` / `bench`
tools, the choice is purely the deployment target — throughput vs footprint, not
features:

| | `corelib-rs` (this crate) | `corelib-rs-no-std` |
|---|---|---|
| Target | desktop / server | microcontroller / firmware |
| `std` / allocator | requires `std`, uses the heap | `#![no_std]`, **no** allocation |
| Release profile | `opt-level = 3`, fat LTO | `opt-level = "z"`, LTO (size-tuned) |
| Optimized for | raw throughput | small `.text` footprint |
| Configurable format | no — always full format | Cargo features trim wire types / value width |
| Arena reference | ~1.4× prost throughput | ~1.13× micropb throughput; Cortex-M flash ~6.0 KB vs ~8.5 KB |

Pick **this crate** for servers and throughput; pick **`corelib-rs-no-std`** for
embedded and footprint. Because the public API mirrors between them, moving code
across is at most a profile change. The head-to-head figures above come from the
multi-language benchmark arena (best-of-5, comparable only within a language) and
are approximate.
