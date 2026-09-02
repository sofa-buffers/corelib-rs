//! The end-of-input probe: `feed(&[], visitor)` (CORELIB_PLAN §5.2, README).
//!
//! There is no `finish`/`finalize` in this API — the three-valued verdict comes
//! only from `feed`'s return value — so an **empty chunk** is how a caller asks
//! "did the stream end on a field boundary?". That makes the empty feed a real
//! entry point with obligations of its own:
//!
//! * it answers `Ok(Status::Complete)` **iff** the decoder sits at a boundary,
//!   and `Ok(Status::Incomplete)` from *every* suspended state — mid header varint, mid
//!   value varint, mid `fixlen_word`, mid string/blob payload, between and inside
//!   array elements, and inside an open sequence;
//! * it must be **inert**: no callback may fire for it (in particular no
//!   zero-length payload chunk, which a consumer reassembling a field would
//!   count), and repeating it must not consume, re-deliver or lose anything;
//! * the message must complete normally afterwards, with exactly the callbacks
//!   the unprobed stream produces.
//!
//! The suspended states are enumerated one field kind at a time and cut where the
//! decoder's own resume machinery differs — a carried partial varint, a
//! `Resume::Payload`, a `Resume::ArrayInt` at and inside an element, a
//! `Resume::ArrayFix` — because "inert" has to hold in each of them separately.

mod common;

use sofab::{
    decode, ArrayKind, Error, FixlenType, IStream, Id, OStream, Signed, Status, Unsigned, Visitor,
};

/// One visitor callback, recorded at **chunk** granularity: a payload chunk is
/// its own entry, so a spurious empty delivery is visible rather than absorbed
/// into a reassembly buffer.
#[derive(Debug, Clone, PartialEq)]
enum Call {
    Unsigned(Id, Unsigned),
    Signed(Id, Signed),
    Fp32(Id, u32),
    Fp64(Id, u64),
    FixlenBegin(Id, FixlenType, usize),
    Str(Id, usize, usize, Vec<u8>),
    Blob(Id, usize, usize, Vec<u8>),
    ArrayBegin(Id, ArrayKind, usize),
    SequenceBegin(Id),
    SequenceEnd,
}

#[derive(Default)]
struct Calls {
    log: Vec<Call>,
}

impl Visitor for Calls {
    fn unsigned(&mut self, id: Id, value: Unsigned) {
        self.log.push(Call::Unsigned(id, value));
    }
    fn signed(&mut self, id: Id, value: Signed) {
        self.log.push(Call::Signed(id, value));
    }
    fn fp32(&mut self, id: Id, value: f32) {
        self.log.push(Call::Fp32(id, value.to_bits()));
    }
    fn fp64(&mut self, id: Id, value: f64) {
        self.log.push(Call::Fp64(id, value.to_bits()));
    }
    fn fixlen_begin(&mut self, id: Id, subtype: FixlenType, total: usize) {
        self.log.push(Call::FixlenBegin(id, subtype, total));
    }
    fn string(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {
        self.log.push(Call::Str(id, total, offset, chunk.to_vec()));
    }
    fn blob(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {
        self.log.push(Call::Blob(id, total, offset, chunk.to_vec()));
    }
    fn array_begin(&mut self, id: Id, kind: ArrayKind, count: usize) {
        self.log.push(Call::ArrayBegin(id, kind, count));
    }
    fn sequence_begin(&mut self, id: Id) {
        self.log.push(Call::SequenceBegin(id));
    }
    fn sequence_end(&mut self) {
        self.log.push(Call::SequenceEnd);
    }
}

fn encode(f: impl Fn(&mut OStream)) -> Vec<u8> {
    let mut buf = vec![0u8; 1024];
    let used = {
        let mut os = OStream::new(&mut buf);
        f(&mut os);
        os.bytes_used()
    };
    buf.truncate(used);
    buf
}

/// A suspended-decoder case: a message and the byte count after which the
/// decoder is left inside the named construct.
struct Suspended {
    what: &'static str,
    msg: Vec<u8>,
    cut: usize,
}

fn suspended_cases() -> Vec<Suspended> {
    let mut cases = Vec::new();
    let mut add = |what, msg: Vec<u8>, cut: usize| {
        assert!(cut > 0 && cut < msg.len(), "{what}: cut must be interior");
        cases.push(Suspended { what, msg, cut });
    };

    // A two-byte field header (id 300), cut between its bytes.
    add(
        "inside a field header varint",
        encode(|os| os.write_unsigned(300, 1).unwrap()),
        1,
    );
    // A ten-byte value varint, cut in the middle of it.
    add(
        "inside a value varint",
        encode(|os| os.write_unsigned(1, u64::MAX).unwrap()),
        4,
    );
    // A string long enough that `(len << 3) | subtype` is a two-byte word.
    let long = "x".repeat(300);
    let string_msg = encode(|os| os.write_str(2, &long).unwrap());
    let payload_at = string_msg.len() - long.len();
    add("inside a fixlen word", string_msg.clone(), payload_at - 1);
    add("inside a string payload", string_msg, payload_at + 100);
    let blob_bytes: Vec<u8> = (0..200u32).map(|i| i as u8).collect();
    let blob_msg = encode(|os| os.write_blob(3, &blob_bytes).unwrap());
    let blob_at = blob_msg.len() - blob_bytes.len();
    add("inside a blob payload", blob_msg, blob_at + 7);
    // Integer array: cut on an element boundary (elements still owed) and inside
    // an element's varint — two different resume states.
    let ints = [u64::MAX, 1 << 40, 7, 0];
    let int_msg = encode(|os| os.write_array_unsigned(4, &ints).unwrap());
    add("between integer array elements", int_msg.clone(), 12);
    add("inside an integer array element", int_msg, 5);
    let signed = [i64::MIN, -1, 3];
    add(
        "inside a signed array element",
        encode(|os| os.write_array_signed(5, &signed).unwrap()),
        4,
    );
    // Float arrays: cut inside an element's fixed-width payload.
    let f32_msg = encode(|os| os.write_array_fp32(6, &[1.5f32, -2.5, 3.5]).unwrap());
    add("inside an fp32 array element", f32_msg, 6);
    let f64_msg = encode(|os| os.write_array_fp64(7, &[1.5f64, -2.5]).unwrap());
    add("inside an fp64 array element", f64_msg, 8);
    // A scalar float, which the decoder streams as a one-element run.
    add(
        "inside a scalar fp64",
        encode(|os| os.write_fp64(8, 1234.5).unwrap()),
        4,
    );
    // An open sequence: every byte is complete, but the frame is not closed.
    let seq = encode(|os| {
        os.write_sequence_begin_lazy(9).unwrap();
        os.write_unsigned(1, 42).unwrap();
        os.write_sequence_end().unwrap();
    });
    add("inside an open sequence", seq.clone(), seq.len() - 1);

    cases
}

#[test]
fn the_probe_reports_incomplete_from_every_suspended_state() {
    for Suspended { what, msg, cut } in suspended_cases() {
        // The reference is the *same two chunks* fed without probes: a payload is
        // delivered per chunk, so only an unprobed run of the identical chunking
        // isolates what the probe did.
        let mut reference = Calls::default();
        let mut ref_is = IStream::new();
        assert_eq!(
            ref_is.feed(&msg[..cut], &mut reference),
            Ok(Status::Incomplete),
            "{what}: the prefix itself must be INCOMPLETE"
        );
        assert_eq!(
            ref_is.feed(&msg[cut..], &mut reference),
            Ok(Status::Complete),
            "{what}"
        );

        let mut calls = Calls::default();
        let mut is = IStream::new();
        assert_eq!(
            is.feed(&msg[..cut], &mut calls),
            Ok(Status::Incomplete),
            "{what}: the prefix itself must be INCOMPLETE"
        );
        let after_prefix = calls.log.clone();

        // Inert: three probes in a row change nothing — no callback, no consumed
        // byte, no lost state.
        for round in 0..3 {
            assert_eq!(
                is.feed(&[], &mut calls),
                Ok(Status::Incomplete),
                "{what}: probe {round} must report INCOMPLETE"
            );
            assert_eq!(
                calls.log, after_prefix,
                "{what}: probe {round} fired a callback"
            );
        }

        // And the message still completes, byte-identically to the unprobed run.
        assert_eq!(
            is.feed(&msg[cut..], &mut calls),
            Ok(Status::Complete),
            "{what}: the remainder must complete the message"
        );
        assert_eq!(
            calls.log, reference.log,
            "{what}: probing changed what the visitor saw"
        );
        assert_eq!(
            is.feed(&[], &mut calls),
            Ok(Status::Complete),
            "{what}: now at a boundary"
        );
    }
}

#[test]
fn the_probe_reports_ok_at_every_boundary() {
    // A fresh decoder, a decoder that has just completed a message, and one that
    // completed a message field by field are all at a boundary — and the empty
    // message (every field at its default) is itself complete, with no callbacks.
    let msg = encode(|os| {
        os.write_unsigned(1, 42).unwrap();
        os.write_str(2, "hi").unwrap();
    });

    let mut calls = Calls::default();
    let mut is = IStream::new();
    assert_eq!(
        is.feed(&[], &mut calls),
        Ok(Status::Complete),
        "a fresh decoder"
    );
    assert!(calls.log.is_empty());

    assert_eq!(is.feed(&msg, &mut calls), Ok(Status::Complete));
    let after = calls.log.clone();
    for _ in 0..3 {
        assert_eq!(
            is.feed(&[], &mut calls),
            Ok(Status::Complete),
            "after a whole message"
        );
        assert_eq!(calls.log, after, "the probe fired a callback");
    }

    // One byte at a time, probing after every single one: the probes are inert,
    // so the log is the one the same chunking produces unprobed. (It is not the
    // one-shot log: a payload arrives per chunk, which is the documented shape.)
    let feed_one_byte_at_a_time = |probe: bool| {
        let mut calls = Calls::default();
        let mut is = IStream::new();
        for chunk in msg.chunks(1) {
            match is.feed(chunk, &mut calls) {
                Ok(Status::Complete) | Ok(Status::Incomplete) => {}
                Err(e) => panic!("one-byte feed reported {e}"),
            }
            if probe {
                match is.feed(&[], &mut calls) {
                    Ok(Status::Complete) | Ok(Status::Incomplete) => {}
                    Err(e) => panic!("probe reported {e}"),
                }
            }
        }
        assert_eq!(is.feed(&[], &mut calls), Ok(Status::Complete));
        calls.log
    };
    assert_eq!(
        feed_one_byte_at_a_time(true),
        feed_one_byte_at_a_time(false),
        "probing between bytes changed what the visitor saw"
    );

    let mut empty = Calls::default();
    assert_eq!(
        decode(&[], &mut empty),
        Ok(Status::Complete),
        "an all-default message"
    );
    assert!(empty.log.is_empty());
}

#[test]
fn the_probe_repeats_a_latched_rejection() {
    // §5.2: INVALID is terminal for the decoder, so the probe cannot be used to
    // walk out of it — it reports the rejection again, and `reset` is the only
    // way back to a usable decoder.
    let mut calls = Calls::default();
    let mut is = IStream::new();
    assert_eq!(is.feed(&[0x07], &mut calls), Err(Error::InvalidMsg));
    for _ in 0..3 {
        assert_eq!(is.feed(&[], &mut calls), Err(Error::InvalidMsg));
    }
    is.reset();
    assert_eq!(is.feed(&[], &mut calls), Ok(Status::Complete));
    assert!(calls.log.is_empty());
}

#[test]
fn a_probe_mid_payload_delivers_no_empty_chunk() {
    // The narrow case behind the "inert" rule, stated on its own because it is
    // the one a reassembling consumer would notice: mid-payload the decoder is
    // holding a `Resume::Payload`, and an empty chunk has nothing to add to it.
    // A zero-length `string` call here would look to generated code like the
    // whole-field-arrived event for a field that is still 190 bytes short.
    let text = "ä".repeat(100); // 200 bytes, multi-byte throughout
    let msg = encode(|os| os.write_str(1, &text).unwrap());
    let payload_at = msg.len() - text.len();

    let mut calls = Calls::default();
    let mut is = IStream::new();
    assert_eq!(
        is.feed(&msg[..payload_at + 10], &mut calls),
        Ok(Status::Incomplete)
    );
    assert_eq!(
        calls.log,
        vec![
            Call::FixlenBegin(1, FixlenType::Str, 200),
            Call::Str(1, 200, 0, text.as_bytes()[..10].to_vec()),
        ]
    );

    assert_eq!(is.feed(&[], &mut calls), Ok(Status::Incomplete));
    assert_eq!(calls.log.len(), 2, "the probe delivered an empty chunk");

    assert_eq!(
        is.feed(&msg[payload_at + 10..], &mut calls),
        Ok(Status::Complete)
    );
    assert_eq!(
        calls.log[2],
        Call::Str(1, 200, 10, text.as_bytes()[10..].to_vec()),
        "the payload resumes at the offset it suspended on"
    );
    assert_eq!(calls.log.len(), 3);
}

#[test]
fn an_empty_string_still_gets_its_one_chunk() {
    // The other side of the same rule: an *empty payload* is not an absent one.
    // `Visitor::string` documents exactly one call with `total == 0`, so the
    // corelib must deliver a zero-length chunk here — the case the probe must not
    // fabricate is a zero-length chunk for a payload that is merely unfinished.
    let msg = encode(|os| {
        os.write_str(1, "").unwrap();
        os.write_blob(2, &[]).unwrap();
    });
    let mut calls = Calls::default();
    assert_eq!(decode(&msg, &mut calls), Ok(Status::Complete));
    assert_eq!(
        calls.log,
        vec![
            Call::FixlenBegin(1, FixlenType::Str, 0),
            Call::Str(1, 0, 0, Vec::new()),
            Call::FixlenBegin(2, FixlenType::Blob, 0),
            Call::Blob(2, 0, 0, Vec::new()),
        ]
    );

    // Chunked, including a probe between the two fields.
    let mut chunked = Calls::default();
    let mut is = IStream::new();
    for chunk in msg.chunks(1) {
        match is.feed(chunk, &mut chunked) {
            Ok(Status::Complete) | Ok(Status::Incomplete) => {}
            Err(e) => panic!("one-byte feed reported {e}"),
        }
        // A probe after every single byte: inert, so it neither duplicates the
        // zero-length chunk of the empty payloads nor adds one of its own.
        let before = chunked.log.clone();
        match is.feed(&[], &mut chunked) {
            Ok(Status::Complete) | Ok(Status::Incomplete) => {}
            Err(e) => panic!("probe reported {e}"),
        }
        assert_eq!(chunked.log, before, "a probe fired a callback");
    }
    assert_eq!(is.feed(&[], &mut chunked), Ok(Status::Complete));
    assert_eq!(chunked.log, calls.log);
}
