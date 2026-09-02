//! Invalid UTF-8 that begins **past the first chunk** of a streamed `string`
//! payload (MESSAGE_SPEC §8, CORELIB_PLAN §5.2/§6.4).
//!
//! The shared `invalid_utf8` vectors (`tests/utf8_tests.rs`) all carry a payload
//! of a handful of bytes, so the offending sequence always starts at payload
//! offset 0 and always arrives in the first `Visitor::string` call. That leaves a
//! whole class untested: an invalid sequence whose **chunk offset is at or beyond
//! the number of payload bytes fed so far**, which is what any consumer that
//! validates from the callbacks — rather than after buffering the whole field —
//! actually has to survive. A validator that looks at the first chunk only, or
//! that stops once it has seen a well-formed prefix, passes every shared vector
//! and still accepts these messages.
//!
//! The corelib's own obligation underneath that is the **payload delivery
//! contract** of [`sofab::Visitor::string`]: a constant `total`, offsets that
//! start at 0 and advance contiguously, and the concatenation of the chunks being
//! the payload byte for byte — at *any* split, including one inside the invalid
//! sequence itself, inside the `fixlen_word` and inside the field header. Break
//! any of that and a downstream validator silently stops seeing the bad bytes,
//! which is exactly the failure the shared vectors cannot catch.
//!
//! Nothing here asks the corelib to validate UTF-8: it does not (§6.4 puts that
//! in generated code, which materializes with `core::str::from_utf8`). What is
//! pinned is that the bytes handed over are complete and correctly located, and
//! that the materialization verdict is the same for every chunking.

mod common;

use common::push_varint;
use sofab::{decode, Error, IStream, Id, Status, Visitor};

/// Wire type `FIXLEN`, and the `Str` / `Blob` subtypes of its length word.
const T_FIXLEN: u64 = 0x2;
const FX_STR: u64 = 0x2;
const FX_BLOB: u64 = 0x3;

/// A `string`/`blob` field written by hand: the encoder refuses invalid UTF-8
/// under the `Str` subtype (§6.4 encode side), so these bytes cannot come from
/// `OStream`.
fn fixlen_field(id: u64, subtype: u64, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    push_varint(&mut out, (id << 3) | T_FIXLEN);
    push_varint(&mut out, ((payload.len() as u64) << 3) | subtype);
    out.extend_from_slice(payload);
    out
}

/// Payload offset at which the invalid sequence starts. Comfortably past any
/// plausible "first chunk", and past the point where a consumer that gave up
/// after a valid prefix would have stopped looking.
const INVALID_AT: usize = 200;

/// A long payload: `INVALID_AT` bytes of valid UTF-8 (ASCII plus a two-byte
/// `ä` every five bytes, so a split lands inside a multi-byte sequence often),
/// then a truncated three-byte sequence `E2 82` followed by an ASCII byte, then
/// a valid tail.
fn payload_with_late_invalid_sequence() -> Vec<u8> {
    let mut p = Vec::new();
    while p.len() < INVALID_AT {
        p.extend_from_slice("sofä".as_bytes()); // 5 bytes: s o f C3 A4
    }
    assert_eq!(
        p.len(),
        INVALID_AT,
        "prefix must land exactly on the offset"
    );
    assert!(
        core::str::from_utf8(&p).is_ok(),
        "the prefix is valid on its own"
    );
    p.extend_from_slice(&[0xE2, 0x82]); // start of a 3-byte sequence …
    p.extend_from_slice(b"A tail"); // … terminated by an ASCII byte: invalid
    assert!(
        core::str::from_utf8(&p).is_err(),
        "the whole payload is invalid"
    );
    p
}

/// The same payload with the truncated sequence completed — valid UTF-8 of the
/// same shape, for the control half of each test.
fn payload_with_a_late_multibyte_char() -> Vec<u8> {
    let mut p = Vec::new();
    while p.len() < INVALID_AT {
        p.extend_from_slice("sofä".as_bytes());
    }
    p.extend_from_slice("€".as_bytes()); // E2 82 AC — the completed sequence
    p.extend_from_slice(b"A tail");
    assert!(core::str::from_utf8(&p).is_ok());
    p
}

/// Records every `string`/`blob` callback and checks the delivery contract as it
/// goes: one field id, a constant `total`, offsets starting at 0 and advancing
/// contiguously by the length of each chunk.
#[derive(Default)]
struct PayloadTrace {
    id: Option<Id>,
    total: Option<usize>,
    /// `(offset, len)` of every callback, in order.
    calls: Vec<(usize, usize)>,
    bytes: Vec<u8>,
}

impl PayloadTrace {
    fn record(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {
        match self.id {
            None => self.id = Some(id),
            Some(seen) => assert_eq!(seen, id, "a payload must not change field id"),
        }
        match self.total {
            None => self.total = Some(total),
            Some(seen) => assert_eq!(seen, total, "`total` must not change mid-payload"),
        }
        assert_eq!(
            offset,
            self.bytes.len(),
            "chunks must be contiguous, in order and never re-delivered"
        );
        assert!(
            offset + chunk.len() <= total,
            "a chunk must not run past the declared total"
        );
        self.bytes.extend_from_slice(chunk);
        self.calls.push((offset, chunk.len()));
    }

    /// The bytes delivered by the first callback — all an eager consumer that
    /// validates per chunk and stops there would ever see.
    fn first_chunk(&self) -> &[u8] {
        let (_, len) = self.calls[0];
        &self.bytes[..len]
    }

    /// The UTF-8 verdict generated code reaches once the field is assembled.
    fn materialize(&self) -> Result<&str, Error> {
        assert_eq!(
            self.total,
            Some(self.bytes.len()),
            "payload not fully delivered"
        );
        core::str::from_utf8(&self.bytes).map_err(|_| Error::InvalidMsg)
    }
}

impl Visitor for PayloadTrace {
    fn string(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {
        self.record(id, total, offset, chunk);
    }
    fn blob(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {
        self.record(id, total, offset, chunk);
    }
}

/// Feed `msg` in `size`-byte chunks, returning the trace and the final verdict
/// of the end-of-input probe.
fn feed_in_chunks(msg: &[u8], size: usize) -> (PayloadTrace, Result<Status, Error>) {
    let mut trace = PayloadTrace::default();
    let mut is = IStream::new();
    for chunk in msg.chunks(size) {
        match is.feed(chunk, &mut trace) {
            Ok(Status::Complete) | Ok(Status::Incomplete) => {}
            Err(e) => panic!("chunk size {size}: corelib rejected a structurally valid frame: {e}"),
        }
    }
    let verdict = is.feed(&[], &mut trace);
    (trace, verdict)
}

/// The chunkings worth trying: one byte at a time, a few odd sizes that put the
/// cut in a different place on every field, and sizes near the invalid sequence.
fn chunk_sizes(msg_len: usize) -> Vec<usize> {
    let mut sizes = vec![1, 2, 3, 5, 7, 13, 64, 128, INVALID_AT, INVALID_AT + 1];
    sizes.push(msg_len - 1);
    sizes.push(msg_len);
    sizes.retain(|&s| s > 0 && s <= msg_len);
    sizes.sort_unstable();
    sizes.dedup();
    sizes
}

#[test]
fn an_invalid_sequence_past_the_first_chunk_is_rejected_at_every_chunking() {
    let payload = payload_with_late_invalid_sequence();
    let msg = fixlen_field(7, FX_STR, &payload);

    for size in chunk_sizes(msg.len()) {
        let (trace, verdict) = feed_in_chunks(&msg, size);
        assert_eq!(
            verdict,
            Ok(Status::Complete),
            "chunk size {size}: message ends at a boundary"
        );
        assert_eq!(trace.id, Some(7), "chunk size {size}");
        assert_eq!(
            trace.bytes, payload,
            "chunk size {size}: the reassembled payload must be byte-exact"
        );
        assert_eq!(
            trace.materialize(),
            Err(Error::InvalidMsg),
            "chunk size {size}: materialization must reject the late invalid sequence"
        );
        // The point of the exercise: for every chunking that splits the payload,
        // the invalid sequence starts at an offset the first callback never
        // reached — it is delivered by a *later* call, at an offset at or beyond
        // everything fed so far.
        if size <= INVALID_AT {
            assert!(
                trace.calls.len() > 1,
                "chunk size {size}: payload should arrive in several calls"
            );
            let first_len = trace.calls[0].1;
            assert!(
                INVALID_AT >= first_len,
                "chunk size {size}: the invalid sequence must start past the first chunk"
            );
            let carrier = trace
                .calls
                .iter()
                .position(|&(off, len)| off <= INVALID_AT && INVALID_AT < off + len)
                .expect("some call carries the first invalid byte");
            assert!(
                carrier > 0,
                "chunk size {size}: the invalid byte must not arrive in the first call"
            );
        }
    }

    // The control: the same payload with the sequence completed is valid at every
    // chunking, so what the test above catches is the bad bytes and not the
    // chunking itself.
    let good = payload_with_a_late_multibyte_char();
    let good_msg = fixlen_field(7, FX_STR, &good);
    for size in chunk_sizes(good_msg.len()) {
        let (trace, verdict) = feed_in_chunks(&good_msg, size);
        assert_eq!(verdict, Ok(Status::Complete), "chunk size {size}");
        assert_eq!(trace.bytes, good, "chunk size {size}");
        assert!(
            trace.materialize().is_ok(),
            "chunk size {size}: a multi-byte char split across chunks stays valid"
        );
    }
}

#[test]
fn a_validator_that_only_saw_the_first_chunk_would_accept_the_message() {
    // States the gap exactly: cut the payload on a character boundary well before
    // the invalid sequence. The first callback is valid UTF-8 all by itself, so a
    // consumer that validated it and stopped — or that only ever validated
    // `offset == 0` — reports COMPLETE for a payload the whole-field check
    // rejects. Only the callback at the later offset carries the evidence.
    let payload = payload_with_late_invalid_sequence();
    let msg = fixlen_field(7, FX_STR, &payload);
    let header_len = msg.len() - payload.len();

    let mut trace = PayloadTrace::default();
    let mut is = IStream::new();
    let first_cut = header_len + 100; // 100 = 20 × "sofä", a char boundary
    assert_eq!(
        is.feed(&msg[..first_cut], &mut trace),
        Ok(Status::Incomplete)
    );

    assert_eq!(trace.calls, vec![(0, 100)], "one call, at offset 0");
    assert!(
        core::str::from_utf8(trace.first_chunk()).is_ok(),
        "the first chunk is valid UTF-8 on its own — this is why the gap hides"
    );

    assert_eq!(is.feed(&msg[first_cut..], &mut trace), Ok(Status::Complete));
    let carrier = trace.calls[1..]
        .iter()
        .find(|&&(off, len)| off <= INVALID_AT && INVALID_AT < off + len)
        .expect("a later call carries the invalid sequence");
    assert!(
        carrier.0 >= 100,
        "the invalid sequence starts at or beyond everything fed so far"
    );
    assert_eq!(trace.materialize(), Err(Error::InvalidMsg));
}

#[test]
fn the_verdict_is_the_same_at_every_two_chunk_split() {
    // Every split point of the whole message — inside the field header varint,
    // inside the `fixlen_word`, inside the valid prefix, inside the invalid
    // sequence itself and inside the tail. §7.2: where the chunk boundary falls
    // must not change the outcome.
    let payload = payload_with_late_invalid_sequence();
    let msg = fixlen_field(7, FX_STR, &payload);

    for cut in 0..=msg.len() {
        let mut trace = PayloadTrace::default();
        let mut is = IStream::new();
        match is.feed(&msg[..cut], &mut trace) {
            Ok(Status::Complete) | Ok(Status::Incomplete) => {}
            Err(e) => panic!("cut {cut}: prefix of a valid frame reported {e}"),
        }
        match is.feed(&msg[cut..], &mut trace) {
            Ok(Status::Complete) => {}
            other => panic!("cut {cut}: completing the message reported {other:?}"),
        }
        assert_eq!(trace.bytes, payload, "cut {cut}");
        assert_eq!(trace.materialize(), Err(Error::InvalidMsg), "cut {cut}");
    }
}

#[test]
fn the_one_shot_path_delivers_the_payload_in_a_single_call() {
    // The contiguous fast path's documented shape (`Visitor::string`): one call,
    // `offset == 0`, `chunk.len() == total` — so the same payload is validated in
    // one `from_utf8` there, reaching the same verdict as the chunked path above.
    let payload = payload_with_late_invalid_sequence();
    let msg = fixlen_field(7, FX_STR, &payload);

    let mut trace = PayloadTrace::default();
    assert_eq!(decode(&msg, &mut trace), Ok(Status::Complete));
    assert_eq!(trace.calls, vec![(0, payload.len())]);
    assert_eq!(trace.materialize(), Err(Error::InvalidMsg));
}

#[test]
fn the_same_late_bytes_in_a_blob_are_delivered_and_never_a_verdict() {
    // §6.4 constrains the `string` subtype only: an identical byte run in a
    // `blob` is opaque, so it stays COMPLETE at every chunking and arrives
    // byte-exact — the chunk offsets are the same contract, the verdict is not.
    let payload = payload_with_late_invalid_sequence();
    let msg = fixlen_field(7, FX_BLOB, &payload);

    for size in chunk_sizes(msg.len()) {
        let (trace, verdict) = feed_in_chunks(&msg, size);
        assert_eq!(verdict, Ok(Status::Complete), "chunk size {size}");
        assert_eq!(trace.bytes, payload, "chunk size {size}");
    }
}
