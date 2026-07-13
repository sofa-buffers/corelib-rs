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
use sofab::{decode, Error, IStream, Id, Visitor};

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
        decode(&bytes, &mut fast).unwrap_or_else(|e| panic!("[{name}] decode failed: {e}"));

        let mut streamed = Recorder::new();
        IStream::new().feed(&bytes, &mut streamed).unwrap();

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
    decode(&bytes, &mut v).unwrap();
    assert_eq!(v.calls, 1, "string delivered in exactly one call");
    assert!(v.ok, "whole string delivered at offset 0");
}

/// A message cut off mid-field is **incomplete**, not malformed (MESSAGE_SPEC
/// §7). The one-shot decoder surfaces `Incomplete` — distinct from `InvalidMsg` —
/// and feeding the same prefix to a streaming decoder yields the same outcome
/// (the caller simply feeds more).
#[test]
fn truncated_input_is_incomplete_not_invalid() {
    fn dec(bytes: &[u8]) -> Result<(), Error> {
        decode(bytes, &mut Recorder::new())
    }
    // header (id0, unsigned) present, value varint missing.
    assert_eq!(dec(&[0x00]), Err(Error::Incomplete));
    // string header says 5 bytes, only 2 follow.
    assert_eq!(dec(&[0x02, 0x2A, 0x41, 0x42]), Err(Error::Incomplete));
    // sequence opened, never closed.
    assert_eq!(dec(&[0x0E, 0x00, 0x2A]), Err(Error::Incomplete));

    // The streaming decoder reports the same outcome on the bare prefix — it is
    // not accepted (Ok) and not rejected (InvalidMsg): it waits for more.
    let mut sink = Recorder::new();
    assert_eq!(
        IStream::new().feed(&[0x00], &mut sink),
        Err(Error::Incomplete)
    );
}

/// The three decode outcomes (MESSAGE_SPEC §7) are distinct: a lone dangling
/// continuation byte is Incomplete, an over-long (>64-bit) varint is InvalidMsg,
/// and a whole message is Complete (`Ok`).
#[test]
fn three_outcomes_are_distinct() {
    fn dec(bytes: &[u8]) -> Result<(), Error> {
        decode(bytes, &mut Recorder::new())
    }
    // A lone 0x80: a varint header with the continuation bit set and no
    // terminating byte — ends mid-field → INCOMPLETE.
    assert_eq!(dec(&[0x80]), Err(Error::Incomplete));

    // 0x00 header (id0, unsigned) then 11 continuation bytes: the value varint
    // exceeds 64 bits → malformed regardless of what follows → INVALID.
    assert_eq!(
        dec(&[0x00, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80]),
        Err(Error::InvalidMsg)
    );

    // id0 unsigned = 42, ending exactly at a field boundary → COMPLETE.
    assert_eq!(dec(&[0x00, 0x2A]), Ok(()));
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

    // And distinct from every other decode outcome, too.
    for other in [
        Error::Argument,
        Error::Usage,
        Error::BufferFull,
        Error::InvalidMsg,
        Error::Incomplete,
    ] {
        assert_ne!(Error::LimitExceeded, other);
    }

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
        Error::Usage,
        Error::BufferFull,
        Error::InvalidMsg,
        Error::Incomplete,
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
    is.feed(&[0x00, 0x2A], &mut a).unwrap(); // id0 unsigned 42
    is.finish().unwrap();

    is.reset();
    let mut b = Recorder::new();
    is.feed(&[0x08, 0x07], &mut b).unwrap(); // id1 unsigned 7
    is.finish().unwrap();

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
    decode(&bytes, &mut v).unwrap();
    assert_eq!(v.calls, 1);
    assert_eq!(v.got, data);
}
