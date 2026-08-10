//! Strict UTF-8 conformance for `string` fields (issue #85, MESSAGE_SPEC §8,
//! CORELIB_PLAN §6.4).
//!
//! Rust's `String`/`&str` is a **Unicode string type**, so it is *always
//! strict*: `SOFAB_STRICT_UTF8` is a no-op for this port (always ON) and there
//! is no primitive to expose. The division of responsibility per §6.4 is:
//!
//! * **Encode** is strict *by construction* on the typed path —
//!   [`OStream::write_str`] takes `&str`, which the Rust type system already
//!   guarantees is valid UTF-8, so that path can never carry invalid bytes and
//!   pays no runtime check. The byte-level [`OStream::write_fixlen`] is public
//!   too, and it *can* be handed arbitrary bytes under the `Str` subtype: there
//!   the check is real, and refusing with `Error::Argument` is what makes the
//!   encode side symmetric with decode (the shared vectors'
//!   `"encode_outcome": "invalid_argument"`).
//! * **Decode** — the corelib delivers a `string` field's *raw bytes* to the
//!   [`Visitor::string`] callback and never builds a `String` itself. Strictness
//!   is enforced by **generated code**, which materializes the field with
//!   `core::str::from_utf8` (an `Err` becomes the sticky `inv` flag →
//!   `Error::InvalidMsg`, the `INVALID` decode outcome). This subsumes
//!   generator #80 and makes std and no_std agree.
//!
//! These tests therefore exercise both halves of each shared `invalid_utf8`
//! vector: the *materialization* boundary on decode — the vector decodes through
//! the corelib into raw bytes, and a `from_utf8` pass over those bytes, exactly
//! what generated code emits, must reject it — and the *encode* refusal, where
//! `write_fixlen(.., Str)` over the same bytes must return `Error::Argument`.
//! The corelib frame itself stays structurally valid; the corelib decode path
//! needs no UTF-8 change.

mod common;

use common::{Event, Recorder};
use serde_json::Value;
use sofab::{decode, Error, FixlenType, IStream, OStream};

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
fn write_fixlen_str_rejects_the_invalid_utf8_vectors() {
    // The *encode* half of each shared negative vector ("encode_outcome":
    // "invalid_argument"): encoding `string_hex` as a `string` must be refused.
    // `write_str` cannot express these bytes, but the byte-level `write_fixlen`
    // can — and it is public (§6.1), so it is the path that has to enforce §6.4's
    // encode-side rule.
    for v in invalid_utf8_vectors() {
        let name = v["name"].as_str().unwrap();
        let id = v["id"].as_u64().unwrap() as u32;
        let raw = hex_to_bytes(v["string_hex"].as_str().unwrap());

        let mut buf = [0u8; 64];
        let mut os = OStream::new(&mut buf);
        assert_eq!(
            os.write_fixlen(id, &raw, FixlenType::Str),
            Err(Error::Argument),
            "[{name}] encoding invalid UTF-8 as a `string` must be InvalidArgument",
        );
        // Refused means nothing reached the wire — not even the field header.
        assert_eq!(
            os.bytes_used(),
            0,
            "[{name}] refused write must emit nothing"
        );

        // The same bytes as a `blob` are perfectly legal: §6.4 constrains the
        // `string` subtype, not the opaque one.
        let mut blob_buf = [0u8; 64];
        let mut blob_os = OStream::new(&mut blob_buf);
        blob_os
            .write_fixlen(id, &raw, FixlenType::Blob)
            .unwrap_or_else(|e| panic!("[{name}] blob must accept the same bytes: {e:?}"));
        assert!(blob_os.bytes_used() > 0);
    }
}

#[test]
fn write_fixlen_str_accepts_valid_utf8() {
    // The check must not cost correctness on the valid side: `write_fixlen` with
    // `Str` produces exactly `write_str`'s bytes, embedded NUL and multi-byte
    // sequences included.
    for text in [
        "",
        "Hello Couch!",
        "a\u{0}b",
        "äöü€",
        "𝄞 g-clef",
        "\u{10FFFF}",
    ] {
        let mut a = [0u8; 64];
        let mut b = [0u8; 64];
        let used_a = {
            let mut os = OStream::new(&mut a);
            os.write_fixlen(3, text.as_bytes(), FixlenType::Str)
                .unwrap();
            os.bytes_used()
        };
        let used_b = {
            let mut os = OStream::new(&mut b);
            os.write_str(3, text).unwrap();
            os.bytes_used()
        };
        assert_eq!(a[..used_a], b[..used_b], "text {text:?}");
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
