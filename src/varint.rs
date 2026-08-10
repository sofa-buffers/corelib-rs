//! Base-128 varint and ZigZag codecs.
//!
//! This is the speed-critical core of the port, and it is written for the
//! contiguous-buffer case: a cursor **advances over a slice** (the technique
//! borrowed from the C++ high-speed port / Protocol Buffers) and the shape of
//! the decoder is chosen by how many bytes are known to be readable.
//!
//! * **One byte** — the overwhelmingly common case (small ids, small values,
//!   sequence markers) is a load, a sign test and a store, inlined into the
//!   caller.
//! * **Ten bytes readable** — [`read_varint_wide`] loads eight bytes at once and
//!   finds the terminator with a SWAR mask, then compacts the 7-bit groups with
//!   three shift/mask rounds instead of walking the bytes one at a time. No
//!   per-byte bounds check and no per-byte overflow check: only the tenth byte
//!   can overflow a `u64`, so that is the only one tested.
//! * **Fewer than ten bytes** — [`read_varint_tail`], a checked byte loop that
//!   can stop and report [`Error::Incomplete`] so the streaming decoder can
//!   carry the partial bytes into the next chunk.
//!
//! The encode side mirrors it: [`write_varint_unchecked`] fills a caller-checked
//! run of at least [`MAX_VARINT_LEN`] bytes with no per-byte bookkeeping.

use crate::{Error, Result, Signed, Unsigned};

/// Maximum number of bytes a [`Unsigned`]-width varint can occupy (10 for
/// `u64`).
pub(crate) const MAX_VARINT_LEN: usize = (Unsigned::BITS as usize + 6) / 7;

// The wide decoder and the unchecked writer below are hand-unrolled for the
// 64-bit value type this build pins (`types::Unsigned`). There are no feature
// flags that could narrow it, but assert the assumption rather than trust it.
const _: () = assert!(Unsigned::BITS == 64 && MAX_VARINT_LEN == 10);

/// Continuation-bit mask over eight packed bytes.
const MSB: u64 = 0x8080_8080_8080_8080;
/// Payload-bit mask over eight packed bytes.
const LOW7: u64 = 0x7F7F_7F7F_7F7F_7F7F;

/// `CONT[n]` sets the continuation bit on the first `n - 1` of eight packed
/// bytes — the mask an `n`-byte varint ORs into its payload. Indexed 1..=8.
const CONT: [u64; 9] = {
    let mut t = [0u64; 9];
    let mut n = 1;
    while n <= 8 {
        t[n] = MSB & !(!0u64 << (8 * (n - 1)));
        n += 1;
    }
    t
};

/// `KEEP7[n]` keeps the payload bits of the first `n` of eight packed bytes and
/// clears everything else — continuation bits and any byte past the varint's
/// terminator. Indexed 1..=8.
const KEEP7: [u64; 9] = {
    let mut t = [0u64; 9];
    let mut n = 1;
    while n <= 8 {
        t[n] = LOW7 & (!0u64 >> (64 - 8 * n));
        n += 1;
    }
    t
};

/// Read one base-128 varint from `buf` starting at `*pos`.
///
/// * `Ok(v)` — a full varint was decoded; `*pos` advanced past it.
/// * `Err(Incomplete)` — `buf` ends mid-varint; `*pos` is left unchanged so the
///   caller can carry the partial bytes to the next chunk. This is the
///   truncation outcome, not a rejection (MESSAGE_SPEC §7).
/// * `Err(InvalidMsg)` — the varint is longer than [`Unsigned`] allows.
#[inline(always)]
pub(crate) fn read_varint(buf: &[u8], pos: &mut usize) -> Result<Unsigned> {
    let start = *pos;
    if start >= buf.len() {
        return Err(Error::Incomplete);
    }
    // SAFETY: `start < buf.len()`, checked above.
    let b0 = unsafe { *buf.get_unchecked(start) };
    if b0 < 0x80 {
        // Single-byte varint: every field id below 16, every value below 128,
        // every element count below 128, every sequence marker.
        *pos = start + 1;
        return Ok(b0 as Unsigned);
    }
    if buf.len() - start >= MAX_VARINT_LEN {
        // SAFETY: at least `MAX_VARINT_LEN` bytes are readable from `start`.
        unsafe { read_varint_wide(buf.as_ptr().add(start), start, pos) }
    } else {
        // The tail returns its length rather than writing the cursor back: it is
        // the one varint path the optimizer may leave outlined, and handing it a
        // `&mut` to the caller's cursor would pin that cursor to the stack for
        // every varint in the message.
        let (value, len) = read_varint_tail(buf, start)?;
        *pos = start + len;
        Ok(value)
    }
}

/// [`read_varint`] for a cursor already known to have a full varint's worth of
/// readable bytes ahead of it — no truncation is possible, so the only error is
/// a value too wide for [`Unsigned`].
///
/// # Safety
///
/// `base.add(*pos)` must have [`MAX_VARINT_LEN`] readable bytes.
#[inline(always)]
pub(crate) unsafe fn read_varint_ready(base: *const u8, pos: &mut usize) -> Result<Unsigned> {
    let start = *pos;
    let b0 = *base.add(start);
    if b0 < 0x80 {
        *pos = start + 1;
        return Ok(b0 as Unsigned);
    }
    read_varint_wide(base.add(start), start, pos)
}

/// Compact eight 7-bit payload groups (one per byte of `x`) into the low 56 bits.
///
/// Three shift/mask rounds merge the groups pairwise — 7-bit groups into 14, 14
/// into 28, 28 into 56 — which costs a constant handful of instructions instead
/// of one shift-and-or per byte. `x` must already have its continuation bits
/// cleared, and any byte past the varint's terminator must already be zeroed.
#[inline(always)]
fn compact7(x: u64) -> u64 {
    let x = (x & 0x007F_007F_007F_007F) | ((x & 0x7F00_7F00_7F00_7F00) >> 1);
    let x = (x & 0x0000_3FFF_0000_3FFF) | ((x & 0x3FFF_0000_3FFF_0000) >> 2);
    (x & 0x0000_0000_0FFF_FFFF) | ((x & 0x0FFF_FFFF_0000_0000) >> 4)
}

/// Decode a varint knowing that [`MAX_VARINT_LEN`] bytes are readable at `p`.
///
/// `start` is `p`'s offset within the caller's buffer; `*pos` is advanced to
/// just past the varint. The first byte is already known to have its
/// continuation bit set (the caller took the one-byte path otherwise), but this
/// function does not rely on that.
///
/// # Safety
///
/// `p` must point at [`MAX_VARINT_LEN`] readable bytes.
#[inline(always)]
unsafe fn read_varint_wide(p: *const u8, start: usize, pos: &mut usize) -> Result<Unsigned> {
    // One unaligned 8-byte load covers every varint up to 56 bits of payload.
    let w = u64::from_le_bytes(core::ptr::read_unaligned(p as *const [u8; 8]));
    let terminators = !w & MSB;
    if terminators != 0 {
        // The lowest clear continuation bit is the last byte of this varint.
        let len = (terminators.trailing_zeros() as usize >> 3) + 1;
        *pos = start + len;
        // One mask drops both the continuation bits and every byte past the
        // terminator. SAFETY: `len` is 1..=8 by construction.
        return Ok(compact7(w & *KEEP7.get_unchecked(len)));
    }

    // All eight bytes continue: 56 payload bits so far, at least nine bytes.
    let mut value = compact7(w & LOW7);
    let b8 = *p.add(8);
    value |= ((b8 & 0x7F) as Unsigned) << 56;
    if b8 < 0x80 {
        *pos = start + 9;
        return Ok(value);
    }

    // Tenth byte: only bit 63 is left, so anything but 0 or 1 either overflows
    // the value or asks for an eleventh byte. Both are malformed.
    let b9 = *p.add(9);
    if b9 > 1 {
        return Err(Error::InvalidMsg);
    }
    value |= (b9 as Unsigned) << 63;
    *pos = start + MAX_VARINT_LEN;
    Ok(value)
}

/// Checked byte loop for the last [`MAX_VARINT_LEN`] − 1 bytes of a buffer,
/// where the varint may legitimately be split across chunks. Returns the value
/// and the number of bytes it occupied.
///
/// **Fewer than [`MAX_VARINT_LEN`] readable bytes is a precondition, not a
/// hint**: a varint with a full ten bytes in front of it is
/// [`read_varint_wide`]'s job, and that is where — and only where — the 64-bit
/// bound is enforced. At most nine bytes are readable here, so `shift` tops out
/// at 56 and no byte this loop reads can overflow the value; a run of
/// continuations that reaches the end of the buffer is [`Error::Incomplete`],
/// because the byte that terminates it may still arrive in the next chunk.
#[inline]
fn read_varint_tail(buf: &[u8], start: usize) -> Result<(Unsigned, usize)> {
    debug_assert!(
        buf.len() - start < MAX_VARINT_LEN,
        "a full-width varint run belongs to read_varint_wide"
    );
    let mut value: Unsigned = 0;
    let mut shift: u32 = 0;
    let mut i = start;
    while i < buf.len() {
        let byte = buf[i];
        i += 1;
        value |= ((byte & 0x7F) as Unsigned) << shift;
        if byte & 0x80 == 0 {
            return Ok((value, i - start));
        }
        shift += 7;
    }
    Err(Error::Incomplete)
}

/// Spread the low 56 bits of `x` into eight 7-bit groups, one per byte — the
/// inverse of [`compact7`], and the same three-round shape in reverse.
#[inline(always)]
fn spread7(x: u64) -> u64 {
    let x = (x & 0x0000_0000_0FFF_FFFF) | ((x & 0x00FF_FFFF_F000_0000) << 4);
    let x = (x & 0x0000_3FFF_0000_3FFF) | ((x & 0x0FFF_C000_0FFF_C000) << 2);
    (x & 0x007F_007F_007F_007F) | ((x & 0x3F80_3F80_3F80_3F80) << 1)
}

/// Number of bytes `value` occupies as a base-128 varint (1..=[`MAX_VARINT_LEN`]).
#[inline(always)]
pub(crate) fn varint_len(value: Unsigned) -> usize {
    let bits = Unsigned::BITS - value.leading_zeros(); // 0 for `value == 0`
    ((bits.max(1) + 6) / 7) as usize
}

/// Write `value` as a base-128 varint at `dst`, returning the number of bytes
/// written (1..=[`MAX_VARINT_LEN`]).
///
/// The whole encoding is computed in registers — group spreading, continuation
/// bits, length — and committed as one 8-byte store (plus up to two trailing
/// bytes), rather than a shift-mask-store loop per byte.
///
/// # Safety
///
/// `dst` must point at [`MAX_VARINT_LEN`] writable bytes. Bytes between the end
/// of the varint and that bound are left **unspecified**: this writes eight
/// bytes whatever the varint's length, so the caller must treat only the
/// returned count as meaningful (as every caller here does — the cursor advances
/// by exactly `n`, and the next write covers the rest).
#[inline(always)]
pub(crate) unsafe fn write_varint_unchecked(dst: *mut u8, value: Unsigned) -> usize {
    // `spread7` reads only the low 56 bits, so this is the payload of the first
    // eight bytes either way.
    let spread = spread7(value);

    if value >> 56 == 0 {
        // Everything fits in the eight-byte store: set a continuation bit on
        // all but the last byte and commit. The length is needed only here.
        let n = varint_len(value);
        // SAFETY: `varint_len` returns 1..=8 for a value below 2^56.
        let cont = *CONT.get_unchecked(n);
        core::ptr::write_unaligned(dst as *mut [u8; 8], (spread | cont).to_le_bytes());
        return n;
    }

    // 57..64 bits: eight continuing bytes carry the low 56, then one or two more.
    core::ptr::write_unaligned(dst as *mut [u8; 8], (spread | MSB).to_le_bytes());
    let hi = (value >> 56) as u8;
    if value >> 63 == 0 {
        // 57..63 bits: bit 63 is clear, so the ninth byte terminates.
        *dst.add(8) = hi;
        9
    } else {
        *dst.add(8) = hi | 0x80;
        *dst.add(9) = 1; // bit 63, the only payload the tenth byte can carry
        MAX_VARINT_LEN
    }
}

/// [`write_varint_unchecked`] for a value that is *usually* one byte — a field
/// header (ids below 16), a fixlen subtype word, an element count, a sequence
/// tag. Placing one byte through the group spreading means computing a whole
/// eight-byte store for it, so the narrow case is worth a test of its own.
///
/// Deliberately **not** folded into `write_varint_unchecked`: the array element
/// loop writes arbitrary user values, and there a never-taken branch is pure
/// cost — biasing it measures +6.6 % on `encode: u64 array (1000)`, whose values
/// are full-width by construction.
///
/// # Safety
///
/// `dst` must point at [`MAX_VARINT_LEN`] writable bytes.
#[inline(always)]
pub(crate) unsafe fn write_varint_unchecked_narrow(dst: *mut u8, value: Unsigned) -> usize {
    if value < 0x80 {
        *dst = value as u8;
        return 1;
    }
    write_varint_unchecked(dst, value)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal base-128 encoding of `value`, written the slow obvious way so the
    /// decoder tests do not lean on the writer they sit next to.
    fn encode(mut value: Unsigned) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let byte = (value & 0x7F) as u8;
            value >>= 7;
            if value == 0 {
                out.push(byte);
                return out;
            }
            out.push(byte | 0x80);
        }
    }

    /// Widest value that still encodes in `len` bytes, for `len` in 1..=9.
    fn widest_in(len: u32) -> Unsigned {
        Unsigned::MAX >> (Unsigned::BITS - 7 * len)
    }

    /// `read_varint` takes the tail path only when fewer than `MAX_VARINT_LEN`
    /// bytes are readable, so hand it a buffer that is exactly the varint.
    fn decode_via_tail(bytes: &[u8]) -> Result<(Unsigned, usize)> {
        assert!(
            bytes.len() < MAX_VARINT_LEN,
            "that buffer takes the wide path"
        );
        let mut pos = 0;
        let value = read_varint(bytes, &mut pos)?;
        let direct = read_varint_tail(bytes, 0);
        assert_eq!(direct, Ok((value, pos)), "tail and read_varint disagree");
        Ok((value, pos))
    }

    /// Every length the tail can be handed — 1..=`MAX_VARINT_LEN` − 1 — decodes.
    /// The nine-byte row is the widest one: its last byte is read with `shift`
    /// at 56, which is the largest shift this loop can ever reach.
    #[test]
    fn tail_decodes_every_length_it_can_be_handed() {
        for len in 1..MAX_VARINT_LEN as u32 + 1 - 1 {
            let value = widest_in(len);
            let bytes = encode(value);
            assert_eq!(bytes.len(), len as usize);
            assert_eq!(decode_via_tail(&bytes), Ok((value, len as usize)));
        }
        // The widest nine-byte varint carries 63 payload bits.
        assert_eq!(widest_in(9), (1 << 63) - 1);
    }

    /// A varint cut short is `Incomplete` — the truncation outcome — and the
    /// cursor stays put so the streaming decoder can carry the bytes forward.
    #[test]
    fn tail_reports_incomplete_for_every_truncation() {
        for len in 2..MAX_VARINT_LEN as u32 {
            let bytes = encode(widest_in(len));
            for cut in 1..bytes.len() {
                let head = &bytes[..cut];
                let mut pos = 0;
                assert_eq!(read_varint(head, &mut pos), Err(Error::Incomplete));
                assert_eq!(pos, 0);
                assert_eq!(read_varint_tail(head, 0), Err(Error::Incomplete));
            }
        }
    }

    /// Nine continuing bytes are *not* a rejection: a tenth byte may still
    /// arrive and terminate the varint legitimately, so the tail defers. The
    /// 64-bit bound is the wide path's business, not the tail's.
    #[test]
    fn tail_defers_a_run_of_continuations() {
        let all_continue = [0x80u8; MAX_VARINT_LEN - 1];
        assert_eq!(read_varint_tail(&all_continue, 0), Err(Error::Incomplete));
        let mut pos = 0;
        assert_eq!(read_varint(&all_continue, &mut pos), Err(Error::Incomplete));
    }

    /// The 64-bit bound is enforced in exactly one place — the wide path — and
    /// it is enforced on the tenth byte.
    #[test]
    fn the_wide_path_owns_the_64_bit_bound() {
        let ten = encode(Unsigned::MAX); // ends in 0x01: bit 63, the widest tenth byte
        assert_eq!(ten.len(), MAX_VARINT_LEN);
        assert_eq!(ten[MAX_VARINT_LEN - 1], 0x01);
        let mut pos = 0;
        assert_eq!(read_varint(&ten, &mut pos), Ok(Unsigned::MAX));
        assert_eq!(pos, MAX_VARINT_LEN);

        for tenth in [0x02u8, 0x7F, 0x80, 0xFF] {
            let mut over = vec![0x80u8; MAX_VARINT_LEN - 1];
            over.push(tenth);
            let mut pos = 0;
            assert_eq!(read_varint(&over, &mut pos), Err(Error::InvalidMsg));
        }
    }

    /// `read_varint_tail` may only be handed the last `MAX_VARINT_LEN` − 1
    /// bytes of a buffer; anything wider belongs to `read_varint_wide`, which is
    /// where the 64-bit bound is checked. The debug assertion documents that
    /// invariant and catches a caller that forgets it.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "read_varint_wide")]
    fn tail_refuses_a_run_the_wide_path_owns() {
        let _ = read_varint_tail(&[0x80u8; MAX_VARINT_LEN], 0);
    }
}
