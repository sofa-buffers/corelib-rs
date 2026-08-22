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
> `#![no_std]`, heap-free and size-optimized for microcontrollers; the public API
> mirrors it, so code moves between them with at most a profile change. See
> [Choosing between the two Rust corelibs](#choosing-between-the-two-rust-corelibs).

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

### Feature flags

**None — always the full format.**

## Why this design

| Goal | How |
|------|-----|
| Streaming **out** | `OStream` writes into a caller buffer and hands it to a sink when it fills, so a message can exceed the buffer — down to a `MIN_OUTPUT_BUFFER` of **1 byte**. A `Flush` sink copies the bytes out; a `FlushTake` sink may instead *take* the buffer and return the replacement to write into next. |
| Streaming **in** | `IStream::feed` takes arbitrarily small chunks and suspends/resumes at any byte boundary; string/blob payloads are delivered incrementally. |
| Zero unnecessary copies | `decode` parses straight from the input buffer, handing string/blob fields back as borrowed slices; `feed` copies only bytes that straddle a chunk boundary. |
| Low allocation | Per-field encode/decode allocates nothing; the decoder dispatches into a monomorphized `Visitor` (no `dyn`, no boxing). The encoder's only heap use is the run of held-back sequence headers (see [Sequences](#sequences)), and even that stays inline until you nest more than 8 deep. |
| Raw speed | `unsafe` pointer-advancing varint decode, bulk `copy_from_slice`, native little-endian loads, `#[inline]`/`#[cold]` hot/error split, `opt-level = 3` + fat-LTO. No array falls back to the scalar path element by element: an integer array is written in runs of whole varints under one capacity check, and an `fp32`/`fp64` array — whose payload on a little-endian host *is* the slice's own memory — as a single bulk run. |
| Type safety | Wire types and value widths live in the type system; array element widths are generic, so an invalid element size is unrepresentable. |
| Cross-language compatibility | The shared `assets/test_vectors.json` is replayed — the same bytes every other port produces. |

### String validity (strict UTF-8)

A `string` field is UTF-8. Rust's `str`/`String` is a **Unicode string type**,
so this port is **always strict** — the `SOFAB_STRICT_UTF8` option
(CORELIB_PLAN §6.4) is a **no-op here, pinned ON**, and there is no primitive to
expose (only byte-container targets need one):

- **Encode.** The typed writers `write_str`, `write_blob`, `write_fp32` and
  `write_fp64` are correct by construction. The byte-level
  `OStream::write_fixlen` takes arbitrary bytes and validates them against the
  **subtype** it is given, not just against the length ceiling: `Str` requires
  valid UTF-8, `Fp32`/`Fp64` exactly 4 / 8 payload bytes (§4.6). Anything else is
  `Error::Argument` (§6.3's `InvalidArgument`) with nothing written. Put
  arbitrary bytes in a `blob` (`write_blob`, or `write_fixlen` with
  `FixlenType::Blob`), which is unconstrained.
- **Decode strictness lives in generated code.** The corelib hands a `string`
  field's **raw bytes** to `Visitor::string` and never builds a `String`;
  generated code materializes it with `core::str::from_utf8`, turning invalid
  bytes into `Error::InvalidMsg` (the `INVALID` decode outcome). Invalid UTF-8 is
  **rejected, never replaced** with `U+FFFD` or truncated. Embedded `U+0000` is
  valid UTF-8 and round-trips byte-exact.

Both halves of the shared `invalid_utf8` negative vectors in
`assets/test_vectors.json` are exercised by `tests/utf8_tests.rs`:
`decode_outcome: invalid` on the decode side and
`encode_outcome: invalid_argument` on the encode side.

## Usage

The codec has four use cases — serialize a message that fits in one buffer,
serialize one too large for the buffer (streamed out in chunks), deserialize a
whole message, and deserialize one arriving in chunks — plus the generated-code
path that wraps them.

### Serialize

`OStream::new` wraps a caller-owned buffer big enough for the whole message. Each
`write_*` returns `Result<()>`, never allocates (the one exception is nesting
sequences more than 8 deep — see [Sequences](#sequences)), and `bytes_used()`
reports the byte count:

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

#### Scalar arrays: the slice you pass is the value

`write_array_unsigned` / `write_array_signed` / `write_array_fp32` / `write_array_fp64`
write **every element of the slice**, gap-free, behind one count prefix. That count is
the array's **length** (MESSAGE_SPEC §3) — a schema `count: N` is a *capacity*, never
appears on the wire, and nothing is filled up to it. Trailing default elements are
ordinary elements: `[1, 2, 0, 0]` encodes as `03 04 01 02 00 00` and `[1, 2]` as
`03 02 01 02`; they are **different values**, not two spellings of one. Passing a
shortened slice is the caller declaring a shorter array.

### Serialize stream

Attach a `Flush` sink with `OStream::with_flush`. When the scratch buffer fills, it
goes to the sink and writing resumes in whatever the sink leaves behind;
`flush()` pushes the tail — so the message can far exceed the buffer:

```rust
use sofab::OStream;

let mut scratch = [0u8; 16];
let mut out = Vec::new();                  // or a socket / file
{
    let mut os = OStream::with_flush(&mut scratch, 0, |chunk: &[u8]| {
        out.extend_from_slice(chunk);
    }).unwrap();                           // Err(Argument) below MIN_OUTPUT_BUFFER,
                                           // or if the offset is past the end
    for i in 0..1000u32 { os.write_unsigned(i, i as u64).unwrap(); }
    os.flush().unwrap();                   // push the tail
}
```

Any `FnMut(&[u8])` is a **copying** sink — the `Flush` trait: it borrows the bytes
and the encoder keeps writing into the same buffer. A **taking** sink, one that
queues the buffer for an async write or hands it to DMA, implements `FlushTake<'a>`
instead: the buffer arrives as an owned `&'a mut [u8]` and the sink returns a
*replacement* buffer plus its start offset. A non-zero offset is how a sink re-arms
header room in every flushed unit — one framing header per packet.

Every `Flush` is a `FlushTake` at *every* lifetime (a blanket impl hands the buffer
straight back), so `OStream` accepts either and code generic over sinks can be bound
by the simpler one — which is what `sofabgen` emits:
`serialize<_F: sofab::Flush>(&self, os: &mut OStream<'_, _F>)`.

### Sequences

A nested sequence is opened with `write_sequence_begin_lazy(id)`, which **holds
the header back** until the sequence turns out to have content. MESSAGE_SPEC §2
omits a sequence-typed **field** whose value equals its declared default, and
"not one child was written" is exactly that condition. Which closer you use
decides whether a contentless frame survives:

| closer | a contentless sequence | use it for |
|--------|------------------------|------------|
| `write_sequence_end()` | **vanishes** — header and end marker both | a `struct`/`union` field, and an array field (the wrapper) |
| `write_sequence_end_keep()` | is written as `begin` + `end` | a wrapper-array **element**, whose presence carries a dynamic array's length (§5.1); and an array field known to differ from a non-empty declared `default` |

The choice is static — a property of the position in the schema rather than of
the value — so generated code makes it at generation time. There is no eager
`write_sequence_begin`; this is the only opener.

**The frames must balance.** Either closer called with **no sequence open** is
`Error::Argument` and writes nothing, leaving the stream exactly as it found it.
It joins the encoder's other two structural argument checks: an id above
`ID_MAX`, and nesting past `MAX_DEPTH` (255).

```rust
use sofab::OStream;

let mut buf = [0u8; 32];
let used = {
    let mut os = OStream::new(&mut buf);
    os.write_sequence_begin_lazy(1).unwrap();  // header held back
    os.write_sequence_end().unwrap();          // no content → nothing is written
    os.write_sequence_begin_lazy(2).unwrap();  // header held back
    os.write_unsigned(0, 42).unwrap();         // content → commits header id 2 first
    os.write_sequence_end().unwrap();
    os.bytes_used()
};
assert_eq!(&buf[..used], &[0x16, 0x00, 0x2A, 0x07]);
```

Held-back ids are encoder state, not buffer content, so the bytes never depend on
the output-buffer size: a pending run survives a flush — including an explicit
`flush()` between two writes — unchanged. The run is **unbounded** up to
`MAX_DEPTH` (255), so an all-default sequence is omitted at *every* legal nesting
depth.

**What it costs.** The hold-back carries a `PendingRun` in a per-message encoder
and tests it on every field write: **+142 Ir/op (+43%)** against eager framing on
the `encode: typical message` workload, and 242 Ir/op as shipped against 381 with
every id sent to the heap.

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

#### Absence is meaningful — initialise the destination before you decode

The flip side of [Sequences](#sequences) above: a field equal to its declared
default is **not on the wire**, so no callback fires for it. For a
sequence-typed *field* that means the entire frame is gone — no
`sequence_begin`, no `sequence_end`, no children — and an all-default message is
the empty byte string, which produces no callbacks at all.

So **do not use `sequence_begin` (or any callback) as your reset/prepare hook.**
Initialise every destination slot to its declared default *first* (MESSAGE_SPEC
§5.1), then let the present fields overwrite what they carry: decode into a
fresh, default-constructed destination, or reset it explicitly before `decode` /
the first `feed`. The failure is silent and only shows up on a **reused** one:

```rust
#[derive(Default)]
struct Dest { elems: Vec<Unsigned> }
impl Visitor for Dest {
    // Tempting, and wrong: msg B never calls this, so the clear never happens.
    fn sequence_begin(&mut self, id: Id) { if id == 4 { self.elems.clear(); } }
    fn unsigned(&mut self, _id: Id, v: Unsigned) { self.elems.push(v); }
}

// a = array field id 4 with elements [10, 11];  b = the same field all-default,
// which §2 omits entirely, so `b` is zero bytes long.
let mut reused = Dest::default();
decode(a, &mut reused).unwrap();
decode(b, &mut reused).unwrap();
assert_eq!(reused.elems, [10, 11]);   // stale — B's meaning was "empty"

let mut fresh = Dest::default();      // default-initialised per message
decode(b, &mut fresh).unwrap();
assert!(fresh.elems.is_empty());      // absent reconstructs to the default
```

A wrapper-array **element** is the one thing that never disappears — its frame is
kept because element presence carries a dynamic array's length (§5.1), so
`sequence_begin` does fire once per present element; it is the enclosing *field*
that can vanish. (Runnable version: the `Visitor::sequence_begin` rustdoc.)

### Deserialize stream

`IStream::feed` takes chunks of any size, suspends/resumes at any byte boundary,
and drives the same `Visitor`. It reports one of three outcomes for the bytes
seen so far (MESSAGE_SPEC §7), with **no separate finalize step**: `Ok(())` when
the stream ends exactly at a field boundary, `Err(Error::Incomplete)` when a
chunk ends mid-field (feed more — this is not an error), and
`Err(Error::InvalidMsg)` for malformed bytes. The caller owns end-of-input, so a
truncated tail is `Incomplete`, never promoted to a rejection. To probe whether
the stream ended cleanly, feed an empty chunk: `feed(&[], …)` returns `Ok(())`
iff the last byte landed on a field boundary.

`Err(Error::InvalidMsg)` is **terminal for that `IStream`**: the decoder latches
the rejection, so every later `feed` answers `InvalidMsg` again instead of
resynchronizing on the bytes that follow the malformed construct.
`IStream::reset()` clears the latch (along with the carry and the sequence depth)
so the decoder can be reused for the next message; `Incomplete` never latches:

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
into typed structs with `serialize()` (stream out through any `OStream`, sparse:
fields at their default are omitted), `decoder()` (stream in from chunks of any
size), one-shot `encode()`, best-effort `decode()` and error-checked
`try_decode()` — the same names in every SofaBuffers language. A hand-written
stand-in in exactly that shape, encoded, then decoded both ways:

```rust
use sofab::{Error, OStream, IStream, Visitor, Id, Signed};

// generated by: sofabgen --lang rust --in point.yaml --out src/
#[derive(Default)]
struct Point { x: i32, y: i32 }

impl Point {
    const MAX_SIZE: usize = 32;
    // Generic over the sink, so one `serialize` streams into a plain buffer and
    // into a flushing transport alike.
    fn serialize<F: sofab::Flush>(&self, os: &mut OStream<'_, F>) {
        let _ = os.write_signed(1, self.x as Signed);
        let _ = os.write_signed(2, self.y as Signed);
    }
    fn encode(&self) -> Vec<u8> {
        let mut buf = vec![0u8; Self::MAX_SIZE];
        let used = { let mut os = OStream::new(&mut buf); self.serialize(&mut os); os.bytes_used() };
        buf.truncate(used);
        buf
    }
    fn decode(data: &[u8]) -> Self {          // best-effort: unknown fields are skipped and
        let mut m = Self::default();          // a malformed tail is dropped, with no status
        let _ = IStream::new().feed(data, &mut m);
        m
    }
    fn try_decode(data: &[u8]) -> Result<Self, Error> {  // error-checked: use this on untrusted input
        let mut m = Self::default();
        IStream::new().feed(data, &mut m)?;
        Ok(m)
    }
    fn decoder() -> PointDecoder { PointDecoder::default() }   // streaming in
}

impl Visitor for Point {
    fn signed(&mut self, id: Id, v: Signed) { match id { 1 => self.x = v as i32, 2 => self.y = v as i32, _ => {} } }
}

#[derive(Default)]
struct PointDecoder { m: Point, is: IStream }

impl PointDecoder {
    fn feed(&mut self, chunk: &[u8]) -> Result<(), Error> { self.is.feed(chunk, &mut self.m) }
    fn finish(mut self) -> Result<Point, Error> { self.feed(&[])?; Ok(self.m) }  // rejects a truncated tail
}

let wire = Point { x: 3, y: 4 }.encode();
let got = Point::try_decode(&wire).unwrap();   // got.x == 3, got.y == 4

let mut dec = Point::decoder();                // the same object, never fully buffered
for chunk in wire.chunks(1) {                  // any chunk size, down to one byte
    match dec.feed(chunk) {
        Ok(()) | Err(Error::Incomplete) => {}  // mid-field is not a failure: feed more
        Err(e) => panic!("malformed: {e}"),
    }
}
let streamed = dec.finish().unwrap();          // streamed.x == 3, streamed.y == 4
```

Streaming out a generated object through a small buffer is the same
`serialize()` over an `OStream::with_flush` sink (see
[Serialize stream](#serialize-stream)); streaming *in* is `decoder()` above,
which wraps `IStream::feed` — your framing decides when the message is over and
`finish()` gives the verdict for it.

## Memory handling

The hot path is allocation-free and **never owns your payload memory** — you own
the buffers on both sides.

- **Encode (`OStream`):** you own the `&mut [u8]`; the library never allocates or
  grows an output buffer — not even for the one-shot path, where the caller (or
  generated code, from the schema's `MAX_SIZE`) allocates and hands one in. With no
  sink, overflow is `Error::BufferFull`; with a sink the buffer goes to the sink,
  which either copies the bytes (`Flush`) or takes the buffer and installs a
  replacement (`FlushTake`). `buffer_set` does the same from the outside, between
  messages or after a buffer-full, and **never drops bytes**: with a sink,
  whatever was already written is drained to it first, so a mid-message buffer
  swap is byte-transparent and needs no `flush()` in front of it; without a sink
  the bytes stay in the buffer *you* own and still hold — read `bytes_used()`,
  take them, install the next buffer, concatenate. An undersized buffer is
  refused before anything is drained, leaving the stream as it was. To collect
  into a `Vec`, drive a small scratch buffer with an appending flush closure —
  *you* own the `Vec`. The encoder's own memory is the run of held-back sequence
  ids ([Sequences](#sequences)): eight of them inline, spilling to the heap only
  if you nest deeper than that.
- **`MIN_OUTPUT_BUFFER` = 1.** The smallest output buffer this port accepts **for
  streaming**. It binds every buffer installed *together with a sink* —
  `with_flush`, `buffer_set` on a stream that has one, and a replacement a sink
  returns — which must have `buffer.len() - offset >= 1`. A smaller one is refused
  where it is handed over, never partway through a message, as a **status** rather
  than a panic: `Error::Argument`. A refused **replacement** additionally **kills
  the stream**: it is dropped unwritten, and every later write, `flush` and
  `buffer_set` reports `Error::Argument` rather than resuming into a message with a
  hole in it — encode again over a bigger buffer. (The one exception is a
  `buffer_set` that supersedes the replacement in the same call: it drains through
  the sink first and then installs the buffer *you* passed — judged before anything
  was drained — so nothing is lost and the stream lives.) It binds **nothing
  else**: a buffer installed *without* a sink has no minimum, so a caller sizing
  from `MAX_SIZE` keeps it exact and a two-byte message still encodes into a
  two-byte buffer. The value is 1 because the encoder splits every atomic unit —
  header, `fixlen_word`, count, scalar, float — across a flush at any byte
  boundary.
- **The start offset is in range on every path.** `offset > buffer.len()` is
  refused wherever a buffer is installed, sink or no sink — `with_offset`,
  `with_flush` and `buffer_set` all report `Error::Argument`. Only `new` is
  infallible, offset `0` being in range for every buffer. An offset equal to
  `buffer.len()` is in range too: a capacity of zero, where the first write
  reports `BufferFull`.
- **No pass-through.** A sink is only ever handed the output buffer; a `string` or
  `blob` payload is copied through it rather than passed to the sink directly. Your
  sink never receives memory it did not get from you.
- **Decode (`decode` / `IStream` + `Visitor`):** you own the input buffer and it
  must outlive the call. On the zero-copy `decode` fast path (and self-contained
  `feed` chunks) string/blob `&[u8]` chunks **borrow** directly from it, valid
  only during the callback — copy them out (`String::push_str`,
  `Vec::extend_from_slice`) to keep them. Scalars/floats arrive by value. A
  payload the transport split across chunks is put back together by
  `PayloadAcc`, which buffers only while a field is genuinely split and hands a
  whole-payload chunk straight through, unbuffered and uncopied.

| Buffer | Owner / lifetime |
|--------|------------------|
| **Output buffer** | Caller-owned `&mut [u8]`; library never allocates or grows it. With a sink: capacity ≥ `MIN_OUTPUT_BUFFER` (1). |
| **Input buffer** | Caller-owned; must outlive the call; string/blob slices borrow from it during the callback. |

This is a **push / visitor** model: values are handed to your `Visitor` as they are
parsed, so there is no address-stability requirement. The only memory the decoder
owns is `IStream`'s internal carry `Vec` — the few bytes of an item that straddled
a chunk boundary; on the encode side it is `OStream`'s run of held-back sequence
ids, inline up to eight levels and spilling to a `Vec` beyond.

## Build & test

```bash
cargo build --release            # opt-level 3, fat LTO
cargo test                       # unit + integration + doctests (incl. shared vectors)
cargo test --release             # the same suite in the shipped configuration
./coverage.sh                    # llvm-cov: summary + HTML + lcov.info
```

Run both test lines: the debug run is where the `debug_assert!`s fire, and the
`--release` run exercises the optimized code the crate actually ships. Integration
tests live in `tests/` and replay the shared vectors from
`assets/test_vectors.json`.

CI runs the same commands plus fmt + clippy (`-D warnings`), on **stable** and the
three most recent pinned stable releases, a library-only build at the declared
**MSRV 1.70**, and the suite on a **big-endian** s390x host under QEMU.

## Benchmarks

Three tools mirror the other ports' `perf`, `bench` and `run_callgrind.sh`
tooling — same workloads and output format, so results are comparable across
languages. `bench` and `run_callgrind.sh` cover all four BENCH_SPEC datasets: a
1000-element `u64` array, a small mixed message, an unbounded **1 MB blob**
(one-shot and streamed through a 4096-byte buffer), and the **composite** message
(wrapper array, multi-byte UTF-8, depth-3 nesting, an omitted default field, a
two-byte field header) with a decode and a skip-all decode row:

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

The `blob 1MB` rows are bandwidth-bound — five bytes of that message are metadata
and a million are payload — so their MB/s is this machine's `memcpy` and does not
belong next to `typical message`. The optional `encode: blob 1MB passthrough` row
is absent: this port copies `string`/`blob` runs through the output buffer rather
than handing them to the sink directly, which CORELIB_PLAN §5.1 permits.

Each workload is self-checked before it is timed: `bench` proves the megabyte
reaches the sink in ~245 buffer-sized handovers, that the chunked decode ends
`COMPLETE` with all 1,000,000 payload bytes delivered, and that `composite` really
is 64 wrapper elements, four sequences (field 4 omitted, not framed) and the
depth-3 nest — before any clock is read.

### Parity sizes

BENCH_SPEC compares ports by encoded size before it compares them by speed: a
dataset that encodes to a different number of bytes here than there is a wire
divergence. This crate is the spec's reference implementation, so these are the
numbers other ports check themselves against — `bench` and `perf` assert them on
every run, and so does `tests/bench_shape_tests.rs`:

| dataset | encoded bytes |
|---|---|
| `perf` message (12 fields) | **170** |
| `blob 1MB` (1-byte header + 4-byte `fixlen_word` + payload) | **1000005** |
| `composite` | **956** |
| `typical` message | 37 |
| `u64 array (1000)` | 9491 |

The first two are BENCH_SPEC's own parity checks; `composite`'s comes from here,
as the spec says it should, for the ports that have to match it.

### Instruction cost, as measured

`bash benches/run_callgrind.sh` on this crate:

| workload | Ir/op | bytes |
|---|---|---|
| encode: u64 array (1000) | 29645 | 9491 |
| encode: typical message | 242 | 37 |
| encode: blob 1MB one-shot | 1000091 | 1000005 |
| encode: blob 1MB streaming | 113952 | 1000005 |
| encode: composite | 21211 | 956 |
| decode: u64 array (1000) | 47737 | 9491 |
| decode: typical message | 533 | 37 |
| decode: blob 1MB | 15887 | 1000005 |
| decode: composite | 5169 | 956 |
| decode: composite skip-all | 4606 | 956 |

Two of those read the opposite of how they look:

* **`encode: blob 1MB one-shot` is not nine times the work of the streaming row.**
  Callgrind counts every iteration of `rep movsb` as an instruction, and glibc
  picks that memcpy for the one-shot row's contiguous megabyte and a vector loop
  for the streaming row's 4 KB pieces — ~1.0 Ir/byte against ~0.11. The gap is
  that choice, not the flush machinery; compare each row against the same row on
  another port.
* **`decode: blob 1MB` at 15,887 Ir for a megabyte is not a fast copy — it is no
  copy.** Decoding hands the visitor a slice into the input buffer, so the only
  work is walking the framing of 245 chunks and nothing ever touches the payload.
  That is why the row reports hundreds of GB/s rather than memory bandwidth.

### Choosing between the two Rust corelibs

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

The public API mirrors between the two, so moving code across is at most a profile
change. Arena figures are approximate (best-of-5, comparable only within a
language).
