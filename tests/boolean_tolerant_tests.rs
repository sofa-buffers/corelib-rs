//! The shared `boolean_tolerant` block: **canonical on encode, tolerant on
//! decode** (CORELIB_PLAN §4.4).
//!
//! §4.4 is normative and says both halves in one breath: an encoder **MUST**
//! write `true` as `1`, and a decoder **MUST** read *every value other than `0`*
//! as `true` — such a value is **not** `INVALID` (§5.2), it is normalized away,
//! and a re-encode emits `1`. A boolean is not bound the way an `enum` or a
//! `bitfield` is (MESSAGE_SPEC §1): those carry the width their declaration
//! implies and a value outside it *is* `INVALID`; a boolean carries no width
//! bound at all.
//!
//! A boolean has no wire type of its own. It rides the unsigned varint
//! (`0b000`), and a boolean array rides the unsigned varint array (`0b011`), so
//! every byte string in this block is ordinary, well-formed wire. What is under
//! test is only how the boolean surface *interprets* those bytes and what it
//! *emits* afterwards.
//!
//! # Why the block is hand-authored and not a vector
//!
//! The positive `vectors` array cannot reach this half of §4.4: its bytes are
//! produced by replaying `fields` ops through a conforming encoder, and a
//! conforming encoder never emits a non-canonical boolean. Bytes carrying `2`,
//! `256` or `2^64-1` at a boolean position only ever arrive from *someone
//! else's* encoder — hence a separate top-level block.
//!
//! # Three defects, three different assertions
//!
//! | defect | caught by |
//! |---|---|
//! | answers `INVALID` for `256`, treating a boolean as a 1-byte type | the outcome check |
//! | masks the accumulated varint to the destination width before the zero test, so `256` becomes **false** | the value check — the outcome is `complete` and looks perfect |
//! | stores the raw `2` without normalizing | the **re-encode** check — the outcome passes, and `2` is true under every truthiness test in every language |
//!
//! A runner that asserts only the outcome certifies a decoder that violates §4.4
//! in two of the three ways; adding a *truthy* value check still certifies the
//! third. Only outcome + exact stored representation + re-encoded bytes closes
//! all three. This is the pair of defects sofa-buffers/corelib-c-cpp#172 and
//! sofa-buffers/generator#581 were opened about.
//!
//! # The two surfaces in this port
//!
//! The corelib is schema-agnostic, so it has no boolean *reader*: a boolean
//! field arrives as [`Visitor::unsigned`] carrying the wire value whole, and the
//! `!= 0` test that turns it into a `bool` is what generated code does with it.
//! [`BooleanField`] below is that code, reduced to the one decision — it is the
//! boolean read surface this block is about, and it writes into a real `bool`
//! destination, because "normalized away" is a statement about what ends up
//! *stored*. Reading the field with a plain unsigned destination and asserting
//! `!= 0` would test the unsigned path instead and hide both the truncation and
//! the missing normalization.
//!
//! What the corelib owes that consumer is the value **unnarrowed**: this build's
//! [`sofab::Unsigned`] is 64-bit, so `256` and `2^64-1` must reach the callback
//! as themselves rather than as a masked remnant, and the message must not be
//! rejected on the way.
//!
//! The write half is the corelib's own: [`OStream::write_boolean`] for a scalar
//! and [`OStream::write_array_unsigned`] for an array (this port has no boolean
//! array writer — §4.7 makes the element width an API concern that never reaches
//! the wire, so the normalized `0`/`1` go out at any width).
//!
//! # Reading the destination back as bytes
//!
//! In Rust a `bool` object may only ever hold the representations for `false`
//! and `true`; an object holding `2` has no value at all and reading it is
//! undefined behaviour, so comparing it against `true` could not tell you
//! whether anything was normalized. The destination is therefore read back
//! through a **byte view** and each byte compared against `0`/`1`, exactly as the
//! C reference does (it decodes into a `bool` array, copies the bytes into a
//! `uint8_t` array and compares those). See [`representation`].
//!
//! The destination is also **poisoned** before every feed — with the logical
//! complement of what the case expects — so that a decoder which never writes it
//! at all cannot pass `boolean_tolerant_zero` against a `false`-initialized
//! buffer.
//!
//! # Gating: `requires` means REJECT here, not skip
//!
//! This block narrows the corpus rule, and the narrowing is normative for it: an
//! unsatisfied tag means the message must be **rejected**, not skipped. §4.4
//! lifts the width bound the *type* carries, never the one a particular *build*
//! has — §6.2.2 lists "scalar value width 32-bit" as a permitted profile
//! variation, and §6.2 spells out the consequence: a varint that does not fit the
//! built width is `INVALID` (§5.2.2). Skipping such a case would assert nothing
//! in exactly the build most likely to truncate.
//!
//! This port has **no Cargo feature flags**: every wire type is compiled in and
//! the scalar value width is always 64-bit (`Cargo.toml`), so its capability set
//! is complete, every tag is satisfied and [`reject`] is unreachable here. The
//! gate is implemented anyway, so that a profile added later does not need the
//! runner rewritten — and never as an unconditional skip of the tagged cases.

use serde_json::Value;
use sofab::{ArrayKind, Error, IStream, Id, OStream, Status, Unsigned, Visitor};

/// The shared vectors, embedded from the verbatim asset copy.
const VECTORS_JSON: &str = include_str!("../assets/test_vectors.json");

/// The number of cases this port knows the block to carry. A **floor**, never an
/// equality: the block may grow upstream, and [`every_boolean_tolerant_case_conforms`]
/// reports the count it actually ran.
const CASE_FLOOR: usize = 8;

// --- the three-valued outcome ------------------------------------------------

/// What a feed sequence answered, in the shared file's spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Complete,
    Incomplete,
    Invalid,
    LimitExceeded,
}

impl Outcome {
    /// The outcome this port's `Result<Status>` carries (§5.2.1: `INCOMPLETE` is
    /// not an error and rides the success arm).
    fn of(result: Result<Status, Error>) -> Self {
        match result {
            Ok(Status::Complete) => Outcome::Complete,
            Ok(Status::Incomplete) => Outcome::Incomplete,
            Err(Error::InvalidMsg) => Outcome::Invalid,
            Err(Error::LimitExceeded) => Outcome::LimitExceeded,
            Err(other) => panic!("decode reported {other:?}, which is not a decode outcome"),
        }
    }

    /// Parse an `expect.outcome` string. Every case in this block states
    /// `complete` today — a *tolerated* value is not a *rejected* one — but the
    /// key is read rather than assumed, so a case added later cannot be silently
    /// mis-run.
    fn named(name: &str) -> Self {
        match name {
            "complete" => Outcome::Complete,
            "incomplete" => Outcome::Incomplete,
            "invalid" => Outcome::Invalid,
            "limit_exceeded" => Outcome::LimitExceeded,
            other => panic!(
                "unknown `expect.outcome` `{other}` in the boolean_tolerant block; \
                 decide what this port must answer for it (test_vectors_README.md)",
            ),
        }
    }
}

// --- capability gating -------------------------------------------------------

/// Whether this build satisfies one `requires` tag.
///
/// Every tag holds here: the port compiles every wire type in and its scalar
/// value width is always 64-bit. An unrecognized tag is *ignored* — it
/// contributes nothing to the needed set and the case runs positively — which is
/// the forward-compatibility rule the reference runner follows, so that all
/// eleven ports agree the day the corpus adds a tag.
fn capability_supported(tag: &str) -> bool {
    match tag {
        // The two tags this block uses, plus the neighbouring ones the corpus
        // defines — all satisfied here, and named so the answer is on record.
        "array" | "int64" | "int32" | "fixlen" | "sequence" | "fp32" | "fp64" => true,
        // A tag this port has not ruled on contributes nothing to the needed
        // set, so the case runs positively rather than being rejected on a name
        // nobody here understands.
        _ => true,
    }
}

/// The `requires` tags of one case (empty when the key is absent).
fn requires(case: &Value) -> Vec<&str> {
    case.get("requires")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

// --- the boolean read surface ------------------------------------------------

/// The consumer above the corelib: generated code's job, reduced to the one
/// decision §4.4 is about — *every* value other than `0` is `true`.
///
/// It records what it saw rather than asserting inside the callback: an
/// assertion that panics out of a `feed` would leave the decoder in a state that
/// masks the very failure it reports, so everything is judged after the feed
/// returns.
struct BooleanField {
    /// The field id the case's bytes carry, read from the case.
    id: Id,
    /// The boolean destination, pre-poisoned by the caller.
    dest: Vec<bool>,
    /// How many elements have been stored into `dest`.
    stored: usize,
    /// Elements delivered past the end of `dest` — a decoder handing over more
    /// values than the wire declares.
    overflowed: usize,
    /// The raw wire values, kept for the failure message (a truncating decoder
    /// should say what it actually handed over).
    raw: Vec<Unsigned>,
    /// `array_begin`'s kind and count, when the field was an array.
    announced: Option<(ArrayKind, usize)>,
    /// Callbacks seen for some *other* id — none of these cases put one on the
    /// wire, so any is a decoding fault.
    foreign: usize,
}

impl BooleanField {
    /// A visitor over a destination of `slots` booleans, each poisoned with the
    /// complement of the value the case expects there (§8.4: a decoder that
    /// never writes the destination must not pass by accident).
    fn poisoned(id: Id, expected: &[bool]) -> Self {
        Self {
            id,
            dest: expected.iter().map(|b| !b).collect(),
            stored: 0,
            overflowed: 0,
            raw: Vec::new(),
            announced: None,
            foreign: 0,
        }
    }
}

impl Visitor for BooleanField {
    fn unsigned(&mut self, id: Id, value: Unsigned) {
        if id != self.id {
            self.foreign += 1;
            return;
        }
        self.raw.push(value);
        match self.dest.get_mut(self.stored) {
            // The boolean read surface, in full: the wire value is *tested*
            // against zero, never masked into the destination first.
            Some(slot) => {
                *slot = value != 0;
                self.stored += 1;
            }
            None => self.overflowed += 1,
        }
    }

    fn array_begin(&mut self, id: Id, kind: ArrayKind, count: usize) {
        if id != self.id {
            self.foreign += 1;
            return;
        }
        self.announced = Some((kind, count));
    }

    fn signed(&mut self, _id: Id, _value: sofab::Signed) {
        self.foreign += 1;
    }
    fn fp32(&mut self, _id: Id, _value: f32) {
        self.foreign += 1;
    }
    fn fp64(&mut self, _id: Id, _value: f64) {
        self.foreign += 1;
    }
    fn string(&mut self, _id: Id, _total: usize, _offset: usize, _chunk: &[u8]) {
        self.foreign += 1;
    }
    fn blob(&mut self, _id: Id, _total: usize, _offset: usize, _chunk: &[u8]) {
        self.foreign += 1;
    }
    fn sequence_begin(&mut self, _id: Id) {
        self.foreign += 1;
    }
    fn sequence_end(&mut self) {
        self.foreign += 1;
    }
}

/// The destination's **object representation**, byte for byte.
///
/// A `bool` that holds anything but `0` or `1` has no value in Rust, so reading
/// it *as a `bool`* — comparing it, or casting it to an integer — cannot expose
/// a decoder that stored `2`: the read is already undefined and the optimizer may
/// assume it never happens. The bytes underneath are the evidence, and this is
/// how the C reference states the same check.
fn representation(dest: &[bool]) -> Vec<u8> {
    // SAFETY: `bool` has size 1 and alignment 1, so `dest`'s storage is exactly
    // `dest.len()` initialized bytes, readable as `u8` (which has no invalid
    // representation). The slice is read-only and borrowed for this call only.
    let bytes = unsafe { core::slice::from_raw_parts(dest.as_ptr().cast::<u8>(), dest.len()) };
    bytes.to_vec()
}

// --- the write surface -------------------------------------------------------

/// Re-encode **what the decoder produced** at field `id`, through this port's
/// boolean write surface, and return the bytes.
///
/// Scalar (one value) goes through [`OStream::write_boolean`]; an array goes
/// through [`OStream::write_array_unsigned`] at `u8` element width, the width
/// never reaching the wire (§4.7). This is where "canonical on encode" becomes
/// observable: a decoder that stored the raw `2` re-encodes as `2`.
fn reencode(id: Id, values: &[bool], name: &str) -> Vec<u8> {
    // Generous room: a header varint, a count varint and one byte per element.
    let mut buf = vec![0u8; 16 + values.len()];
    let used = {
        let mut os = OStream::new(&mut buf);
        let wrote = if values.len() == 1 {
            os.write_boolean(id, values[0])
        } else {
            let elements: Vec<u8> = values.iter().map(|&b| u8::from(b)).collect();
            os.write_array_unsigned(id, &elements)
        };
        assert!(
            wrote.is_ok(),
            "[{name}] the encoder refused to write the decoded value back: {wrote:?}",
        );
        // No flush sink is installed, so the bytes are already in `buf` and
        // `flush` is the finish step: it reports what was pending and leaves the
        // buffer intact. Comparing a half-emitted buffer is a false green for
        // cases this short, so the step is taken rather than assumed.
        let flushed = os.flush();
        assert_eq!(
            flushed,
            Ok(os.bytes_used()),
            "[{name}] finishing the encoder did not report the bytes written",
        );
        os.bytes_used()
    };
    buf[..used].to_vec()
}

// --- reading the block -------------------------------------------------------

fn boolean_tolerant() -> Vec<Value> {
    let doc: Value = serde_json::from_str(VECTORS_JSON).expect("parse test_vectors.json");
    doc["boolean_tolerant"]
        .as_array()
        .expect(
            "the shared file carries a `boolean_tolerant` block (CORELIB_PLAN \
             §4.4); refresh assets/test_vectors.json from corelib-c-cpp@main — a \
             runner that finds no block iterates nothing and passes",
        )
        .clone()
}

fn hex_to_bytes(hex: &str) -> Vec<u8> {
    assert!(hex.len() % 2 == 0, "odd-length hex string `{hex}`");
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex byte"))
        .collect()
}

fn as_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The booleans a case expects, in wire order.
fn expected_values(case: &Value) -> Vec<bool> {
    case["expect"]["values"]
        .as_array()
        .expect("expect.values")
        .iter()
        .map(|v| v.as_bool().expect("expect.values holds JSON booleans"))
        .collect()
}

// --- one case, both halves ---------------------------------------------------

/// The positive path: decode into a poisoned boolean destination, then re-encode
/// what the decoder produced. Returns the number of checks performed.
///
/// `chunk_size` of `None` feeds the whole message in one call; `Some(n)` feeds it
/// `n` bytes at a time, which is what puts a ten-byte varint across feed
/// boundaries (cases 6 and 8).
fn decode_and_reencode(case: &Value, chunk_size: Option<usize>) -> usize {
    let name = case["name"].as_str().expect("name");
    let id = case["id"].as_u64().expect("the case states its field `id`") as Id;
    let expected = expected_values(case);
    let bytes = hex_to_bytes(case["serialized_hex"].as_str().expect("serialized_hex"));
    let outcome = Outcome::named(case["expect"]["outcome"].as_str().expect("expect.outcome"));

    // (A) decode — a fresh stream and a fresh, poisoned destination per case, so
    //     neither a previous verdict nor a previous value can reach this one.
    let mut field = BooleanField::poisoned(id, &expected);
    let mut stream = IStream::new();
    let mut got = Outcome::Complete;
    match chunk_size {
        None => got = Outcome::of(stream.feed(&bytes, &mut field)),
        Some(n) => {
            for chunk in bytes.chunks(n) {
                got = Outcome::of(stream.feed(chunk, &mut field));
            }
        }
    }

    let how = match chunk_size {
        None => String::from("whole"),
        Some(n) => format!("{n} byte(s) at a time"),
    };
    assert_eq!(
        got, outcome,
        "[{name}] fed {how}: outcome mismatch for {} — §4.4 gives a boolean no \
         width bound, so a non-canonical value is tolerated, never rejected",
        case["serialized_hex"],
    );
    assert_eq!(
        field.foreign, 0,
        "[{name}] delivered a field this case has none of"
    );
    assert_eq!(
        field.overflowed,
        0,
        "[{name}] delivered {} element(s) past the {} the wire declares",
        field.overflowed,
        expected.len(),
    );
    assert_eq!(
        field.stored,
        expected.len(),
        "[{name}] fed {how}: the decode delivered {} of {} element(s) (raw values \
         seen: {:?})",
        field.stored,
        expected.len(),
        field.raw,
    );
    if expected.len() > 1 {
        // The element count the wire carries, cross-checked against the block.
        assert_eq!(
            field.announced,
            Some((ArrayKind::Unsigned, expected.len())),
            "[{name}] the array was announced as {:?}, not {} unsigned elements",
            field.announced,
            expected.len(),
        );
    }

    // The stored *representation*, not a truthiness test: `2` is true under
    // every truthiness test there is, and a `256` masked to the destination
    // width is `false` while the outcome stays `complete`.
    let want: Vec<u8> = expected.iter().map(|&b| u8::from(b)).collect();
    assert_eq!(
        representation(&field.dest),
        want,
        "[{name}] fed {how}: the destination holds {:?} where {expected:?} is \
         required (raw wire values: {:?})",
        representation(&field.dest),
        field.raw,
    );

    // (B) re-encode the destination — never `expect.values` from the JSON, which
    //     would match `reencoded_hex` trivially and leave the decode unverified.
    let produced = reencode(id, &field.dest, name);
    let want_hex = case["expect"]["reencoded_hex"]
        .as_str()
        .expect("expect.reencoded_hex");
    assert_eq!(
        as_hex(&produced),
        want_hex,
        "[{name}] fed {how}: the re-encode emitted {} where {want_hex} is required \
         — §4.4 makes `1` the only encoding of true",
        as_hex(&produced),
    );
    assert_eq!(
        produced.len(),
        want_hex.len() / 2,
        "[{name}] the re-encode is {} bytes, not {}",
        produced.len(),
        want_hex.len() / 2,
    );

    2
}

/// The reject path: the build cannot represent what the case carries, so the
/// message is `INVALID` (§5.2.2 — a width overflow, never the `limit_exceeded`
/// tier §6.2.1 reserves for policy) and the verdict is terminal.
///
/// Unreachable in this port — its capability set is complete — and kept so that a
/// reduced profile added later needs no new code here.
fn reject(case: &Value) -> usize {
    let name = case["name"].as_str().expect("name");
    let bytes = hex_to_bytes(case["serialized_hex"].as_str().expect("serialized_hex"));
    let mut sink = BooleanField::poisoned(Id::MAX, &[]);
    let mut stream = IStream::new();

    assert_eq!(
        Outcome::of(stream.feed(&bytes, &mut sink)),
        Outcome::Invalid,
        "[{name}] requires a capability this build lacks, so the message is not \
         representable and must be rejected — reading it by truncation is the \
         corruption this block exists to catch",
    );
    assert_eq!(
        Outcome::of(stream.feed(&[0x00], &mut sink)),
        Outcome::Invalid,
        "[{name}] the rejection was lifted by a further byte; a verdict that more \
         bytes can change is `INCOMPLETE`, not `INVALID` (§5.2.1)",
    );
    1
}

// --- the block is well formed ------------------------------------------------

#[test]
fn the_block_is_present_and_well_formed() {
    let cases = boolean_tolerant();
    assert!(
        cases.len() >= CASE_FLOOR,
        "the boolean_tolerant block carries {} cases, fewer than the {CASE_FLOOR} \
         this port knows it to have — the shared copy is stale or truncated",
        cases.len(),
    );

    for case in &cases {
        let name = case["name"].as_str().expect("every case is named");
        for key in ["group", "description", "id", "serialized_hex"] {
            assert!(!case[key].is_null(), "[{name}] the case has no `{key}`");
        }
        assert_eq!(
            case["group"], "boolean/tolerant",
            "[{name}] sits outside the block's group",
        );
        // The block is decode-then-re-encode only: no `fields` op list to replay
        // and no sparse column, because no conforming encoder produces these
        // bytes in the first place.
        assert!(
            case["fields"].is_null() && case["serialized_sparse"].is_null(),
            "[{name}] carries an encoder-side column; these bytes are hand-authored \
             precisely because a conforming encoder cannot emit them",
        );
        assert!(
            !expected_values(case).is_empty(),
            "[{name}] expects no value at all",
        );
        assert!(
            case["expect"]["reencoded_hex"].is_string(),
            "[{name}] states no `reencoded_hex`; without it the normalization half \
             of §4.4 is unasserted",
        );
    }

    println!("boolean_tolerant: {} cases found", cases.len());
}

/// The guard against comparing the re-encode with the input: for most of these
/// cases the two **differ**, and that difference is the test (§4.4's
/// normalization). A runner that asserted `reencoded == serialized` — or
/// weakened the comparison to a length check, `0002` and `0001` being the same
/// length — would pass the canonical cases and prove nothing.
#[test]
fn the_block_carries_cases_whose_re_encode_differs() {
    let cases = boolean_tolerant();
    let normalizing = cases
        .iter()
        .filter(|c| c["serialized_hex"] != c["expect"]["reencoded_hex"])
        .count();
    let same_length = cases
        .iter()
        .filter(|c| {
            c["serialized_hex"] != c["expect"]["reencoded_hex"]
                && c["serialized_hex"].as_str().map(str::len)
                    == c["expect"]["reencoded_hex"].as_str().map(str::len)
        })
        .count();

    assert!(
        normalizing > 0,
        "no case re-encodes to different bytes than it was fed; the block would \
         then say nothing about normalization",
    );
    assert!(
        same_length > 0,
        "every normalizing case changes the byte count, so a length-only \
         comparison would still pass — the block has lost its `0002` -> `0001` \
         shape",
    );
    println!(
        "boolean_tolerant: {normalizing} of {} cases re-encode to different bytes \
         ({same_length} of them at an identical length)",
        cases.len(),
    );
}

// --- the block itself --------------------------------------------------------

#[test]
fn every_boolean_tolerant_case_conforms() {
    let cases = boolean_tolerant();
    let mut decoded = 0;
    let mut rejected = 0;
    let mut checks = 0;

    for case in &cases {
        let tags = requires(case);
        if tags.iter().all(|t| capability_supported(t)) {
            decoded += 1;
            checks += decode_and_reencode(case, None);
        } else {
            // Not a skip: an unsatisfied tag makes the message unrepresentable
            // for this build, and unrepresentable means rejected (§7 of the
            // block's spec, CORELIB_PLAN §6.2/§6.2.2).
            rejected += 1;
            checks += reject(case);
        }
    }

    println!(
        "boolean_tolerant: {} cases found, {decoded} decoded, {rejected} rejected \
         (unrepresentable here), {checks} checks",
        cases.len(),
    );
    assert_eq!(
        decoded + rejected,
        cases.len(),
        "every case is either decoded or rejected; none is skipped",
    );
    assert!(
        decoded > 0,
        "no boolean_tolerant case ran — an empty or absent block is a failure, \
         not a pass",
    );
}

/// The same cases fed one byte at a time. Cases 6 and 8 carry ten-byte varints,
/// so this is what drives the value accumulator across feed boundaries — a
/// per-chunk accumulator that masks or resets between feeds reads `2^64-1` as
/// something else entirely, and the whole-message feed above would never see it.
#[test]
fn a_tolerated_boolean_survives_a_chunked_feed() {
    let cases = boolean_tolerant();
    let mut decoded = 0;
    let mut checks = 0;

    for case in &cases {
        if !requires(case).iter().all(|t| capability_supported(t)) {
            continue;
        }
        decoded += 1;
        checks += decode_and_reencode(case, Some(1));
    }

    println!("boolean_tolerant chunked: {decoded} cases fed one byte at a time, {checks} checks");
    assert!(decoded > 0, "no boolean_tolerant case ran chunked");
}
