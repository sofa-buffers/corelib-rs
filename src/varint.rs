//! Base-128 varint and ZigZag codecs.
//!
//! The decode side here is the speed-critical core: a slice reader that
//! **advances a cursor over a contiguous buffer** (the technique borrowed from
//! the C++ high-speed port / Protocol Buffers). When at least one full varint's
//! worth of bytes is guaranteed present, [`read_varint`] decodes without
//! per-byte bounds checks; only near the end of the buffer does it fall back to
//! a checked loop that can report "need more bytes" for the streaming decoder.

use crate::{Error, Result, Signed, Unsigned};

/// Maximum number of bytes a [`Unsigned`]-width varint can occupy (10 for
/// `u64`, 5 for `u32`).
pub(crate) const MAX_VARINT_LEN: usize = (Unsigned::BITS as usize + 6) / 7;

/// Read one base-128 varint from `buf` starting at `*pos`.
///
/// * `Ok(Some(v))` — a full varint was decoded; `*pos` advanced past it.
/// * `Ok(None)` — `buf` ends mid-varint; `*pos` is left unchanged so the caller
///   can carry the partial bytes to the next chunk.
/// * `Err(InvalidMsg)` — the varint is longer than [`Unsigned`] allows.
#[inline]
pub(crate) fn read_varint(buf: &[u8], pos: &mut usize) -> Result<Option<Unsigned>> {
    let start = *pos;
    if buf.len() - start >= MAX_VARINT_LEN {
        // Fast path: a complete varint is guaranteed to fit, so skip per-byte
        // bounds checks and just advance a pointer.
        // SAFETY: at least `MAX_VARINT_LEN` bytes remain from `start`, and the
        // loop reads at most that many before terminating or erroring.
        unsafe { read_varint_unchecked(buf.as_ptr(), start, pos) }.map(Some)
    } else {
        read_varint_checked(buf, pos)
    }
}

/// Fast-path decode: no bounds checks. `start` must have at least
/// [`MAX_VARINT_LEN`] readable bytes at `base + start`.
#[inline]
unsafe fn read_varint_unchecked(
    base: *const u8,
    start: usize,
    pos: &mut usize,
) -> Result<Unsigned> {
    let mut value: Unsigned = 0;
    let mut shift: u32 = 0;
    let mut i = start;
    loop {
        let byte = *base.add(i);
        i += 1;
        value |= ((byte & 0x7F) as Unsigned) << shift;
        if byte & 0x80 == 0 {
            *pos = i;
            return Ok(value);
        }
        shift += 7;
        if shift >= Unsigned::BITS {
            return Err(Error::InvalidMsg);
        }
    }
}

/// Slow-path decode used within the last [`MAX_VARINT_LEN`] − 1 bytes of a
/// buffer, where the varint may legitimately be split across chunks.
#[inline]
fn read_varint_checked(buf: &[u8], pos: &mut usize) -> Result<Option<Unsigned>> {
    let mut value: Unsigned = 0;
    let mut shift: u32 = 0;
    let mut i = *pos;
    while i < buf.len() {
        let byte = buf[i];
        i += 1;
        value |= ((byte & 0x7F) as Unsigned) << shift;
        if byte & 0x80 == 0 {
            *pos = i;
            return Ok(Some(value));
        }
        shift += 7;
        if shift >= Unsigned::BITS {
            return Err(Error::InvalidMsg);
        }
    }
    Ok(None)
}

/// ZigZag encode a signed value to its unsigned varint representation.
#[inline]
pub(crate) fn zigzag_encode(v: Signed) -> Unsigned {
    // `wrapping_shl` avoids the debug-mode overflow panic for `Signed::MIN`.
    (v.wrapping_shl(1) ^ (v >> (Signed::BITS - 1))) as Unsigned
}

/// ZigZag decode an unsigned varint back to a signed value.
#[inline]
pub(crate) fn zigzag_decode(u: Unsigned) -> Signed {
    ((u >> 1) as Signed) ^ -((u & 1) as Signed)
}
