//! [`PayloadAcc`] against the real decoder, at every chunk boundary there is.
//!
//! The unit tests next to the type (`src/payload.rs`) pin its behaviour on
//! synthetic `(total, offset, chunk)` triples. This file closes the other half:
//! the accumulator is driven by [`IStream::feed`] itself, in the shape generated
//! code uses it — one accumulator per decoder, `feed` per callback, materialize
//! on the call that completes the payload — over **every** chunk size a
//! transport could hand over.
//!
//! That is the obligation the shared vectors cannot express (CORELIB_PLAN §7):
//! reassembly is not wire-visible, so two implementations can disagree about a
//! split payload and still emit byte-identical output. The `invalid_utf8`
//! vectors additionally never reach a payload long enough to be split at all, so
//! the verdict on a field whose invalid sequence arrives in a later chunk — and
//! the `chunkOffset >= total` boundary behind it — is untested by them in
//! principle.

mod common;

use common::push_varint;
use sofab::{decode, Error, FixlenType, IStream, Id, OStream, PayloadAcc, Status, Visitor};

/// Wire type `FIXLEN`, and the `Str` subtype of its length word.
const T_FIXLEN: u64 = 0x2;
const FX_STR: u64 = 0x2;

/// A destination in the shape generated code has: the message fields, one
/// accumulator shared by every payload, and the sticky `inv` flag that carries
/// the `INVALID` verdict out of a callback that cannot return one.
#[derive(Default)]
struct Sink {
    acc: PayloadAcc,
    name: String,
    note: String,
    data: Vec<u8>,
    inv: bool,
}

impl Visitor for Sink {
    fn string(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {
        let Some(bytes) = self.acc.feed(total, offset, chunk) else {
            return;
        };
        // MESSAGE_SPEC §8 / CORELIB_PLAN §6.4: the verdict is passed here, on the
        // assembled field, and invalid bytes are rejected rather than replaced.
        let text = match core::str::from_utf8(bytes) {
            Ok(s) => s.to_owned(),
            Err(_) => {
                self.inv = true;
                return;
            }
        };
        match id {
            1 => self.name = text,
            2 => self.note = text,
            _ => {}
        }
    }

    fn blob(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {
        if let Some(bytes) = self.acc.feed(total, offset, chunk) {
            if id == 3 {
                self.data = bytes.to_vec();
            }
        }
    }
}

impl Sink {
    /// The three-valued outcome generated code reports: `INVALID` wins over the
    /// stream's own status (§5.2), and the stream's status is otherwise handed
    /// straight back — generated code "passes this status through verbatim"
    /// (MESSAGE_SPEC §7), it does not decide for the caller whether an
    /// unfinished message is acceptable (CORELIB_PLAN §5.2.4).
    #[allow(clippy::type_complexity)]
    fn finish(
        self,
        status: Result<Status, Error>,
    ) -> Result<(Status, String, String, Vec<u8>), Error> {
        if self.inv {
            return Err(Error::InvalidMsg);
        }
        Ok((status?, self.name, self.note, self.data))
    }
}

/// A message with an empty string, a long one and a blob — long enough that most
/// chunk sizes cut every payload somewhere, and with a multi-byte character on
/// the boundary of nearly every cut.
fn message() -> Vec<u8> {
    let mut buf = [0u8; 512];
    let used = {
        let mut os = OStream::new(&mut buf);
        os.write_str(1, "").unwrap();
        os.write_str(2, "sofä buffers — ünicode ✓ straddling every cut point")
            .unwrap();
        os.write_blob(3, &(0u8..=200).collect::<Vec<u8>>()).unwrap();
        os.bytes_used()
    };
    buf[..used].to_vec()
}

/// The values `message()` carries, as the destination must end up holding them.
fn expected() -> (String, String, Vec<u8>) {
    (
        String::new(),
        "sofä buffers — ünicode ✓ straddling every cut point".to_owned(),
        (0u8..=200).collect(),
    )
}

/// Decode `wire` by feeding it in fixed-size pieces.
#[allow(clippy::type_complexity)]
fn feed_in_chunks(wire: &[u8], step: usize) -> Result<(Status, String, String, Vec<u8>), Error> {
    let mut sink = Sink::default();
    let mut is = IStream::new();
    let mut status = Ok(Status::Complete);
    for chunk in wire.chunks(step) {
        status = is.feed(chunk, &mut sink);
        if let Err(Error::InvalidMsg) = status {
            break;
        }
    }
    sink.finish(status)
}

#[test]
fn every_chunk_size_assembles_the_same_message() {
    let wire = message();
    for step in 1..=wire.len() + 1 {
        let (status, name, note, data) = feed_in_chunks(&wire, step).unwrap();
        assert_eq!(status, Status::Complete, "fed {step} bytes at a time");
        assert_eq!((name, note, data), expected(), "fed {step} bytes at a time");
    }
}

#[test]
fn the_contiguous_path_assembles_the_same_message() {
    // The zero-copy `decode` path hands every payload over whole, so the
    // accumulator takes its pass-through branch throughout and must agree with
    // every split above.
    let wire = message();
    let mut sink = Sink::default();
    let status = decode(&wire, &mut sink);
    let (status, name, note, data) = sink.finish(status).unwrap();
    assert_eq!(status, Status::Complete);
    assert_eq!((name, note, data), expected());
}

#[test]
fn one_accumulator_serves_message_after_message() {
    // Generated decoders keep the accumulator for the life of the object, so a
    // message that follows another — including one whose payload was abandoned
    // mid-flight — must not inherit a byte of it.
    let wire = message();
    let truncated = &wire[..wire.len() - 30];

    let mut sink = Sink::default();
    let mut is = IStream::new();
    for chunk in truncated.chunks(3) {
        let _ = is.feed(chunk, &mut sink);
    }
    is.reset();
    sink.acc.reset();

    let mut status = Ok(Status::Complete);
    for chunk in wire.chunks(3) {
        status = is.feed(chunk, &mut sink);
    }
    let (status, name, note, data) = sink.finish(status).unwrap();
    assert_eq!(status, Status::Complete);
    assert_eq!((name, note, data), expected());
}

/// A `string` field written by hand: `OStream` refuses invalid UTF-8 under the
/// `Str` subtype (§6.4, encode side), so these bytes cannot come from it.
fn invalid_utf8_field() -> Vec<u8> {
    let mut payload = Vec::new();
    while payload.len() < 200 {
        payload.extend_from_slice("sofä".as_bytes());
    }
    payload.extend_from_slice(&[0xE2, 0x82]); // start of a 3-byte sequence …
    payload.extend_from_slice(b"A tail"); // … cut short by an ASCII byte
    assert!(core::str::from_utf8(&payload).is_err());

    let mut out = Vec::new();
    push_varint(&mut out, (2 << 3) | T_FIXLEN);
    push_varint(&mut out, ((payload.len() as u64) << 3) | FX_STR);
    out.extend_from_slice(&payload);
    out
}

#[test]
fn a_late_invalid_sequence_is_rejected_at_every_chunk_size() {
    // The gap the shared vectors leave: the offending sequence starts ~200 bytes
    // into the payload, so it arrives in a later chunk for every small step, and
    // for many of them it is itself split. Assembling first and judging once is
    // what makes the verdict independent of that (§7.2) — a consumer that
    // validated per chunk would accept the halves and let the field through.
    let wire = invalid_utf8_field();
    for step in 1..=wire.len() + 1 {
        assert_eq!(
            feed_in_chunks(&wire, step),
            Err(Error::InvalidMsg),
            "fed {step} bytes at a time"
        );
    }
    let mut sink = Sink::default();
    let status = decode(&wire, &mut sink);
    assert_eq!(sink.finish(status), Err(Error::InvalidMsg));
}

#[test]
fn a_truncated_payload_leaves_the_field_at_its_default() {
    // Nothing is handed back until the payload is complete, so a field whose
    // bytes stop half way stays at its declared default and the stream's own
    // `Incomplete` is the verdict — never a truncated value (§5.2).
    let wire = message();
    for cut in 1..wire.len() {
        let mut sink = Sink::default();
        let mut is = IStream::new();
        let status = is.feed(&wire[..cut], &mut sink);
        if status == Ok(Status::Complete) {
            continue; // the cut fell on a field boundary
        }
        assert_eq!(status, Ok(Status::Incomplete), "cut at {cut}");
        assert!(
            !sink.inv,
            "a truncated payload is unfinished, not malformed (cut at {cut})"
        );
        // Whatever is still outstanding was never placed: the long string is
        // either whole or absent, never a prefix.
        let (_, note, _) = expected();
        assert!(
            sink.note.is_empty() || sink.note == note,
            "partial field materialized at cut {cut}"
        );
    }
}

#[test]
fn the_accumulator_is_idle_between_payloads() {
    // The buffer holds bytes only while a payload is genuinely split: the
    // contiguous path never touches it, and it is empty again once the field has
    // been handed over.
    #[derive(Default)]
    struct Watch {
        acc: PayloadAcc,
        buffered_after_completion: Vec<usize>,
    }
    impl Visitor for Watch {
        fn string(&mut self, _id: Id, total: usize, offset: usize, chunk: &[u8]) {
            let done = self.acc.feed(total, offset, chunk).is_some();
            if done {
                self.buffered_after_completion.push(self.acc.buffered());
            }
        }
        fn fixlen_begin(&mut self, _id: Id, subtype: FixlenType, _total: usize) {
            assert_eq!(
                self.acc.buffered(),
                0,
                "bytes left over when the next {subtype:?} field starts"
            );
        }
    }

    let wire = message();
    let mut watch = Watch::default();
    assert_eq!(decode(&wire, &mut watch), Ok(Status::Complete));
    assert_eq!(
        watch.buffered_after_completion,
        vec![0, 0],
        "the contiguous path must not buffer a byte"
    );
}
