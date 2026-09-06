//! The shared `header_limits` block: a ceiling answers **at the length or count
//! word** (CORELIB_PLAN §6.2.1, §6.3).
//!
//! The block — a fourth top-level block in `assets/test_vectors.json`, added by
//! sofa-buffers/corelib-c-cpp#163 — carries a class no earlier block could: bytes
//! that *declare* a length or a count and then **end**, with not one payload byte
//! behind them.
//!
//! ```text
//! 02 a2 06   then EOF
//! ^^ id 0, wire type 2 (fixlen)
//!    ^^^^^ length word (100 << 3) | 2  ->  a 100-byte string is declared
//!            ... and the message ends.
//! ```
//!
//! A conformant decoder answers at that word, before the payload is asked for, so
//! the answer is the ceiling's and it is **terminal**. `INCOMPLETE` is wrong here:
//! §5.2.1 defines it as the outcome more bytes *can* change, and after a ceiling
//! has fired nothing can — §5.2.4 has a streaming caller read it as "feed me the
//! next chunk", which would then be a false statement about the state.
//!
//! # Which ceiling speaks is the subject
//!
//! A case carries `schema` **or** `limits`, never both, and the two give opposite
//! answers on the same word:
//!
//! | the case states | the ceiling | a breach is |
//! |---|---|---|
//! | `"schema": { "maxlen": N }` | the schema bound | `invalid` (MESSAGE_SPEC §7.1) |
//! | `"limits": { "max_dyn_…": N }` | the receiver cap | `limit_exceeded` (§6.2.1) |
//!
//! `header_string_schema_bounded` and `header_string_over_cap` carry the
//! **identical bytes** and differ only in which ceiling the case configures. That
//! pair is what keeps the two categories apart, and
//! [`the_identical_bytes_pair_keeps_the_two_categories_apart`] names it.
//!
//! # How this port runs them
//!
//! The corelib is schema-agnostic and **enforces no receiver limit of its own** —
//! see [`sofab::Error::LimitExceeded`], whose values are configured in sofabgen
//! and carried by the generated decode visitor. What the corelib owes that
//! consumer is the *information and the ordering*: [`Visitor::fixlen_begin`] fires
//! once per scalar fixlen field and [`Visitor::array_begin`] once per array field,
//! after the bound-bearing word is read and validated and **before any payload
//! byte**. That is this port's enforcement point, and it is the guard these cases
//! exist to keep: without it the only event carrying the declared size is the
//! payload callback, which cannot fire for a message that ends at the word — and
//! the port then answers `INCOMPLETE`, which is what corelib-c-cpp#161 was opened
//! about.
//!
//! [`Consumer`] below is the code that sits above the corelib: it holds the case's
//! ceiling, judges the declared size at the hook, and makes the rejection terminal.
//! It is deliberately the smallest thing that can be called a receiver — the
//! subject under test is *when the corelib hands over the number*, not how a
//! generated struct stores it.
//!
//! # Gating: `requires` means SKIP here, for every tag
//!
//! Unlike a *vector*, where an unsatisfied wire-construct tag turns the vector into
//! a negative case, an unsatisfied tag in this block means the case does not run.
//! These cases already assert a rejection *with a specific category*, so a build
//! that cannot represent the construct would reject for an unrelated reason and
//! appear to pass while testing nothing.
//!
//! This build compiles every wire type and the 64-bit value width in, so the
//! wire-construct tags are all satisfied. `receiver_caps` is a **profile**
//! capability, and this port declares it: it ships `Error::LimitExceeded` as a
//! category distinct from `Error::InvalidMsg` (policy vs malformation, §6.3), and
//! it announces the bound-bearing word ahead of the payload so a cap can fire
//! there. A port that could not tell the two categories apart, or that only
//! learned the size from the payload callback, would have to skip those eight
//! cases rather than assert them.
//!
//! # The controls are not filler
//!
//! Every rejection is paired with the same shape at a size the ceiling **admits**,
//! which must still answer `incomplete`: a port that rejects every short read
//! passes all six rejection cases and is badly broken. Each control is also driven
//! one step further here — its declared payload is fed and the message must reach
//! `COMPLETE` — because that is precisely what `INCOMPLETE` claims and a latched
//! rejection cannot do.
//!
//! [`lifting_the_ceilings_falls_back_to_incomplete`] is the matching negative
//! control from the other side: with no ceiling configured, the rejections must
//! *stop* rejecting, which is what shows the verdicts come from the guard and not
//! from something incidental about the bytes.

use serde_json::Value;
use sofab::{ArrayKind, Error, FixlenType, IStream, Id, Status, Visitor};

/// The shared vectors, embedded from the verbatim asset copy.
const VECTORS_JSON: &str = include_str!("../assets/test_vectors.json");

/// This port's **format** ceiling on a declared fixlen length and on an array
/// count (`INT32_MAX`, §4.6/§4.7). It is the corelib's own and the one ceiling a
/// receiver cannot lift, so it is what a case is measured against in the
/// ceilings-lifted control below. The crate's `FIXLEN_MAX`/`ARRAY_MAX` are
/// internal, so the value is restated here rather than imported.
const FORMAT_CEILING: u64 = i32::MAX as u64;

// --- the three-valued outcome, plus the policy category ----------------------

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
    /// not an error and rides the success arm; `INVALID` and the terminal
    /// `LimitExceeded` ride the error channel §6.3 pairs them with).
    fn of(result: Result<Status, Error>) -> Self {
        match result {
            Ok(Status::Complete) => Outcome::Complete,
            Ok(Status::Incomplete) => Outcome::Incomplete,
            Err(Error::InvalidMsg) => Outcome::Invalid,
            Err(Error::LimitExceeded) => Outcome::LimitExceeded,
            Err(other) => panic!("decode reported {other:?}, which is not a decode outcome"),
        }
    }

    /// Parse an `expect.outcome` string.
    fn named(name: &str) -> Self {
        match name {
            "complete" => Outcome::Complete,
            "incomplete" => Outcome::Incomplete,
            "invalid" => Outcome::Invalid,
            "limit_exceeded" => Outcome::LimitExceeded,
            other => panic!(
                "unknown `expect.outcome` `{other}` in the header_limits block; \
                 decide what this port must answer for it (test_vectors_README.md)",
            ),
        }
    }

    fn is_rejection(self) -> bool {
        matches!(self, Outcome::Invalid | Outcome::LimitExceeded)
    }
}

// --- capability gating -------------------------------------------------------

/// Whether this port satisfies one `requires` tag. An unsatisfied tag means the
/// case is **skipped**, never rejected.
fn capability_supported(tag: &str) -> bool {
    match tag {
        // Wire constructs: this build has every wire type and the 64-bit value
        // width compiled in, so every construct in the block is representable.
        "fixlen" | "array" | "sequence" | "fp32" | "fp64" | "int32" | "int64" => true,
        // Profile capability, declared here — see the module docs.
        "receiver_caps" => true,
        other => panic!(
            "the shared file requires capability `{other}`, which this port has not \
             ruled on; decide whether it holds here before the case is run or \
             skipped (test_vectors_README.md, CORELIB_PLAN §7.2)",
        ),
    }
}

/// The `requires` tags of one case (empty when the key is absent).
fn requires(case: &Value) -> Vec<&str> {
    case.get("requires")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

// --- the ceilings one case configures ----------------------------------------

/// The ceiling a case asks the port to configure, bound to the field it applies
/// to. Exactly one of the four is set for a rejection case; all four are `None`
/// in the ceilings-lifted control.
#[derive(Debug, Clone, Copy, Default)]
struct Ceilings {
    field_id: Id,
    /// §6.2.1 receiver caps. A breach is `LimitExceeded`: policy, not malformation.
    max_dyn_string_len: Option<u64>,
    max_dyn_blob_len: Option<u64>,
    max_dyn_array_count: Option<u64>,
    /// A schema `maxlen`. A breach is `InvalidMsg`: MESSAGE_SPEC §7.1 says these
    /// bytes are not a legal value for this field, whatever the receiver's
    /// capacity.
    schema_maxlen: Option<u64>,
}

impl Ceilings {
    /// The ceilings the case states. `schema` and `limits` are mutually exclusive
    /// (§6.2.1 forbids applying a receiver cap to a field the schema bounds), and
    /// [`the_block_is_present_and_well_formed`] holds the file to that.
    fn of(case: &Value) -> Self {
        let limits = &case["limits"];
        Self {
            field_id: case["field_id"].as_u64().expect("field_id") as Id,
            max_dyn_string_len: limits["max_dyn_string_len"].as_u64(),
            max_dyn_blob_len: limits["max_dyn_blob_len"].as_u64(),
            max_dyn_array_count: limits["max_dyn_array_count"].as_u64(),
            schema_maxlen: case["schema"]["maxlen"].as_u64(),
        }
    }

    /// The same field with every ceiling removed — the negative control.
    fn lifted(field_id: Id) -> Self {
        Self {
            field_id,
            ..Self::default()
        }
    }
}

// --- the consumer above the corelib ------------------------------------------

/// The receiving half: generated code's job, reduced to the one decision this
/// block is about.
///
/// It judges the declared size **in the header hook**, which is where the corelib
/// hands it over — before a payload byte is asked for and therefore before a
/// message that ends at the word can be mistaken for a truncated one.
#[derive(Default)]
struct Receiver {
    ceilings: Ceilings,
    /// The verdict this receiver raised, if any.
    verdict: Option<Error>,
    /// Every callback the corelib made, header hooks included. A terminal
    /// rejection must stop this from growing.
    calls: usize,
}

impl Receiver {
    fn new(ceilings: Ceilings) -> Self {
        Self {
            ceilings,
            verdict: None,
            calls: 0,
        }
    }

    /// Measure a declared size against whichever ceiling this field carries.
    ///
    /// `cap` is the §6.2.1 receiver cap for the subtype that actually arrived —
    /// the corelib reports what is *on the wire*, and a receiver measures against
    /// the cap for that kind or not at all.
    fn judge(&mut self, id: Id, declared: u64, cap: Option<u64>) {
        if id != self.ceilings.field_id || self.verdict.is_some() {
            return;
        }
        // The schema bound first: a field the schema bounds carries no receiver
        // cap at all (§6.2.1), so the two can never both fire.
        if let Some(maxlen) = self.ceilings.schema_maxlen {
            if declared > maxlen {
                self.verdict = Some(Error::InvalidMsg);
            }
            return;
        }
        if let Some(cap) = cap {
            if declared > cap {
                self.verdict = Some(Error::LimitExceeded);
            }
        }
    }
}

impl Visitor for Receiver {
    fn fixlen_begin(&mut self, id: Id, subtype: FixlenType, total: usize) {
        self.calls += 1;
        // A float's width is fixed by its subtype and bounded by the format, so
        // no dynamic cap binds it; only the two payload-bearing subtypes are
        // measured, each against its own cap (§6.2.1 keeps string and blob apart
        // because a deployment may accept a megabyte of opaque bytes and no such
        // quantity of text).
        let cap = match subtype {
            FixlenType::Str => self.ceilings.max_dyn_string_len,
            FixlenType::Blob => self.ceilings.max_dyn_blob_len,
            FixlenType::Fp32 | FixlenType::Fp64 => return,
        };
        self.judge(id, total as u64, cap);
    }

    fn array_begin(&mut self, id: Id, _kind: ArrayKind, count: usize) {
        self.calls += 1;
        // A count ahead of its payload is bound exactly as a length is.
        let cap = self.ceilings.max_dyn_array_count;
        self.judge(id, count as u64, cap);
    }

    fn unsigned(&mut self, _id: Id, _value: sofab::Unsigned) {
        self.calls += 1;
    }
    fn signed(&mut self, _id: Id, _value: sofab::Signed) {
        self.calls += 1;
    }
    fn fp32(&mut self, _id: Id, _value: f32) {
        self.calls += 1;
    }
    fn fp64(&mut self, _id: Id, _value: f64) {
        self.calls += 1;
    }
    fn string(&mut self, _id: Id, _total: usize, _offset: usize, _chunk: &[u8]) {
        self.calls += 1;
    }
    fn blob(&mut self, _id: Id, _total: usize, _offset: usize, _chunk: &[u8]) {
        self.calls += 1;
    }
    fn sequence_begin(&mut self, _id: Id) {
        self.calls += 1;
    }
    fn sequence_end(&mut self) {
        self.calls += 1;
    }
}

/// A decoder plus its receiver: the pair a caller actually holds.
///
/// Its whole contribution over a bare [`IStream`] is the two things §6.3 asks of
/// a ceiling — the receiver's verdict *dominates* the corelib's own outcome for
/// the same bytes, and once raised it is **terminal**: a further feed re-raises
/// it and consumes nothing.
struct Consumer {
    stream: IStream,
    receiver: Receiver,
    /// The terminal verdict, once one has been reached.
    latched: Option<Error>,
    /// Bytes actually handed to the decoder. A re-raise must not move this.
    consumed: usize,
}

impl Consumer {
    fn new(ceilings: Ceilings) -> Self {
        Self {
            stream: IStream::new(),
            receiver: Receiver::new(ceilings),
            latched: None,
            consumed: 0,
        }
    }

    fn feed(&mut self, chunk: &[u8]) -> Result<Status, Error> {
        if let Some(terminal) = self.latched {
            // Terminal (§6.3): re-raise, and do not touch the bytes.
            return Err(terminal);
        }
        self.consumed += chunk.len();
        let outcome = self.stream.feed(chunk, &mut self.receiver);
        if let Some(raised) = self.receiver.verdict {
            // The ceiling fired inside the header hook. It was decided at the
            // word, so it overrides the `INCOMPLETE` the corelib reports for a
            // message whose payload never arrived.
            self.latched = Some(raised);
            return Err(raised);
        }
        if let Err(malformed) = outcome {
            self.latched = Some(malformed);
        }
        outcome
    }

    /// Feed a whole case: its `chunks` when it has them, otherwise `serialized`
    /// in one call. The answer is the first terminal verdict, or the outcome of
    /// the last chunk when there is none.
    fn feed_all(&mut self, chunks: &[Vec<u8>]) -> Outcome {
        let mut last = Outcome::Complete;
        for chunk in chunks {
            last = Outcome::of(self.feed(chunk));
            if last.is_rejection() {
                return last;
            }
        }
        last
    }
}

// --- reading the block -------------------------------------------------------

fn header_limits() -> Vec<Value> {
    let doc: Value = serde_json::from_str(VECTORS_JSON).expect("parse test_vectors.json");
    doc["header_limits"]
        .as_array()
        .expect(
            "the shared file carries a `header_limits` block \
             (corelib-c-cpp#163); refresh assets/test_vectors.json",
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

/// The bytes a case is fed: `chunks` when present, else `serialized` whole.
fn case_chunks(case: &Value) -> Vec<Vec<u8>> {
    match case["chunks"].as_array() {
        Some(chunks) => chunks
            .iter()
            .map(|c| hex_to_bytes(c.as_str().expect("chunk hex")))
            .collect(),
        None => vec![hex_to_bytes(
            case["serialized"].as_str().expect("serialized hex"),
        )],
    }
}

/// The payload that would finish the message a case's header declares — the
/// bytes that make `INCOMPLETE` mean what §5.2.1 says it means.
///
/// Only the shapes this block uses at an admitted size are built: a `string` /
/// `blob` header at id 0 (its declared bytes) and an unsigned varint array header
/// at id 0 (its declared elements, one byte each). `None` for anything else, and
/// the caller then skips this check rather than guessing.
fn completing_payload(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut pos = 0;
    let header = read_varint(bytes, &mut pos)?;
    if header >> 3 != 0 {
        return None; // not the id-0 field these cases put on the wire
    }
    let word = read_varint(bytes, &mut pos)?;
    match (header & 0x07) as u8 {
        // fixlen: the length word carries `(len << 3) | subtype`.
        0x2 => match (word & 0x07) as u8 {
            0x2 | 0x3 => Some(vec![b'x'; usize::try_from(word >> 3).ok()?]),
            _ => None,
        },
        // unsigned varint array: the count word, then one varint per element.
        0x3 => Some(vec![0x00; usize::try_from(word).ok()?]),
        _ => None,
    }
}

/// A minimal varint reader for [`completing_payload`], independent of the crate's.
fn read_varint(bytes: &[u8], pos: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    let mut shift = 0u32;
    loop {
        let byte = *bytes.get(*pos)?;
        *pos += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
}

// --- the block is well formed ------------------------------------------------

#[test]
fn the_block_is_present_and_well_formed() {
    let cases = header_limits();
    assert!(!cases.is_empty(), "the header_limits block is empty");

    let mut rejections = 0;
    let mut controls = 0;
    for case in &cases {
        let name = case["name"].as_str().expect("name");
        for key in ["group", "description", "field_id", "declared", "serialized"] {
            assert!(!case[key].is_null(), "[{name}] the case has no `{key}`");
        }
        let outcome = Outcome::named(case["expect"]["outcome"].as_str().expect("expect.outcome"));

        // A case states one ceiling or the other, never both: §6.2.1 forbids
        // applying a receiver cap to a field the schema already bounds, and the
        // two answer differently, so a case carrying both would have no defined
        // verdict at all.
        assert!(
            !(case["limits"].is_object() && case["schema"].is_object()),
            "[{name}] states both `limits` and `schema`; §6.2.1 forbids a receiver \
             cap on a schema-bounded field",
        );
        if outcome.is_rejection() {
            rejections += 1;
            assert!(
                case["limits"].is_object() || case["schema"].is_object(),
                "[{name}] expects a rejection but configures no ceiling to raise it",
            );
            assert_eq!(
                case["expect"]["terminal"],
                Value::Bool(true),
                "[{name}] a ceiling that fired at the word is terminal (§6.3)",
            );
        } else {
            controls += 1;
            assert!(
                case["expect"]["terminal"].is_null(),
                "[{name}] `incomplete` is precisely the state more bytes can lift, \
                 so it carries no `terminal`",
            );
        }
    }

    // "Every rejection is paired with its in-cap control … treat a missing
    // control as a bug in the block, not an omission." Checked per ceiling, so a
    // block that grew a rejection on a new ceiling and no control for it fails.
    for case in &cases {
        let outcome = Outcome::named(case["expect"]["outcome"].as_str().unwrap());
        if !outcome.is_rejection() {
            continue;
        }
        let name = case["name"].as_str().unwrap();
        let ceiling_keys = |c: &Value| -> Vec<String> {
            ["limits", "schema"]
                .iter()
                .filter_map(|k| c[*k].as_object().map(|o| (k, o)))
                .flat_map(|(k, o)| o.keys().map(move |f| format!("{k}.{f}")))
                .collect()
        };
        let keys = ceiling_keys(case);
        assert!(
            cases.iter().any(|other| {
                !Outcome::named(other["expect"]["outcome"].as_str().unwrap()).is_rejection()
                    && ceiling_keys(other) == keys
            }),
            "[{name}] rejects on {keys:?} with no in-cap control on the same ceiling; \
             without one the block proves nothing — a port that rejects every short \
             read would pass it",
        );
    }

    println!(
        "header_limits: {} cases ({rejections} rejections, {controls} in-cap controls)",
        cases.len()
    );
    assert!(rejections > 0 && controls > 0);
}

// --- the block itself --------------------------------------------------------

#[test]
fn every_header_limits_case_conforms() {
    let cases = header_limits();
    let mut ran = 0;
    let mut gated = 0;
    let mut checks = 0;

    for case in &cases {
        let name = case["name"].as_str().expect("name");
        let tags = requires(case);
        if !tags.iter().all(|t| capability_supported(t)) {
            // Unsatisfied `requires` means SKIP in this block, for every tag —
            // never the reduced-build rejection a vector gets.
            gated += 1;
            continue;
        }
        ran += 1;

        let ceilings = Ceilings::of(case);
        let chunks = case_chunks(case);
        let expected = Outcome::named(case["expect"]["outcome"].as_str().expect("expect.outcome"));

        // (a) feed `serialized` — or `chunks` where present — under the case's
        //     stated ceiling, and (b) assert `expect.outcome`.
        let mut consumer = Consumer::new(ceilings);
        let got = consumer.feed_all(&chunks);
        checks += 1;
        assert_eq!(
            got,
            expected,
            "[{name}] outcome mismatch for {} (declared {})",
            case["serialized"].as_str().unwrap_or("?"),
            case["declared"],
        );

        if case["expect"]["terminal"] == Value::Bool(true) {
            // (c) a further feed re-raises rather than consuming. Both entry
            //     points: more bytes, and the empty end-of-input probe.
            let before_consumed = consumer.consumed;
            let before_calls = consumer.receiver.calls;
            let more = [b'x'; 8];
            for further in [&more[..], &[][..], &more[..]] {
                checks += 1;
                assert_eq!(
                    Outcome::of(consumer.feed(further)),
                    expected,
                    "[{name}] a further feed did not re-raise the terminal verdict",
                );
            }
            checks += 1;
            assert_eq!(
                (consumer.consumed, consumer.receiver.calls),
                (before_consumed, before_calls),
                "[{name}] the terminal verdict consumed bytes / delivered fields \
                 instead of re-raising",
            );
        } else {
            // The in-cap control, driven one step further: `INCOMPLETE` claims
            // more bytes can change the verdict (§5.2.1), so the payload the
            // header declares must complete the message under the same ceiling.
            assert_eq!(expected, Outcome::Incomplete);
            let whole = hex_to_bytes(case["serialized"].as_str().unwrap());
            if let Some(payload) = completing_payload(&whole) {
                checks += 1;
                assert_eq!(
                    Outcome::of(consumer.feed(&payload)),
                    Outcome::Complete,
                    "[{name}] the ceiling admits this size, so its declared payload \
                     must complete the message",
                );
            }
        }
    }

    println!(
        "header_limits: {ran} of {} cases ran ({gated} gated out by `requires`), {checks} checks",
        cases.len(),
    );
    assert!(ran > 0, "no header_limits case ran");
}

// --- the pair the block exists for -------------------------------------------

#[test]
fn the_identical_bytes_pair_keeps_the_two_categories_apart() {
    let cases = header_limits();
    let find = |name: &str| -> Value {
        cases
            .iter()
            .find(|c| c["name"] == name)
            .unwrap_or_else(|| panic!("the block carries `{name}`"))
            .clone()
    };
    let capped = find("header_string_over_cap");
    let bounded = find("header_string_schema_bounded");

    // The premise: the same bytes, differing only in the ceiling configured.
    assert_eq!(
        capped["serialized"], bounded["serialized"],
        "the pair must carry identical bytes, or it tests nothing",
    );

    let run = |case: &Value| {
        let mut consumer = Consumer::new(Ceilings::of(case));
        consumer.feed_all(&case_chunks(case))
    };
    // A receiver cap: the bytes are well-formed, this receiver declines to hold
    // that much (§6.2.1). A schema bound: the bytes are not a legal value for
    // this field at all (MESSAGE_SPEC §7.1). Routing both to one category passes
    // every other case in the block and fails here.
    assert_eq!(run(&capped), Outcome::LimitExceeded);
    assert_eq!(run(&bounded), Outcome::Invalid);
    assert_ne!(run(&capped), run(&bounded));
}

// --- the negative control ----------------------------------------------------

#[test]
fn lifting_the_ceilings_falls_back_to_incomplete() {
    // Run the block's rejections again with no ceiling configured. Each must stop
    // rejecting and fall back to `incomplete` — that is what shows the verdicts
    // above come from the guard and not from something incidental about the
    // bytes. The one exception a case can legitimately have is a declared size
    // past this port's own **format** ceiling, which no receiver can lift.
    let cases = header_limits();
    let mut fell_back = 0;
    let mut format_rejected = 0;

    for case in &cases {
        let expected = Outcome::named(case["expect"]["outcome"].as_str().unwrap());
        if !expected.is_rejection() {
            continue;
        }
        let name = case["name"].as_str().unwrap();
        let tags = requires(case);
        if !tags.iter().all(|t| capability_supported(t)) {
            continue;
        }
        let declared = case["declared"].as_u64().expect("declared");
        let field_id = case["field_id"].as_u64().expect("field_id") as Id;

        let mut consumer = Consumer::new(Ceilings::lifted(field_id));
        let got = consumer.feed_all(&case_chunks(case));

        if declared > FORMAT_CEILING {
            format_rejected += 1;
            assert_eq!(
                got,
                Outcome::Invalid,
                "[{name}] declares {declared}, past the format ceiling \
                 {FORMAT_CEILING} — the corelib rejects it whatever the receiver \
                 configures",
            );
        } else {
            fell_back += 1;
            assert_eq!(
                got,
                Outcome::Incomplete,
                "[{name}] still rejects with every ceiling lifted, so its verdict \
                 above did not come from the ceiling",
            );
        }
    }

    println!(
        "header_limits negative control: {fell_back} rejections fell back to \
         incomplete, {format_rejected} stayed invalid on the format ceiling \
         ({FORMAT_CEILING})",
    );
    assert!(
        fell_back > 0,
        "no rejection was re-run with its ceiling lifted"
    );
}
