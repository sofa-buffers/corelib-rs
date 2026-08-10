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

- **Encode is strict.** The typed writer `OStream::write_str` takes `&str`, which
  is already guaranteed valid UTF-8 by the type system, so that path can never
  carry invalid bytes and pays no runtime check. The byte-level
  `OStream::write_fixlen` is public as well and *can* be handed arbitrary bytes
  under the `Str` subtype — there the check is real: a payload that is not valid
  UTF-8 is refused with `Error::Argument` (§6.3's `InvalidArgument`), before a
  single byte reaches the buffer. Put arbitrary bytes in a `blob` (`write_blob`,
  or `write_fixlen` with `FixlenType::Blob`), which is unconstrained.
- **Decode strictness lives in generated code.** The corelib hands a `string`
  field's **raw bytes** to `Visitor::string` and never builds a `String`;
  generated code materializes it with `core::str::from_utf8`, turning invalid
  bytes into `Error::InvalidMsg` (the `INVALID` decode outcome). Invalid UTF-8 is
  **rejected, never replaced** with `U+FFFD` or truncated. Embedded `U+0000` is
  valid UTF-8 and round-trips byte-exact. This matches `corelib-rs-no-std`
  exactly (subsumes generator #80).

Both halves of the shared `invalid_utf8` negative vectors in
`assets/test_vectors.json` (tracking corelib-c-cpp#97) are exercised by
`tests/utf8_tests.rs`: `decode_outcome: invalid` on the decode side and
`encode_outcome: invalid_argument` on the encode side.

`write_fixlen` validates the payload against the **subtype** it is given, not
just against the length ceiling, so this byte-level entry point cannot produce a
message a conformant decoder must reject: `Fp32`/`Fp64` require exactly 4 / 8
payload bytes (§4.6) and `Str` requires valid UTF-8; anything else is
`Error::Argument` with nothing written. `write_fp32`/`write_fp64`/`write_str`/
`write_blob` are correct by construction and skip the check entirely.

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
therefore ordinary elements: `[1, 2, 0, 0]` encodes as `03 04 01 02 00 00` and
`[1, 2]` as `03 02 01 02`; they are **different values**, not two spellings of one, so
dropping a trailing zero on the way in loses data. The crate ships no helper that
shortens a slice for the encoder, and passing a shortened slice is the caller
declaring a shorter array.

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
    });                                    // panics below MIN_OUTPUT_BUFFER;
                                           // try_with_flush returns Err instead
    for i in 0..1000u32 { os.write_unsigned(i, i as u64).unwrap(); }
    os.flush().unwrap();                   // push the tail
}
```

Any `FnMut(&[u8])` is a **copying** sink: it borrows the bytes and the encoder keeps
writing into the same buffer. That is the `Flush` trait, and it is all a closure can
be. A **taking** sink — one that queues the buffer for an async write or hands it to
DMA — implements `FlushTake<'a>` instead and returns a *replacement* buffer plus its
start offset. It cannot do otherwise: the buffer arrives as an owned `&'a mut [u8]`,
so a sink that keeps it has nothing to give back and must supply a replacement to
compile. Returning a buffer with a non-zero offset is how a sink re-arms header room
in every flushed unit — one framing header per packet.

The two traits differ only in that lifetime, and it is the reason there are two:
storing the buffer means naming the lifetime it is stored under, while copying never
touches it. Every `Flush` is therefore a `FlushTake` at *every* lifetime (a blanket
impl hands the buffer straight back), so `OStream` accepts either and code generic
over sinks can be bound by the simpler one — which is what generated crates do:
`sofabgen` emits `serialize<_F: sofab::Flush>(&self, os: &mut OStream<'_, _F>)`.

### Sequences

A nested sequence is opened with `write_sequence_begin_lazy(id)`, which **holds
the header back** until the sequence turns out to have content. MESSAGE_SPEC §2
omits a sequence-typed **field** whose value equals its declared default, and
"not one child was written" is exactly that condition — so which of the two
closers you use decides whether a contentless frame survives:

| closer | a contentless sequence | use it for |
|--------|------------------------|------------|
| `write_sequence_end()` | **vanishes** — header and end marker both | a `struct`/`union` field, and an array field (the wrapper) |
| `write_sequence_end_keep()` | is written as `begin` + `end` | a wrapper-array **element**, whose presence carries a dynamic array's length (§5.1); and an array field known to differ from a non-empty declared `default` |

"A sequence is always framed" is therefore no longer true of a **field** — it is
still true of an **element**. The choice is static, a property of the position in
the schema rather than of the value, so generated code makes it at generation
time. There is no eager `write_sequence_begin`; this is the only opener.

**The frames must balance.** Either closer called with **no sequence open** is
`Error::Argument` (§6.3's `InvalidArgument`) and writes nothing: the encoder
refuses to emit a lone end marker, one of the byte sequences §5.2 calls malformed
*regardless of what follows* and which this port's own decoder answers
`Error::InvalidMsg` to. It joins the encoder's other two structural argument
checks — an id above `ID_MAX`, and nesting past `MAX_DEPTH` (255) — and a
rejected close leaves the stream exactly as it found it, so the open-frame count
stays truthful for the rest of the message.

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
depth, not just shallow ones (CORELIB_PLAN §6: only a heap-free profile, such as
`corelib-rs-no-std`, may bound the run and frame eagerly past the bound).

**What it costs.** Not nothing. Callgrind (`bash benches/run_callgrind.sh`),
workload `encode: typical message` — a 37-byte message with one nested sequence,
encoded through a freshly built `OStream`:

| encoder | Ir/op |
|---|---|
| **lazy framing, as shipped** (8 inline slots, spill beyond) | **242** |
| lazy framing, `INLINE_PENDING = 1` | 237 |
| lazy framing, `INLINE_PENDING = 0` (heap on every sequence) | 381 |

The hold-back is not free — it carries a `PendingRun` in a per-message encoder
and tests it on every field write. Measured against eager framing when the
feature landed, it cost **+142 Ir/op (+43%)** on this workload. Keeping the run
off the heap is what most of the tuning buys (242 vs 381); the width of the
inline array is worth about 5 Ir of that, so 8 slots is a depth choice, not a
speed one.

The absolute figures above are lower than the ones this table carried when the
feature landed, because the varint codecs, the encoder's capacity handling and
the fixlen paths were reworked since (`encode: typical message` 475 → 242,
`encode: u64 array (1000)` 139928 → 29645, `decode: u64 array (1000)` 94180 →
47737, `decode: typical message` 1186 → 533). The ratios the `INLINE_PENDING`
choice rests on did not move.

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
MESSAGE_SPEC §5.1 puts the duty before the decode: initialise every destination
slot to its declared default *first*, then let the present fields overwrite what
they carry. Decode into a fresh, default-constructed destination, or reset it
explicitly before `decode` / the first `feed`.

The failure is silent and only shows up on a **reused** destination:

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

Do it the second way and the omission is lossless by construction. A
wrapper-array **element** is the one thing that never disappears — its frame is
kept precisely because element presence carries a dynamic array's length (§5.1),
so `sequence_begin` does fire once per present element; it is the enclosing
*field* that can vanish. (Runnable version: the `Visitor::sequence_begin`
rustdoc.)

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
returns `Ok(())` iff the last byte landed on a field boundary.

`Err(Error::InvalidMsg)` is **terminal for that `IStream`** (§5.2: "can more
bytes change it? — no"). The decoder latches the rejection, so every later `feed`
answers `InvalidMsg` again instead of resynchronizing on the bytes that follow
the malformed construct — that is what keeps the verdict a property of the bytes
rather than of where your chunk boundaries happened to fall. `IStream::reset()`
clears the latch (along with the carry and the sequence depth) so the decoder can
be reused for the next message; `Incomplete` never latches — it is not an error,
just "feed more":

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
`try_decode()` — the same names in every SofaBuffers language, so what you learn
here carries across ports. A hand-written stand-in in exactly that shape
(real output decodes through a private `Visitor` module), encoded, then decoded
both ways:

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
[Serialize stream](#serialize-stream)); streaming *in* is `decoder()` above — the
generated reader wraps the `IStream::feed` path shown earlier, so your framing
decides when the message is over and `finish()` gives the verdict for it.

## Memory handling

The hot path is allocation-free and **never owns your payload memory** — you own
the buffers on both sides.

- **Encode (`OStream`):** you own the `&mut [u8]`; the library never allocates or
  grows an output buffer — not even for the one-shot path, where the caller (or
  generated code, from the schema's `MAX_SIZE`) allocates and hands one in. With no
  sink, overflow is `Error::BufferFull`; with a sink the buffer goes to the sink,
  which either copies the bytes (`Flush`) or takes the buffer and installs a
  replacement (`FlushTake`).
  `buffer_set` does the same from the outside, between messages or after a
  buffer-full. **It never drops bytes:** on a stream *with* a sink whatever was
  already written goes to the sink first — dropping it would truncate the message
  while every call still returned `Ok`, the "partial output as if it were
  complete" §5.1 forbids — so a mid-message buffer swap is byte-transparent and
  needs no `flush()` in front of it. Without a sink there is nowhere to drain to,
  so the bytes stay in the buffer *you* own and still hold: read `bytes_used()`,
  take them, install the next buffer, concatenate. (An undersized buffer is
  refused before anything is drained, leaving the stream as it was.)
  To collect into a `Vec`, drive a small scratch buffer with an
  appending flush closure — *you* own the `Vec`. The encoder's own memory is the
  run of held-back sequence ids ([Sequences](#sequences)): eight of them inline,
  spilling to the heap only if you nest deeper than that.
- **`MIN_OUTPUT_BUFFER` = 1.** The smallest output buffer this port accepts **for
  streaming**. It binds every buffer installed *together with a sink* —
  `with_flush`, `buffer_set` on a stream that has one, and a replacement a sink
  returns — which must have `buffer.len() - offset >= 1`; a smaller one is refused
  where it is handed over, never partway through a message. The buffer and its
  offset come from your code rather than from decoded input, so `with_flush` treats
  a shortfall as a precondition violation and **panics**, like an out-of-range slice
  index; `try_with_flush` and `buffer_set` report it as `Error::Argument`, and so
  does a replacement a sink returns.
  A refused **replacement** additionally **kills the stream**: it is dropped
  unwritten — the encoder never puts a byte in it, and the sink is never handed its
  contents back as output — and since the write that hit the refusal never landed,
  every later write, `flush` and `buffer_set` reports `Error::Argument` rather than
  resuming into a message with a hole in it. Encode again over a bigger buffer.
  (The one exception is a `buffer_set` that supersedes the replacement in the same
  call: it drains through the sink first and then installs the buffer *you* passed
  — judged before anything was drained — so nothing is lost and the stream lives.)
  It binds **nothing else**: a buffer installed *without* a sink has no minimum,
  because no flush can occur there, so a caller sizing from `MAX_SIZE` keeps it
  exact and a two-byte message still encodes into a two-byte buffer. The value is 1
  because the encoder splits every atomic unit — header, `fixlen_word`, count,
  scalar, float — across a flush at any byte boundary, so nothing needs to land
  contiguously.
- **The start offset is in range on every path.** What the sinkless case waives is
  the *minimum*, not the offset: `offset > buffer.len()` names no installation at
  all and is refused wherever a buffer is installed, sink or no sink —
  `with_offset` **panics** (the `try_with_offset` / `buffer_set` /
  `try_with_flush` status form is `Error::Argument`, and `with_flush` panics).
  `offset == buffer.len()` is in range: a capacity of zero, where the first write
  reports `BufferFull`. Like the minimum, a refused `buffer_set` is judged before
  anything is drained and leaves the stream exactly as it was.
- **No pass-through.** A sink is only ever handed the output buffer; a `string` or
  `blob` payload is copied through it rather than passed to the sink directly. Your
  sink never receives memory it did not get from you.
- **Decode (`decode` / `IStream` + `Visitor`):** you own the input buffer and it
  must outlive the call. On the zero-copy `decode` fast path (and self-contained
  `feed` chunks) string/blob `&[u8]` chunks **borrow** directly from it, valid
  only during the callback — copy them out (`String::push_str`,
  `Vec::extend_from_slice`) to keep them. Scalars/floats arrive by value.

| Buffer | Owner / lifetime |
|--------|------------------|
| **Output buffer** | Caller-owned `&mut [u8]`; library never allocates or grows it. With a sink: capacity ≥ `MIN_OUTPUT_BUFFER` (1). |
| **Input buffer** | Caller-owned; must outlive the call; string/blob slices borrow from it during the callback. |

This is a **push / visitor** model: values are handed to your `Visitor` as they
are parsed, so there is no address-stability requirement. The only memory the
decoder owns is `IStream`'s small internal carry `Vec` — the few bytes of an item
that straddled a chunk boundary; on the encode side it is `OStream`'s run of
held-back sequence ids — inline up to eight levels, spilling to a `Vec` beyond,
never larger than the depth you actually nest to.

## Build & test

```bash
cargo build --release            # opt-level 3, fat LTO
cargo test                       # unit + integration + doctests (incl. shared vectors)
cargo test --release             # the same suite in the shipped configuration
./coverage.sh                    # llvm-cov: summary + HTML + lcov.info
```

Run both test lines. The debug run is the one where the `debug_assert!`s fire;
the `--release` run is the one that exercises the optimized code the crate
actually ships — the decoder's unchecked unaligned loads and the encoder's
unchecked stores are guarded by reasoning the inliner can see through, so they
have to be asserted on at `opt-level = 3` with fat LTO, not only at
`opt-level = 0`.

CI runs fmt + clippy (`-D warnings`), the full suite on **stable** and the three
most recent pinned stable releases, that same suite again in the **release
profile**, a library-only build at the declared **MSRV 1.70**, the suite on a
**big-endian** s390x host under QEMU, and llvm-cov coverage.
Integration tests live in `tests/` (shared-vector replay, fast-path decode,
encoder/decoder byte-exact checks, round-trip, malformed-input errors, the
output-buffer and flush-handover contract, non-canonical-but-valid input, and
chunk lifetime — every chunk scrubbed the moment `feed` returns).

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
and a million are payload — so read them **against each other**, not next to
`typical message`. The optional `encode: blob 1MB passthrough` row is absent: this
port copies `string`/`blob` runs through the output buffer rather than handing them
to the sink directly, which CORELIB_PLAN §5.1 permits.

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

Pick **this crate** for servers and throughput; pick **`corelib-rs-no-std`** for
embedded and footprint. The public API mirrors between them, so moving code across
is at most a profile change. Arena figures are approximate (best-of-5, comparable
only within a language).
