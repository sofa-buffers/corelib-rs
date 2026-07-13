//! Error and result types.
//!
//! Mirrors the C `sofab_ret_t` status codes (minus `OK`, which Rust models as
//! `Ok(())`) and the no_std port's [`Error`], so code moves between the two Rust
//! crates unchanged. Unlike the no_std crate, this one is `std`, so [`Error`]
//! also implements [`std::error::Error`] and [`core::fmt::Display`] for use with
//! `?` in `fn() -> Result<_, Box<dyn Error>>` and friends.

/// Errors returned by the encoder and decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// Invalid caller argument (e.g. a field id greater than [`crate::ID_MAX`], a
    /// length/count above the maximum, or more than [`crate::MAX_DEPTH`] nested
    /// sequences). Corresponds to `SOFAB_RET_E_ARGUMENT`.
    Argument,

    /// Invalid API usage (e.g. a decoded value does not fit the requested type).
    /// Corresponds to `SOFAB_RET_E_USAGE`.
    Usage,

    /// The output buffer is full and no [`crate::Flush`] sink is available.
    /// Corresponds to `SOFAB_RET_E_BUFFER_FULL`.
    BufferFull,

    /// The input bytes are not a valid Sofab message *regardless of what follows*
    /// (varint overflow, bad type tag, oversized length/count, dangling sequence
    /// end, nesting past [`crate::MAX_DEPTH`], wrong fixlen length/subtype,
    /// invalid UTF-8, …).
    /// Corresponds to `SOFAB_RET_E_INVALID_MSG`.
    ///
    /// This is distinct from [`Error::Incomplete`]: a truncated message is *not*
    /// malformed, it is merely unfinished. See MESSAGE_SPEC §7.
    InvalidMsg,

    /// The consumed bytes end **inside** a field — a partial varint (continuation
    /// bit set with no terminating byte), a fixlen/array payload shorter than
    /// declared, or an open (unclosed) sequence. This is the third decode outcome
    /// (MESSAGE_SPEC §7): not an error in the sense of malformed input — the
    /// caller owns end-of-input and may feed more bytes to complete the message.
    /// A streaming [`crate::IStream::feed`] returns this whenever a chunk ends
    /// mid-field; the one-shot [`crate::decode`] returns it for truncated input.
    Incomplete,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let msg = match self {
            Error::Argument => "invalid argument",
            Error::Usage => "invalid API usage",
            Error::BufferFull => "output buffer full and no flush sink set",
            Error::InvalidMsg => "malformed SofaBuffers message",
            Error::Incomplete => "incomplete SofaBuffers message (ends mid-field)",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for Error {}

/// Convenience alias for fallible Sofab operations.
pub type Result<T> = core::result::Result<T, Error>;
