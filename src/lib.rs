//! # SofaBuffers (`sofab`) — Rust core library (high-speed `std` build)
//!
//! A compact, **streaming** implementation of the SofaBuffers (Sofab)
//! serialization format, tuned for **throughput on big machines**. Where the
//! sibling crate [`corelib-rs-no-std`] targets microcontrollers (heap-free,
//! `#![no_std]`, optimized for size), this crate targets servers: it uses `std`,
//! allocates freely, and reaches for the fastest decode strategy available —
//! **advancing a cursor over a contiguous buffer** with zero copies, the
//! technique from the C++ high-speed port and Protocol Buffers.
//!
//! Every wire type is always compiled in — there are **no Cargo feature flags
//! and no build-time configuration**. The scalar value type is always 64-bit
//! (`u64`/`i64`). The wire format is byte-identical to every other `corelib-*`
//! port, and the method names mirror the no_std crate so code moves between them
//! freely.
//!
//! [`corelib-rs-no-std`]: https://github.com/sofa-buffers/corelib-rs-no-std
//!
//! ## Two decode paths, one [`Visitor`]
//!
//! * [`decode`] — give it a whole message; it advances a pointer over the buffer
//!   and hands every field (and zero-copy string/blob slices) to your visitor.
//! * [`IStream`] — feed it arbitrarily small chunks; it suspends and resumes at
//!   any byte boundary (CORELIB_PLAN §5.2) yet still takes the zero-copy fast
//!   path whenever a chunk is self-contained.
//!
//! ## Absence is meaningful: initialise the destination, not from a callback
//!
//! MESSAGE_SPEC §2 omits any field equal to its declared default — and a
//! sequence-typed *field* is omitted whole, so an all-default
//! `struct`/`union`/array produces **no callback at all**: no
//! [`Visitor::sequence_begin`], no `sequence_end`, no children. An all-default
//! message is the empty byte string and decodes to zero callbacks of any kind.
//! (Encoding side: [`OStream::write_sequence_end`] drops such a frame,
//! [`OStream::write_sequence_end_keep`] forces it out for a wrapper-array
//! *element*, whose presence is what carries a dynamic array's length, §5.1.)
//!
//! A [`Visitor`] therefore **must not** use `sequence_begin` — or any callback —
//! as its reset/prepare hook. MESSAGE_SPEC §5.1 puts the duty *before* the
//! decode: initialise every destination slot to its declared default first, then
//! let the present fields overwrite what they carry. Decode into a fresh,
//! default-constructed destination, or reset it explicitly before [`decode`] /
//! the first [`IStream::feed`]. Done that way the omission is lossless by
//! construction — absent reconstructs to the default. Done from a callback, a
//! reused destination silently keeps the previous message's values for exactly
//! the fields the new message left at their defaults. [`Visitor::sequence_begin`]
//! carries a runnable demonstration.
//!
//! ## String validity: strict UTF-8 (always on)
//!
//! A `string` field is UTF-8 (MESSAGE_SPEC §8). Because Rust's `str`/`String`
//! is a **Unicode string type**, this port is **always strict** — the
//! `SOFAB_STRICT_UTF8` option (CORELIB_PLAN §6.4) is a **no-op here, pinned ON**,
//! and there is no primitive to expose (only byte-container targets need one):
//!
//! * **Encode is strict.** [`OStream::write_str`] takes `&str`, which the type
//!   system already guarantees is valid UTF-8, so that path can never carry
//!   invalid bytes and pays no runtime check. The byte-level
//!   [`OStream::write_fixlen`] *can* be handed arbitrary bytes under the `Str`
//!   subtype, so it validates: a non-UTF-8 `string` payload — like an
//!   `fp32`/`fp64` payload of the wrong width (§4.6) — is refused with
//!   [`Error::Argument`] before anything is written. (Arbitrary bytes go in a
//!   `blob` via [`OStream::write_blob`].)
//! * **Decode strictness lives in generated code.** The corelib delivers a
//!   `string` field's **raw bytes** to [`Visitor::string`] and never builds a
//!   `String` itself. Generated code materializes the field with
//!   `core::str::from_utf8`; an `Err` becomes the sticky `inv` flag →
//!   [`Error::InvalidMsg`] (the `INVALID` decode outcome). Invalid UTF-8 is
//!   therefore **rejected, never replaced** with `U+FFFD` or truncated
//!   (MESSAGE_SPEC §8). Embedded `U+0000` is valid UTF-8 and round-trips
//!   byte-exact. This matches `corelib-rs-no-std` exactly (generator #80).
//! * **Skipped fields are never validated** — a skipped `string` is a length
//!   jump over bytes the visitor never sees, so no `from_utf8` runs (§6.4).
//!
//! ## Example
//!
//! ```
//! use sofab::{OStream, decode, Visitor, Id, Unsigned, Signed};
//!
//! // --- encode (into a caller buffer; swap in a flush sink to stream out) ---
//! let mut buf = [0u8; 32];
//! let used = {
//!     let mut os = OStream::new(&mut buf);
//!     os.write_unsigned(1, 42).unwrap();
//!     os.write_signed(2, -7).unwrap();
//!     os.bytes_used()
//! };
//!
//! // --- decode (one-shot, zero-copy) ---
//! #[derive(Default)]
//! struct Sink { a: Unsigned, b: Signed }
//! impl Visitor for Sink {
//!     fn unsigned(&mut self, id: Id, v: Unsigned) { if id == 1 { self.a = v; } }
//!     fn signed(&mut self, id: Id, v: Signed) { if id == 2 { self.b = v; } }
//! }
//! let mut sink = Sink::default();
//! decode(&buf[..used], &mut sink).unwrap();
//! assert_eq!((sink.a, sink.b), (42, -7));
//! ```

#![deny(missing_docs)]

mod error;
mod istream;
mod ostream;
mod trim;
mod types;
mod varint;

pub use error::{Error, Result};
pub use istream::{decode, IStream, Visitor};
pub use ostream::{Flush, FlushTake, NoFlush, OStream, SignedElem, UnsignedElem};
pub use trim::{trim_tail, trim_tail_f32, trim_tail_f64};
pub use types::{
    ArrayKind, FixlenType, Id, Signed, Unsigned, API_VERSION, ID_MAX, MAX_DEPTH, MIN_OUTPUT_BUFFER,
};
