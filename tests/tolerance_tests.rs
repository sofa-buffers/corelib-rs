//! Input that is **non-canonical but well-formed** must decode to the value it
//! denotes and re-encode canonically — never `INVALID` (CORELIB_PLAN §7.2 item
//! 5b) — and the one header whose id a decoder never *uses* must still be
//! bounded like every other (§6.2, §7.2 item 5).
//!
//! These are the cases where a decoder can be *stricter* than the format allows.
//! They are the mirror of the malformed-input tests in `istream_tests.rs`, and
//! the ones a majority-vote conformance check cannot catch: an implementation may
//! be uniformly too strict and every port agree on the wrong answer.

// Headers below are written as `(id << 3) | <wire type>` throughout, including
// the unsigned type whose tag is `0x0`. Dropping the `| 0x0` would save nothing
// and hide which of the eight wire types each header names.
#![allow(clippy::identity_op)]

use sofab::{
    decode, ArrayKind, Error, FixlenType, IStream, Id, OStream, Signed, Status, Unsigned, Visitor,
};

#[derive(Debug, PartialEq, Clone)]
enum Event {
    Unsigned(Id, Unsigned),
    Signed(Id, Signed),
    Fp64(Id, u64),
    Str(Id, Vec<u8>),
    Array(Id, ArrayKind, usize),
    SeqBegin(Id),
    SeqEnd,
}

#[derive(Default)]
struct Rec {
    ev: Vec<Event>,
}

impl Visitor for Rec {
    fn unsigned(&mut self, id: Id, v: Unsigned) {
        self.ev.push(Event::Unsigned(id, v));
    }
    fn signed(&mut self, id: Id, v: Signed) {
        self.ev.push(Event::Signed(id, v));
    }
    fn fp64(&mut self, id: Id, v: f64) {
        self.ev.push(Event::Fp64(id, v.to_bits()));
    }
    fn string(&mut self, id: Id, _total: usize, offset: usize, chunk: &[u8]) {
        if offset == 0 {
            self.ev.push(Event::Str(id, chunk.to_vec()));
        } else if let Some(Event::Str(_, buf)) = self.ev.last_mut() {
            buf.extend_from_slice(chunk);
        }
    }
    fn array_begin(&mut self, id: Id, kind: ArrayKind, count: usize) {
        self.ev.push(Event::Array(id, kind, count));
    }
    fn sequence_begin(&mut self, id: Id) {
        self.ev.push(Event::SeqBegin(id));
    }
    fn sequence_end(&mut self) {
        self.ev.push(Event::SeqEnd);
    }
}

/// Decode `bytes` twice — one-shot and one byte at a time — and require both
/// paths to agree. A tolerance that only the contiguous fast path grants is not
/// a tolerance.
fn events(bytes: &[u8]) -> Vec<Event> {
    let mut one_shot = Rec::default();
    assert_eq!(
        decode(bytes, &mut one_shot),
        Ok(Status::Complete),
        "well-formed input must decode"
    );

    let mut chunked = Rec::default();
    let mut is = IStream::new();
    let mut last = Ok(Status::Complete);
    for b in bytes {
        // Every byte but the final one leaves the decoder mid-message, which is
        // `Incomplete` — an outcome, not an error (§5.2). Only the last one has
        // to complete.
        last = is.feed(&[*b], &mut chunked);
        assert!(
            matches!(last, Ok(Status::Complete) | Ok(Status::Incomplete)),
            "byte-at-a-time feed reported {last:?}"
        );
    }
    assert_eq!(
        last,
        Ok(Status::Complete),
        "the last byte must complete the message"
    );
    assert_eq!(one_shot.ev, chunked.ev, "the two decode paths disagreed");
    one_shot.ev
}

/// Minimal varint of `v`.
fn varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let mut b = (v as u8) & 0x7F;
        v >>= 7;
        if v != 0 {
            b |= 0x80;
        }
        out.push(b);
        if v == 0 {
            return;
        }
    }
}

/// `v` encoded in exactly `n` bytes — minimal for `n == len(v)`, padded with
/// redundant continuation bytes beyond that. §4.1 permits the padding: a varint
/// denotes a value, not a byte count.
fn varint_padded(out: &mut Vec<u8>, mut v: u64, n: usize) {
    for i in 0..n {
        let mut b = (v as u8) & 0x7F;
        v >>= 7;
        if i + 1 < n {
            b |= 0x80;
        }
        out.push(b);
    }
}

/// §7.2 5b: a non-minimal varint at a **field header**, at a **`fixlen_word`**
/// and at an **element count** — the three positions §4.1 names — decodes to the
/// value it denotes.
#[test]
fn non_minimal_varints_decode_to_the_value_they_denote() {
    let mut w = Vec::new();

    // Field header padded to the full 10 bytes: id 7, unsigned. Value padded too.
    varint_padded(&mut w, (7u64 << 3) | 0x0, 10);
    varint_padded(&mut w, 42, 6);

    // fixlen_word padded: id 1, string, length 3.
    varint(&mut w, (1u64 << 3) | 0x2);
    varint_padded(&mut w, (3u64 << 3) | FixlenType::Str as u64, 5);
    w.extend_from_slice(b"abc");

    // Element count padded: id 2, unsigned array, count 2.
    varint(&mut w, (2u64 << 3) | 0x3);
    varint_padded(&mut w, 2, 4);
    varint(&mut w, 1);
    varint_padded(&mut w, 300, 4); // and a padded element, while we are here

    assert_eq!(
        events(&w),
        [
            Event::Unsigned(7, 42),
            Event::Str(1, b"abc".to_vec()),
            Event::Array(2, ArrayKind::Unsigned, 2),
            Event::Unsigned(2, 1),
            Event::Unsigned(2, 300),
        ]
    );
}

/// §7.2 5b: a **sequence-end header whose id is non-zero but within `ID_MAX`**
/// decodes as an ordinary sequence end. The id is discarded (§4.9) — but
/// discarding it is not the same as rejecting the header that carries it.
#[test]
fn a_sequence_end_with_a_non_zero_in_range_id_is_an_ordinary_end() {
    for id in [1u64, 15, 16, 1000, sofab::ID_MAX as u64] {
        let mut w = Vec::new();
        varint(&mut w, (3u64 << 3) | 0x6); // sequence start, id 3
        varint(&mut w, (9u64 << 3) | 0x0); // unsigned id 9
        varint(&mut w, 5);
        varint(&mut w, (id << 3) | 0x7); // sequence end carrying `id`

        assert_eq!(
            events(&w),
            [Event::SeqBegin(3), Event::Unsigned(9, 5), Event::SeqEnd],
            "sequence end with id {id} must decode as a plain end"
        );
    }
}

/// The other half of 5b for that header: whatever id it arrived with, it
/// **re-encodes as the canonical single byte `0x07`**. Tolerating a form on the
/// way in must not propagate it on the way out.
#[test]
fn a_non_canonical_sequence_end_re_encodes_as_0x07() {
    let mut w = Vec::new();
    varint(&mut w, (3u64 << 3) | 0x6);
    varint(&mut w, (9u64 << 3) | 0x0);
    varint(&mut w, 5);
    varint(&mut w, (1234u64 << 3) | 0x7); // three bytes on the wire

    // Re-encode from what the decoder reported.
    let mut out = [0u8; 32];
    let used = {
        let mut os = OStream::new(&mut out);
        for e in events(&w) {
            match e {
                Event::SeqBegin(id) => os.write_sequence_begin_lazy(id).unwrap(),
                Event::SeqEnd => os.write_sequence_end().unwrap(),
                Event::Unsigned(id, v) => os.write_unsigned(id, v).unwrap(),
                other => panic!("unexpected event {other:?}"),
            }
        }
        os.bytes_used()
    };

    assert_eq!(&out[..used], &[0x1E, 0x48, 0x05, 0x07]);
    assert!(used < w.len(), "the canonical form must be the shorter one");
}

/// §7.2 item 5: the `ID_MAX` ceiling binds the **sequence-end** header too.
///
/// §6.2 admits no exception, and an implementation that validates the id only in
/// the branches that *use* it passes the value-bearing case and misses this one —
/// which is exactly what `istream_tests::id_above_max_is_invalid` covers and this
/// does not.
#[test]
fn an_oversized_id_on_a_sequence_end_header_is_invalid() {
    let over = sofab::ID_MAX as u64 + 1;

    let mut w = Vec::new();
    varint(&mut w, (3u64 << 3) | 0x6); // an open sequence, so the end is balanced
    varint(&mut w, (over << 3) | 0x7);

    let mut rec = Rec::default();
    assert_eq!(IStream::new().feed(&w, &mut rec), Err(Error::InvalidMsg));

    let mut rec = Rec::default();
    assert_eq!(decode(&w, &mut rec), Err(Error::InvalidMsg));

    // And the ceiling itself is still fine — the boundary is where it says it is.
    let mut ok = Vec::new();
    varint(&mut ok, (3u64 << 3) | 0x6);
    varint(&mut ok, ((sofab::ID_MAX as u64) << 3) | 0x7);
    assert_eq!(events(&ok), [Event::SeqBegin(3), Event::SeqEnd]);
}
