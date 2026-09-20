//! The shared `sequence_growth` block (CORELIB_PLAN §7.2 item 8, ARCHITECTURE §9.5).
//!
//! A growth case is **a delivery sequence of element ids, not a byte vector**. A
//! wrapper array carries no element count on the wire — its length is *highest
//! present id + 1* (MESSAGE_SPEC §5.1) — so two ports that grow differently emit
//! identical bytes and reach identical outcomes. No `serialized` hex could tell
//! them apart, and the port therefore builds the message itself from `deliver`
//! and measures the container it filled.
//!
//! # Why this port runs the block at all
//!
//! `requires: ["dynamic_arrays"]` is the one tag that gates on **how a port
//! allocates** rather than on a wire construct it can represent, so a statically
//! bounded profile honours it and skips (`test_vectors_README.md`; C, C++
//! `c-cpp` and Rust `no_std` are named there). This crate is the `std` profile
//! and is not among them.
//!
//! The container is not the corelib's, though: this crate ships no collector
//! layer — no `Caps`, no `Bounds`, no `StringSeq` — so growth lives in generated
//! code, and the receiver cap is held above the corelib exactly as it is for
//! `header_limits`. What the block pins *here* is therefore the half that is the
//! corelib's: that the decoder reports each element id, sparsely and unshifted,
//! that it reports an id **before** the element's frame is entered so a cap can
//! refuse it before the container grows, and that the refusal is terminal.
//!
//! # Growth geometry is not asserted here
//!
//! A conformant decoder grows to *at least* `id + 1` so a sparse array does not
//! cost O(n²) copies (ARCHITECTURE §9.5 shape B). That is an allocation-shape
//! property of the container, and the container here is the test's own, so
//! asserting it would only measure this file. It is stated rather than reported
//! as passed, which is what CORELIB_PLAN §7.2 item 8 asks for.

use serde_json::Value;
use sofab::{decode, Error, IStream, Id, OStream, Status, Unsigned, Visitor};

const VECTORS_JSON: &str = include_str!("../assets/test_vectors.json");

/// The receiver cap the block is run against.
///
/// The cases express their boundaries as offsets on the cap (`id_from_cap: -1`
/// is `cap - 1`), because §6.2.1 deliberately fixes no family-wide number. Four
/// is the floor every case needs; this runs at **eight** so that a cap-relative
/// index can never coincide with an absolute one a case also uses — at four,
/// `growth_gap_filled`'s absolute length 4 and `length_from_cap: 0` would be the
/// same number, and a runner that confused the two would still pass.
const CAP: usize = 8;

/// Scratch buffer for building a case's message. The largest case writes a
/// handful of short elements, so this is far above what any of them needs.
const BUILD_BUF: usize = 4096;

// --- capability gating -------------------------------------------------------

/// Whether this port satisfies one `requires` tag. An unsatisfied tag means the
/// case is **skipped**, never rejected: a growth case's message is an ordinary
/// well-formed wrapper array that even a statically bounded port decodes
/// perfectly — only the growth it asserts would be out of reach.
fn capability_supported(tag: &str) -> bool {
    match tag {
        // Wire constructs: this build has the full wire format compiled in.
        "fixlen" | "array" | "sequence" | "fp32" | "fp64" | "int32" | "int64" => true,
        // Profile capability. This is the `std` crate: generated destinations
        // are heap-backed and grow, so the tag holds — see the module docs.
        "dynamic_arrays" => true,
        other => panic!(
            "the shared file requires capability `{other}`, which this port has not \
             ruled on; decide whether it holds here before the case is run or \
             skipped (test_vectors_README.md, CORELIB_PLAN §7.2)",
        ),
    }
}

fn requires(case: &Value) -> Vec<&str> {
    case.get("requires")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

// --- reading the block -------------------------------------------------------

fn growth_cases() -> Vec<Value> {
    let doc: Value = serde_json::from_str(VECTORS_JSON).expect("parse test_vectors.json");
    doc["sequence_growth"]
        .as_array()
        .expect("the shared file carries a `sequence_growth` array")
        .clone()
}

/// Resolve a value that is written either absolutely or as an offset on the cap.
///
/// Exactly one of the two keys is present; both or neither is a malformed case,
/// not a tolerated default, so it panics rather than guessing.
fn resolve(case: &Value, absolute: &str, from_cap: &str, what: &str) -> usize {
    let abs = case.get(absolute).and_then(Value::as_i64);
    let rel = case.get(from_cap).and_then(Value::as_i64);
    match (abs, rel) {
        (Some(v), None) => usize::try_from(v).expect("a non-negative index"),
        (None, Some(off)) => {
            let resolved = CAP as i64 + off;
            assert!(
                resolved >= 0,
                "{what}: `{from_cap}` {off} resolves below zero at cap {CAP}",
            );
            resolved as usize
        }
        _ => panic!(
            "{what}: exactly one of `{absolute}` / `{from_cap}` must be present \
             (test_vectors_README.md)",
        ),
    }
}

/// The element index one `deliver` entry asks for.
fn deliver_index(el: &Value) -> usize {
    resolve(el, "id", "id_from_cap", "deliver entry")
}

// --- building the message ----------------------------------------------------

/// Build a case's message: the wrapper frame, then one element per `deliver`
/// entry at the index it names.
///
/// The frame is closed with `keep` so that an empty wrapper survives as an
/// explicitly framed, zero-length array (§5.1) rather than being omitted.
fn build(case: &Value) -> Vec<u8> {
    let field_id = case["field_id"].as_u64().expect("field_id") as Id;
    let element_type = case["element_type"].as_str().expect("element_type");
    let deliver = case["deliver"].as_array().expect("deliver");

    let mut buf = [0u8; BUILD_BUF];
    let used = {
        let mut os = OStream::new(&mut buf);
        os.write_sequence_begin_lazy(field_id)
            .expect("open wrapper");
        for el in deliver {
            let id = u32::try_from(deliver_index(el)).expect("an id within range") as Id;
            match element_type {
                "string" => {
                    let s = el["value"].as_str().expect("a string element value");
                    os.write_str(id, s).expect("write element");
                }
                "struct" => {
                    // A struct element is a framed sub-sequence carrying one
                    // unsigned field at id 0.
                    let v = el["value"].as_u64().expect("a struct element value");
                    os.write_sequence_begin_lazy(id).expect("open element");
                    os.write_unsigned(0, v as Unsigned).expect("write field");
                    os.write_sequence_end_keep().expect("close element");
                }
                other => panic!("unknown `element_type` `{other}` in the sequence_growth block"),
            }
        }
        os.write_sequence_end_keep().expect("close wrapper");
        os.flush().expect("flush");
        os.bytes_used()
    };
    buf[..used].to_vec()
}

// --- the receiving container -------------------------------------------------

/// What one slot of the grown container holds.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Slot {
    /// Never delivered — the element default (`""` / `0`).
    Default,
    Str(String),
    Num(u64),
}

/// A growing wrapper-array destination with a receiver cap on the element index.
///
/// This is the shape generated code has: a container that extends to hold the
/// highest id it is given, and refuses an index at or above the cap **before**
/// extending (§6.2.1 — the cap binds the element *index*, ARCHITECTURE §9.5).
struct Growing {
    field_id: Id,
    cap: usize,
    /// Depth relative to the wrapper: 0 outside it, 1 inside it, 2 inside an
    /// element's own frame. The visitor is told a level opened and closed and
    /// tracks its own depth — `sequence_begin` carries no depth.
    depth: usize,
    /// The element currently being filled, while inside its frame.
    current: Option<usize>,
    slots: Vec<Slot>,
    /// Raised the moment an index is refused; the feed loop makes it terminal.
    verdict: Option<Error>,
}

impl Growing {
    fn new(field_id: Id, cap: usize) -> Self {
        Self {
            field_id,
            cap,
            depth: 0,
            current: None,
            slots: Vec::new(),
            verdict: None,
        }
    }

    /// Admit an element index, or refuse it.
    ///
    /// Returns `false` when the index is refused, and the container is left
    /// exactly as it was: the refusal happens before any extension, which is
    /// what `expect.max_length` measures.
    fn admit(&mut self, index: usize) -> bool {
        if self.verdict.is_some() {
            return false;
        }
        if index >= self.cap {
            self.verdict = Some(Error::LimitExceeded);
            return false;
        }
        if self.slots.len() < index + 1 {
            self.slots.resize(index + 1, Slot::Default);
        }
        true
    }
}

impl Visitor for Growing {
    fn sequence_begin(&mut self, id: Id) {
        self.depth += 1;
        if self.depth == 1 {
            // The wrapper field itself. Asserting the id here is what makes the
            // depth counting below trustworthy: if the decoder opened some other
            // field first, every index measured after it would be misattributed.
            assert_eq!(
                id, self.field_id,
                "the outermost frame should be the case's wrapper field",
            );
            return;
        }
        // An element's own frame opens at depth 2.
        if self.depth == 2 && self.verdict.is_none() {
            let index = id as usize;
            if self.admit(index) {
                self.current = Some(index);
            }
        }
    }

    fn sequence_end(&mut self) {
        if self.depth == 2 {
            self.current = None;
        }
        self.depth = self.depth.saturating_sub(1);
    }

    fn string(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {
        // A leaf element sits directly inside the wrapper.
        if self.depth != 1 || self.verdict.is_some() {
            return;
        }
        let index = id as usize;
        if offset == 0 && !self.admit(index) {
            return;
        }
        if self.verdict.is_some() || index >= self.slots.len() {
            return;
        }
        // A payload may arrive in chunks, so append rather than assign.
        let text = match &mut self.slots[index] {
            Slot::Str(s) => s,
            slot => {
                *slot = Slot::Str(String::with_capacity(total));
                match slot {
                    Slot::Str(s) => s,
                    _ => unreachable!("just assigned"),
                }
            }
        };
        text.push_str(&String::from_utf8_lossy(chunk));
    }

    fn unsigned(&mut self, id: Id, value: Unsigned) {
        // The single field of a struct element, at id 0 inside its frame.
        if self.depth == 2 && id == 0 {
            if let Some(index) = self.current {
                self.slots[index] = Slot::Num(value);
            }
        }
    }
}

// --- feeding, with the cap's verdict dominating and latching ------------------

/// What a case answered, in the shared file's spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Complete,
    Incomplete,
    Invalid,
    LimitExceeded,
}

impl Outcome {
    fn of(result: Result<Status, Error>) -> Self {
        match result {
            Ok(Status::Complete) => Outcome::Complete,
            Ok(Status::Incomplete) => Outcome::Incomplete,
            Err(Error::InvalidMsg) => Outcome::Invalid,
            Err(Error::LimitExceeded) => Outcome::LimitExceeded,
            Err(other) => panic!("decode reported {other:?}, which is not a decode outcome"),
        }
    }

    fn named(name: &str) -> Self {
        match name {
            "complete" => Outcome::Complete,
            "limit_exceeded" => Outcome::LimitExceeded,
            other => panic!(
                "unknown `expect.outcome` `{other}` in the sequence_growth block; \
                 decide what this port must answer for it (test_vectors_README.md)",
            ),
        }
    }
}

/// Run one case at a given cap, returning the outcome and the container.
fn run_at(case: &Value, cap: usize) -> (Outcome, Vec<Slot>) {
    let field_id = case["field_id"].as_u64().expect("field_id") as Id;
    let bytes = build(case);

    let mut dest = Growing::new(field_id, cap);
    let mut stream = IStream::new();
    let outcome = stream.feed(&bytes, &mut dest);

    // The receiver's verdict dominates the corelib's own answer for the same
    // bytes, and is terminal once raised (§6.3).
    let answer = match dest.verdict {
        Some(raised) => Outcome::of(Err(raised)),
        None => Outcome::of(outcome),
    };
    (answer, dest.slots)
}

fn run(case: &Value) -> (Outcome, Vec<Slot>) {
    run_at(case, CAP)
}

// --- the cases ---------------------------------------------------------------

#[test]
fn every_growth_case_conforms() {
    let cases = growth_cases();
    let mut ran = 0usize;
    let mut gated = 0usize;

    for case in &cases {
        let name = case["name"].as_str().expect("name");
        let tags = requires(case);
        if !tags.iter().all(|t| capability_supported(t)) {
            gated += 1;
            continue;
        }
        ran += 1;

        let expect = &case["expect"];
        let want = Outcome::named(expect["outcome"].as_str().expect("outcome"));
        let (got, slots) = run(case);
        assert_eq!(got, want, "{name}: outcome");

        match want {
            Outcome::Complete => {
                let want_len = resolve(expect, "length", "length_from_cap", name);
                assert_eq!(slots.len(), want_len, "{name}: container length");

                // Every delivered element sits at its own index, unshifted.
                for el in case["deliver"].as_array().expect("deliver") {
                    let index = deliver_index(el);
                    let got = &slots[index];
                    match case["element_type"].as_str().expect("element_type") {
                        "string" => {
                            let want = el["value"].as_str().expect("value");
                            assert_eq!(got, &Slot::Str(want.to_string()), "{name}: slot {index}");
                        }
                        _ => {
                            let want = el["value"].as_u64().expect("value");
                            assert_eq!(got, &Slot::Num(want), "{name}: slot {index}");
                        }
                    }
                }

                // A gap holds the element default, not a shifted neighbour.
                for id in expect
                    .get("default_ids")
                    .and_then(Value::as_array)
                    .map(|a| a.to_vec())
                    .unwrap_or_default()
                {
                    let index = id.as_u64().expect("a default id") as usize;
                    assert_eq!(slots[index], Slot::Default, "{name}: slot {index} is a gap");
                }
            }
            _ => {
                // The container must not have been extended past the point the
                // refusal happened — that is what makes it a refusal *before*
                // growth rather than a rollback after it.
                if let Some(max) = expect.get("max_length").and_then(Value::as_u64) {
                    assert!(
                        slots.len() as u64 <= max,
                        "{name}: container grew to {} past max_length {max}",
                        slots.len(),
                    );
                }
            }
        }
    }

    assert!(
        ran > 0,
        "no growth case ran — the gating excluded all of them"
    );
    println!(
        "sequence_growth: {ran} of {} cases ran ({gated} gated out by `requires`) at cap {CAP}",
        cases.len()
    );
}

#[test]
fn a_refusal_is_terminal_and_consumes_nothing_after() {
    // §6.3: once the cap has refused, a further feed re-raises and moves nothing.
    let cases = growth_cases();
    let mut checked = 0usize;
    for case in &cases {
        if case["expect"]["outcome"].as_str() != Some("limit_exceeded") {
            continue;
        }
        let name = case["name"].as_str().expect("name");
        let field_id = case["field_id"].as_u64().expect("field_id") as Id;
        let bytes = build(case);

        let mut dest = Growing::new(field_id, CAP);
        let mut stream = IStream::new();
        let _ = stream.feed(&bytes, &mut dest);
        let raised = dest.verdict.expect("the cap refused");
        let len_at_refusal = dest.slots.len();

        // Feeding the same bytes again must not admit anything further.
        let _ = stream.feed(&bytes, &mut dest);
        assert_eq!(
            dest.verdict,
            Some(raised),
            "{name}: verdict changed on re-feed"
        );
        assert_eq!(
            dest.slots.len(),
            len_at_refusal,
            "{name}: the container grew after the refusal",
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "no limit_exceeded case to check terminality on"
    );
}

#[test]
fn lifting_the_cap_lets_every_refused_case_through() {
    // The negative control, and the reason the refusals above mean anything: a
    // refused case must be refused by *policy*, not by its bytes. Raise the cap
    // and the same message must decode, growing past what `max_length` allowed.
    let cases = growth_cases();
    let mut checked = 0usize;
    for case in &cases {
        let expect = &case["expect"];
        if expect["outcome"].as_str() != Some("limit_exceeded") {
            continue;
        }
        let name = case["name"].as_str().expect("name");
        let (got, slots) = run_at(case, CAP * 4);
        assert_eq!(
            got,
            Outcome::Complete,
            "{name}: refused even with the cap lifted"
        );
        if let Some(max) = expect.get("max_length").and_then(Value::as_u64) {
            assert!(
                slots.len() as u64 > max,
                "{name}: with the cap lifted the container should grow past max_length {max}, \
                 got {} — the case may never have reached the cap at all",
                slots.len(),
            );
        }
        checked += 1;
    }
    assert!(checked > 0, "no limit_exceeded case to lift the cap on");
}

#[test]
fn the_block_is_present_and_well_formed() {
    let cases = growth_cases();
    // Floors, not equalities: the shared file may grow cases, and this port
    // should adopt them rather than pin their count.
    assert!(
        cases.len() >= 8,
        "the block should carry at least the eight authored cases"
    );

    let mut groups = std::collections::BTreeSet::new();
    let mut element_types = std::collections::BTreeSet::new();
    let mut outcomes = std::collections::BTreeSet::new();

    for case in &cases {
        let name = case["name"].as_str().expect("every case is named");
        assert!(
            requires(case).contains(&"dynamic_arrays"),
            "{name}: every growth case must carry the `dynamic_arrays` tag, \
             or a statically bounded port would run it",
        );
        assert!(case.get("field_id").is_some(), "{name}: field_id");
        assert!(case.get("deliver").is_some(), "{name}: deliver");
        groups.insert(case["group"].as_str().expect("group").to_string());
        element_types.insert(
            case["element_type"]
                .as_str()
                .expect("element_type")
                .to_string(),
        );
        outcomes.insert(
            case["expect"]["outcome"]
                .as_str()
                .expect("outcome")
                .to_string(),
        );

        // Every id resolves, and every case stays inside the cap this runs at
        // except where it is deliberately over it.
        for el in case["deliver"].as_array().expect("deliver") {
            let _ = deliver_index(el);
        }
    }

    assert!(
        groups.len() >= 4,
        "all four groups should be represented, got {groups:?}"
    );
    assert_eq!(
        element_types,
        ["string", "struct"].iter().map(|s| s.to_string()).collect(),
        "both element kinds carry the boundary cases",
    );
    assert_eq!(
        outcomes,
        ["complete", "limit_exceeded"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        "the block asserts both a grown container and a refused index",
    );
}

#[test]
fn the_cap_separates_absolute_indices_from_cap_relative_ones() {
    // The cases mix absolute ids/lengths with cap-relative ones, and a runner
    // that resolved the two the same way would still pass if the cap happened to
    // make them equal. Measured against the block itself rather than asserted on
    // the constant: every cap-relative value must land strictly above every
    // absolute one the file uses.
    let cases = growth_cases();
    let mut absolute = Vec::new();
    let mut relative = Vec::new();

    for case in &cases {
        let expect = &case["expect"];
        for (holder, abs_key, rel_key) in [
            (expect, "length", "length_from_cap"),
            (expect, "max_length", "max_length_from_cap"),
        ] {
            if let Some(v) = holder.get(abs_key).and_then(Value::as_u64) {
                absolute.push(v as usize);
            }
            if let Some(off) = holder.get(rel_key).and_then(Value::as_i64) {
                relative.push((CAP as i64 + off) as usize);
            }
        }
        for el in case["deliver"].as_array().expect("deliver") {
            if let Some(v) = el.get("id").and_then(Value::as_u64) {
                absolute.push(v as usize);
            }
            if let Some(off) = el.get("id_from_cap").and_then(Value::as_i64) {
                relative.push((CAP as i64 + off) as usize);
            }
        }
    }

    assert!(
        !relative.is_empty(),
        "the block should express boundaries on the cap"
    );
    let highest_absolute = absolute.iter().copied().max().unwrap_or(0);
    let lowest_relative = relative
        .iter()
        .copied()
        .min()
        .expect("a cap-relative value");
    assert!(
        lowest_relative > highest_absolute,
        "cap {CAP} is too low: the lowest cap-relative value ({lowest_relative}) does not \
         clear the highest absolute one ({highest_absolute}), so the two spellings could \
         be confused without failing",
    );
}

#[test]
fn a_sparse_delivery_reaches_the_decoder_unshifted() {
    // The corelib's own half, asserted without the cap in the way: ids arrive as
    // sent, with the gaps still gaps. If this failed, every length assertion
    // above would be measuring the wrong thing.
    let mut buf = [0u8; BUILD_BUF];
    let used = {
        let mut os = OStream::new(&mut buf);
        os.write_sequence_begin_lazy(0).unwrap();
        os.write_str(0, "a").unwrap();
        os.write_str(2, "c").unwrap();
        os.write_str(5, "f").unwrap();
        os.write_sequence_end_keep().unwrap();
        os.flush().unwrap();
        os.bytes_used()
    };

    let mut dest = Growing::new(0, CAP);
    let status = decode(&buf[..used], &mut dest).expect("a well-formed sparse wrapper decodes");
    assert_eq!(status, Status::Complete);
    assert_eq!(dest.verdict, None);
    assert_eq!(dest.slots.len(), 6, "length is highest id + 1 (§5.1)");
    assert_eq!(dest.slots[0], Slot::Str("a".into()));
    assert_eq!(dest.slots[1], Slot::Default);
    assert_eq!(dest.slots[2], Slot::Str("c".into()));
    assert_eq!(dest.slots[3], Slot::Default);
    assert_eq!(dest.slots[4], Slot::Default);
    assert_eq!(dest.slots[5], Slot::Str("f".into()));
}
