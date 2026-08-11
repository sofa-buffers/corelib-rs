//! Shared types and wire constants (see the SofaBuffers documentation:
//! <https://github.com/sofa-buffers/documentation>).

/// SofaBuffers wire/API version implemented by this library.
///
/// Normative per the architecture spec (`API_VERSION == 1`).
pub const API_VERSION: u32 = 1;

/// Field identifier type. Application-assigned; need not be contiguous.
pub type Id = u32;

/// Largest valid field id (`INT32_MAX`), matching `SOFAB_ID_MAX` in C.
pub const ID_MAX: Id = i32::MAX as u32;

/// Unsigned value type used by the scalar API — always 64-bit (this build targets
/// 64-bit hosts and does not trade range for footprint).
pub type Unsigned = u64;
/// Signed value type used by the scalar API — always 64-bit.
pub type Signed = i64;

/// Maximum number of elements in an array (`INT32_MAX`).
pub(crate) const ARRAY_MAX: u64 = i32::MAX as u64;

/// Maximum number of bytes in a fixlen field / per fixlen-array element
/// (`INT32_MAX`).
pub(crate) const FIXLEN_MAX: u64 = i32::MAX as u64;

/// Smallest output buffer this port accepts **for streaming** — the capacity
/// (`buffer.len() - offset`) every buffer installed **together with a flush
/// sink** — [`crate::Flush`] or [`crate::FlushTake`] — must have
/// (CORELIB_PLAN §5.1).
///
/// This crate declares **1**: it splits every atomic unit — a field header, a
/// `fixlen_word`, an element count, a scalar varint, a float — across a flush at
/// any byte boundary, so no write needs to land contiguously. Nothing above one
/// byte is ever reserved.
///
/// The constant binds a buffer handed to [`crate::OStream::with_flush`], to
/// [`crate::OStream::buffer_set`] on a stream that has a sink, and to a
/// replacement a sink installs from inside its callback. It binds **nothing
/// else**: a buffer installed *without* a sink is subject to no minimum, because
/// no flush can occur there — that buffer either holds the message or reports
/// [`crate::Error::BufferFull`]. A caller sizing a one-shot buffer from a
/// schema's `MAX_SIZE` therefore keeps it exact, and a two-byte message still
/// encodes into a two-byte buffer.
pub const MIN_OUTPUT_BUFFER: usize = 1;

/// Maximum nested-sequence depth. An encoder must not open more than this many
/// nested sequences, and a decoder rejects a message that nests deeper with
/// [`crate::Error::InvalidMsg`] (normative per the architecture spec, §6.2).
pub const MAX_DEPTH: u32 = 255;

// --- 3-bit wire field type tags (low 3 bits of the field header varint) ------
pub(crate) const T_VARINT_UNSIGNED: u8 = 0x0;
pub(crate) const T_VARINT_SIGNED: u8 = 0x1;
pub(crate) const T_FIXLEN: u8 = 0x2;
pub(crate) const T_VARINTARRAY_UNSIGNED: u8 = 0x3;
pub(crate) const T_VARINTARRAY_SIGNED: u8 = 0x4;
pub(crate) const T_FIXLENARRAY: u8 = 0x5;
pub(crate) const T_SEQUENCE_START: u8 = 0x6;
pub(crate) const T_SEQUENCE_END: u8 = 0x7;

/// Sub-type of a fixed-length field (the 3-bit tag inside the fixlen header).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FixlenType {
    /// 32-bit IEEE-754 float, little-endian on the wire.
    Fp32 = 0x0,
    /// 64-bit IEEE-754 double, little-endian on the wire.
    Fp64 = 0x1,
    /// UTF-8 / raw text (no NUL on the wire).
    Str = 0x2,
    /// Arbitrary raw bytes.
    Blob = 0x3,
}

/// Element category of an array, reported to a [`crate::Visitor`] at the start
/// of an array field.
///
/// For a fixlen array the kind names the **element subtype** (`Fp32` / `Fp64`),
/// not merely "some fixlen array": the receiver has to know which of the two it
/// is before it can decide whether the array is the declared field's value at
/// all (CORELIB_PLAN §4.8 step 3, MESSAGE_SPEC §7.3). The hook is therefore
/// delivered *after* the `fixlen_word` for wire type `ARRAY_FIXLEN`, and right
/// after the count word for the integer arrays, which carry no second word.
///
/// The discriminants are normative across the family; do not renumber them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrayKind {
    /// Unsigned-integer elements (delivered via [`crate::Visitor::unsigned`]).
    Unsigned = 0,
    /// Signed-integer elements (delivered via [`crate::Visitor::signed`]).
    Signed = 1,
    /// 32-bit float elements (delivered via [`crate::Visitor::fp32`]).
    Fp32 = 2,
    /// 64-bit float elements (delivered via [`crate::Visitor::fp64`]).
    Fp64 = 3,
}
