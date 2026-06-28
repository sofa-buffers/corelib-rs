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
    /// Invalid caller argument (e.g. a field id greater than [`crate::ID_MAX`],
    /// or an empty array). Corresponds to `SOFAB_RET_E_ARGUMENT`.
    Argument,

    /// Invalid API usage (e.g. a decoded value does not fit the requested type).
    /// Corresponds to `SOFAB_RET_E_USAGE`.
    Usage,

    /// The output buffer is full and no [`crate::Flush`] sink is available.
    /// Corresponds to `SOFAB_RET_E_BUFFER_FULL`.
    BufferFull,

    /// The input bytes are not a valid Sofab message (varint overflow, bad type
    /// tag, zero-length array, dangling sequence end, truncated message, …).
    /// Corresponds to `SOFAB_RET_E_INVALID_MSG`.
    InvalidMsg,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let msg = match self {
            Error::Argument => "invalid argument",
            Error::Usage => "invalid API usage",
            Error::BufferFull => "output buffer full and no flush sink set",
            Error::InvalidMsg => "malformed SofaBuffers message",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for Error {}

/// Convenience alias for fallible Sofab operations.
pub type Result<T> = core::result::Result<T, Error>;
