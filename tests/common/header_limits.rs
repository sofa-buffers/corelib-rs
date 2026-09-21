//! The machinery both header-ceiling blocks run on: `header_limits` (the field
//! at the top level) and `header_limits_nested` (the identical field one or two
//! sequence frames deeper).
//!
//! # Why this is one module and not two
//!
//! The two blocks are required to differ in **where the field arrives** and in
//! nothing else. A nested runner with its own leaf — an uncapped read plus a
//! size comparison the flat leaf does not make — would assert its own
//! arithmetic instead of the corelib's enforcement point, and would pass even if
//! the nested decode path never announced the bound-bearing word at all. So the
//! leaf lives here, once: [`Receiver::judge`] and the two header hooks that call
//! it are the same code for both blocks, and `frames` is the only thing that
//! moves.
//!
//! The module is pulled into both test targets with `#[path]` rather than
//! through `tests/common/mod.rs`, so the eight other targets that say
//! `mod common;` do not compile the vector file in as well.
//!
//! # What sits where
//!
//! The corelib is schema-agnostic and enforces no receiver limit of its own; it
//! owes the consumer the *information and the ordering* — [`Visitor::fixlen_begin`]
//! once per scalar fixlen field and [`Visitor::array_begin`] once per array
//! field, after the bound-bearing word is read and validated and **before any
//! payload byte**, at every depth. [`Receiver`] is the consumer above it, reduced
//! to the one decision these blocks are about, and [`Consumer`] adds the two
//! things CORELIB_PLAN §6.3 asks of a ceiling: the verdict dominates the
//! corelib's own outcome for the same bytes, and once raised it is terminal.

#![allow(dead_code)]

use serde_json::Value;
use sofab::{ArrayKind, Error, FixlenType, IStream, Id, Status, Visitor};

/// The shared vectors, embedded from the verbatim asset copy.
pub const VECTORS_JSON: &str = include_str!("../../assets/test_vectors.json");

/// This port's **format** ceiling on a declared fixlen length and on an array
/// count (`INT32_MAX`, §4.6/§4.7). It is the corelib's own and the one ceiling a
/// receiver cannot lift, so it is what a case is measured against in a
/// ceilings-lifted control. The crate's `FIXLEN_MAX`/`ARRAY_MAX` are internal,
/// so the value is restated here rather than imported.
pub const FORMAT_CEILING: u64 = i32::MAX as u64;

/// A sequence-end marker on the wire: the varint `T_SEQUENCE_END`, carrying no
/// id because it closes the innermost open frame (CORELIB_PLAN §4.9). Used only
/// to drive an in-cap control past the point where its frames are still open.
pub const SEQUENCE_END_MARKER: u8 = 0x07;

// --- the three-valued outcome, plus the policy category ----------------------

/// What a feed sequence answered, in the shared file's spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Complete,
    Incomplete,
    Invalid,
    LimitExceeded,
}

impl Outcome {
    /// The outcome this port's `Result<Status>` carries (§5.2.1: `INCOMPLETE` is
    /// not an error and rides the success arm; `INVALID` and the terminal
    /// `LimitExceeded` ride the error channel §6.3 pairs them with).
    pub fn of(result: Result<Status, Error>) -> Self {
        match result {
            Ok(Status::Complete) => Outcome::Complete,
            Ok(Status::Incomplete) => Outcome::Incomplete,
            Err(Error::InvalidMsg) => Outcome::Invalid,
            Err(Error::LimitExceeded) => Outcome::LimitExceeded,
            Err(other) => panic!("decode reported {other:?}, which is not a decode outcome"),
        }
    }

    /// Parse an `expect.outcome` string.
    pub fn named(name: &str) -> Self {
        match name {
            "complete" => Outcome::Complete,
            "incomplete" => Outcome::Incomplete,
            "invalid" => Outcome::Invalid,
            "limit_exceeded" => Outcome::LimitExceeded,
            other => panic!(
                "unknown `expect.outcome` `{other}` in a header-ceiling block; \
                 decide what this port must answer for it (test_vectors_README.md)",
            ),
        }
    }

    pub fn is_rejection(self) -> bool {
        matches!(self, Outcome::Invalid | Outcome::LimitExceeded)
    }
}

// --- capability gating -------------------------------------------------------

/// Whether this port satisfies one `requires` tag. An unsatisfied tag means the
/// case is **skipped**, never rejected.
pub fn capability_supported(tag: &str) -> bool {
    match tag {
        // Wire constructs: this build has every wire type and the 64-bit value
        // width compiled in, so every construct in either block is
        // representable — nested sequences included.
        "fixlen" | "array" | "sequence" | "fp32" | "fp64" | "int32" | "int64" => true,
        // Profile capability, declared here — see the two test modules' docs.
        "receiver_caps" => true,
        other => panic!(
            "the shared file requires capability `{other}`, which this port has not \
             ruled on; decide whether it holds here before the case is run or \
             skipped (test_vectors_README.md, CORELIB_PLAN §7.2)",
        ),
    }
}

/// The `requires` tags of one case (empty when the key is absent).
pub fn requires(case: &Value) -> Vec<&str> {
    case.get("requires")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// Are every one of a case's `requires` tags satisfied here?
pub fn admitted(case: &Value) -> bool {
    requires(case).iter().all(|t| capability_supported(t))
}

// --- the ceilings one case configures ----------------------------------------

/// The ceiling a case asks the port to configure, bound to the field it applies
/// to **and to the depth that field arrives at**.
///
/// Exactly one of the four ceilings is set for a rejection case. `frames` is the
/// chain of sequence field ids the target field is nested in, outermost first —
/// empty for the flat block, one or two deep for `header_limits_nested`. The
/// ceiling fires only when the decoder is inside exactly that chain: binding it
/// at the top level instead is the mistake the nested block exists to catch.
#[derive(Debug, Clone, Default)]
pub struct Ceilings {
    pub frames: Vec<Id>,
    pub field_id: Id,
    /// §6.2.1 receiver caps. A breach is `LimitExceeded`: policy, not malformation.
    pub max_dyn_string_len: Option<u64>,
    pub max_dyn_blob_len: Option<u64>,
    pub max_dyn_array_count: Option<u64>,
    /// A schema `maxlen`. A breach is `InvalidMsg`: MESSAGE_SPEC §7.1 says these
    /// bytes are not a legal value for this field, whatever the receiver's
    /// capacity.
    pub schema_maxlen: Option<u64>,
}

impl Ceilings {
    /// The ceilings the case states. `schema` and `limits` are mutually exclusive
    /// (§6.2.1 forbids applying a receiver cap to a field the schema bounds), and
    /// each block's well-formedness test holds the file to that.
    pub fn of(case: &Value) -> Self {
        let limits = &case["limits"];
        Self {
            frames: frames_of(case),
            field_id: case["field_id"].as_u64().expect("field_id") as Id,
            max_dyn_string_len: limits["max_dyn_string_len"].as_u64(),
            max_dyn_blob_len: limits["max_dyn_blob_len"].as_u64(),
            max_dyn_array_count: limits["max_dyn_array_count"].as_u64(),
            schema_maxlen: case["schema"]["maxlen"].as_u64(),
        }
    }

    /// The same field at the same depth with every ceiling removed — the flat
    /// block's negative control.
    pub fn lifted(case: &Value) -> Self {
        Self {
            frames: frames_of(case),
            field_id: case["field_id"].as_u64().expect("field_id") as Id,
            ..Self::default()
        }
    }

    /// The same field at the same depth with **the ceiling the case states**
    /// raised to `to` — the nested block's negative control, which must lift the
    /// same *kind* of ceiling the case configures or it proves nothing.
    pub fn raised_to(case: &Value, to: u64) -> Self {
        let stated = Self::of(case);
        Self {
            max_dyn_string_len: stated.max_dyn_string_len.map(|_| to),
            max_dyn_blob_len: stated.max_dyn_blob_len.map(|_| to),
            max_dyn_array_count: stated.max_dyn_array_count.map(|_| to),
            schema_maxlen: stated.schema_maxlen.map(|_| to),
            ..stated
        }
    }

    /// The same ceilings, but bound at the top level instead of at the depth the
    /// case states — the mis-binding the nested block is about.
    pub fn misbound_at_top_level(case: &Value) -> Self {
        Self {
            frames: Vec::new(),
            ..Self::of(case)
        }
    }

    /// How many ceilings this configures. One for every case in either block.
    pub fn count(&self) -> usize {
        [
            self.max_dyn_string_len,
            self.max_dyn_blob_len,
            self.max_dyn_array_count,
            self.schema_maxlen,
        ]
        .iter()
        .filter(|c| c.is_some())
        .count()
    }
}

/// A case's `frames` chain, outermost first; empty when the key is absent (the
/// flat block, whose field is at the top level).
pub fn frames_of(case: &Value) -> Vec<Id> {
    case.get("frames")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|f| f.as_u64().expect("a frame is a sequence field id") as Id)
                .collect()
        })
        .unwrap_or_default()
}

// --- the consumer above the corelib ------------------------------------------

/// The receiving half: generated code's job, reduced to the one decision these
/// blocks are about.
///
/// It judges the declared size **in the header hook**, which is where the corelib
/// hands it over — before a payload byte is asked for and therefore before a
/// message that ends at the word can be mistaken for a truncated one. The
/// decoder announces a nested sequence through [`Visitor::sequence_begin`] /
/// [`Visitor::sequence_end`], so following that chain is how this receiver
/// descends: the ceiling is applied at the innermost depth of `frames` and
/// nowhere else.
#[derive(Default)]
pub struct Receiver {
    ceilings: Ceilings,
    /// The sequence frames currently open, outermost first.
    stack: Vec<Id>,
    /// The verdict this receiver raised, if any.
    pub verdict: Option<Error>,
    /// Every callback the corelib made, header hooks included. A terminal
    /// rejection must stop this from growing.
    pub calls: usize,
    /// Payload actually delivered for the bound field at the bound depth —
    /// bytes for a string/blob, elements for an array. §6.2.1 is "rejected,
    /// never clamped", so after a rejection this must still be zero.
    pub materialized: usize,
}

impl Receiver {
    pub fn new(ceilings: Ceilings) -> Self {
        Self {
            ceilings,
            stack: Vec::new(),
            verdict: None,
            calls: 0,
            materialized: 0,
        }
    }

    /// Is the decoder inside exactly the frame chain this receiver descends?
    fn at_bound_depth(&self) -> bool {
        self.stack == self.ceilings.frames
    }

    /// Is `id` the bound field, arriving at the bound depth?
    fn is_bound_field(&self, id: Id) -> bool {
        id == self.ceilings.field_id && self.at_bound_depth()
    }

    /// Measure a declared size against whichever ceiling this field carries.
    ///
    /// `cap` is the §6.2.1 receiver cap for the subtype that actually arrived —
    /// the corelib reports what is *on the wire*, and a receiver measures against
    /// the cap for that kind or not at all.
    fn judge(&mut self, id: Id, declared: u64, cap: Option<u64>) {
        if !self.is_bound_field(id) || self.verdict.is_some() {
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

    /// One element of the bound field materialized.
    fn materialize(&mut self, id: Id, amount: usize) {
        if self.is_bound_field(id) {
            self.materialized += amount;
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

    fn unsigned(&mut self, id: Id, _value: sofab::Unsigned) {
        self.calls += 1;
        self.materialize(id, 1);
    }
    fn signed(&mut self, id: Id, _value: sofab::Signed) {
        self.calls += 1;
        self.materialize(id, 1);
    }
    fn fp32(&mut self, id: Id, _value: f32) {
        self.calls += 1;
        self.materialize(id, 1);
    }
    fn fp64(&mut self, id: Id, _value: f64) {
        self.calls += 1;
        self.materialize(id, 1);
    }
    fn string(&mut self, id: Id, _total: usize, _offset: usize, chunk: &[u8]) {
        self.calls += 1;
        self.materialize(id, chunk.len());
    }
    fn blob(&mut self, id: Id, _total: usize, _offset: usize, chunk: &[u8]) {
        self.calls += 1;
        self.materialize(id, chunk.len());
    }
    fn sequence_begin(&mut self, id: Id) {
        self.calls += 1;
        self.stack.push(id);
    }
    fn sequence_end(&mut self) {
        self.calls += 1;
        self.stack.pop();
    }
}

/// A decoder plus its receiver: the pair a caller actually holds.
///
/// Its whole contribution over a bare [`IStream`] is the two things §6.3 asks of
/// a ceiling — the receiver's verdict *dominates* the corelib's own outcome for
/// the same bytes, and once raised it is **terminal**: a further feed re-raises
/// it and consumes nothing.
pub struct Consumer {
    stream: IStream,
    pub receiver: Receiver,
    /// The terminal verdict, once one has been reached.
    latched: Option<Error>,
    /// Bytes actually handed to the decoder. A re-raise must not move this.
    pub consumed: usize,
}

impl Consumer {
    pub fn new(ceilings: Ceilings) -> Self {
        Self {
            stream: IStream::new(),
            receiver: Receiver::new(ceilings),
            latched: None,
            consumed: 0,
        }
    }

    pub fn feed(&mut self, chunk: &[u8]) -> Result<Status, Error> {
        if let Some(terminal) = self.latched {
            // Terminal (§6.3): re-raise, and do not touch the bytes.
            return Err(terminal);
        }
        self.consumed += chunk.len();
        let outcome = self.stream.feed(chunk, &mut self.receiver);
        if let Some(raised) = self.receiver.verdict {
            // The ceiling fired inside the header hook. It was decided at the
            // word, so it overrides the `INCOMPLETE` the corelib reports for a
            // message whose payload never arrived — and, in the nested block,
            // the second `INCOMPLETE` the still-open frames would justify.
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
    /// the last chunk when there is none. Every feed before the last must answer
    /// `incomplete`, or the decoder answered on bytes it had not yet seen.
    pub fn feed_all(&mut self, chunks: &[Vec<u8>], name: &str) -> Outcome {
        let mut last = Outcome::Complete;
        for (i, chunk) in chunks.iter().enumerate() {
            last = Outcome::of(self.feed(chunk));
            if last.is_rejection() {
                return last;
            }
            if i + 1 < chunks.len() {
                assert_eq!(
                    last,
                    Outcome::Incomplete,
                    "[{name}] chunk {i} of {} already answered {last:?}; a verdict \
                     before the last chunk is a verdict on bytes not yet seen",
                    chunks.len(),
                );
            }
        }
        last
    }
}

// --- reading a block ---------------------------------------------------------

/// One top-level block of the shared file, by name.
pub fn block(name: &str) -> Vec<Value> {
    let doc: Value = serde_json::from_str(VECTORS_JSON).expect("parse test_vectors.json");
    doc[name]
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "the shared file carries no `{name}` block; refresh \
                 assets/test_vectors.json from corelib-c-cpp"
            )
        })
        .clone()
}

pub fn hex_to_bytes(hex: &str) -> Vec<u8> {
    assert!(hex.len() % 2 == 0, "odd-length hex string `{hex}`");
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex byte"))
        .collect()
}

/// The bytes a case is fed: `chunks` when present, else `serialized` whole.
pub fn case_chunks(case: &Value) -> Vec<Vec<u8>> {
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

/// The payload that would finish the field a case's header declares — the bytes
/// that make `INCOMPLETE` mean what §5.2.1 says it means.
///
/// `frames` sequence-open headers are skipped first, so the same reader serves
/// the flat block (`frames == 0`) and the nested one. Only the shapes these
/// blocks use at an admitted size are built: a `string` / `blob` header (its
/// declared bytes) and an unsigned varint array header (its declared elements,
/// one byte each). `None` for anything else, and the caller then skips this
/// check rather than guessing.
///
/// The frames themselves are **not** closed here: a nested case's message is
/// still open after this, which is exactly the second reason for `INCOMPLETE`
/// the nested block is about.
pub fn completing_payload(bytes: &[u8], frames: usize, field_id: Id) -> Option<Vec<u8>> {
    let mut pos = 0;
    for _ in 0..frames {
        let header = read_varint(bytes, &mut pos)?;
        if header & 0x07 != 0x6 {
            return None; // not the sequence-open header the case's `frames` claims
        }
    }
    let header = read_varint(bytes, &mut pos)?;
    if header >> 3 != u64::from(field_id) {
        return None; // not the field these cases put on the wire
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
pub fn read_varint(bytes: &[u8], pos: &mut usize) -> Option<u64> {
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
