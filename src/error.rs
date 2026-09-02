//! Error and result types.
//!
//! Mirrors the C `sofab_ret_t` status codes (minus `OK`, which Rust models as
//! the `Ok` arm) and the no_std port's [`Error`], so code moves between the two
//! Rust crates unchanged.
//!
//! **`INCOMPLETE` is not here, by design.** CORELIB_PLAN §5.2.1 calls it a
//! first-class decode *outcome* and not an error, and §6.3 keeps the per-`feed`
//! result ("the three-valued outcome … *not* a code from this table") apart from
//! this table. It is therefore carried in the success arm, as
//! [`crate::Status::Incomplete`]; what remains here is what genuinely failed, so
//! `?` propagates real errors instead of the commonest normal case.
//!
//! Unlike the no_std crate, this one is `std`, so [`Error`] also implements
//! [`std::error::Error`] and [`core::fmt::Display`] for use with `?` in
//! `fn() -> Result<_, Box<dyn Error>>` and friends.

/// Errors returned by the encoder and decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// Invalid caller argument (e.g. a field id greater than [`crate::ID_MAX`], a
    /// length/count above the maximum, or more than [`crate::MAX_DEPTH`] nested
    /// sequences). Corresponds to `SOFAB_RET_E_ARGUMENT`.
    Argument,

    /// The output buffer is full and no [`crate::Flush`] sink is available.
    /// Corresponds to `SOFAB_RET_E_BUFFER_FULL`.
    BufferFull,

    /// The input bytes are not a valid Sofab message *regardless of what follows*
    /// (varint overflow, bad type tag, oversized length/count, dangling sequence
    /// end, nesting past [`crate::MAX_DEPTH`], wrong fixlen length/subtype,
    /// invalid UTF-8, …).
    /// Corresponds to `SOFAB_RET_E_INVALID_MSG`.
    ///
    /// This is distinct from [`crate::Status::Incomplete`]: a truncated message
    /// is *not* malformed, it is merely unfinished — and it is not an error at
    /// all, so it never reaches this enum. See MESSAGE_SPEC §7.
    InvalidMsg,

    /// A receiver-configured decode limit was exceeded — a dynamic (unbounded)
    /// field carried more than the maximum element count or byte length the
    /// receiver is willing to accept (`max_dyn_array_count`, `max_dyn_string_len`,
    /// `max_dyn_blob_len`). Corresponds to `SOFAB_RET_E_LIMIT_EXCEEDED` and the
    /// no_std port's `Error::LimitExceeded`.
    ///
    /// This is **policy, not wire malformation**, and is therefore deliberately
    /// distinct from [`Error::InvalidMsg`]: the same bytes are perfectly valid for
    /// a receiver with a higher (or no) limit. Two backends configured with
    /// different limits must not read a limit rejection as a wire-conformance
    /// divergence. It is a hard decode error — the message is rejected, never
    /// clamped or truncated — and this crate never sets a default limit.
    ///
    /// This corelib itself does **not** enforce any limit: the values are
    /// configured in sofabgen and baked into the generated decode visitor, which
    /// checks the count/length exposed by the decode callbacks *before* allocating
    /// and reports a violation as this category. See sofa-buffers/generator#102.
    LimitExceeded,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let msg = match self {
            Error::Argument => "invalid argument",
            Error::BufferFull => "output buffer full and no flush sink set",
            Error::InvalidMsg => "malformed SofaBuffers message",
            Error::LimitExceeded => "receiver-configured decode limit exceeded",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for Error {}

/// Convenience alias for fallible Sofab operations.
pub type Result<T> = core::result::Result<T, Error>;
