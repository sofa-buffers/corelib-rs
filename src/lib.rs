//! # SofaBuffers (`sofab`) — Rust core library (high-speed `std` build)
//!
//! A compact, **streaming** implementation of the SofaBuffers (Sofab)
//! serialization format, tuned for **throughput on big machines**. Where the
//! sibling crate [`corelib-rs-no-std`] targets microcontrollers (heap-free,
//! `#![no_std]`, `#![forbid(unsafe_code)]`, optimized for size), this crate
//! targets servers: it uses `std`, allocates freely, and reaches for the fastest
//! decode strategy available — **advancing a cursor over a contiguous buffer**
//! with zero copies, the technique from the C++ high-speed port and Protocol
//! Buffers.
//!
//! The public API is intentionally **the same** as the no_std crate (same type
//! and method names, same semantics, byte-identical wire format), so code moves
//! between them with at most a feature/profile change. The additions here are
//! purely the std fast paths: the one-shot [`decode`] entry point and the
//! `Vec`-friendly encode helpers.
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
pub use ostream::{Flush, NoFlush, OStream};
pub use types::{Id, Signed, Unsigned, API_VERSION, ID_MAX};

#[cfg(feature = "fixlen")]
pub use types::FixlenType;

#[cfg(feature = "array")]
pub use types::ArrayKind;

#[cfg(feature = "array")]
pub use ostream::{SignedElem, UnsignedElem};

/// Compile-time view of how this `sofab` build was configured.
///
/// Each constant reflects the Cargo feature / value-width the **library** was
/// compiled with, so application code can assert that the library supports what
/// it needs. Prefer the [`require!`](crate::require) macro for a ready-made
/// message; these constants are the building blocks.
///
/// ```
/// const _: () = assert!(sofab::config::VALUE_BITS >= 64);
/// ```
pub mod config {
    /// Fixed-length fields (`fp32` / `fp64` / string / blob) are compiled in.
    pub const FIXLEN: bool = cfg!(feature = "fixlen");
    /// Array fields are compiled in.
    pub const ARRAY: bool = cfg!(feature = "array");
    /// Nested sequences are compiled in.
    pub const SEQUENCE: bool = cfg!(feature = "sequence");
    /// 64-bit floating point (`fp64`) is compiled in.
    pub const FP64: bool = cfg!(feature = "fp64");
    /// Width of the scalar value type in bits: `64` with the default-on
    /// `value64` feature, or `32` when it is disabled.
    pub const VALUE_BITS: u32 = <crate::Unsigned>::BITS;
}

/// Assert at compile time that this `sofab` build supports what your code needs.
///
/// Each argument is checked against [`config`](crate::config); a missing
/// capability fails the build with a clear message. Accepts any of `fixlen`,
/// `array`, `sequence`, `fp64`, `value32`, `value64`, separated by commas.
///
/// ```
/// sofab::require!(fp64, array, value64);
/// ```
#[macro_export]
macro_rules! require {
    (fixlen) => {
        #[allow(clippy::assertions_on_constants)]
        const _: () = ::core::assert!(
            $crate::config::FIXLEN,
            "sofab: this application requires the `fixlen` feature, but it is disabled"
        );
    };
    (array) => {
        #[allow(clippy::assertions_on_constants)]
        const _: () = ::core::assert!(
            $crate::config::ARRAY,
            "sofab: this application requires the `array` feature, but it is disabled"
        );
    };
    (sequence) => {
        #[allow(clippy::assertions_on_constants)]
        const _: () = ::core::assert!(
            $crate::config::SEQUENCE,
            "sofab: this application requires the `sequence` feature, but it is disabled"
        );
    };
    (fp64) => {
        #[allow(clippy::assertions_on_constants)]
        const _: () = ::core::assert!(
            $crate::config::FP64,
            "sofab: this application requires the `fp64` feature, but it is disabled"
        );
    };
    (value32) => {
        #[allow(clippy::assertions_on_constants)]
        const _: () = ::core::assert!(
            $crate::config::VALUE_BITS == 32,
            "sofab: this application requires the 32-bit value width (disable the default `value64` feature)"
        );
    };
    (value64) => {
        #[allow(clippy::assertions_on_constants)]
        const _: () = ::core::assert!(
            $crate::config::VALUE_BITS == 64,
            "sofab: this application requires the 64-bit value width (the default `value64` feature is disabled)"
        );
    };
    ($($cap:ident),+ $(,)?) => {
        $( $crate::require!($cap); )+
    };
}
