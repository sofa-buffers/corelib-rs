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
//!    recover every other field, whole and chunked, and end on a clean message
//!    boundary.
//!
//! Roundtrip (encode → decode) falls out of running (1) and (3) on every vector.
//!
//! This build has every wire type and the 64-bit value width compiled in, so all
//! shared vectors are representable and run (a vector's `requires` tags are kept
//! only as a sanity check that the file carries them).
//!
//! # The skip matrix
//!
//! The shared file carries a skip matrix (group `skip/matrix`) plus the axes
//! beside it (group `skip`): every vector there is a chain of
//! `[read P] [skipped S] [read unsigned anchor]` rows covering all 100 ordered
//! pairs of the ten skippable constructs, and the anchor is the detector — a
//! walk that consumes one byte too few or too many resumes inside the next
//! field and its value comparison fails. Scenario 5 runs for **every** vector
//! carrying `skip_ids`, whole and one byte at a time; the chunked variant is
//! where a resync bug that a single-buffer feed papers over shows up.
//!
//! Nothing here is bounded by a fixed size: `skip_ids`, field ids, element
//! counts and payload lengths are all read straight out of the file into `Vec`s,
//! and every encode buffer is sized from the vector's own ground truth. The C
//! harness's fixed `MAXSKIP` silently *truncated* an over-long `skip_ids` list —
//! the surplus ids were read instead of skipped and the vector still passed,
//! testing less than it claimed. [`the_loader_carries_the_large_cases_whole`]
//! pins the sizes the matrix needs so that failure mode cannot come back
//! quietly.
//!
//! Both loops print what they ran — vectors, `requires`-gated vectors, and
//! individual checks. `cargo test` captures stdout for passing tests, so CI runs
//! this target once more with `--nocapture` to put those counts in the log.
//!
//! # Not covered here
//!
//! The file's `sequence_growth` block (CORELIB_PLAN §7.2 item 8) is read by
//! nothing in this repo yet; the loader ignores unknown top-level blocks, so it
//! costs nothing to carry. `invalid_utf8` is consumed by `tests/utf8_tests.rs`.

mod common;

use common::{Event, Recorder};
use serde_json::Value;
use sofab::ArrayKind;
use sofab::{Error, FlushTake, IStream, Id, OStream, Signed, Unsigned, Visitor, ID_MAX};

/// The shared vectors, embedded from the verbatim asset copy.
///
/// **Which column this repo asserts:** `serialized` — the primitive-layer ground
/// truth, every sequence framed. That is the only form a corelib can produce or
/// consume: it has no message layer, so it never sees a field's declared default
/// and cannot decide that a sequence is all-default.
///
/// The file's sibling `serialized_sparse` column (present on every vector here,
/// read by nothing here) is the **message-layer** form of MESSAGE_SPEC §2, where
/// an all-default sequence-typed field is omitted. It is exercised by the
/// generator's conformance drivers (`tests/conformance/<lang>/` in
/// sofa-buffers/generator), which own the schema and thus the defaults. The
/// corelib primitive that makes that form reachable — dropping a contentless
/// frame — is tested directly in `tests/ostream_tests.rs`
/// ("lazy sequence framing"), not through this file.
///
/// **What the shared set does not cover** (cross-repo, not specific to this
/// port): every `array/*` vector has *leaf* elements — strings — so no vector
/// puts a **sequence** at element position. Each column is therefore reproduced
/// with one closer used uniformly: `serialized` with `end_keep` everywhere (what
/// [`write_fields`] below does), `serialized_sparse` with `end` everywhere. No
/// vector, in either column, forces the per-position choice §5.1 actually
/// requires, so none of them would catch `end` used where `end_keep` is
/// mandatory — the one confusion that corrupts a decoded *value* (an array's
/// length) rather than costing bytes. Until the shared set grows such a vector,
/// that case is pinned by this repo's own tests:
/// `ostream_tests::end_keep_frames_a_contentless_sequence` at the byte level and
/// `roundtrip_tests::an_all_default_array_element_keeps_the_arrays_length` at
/// the decoded-length level.
const VECTORS_JSON: &str = include_str!("../assets/test_vectors.json");

// --- requires / capability gating -------------------------------------------

/// The `requires` tags for a vector (empty if the key is absent).
fn parse_requires(v: &Value) -> Vec<&str> {
    v.get("requires")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// Whether this build supports every capability a vector requires. This build
/// has every wire type and the 64-bit value width compiled in, so every vector
/// is supported; `parse_requires` is still exercised by the presence check.
fn vector_supported(_requires: &[&str]) -> bool {
    true
}

// --- helpers ----------------------------------------------------------------

/// A finite float as a JSON number, or `+/-infinity` as the strings `inf`/`-inf`.
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

fn to_unsigned(v: u64) -> Unsigned {
    v
}
fn to_signed(v: i64) -> Signed {
    v
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
fn array_kind(element_type: &str) -> ArrayKind {
    match element_type {
        "u8" | "u16" | "u32" | "u64" => ArrayKind::Unsigned,
        "i8" | "i16" | "i32" | "i64" => ArrayKind::Signed,
        "fp32" => ArrayKind::Fp32,
        "fp64" => ArrayKind::Fp64,
        other => panic!("unknown element_type {other:?}"),
    }
}

// --- encode -----------------------------------------------------------------

/// Write a vector's `fields[]` into any stream (buffered or flushing).
fn write_fields<'a, F: FlushTake<'a>>(os: &mut OStream<'a, F>, fields: &[Value]) {
    // A vector's `serialized` form is the primitive-layer ground truth and always
    // carries the frame, so every sequence closes with `end_keep`: identical bytes
    // once the sequence has content, and the empty-sequence vectors keep their
    // `begin`+`end` pair instead of vanishing.
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
            "fp32" => os.write_fp32(id, as_f64(&f["value"]) as f32).unwrap(),
            "fp64" => os.write_fp64(id, as_f64(&f["value"])).unwrap(),
            "string" => os.write_str(id, f["value"].as_str().unwrap()).unwrap(),
            "blob" => os
                .write_blob(id, &hex_to_bytes(f["value_hex"].as_str().unwrap()))
                .unwrap(),
            "array" => encode_array(os, id, f),
            "sequence_begin" => os.write_sequence_begin_lazy(id).unwrap(),
            "sequence_end" => os.write_sequence_end_keep().unwrap(),
            other => panic!("unsupported op {other:?} (vector should be `requires`-skipped)"),
        }
    }
}

fn encode_array<'a, F: FlushTake<'a>>(os: &mut OStream<'a, F>, id: Id, f: &Value) {
    let et = f["element_type"].as_str().unwrap();
    let vals = f["values"].as_array().unwrap();
    match et {
        "u8" => os.write_array_unsigned(id, &u_vec::<u8>(vals)).unwrap(),
        "u16" => os.write_array_unsigned(id, &u_vec::<u16>(vals)).unwrap(),
        "u32" => os.write_array_unsigned(id, &u_vec::<u32>(vals)).unwrap(),
        "u64" => os.write_array_unsigned(id, &u_vec::<u64>(vals)).unwrap(),
        "i8" => os.write_array_signed(id, &i_vec::<i8>(vals)).unwrap(),
        "i16" => os.write_array_signed(id, &i_vec::<i16>(vals)).unwrap(),
        "i32" => os.write_array_signed(id, &i_vec::<i32>(vals)).unwrap(),
        "i64" => os.write_array_signed(id, &i_vec::<i64>(vals)).unwrap(),
        "fp32" => {
            let a: Vec<f32> = vals.iter().map(|v| as_f64(v) as f32).collect();
            os.write_array_fp32(id, &a).unwrap();
        }
        "fp64" => {
            let a: Vec<f64> = vals.iter().map(as_f64).collect();
            os.write_array_fp64(id, &a).unwrap();
        }
        other => panic!("unsupported element_type {other:?}"),
    }
}

fn u_vec<T: TryFrom<u64>>(vals: &[Value]) -> Vec<T> {
    vals.iter()
        .map(|v| {
            T::try_from(v.as_u64().unwrap())
                .ok()
                .expect("u element fits")
        })
        .collect()
}

fn i_vec<T: TryFrom<i64>>(vals: &[Value]) -> Vec<T> {
    vals.iter()
        .map(|v| {
            T::try_from(v.as_i64().unwrap())
                .ok()
                .expect("i element fits")
        })
        .collect()
}

/// Encode `fields[]` into a single buffer of exactly `capacity` bytes,
/// returning the message bytes (without the reserved framing `offset`).
///
/// Callers pass the vector's own `serialized.length` rather than a fixed
/// scratch size: no vector is capped by a constant that a longer one would
/// silently outgrow (the 130-byte payloads and 130-element arrays of the skip
/// axes are the current maximum, and the file is free to grow past it), and an
/// exactly-sized buffer additionally asserts that the encoder reserves no
/// headroom — its unchecked fast paths must fall back to the byte-at-a-time
/// ones near the end of the buffer rather than report `BufferFull`.
fn encode_fields(fields: &[Value], offset: usize, capacity: usize) -> Vec<u8> {
    let mut buf = vec![0u8; capacity];
    let used = {
        let mut os = OStream::with_offset(&mut buf, offset).unwrap();
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
        let mut os =
            OStream::with_flush(&mut scratch, 0, |c: &[u8]| out.extend_from_slice(c)).unwrap();
        write_fields(&mut os, fields);
        os.flush().unwrap();
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
        "fp32" => ev.push(Event::Fp32(id, (as_f64(&f["value"]) as f32).to_bits())),
        "fp64" => ev.push(Event::Fp64(id, as_f64(&f["value"]).to_bits())),
        "string" => ev.push(Event::Str(
            id,
            f["value"].as_str().unwrap().as_bytes().to_vec(),
        )),
        "blob" => ev.push(Event::Blob(
            id,
            hex_to_bytes(f["value_hex"].as_str().unwrap()),
        )),
        "array" => expected_array_events(ev, id, f),
        "sequence_begin" => ev.push(Event::SequenceBegin(id)),
        "sequence_end" => ev.push(Event::SequenceEnd),
        other => panic!("unsupported op {other:?}"),
    }
}

fn expected_array_events(ev: &mut Vec<Event>, id: Id, f: &Value) {
    let et = f["element_type"].as_str().unwrap();
    let vals = f["values"].as_array().unwrap();
    ev.push(Event::ArrayBegin(id, array_kind(et), vals.len()));
    for v in vals {
        match et {
            "u8" | "u16" | "u32" => ev.push(Event::Unsigned(id, to_unsigned(v.as_u64().unwrap()))),
            "u64" => ev.push(Event::Unsigned(id, to_unsigned(v.as_u64().unwrap()))),
            "i8" | "i16" | "i32" => ev.push(Event::Signed(id, to_signed(v.as_i64().unwrap()))),
            "i64" => ev.push(Event::Signed(id, to_signed(v.as_i64().unwrap()))),
            "fp32" => ev.push(Event::Fp32(id, (as_f64(v) as f32).to_bits())),
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
    let mut depth: u32 = 0;
    // `Some(d)` while inside a skipped sub-tree opened at depth `d`.
    #[allow(unused_mut)]
    let mut skip_until: Option<u32> = None;
    for f in fields {
        let op = f["op"].as_str().unwrap();
        let id = f.get("id").and_then(Value::as_u64).unwrap_or(0) as Id;
        match op {
            "sequence_begin" => {
                if skip_until.is_none() && skip.contains(&id) {
                    skip_until = Some(depth);
                } else if skip_until.is_none() {
                    ev.push(Event::SequenceBegin(id));
                }
                depth += 1;
            }
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
        // Intermediate chunks legitimately end mid-field (Incomplete); only a
        // genuinely malformed byte is an error.
        match is.feed(&[b], &mut rec) {
            Ok(()) | Err(Error::Incomplete) => {}
            Err(e) => panic!("chunked decode failed: {e}"),
        }
    }
    is.feed(&[], &mut rec).expect("stream ended mid-message"); // clean boundary
    rec.events
}

/// A [`Visitor`] modelling a receiver that ignores a set of field `skip_ids`.
/// Scalars/arrays with a skipped id are dropped; a skipped `sequence_begin`
/// drops the whole nested sequence by tracking depth until the matching end.
struct SkipRecorder<'a> {
    skip: &'a [Id],
    events: Vec<Event>,
    pending: Option<(Id, bool, Vec<u8>)>,
    depth: u32,
    skip_until: Option<u32>,
}

impl<'a> SkipRecorder<'a> {
    fn new(skip: &'a [Id]) -> Self {
        SkipRecorder {
            skip,
            events: Vec::new(),
            pending: None,
            depth: 0,
            skip_until: None,
        }
    }

    fn skipping(&self) -> bool {
        self.skip_until.is_some()
    }

    fn drop_id(&self, id: Id) -> bool {
        self.skipping() || self.skip.contains(&id)
    }

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
    fn fp32(&mut self, id: Id, v: f32) {
        if !self.drop_id(id) {
            self.events.push(Event::Fp32(id, v.to_bits()));
        }
    }
    fn fp64(&mut self, id: Id, v: f64) {
        if !self.drop_id(id) {
            self.events.push(Event::Fp64(id, v.to_bits()));
        }
    }
    fn string(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {
        if !self.drop_id(id) {
            self.accumulate(id, false, total, offset, chunk);
        }
    }
    fn blob(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {
        if !self.drop_id(id) {
            self.accumulate(id, true, total, offset, chunk);
        }
    }
    fn array_begin(&mut self, id: Id, kind: ArrayKind, count: usize) {
        // Array elements arrive via the scalar/float callbacks with this id,
        // so a skipped id drops them too — only the header is handled here.
        if !self.drop_id(id) {
            self.events.push(Event::ArrayBegin(id, kind, count));
        }
    }
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
    // "the message is fully consumed": an empty feed is the end-of-message
    // probe, and errors if the walk left the decoder inside a field.
    is.feed(&[], &mut rec)
        .expect("skip decode ended mid-message");
    rec.events
}

fn decode_with_skip_chunked(bytes: &[u8], skip: &[Id]) -> Vec<Event> {
    let mut rec = SkipRecorder::new(skip);
    let mut is = IStream::new();
    for &b in bytes {
        match is.feed(&[b], &mut rec) {
            Ok(()) | Err(Error::Incomplete) => {}
            Err(e) => panic!("skip chunked decode failed: {e}"),
        }
    }
    is.feed(&[], &mut rec).expect("stream ended mid-message"); // clean boundary
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
fn trailing_default_array_elements_stay_on_the_wire() {
    // MESSAGE_SPEC §3: "A default-valued element stays on the wire, trailing ones
    // included — `M` is the length, so eliding one would shorten the array:
    // `[1, 2, 3, 0, 0]` and `[1, 2, 3]` are different values and encode
    // differently." The shared set pins that with a named vector; the loop below
    // runs it among the rest, but nothing named it, so a drift in the vector file
    // could drop the case without a test going red. This names it, and states the
    // rule the corelib must never grow a helper for: the encoder writes **every**
    // element it is handed, and a shortened one is a *different value*, not a
    // smaller encoding of the same one.
    let doc: Value = serde_json::from_str(VECTORS_JSON).unwrap();
    let vec = doc["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == "array_unsigned_trailing_defaults")
        .expect("shared vector `array_unsigned_trailing_defaults`");

    let fields = vec["fields"].as_array().unwrap();
    let expected_hex = vec["serialized"]["hex"].as_str().unwrap();
    assert_eq!(expected_hex, "030401020000", "vector file drifted");
    assert_eq!(
        fields[0]["values"].as_array().unwrap().len(),
        4,
        "the vector must carry the trailing defaults",
    );

    // Encode: all four elements, count M = 4.
    let bytes = encode_fields(fields, 0, expected_hex.len() / 2);
    assert_eq!(bytes_to_hex(&bytes), expected_hex);

    // Decode: the array reports length 4 and yields both trailing zeros.
    assert_eq!(
        decode(&bytes),
        vec![
            Event::ArrayBegin(0, ArrayKind::Unsigned, 4),
            Event::Unsigned(0, 1),
            Event::Unsigned(0, 2),
            Event::Unsigned(0, 0),
            Event::Unsigned(0, 0),
        ],
    );

    // The trimmed form is a different value, not a shorter spelling of this one:
    // different bytes, and a decoded length of 2.
    let mut buf = [0u8; 8];
    let trimmed = {
        let mut os = OStream::new(&mut buf);
        os.write_array_unsigned(0, &[1u32, 2]).unwrap();
        os.bytes_used()
    };
    assert_eq!(bytes_to_hex(&buf[..trimmed]), "03020102");
    assert_ne!(&buf[..trimmed], &bytes[..]);
}

#[test]
fn all_shared_vectors_conform() {
    let doc: Value = serde_json::from_str(VECTORS_JSON).unwrap();
    let vectors = doc["vectors"].as_array().unwrap();

    let mut ran = 0;
    let mut gated = 0;
    let mut checks = 0;
    for vec in vectors {
        if !vector_supported(&parse_requires(vec)) {
            gated += 1; // capability disabled in this build — skip per `requires`
            continue;
        }
        ran += 1;

        let name = vec["name"].as_str().unwrap();
        let offset = vec["offset"].as_u64().unwrap_or(0) as usize;
        let fields = vec["fields"].as_array().unwrap();
        let expected_hex = vec["serialized"]["hex"].as_str().unwrap();
        let expected_bytes = hex_to_bytes(expected_hex);

        // 1. Vector encode: replay fields, bytes must match the ground truth.
        let encoded = encode_fields(fields, offset, offset + expected_bytes.len());
        checks += 1;
        assert_eq!(
            encoded,
            expected_bytes,
            "[{name}] encode mismatch:\n  got {}\n  exp {expected_hex}",
            bytes_to_hex(&encoded),
        );

        // 2. Chunked encode: stream out through tiny flush buffers.
        for &bs in &[1usize, 3, 7] {
            checks += 1;
            assert_eq!(
                chunked_encode(fields, bs),
                expected_bytes,
                "[{name}] chunked-encode (buffer={bs}) mismatch",
            );
        }

        // 3. Vector decode: feed the official bytes, recovered fields must match.
        let want = expected_events(fields);
        checks += 1;
        assert_eq!(decode(&expected_bytes), want, "[{name}] decode mismatch");

        // 4. Chunked decode: one byte at a time yields identical events.
        checks += 1;
        assert_eq!(
            decode_one_byte_at_a_time(&expected_bytes),
            want,
            "[{name}] chunked decode mismatch",
        );
    }

    println!(
        "shared vectors: {ran} of {} vectors ran ({gated} gated out by `requires`), {checks} checks",
        vectors.len(),
    );
    assert!(ran > 0, "no vectors ran");
}

#[test]
fn skip_ids_vectors_conform() {
    // The spec's `skip_ids` scenario: a receiver that ignores those ids (a
    // skipped `sequence_begin` skips the whole sub-tree) must still recover every
    // other field, in order, without losing decoder sync — including over chunk
    // boundaries.
    //
    // It runs for **every** vector carrying `skip_ids`: the 36 `skip/matrix`
    // vectors (all 100 ordered pairs of skipped-after-read wire types), the 16
    // `skip` axis vectors (empty and two-varint-byte lengths, fp64 element
    // width, a three-byte id, the message edges, the last field inside a
    // sequence), and the older `sequence` / `composite` ones.
    let doc: Value = serde_json::from_str(VECTORS_JSON).unwrap();
    let vectors = doc["vectors"].as_array().unwrap();

    let mut carrying = 0;
    let mut ran = 0;
    let mut gated = 0;
    let mut checks = 0;
    // How many vectors of each group ran, so the log names the matrix itself
    // rather than one lump total.
    let mut by_group: Vec<(&str, usize)> = Vec::new();
    for vec in vectors {
        let skip_ids: Vec<Id> = match vec.get("skip_ids").and_then(Value::as_array) {
            Some(a) => a
                .iter()
                .map(|x| Id::try_from(x.as_u64().expect("skip id")).expect("skip id fits `Id`"))
                .collect(),
            None => continue, // fields are only ever skipped when `skip_ids` is present
        };
        carrying += 1;
        if !vector_supported(&parse_requires(vec)) {
            gated += 1;
            continue;
        }
        ran += 1;

        let name = vec["name"].as_str().unwrap();
        let group = vec["group"].as_str().unwrap_or("");
        match by_group.iter_mut().find(|(g, _)| *g == group) {
            Some((_, n)) => *n += 1,
            None => by_group.push((group, 1)),
        }
        let fields = vec["fields"].as_array().unwrap();
        let bytes = hex_to_bytes(vec["serialized"]["hex"].as_str().unwrap());

        let want = expected_events_with_skip(fields, &skip_ids);
        // Sanity: the skip set must actually drop something.
        checks += 1;
        assert!(
            want.len() < expected_events(fields).len(),
            "[{name}] skip_ids dropped nothing",
        );

        checks += 1;
        assert_eq!(
            decode_with_skip(&bytes, &skip_ids),
            want,
            "[{name}] skip decode mismatch",
        );
        checks += 1;
        assert_eq!(
            decode_with_skip_chunked(&bytes, &skip_ids),
            want,
            "[{name}] skip chunked decode mismatch",
        );
    }

    by_group.sort_unstable();
    let groups: Vec<String> = by_group.iter().map(|(g, n)| format!("{g} {n}")).collect();
    println!(
        "skip scenario: {ran} of {carrying} vectors carrying `skip_ids` ran \
         ({gated} gated out by `requires`), {checks} checks — {}",
        groups.join(", "),
    );

    // Every vector carrying `skip_ids` is accounted for: run, or gated out by
    // `requires` — never dropped on the floor.
    assert_eq!(ran + gated, carrying, "skip vectors went missing");
    // This build compiles in every wire type and the 64-bit value width, so
    // nothing is gated: the whole matrix runs here.
    assert_eq!(
        gated, 0,
        "no vector should be `requires`-gated in this build"
    );

    // Floors from corelib-c-cpp#160 / corelib-rs#98: the regenerated file has 58
    // vectors carrying `skip_ids`, 36 of them the matrix and 16 the axes beside
    // it. A hand-edited or half-copied asset shows up here rather than as a
    // suite that quietly checks less.
    let count = |g: &str| {
        by_group
            .iter()
            .find(|(n, _)| *n == g)
            .map_or(0, |(_, n)| *n)
    };
    assert!(
        count("skip/matrix") >= 36,
        "expected the full 36-vector skip matrix (saw {})",
        count("skip/matrix"),
    );
    assert!(
        count("skip") >= 16,
        "expected the 16 `skip` axis vectors (saw {})",
        count("skip"),
    );
    assert!(
        carrying >= 58,
        "expected 58 vectors carrying `skip_ids` (saw {carrying})",
    );
}

#[test]
fn the_loader_carries_the_large_cases_whole() {
    // Upstream's C harness had a fixed `MAXSKIP` that *truncated* an over-long
    // `skip_ids` list: the surplus ids were read instead of skipped, so the
    // vector still passed while testing less than it claimed (fixed in
    // corelib-c-cpp#160, which now refuses instead). Nothing on this side is
    // bounded by a constant — `skip_ids`, ids, element counts and payloads are
    // read into `Vec`s and `u64`s, and the encode buffer is sized from each
    // vector's own `serialized` length — so the guard this port needs is the
    // other one: assert the sizes the skip matrix actually depends on are
    // present *and* carried whole, so a cap introduced later fails loudly here.
    let doc: Value = serde_json::from_str(VECTORS_JSON).unwrap();

    let mut max_skip_ids = 0;
    let mut max_id = 0u64;
    let mut max_elements = 0;
    let mut max_payload = 0;
    let mut fp64_arrays = 0;
    for vec in doc["vectors"].as_array().unwrap() {
        // Only the skip scenario is at issue here: an id that is skipped, a
        // payload that is walked past, a count that is stepped over.
        let Some(skip_ids) = vec.get("skip_ids").and_then(Value::as_array) else {
            continue;
        };
        max_skip_ids = max_skip_ids.max(skip_ids.len());
        for f in vec["fields"].as_array().unwrap() {
            max_id = max_id.max(f.get("id").and_then(Value::as_u64).unwrap_or(0));
            match f["op"].as_str().unwrap() {
                "array" => {
                    let values = f["values"].as_array().unwrap();
                    max_elements = max_elements.max(values.len());
                    if f["element_type"] == "fp64" {
                        fp64_arrays += 1;
                    }
                }
                "string" => max_payload = max_payload.max(f["value"].as_str().unwrap().len()),
                "blob" => {
                    max_payload = max_payload.max(f["value_hex"].as_str().unwrap().len() / 2);
                }
                _ => {}
            }
        }
    }

    assert!(
        max_skip_ids >= 9,
        "`skip_ids` lists reach 9 entries; the loader saw at most {max_skip_ids} \
         — a cap is truncating them",
    );
    assert!(
        max_id >= 100_001,
        "the skip matrix needs three-byte header varints (id 100001); the \
         loader saw at most {max_id}",
    );
    assert!(
        max_id <= u64::from(ID_MAX),
        "a vector id exceeds ID_MAX ({ID_MAX}), which `Id` cannot represent",
    );
    assert!(
        max_elements >= 130,
        "arrays of 130 elements need a two-byte count varint; the loader saw at \
         most {max_elements}",
    );
    assert!(
        max_payload >= 130,
        "130-byte payloads need a two-byte `fixlen_word`; the loader saw at most \
         {max_payload} bytes",
    );
    assert!(
        fp64_arrays > 0,
        "no fp64 array among the skip vectors — the 8-byte element width is \
         then unpinned on the skip path",
    );
}

#[test]
fn unknown_top_level_blocks_are_tolerated() {
    // The asset is copied verbatim from corelib-c-cpp (CORELIB_PLAN §7.1/§8), so
    // it carries blocks this file does not drive scenarios from. The loader must
    // ignore them rather than fail or warn: adopting a regenerated file must
    // never mean editing it down to what is understood here.
    let doc: Value = serde_json::from_str(VECTORS_JSON).expect("parse test_vectors.json");
    assert!(
        doc["vectors"].as_array().is_some_and(|v| !v.is_empty()),
        "the unread top-level blocks must not affect `vectors`",
    );

    // Ignoring them is a decision, though, not an oversight — so each one is
    // named, and a block the shared file grows later fails here until this port
    // decides whether to run it:
    //   * `invalid_utf8`    — run, by `tests/utf8_tests.rs`.
    //   * `sequence_growth` — CORELIB_PLAN §7.2 item 8, not exercised by this
    //     port yet; corelib-rs#98 leaves it as follow-up work.
    let driven_here = ["format", "version", "description", "notes", "vectors"];
    let decided_elsewhere = ["invalid_utf8", "sequence_growth"];
    for key in doc.as_object().expect("top-level object").keys() {
        assert!(
            driven_here.contains(&key.as_str()) || decided_elsewhere.contains(&key.as_str()),
            "the shared file grew top-level block `{key}`; the loader ignores it, \
             but decide whether this port should run it (CORELIB_PLAN §7.2)",
        );
    }
}
