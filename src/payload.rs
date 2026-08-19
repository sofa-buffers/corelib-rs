//! Chunk reassembly for streamed `string` / `blob` payloads.
//!
//! This is the **generated-code support layer**. What lives here carries no
//! schema knowledge whatsoever — every bound arrives as an argument, exactly as
//! [`crate::Visitor`]'s own callbacks take one — so it has the same shape for
//! every schema and is written once, here, instead of being emitted into every
//! crate `sofabgen` produces. Nothing in this module touches the wire: it works
//! purely on bytes the decoder has already handed over.

/// Reassembles a `string` or `blob` payload that arrives in more than one chunk.
///
/// [`crate::Visitor::string`] and [`crate::Visitor::blob`] deliver a payload as
/// `(total, offset, chunk)` — one call carrying the whole field on the
/// contiguous path, and any number of contiguous pieces once a transport has
/// torn the field apart (CORELIB_PLAN §5.2). A consumer that wants the field as
/// one value — a `String`, a `Vec<u8>` — has to put it back together, and that
/// is the same handful of lines for every field of every schema.
///
/// [`PayloadAcc::feed`] hands the payload back exactly once, on the call that
/// completes it, and `None` while bytes are still outstanding:
///
/// ```
/// use sofab::PayloadAcc;
///
/// let mut acc = PayloadAcc::new();
/// assert_eq!(acc.feed(5, 0, b"so"), None);                // more to come
/// assert_eq!(acc.feed(5, 2, b"fab"), Some(&b"sofab"[..])); // complete
/// ```
///
/// The whole-payload case costs nothing: a chunk that already holds the field is
/// returned **borrowed from the input buffer**, with no accumulate and no second
/// copy — this port's zero-copy [`crate::decode`] path always delivers a payload
/// that way, and a self-contained [`crate::IStream::feed`] chunk usually does.
///
/// One accumulator serves a whole message: a payload always starts at
/// `offset == 0`, which is where the previous one is dropped, so the buffer is
/// reused field after field and message after message.
///
/// # What it deliberately does not do
///
/// * **It does not validate.** The bytes come back raw, and the materialization
///   verdict stays with the caller — for a `string`, `core::str::from_utf8` on
///   what `feed` returned, whose `Err` is the `INVALID` decode outcome
///   (MESSAGE_SPEC §8, CORELIB_PLAN §6.4). That verdict belongs on the
///   *assembled* payload rather than on each chunk: a multi-byte sequence may
///   straddle a chunk boundary, so validating per chunk would reject valid text
///   and — worse — could accept a broken sequence whose halves each look
///   plausible. Feeding first and judging once is what makes the verdict
///   independent of where the chunk boundaries fell (§7.2).
/// * **It does not allocate on `total`.** `total` is decoded input: a hostile
///   message announces a gigabyte and then sends three bytes. The buffer grows
///   with the bytes that actually arrive, so an announcement that never
///   materializes costs what it actually sent. A schema `maxlen` is a separate,
///   earlier judgement — generated code latches that on
///   [`crate::Visitor::fixlen_begin`] or on the first chunk, before a byte is
///   ever fed here.
#[derive(Debug)]
pub struct PayloadAcc {
    /// Bytes of a payload seen so far. Empty whenever the payload arrived whole
    /// in one chunk, which is the case that skips this buffer entirely.
    buf: Vec<u8>,
    /// Set once the current payload has been handed back, so a stray further
    /// chunk of it cannot yield a second (and then truncated) copy.
    complete: bool,
}

impl Default for PayloadAcc {
    fn default() -> Self {
        Self::new()
    }
}

impl PayloadAcc {
    /// Create an empty accumulator. Allocates nothing until a payload actually
    /// straddles a chunk boundary.
    pub const fn new() -> Self {
        PayloadAcc {
            buf: Vec::new(),
            complete: false,
        }
    }

    /// Accept one payload chunk, as delivered to [`crate::Visitor::string`] /
    /// [`crate::Visitor::blob`], and return the whole payload once it is
    /// complete.
    ///
    /// * `Some(bytes)` — `bytes` is the payload, exactly `total` bytes long. It
    ///   borrows either from `chunk` (whole-payload case, no copy) or from the
    ///   accumulator; either way it is valid until the next call.
    /// * `None` — bytes are still outstanding; feed the next chunk.
    ///
    /// `offset` is read for one purpose: `offset == 0` marks the start of a
    /// payload and drops whatever the accumulator still held. That is what makes
    /// the accumulator self-healing — a field abandoned half way (a skipped
    /// payload, a bound that turned the message INVALID) leaves no bytes to
    /// contaminate the next one, without the caller having to reset anything.
    ///
    /// A payload is handed back **once**: further chunks of one already
    /// completed return `None` rather than a second, shorter copy. The decoder
    /// does not deliver such a chunk — a chunk with `offset >= total` is not
    /// something [`crate::IStream`] emits — but a consumer that stacks this on
    /// another source, or hands the same accumulator two payloads at once, gets
    /// a defined answer instead of a truncated field.
    ///
    /// ```
    /// use sofab::PayloadAcc;
    ///
    /// let mut acc = PayloadAcc::new();
    ///
    /// // Whole payload in one chunk: returned borrowed from `chunk` itself.
    /// assert_eq!(acc.feed(3, 0, b"abc"), Some(&b"abc"[..]));
    ///
    /// // A payload abandoned half way is dropped when the next one starts.
    /// assert_eq!(acc.feed(9, 0, b"lost"), None);
    /// assert_eq!(acc.feed(2, 0, b"o"), None);
    /// assert_eq!(acc.feed(2, 1, b"k"), Some(&b"ok"[..]));
    /// ```
    pub fn feed<'a>(
        &'a mut self,
        total: usize,
        offset: usize,
        chunk: &'a [u8],
    ) -> Option<&'a [u8]> {
        if offset == 0 {
            self.buf.clear();
            self.complete = false;
            if chunk.len() >= total {
                // The whole field is here. Hand back the input slice: building
                // the value from it directly is what saves the accumulate pass
                // and the second copy, and it is the common case — the
                // contiguous `decode` path never splits a payload at all.
                self.complete = true;
                return Some(&chunk[..total]);
            }
        } else if self.complete {
            return None;
        }
        self.buf.extend_from_slice(chunk);
        if self.buf.len() < total {
            return None;
        }
        self.complete = true;
        // Slice to `total` rather than handing back the buffer: a source that
        // over-delivers must not widen the field beyond what was announced.
        Some(&self.buf[..total])
    }

    /// Drop a partially accumulated payload, keeping the buffer's capacity so
    /// the next message reuses it.
    ///
    /// Rarely needed — the next payload's first chunk does the same — but it is
    /// how a consumer explicitly drops the tail of an abandoned message before
    /// reusing its decoder, mirroring [`crate::IStream::reset`].
    pub fn reset(&mut self) {
        self.buf.clear();
        self.complete = false;
    }

    /// Number of payload bytes currently held.
    ///
    /// Zero for a payload that arrived whole in one chunk (nothing was buffered)
    /// and zero between messages; non-zero exactly while a split payload is
    /// still incomplete.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed `payload` in fixed-size pieces of `step` bytes, and return what the
    /// accumulator finally yielded — the way generated code materializes a
    /// field, just collected instead of placed.
    fn assemble(payload: &[u8], step: usize) -> Option<Vec<u8>> {
        let mut acc = PayloadAcc::new();
        let mut done = None;
        let mut offset = 0;
        while offset < payload.len() {
            let end = (offset + step).min(payload.len());
            if let Some(bytes) = acc.feed(payload.len(), offset, &payload[offset..end]) {
                assert!(done.is_none(), "payload handed back twice");
                done = Some(bytes.to_vec());
            }
            offset = end;
        }
        done
    }

    #[test]
    fn whole_payload_in_one_chunk_is_handed_straight_back() {
        let mut acc = PayloadAcc::new();
        assert_eq!(acc.feed(5, 0, b"sofab"), Some(&b"sofab"[..]));
        // The point of the fast path: nothing was buffered on the way.
        assert_eq!(acc.buffered(), 0);
    }

    #[test]
    fn every_split_of_a_payload_yields_the_same_bytes() {
        // The obligation the shared vectors cannot express: the assembled value
        // must not depend on where the chunk boundaries fell. `step` walks every
        // fixed split, and the explicit two-part loop walks every single cut
        // point 1..n — including the ones inside the multi-byte sequences.
        let payload = "sofäbuffers — ünicode ✓".as_bytes();
        for step in 1..=payload.len() + 2 {
            assert_eq!(
                assemble(payload, step).as_deref(),
                Some(payload),
                "split into {step}-byte chunks"
            );
        }
        for cut in 1..payload.len() {
            let mut acc = PayloadAcc::new();
            assert_eq!(acc.feed(payload.len(), 0, &payload[..cut]), None);
            assert_eq!(
                acc.feed(payload.len(), cut, &payload[cut..]),
                Some(payload),
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn empty_payload_completes_immediately() {
        let mut acc = PayloadAcc::new();
        assert_eq!(acc.feed(0, 0, b""), Some(&b""[..]));
        assert_eq!(acc.buffered(), 0);
    }

    #[test]
    fn incomplete_payload_yields_nothing() {
        let mut acc = PayloadAcc::new();
        assert_eq!(acc.feed(4, 0, b"so"), None);
        assert_eq!(acc.buffered(), 2);
        assert_eq!(acc.feed(4, 2, b"f"), None);
        assert_eq!(acc.buffered(), 3);
    }

    #[test]
    fn a_new_payload_drops_what_the_previous_one_left() {
        let mut acc = PayloadAcc::new();
        assert_eq!(acc.feed(16, 0, b"abandoned"), None);
        // No reset in between: `offset == 0` is the reset.
        assert_eq!(acc.feed(4, 0, b"so"), None);
        assert_eq!(acc.feed(4, 2, b"fa"), Some(&b"sofa"[..]));
    }

    #[test]
    fn a_completed_payload_is_not_handed_back_twice() {
        // A chunk with `offset >= total`: the decoder never emits one, and the
        // shared `invalid_utf8` vectors never reach the case, so this is the
        // boundary that would otherwise go untested. A second, shorter copy here
        // would truncate a field that was already correct.
        let mut acc = PayloadAcc::new();
        assert_eq!(acc.feed(4, 0, b"so"), None);
        assert_eq!(acc.feed(4, 2, b"fa"), Some(&b"sofa"[..]));
        assert_eq!(acc.feed(4, 4, b""), None);
        assert_eq!(acc.feed(4, 4, b"more"), None);

        // Same for the fast path, which hands the payload back without buffering.
        let mut acc = PayloadAcc::new();
        assert_eq!(acc.feed(4, 0, b"sofa"), Some(&b"sofa"[..]));
        assert_eq!(acc.feed(4, 4, b""), None);
    }

    #[test]
    fn over_delivery_does_not_widen_the_field() {
        // Both paths cut at `total`: a source that hands over more than was
        // announced must not be able to lengthen the value.
        let mut acc = PayloadAcc::new();
        assert_eq!(acc.feed(3, 0, b"sofabuffers"), Some(&b"sof"[..]));

        let mut acc = PayloadAcc::new();
        assert_eq!(acc.feed(3, 0, b"so"), None);
        assert_eq!(acc.feed(3, 2, b"fabuffers"), Some(&b"sof"[..]));
    }

    #[test]
    fn reset_drops_a_partial_payload() {
        let mut acc = PayloadAcc::new();
        assert_eq!(acc.feed(8, 0, b"partial"), None);
        acc.reset();
        assert_eq!(acc.buffered(), 0);
        // The dropped bytes are gone rather than merely hidden: a continuation
        // of the abandoned payload cannot complete it out of stale state.
        assert_eq!(acc.feed(8, 7, b"!"), None);
    }

    #[test]
    fn the_buffer_is_reused_across_payloads() {
        // Nothing here is a promise about capacity numbers — only that a second
        // split payload does not start from a fresh allocation, which is why one
        // accumulator is kept per decoder rather than one per field.
        let mut acc = PayloadAcc::new();
        assert_eq!(acc.feed(6, 0, b"sof"), None);
        assert_eq!(acc.feed(6, 3, b"abs"), Some(&b"sofabs"[..]));
        let cap = acc.buf.capacity();
        assert!(cap >= 6);
        assert_eq!(acc.feed(6, 0, b"buf"), None);
        assert_eq!(acc.feed(6, 3, b"fer"), Some(&b"buffer"[..]));
        assert_eq!(acc.buf.capacity(), cap);
    }

    #[test]
    fn does_not_allocate_on_an_announced_total() {
        // The eager-allocation guard: `total` is decoded input, so a huge
        // announcement followed by a few bytes must cost those few bytes.
        let mut acc = PayloadAcc::new();
        assert_eq!(acc.feed(1 << 30, 0, b"three"), None);
        assert!(
            acc.buf.capacity() < 1024,
            "reserved {} bytes on an announcement of 1 GiB",
            acc.buf.capacity()
        );
    }

    #[test]
    fn default_is_an_empty_accumulator() {
        let mut acc = PayloadAcc::default();
        assert_eq!(acc.buffered(), 0);
        assert_eq!(acc.feed(2, 0, b"ok"), Some(&b"ok"[..]));
    }
}
