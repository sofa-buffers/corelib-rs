//! Conformance against the **shared** cross-language test vectors.
//!
//! The architecture spec mandates that every `corelib-<lang>` consume
//! `assets/test_vectors.json` — copied verbatim from the documentation
//! repository — as the single source of truth, rather than a divergent
//! hand-maintained copy. This suite embeds that file at build time and, for
//! every vector, checks the spec's scenarios:
//!
//! 1. **encode** — replay `fields[]`, assert output matches `serialized.hex`.
//! 2. **chunked-encode** — re-encode through tiny 1/3/7-byte flush buffers
//!    (exercising [`OStream`]'s buffer-full → flush → resume path) and assert
//!    the streamed-out bytes still match `serialized.hex`.
//! 3. **decode** — feed the official hex, assert the recovered fields match.
//! 4. **chunked-decode** — feed the same bytes one byte at a time, assert identical.
//! 5. **skip** — for vectors carrying `skip_ids`, a receiver that ignores those
//!    ids (skipping a `sequence_begin` skips its whole sub-tree) must still
//!    recover every other field, whole and chunked.
//!
//! Roundtrip (encode → decode) falls out of running (1) and (3) on every vector.
//!
//! ## `requires`-aware feature gating
//!
//! Each vector may carry a top-level `requires` array naming the optional
//! capabilities it needs (`fixlen` / `array` / `sequence` / `fp64` / `int64`).
//! This suite honours it: built without a feature, it **skips** the vectors that
//! need it, so the same vector file runs against every build configuration
//! (`cargo test --test vectors_tests --no-default-features --features …`). The
//! `int64` tag maps to this crate's `value64` feature.

mod common;

use common::{Event, Recorder};
use serde_json::Value;
#[cfg(feature = "array")]
use sofab::ArrayKind;
use sofab::{Flush, IStream, Id, OStream, Signed, Unsigned, Visitor};

/// The shared vectors, embedded from the verbatim asset copy.
const VECTORS_JSON: &str = include_str!("../assets/test_vectors.json");

// --- requires / capability gating -------------------------------------------

/// The `requires` tags for a vector (empty if the key is absent).
fn parse_requires(v: &Value) -> Vec<&str> {
    v.get("requires")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// Whether this build supports every capability a vector requires. `int64` maps
/// to the `value64` feature; unknown tags are assumed supported.
fn vector_supported(requires: &[&str]) -> bool {
    requires.iter().all(|r| match *r {
        "fixlen" => cfg!(feature = "fixlen"),
        "array" => cfg!(feature = "array"),
        "sequence" => cfg!(feature = "sequence"),
        "fp64" => cfg!(feature = "fp64"),
        "int64" => cfg!(feature = "value64"),
        _ => true,
    })
}

// --- helpers ----------------------------------------------------------------

/// A finite float as a JSON number, or `+/-infinity` as the strings `inf`/`-inf`.
#[cfg(feature = "fixlen")]
fn as_f64(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().expect("float number"),
        Value::String(s) => match s.as_str() {
            "inf" => f64::INFINITY,
            "-inf" => f64::NEG_INFINITY,
            other => panic!("unexpected float string {other:?}"),
        },
        other => panic!("unexpected float JSON {other:?}"),
    }
}

// The scalar value type is `u64`/`i64` (default) or `u32`/`i32` (`value64` off);
// the cast is only a no-op in the former, so silence the lint for the latter.
#[allow(clippy::unnecessary_cast)]
fn to_unsigned(v: u64) -> Unsigned {
    v as Unsigned
}
#[allow(clippy::unnecessary_cast)]
fn to_signed(v: i64) -> Signed {
    v as Signed
}

fn hex_to_bytes(hex: &str) -> Vec<u8> {
    assert!(hex.len() % 2 == 0, "odd hex length");
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex byte"))
        .collect()
}

fn bytes_to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Map a vector's `element_type` string to the array kind the decoder reports.
#[cfg(feature = "array")]
fn array_kind(element_type: &str) -> ArrayKind {
    match element_type {
        "u8" | "u16" | "u32" | "u64" => ArrayKind::Unsigned,
        "i8" | "i16" | "i32" | "i64" => ArrayKind::Signed,
        #[cfg(feature = "fixlen")]
        "fp32" | "fp64" => ArrayKind::Fixlen,
        other => panic!("unknown element_type {other:?}"),
    }
}

// --- encode -----------------------------------------------------------------

/// Write a vector's `fields[]` into any stream (buffered or flushing). Feature-
/// gated per op; vectors needing a disabled op are filtered out by `requires`.
fn write_fields<F: Flush>(os: &mut OStream<F>, fields: &[Value]) {
    for f in fields {
        let op = f["op"].as_str().expect("op");
        let id = f.get("id").and_then(Value::as_u64).unwrap_or(0) as Id;
        match op {
            "unsigned" => os
                .write_unsigned(id, to_unsigned(f["value"].as_u64().unwrap()))
                .unwrap(),
            "signed" => os
                .write_signed(id, to_signed(f["value"].as_i64().unwrap()))
                .unwrap(),
            "boolean" => os.write_boolean(id, f["value"].as_bool().unwrap()).unwrap(),
            #[cfg(feature = "fixlen")]
            "fp32" => os.write_fp32(id, as_f64(&f["value"]) as f32).unwrap(),
            #[cfg(feature = "fp64")]
            "fp64" => os.write_fp64(id, as_f64(&f["value"])).unwrap(),
            #[cfg(feature = "fixlen")]
            "string" => os.write_str(id, f["value"].as_str().unwrap()).unwrap(),
            #[cfg(feature = "fixlen")]
            "blob" => os
                .write_blob(id, &hex_to_bytes(f["value_hex"].as_str().unwrap()))
                .unwrap(),
            #[cfg(feature = "array")]
            "array" => encode_array(os, id, f),
            #[cfg(feature = "sequence")]
            "sequence_begin" => os.write_sequence_begin(id).unwrap(),
            #[cfg(feature = "sequence")]
            "sequence_end" => os.write_sequence_end().unwrap(),
            other => panic!("unsupported op {other:?} (vector should be `requires`-skipped)"),
        }
    }
}

#[cfg(feature = "array")]
fn encode_array<F: Flush>(os: &mut OStream<F>, id: Id, f: &Value) {
    let et = f["element_type"].as_str().unwrap();
    let vals = f["values"].as_array().unwrap();
    match et {
        "u8" => os.write_array_unsigned(id, &u_vec::<u8>(vals)).unwrap(),
        "u16" => os.write_array_unsigned(id, &u_vec::<u16>(vals)).unwrap(),
        "u32" => os.write_array_unsigned(id, &u_vec::<u32>(vals)).unwrap(),
        #[cfg(feature = "value64")]
        "u64" => os.write_array_unsigned(id, &u_vec::<u64>(vals)).unwrap(),
        "i8" => os.write_array_signed(id, &i_vec::<i8>(vals)).unwrap(),
        "i16" => os.write_array_signed(id, &i_vec::<i16>(vals)).unwrap(),
        "i32" => os.write_array_signed(id, &i_vec::<i32>(vals)).unwrap(),
        #[cfg(feature = "value64")]
        "i64" => os.write_array_signed(id, &i_vec::<i64>(vals)).unwrap(),
        #[cfg(feature = "fixlen")]
        "fp32" => {
            let a: Vec<f32> = vals.iter().map(|v| as_f64(v) as f32).collect();
            os.write_array_fp32(id, &a).unwrap();
        }
        #[cfg(feature = "fp64")]
        "fp64" => {
            let a: Vec<f64> = vals.iter().map(as_f64).collect();
            os.write_array_fp64(id, &a).unwrap();
        }
        other => panic!("unsupported element_type {other:?}"),
    }
}

#[cfg(feature = "array")]
fn u_vec<T: TryFrom<u64>>(vals: &[Value]) -> Vec<T> {
    vals.iter()
        .map(|v| {
            T::try_from(v.as_u64().unwrap())
                .ok()
                .expect("u element fits")
        })
        .collect()
}

#[cfg(feature = "array")]
fn i_vec<T: TryFrom<i64>>(vals: &[Value]) -> Vec<T> {
    vals.iter()
        .map(|v| {
            T::try_from(v.as_i64().unwrap())
                .ok()
                .expect("i element fits")
        })
        .collect()
}

/// Encode `fields[]` into a single buffer, returning the message bytes (without
/// the reserved framing `offset`).
fn encode_fields(fields: &[Value], offset: usize) -> Vec<u8> {
    let mut buf = vec![0u8; 4096];
    let used = {
        let mut os = OStream::with_offset(&mut buf, offset);
        write_fields(&mut os, fields);
        os.bytes_used()
    };
    buf[offset..used].to_vec()
}

/// Encode `fields[]` through a tiny `buf_size`-byte buffer with a flush sink, so
/// the encoder repeatedly fills, flushes, and resumes. Returns the streamed-out
/// bytes (the message is independent of any reserved offset, so we use 0).
fn chunked_encode(fields: &[Value], buf_size: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut scratch = vec![0u8; buf_size];
    {
        let mut os = OStream::with_flush(&mut scratch, 0, |c: &[u8]| out.extend_from_slice(c));
        write_fields(&mut os, fields);
        os.flush();
    }
    out
}

// --- expected decode events -------------------------------------------------

/// The events a correct decoder must emit for one vector's `fields[]`.
fn expected_events(fields: &[Value]) -> Vec<Event> {
    let mut ev = Vec::new();
    for f in fields {
        push_field_events(&mut ev, f);
    }
    ev
}

/// Append the decoder events for a single `fields[]` entry.
fn push_field_events(ev: &mut Vec<Event>, f: &Value) {
    let op = f["op"].as_str().unwrap();
    let id = f.get("id").and_then(Value::as_u64).unwrap_or(0) as Id;
    match op {
        "unsigned" => ev.push(Event::Unsigned(
            id,
            to_unsigned(f["value"].as_u64().unwrap()),
        )),
        // booleans decode as plain unsigned 0/1.
        "boolean" => ev.push(Event::Unsigned(
            id,
            to_unsigned(f["value"].as_bool().unwrap() as u64),
        )),
        "signed" => ev.push(Event::Signed(id, to_signed(f["value"].as_i64().unwrap()))),
        #[cfg(feature = "fixlen")]
        "fp32" => ev.push(Event::Fp32(id, (as_f64(&f["value"]) as f32).to_bits())),
        #[cfg(feature = "fp64")]
        "fp64" => ev.push(Event::Fp64(id, as_f64(&f["value"]).to_bits())),
        #[cfg(feature = "fixlen")]
        "string" => ev.push(Event::Str(
            id,
            f["value"].as_str().unwrap().as_bytes().to_vec(),
        )),
        #[cfg(feature = "fixlen")]
        "blob" => ev.push(Event::Blob(
            id,
            hex_to_bytes(f["value_hex"].as_str().unwrap()),
        )),
        #[cfg(feature = "array")]
        "array" => expected_array_events(ev, id, f),
        #[cfg(feature = "sequence")]
        "sequence_begin" => ev.push(Event::SequenceBegin(id)),
        #[cfg(feature = "sequence")]
        "sequence_end" => ev.push(Event::SequenceEnd),
        other => panic!("unsupported op {other:?}"),
    }
}

#[cfg(feature = "array")]
fn expected_array_events(ev: &mut Vec<Event>, id: Id, f: &Value) {
    let et = f["element_type"].as_str().unwrap();
    let vals = f["values"].as_array().unwrap();
    ev.push(Event::ArrayBegin(id, array_kind(et), vals.len()));
    for v in vals {
        match et {
            "u8" | "u16" | "u32" => ev.push(Event::Unsigned(id, to_unsigned(v.as_u64().unwrap()))),
            #[cfg(feature = "value64")]
            "u64" => ev.push(Event::Unsigned(id, to_unsigned(v.as_u64().unwrap()))),
            "i8" | "i16" | "i32" => ev.push(Event::Signed(id, to_signed(v.as_i64().unwrap()))),
            #[cfg(feature = "value64")]
            "i64" => ev.push(Event::Signed(id, to_signed(v.as_i64().unwrap()))),
            #[cfg(feature = "fixlen")]
            "fp32" => ev.push(Event::Fp32(id, (as_f64(v) as f32).to_bits())),
            #[cfg(feature = "fp64")]
            "fp64" => ev.push(Event::Fp64(id, as_f64(v).to_bits())),
            other => panic!("unsupported element_type {other:?}"),
        }
    }
}

/// The events a receiver must observe for `fields[]` when it ignores `skip_ids`.
///
/// Scalars/arrays whose id is in `skip_ids` are dropped; a `sequence_begin`
/// whose id is in `skip_ids` drops the *entire* nested sequence (its begin,
/// everything inside, and the matching end), and decoding resumes after it.
fn expected_events_with_skip(fields: &[Value], skip: &[Id]) -> Vec<Event> {
    let mut ev = Vec::new();
    #[cfg(feature = "sequence")]
    let mut depth: u32 = 0;
    // `Some(d)` while inside a skipped sub-tree opened at depth `d`.
    #[allow(unused_mut)]
    let mut skip_until: Option<u32> = None;
    for f in fields {
        let op = f["op"].as_str().unwrap();
        let id = f.get("id").and_then(Value::as_u64).unwrap_or(0) as Id;
        match op {
            #[cfg(feature = "sequence")]
            "sequence_begin" => {
                if skip_until.is_none() && skip.contains(&id) {
                    skip_until = Some(depth);
                } else if skip_until.is_none() {
                    ev.push(Event::SequenceBegin(id));
                }
                depth += 1;
            }
            #[cfg(feature = "sequence")]
            "sequence_end" => {
                depth -= 1;
                match skip_until {
                    Some(d) if d == depth => skip_until = None,
                    Some(_) => {}
                    None => ev.push(Event::SequenceEnd),
                }
            }
            _ => {
                if skip_until.is_none() && !skip.contains(&id) {
                    push_field_events(&mut ev, f);
                }
            }
        }
    }
    ev
}

// --- decode -----------------------------------------------------------------

fn decode(bytes: &[u8]) -> Vec<Event> {
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    is.feed(bytes, &mut rec).expect("decode");
    rec.events
}

fn decode_one_byte_at_a_time(bytes: &[u8]) -> Vec<Event> {
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    for &b in bytes {
        is.feed(&[b], &mut rec).expect("chunked decode");
    }
    rec.events
}

/// A [`Visitor`] modelling a receiver that ignores a set of field `skip_ids`.
/// Scalars/arrays with a skipped id are dropped; a skipped `sequence_begin`
/// drops the whole nested sequence by tracking depth until the matching end.
struct SkipRecorder<'a> {
    skip: &'a [Id],
    events: Vec<Event>,
    #[cfg(feature = "fixlen")]
    pending: Option<(Id, bool, Vec<u8>)>,
    #[cfg(feature = "sequence")]
    depth: u32,
    #[cfg(feature = "sequence")]
    skip_until: Option<u32>,
}

impl<'a> SkipRecorder<'a> {
    fn new(skip: &'a [Id]) -> Self {
        SkipRecorder {
            skip,
            events: Vec::new(),
            #[cfg(feature = "fixlen")]
            pending: None,
            #[cfg(feature = "sequence")]
            depth: 0,
            #[cfg(feature = "sequence")]
            skip_until: None,
        }
    }

    fn skipping(&self) -> bool {
        #[cfg(feature = "sequence")]
        {
            self.skip_until.is_some()
        }
        #[cfg(not(feature = "sequence"))]
        {
            false
        }
    }

    fn drop_id(&self, id: Id) -> bool {
        self.skipping() || self.skip.contains(&id)
    }

    #[cfg(feature = "fixlen")]
    fn accumulate(&mut self, id: Id, is_blob: bool, total: usize, offset: usize, chunk: &[u8]) {
        if offset == 0 {
            self.pending = Some((id, is_blob, Vec::with_capacity(total)));
        }
        let done = {
            let p = self.pending.as_mut().expect("chunk without begin");
            p.2.extend_from_slice(chunk);
            p.2.len() == total
        };
        if done {
            let (i, b, buf) = self.pending.take().unwrap();
            self.events.push(if b {
                Event::Blob(i, buf)
            } else {
                Event::Str(i, buf)
            });
        }
    }
}

impl Visitor for SkipRecorder<'_> {
    fn unsigned(&mut self, id: Id, v: Unsigned) {
        if !self.drop_id(id) {
            self.events.push(Event::Unsigned(id, v));
        }
    }
    fn signed(&mut self, id: Id, v: Signed) {
        if !self.drop_id(id) {
            self.events.push(Event::Signed(id, v));
        }
    }
    #[cfg(feature = "fixlen")]
    fn fp32(&mut self, id: Id, v: f32) {
        if !self.drop_id(id) {
            self.events.push(Event::Fp32(id, v.to_bits()));
        }
    }
    #[cfg(feature = "fp64")]
    fn fp64(&mut self, id: Id, v: f64) {
        if !self.drop_id(id) {
            self.events.push(Event::Fp64(id, v.to_bits()));
        }
    }
    #[cfg(feature = "fixlen")]
    fn string(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {
        if !self.drop_id(id) {
            self.accumulate(id, false, total, offset, chunk);
        }
    }
    #[cfg(feature = "fixlen")]
    fn blob(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {
        if !self.drop_id(id) {
            self.accumulate(id, true, total, offset, chunk);
        }
    }
    #[cfg(feature = "array")]
    fn array_begin(&mut self, id: Id, kind: ArrayKind, count: usize) {
        // Array elements arrive via the scalar/float callbacks with this id,
        // so a skipped id drops them too — only the header is handled here.
        if !self.drop_id(id) {
            self.events.push(Event::ArrayBegin(id, kind, count));
        }
    }
    #[cfg(feature = "sequence")]
    fn sequence_begin(&mut self, id: Id) {
        if !self.skipping() {
            if self.skip.contains(&id) {
                self.skip_until = Some(self.depth);
            } else {
                self.events.push(Event::SequenceBegin(id));
            }
        }
        self.depth += 1;
    }
    #[cfg(feature = "sequence")]
    fn sequence_end(&mut self) {
        self.depth -= 1;
        match self.skip_until {
            Some(d) if d == self.depth => self.skip_until = None,
            Some(_) => {}
            None => self.events.push(Event::SequenceEnd),
        }
    }
}

fn decode_with_skip(bytes: &[u8], skip: &[Id]) -> Vec<Event> {
    let mut rec = SkipRecorder::new(skip);
    let mut is = IStream::new();
    is.feed(bytes, &mut rec).expect("skip decode");
    rec.events
}

fn decode_with_skip_chunked(bytes: &[u8], skip: &[Id]) -> Vec<Event> {
    let mut rec = SkipRecorder::new(skip);
    let mut is = IStream::new();
    for &b in bytes {
        is.feed(&[b], &mut rec).expect("skip chunked decode");
    }
    rec.events
}

// --- the suite --------------------------------------------------------------

#[test]
fn shared_vectors_present_and_parsed() {
    let doc: Value = serde_json::from_str(VECTORS_JSON).expect("parse test_vectors.json");
    assert_eq!(doc["format"], "sofabuffers-test-vectors");
    assert_eq!(doc["version"], 1);
    let vectors = doc["vectors"].as_array().expect("vectors array");
    assert!(!vectors.is_empty(), "expected at least one shared vector");
    assert!(
        vectors.iter().any(|v| v.get("requires").is_some()),
        "expected `requires` capability tags in the vector file",
    );
}

#[test]
fn all_shared_vectors_conform() {
    let doc: Value = serde_json::from_str(VECTORS_JSON).unwrap();
    let vectors = doc["vectors"].as_array().unwrap();

    let mut ran = 0;
    for vec in vectors {
        if !vector_supported(&parse_requires(vec)) {
            continue; // capability disabled in this build — skip per `requires`
        }
        ran += 1;

        let name = vec["name"].as_str().unwrap();
        let offset = vec["offset"].as_u64().unwrap_or(0) as usize;
        let fields = vec["fields"].as_array().unwrap();
        let expected_hex = vec["serialized"]["hex"].as_str().unwrap();
        let expected_bytes = hex_to_bytes(expected_hex);

        // 1. Vector encode: replay fields, bytes must match the ground truth.
        let encoded = encode_fields(fields, offset);
        assert_eq!(
            encoded,
            expected_bytes,
            "[{name}] encode mismatch:\n  got {}\n  exp {expected_hex}",
            bytes_to_hex(&encoded),
        );

        // 2. Chunked encode: stream out through tiny flush buffers.
        for &bs in &[1usize, 3, 7] {
            assert_eq!(
                chunked_encode(fields, bs),
                expected_bytes,
                "[{name}] chunked-encode (buffer={bs}) mismatch",
            );
        }

        // 3. Vector decode: feed the official bytes, recovered fields must match.
        let want = expected_events(fields);
        assert_eq!(decode(&expected_bytes), want, "[{name}] decode mismatch");

        // 4. Chunked decode: one byte at a time yields identical events.
        assert_eq!(
            decode_one_byte_at_a_time(&expected_bytes),
            want,
            "[{name}] chunked decode mismatch",
        );
    }

    assert!(ran > 0, "no vectors ran for this feature configuration");
}

#[test]
fn skip_ids_vectors_conform() {
    // The spec's `skip_ids` scenario: a receiver that ignores those ids (a
    // skipped `sequence_begin` skips the whole sub-tree) must still recover every
    // other field, in order, without losing decoder sync — including over chunk
    // boundaries.
    let doc: Value = serde_json::from_str(VECTORS_JSON).unwrap();
    let vectors = doc["vectors"].as_array().unwrap();

    let mut seen = 0;
    for vec in vectors {
        let skip_ids: Vec<Id> = match vec.get("skip_ids").and_then(Value::as_array) {
            Some(a) => a.iter().map(|x| x.as_u64().unwrap() as Id).collect(),
            None => continue,
        };
        if !vector_supported(&parse_requires(vec)) {
            continue;
        }
        seen += 1;

        let name = vec["name"].as_str().unwrap();
        let fields = vec["fields"].as_array().unwrap();
        let bytes = hex_to_bytes(vec["serialized"]["hex"].as_str().unwrap());

        let want = expected_events_with_skip(fields, &skip_ids);
        // Sanity: the skip set must actually drop something.
        assert!(
            want.len() < expected_events(fields).len(),
            "[{name}] skip_ids dropped nothing",
        );

        assert_eq!(
            decode_with_skip(&bytes, &skip_ids),
            want,
            "[{name}] skip decode mismatch",
        );
        assert_eq!(
            decode_with_skip_chunked(&bytes, &skip_ids),
            want,
            "[{name}] skip chunked decode mismatch",
        );
    }

    // Under the full build every shared skip vector is supported.
    #[cfg(all(
        feature = "fixlen",
        feature = "array",
        feature = "sequence",
        feature = "fp64",
        feature = "value64"
    ))]
    assert!(seen >= 8, "expected the shared skip vectors (saw {seen})");
    let _ = seen;
}
