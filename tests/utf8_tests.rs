//! Strict UTF-8 conformance for `string` fields (issue #85, MESSAGE_SPEC §8,
//! CORELIB_PLAN §6.4).
//!
//! Rust's `String`/`&str` is a **Unicode string type**, so it is *always
//! strict*: `SOFAB_STRICT_UTF8` is a no-op for this port (always ON) and there
//! is no primitive to expose. The division of responsibility per §6.4 is:
//!
//! * **Encode** is strict *by construction* — [`OStream::write_str`] takes
//!   `&str`, which the Rust type system already guarantees is valid UTF-8, so a
//!   `string` field can never carry invalid bytes. There is nothing to check and
//!   no way to construct a counter-example.
//! * **Decode** — the corelib delivers a `string` field's *raw bytes* to the
//!   [`Visitor::string`] callback and never builds a `String` itself. Strictness
//!   is enforced by **generated code**, which materializes the field with
//!   `core::str::from_utf8` (an `Err` becomes the sticky `inv` flag →
//!   `Error::InvalidMsg`, the `INVALID` decode outcome). This subsumes
//!   generator #80 and makes std and no_std agree.
//!
//! These tests therefore exercise the *materialization* boundary: each shared
//! `invalid_utf8` vector decodes through the corelib into raw bytes, and a
//! `from_utf8` pass over those bytes — exactly what generated code emits — must
//! reject it. The corelib frame itself stays structurally valid; the corelib
//! decode path needs no UTF-8 change.

mod common;

use common::{Event, Recorder};
use serde_json::Value;
use sofab::{decode, Error, IStream, OStream};

/// The shared vectors, embedded from the verbatim asset copy.
const VECTORS_JSON: &str = include_str!("../assets/test_vectors.json");

fn hex_to_bytes(hex: &str) -> Vec<u8> {
    assert!(hex.len() % 2 == 0, "odd hex length");
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex byte"))
        .collect()
}

/// The shared `invalid_utf8` negative vectors (tracks corelib-c-cpp#97).
fn invalid_utf8_vectors() -> Vec<Value> {
    let doc: Value = serde_json::from_str(VECTORS_JSON).expect("parse test_vectors.json");
    doc["invalid_utf8"]
        .as_array()
        .expect("invalid_utf8 array")
        .clone()
}

/// Decode `bytes` through the corelib and materialize every `string` field with
/// `core::str::from_utf8`, exactly as generated Rust code does. Returns `Err`
/// with the *decode outcome* when either the frame is malformed (corelib error)
/// or a `string` payload is not valid UTF-8 (`Error::InvalidMsg`, the `INVALID`
/// outcome the generated `inv`-flag path reports).
fn decode_and_materialize(bytes: &[u8]) -> Result<Vec<Event>, Error> {
    let mut rec = Recorder::new();
    decode(bytes, &mut rec)?; // structural frame validity is the corelib's job
    for e in &rec.events {
        if let Event::Str(_, buf) = e {
            // Generated code: `core::str::from_utf8(buf).map_err(|_| inv)?`.
            core::str::from_utf8(buf).map_err(|_| Error::InvalidMsg)?;
        }
    }
    Ok(rec.events)
}

/// Same, but fed one byte at a time to prove chunk boundaries never change the
/// outcome (§6.4 cross-chunk semantics): validity is a property of the complete
/// payload, materialized once the whole field is assembled.
fn decode_and_materialize_chunked(bytes: &[u8]) -> Result<Vec<Event>, Error> {
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    for &b in bytes {
        match is.feed(&[b], &mut rec) {
            Ok(()) | Err(Error::Incomplete) => {}
            Err(e) => return Err(e),
        }
    }
    is.feed(&[], &mut rec)?; // clean boundary or Incomplete
    for e in &rec.events {
        if let Event::Str(_, buf) = e {
            core::str::from_utf8(buf).map_err(|_| Error::InvalidMsg)?;
        }
    }
    Ok(rec.events)
}

#[test]
fn invalid_utf8_group_present() {
    let vs = invalid_utf8_vectors();
    assert!(
        vs.len() >= 8,
        "expected the shared invalid_utf8 negative vectors (saw {})",
        vs.len()
    );
    for v in &vs {
        // Contract of the shared negative vectors (corelib-c-cpp#97).
        assert_eq!(v["group"], "invalid/utf8");
        assert_eq!(v["decode_outcome"], "invalid");
        assert_eq!(v["encode_outcome"], "invalid_argument");
    }
}

#[test]
fn invalid_utf8_vectors_decode_to_invalid() {
    // Every shared invalid_utf8 vector, decoded through the corelib and
    // materialized with from_utf8 (as generated code does), must yield INVALID.
    for v in invalid_utf8_vectors() {
        let name = v["name"].as_str().unwrap();
        let bytes = hex_to_bytes(v["serialized_hex"].as_str().unwrap());

        assert_eq!(
            decode_and_materialize(&bytes),
            Err(Error::InvalidMsg),
            "[{name}] expected INVALID from from_utf8 materialization",
        );
        // Chunk boundaries must not change the outcome (§6.4).
        assert_eq!(
            decode_and_materialize_chunked(&bytes),
            Err(Error::InvalidMsg),
            "[{name}] chunked: expected INVALID",
        );
    }
}

#[test]
fn corelib_frame_itself_stays_valid() {
    // The corelib does NOT enforce UTF-8: for these structurally well-formed
    // frames its own decode succeeds and hands the raw bytes to the visitor —
    // strictness is the generated code's from_utf8, not the corelib's. This
    // pins the division of responsibility (§6.4): decode side is corelib-untouched.
    for v in invalid_utf8_vectors() {
        let name = v["name"].as_str().unwrap();
        let bytes = hex_to_bytes(v["serialized_hex"].as_str().unwrap());
        let mut rec = Recorder::new();
        assert!(
            decode(&bytes, &mut rec).is_ok(),
            "[{name}] corelib frame should be structurally valid",
        );
        // The raw bytes delivered to the visitor are exactly the invalid form.
        let want = hex_to_bytes(v["string_hex"].as_str().unwrap());
        match rec.events.as_slice() {
            [Event::Str(0, got)] => assert_eq!(*got, want, "[{name}] raw bytes"),
            other => panic!("[{name}] unexpected events {other:?}"),
        }
    }
}

#[test]
fn from_utf8_rejects_each_invalid_form() {
    // The generated-code check itself: from_utf8 rejects every invalid form —
    // overlong encodings (incl. the C0 80 "Modified UTF-8" NUL), lone
    // surrogates, out-of-range code points, bare continuation / lone 0xFF, and
    // sequences truncated at end-of-payload.
    for v in invalid_utf8_vectors() {
        let name = v["name"].as_str().unwrap();
        let raw = hex_to_bytes(v["string_hex"].as_str().unwrap());
        assert!(
            core::str::from_utf8(&raw).is_err(),
            "[{name}] from_utf8 must reject",
        );
    }
}

#[test]
fn embedded_nul_roundtrips() {
    // U+0000 is valid UTF-8; a `string` carrying an embedded NUL must round-trip
    // byte-exact — never rejected, never truncated (§8, §6.4). Contrast with the
    // overlong C0 80 form, which is still rejected.
    let original = "a\u{0}b\u{0}"; // interior + trailing NUL
    let mut buf = [0u8; 32];
    let used = {
        let mut os = OStream::new(&mut buf);
        os.write_str(9, original).unwrap();
        os.bytes_used()
    };

    let events = decode_and_materialize(&buf[..used]).expect("valid UTF-8 with NUL");
    match events.as_slice() {
        [Event::Str(9, bytes)] => {
            assert_eq!(bytes.as_slice(), original.as_bytes());
            assert_eq!(core::str::from_utf8(bytes).unwrap(), original);
        }
        other => panic!("unexpected events {other:?}"),
    }

    // The overlong NUL (C0 80) is NOT the same thing and stays rejected.
    assert!(core::str::from_utf8(&hex_to_bytes("c080")).is_err());
}
