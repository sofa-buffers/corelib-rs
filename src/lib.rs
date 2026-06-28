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
//!   any byte boundary (ARCHITECTURE §5.2) yet still takes the zero-copy fast
//!   path whenever a chunk is self-contained.
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
mod types;
mod varint;

pub use error::{Error, Result};
pub use istream::{decode, IStream, Visitor};
pub use ostream::{Flush, NoFlush, OStream, SignedElem, UnsignedElem};
pub use types::{ArrayKind, FixlenType, Id, Signed, Unsigned, API_VERSION, ID_MAX};
