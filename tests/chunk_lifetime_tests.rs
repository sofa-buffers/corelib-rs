//! A fed chunk is borrowed **only for the duration of the `feed` call**
//! (CORELIB_PLAN §6): once `feed` returns, the caller may reuse, overwrite or
//! free that memory and the decoded message must not be affected.
//!
//! §7.2 item 4 makes it a checked property rather than a stated one — scrub every
//! chunk after `feed` returns and assert the message is unchanged. A decoder that
//! kept a slice into a chunk reads back the fill pattern, and nothing else in the
//! test suite would notice: every other chunked test hands `feed` memory that
//! happens to stay alive.

use sofab::{decode, ArrayKind, IStream, Id, OStream, Signed, Status, Unsigned, Visitor};

const SCRUB: u8 = 0xA5;

#[derive(Debug, Default, PartialEq)]
struct Message {
    unsigned: Vec<(Id, Unsigned)>,
    signed: Vec<(Id, Signed)>,
    fp64: Vec<(Id, u64)>,
    strings: Vec<(Id, Vec<u8>)>,
    blobs: Vec<(Id, Vec<u8>)>,
    arrays: Vec<(Id, ArrayKind, usize)>,
    seq_begin: Vec<Id>,
    seq_ends: usize,
}

/// Accumulates payloads the way generated code does — by copying out of the
/// chunk it is handed, during the call.
impl Visitor for Message {
    fn unsigned(&mut self, id: Id, v: Unsigned) {
        self.unsigned.push((id, v));
    }
    fn signed(&mut self, id: Id, v: Signed) {
        self.signed.push((id, v));
    }
    fn fp64(&mut self, id: Id, v: f64) {
        self.fp64.push((id, v.to_bits()));
    }
    fn string(&mut self, id: Id, _total: usize, offset: usize, chunk: &[u8]) {
        if offset == 0 {
            self.strings.push((id, chunk.to_vec()));
        } else {
            self.strings.last_mut().unwrap().1.extend_from_slice(chunk);
        }
    }
    fn blob(&mut self, id: Id, _total: usize, offset: usize, chunk: &[u8]) {
        if offset == 0 {
            self.blobs.push((id, chunk.to_vec()));
        } else {
            self.blobs.last_mut().unwrap().1.extend_from_slice(chunk);
        }
    }
    fn array_begin(&mut self, id: Id, kind: ArrayKind, count: usize) {
        self.arrays.push((id, kind, count));
    }
    fn sequence_begin(&mut self, id: Id) {
        self.seq_begin.push(id);
    }
    fn sequence_end(&mut self) {
        self.seq_ends += 1;
    }
}

/// A message with payloads long enough to straddle any chunk size below.
fn wire() -> Vec<u8> {
    let mut buf = vec![0u8; 512];
    let used = {
        let mut os = OStream::new(&mut buf);
        os.write_unsigned(1, 0xDEAD_BEEF).unwrap();
        os.write_signed(2, -123_456).unwrap();
        os.write_str(3, "a string long enough to straddle several chunks")
            .unwrap();
        os.write_blob(4, &(0..64u8).collect::<Vec<_>>()).unwrap();
        os.write_fp64(5, core::f64::consts::E).unwrap();
        os.write_array_unsigned(6, &[1u64, 300, 70_000, 5_000_000_000])
            .unwrap();
        os.write_sequence_begin_lazy(7).unwrap();
        os.write_str(1, "nested").unwrap();
        os.write_sequence_end().unwrap();
        os.bytes_used()
    };
    buf.truncate(used);
    buf
}

/// Feed `wire` in `size`-byte pieces, each in freshly owned memory that is
/// scrubbed the instant `feed` returns.
fn decode_scrubbing(wire: &[u8], size: usize) -> Message {
    let mut sink = Message::default();
    let mut is = IStream::new();
    let mut last = Ok(Status::Complete);
    for piece in wire.chunks(size) {
        let mut owned = piece.to_vec();
        last = is.feed(&owned, &mut sink);
        assert!(
            matches!(last, Ok(Status::Complete) | Ok(Status::Incomplete)),
            "feed reported {last:?}"
        );
        // The chunk's memory is the caller's again the moment `feed` returned.
        owned.fill(SCRUB);
        // Keep it alive but poisoned until the next `feed`, so a decoder holding
        // a slice reads the fill pattern rather than freed memory — a far more
        // reliable way to fail than hoping the allocator reuses the page.
        drop(owned);
    }
    assert_eq!(
        last,
        Ok(Status::Complete),
        "the final chunk must complete the message"
    );
    sink
}

#[test]
fn scrubbing_every_chunk_after_feed_leaves_the_message_unchanged() {
    let wire = wire();

    let mut reference = Message::default();
    assert_eq!(
        IStream::new().feed(&wire, &mut reference),
        Ok(Status::Complete)
    );

    // Sanity: the reference actually carries the payloads we are protecting.
    assert_eq!(reference.strings.len(), 2);
    assert_eq!(reference.blobs.len(), 1);
    assert!(!reference.strings[0].1.contains(&SCRUB));

    for size in 1..=wire.len() {
        assert_eq!(
            decode_scrubbing(&wire, size),
            reference,
            "chunk size {size} did not survive the scrub"
        );
    }
}

/// The same property where it is easiest to get wrong: a payload that arrives
/// **whole inside one chunk**. That is the case a decoder is tempted to hand on
/// as a borrowed slice, because it looks exactly like the one-shot fast path —
/// and §6 forbids exactly that, since the caller's obligation must not depend on
/// where the chunk boundaries happened to fall.
#[test]
fn a_payload_whole_inside_one_chunk_is_still_copied_out() {
    let mut buf = vec![0u8; 128];
    let used = {
        let mut os = OStream::new(&mut buf);
        os.write_str(1, "self-contained").unwrap();
        os.bytes_used()
    };
    buf.truncate(used);

    let mut sink = Message::default();
    {
        let mut owned = buf.clone();
        let mut is = IStream::new();
        assert_eq!(is.feed(&owned, &mut sink), Ok(Status::Complete));
        owned.fill(SCRUB);
    }

    assert_eq!(sink.strings, [(1, b"self-contained".to_vec())]);
}

/// **The one-shot buffer too** (§7.2 item 4, last bullet; §6.7.1).
///
/// "Run `decode(buffer)`, scrub the whole buffer, and assert the decoded message
/// is unchanged. The one-shot path has no view exemption, and **this is the test
/// that proves it**; a port that borrows from the buffer it was handed passes
/// every other item on this list."
///
/// The two tests above both go through `IStream::feed`. That `decode` is
/// `IStream::new().feed(…)` in this port makes the outcome certain — it does not
/// make the test present, and §13 lists this as the proof of §6.7.
#[test]
fn scrubbing_the_one_shot_buffer_leaves_the_message_unchanged() {
    let reference_wire = wire();
    let mut reference = Message::default();
    assert_eq!(
        decode(&reference_wire, &mut reference),
        Ok(Status::Complete)
    );
    assert_eq!(reference.strings.len(), 2);
    assert_eq!(reference.blobs.len(), 1);

    // A buffer of its own, so the scrub cannot be confused with the reference's.
    let mut owned = wire();
    let mut sink = Message::default();
    assert_eq!(decode(&owned, &mut sink), Ok(Status::Complete));
    owned.fill(SCRUB);

    assert_eq!(
        sink, reference,
        "the one-shot decode borrowed from its buffer"
    );
    assert!(!sink.strings[0].1.contains(&SCRUB));
    assert!(!sink.blobs[0].1.contains(&SCRUB));
}
