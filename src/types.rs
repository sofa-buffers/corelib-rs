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

impl FixlenType {
    /// Decode a 3-bit fixlen tag from the wire, rejecting unsupported subtypes.
    pub(crate) fn from_raw(raw: u8) -> crate::Result<Self> {
        match raw {
            0x0 => Ok(FixlenType::Fp32),
            0x1 => Ok(FixlenType::Fp64),
            0x2 => Ok(FixlenType::Str),
            0x3 => Ok(FixlenType::Blob),
            _ => Err(crate::Error::InvalidMsg),
        }
    }
}

/// Element category of an array, reported to a [`crate::Visitor`] at the start
/// of an array field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrayKind {
    /// Unsigned-integer elements (delivered via [`crate::Visitor::unsigned`]).
    Unsigned,
    /// Signed-integer elements (delivered via [`crate::Visitor::signed`]).
    Signed,
    /// Floating-point elements (delivered via `fp32` / `fp64`).
    Fixlen,
}
