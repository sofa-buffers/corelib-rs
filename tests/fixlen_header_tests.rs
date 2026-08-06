//! The scalar fixlen header (CORELIB_PLAN §5.2) — regression suite for the
//! `Visitor::fixlen_begin` hook.
//!
//! A `string`/`blob` field's `maxlen` bound is fully established by its **length
//! word**: the number that exceeds the bound is already on the wire, and no
//! later byte can make it legal. §5.2 makes **INVALID dominate INCOMPLETE**, so a
//! message truncated *exactly at that word* must stay INVALID — it must not
//! degrade to INCOMPLETE just because the payload never arrived.
//!
//! The corelib is schema-agnostic, so the reject itself is the consumer's to
//! make. What the corelib owes the consumer is the *information and the
//! ordering*: [`Visitor::fixlen_begin`] fires once per scalar fixlen field,
//! after the length word is read and validated and **before** any payload byte —
//! for `total == 0` too — carrying the subtype on the wire so a consumer can
//! judge the length there or route a contradicting subtype to a §7.3 skip.
//!
//! `fixlen_begin` is the scalar twin of `array_begin`: the same "announce the
//! bound-bearing word before its payload" hook, one field kind over.
//!
//! Both entry points share one dispatch — [`sofab::decode`] is a single
//! whole-buffer `feed` — so a whole message and the same bytes fed a byte at a
//! time run the exact same code; this suite exercises both.

use sofab::{Error, FixlenType, IStream, Id, Visitor};

/// One header/payload event, recorded in order. A dedicated visitor keeps this
/// suite's assertions about *ordering* — header before the first payload byte —
/// independent of payload reassembly.
#[derive(Debug, Clone, PartialEq)]
enum Ev {
    FixlenBegin(Id, FixlenType, usize),
    /// A payload chunk: `(id, total, offset, len)` — bytes elided, only the
    /// shape matters for ordering.
    Str(Id, usize, usize, usize),
    Blob(Id, usize, usize, usize),
    Fp32(Id),
    Fp64(Id),
}

#[derive(Default)]
struct Rec {
    events: Vec<Ev>,
}

impl Visitor for Rec {
    fn fixlen_begin(&mut self, id: Id, subtype: FixlenType, total: usize) {
        self.events.push(Ev::FixlenBegin(id, subtype, total));
    }
    fn string(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {
        self.events.push(Ev::Str(id, total, offset, chunk.len()));
    }
    fn blob(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {
        self.events.push(Ev::Blob(id, total, offset, chunk.len()));
    }
    fn fp32(&mut self, id: Id, _value: f32) {
        self.events.push(Ev::Fp32(id));
    }
    fn fp64(&mut self, id: Id, _value: f64) {
        self.events.push(Ev::Fp64(id));
    }
}

/// Feed `bytes` in one shot; return the three-valued outcome (§7) and every event.
fn feed(bytes: &[u8]) -> (Result<(), Error>, Vec<Ev>) {
    let mut rec = Rec::default();
    let mut is = IStream::new();
    let outcome = is.feed(bytes, &mut rec);
    (outcome, rec.events)
}

/// `[ FIXLEN id 3 ][ length_word ]` + `payload` bytes.
///
/// The tag `0x1a` is a fixlen field at id 3; the length word is
/// `(len << 3) | subtype` (subtype 2 = string, 3 = blob).
fn string_field(len: usize) -> Vec<u8> {
    let mut v = vec![0x1a];
    push_varint(&mut v, ((len as u64) << 3) | 0x2);
    v.resize(v.len() + len, b'x');
    v
}

/// A tiny local varint helper so this file needs no `mod common` machinery.
fn push_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let mut b = (value as u8) & 0x7F;
        value >>= 7;
        if value != 0 {
            b |= 0x80;
        }
        out.push(b);
        if value == 0 {
            break;
        }
    }
}

// --- the primary vector: truncated exactly at the length word ----------------

#[test]
fn header_fires_at_the_length_word_before_any_payload_byte() {
    // `1a 52`: tag + length word for a 10-byte string, nothing more. This is the
    // vector from the finding. The header must already be recorded — a consumer
    // measuring `total` against a `maxlen` of, say, 8 can reject *now* — even
    // though the corelib's own outcome for these bytes is INCOMPLETE.
    let bytes = vec![0x1a, 0x52]; // (10 << 3) | 2 == 0x52
    let (outcome, events) = feed(&bytes);
    assert_eq!(outcome, Err(Error::Incomplete));
    assert_eq!(events, [Ev::FixlenBegin(3, FixlenType::Str, 10)]);
}

#[test]
fn header_is_announced_only_once_the_length_word_has_arrived() {
    // Byte by byte: nothing on the tag alone, the header on the length word, and
    // then payload chunks. Pins the exact byte the announcement lands on.
    let bytes = string_field(10);
    let mut rec = Rec::default();
    let mut is = IStream::new();
    for (i, b) in bytes.iter().enumerate() {
        let _ = is.feed(&[*b], &mut rec);
        let announced = rec.events.iter().any(|e| matches!(e, Ev::FixlenBegin(..)));
        // byte 0 is the tag, byte 1 the length word (single-byte for len 10).
        assert_eq!(
            announced,
            i >= 1,
            "fixlen_begin after byte {i} ({b:#04x}) — expected only from the length word on",
        );
    }
    // And the header strictly precedes the first payload chunk.
    let first_payload = rec
        .events
        .iter()
        .position(|e| matches!(e, Ev::Str(..)))
        .expect("payload delivered");
    assert!(matches!(rec.events[0], Ev::FixlenBegin(..)));
    assert!(first_payload > 0);
}

#[test]
fn header_fires_exactly_once_across_a_chunked_payload() {
    // Cost invariant: one header for a 10-byte string, not one per chunk. Feed in
    // 3-byte bites so the payload spans several `string` calls.
    let bytes = string_field(10);
    let mut rec = Rec::default();
    let mut is = IStream::new();
    for chunk in bytes.chunks(3) {
        let _ = is.feed(chunk, &mut rec);
    }
    let begins = rec
        .events
        .iter()
        .filter(|e| matches!(e, Ev::FixlenBegin(..)))
        .count();
    assert_eq!(begins, 1);
}

// --- the in-bound control: an ordering fix, not a blanket reject -------------

#[test]
fn in_bound_string_truncated_at_its_word_still_announces_then_completes() {
    // The header fires for an in-bound length too — it is the *consumer* that
    // decides whether `total` is over a bound, so the corelib announces every
    // scalar fixlen field the same way. Truncated at the word, this is
    // INCOMPLETE (payload owed); whole, it decodes with the same single header.
    let (outcome, events) = feed(&[0x1a, 0x52]); // total 10, header only
    assert_eq!(outcome, Err(Error::Incomplete));
    assert_eq!(events, [Ev::FixlenBegin(3, FixlenType::Str, 10)]);

    let (outcome, events) = feed(&string_field(10));
    assert_eq!(outcome, Ok(()));
    assert_eq!(
        events,
        [
            Ev::FixlenBegin(3, FixlenType::Str, 10),
            Ev::Str(3, 10, 0, 10),
        ]
    );
}

// --- zero-length fields: the header must still fire --------------------------

#[test]
fn empty_string_announces_the_header_before_its_zero_length_chunk() {
    // `1a 02`: an empty string. `total == 0` still fires the header — once —
    // ahead of the single empty payload chunk the callback contract promises.
    let (outcome, events) = feed(&[0x1a, 0x02]);
    assert_eq!(outcome, Ok(()));
    assert_eq!(
        events,
        [Ev::FixlenBegin(3, FixlenType::Str, 0), Ev::Str(3, 0, 0, 0)]
    );
}

#[test]
fn empty_blob_announces_the_header_with_its_subtype() {
    // `1a 03`: an empty blob. Same shape, subtype `Blob`.
    let (outcome, events) = feed(&[0x1a, 0x03]);
    assert_eq!(outcome, Ok(()));
    assert_eq!(
        events,
        [
            Ev::FixlenBegin(3, FixlenType::Blob, 0),
            Ev::Blob(3, 0, 0, 0)
        ]
    );
}

// --- blob carries its own subtype --------------------------------------------

#[test]
fn blob_field_announces_the_blob_subtype() {
    // `1a 53`: (10 << 3) | 3 — a 10-byte blob. The subtype on the wire is Blob,
    // so a consumer whose field expects a string routes it to a §7.3 skip.
    let mut bytes = vec![0x1a, 0x53];
    bytes.resize(2 + 10, 0x00);
    let (outcome, events) = feed(&bytes);
    assert_eq!(outcome, Ok(()));
    assert_eq!(
        events,
        [
            Ev::FixlenBegin(3, FixlenType::Blob, 10),
            Ev::Blob(3, 10, 0, 10),
        ]
    );
}

// --- float scalars are announced too, and never for array elements -----------

#[test]
fn scalar_fp32_announces_its_header_at_the_length_word() {
    // `1a 20`: (4 << 3) | 0 — a scalar fp32 (subtype 0, width 4). Floats are
    // fixed-width, but the header still fires once, before the payload, so a
    // consumer can route a subtype it did not expect to a §7.3 skip.
    let mut bytes = vec![0x1a, 0x20];
    bytes.resize(2 + 4, 0x00);
    let (outcome, events) = feed(&bytes);
    assert_eq!(outcome, Ok(()));
    assert_eq!(
        events,
        [Ev::FixlenBegin(3, FixlenType::Fp32, 4), Ev::Fp32(3)]
    );
}

#[test]
fn scalar_fp32_truncated_at_its_word_is_announced_then_incomplete() {
    // The word arrived and is a legal fp32, so the field is decidable now even
    // though its 4 payload bytes are still owed.
    let (outcome, events) = feed(&[0x1a, 0x20]);
    assert_eq!(outcome, Err(Error::Incomplete));
    assert_eq!(events, [Ev::FixlenBegin(3, FixlenType::Fp32, 4)]);
}

#[test]
fn scalar_fp32_split_across_its_value_fires_the_header_exactly_once() {
    // The std decoder carries small items and re-parses them, so a float whose
    // 4 value bytes straddle a chunk boundary must not re-announce on the resume.
    // Feed the header word, then the value one byte at a time.
    let mut bytes = vec![0x1a, 0x20];
    bytes.resize(2 + 4, 0x00);
    let mut rec = Rec::default();
    let mut is = IStream::new();
    for chunk in bytes.chunks(1) {
        let _ = is.feed(chunk, &mut rec);
    }
    let begins = rec
        .events
        .iter()
        .filter(|e| matches!(e, Ev::FixlenBegin(..)))
        .count();
    assert_eq!(begins, 1);
    assert_eq!(
        rec.events,
        [Ev::FixlenBegin(3, FixlenType::Fp32, 4), Ev::Fp32(3)]
    );
}

#[test]
fn scalar_fp64_announces_its_header_at_the_length_word() {
    // `1a 41`: (8 << 3) | 1 — a scalar fp64 (subtype 1, width 8).
    let mut bytes = vec![0x1a, 0x41];
    bytes.resize(2 + 8, 0x00);
    let (outcome, events) = feed(&bytes);
    assert_eq!(outcome, Ok(()));
    assert_eq!(
        events,
        [Ev::FixlenBegin(3, FixlenType::Fp64, 8), Ev::Fp64(3)]
    );
}

#[test]
fn fixlen_array_elements_do_not_fire_the_scalar_header() {
    // An fp32[2] array: its header is `array_begin`, not `fixlen_begin`. The
    // scalar hook must stay confined to the scalar path — no `FixlenBegin` for
    // the array or its elements.
    // `05 02 20`: FIXLENARRAY id 0, count 2, fixlen_word fp32(4), + 8 zero bytes.
    let mut bytes = vec![0x05, 0x02, 0x20];
    bytes.resize(3 + 8, 0x00);
    let (outcome, events) = feed(&bytes);
    assert_eq!(outcome, Ok(()));
    assert!(
        !events.iter().any(|e| matches!(e, Ev::FixlenBegin(..))),
        "array elements must not fire the scalar fixlen header: {events:?}",
    );
}

// --- a malformed float word is rejected before the header fires --------------

#[test]
fn malformed_float_width_is_invalid_and_announces_nothing() {
    // `1a 08`: (1 << 3) | 0 — subtype fp32 but declared length 1, not its one
    // legal width of 4. §4.6 makes this a format violation, judged *before* the
    // header hook — the reject dominates and nothing is announced.
    let (outcome, events) = feed(&[0x1a, 0x08]);
    assert_eq!(outcome, Err(Error::InvalidMsg));
    assert!(events.is_empty());
}
