//! Tests for the fast contiguous [`decode`] path (the zero-copy "advance a
//! pointer over the buffer" reader).
//!
//! Strategy: the streaming [`IStream::feed`] path is already pinned to the shared
//! cross-language vectors by `vectors_tests.rs`. Here we assert that the one-shot
//! [`decode`] path produces **exactly the same events** for every shared vector,
//! plus the two properties unique to the fast path: single-call (zero-copy)
//! string/blob delivery, and strict rejection of truncated input.

mod common;

use common::Recorder;
use serde_json::Value;
use sofab::{decode, Error, IStream, Id, Status, Visitor};

const VECTORS_JSON: &str = include_str!("../assets/test_vectors.json");

fn parse_requires(v: &Value) -> Vec<String> {
    v.get("requires")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn vector_supported(_requires: &[String]) -> bool {
    // This build has every wire type and the 64-bit value width compiled in, so
    // every shared vector is representable.
    true
}

fn hex_to_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

/// For every shared vector, the fast `decode` path must yield the same events as
/// the streaming `feed` path.
#[test]
fn fast_path_matches_streaming_on_all_vectors() {
    let doc: Value = serde_json::from_str(VECTORS_JSON).unwrap();
    let vectors = doc["vectors"].as_array().unwrap();

    let mut ran = 0;
    for vec in vectors {
        if !vector_supported(&parse_requires(vec)) {
            continue;
        }
        ran += 1;
        let name = vec["name"].as_str().unwrap();
        let bytes = hex_to_bytes(vec["serialized"]["hex"].as_str().unwrap());

        let mut fast = Recorder::new();
        assert_eq!(
            decode(&bytes, &mut fast),
            Ok(Status::Complete),
            "[{name}] decode failed"
        );

        let mut streamed = Recorder::new();
        assert_eq!(
            IStream::new().feed(&bytes, &mut streamed),
            Ok(Status::Complete)
        );

        assert_eq!(fast.events, streamed.events, "[{name}] fast vs streaming");
    }
    assert!(ran > 0);
}

/// The fast path delivers a string/blob payload as one borrowed slice (offset 0,
/// whole length) — no chunking, no copy.
#[test]
fn strings_delivered_zero_copy_single_call() {
    #[derive(Default)]
    struct Once {
        calls: usize,
        ok: bool,
    }
    impl Visitor for Once {
        fn string(&mut self, _id: Id, total: usize, offset: usize, chunk: &[u8]) {
            self.calls += 1;
            self.ok = offset == 0 && chunk.len() == total;
        }
    }
    // "Hello Couch!" string at id 0 (vector `string_hello`).
    let bytes = [
        0x02, 0x62, 0x48, 0x65, 0x6C, 0x6C, 0x6F, 0x20, 0x43, 0x6F, 0x75, 0x63, 0x68, 0x21,
    ];
    let mut v = Once::default();
    assert_eq!(decode(&bytes, &mut v), Ok(Status::Complete));
    assert_eq!(v.calls, 1, "string delivered in exactly one call");
    assert!(v.ok, "whole string delivered at offset 0");
}

/// A message cut off mid-field is **incomplete**, not malformed (MESSAGE_SPEC
/// §7). The one-shot decoder surfaces `Incomplete` — distinct from `InvalidMsg` —
/// and feeding the same prefix to a streaming decoder yields the same outcome
/// (the caller simply feeds more).
#[test]
fn truncated_input_is_incomplete_not_invalid() {
    fn dec(bytes: &[u8]) -> Result<Status, Error> {
        decode(bytes, &mut Recorder::new())
    }
    // header (id0, unsigned) present, value varint missing.
    assert_eq!(dec(&[0x00]), Ok(Status::Incomplete));
    // string header says 5 bytes, only 2 follow.
    assert_eq!(dec(&[0x02, 0x2A, 0x41, 0x42]), Ok(Status::Incomplete));
    // sequence opened, never closed.
    assert_eq!(dec(&[0x0E, 0x00, 0x2A]), Ok(Status::Incomplete));

    // The streaming decoder reports the same outcome on the bare prefix — it is
    // not accepted (Ok) and not rejected (InvalidMsg): it waits for more.
    let mut sink = Recorder::new();
    assert_eq!(
        IStream::new().feed(&[0x00], &mut sink),
        Ok(Status::Incomplete)
    );
}

/// The three decode outcomes (MESSAGE_SPEC §7) are distinct: a lone dangling
/// continuation byte is `Status::Incomplete`, an over-long (>64-bit) varint is
/// `Err(InvalidMsg)`, and a whole message is `Status::Complete`. The first and
/// the last are both `Ok` — `INCOMPLETE` is not an error (CORELIB_PLAN §5.2.1) —
/// and it is the returned `Status`, not a second accessor, that tells them
/// apart.
#[test]
fn three_outcomes_are_distinct() {
    fn dec(bytes: &[u8]) -> Result<Status, Error> {
        decode(bytes, &mut Recorder::new())
    }
    // A lone 0x80: a varint header with the continuation bit set and no
    // terminating byte — ends mid-field → INCOMPLETE.
    assert_eq!(dec(&[0x80]), Ok(Status::Incomplete));

    // 0x00 header (id0, unsigned) then 11 continuation bytes: the value varint
    // exceeds 64 bits → malformed regardless of what follows → INVALID.
    assert_eq!(
        dec(&[0x00, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80]),
        Err(Error::InvalidMsg)
    );

    // id0 unsigned = 42, ending exactly at a field boundary → COMPLETE.
    assert_eq!(dec(&[0x00, 0x2A]), Ok(Status::Complete));
}

/// A receiver-configured decode-limit violation (`LimitExceeded`) is policy, not
/// wire malformation, so it must be a category of its own — distinguishable from
/// every other outcome and, above all, from `InvalidMsg`. Enforcement lives in
/// generated code (sofa-buffers/generator#102); this corelib only owns the
/// category, so the guarantee under test is purely that the variant is distinct.
#[test]
fn limit_exceeded_is_distinct_from_invalid_msg() {
    // The whole point: exceeding a receiver limit is not malformation. The same
    // bytes are valid for a receiver with a higher (or no) limit, so a differential
    // fuzzer must never conflate the two.
    assert_ne!(Error::LimitExceeded, Error::InvalidMsg);

    // And distinct from every other error code, too.
    for other in [Error::Argument, Error::BufferFull, Error::InvalidMsg] {
        assert_ne!(Error::LimitExceeded, other);
    }

    // `INCOMPLETE` is no longer in that list because it is not an error code at
    // all (§5.2.1, §6.3): it arrives on the success arm as `Status::Incomplete`,
    // so a policy rejection cannot be confused with a merely truncated stream —
    // the two are not even the same arm of the `Result`.
    let mut sink = Recorder::new();
    assert_eq!(
        IStream::new().feed(&[0x00], &mut sink),
        Ok(Status::Incomplete)
    );
    assert_ne!(
        Ok(Status::Incomplete),
        Err::<Status, Error>(Error::LimitExceeded)
    );

    // Its Display text is its own, so logs/telemetry can tell a policy rejection
    // apart from a malformed message.
    assert_ne!(
        Error::LimitExceeded.to_string(),
        Error::InvalidMsg.to_string()
    );
}

/// `Error` renders via `Display` and is a `std::error::Error` (the std-only
/// addition over the no_std port).
#[test]
fn error_display_and_std_error() {
    for e in [
        Error::Argument,
        Error::BufferFull,
        Error::InvalidMsg,
        Error::LimitExceeded,
    ] {
        let s = format!("{e}");
        assert!(!s.is_empty(), "{e:?} has empty Display");
        let dyn_err: &dyn std::error::Error = &e;
        assert_eq!(dyn_err.to_string(), s);
    }
}

/// A decoder can be `reset` and reused for a fresh message without reallocating.
#[test]
fn istream_reset_reuses_decoder() {
    let mut is = IStream::new();
    let mut a = Recorder::new();
    assert_eq!(is.feed(&[0x00, 0x2A], &mut a), Ok(Status::Complete)); // id0 unsigned 42
    assert_eq!(is.feed(&[], &mut a), Ok(Status::Complete)); // clean boundary => Ok

    is.reset();
    let mut b = Recorder::new();
    assert_eq!(is.feed(&[0x08, 0x07], &mut b), Ok(Status::Complete)); // id1 unsigned 7
    assert_eq!(is.feed(&[], &mut b), Ok(Status::Complete)); // clean boundary => Ok

    assert_eq!(a.events.len(), 1);
    assert_eq!(b.events.len(), 1);
}

/// Decoding a large blob through the fast path borrows straight from the input.
#[test]
fn large_blob_single_call() {
    // build [id7 blob, 1000 bytes] via the encoder-free route: header + word + data
    let data: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
    let mut bytes = vec![0x3Au8]; // (7<<3)|2 = fixlen, id 7
                                  // word = (1000<<3)|3
    let mut word: u64 = (1000 << 3) | 3;
    loop {
        let mut b = (word as u8) & 0x7F;
        word >>= 7;
        if word != 0 {
            b |= 0x80;
        }
        bytes.push(b);
        if word == 0 {
            break;
        }
    }
    bytes.extend_from_slice(&data);

    #[derive(Default)]
    struct Cap {
        calls: usize,
        got: Vec<u8>,
    }
    impl Visitor for Cap {
        fn blob(&mut self, _id: Id, _total: usize, _offset: usize, chunk: &[u8]) {
            self.calls += 1;
            self.got.extend_from_slice(chunk);
        }
    }
    let mut v = Cap::default();
    assert_eq!(decode(&bytes, &mut v), Ok(Status::Complete));
    assert_eq!(v.calls, 1);
    assert_eq!(v.got, data);
}

/// An omitted sequence is **silent**: it produces no `sequence_begin`, no
/// `sequence_end` and no children, on both decode paths. MESSAGE_SPEC §2 drops a
/// sequence-typed field whose value equals its declared default, and an
/// all-default message is the empty byte string — zero callbacks of any kind.
///
/// This pins the contract the decoder docs now state, and the reason they state
/// it: a consumer cannot hook "prepare my destination" onto a callback that an
/// absent field never fires. §5.1 puts that duty *before* the decode instead —
/// "initialise every destination slot to its element default" — which the second
/// half checks by replaying the exact failure with a reused destination and then
/// with a default-initialised one.
#[test]
fn an_omitted_sequence_fires_no_callbacks_and_absence_must_be_prepared_for() {
    use common::Event;
    use sofab::{OStream, Unsigned};

    // Message A: array field id 4 with two framed elements (element frames are
    // kept — presence carries a dynamic array's length, §5.1).
    let mut buf = [0u8; 64];
    let n = {
        let mut os = OStream::new(&mut buf);
        os.write_sequence_begin_lazy(4).unwrap();
        os.write_sequence_begin_lazy(0).unwrap();
        os.write_unsigned(0, 10).unwrap();
        os.write_sequence_end_keep().unwrap();
        os.write_sequence_begin_lazy(1).unwrap();
        os.write_unsigned(0, 11).unwrap();
        os.write_sequence_end_keep().unwrap();
        os.write_sequence_end().unwrap();
        os.bytes_used()
    };
    let a = buf[..n].to_vec();
    assert_eq!(
        a,
        [0x26, 0x06, 0x00, 0x0A, 0x07, 0x0E, 0x00, 0x0B, 0x07, 0x07]
    );

    // Message B: the same field all-default. §2 omits it, so B is zero bytes.
    let mut buf = [0u8; 64];
    let n = {
        let mut os = OStream::new(&mut buf);
        os.write_sequence_begin_lazy(4).unwrap();
        os.write_sequence_end().unwrap();
        os.bytes_used()
    };
    let b = buf[..n].to_vec();
    assert!(
        b.is_empty(),
        "an all-default sequence field must emit nothing"
    );

    // Not one callback for B, on either path.
    for path in ["decode", "feed"] {
        let mut rec = common::Recorder::new();
        if path == "decode" {
            assert_eq!(decode(&b, &mut rec), Ok(Status::Complete));
        } else {
            assert_eq!(IStream::new().feed(&b, &mut rec), Ok(Status::Complete));
        }
        assert!(
            rec.events.is_empty(),
            "[{path}] omitted sequence called back"
        );
    }

    // A, by contrast, delivers the wrapper and both element frames.
    let mut rec = common::Recorder::new();
    assert_eq!(decode(&a, &mut rec), Ok(Status::Complete));
    assert_eq!(
        rec.events,
        [
            Event::SequenceBegin(4),
            Event::SequenceBegin(0),
            Event::Unsigned(0, 10),
            Event::SequenceEnd,
            Event::SequenceBegin(1),
            Event::Unsigned(0, 11),
            Event::SequenceEnd,
            Event::SequenceEnd,
        ]
    );

    // The consequence, and the documented remedy. `Dest` does the tempting thing
    // and resets from `sequence_begin`; because B never calls it, a reused
    // destination keeps A's elements.
    #[derive(Default)]
    struct Dest {
        elems: Vec<Unsigned>,
    }
    impl Visitor for Dest {
        fn sequence_begin(&mut self, id: Id) {
            if id == 4 {
                self.elems.clear();
            }
        }
        fn unsigned(&mut self, _id: Id, v: Unsigned) {
            self.elems.push(v);
        }
    }

    let mut reused = Dest::default();
    assert_eq!(decode(&a, &mut reused), Ok(Status::Complete));
    assert_eq!(reused.elems, [10, 11]);
    assert_eq!(decode(&b, &mut reused), Ok(Status::Complete));
    assert_eq!(
        reused.elems,
        [10, 11],
        "if this ever clears itself, the callback-absence contract changed"
    );

    // §5.1 done right: initialise the destination before applying the message.
    let mut fresh = Dest::default();
    assert_eq!(decode(&b, &mut fresh), Ok(Status::Complete));
    assert!(
        fresh.elems.is_empty(),
        "absent must reconstruct to the default"
    );
}
