//! Shared types and wire constants (see the SofaBuffers documentation:
//! <https://github.com/sofa-buffers/documentation>).
//!
//! These are identical to the no_std port so the two Rust crates are
//! source-compatible at the type level.

/// SofaBuffers wire/API version implemented by this library.
///
/// Normative per the architecture spec (`API_VERSION == 1`).
pub const API_VERSION: u32 = 1;

/// Field identifier type. Application-assigned; need not be contiguous.
pub type Id = u32;

/// Largest valid field id (`INT32_MAX`), matching `SOFAB_ID_MAX` in C.
pub const ID_MAX: Id = i32::MAX as u32;

/// Unsigned value type used by the scalar API.
///
/// 64-bit by default (the natural width on a 64-bit host and the C reference
/// default). Disabling the `value64` feature narrows it to 32 bits, purely for
/// parity with a 32-bit `sofab_value_t` build; values above 2³²−1 then cannot be
/// represented. Guard with [`require!`](crate::require)`(value64)` /
/// `require!(value32)`.
#[cfg(feature = "value64")]
pub type Unsigned = u64;
/// Signed value type used by the scalar API.
#[cfg(feature = "value64")]
pub type Signed = i64;

/// Unsigned value type used by the scalar API (32-bit build, `value64` off).
#[cfg(not(feature = "value64"))]
pub type Unsigned = u32;
/// Signed value type used by the scalar API (32-bit build, `value64` off).
#[cfg(not(feature = "value64"))]
pub type Signed = i32;

/// Maximum number of elements in an array / bytes in a fixlen field
/// (`INT32_MAX`).
#[cfg(any(feature = "array", feature = "fixlen"))]
pub(crate) const ARRAY_MAX: u64 = i32::MAX as u64;

// --- 3-bit wire field type tags (low 3 bits of the field header varint) ------
pub(crate) const T_VARINT_UNSIGNED: u8 = 0x0;
pub(crate) const T_VARINT_SIGNED: u8 = 0x1;
#[cfg(feature = "fixlen")]
pub(crate) const T_FIXLEN: u8 = 0x2;
#[cfg(feature = "array")]
pub(crate) const T_VARINTARRAY_UNSIGNED: u8 = 0x3;
#[cfg(feature = "array")]
pub(crate) const T_VARINTARRAY_SIGNED: u8 = 0x4;
#[cfg(all(feature = "array", feature = "fixlen"))]
pub(crate) const T_FIXLENARRAY: u8 = 0x5;
#[cfg(feature = "sequence")]
pub(crate) const T_SEQUENCE_START: u8 = 0x6;
#[cfg(feature = "sequence")]
pub(crate) const T_SEQUENCE_END: u8 = 0x7;

/// Sub-type of a fixed-length field (the 3-bit tag inside the fixlen header).
#[cfg(feature = "fixlen")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FixlenType {
    /// 32-bit IEEE-754 float, little-endian on the wire.
    Fp32 = 0x0,
    /// 64-bit IEEE-754 double, little-endian on the wire.
    #[cfg(feature = "fp64")]
    Fp64 = 0x1,
    /// UTF-8 / raw text (no NUL on the wire).
    Str = 0x2,
    /// Arbitrary raw bytes.
    Blob = 0x3,
}

#[cfg(feature = "fixlen")]
impl FixlenType {
    /// Decode a 3-bit fixlen tag from the wire, rejecting unsupported subtypes.
    pub(crate) fn from_raw(raw: u8) -> crate::Result<Self> {
        match raw {
            0x0 => Ok(FixlenType::Fp32),
            #[cfg(feature = "fp64")]
            0x1 => Ok(FixlenType::Fp64),
            0x2 => Ok(FixlenType::Str),
            0x3 => Ok(FixlenType::Blob),
            _ => Err(crate::Error::InvalidMsg),
        }
    }
}

/// Element category of an array, reported to a [`crate::Visitor`] at the start
/// of an array field.
#[cfg(feature = "array")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrayKind {
    /// Unsigned-integer elements (delivered via [`crate::Visitor::unsigned`]).
    Unsigned,
    /// Signed-integer elements (delivered via [`crate::Visitor::signed`]).
    Signed,
    /// Floating-point elements (delivered via `fp32` / `fp64`).
    #[cfg(feature = "fixlen")]
    Fixlen,
}
