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
cargo add sofa-buffers-corelib   # the crates.io package name…
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

### Write operations

**Encoder — [`OStream`]** (writes into a caller buffer). Field ids are `u32` in
`0..=ID_MAX` (`i32::MAX`); every writer returns `Result<()>` and never allocates:

| Operation | Signature (value parameter) | Purpose |
|-----------|------------------------------|---------|
| `new` / `with_offset` / `with_flush` | `&mut [u8]` (`+ offset`, `+ sink`) | construct over a buffer; reserve a header offset; attach a flush sink |
| `write_unsigned` / `write_signed` / `write_boolean` | `Unsigned` (`u64`) / `Signed` (`i64`) / `bool` | scalar integers (varint / zig-zag) and booleans |
| `write_fp32` / `write_fp64` | `f32` / `f64` | little-endian IEEE-754 floats |
| `write_str` / `write_blob` | `&str` / `&[u8]` | UTF-8 text (no NUL on the wire) / raw bytes |
| `write_fixlen` | `&[u8]`, [`FixlenType`] | low-level fixed-length write (the primitive the four above build on) |
| `write_array_unsigned` / `write_array_signed` | `&[T: UnsignedElem]` / `&[T: SignedElem]` | integer arrays — element type generic (see [Allowed types](#allowed-types)) |
| `write_array_fp32` / `write_array_fp64` | `&[f32]` / `&[f64]` | float arrays with a single shared descriptor |
| `write_sequence_begin` / `write_sequence_end` | `Id` / — | open / close a nested sequence |
| `flush` / `buffer_set` / `bytes_used` | — / `&mut [u8]` / — | drain pending bytes; swap the output buffer mid-stream; bytes written |

Empty integer/float arrays are rejected (`Error::Argument`); empty strings/blobs
are valid.

### Read operations

Decoding is **push-based**: there is no `read_xxx()` that returns a value.
Instead you implement [`Visitor`] and the decoder calls one method per decoded
field. Two entry points drive the same `Visitor`:

| Operation | Purpose |
|-----------|---------|
| `decode(bytes, visitor)` | one-shot, zero-copy decode of a complete message |
| `IStream::new` / `feed(chunk, visitor)` / `finish` / `reset` | streaming decode: feed any-size chunks; assert a clean end; reuse the decoder |

Every value reaches the caller through one of these `Visitor` callbacks (all have
a default empty body, so overriding only the ones you care about **skips** the
rest — the equivalent of the C decoder's auto-skip):

| Callback | Destination type handed to you | Delivers |
|----------|--------------------------------|----------|
| `unsigned(id, value)` | `Unsigned` (`u64`), by value | an unsigned scalar **or** an unsigned-array element |
| `signed(id, value)` | `Signed` (`i64`), by value | a signed scalar **or** a signed-array element |
| `fp32(id, value)` | `f32`, by value | an `fp32` scalar **or** an `fp32`-array element |
| `fp64(id, value)` | `f64`, by value | an `fp64` scalar **or** an `fp64`-array element |
| `string(id, total, offset, chunk)` | `chunk: &[u8]`, **borrowed** | a slice of a string field (raw bytes; not validated as UTF-8 by the library) |
| `blob(id, total, offset, chunk)` | `chunk: &[u8]`, **borrowed** | a slice of a blob field |
| `array_begin(id, kind, count)` | [`ArrayKind`], `count: usize` | the header of an array; its `count` elements then arrive via the scalar/float callbacks above, all with the same `id` |
| `sequence_begin(id)` / `sequence_end()` | `Id` / — | nested-sequence framing (open / close) |

`total` is the full field length and `offset` is this chunk's byte position
within the field; on the contiguous [`decode`] path a string/blob always arrives
in a **single** call (`offset == 0`, `chunk.len() == total`). There is no
distinct "skip" call — a field whose callback is left at the default is simply
not delivered.

### Allowed types

The scalar API is fixed-width: `write_unsigned`/`Visitor::unsigned` are always
`u64` and `write_signed`/`Visitor::signed` always `i64` (`Unsigned` / `Signed`);
this build does not parameterize the scalar width. Floats are `f32` / `f64`.

Only the **integer-array writers are generic**, over the element width:

- `write_array_unsigned<T: UnsignedElem>` — `T ∈ {u8, u16, u32, u64}`
- `write_array_signed<T: SignedElem>` — `T ∈ {i8, i16, i32, i64}`

These are the only impls of the sealed-by-construction `UnsignedElem` /
`SignedElem` traits, so any other element type is a compile error. Elements are
zero-/sign-extended to 64-bit on the wire, so the *decode* side always reports
array elements as `u64` / `i64` (the original narrower width is not carried).
Float arrays are not generic: `write_array_fp32` / `write_array_fp64` take
`&[f32]` / `&[f64]`.

Fixed-length fields ([`FixlenType`]) are `Fp32`, `Fp64`, `Str`, `Blob`. A
**fixlen array may only hold `Fp32` or `Fp64` elements** — a `Str`/`Blob`
element width is rejected as `Error::InvalidMsg` on decode (variable-length
subtypes are not representable as fixed-stride array elements; use a nested
sequence of string/blob fields instead). Array element counts and fixlen byte
lengths are capped at `i32::MAX`.

### Memory handling

The high-speed `std` build allocates freely for *speed*, but the encode/decode
hot path is deliberately allocation-free and **never owns your payload memory**:

| Path | Who owns the buffer | Copy vs. borrow |
|------|---------------------|-----------------|
| **Encode** | The **caller** owns the output buffer (`&mut [u8]`). The library never allocates or grows it. | Bytes are written straight into your buffer. With no flush sink, overflow is `Error::BufferFull`; with a [`Flush`] sink the full buffer is drained to the sink and writing resumes at the start (`buffer_set` can even swap in a fresh buffer mid-stream). To collect into a growable `Vec`, drive a small scratch buffer with a flush closure that appends — *you* own the `Vec`. |
| **Decode — scalars/floats** | n/a (passed by value) | `unsigned`/`signed`/`fp32`/`fp64` receive a copied value; nothing is retained by the decoder. |
| **Decode — string/blob** | The **caller's `Visitor`** owns any retained bytes. The library allocates **no** `String`/`Vec` for payloads. | The `chunk: &[u8]` is a **borrow**, valid only for the duration of the callback. On the [`decode`] / self-contained-chunk fast path it borrows directly from your input buffer (zero copy); across a `feed` chunk boundary it borrows from a small internal carry buffer. If you want to keep the data, **copy it out inside the callback** (e.g. `String::push_str`, `Vec::extend_from_slice`). |
| **Decode — arrays/sequences** | n/a | Array elements stream through the scalar/float callbacks one at a time; the decoder holds only `O(1)` resume state, never a materialized array. |

This is a **push / visitor** model, **not** lazy binding: the decoder hands each
value to your `Visitor` as it is parsed, rather than recording a destination
pointer to be filled by a later `feed()`. Consequently there is no
address-stability requirement on any destination beyond the `&mut Visitor` you
pass in (which must, of course, outlive the `decode` / `feed` call). The only
memory the decoder itself owns is `IStream`'s internal carry `Vec`, which holds
just the few bytes of a small item (header / varint / float) that straddled a
chunk boundary; long string/blob payloads are streamed, never buffered, and
`reset` reuses the carry allocation across messages.

## Feature flags

This is the **high-speed `std` build**: every wire type is always compiled in and
the scalar value type is always 64-bit (`u64`/`i64`), so it never trades wire-type
granularity or value range for footprint. There are therefore **no Cargo feature
flags** to set — the toggle set described in the spec (§5.3) lives in the
trimmable, `#![no_std]` sibling crate instead.

| Feature flag | Default | Effect |
|--------------|---------|--------|
| *(none)* | — | All wire types (unsigned/signed integers, fp32, fp64, string, blob, integer arrays, float arrays, nested sequences) are always on; the value type is always 64-bit. |

```toml
sofa-buffers-corelib = "0.1"   # nothing to configure (import as `use sofab::…`)
```

For the trimmable build — drop fixlen / fp64 / array / sequence support, switch to
a 32-bit value type, or disable overflow checks to shrink the footprint for
constrained targets — use
[`corelib-rs-no-std`](https://github.com/sofa-buffers/corelib-rs-no-std), whose
Cargo features cover those toggles.

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
`bench` tools — so the numbers reflect the two implementations, not the
benchmark. Crucially, each is built **the way it is meant to ship**, which is
the comparison that actually matters:

- **`corelib-rs` — tuned for raw speed:** `opt-level = 3`, fat LTO,
  `codegen-units = 1`.
- **`corelib-rs-no-std` — full features, tuned for a small `.text`:**
  `opt-level = "z"`, LTO, `codegen-units = 1` (its release profile).

So this is a **speed-optimized vs size-optimized** comparison, by design.
Median of 15 runs on a single 6-core x86-64 VM (median is robust to the VM's
run-to-run jitter); `cycles/op` lower is better, MB/s higher is better.

| Workload | `std` cycles/op | `no_std` cycles/op | `std` MB/s | `no_std` MB/s | `std` faster |
| --- | ---: | ---: | ---: | ---: | ---: |
| serialize — typical message (170 B)   |  3,178 |   4,835 | 149.5 |  98.3 | 1.5× |
| deserialize — typical message (170 B) |  3,600 |   5,636 | 132.2 |  84.3 | 1.6× |
| encode — `u64` array ×1000 (9,491 B)  | 39,614 |  91,272 | 670.7 | 290.6 | 2.3× |
| decode — `u64` array ×1000 (9,491 B)  | 32,152 | 178,368 | 825.1 | 148.7 | 5.5× |

**In plain terms:** the speed-tuned `std` build is faster on every workload, and
the lead **grows with payload size** — about 1.5× on a small typical message,
2.3× encoding a 1000-element `u64` array, and up to **5.5×** decoding one. That
is exactly the intended trade-off: `corelib-rs` spends code size to go fast,
while `corelib-rs-no-std` gives up that throughput for a tiny, allocator-free
binary that runs on microcontrollers where `std` cannot build at all. Pick this
crate for servers and throughput; pick `no_std` for embedded and footprint.
