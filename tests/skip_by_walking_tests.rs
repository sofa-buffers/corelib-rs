//! Skipping a field means **walking** it (MESSAGE_SPEC §7.3, CORELIB_PLAN §5.2).
//!
//! [`Visitor`] gives every callback a default no-op body, so a consumer that
//! overrides nothing is the corelib's skip path: the decoder still has to read
//! each field's structure — a varint's continuation bytes, a `fixlen_word` and
//! its payload length, an array's count, `fixlen_word` and every element, a
//! sequence's frame — and land on the next field boundary. Nothing about the
//! verdict may depend on whether the consumer was interested.
//!
//! That is not observable from the skipping visitor itself (it records nothing),
//! so it is pinned two ways: the verdict of a blind visitor is compared against a
//! recording one at **every prefix** of the message — a walk that consumed one
//! byte too few or too many would land somewhere else and disagree — and a
//! visitor that handles only *some* field kinds must still see the fields after
//! the ones it skipped.
//!
//! The last test is §6.4's other half: a skipped `string` is a length jump over
//! bytes the visitor never sees, so no UTF-8 check runs on it and its content
//! cannot change the outcome.

mod common;

use common::{push_varint, Event, Recorder};
use sofab::{decode, ArrayKind, Error, IStream, Id, OStream, Signed, Status, Unsigned, Visitor};

/// The skip path: every callback left at its default no-op.
#[derive(Default)]
struct Blind;
impl Visitor for Blind {}

/// A visitor that handles integers only — everything else is walked past.
#[derive(Default)]
struct IntegersOnly {
    seen: Vec<(Id, Unsigned)>,
}
impl Visitor for IntegersOnly {
    fn unsigned(&mut self, id: Id, value: Unsigned) {
        self.seen.push((id, value));
    }
}

/// One field of every wire type, with the skipped constructs sandwiched between
/// two integer fields so a mis-walk shows up as a lost or shifted sentinel.
fn write_everything(os: &mut OStream) {
    os.write_unsigned(1, 11).unwrap(); // leading sentinel
    os.write_signed(2, -12345).unwrap();
    os.write_fp32(3, 1.5).unwrap();
    os.write_fp64(4, -2.5).unwrap();
    os.write_str(5, "sofa-buffers").unwrap();
    os.write_blob(6, &[0u8, 1, 253, 255]).unwrap();
    os.write_array_unsigned(7, &[1u64, 1 << 20, u64::MAX, 0])
        .unwrap();
    os.write_array_signed(8, &[-1i64, i64::MIN, 7]).unwrap();
    os.write_array_fp32(9, &[1.5f32, -0.0]).unwrap();
    os.write_array_fp64(10, &[3.5f64, -0.5]).unwrap();
    os.write_sequence_begin_lazy(11).unwrap();
    os.write_str(1, "nested").unwrap();
    os.write_array_unsigned(2, &[9u64; 3]).unwrap();
    os.write_sequence_end().unwrap();
    os.write_unsigned(12, 12).unwrap(); // trailing sentinel
}

fn one_shot() -> Vec<u8> {
    let mut buf = vec![0u8; 512];
    let used = {
        let mut os = OStream::new(&mut buf);
        write_everything(&mut os);
        os.bytes_used()
    };
    buf.truncate(used);
    buf
}

/// Feed `msg` in `size`-byte chunks with a fresh visitor, returning the verdict
/// of the end-of-input probe.
fn chunked_verdict<V: Visitor>(msg: &[u8], size: usize, v: &mut V) -> Result<Status, Error> {
    let mut is = IStream::new();
    for chunk in msg.chunks(size) {
        match is.feed(chunk, v) {
            Ok(Status::Complete) | Ok(Status::Incomplete) => {}
            e => return e,
        }
    }
    is.feed(&[], v)
}

#[test]
fn a_visitor_that_overrides_nothing_walks_every_wire_type() {
    let msg = one_shot();
    assert_eq!(decode(&msg, &mut Blind), Ok(Status::Complete));

    // Every chunk size, so each construct is skipped from every resume state as
    // well as contiguously.
    for size in 1..=msg.len() {
        assert_eq!(
            chunked_verdict(&msg, size, &mut Blind),
            Ok(Status::Complete),
            "the skip path lost the boundary at chunk size {size}"
        );
    }
}

#[test]
fn the_skip_path_lands_where_the_recording_path_lands_at_every_prefix() {
    // The walk is only correct if it consumes exactly the bytes the field
    // occupies. A prefix cut inside a field is INCOMPLETE and one cut between
    // fields is COMPLETE — so comparing the two visitors' verdicts at every cut
    // compares their cursors at every field boundary of the message.
    let msg = one_shot();
    for cut in 0..=msg.len() {
        let blind = decode(&msg[..cut], &mut Blind);
        let mut rec = Recorder::new();
        let recording = decode(&msg[..cut], &mut rec);
        assert_eq!(
            blind, recording,
            "prefix of length {cut}: skipping and recording disagree"
        );
        assert!(
            matches!(blind, Ok(Status::Complete) | Ok(Status::Incomplete)),
            "prefix of length {cut}: a prefix of a valid message is never invalid"
        );
    }
}

#[test]
fn a_partially_interested_visitor_still_sees_the_fields_after_the_skipped_ones() {
    // The observable half: the fields a visitor does handle must arrive with the
    // right ids and values even though everything between them was walked past.
    let msg = one_shot();
    let want = vec![
        (1, 11),
        (7, 1),
        (7, 1 << 20),
        (7, u64::MAX),
        (7, 0),
        (2, 9),
        (2, 9),
        (2, 9),
        (12, 12),
    ];

    let mut ints = IntegersOnly::default();
    assert_eq!(decode(&msg, &mut ints), Ok(Status::Complete));
    assert_eq!(ints.seen, want, "one-shot");

    for size in 1..=msg.len() {
        let mut ints = IntegersOnly::default();
        assert_eq!(chunked_verdict(&msg, size, &mut ints), Ok(Status::Complete));
        assert_eq!(ints.seen, want, "chunk size {size}");
    }
}

#[test]
fn a_skipped_string_is_never_utf8_checked() {
    // §6.4: strictness lives in the consumer's materialization, so a `string`
    // nobody materializes cannot make a message INVALID — it is a length jump
    // over bytes the visitor never sees. The field after it still arrives, which
    // is what proves the jump was by length and not by scanning for something.
    let mut msg = Vec::new();
    // Overlong F0 80 80, a lone 0xFF, and the "Modified UTF-8" NUL C0 80. Built
    // at runtime so `from_utf8` sees a value rather than a literal the compiler
    // would rather lint about than let the test state.
    let mut bad: Vec<u8> = vec![0xF0, 0x80, 0x80];
    bad.extend_from_slice(&[0x41, 0xFF, 0xC0, 0x80]);
    assert!(core::str::from_utf8(&bad).is_err());
    push_varint(&mut msg, (4 << 3) | 0x2); // id 4, FIXLEN
    push_varint(&mut msg, ((bad.len() as u64) << 3) | 0x2); // Str subtype
    msg.extend_from_slice(&bad);
    push_varint(&mut msg, 5 << 3); // id 5, unsigned (wire type 0)
    push_varint(&mut msg, 42);

    assert_eq!(decode(&msg, &mut Blind), Ok(Status::Complete));

    let mut ints = IntegersOnly::default();
    assert_eq!(decode(&msg, &mut ints), Ok(Status::Complete));
    assert_eq!(ints.seen, vec![(5, 42)]);

    for size in 1..=msg.len() {
        let mut ints = IntegersOnly::default();
        assert_eq!(
            chunked_verdict(&msg, size, &mut ints),
            Ok(Status::Complete),
            "size {size}"
        );
        assert_eq!(ints.seen, vec![(5, 42)], "chunk size {size}");
    }

    // A visitor that *does* take the bytes gets them verbatim — the corelib
    // neither replaced nor truncated them, it simply never looked.
    let mut rec = Recorder::new();
    assert_eq!(decode(&msg, &mut rec), Ok(Status::Complete));
    assert_eq!(
        rec.events,
        vec![Event::Str(4, bad.clone()), Event::Unsigned(5, 42)]
    );
}

#[test]
fn skipping_does_not_soften_a_malformed_field() {
    // A skip is a walk, not a tolerance: the constructs §5.2 calls malformed stay
    // INVALID for a visitor that would have ignored the field anyway.
    let cases: &[(&str, &[u8])] = &[
        ("reserved fixlen subtype", &[0x02, 0x0C]), // (1 << 3) | 4 — no such subtype
        ("dangling sequence end", &[0x07]),
        (
            "unterminated 11-byte varint",
            &[
                0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x02,
            ],
        ),
        (
            "fp32 field with a 5-byte payload",
            &[0x02, 0x28, 0, 0, 0, 0, 0],
        ),
    ];
    for (what, bytes) in cases {
        assert_eq!(
            decode(bytes, &mut Blind),
            Err(Error::InvalidMsg),
            "{what}: a skipping visitor must reach the same INVALID verdict"
        );
        let mut rec = Recorder::new();
        assert_eq!(decode(bytes, &mut rec), Err(Error::InvalidMsg), "{what}");
    }
}

/// A visitor whose only override is `array_begin`, to pin the one callback the
/// skip path fires *before* it knows whether anyone is interested: kind and count
/// are announced for every array, including the ones nothing consumes.
#[derive(Default)]
struct ArrayHeaders {
    seen: Vec<(Id, ArrayKind, usize)>,
}
impl Visitor for ArrayHeaders {
    fn array_begin(&mut self, id: Id, kind: ArrayKind, count: usize) {
        self.seen.push((id, kind, count));
    }
}

#[test]
fn array_headers_are_announced_even_when_the_elements_are_skipped() {
    let msg = one_shot();
    let want = vec![
        (7, ArrayKind::Unsigned, 4),
        (8, ArrayKind::Signed, 3),
        (9, ArrayKind::Fp32, 2),
        (10, ArrayKind::Fp64, 2),
        (2, ArrayKind::Unsigned, 3), // nested, inside the sequence
    ];

    let mut headers = ArrayHeaders::default();
    assert_eq!(decode(&msg, &mut headers), Ok(Status::Complete));
    assert_eq!(headers.seen, want, "one-shot");

    for size in 1..=msg.len() {
        let mut headers = ArrayHeaders::default();
        assert_eq!(
            chunked_verdict(&msg, size, &mut headers),
            Ok(Status::Complete)
        );
        assert_eq!(headers.seen, want, "chunk size {size}");
    }
}

/// Signed elements are the one kind `IntegersOnly` above cannot see; this pins
/// that a visitor overriding only `signed` skips the unsigned ones symmetrically.
#[derive(Default)]
struct SignedOnly {
    seen: Vec<(Id, Signed)>,
}
impl Visitor for SignedOnly {
    fn signed(&mut self, id: Id, value: Signed) {
        self.seen.push((id, value));
    }
}

#[test]
fn a_signed_only_visitor_skips_the_unsigned_side() {
    let msg = one_shot();
    let want = vec![(2, -12345), (8, -1), (8, i64::MIN), (8, 7)];
    let mut only = SignedOnly::default();
    assert_eq!(decode(&msg, &mut only), Ok(Status::Complete));
    assert_eq!(only.seen, want);
}
