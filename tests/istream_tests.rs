//! Decoder tests. Inputs are the exact encoded byte vectors from the C
//! reference suite; we assert the decoded events.

// Float test vectors are deliberately the literals used by the C suite.
#![allow(clippy::approx_constant, clippy::excessive_precision)]

mod common;

use common::{push_varint, Event, Recorder};
use sofab::{ArrayKind, Error, IStream};

/// Decode `bytes` in one shot and return the recorded events.
fn decode(bytes: &[u8]) -> Vec<Event> {
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    is.feed(bytes, &mut rec).expect("decode failed");
    rec.events
}

#[test]
fn decode_unsigned() {
    assert_eq!(decode(&[0x00, 0x80, 0x01]), [Event::Unsigned(0, 128)]);
    assert_eq!(
        decode(&[0xF8, 0xFF, 0xFF, 0xFF, 0x3F, 0x00]),
        [Event::Unsigned(sofab::ID_MAX, 0)]
    );
    assert_eq!(
        decode(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01]),
        [Event::Unsigned(0, u64::MAX)]
    );
}

#[test]
fn decode_signed() {
    assert_eq!(
        decode(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01]),
        [Event::Signed(0, i64::MIN)]
    );
    assert_eq!(
        decode(&[0x01, 0xFE, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01]),
        [Event::Signed(0, i64::MAX)]
    );
}

#[test]
fn decode_fp32() {
    assert_eq!(
        decode(&[0x02, 0x20, 0x56, 0x0E, 0x49, 0x40]),
        [Event::Fp32(0, 3.1415_f32.to_bits())]
    );
}

#[test]
fn decode_fp64() {
    assert_eq!(
        decode(&[0x02, 0x41, 0x00, 0x00, 0x00, 0x60, 0xFB, 0x21, 0x09, 0x40]),
        [Event::Fp64(0, (3.14159265_f32 as f64).to_bits())]
    );
}

#[test]
fn decode_string() {
    assert_eq!(
        decode(&[
            0x02, 0x62, 0x48, 0x65, 0x6C, 0x6C, 0x6F, 0x20, 0x43, 0x6F, 0x75, 0x63, 0x68, 0x21
        ]),
        [Event::Str(0, b"Hello Couch!".to_vec())]
    );
}

#[test]
fn decode_string_empty() {
    assert_eq!(decode(&[0x02, 0x02]), [Event::Str(0, vec![])]);
}

#[test]
fn decode_blob() {
    assert_eq!(
        decode(&[0x02, 0x2B, 0x01, 0x02, 0x03, 0x04, 0x05]),
        [Event::Blob(0, vec![1, 2, 3, 4, 5])]
    );
}

#[test]
fn decode_blob_empty() {
    assert_eq!(decode(&[0x02, 0x03]), [Event::Blob(0, vec![])]);
}

#[test]
fn decode_array_of_u32() {
    let bytes = [
        0x03, 0x05, 0x01, 0x02, 0x03, 0x80, 0x80, 0x80, 0x80, 0x08, 0xFF, 0xFF, 0xFF, 0xFF, 0x0F,
    ];
    assert_eq!(
        decode(&bytes),
        [
            Event::ArrayBegin(0, ArrayKind::Unsigned, 5),
            Event::Unsigned(0, 1),
            Event::Unsigned(0, 2),
            Event::Unsigned(0, 3),
            Event::Unsigned(0, 0x8000_0000),
            Event::Unsigned(0, u32::MAX as u64),
        ]
    );
}

#[test]
fn decode_array_of_i32() {
    let bytes = [
        0x04, 0x05, 0x01, 0x03, 0x05, 0xFF, 0xFF, 0xFF, 0xFF, 0x0F, 0xFE, 0xFF, 0xFF, 0xFF, 0x0F,
    ];
    assert_eq!(
        decode(&bytes),
        [
            Event::ArrayBegin(0, ArrayKind::Signed, 5),
            Event::Signed(0, -1),
            Event::Signed(0, -2),
            Event::Signed(0, -3),
            Event::Signed(0, i32::MIN as i64),
            Event::Signed(0, i32::MAX as i64),
        ]
    );
}

#[test]
fn decode_array_of_fp32() {
    let bytes = [
        0x05, 0x05, 0x20, 0x00, 0x00, 0x80, 0x3F, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x40, 0x40,
        0xFF, 0xFF, 0x7F, 0xFF, 0xFF, 0xFF, 0x7F, 0x7F,
    ];
    let want = [1.0_f32, 2.0, 3.0, -f32::MAX, f32::MAX];
    let mut expected = vec![Event::ArrayBegin(0, ArrayKind::Fp32, 5)];
    expected.extend(want.iter().map(|f| Event::Fp32(0, f.to_bits())));
    assert_eq!(decode(&bytes), expected);
}

#[test]
fn decode_nested_sequence() {
    let bytes = [0x00, 0x2A, 0x0E, 0x00, 0x2A, 0x11, 0x53, 0x07, 0x11, 0x53];
    assert_eq!(
        decode(&bytes),
        [
            Event::Unsigned(0, 42),
            Event::SequenceBegin(1),
            Event::Unsigned(0, 42),
            Event::Signed(2, -42),
            Event::SequenceEnd,
            Event::Signed(2, -42),
        ]
    );
}

// --- streaming: identical result regardless of how bytes are chunked --------

#[test]
fn streaming_chunked_feed_matches_oneshot() {
    // A message with a varint that spans a chunk boundary and a string that
    // spans several boundaries.
    let msg = [
        0x00, 0x80, 0x01, // unsigned id0 = 128 (varint split below)
        0x02, 0x62, // string id0, len 12
        0x48, 0x65, 0x6C, 0x6C, 0x6F, 0x20, 0x43, 0x6F, 0x75, 0x63, 0x68,
        0x21, // "Hello Couch!"
    ];
    let oneshot = decode(&msg);

    // Feed one byte at a time. Intermediate chunks end mid-field and so return
    // Incomplete; only a malformed byte would be an error.
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    for b in msg {
        match is.feed(&[b], &mut rec) {
            Ok(()) | Err(Error::Incomplete) => {}
            Err(e) => panic!("feed failed: {e}"),
        }
    }
    is.feed(&[], &mut rec).unwrap(); // clean boundary => Ok
    assert_eq!(rec.events, oneshot);

    // Feed in awkward 3-byte chunks.
    let mut rec2 = Recorder::new();
    let mut is2 = IStream::new();
    for chunk in msg.chunks(3) {
        match is2.feed(chunk, &mut rec2) {
            Ok(()) | Err(Error::Incomplete) => {}
            Err(e) => panic!("feed failed: {e}"),
        }
    }
    is2.feed(&[], &mut rec2).unwrap(); // clean boundary => Ok
    assert_eq!(rec2.events, oneshot);
}

// --- error cases ------------------------------------------------------------

#[test]
fn decode_zero_count_arrays() {
    // A zero-count integer array is exactly [ header ][ count = 0 ] (§4.7).
    assert_eq!(
        decode(&[0x03, 0x00]),
        [Event::ArrayBegin(0, ArrayKind::Unsigned, 0)]
    );
    assert_eq!(
        decode(&[0x04, 0x00]),
        [Event::ArrayBegin(0, ArrayKind::Signed, 0)]
    );
    // A zero-count fixlen array still carries its fixlen_word (0x20 = fp32,
    // 0x41 = fp64), but no payload (§4.8).
    assert_eq!(
        decode(&[0x05, 0x00, 0x20]),
        [Event::ArrayBegin(0, ArrayKind::Fp32, 0)]
    );
    assert_eq!(
        decode(&[0x05, 0x00, 0x41]),
        [Event::ArrayBegin(0, ArrayKind::Fp64, 0)]
    );
    // A zero-count fixlen array is followed directly by the next field once its
    // fixlen_word is consumed.
    assert_eq!(
        decode(&[0x05, 0x00, 0x20, 0x00, 0x2A]),
        [
            Event::ArrayBegin(0, ArrayKind::Fp32, 0),
            Event::Unsigned(0, 42),
        ]
    );
}

#[test]
fn nesting_beyond_max_depth_is_invalid() {
    // 255 nested sequence-start bytes (id 0 -> 0x06) are accepted; the 256th
    // exceeds MAX_DEPTH and must be rejected (§4.9, §6.2). 255 *open* sequences
    // is a valid-but-unfinished message: Incomplete, not an error.
    let ok: Vec<u8> = vec![0x06; sofab::MAX_DEPTH as usize];
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    assert_eq!(is.feed(&ok, &mut rec), Err(Error::Incomplete));

    let too_deep: Vec<u8> = vec![0x06; sofab::MAX_DEPTH as usize + 1];
    let mut rec2 = Recorder::new();
    let mut is2 = IStream::new();
    assert_eq!(is2.feed(&too_deep, &mut rec2), Err(Error::InvalidMsg));
}

#[test]
fn varint_overflow_is_invalid() {
    // 11 continuation bytes overflow the 64-bit value type.
    let bytes = [
        0x00, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80,
    ];
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    assert_eq!(is.feed(&bytes, &mut rec), Err(Error::InvalidMsg));
}

/// A varint whose payload does not fit the 64-bit value type: eleven
/// continuation bytes. Wherever one of these appears the message is INVALID
/// regardless of what follows (MESSAGE_SPEC §7) — never Incomplete.
const OVERLONG: [u8; 12] = [0x80; 12];

/// `prefix` followed by an over-wide varint must be rejected as `InvalidMsg`.
fn assert_overlong_rejected(prefix: &[u8], what: &str) {
    let mut bytes = prefix.to_vec();
    bytes.extend_from_slice(&OVERLONG);
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    assert_eq!(
        is.feed(&bytes, &mut rec),
        Err(Error::InvalidMsg),
        "an over-wide varint as {what} was not rejected"
    );
}

#[test]
fn varint_overflow_is_invalid_at_every_position() {
    // Every place the wire format reads a varint, in field-header order.
    assert_overlong_rejected(&[], "a field header");
    assert_overlong_rejected(&[0x00], "an unsigned value");
    assert_overlong_rejected(&[0x01], "a signed value");
    assert_overlong_rejected(&[0x02], "a fixlen word");
    assert_overlong_rejected(&[0x03], "an unsigned array count");
    assert_overlong_rejected(&[0x04], "a signed array count");
    assert_overlong_rejected(&[0x05], "a fixlen array count");
    assert_overlong_rejected(&[0x05, 0x01], "a fixlen array word");
    assert_overlong_rejected(&[0x03, 0x01], "an unsigned array element");
    assert_overlong_rejected(&[0x04, 0x01], "a signed array element");
}

#[test]
fn fp64_with_wrong_length_is_invalid() {
    // FIXLEN, subtype FP64 (1), but length 2 instead of 8.
    let bytes = [0x02, (2 << 3) | 0x01, 0xAA, 0xBB];
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    assert_eq!(is.feed(&bytes, &mut rec), Err(Error::InvalidMsg));
}

#[test]
fn oversized_count_is_invalid_for_every_array_kind() {
    // The `count > ARRAY_MAX` guard applies to all three array wire types, not
    // just the unsigned one (§4.7, §4.8).
    for tag in [0x03u8, 0x04, 0x05] {
        let mut bytes = vec![tag];
        push_varint(&mut bytes, i32::MAX as u64 + 1);
        let mut rec = Recorder::new();
        let mut is = IStream::new();
        assert_eq!(
            is.feed(&bytes, &mut rec),
            Err(Error::InvalidMsg),
            "oversized count accepted for wire tag {tag:#x}"
        );
    }
}

#[test]
fn dangling_sequence_end_is_invalid() {
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    assert_eq!(is.feed(&[0x07], &mut rec), Err(Error::InvalidMsg));
}

#[test]
fn id_above_max_is_invalid() {
    // Craft a header whose id field is ID_MAX + 1, type unsigned.
    let header = (sofab::ID_MAX as u64 + 1) << 3; // type tag 0 = unsigned
    let mut bytes = Vec::new();
    push_varint(&mut bytes, header);
    bytes.push(0x00); // value
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    assert_eq!(is.feed(&bytes, &mut rec), Err(Error::InvalidMsg));
}

#[test]
fn fp32_with_wrong_length_is_invalid() {
    // FIXLEN, subtype FP32 (0), but length 2 instead of 4.
    let bytes = [0x02, 2 << 3, 0xAA, 0xBB]; // len 2, subtype FP32 (tag 0)
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    assert_eq!(is.feed(&bytes, &mut rec), Err(Error::InvalidMsg));
}

#[test]
fn reserved_fixlen_subtype_is_invalid() {
    // A FIXLEN field (wire tag 2) whose fixlen word carries a reserved subtype
    // tag (0x4). Only 0x0..=0x3 are defined; the decoder rejects
    // 0x4..=0x7 with InvalidMsg (§7). Word = (len 4 << 3) | subtype 0x4 = 0x24.
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    assert_eq!(
        is.feed(&[0x02, (4 << 3) | 0x04], &mut rec),
        Err(Error::InvalidMsg)
    );

    // Mirror inside a FIXLENARRAY (wire tag 5): header, count = 1, then the
    // reserved fixlen word. The array's fixlen_word must decode to a valid
    // float subtype; a reserved tag is rejected the same way.
    let mut rec2 = Recorder::new();
    let mut is2 = IStream::new();
    assert_eq!(
        is2.feed(&[0x05, 0x01, (4 << 3) | 0x04], &mut rec2),
        Err(Error::InvalidMsg)
    );
}

#[test]
fn oversized_count_or_length_is_invalid() {
    // An unsigned varint array (wire tag 3) whose count exceeds ARRAY_MAX
    // (i32::MAX): count = i32::MAX + 1 must be rejected before any element is
    // read (the `count > ARRAY_MAX` guard), not treated as Incomplete.
    let mut bytes = vec![0x03];
    push_varint(&mut bytes, i32::MAX as u64 + 1); // one past ARRAY_MAX
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    assert_eq!(is.feed(&bytes, &mut rec), Err(Error::InvalidMsg));

    // A FIXLEN string (wire tag 2) whose declared length exceeds ARRAY_MAX:
    // fixlen word = (len << 3) | Str(0x2), len = i32::MAX + 1. Rejected by the
    // `(word >> 3) > ARRAY_MAX` guard, again distinct from Incomplete.
    let mut bytes2 = vec![0x02];
    let word = ((i32::MAX as u64 + 1) << 3) | 0x02; // Str subtype, oversized len
    push_varint(&mut bytes2, word);
    let mut rec2 = Recorder::new();
    let mut is2 = IStream::new();
    assert_eq!(is2.feed(&bytes2, &mut rec2), Err(Error::InvalidMsg));
}

// --- F-0042: the fixlen array's element subtype must reach the visitor -------
//
// CORELIB_PLAN §4.8 fixes the decode order for a fixlen array (wire type
// 0b101): read `element_count` (format ceiling `ARRAY_MAX` only, allocate
// nothing), read the `fixlen_word`, validate it as a *format* matter, and only
// then hand the header to the receiver. A receiver that bounds the array
// against a schema-declared element count may apply that bound only to a field
// that survives step 3 — i.e. one whose element subtype matches the declared
// element type; a contradicting subtype means the field is skipped under
// MESSAGE_SPEC §7.3 and was never this array's value.
//
// The corelib is schema-agnostic, so what it owes the receiver is exactly two
// things, both asserted below: `array_begin` must name the element **subtype**
// (`Fp32` / `Fp64`, never a collapsed "fixlen"), and it must not fire until the
// `fixlen_word` has been read and validated. Everything the receiver does with
// that — bound, skip, accept — is its own.
//
// The byte vectors are the Crucible F-0042 isolates verbatim. Their frame is
// `a6 06` = sequence start id 100 (`arrays`), `56` = sequence start id 10
// (`nested`), `05` = ARRAY_FIXLEN at id 0 (declared `array<fp32, count 5>` in
// the fuzzed schema), and `07 07` closes both sequences. `20` is the
// `fixlen_word` for fp32 (subtype 0, elem_len 4), `41` for fp64 (subtype 1,
// elem_len 8) — the contradicting subtype.

/// Decode `bytes` in one shot, returning both the verdict and the events
/// recorded before it. A truncated message returns `Err(Incomplete)` *and* the
/// events already delivered, which is the whole point for row 5.
fn decode_partial(bytes: &[u8]) -> (Result<(), Error>, Vec<Event>) {
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    let r = is.feed(bytes, &mut rec);
    (r, rec.events)
}

/// `a6 06 56 05` + `count` + `fixlen_word` + `payload` + `07 07`.
fn nested_fixlen_array(count: u64, word: u64, payload_len: usize) -> Vec<u8> {
    let mut b = vec![0xA6, 0x06, 0x56, 0x05];
    push_varint(&mut b, count);
    push_varint(&mut b, word);
    b.extend(core::iter::repeat(0x00).take(payload_len));
    b.extend_from_slice(&[0x07, 0x07]);
    b
}

fn frame(events: &[Event]) -> Vec<Event> {
    let mut v = vec![Event::SequenceBegin(100), Event::SequenceBegin(10)];
    v.extend_from_slice(events);
    v.push(Event::SequenceEnd);
    v.push(Event::SequenceEnd);
    v
}

/// Row 1 — in-count, contradicting subtype: `a6 06 56 05 03 41 00*24 07 07`.
/// The header is well-formed, so the corelib accepts it; it reports kind
/// `Fp64`, which is what lets a receiver whose slot is `array<fp32, …>` skip
/// the field instead of materializing 3 elements into it.
#[test]
fn fixlen_array_header_reports_fp64_subtype() {
    let bytes = nested_fixlen_array(3, 0x41, 3 * 8);
    let (r, ev) = decode_partial(&bytes);
    assert_eq!(r, Ok(()));
    let mut want = vec![Event::ArrayBegin(0, ArrayKind::Fp64, 3)];
    want.extend((0..3).map(|_| Event::Fp64(0, 0.0f64.to_bits())));
    assert_eq!(ev, frame(&want));
}

/// Row 2 — THE PRIMARY ROW: over-count *and* a contradicting subtype:
/// `a6 06 56 05 08 41 00*64 07 07`. count 8 exceeds the schema's declared
/// count 5, but the `fixlen_word` says fp64, so the field is skipped under
/// §7.3 and the schema bound must never be applied. The corelib therefore
/// accepts these bytes and reports `(Fp64, 8)`: the count is delivered *with*
/// the subtype that disarms it, in one call, so the receiver can never be
/// forced to judge the count before it knows the type.
#[test]
fn overcount_with_contradicting_subtype_is_accepted_and_typed() {
    let bytes = nested_fixlen_array(8, 0x41, 8 * 8);
    let (r, ev) = decode_partial(&bytes);
    assert_eq!(r, Ok(()));
    assert_eq!(ev[2], Event::ArrayBegin(0, ArrayKind::Fp64, 8));
    // Exactly one header event, and 8 element events after it.
    assert_eq!(
        ev.iter()
            .filter(|e| matches!(e, Event::ArrayBegin(..)))
            .count(),
        1
    );
    assert_eq!(
        ev.iter().filter(|e| matches!(e, Event::Fp64(..))).count(),
        8
    );
}

/// Row 3 — CONTROL, the single most important one: over-count with a
/// *matching* subtype, `a6 06 56 05 08 20 00*32 07 07`. The receiver must
/// still reject this as INVALID (count 8 > declared 5), and it can only do so
/// because the header it is handed says `Fp32` — the bound is reordered here,
/// never weakened. The corelib itself has no schema and accepts the bytes.
#[test]
fn overcount_with_matching_subtype_reports_fp32_so_the_bound_still_applies() {
    let bytes = nested_fixlen_array(8, 0x20, 8 * 4);
    let (r, ev) = decode_partial(&bytes);
    assert_eq!(r, Ok(()));
    assert_eq!(ev[2], Event::ArrayBegin(0, ArrayKind::Fp32, 8));
    assert_eq!(
        ev.iter().filter(|e| matches!(e, Event::Fp32(..))).count(),
        8
    );
}

/// Row 4 — SECOND PRIMARY ROW: EOF between the count word and the
/// `fixlen_word`, `a6 06 56 05 08`. INCOMPLETE, not INVALID: the decoder
/// cannot yet know whether this is a field it must bound. The header hook must
/// therefore NOT have fired — firing it here is exactly the bug in the five
/// impls that call `array_begin` off the count word.
#[test]
fn truncation_between_count_and_fixlen_word_fires_no_header() {
    let (r, ev) = decode_partial(&[0xA6, 0x06, 0x56, 0x05, 0x08]);
    assert_eq!(r, Err(Error::Incomplete));
    assert_eq!(ev, [Event::SequenceBegin(100), Event::SequenceBegin(10)]);
    assert!(!ev.iter().any(|e| matches!(e, Event::ArrayBegin(..))));
}

/// Row 5 — CONTROL: `a6 06 56 05 08 20`, EOF *after* the `fixlen_word`. The
/// subtype is known and matches, so an over-count is malformed regardless of
/// what follows and INVALID dominates INCOMPLETE (§5.2). The corelib reports
/// the truncation, but it must already have delivered `(Fp32, 8)` — without
/// that call the receiver has nothing to reject on, which is precisely why the
/// two generator-only workarounds in generator#232 regressed this row.
#[test]
fn overcount_matching_subtype_delivers_header_before_the_truncation() {
    let (r, ev) = decode_partial(&[0xA6, 0x06, 0x56, 0x05, 0x08, 0x20]);
    assert_eq!(r, Err(Error::Incomplete));
    assert_eq!(
        ev,
        [
            Event::SequenceBegin(100),
            Event::SequenceBegin(10),
            Event::ArrayBegin(0, ArrayKind::Fp32, 8),
        ]
    );
}

/// Row 6 — HAPPY-PATH CONTROL: `a6 06 56 05 03 20 00*12 07 07`, the only
/// vector whose re-encode equals its input. Accepted, `Fp32`, three elements.
#[test]
fn valid_in_count_fp32_array_roundtrips() {
    let bytes = nested_fixlen_array(3, 0x20, 3 * 4);
    let (r, ev) = decode_partial(&bytes);
    assert_eq!(r, Ok(()));
    let mut want = vec![Event::ArrayBegin(0, ArrayKind::Fp32, 3)];
    want.extend((0..3).map(|_| Event::Fp32(0, 0.0f32.to_bits())));
    assert_eq!(ev, frame(&want));

    // Byte-identical re-encode: the same header, count, word and payload.
    let mut round = vec![0xA6, 0x06, 0x56, 0x05];
    push_varint(&mut round, 3);
    push_varint(&mut round, 0x20);
    round.extend(core::iter::repeat(0x00).take(12));
    round.extend_from_slice(&[0x07, 0x07]);
    assert_eq!(round, bytes);
}

/// Vector 7 — the zero-count case, `a6 06 56 05 00 41 07 07`. A zero-count
/// fixlen array still carries its `fixlen_word` (§4.8), so the hook fires
/// exactly once, with the *correct* kind, and no payload is read. This is the
/// case a naive call-site move breaks, by special-casing `count == 0` ahead of
/// the word.
#[test]
fn zero_count_fixlen_array_still_reports_its_subtype() {
    let (r, ev) = decode_partial(&[0xA6, 0x06, 0x56, 0x05, 0x00, 0x41, 0x07, 0x07]);
    assert_eq!(r, Ok(()));
    assert_eq!(ev, frame(&[Event::ArrayBegin(0, ArrayKind::Fp64, 0)]));

    // An empty fp32 array stays distinguishable from an empty fp64 one.
    let (r32, ev32) = decode_partial(&[0xA6, 0x06, 0x56, 0x05, 0x00, 0x20, 0x07, 0x07]);
    assert_eq!(r32, Ok(()));
    assert_eq!(ev32, frame(&[Event::ArrayBegin(0, ArrayKind::Fp32, 0)]));
    assert_ne!(ev, ev32);
}

/// Vector 8 — the format/schema boundary: `a6 06 56 05 03 22 00*12 07 07`.
/// `0x22` is subtype 2 (string) with elem_len 4. §4.8 permits only fp32 and
/// fp64 as fixlen-array elements, so this is a FORMAT violation judged in step
/// 4, *before* the hook — INVALID, never routed to the §7.3 skip path even
/// though the subtype also contradicts the declared fp32. This is the most
/// likely over-correction when implementing the reorder.
#[test]
fn illegal_fixlen_array_subtype_is_invalid_not_a_skip() {
    for word in [0x22u64, 0x23] {
        // string (2) and blob (3), both with elem_len 4
        let bytes = nested_fixlen_array(3, word, 12);
        let (r, ev) = decode_partial(&bytes);
        assert_eq!(r, Err(Error::InvalidMsg), "word {word:#x}");
        assert!(!ev.iter().any(|e| matches!(e, Event::ArrayBegin(..))));
    }
    // Width mismatches are equally a format violation: fp32 with elem_len != 4,
    // fp64 with elem_len != 8.
    // subtype fp32 (0x0) with elem_len 8 / 1, and fp64 (0x1) with elem_len 4.
    for word in [8u64 << 3, (4u64 << 3) | 0x1, 1u64 << 3] {
        let bytes = nested_fixlen_array(1, word, 8);
        let (r, ev) = decode_partial(&bytes);
        assert_eq!(r, Err(Error::InvalidMsg), "word {word:#x}");
        assert!(!ev.iter().any(|e| matches!(e, Event::ArrayBegin(..))));
    }
}

/// Vector 9 — the cross-check one wire type earlier: `a6 06 56 03 08 00*8
/// 07 07`, an ARRAY_UNSIGNED header at a slot declared `array<fp32, count 5>`.
/// The integer arrays carry no second word, so their hook keeps firing right
/// after the count varint — and it reports `Unsigned`, which is already enough
/// for the receiver to skip rather than bound. Integer arrays are untouched by
/// this change.
#[test]
fn integer_array_header_position_and_kind_are_unchanged() {
    let mut bytes = vec![0xA6, 0x06, 0x56, 0x03];
    push_varint(&mut bytes, 8);
    bytes.extend(core::iter::repeat(0x00).take(8));
    bytes.extend_from_slice(&[0x07, 0x07]);
    let (r, ev) = decode_partial(&bytes);
    assert_eq!(r, Ok(()));
    assert_eq!(ev[2], Event::ArrayBegin(0, ArrayKind::Unsigned, 8));

    // The hook still fires off the count word alone: truncating right after it
    // has already delivered the header (contrast row 4).
    let (r2, ev2) = decode_partial(&[0xA6, 0x06, 0x56, 0x03, 0x08]);
    assert_eq!(r2, Err(Error::Incomplete));
    assert_eq!(ev2[2], Event::ArrayBegin(0, ArrayKind::Unsigned, 8));

    // Signed likewise.
    let (r3, ev3) = decode_partial(&[0xA6, 0x06, 0x56, 0x04, 0x02, 0x00, 0x00, 0x07, 0x07]);
    assert_eq!(r3, Ok(()));
    assert_eq!(ev3[2], Event::ArrayBegin(0, ArrayKind::Signed, 2));
}

/// The FORMAT ceiling `ARRAY_MAX` (2^31-1) keeps firing on the COUNT word —
/// before the `fixlen_word` is read and before the hook. Moving the hook past
/// the word must not drag the ceiling with it: an absurd count is INVALID (not
/// INCOMPLETE, even though no `fixlen_word` follows) and nothing is allocated.
#[test]
fn array_max_ceiling_still_fires_on_the_count_word() {
    let mut bytes = vec![0xA6, 0x06, 0x56, 0x05];
    push_varint(&mut bytes, i32::MAX as u64 + 1);
    // No fixlen_word at all: the ceiling must reject before it is even missed.
    let (r, ev) = decode_partial(&bytes);
    assert_eq!(r, Err(Error::InvalidMsg));
    assert!(!ev.iter().any(|e| matches!(e, Event::ArrayBegin(..))));

    // Still INVALID with a perfectly good fp32 word following it.
    let mut with_word = bytes.clone();
    push_varint(&mut with_word, 0x20);
    let (r2, _) = decode_partial(&with_word);
    assert_eq!(r2, Err(Error::InvalidMsg));

    // ARRAY_MAX itself is a legal count; it is short of payload, not malformed.
    let mut at_max = vec![0xA6, 0x06, 0x56, 0x05];
    push_varint(&mut at_max, i32::MAX as u64);
    push_varint(&mut at_max, 0x20);
    let (r3, ev3) = decode_partial(&at_max);
    assert_eq!(r3, Err(Error::Incomplete));
    assert_eq!(
        ev3[2],
        Event::ArrayBegin(0, ArrayKind::Fp32, i32::MAX as usize)
    );
}

/// MESSAGE_SPEC §7.4: an occurrence skipped under §7.3 is not an occurrence.
/// The corelib's part of that is to keep the two occurrences apart — a
/// correctly typed fp32 array followed by a mis-typed fp64 one at the same id
/// must arrive as two headers with *different* kinds, so the receiver can drop
/// the second without disturbing the value it took from the first.
#[test]
fn a_mistyped_later_occurrence_is_distinguishable_from_an_earlier_good_one() {
    let mut bytes = vec![0xA6, 0x06, 0x56];
    bytes.extend_from_slice(&[0x05, 0x02, 0x20]); // fp32, 2 elements
    bytes.extend(core::iter::repeat(0x00).take(8));
    bytes.extend_from_slice(&[0x05, 0x02, 0x41]); // fp64, 2 elements, same id
    bytes.extend(core::iter::repeat(0x00).take(16));
    bytes.extend_from_slice(&[0x07, 0x07]);
    let (r, ev) = decode_partial(&bytes);
    assert_eq!(r, Ok(()));
    let begins: Vec<&Event> = ev
        .iter()
        .filter(|e| matches!(e, Event::ArrayBegin(..)))
        .collect();
    assert_eq!(
        begins,
        [
            &Event::ArrayBegin(0, ArrayKind::Fp32, 2),
            &Event::ArrayBegin(0, ArrayKind::Fp64, 2),
        ]
    );
}

/// The header hook fires exactly once per array field, never per element —
/// unchanged by the reorder, and asserted for a chunked feed too, where the
/// field straddles a boundary and could plausibly be replayed.
#[test]
fn header_fires_exactly_once_even_when_the_field_straddles_a_chunk() {
    let bytes = nested_fixlen_array(4, 0x41, 4 * 8);
    for split in 1..bytes.len() {
        let mut rec = Recorder::new();
        let mut is = IStream::new();
        is.feed(&bytes[..split], &mut rec).ok();
        is.feed(&bytes[split..], &mut rec).expect("decode failed");
        let begins: Vec<&Event> = rec
            .events
            .iter()
            .filter(|e| matches!(e, Event::ArrayBegin(..)))
            .collect();
        assert_eq!(
            begins,
            [&Event::ArrayBegin(0, ArrayKind::Fp64, 4)],
            "split at {split}"
        );
    }
}

// --- INVALID is terminal for the stream (CORELIB_PLAN §5.2) ------------------
//
// The §5.2 outcome table marks `INVALID` "no — terminal" in the *can more bytes
// change it?* column: the bytes consumed so far are malformed regardless of what
// follows, so no later chunk may talk the same decoder back into `COMPLETE` or
// `INCOMPLETE`. §7.2 item 4 turns that into a testable property — feeding an
// input one byte at a time (and in odd-sized chunks) must yield the same verdict
// as feeding it whole — which a decoder that resynchronizes after a rejection
// cannot satisfy: its answer becomes chunk-size dependent.

/// Every collection of malformed inputs used below, each paired with well-formed
/// bytes that follow the malformed construct. A decoder that resynchronizes
/// would decode those trailing bytes and report `Ok`/`Incomplete`.
fn malformed_with_valid_tail() -> Vec<(&'static str, Vec<u8>)> {
    let mut v: Vec<(&'static str, Vec<u8>)> = Vec::new();

    // fp64 fixlen word declaring length 11 (≠ 8), then a whole unsigned field.
    v.push(("fp64 wrong width", vec![0x0A, 0x59, 0x08, 0x2A]));
    // Reserved fixlen subtype 0x4, then a whole unsigned field.
    v.push(("reserved subtype", vec![0x02, (4 << 3) | 0x04, 0x08, 0x2A]));
    // A value varint wider than 64 bits, then a whole unsigned field.
    let mut overlong = vec![0x00];
    overlong.extend(core::iter::repeat(0x80).take(10));
    overlong.extend_from_slice(&[0x01, 0x08, 0x2A]);
    v.push(("overlong value varint", overlong));
    // A header varint wider than 64 bits.
    v.push(("overlong header varint", vec![0x80; 12]));
    // A sequence-end marker with no open sequence, then a whole unsigned field.
    v.push(("dangling sequence end", vec![0x00, 0x2A, 0x07, 0x08, 0x2A]));
    // An array count above ARRAY_MAX, then a whole unsigned field.
    let mut over_count = vec![0x03];
    push_varint(&mut over_count, i32::MAX as u64 + 1);
    over_count.extend_from_slice(&[0x08, 0x2A]);
    v.push(("count above ARRAY_MAX", over_count));

    v
}

/// Once a `feed` has answered `InvalidMsg`, the same `IStream` keeps answering
/// `InvalidMsg` — for a complete valid message, for an empty probe chunk and for
/// a truncated prefix alike. `reset()` is the documented reuse hook and is the
/// only thing that brings the decoder back.
#[test]
fn invalid_is_terminal_and_only_reset_clears_it() {
    for (name, bytes) in malformed_with_valid_tail() {
        let mut rec = Recorder::new();
        let mut is = IStream::new();
        assert_eq!(
            is.feed(&bytes, &mut rec),
            Err(Error::InvalidMsg),
            "[{name}] not rejected in the first place"
        );

        // A complete, perfectly valid message afterwards changes nothing.
        assert_eq!(
            is.feed(&[0x08, 0x2A], &mut rec),
            Err(Error::InvalidMsg),
            "[{name}] resynchronized on a valid message"
        );
        // Neither does the empty end-of-input probe …
        assert_eq!(
            is.feed(&[], &mut rec),
            Err(Error::InvalidMsg),
            "[{name}] empty probe escaped the rejection"
        );
        // … nor a truncated prefix, which must not degrade to Incomplete.
        assert_eq!(
            is.feed(&[0x08], &mut rec),
            Err(Error::InvalidMsg),
            "[{name}] degraded to Incomplete"
        );

        // `reset()` is the reuse hook: after it the decoder is new again.
        is.reset();
        let mut fresh = Recorder::new();
        assert_eq!(
            is.feed(&[0x08, 0x2A], &mut fresh),
            Ok(()),
            "[{name}] reset did not restore the decoder"
        );
        assert_eq!(fresh.events, [Event::Unsigned(1, 42)]);
    }
}

/// A rejection latched mid-way through a construct must survive the states that
/// carry bytes across the boundary — a non-empty carry and an open sequence —
/// neither of which may be "recovered" by later bytes.
#[test]
fn invalid_survives_a_pending_carry_and_an_open_sequence() {
    // Open sequence (depth 1), then a malformed fixlen word, then a
    // sequence-end that would balance the depth the decoder still holds.
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    assert_eq!(is.feed(&[0x0E], &mut rec), Err(Error::Incomplete));
    assert_eq!(is.feed(&[0x0A, 0x59], &mut rec), Err(Error::InvalidMsg));
    assert_eq!(is.feed(&[0x07], &mut rec), Err(Error::InvalidMsg));

    // A header carried over the boundary, then bytes that make the *carried*
    // field malformed, then a whole valid field.
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    assert_eq!(is.feed(&[0x02], &mut rec), Err(Error::Incomplete));
    assert_eq!(
        is.feed(&[(4 << 3) | 0x04], &mut rec),
        Err(Error::InvalidMsg)
    );
    assert_eq!(is.feed(&[0x08, 0x01], &mut rec), Err(Error::InvalidMsg));
}

/// §7.2 item 4: the verdict is a property of the bytes, not of how they were
/// cut. For every malformed input, the status of the *last* `feed` of a chunked
/// run equals the one-shot status, at every chunk size — which only holds if the
/// rejection sticks.
#[test]
fn the_chunked_verdict_matches_the_one_shot_verdict_at_every_chunk_size() {
    fn final_status(msg: &[u8], size: usize) -> Result<(), Error> {
        let mut rec = Recorder::new();
        let mut is = IStream::new();
        let mut last = Ok(());
        for chunk in msg.chunks(size) {
            last = is.feed(chunk, &mut rec);
        }
        last
    }

    for (name, bytes) in malformed_with_valid_tail() {
        let one_shot = IStream::new().feed(&bytes, &mut Recorder::new());
        assert_eq!(one_shot, Err(Error::InvalidMsg), "[{name}] one-shot");
        for size in 1..=bytes.len() {
            assert_eq!(
                final_status(&bytes, size),
                one_shot,
                "[{name}] chunk size {size} disagrees with the one-shot verdict"
            );
        }
    }
}
