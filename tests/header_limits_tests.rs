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
//!
//! # Where the machinery lives
//!
//! [`Receiver`], [`Consumer`] and the ceilings they hold are in
//! `tests/common/header_limits.rs`, shared verbatim with
//! `tests/header_limits_nested_tests.rs`. The nested block is this block one or
//! two sequence frames deeper and must differ in *where the field arrives* and
//! in nothing else, so the leaf that judges the declared size is one piece of
//! code for both. Here every case's `frames` chain is empty: the field is at the
//! top level.

#[path = "common/header_limits.rs"]
mod support;

use serde_json::Value;
use support::{
    admitted, block, case_chunks, completing_payload, hex_to_bytes, Ceilings, Consumer, Outcome,
    FORMAT_CEILING,
};

// --- reading the block -------------------------------------------------------

fn header_limits() -> Vec<Value> {
    block("header_limits")
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
        // The flat block puts every field at the top level; a case that grew a
        // `frames` chain belongs in `header_limits_nested`, whose runner binds
        // the ceiling at depth. Reading it here and ignoring it would cap
        // nothing and answer `incomplete`.
        assert!(
            case["frames"].is_null(),
            "[{name}] carries a `frames` chain; the nested block and its runner \
             own that axis (tests/header_limits_nested_tests.rs)",
        );
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
        if !admitted(case) {
            // Unsatisfied `requires` means SKIP in this block, for every tag —
            // never the reduced-build rejection a vector gets.
            gated += 1;
            continue;
        }
        ran += 1;

        let ceilings = Ceilings::of(case);
        let field_id = ceilings.field_id;
        let chunks = case_chunks(case);
        let expected = Outcome::named(case["expect"]["outcome"].as_str().expect("expect.outcome"));

        // (a) feed `serialized` — or `chunks` where present — under the case's
        //     stated ceiling, and (b) assert `expect.outcome`.
        let mut consumer = Consumer::new(ceilings);
        let got = consumer.feed_all(&chunks, name);
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
            // (d) rejected, never clamped (§6.2.1): nothing of the field was
            //     materialized, checked *after* the further feeds so a late
            //     materialization is caught too.
            checks += 1;
            assert_eq!(
                consumer.receiver.materialized, 0,
                "[{name}] the rejected field materialized payload; §6.2.1 rejects, \
                 it does not clamp",
            );
        } else {
            // The in-cap control, driven one step further: `INCOMPLETE` claims
            // more bytes can change the verdict (§5.2.1), so the payload the
            // header declares must complete the message under the same ceiling.
            assert_eq!(expected, Outcome::Incomplete);
            let whole = hex_to_bytes(case["serialized"].as_str().unwrap());
            if let Some(payload) = completing_payload(&whole, 0, field_id) {
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
    assert_eq!(
        ran + gated,
        cases.len(),
        "every case is either run or gated, and named as one or the other",
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
        consumer.feed_all(&case_chunks(case), case["name"].as_str().unwrap_or("?"))
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
        if !admitted(case) {
            continue;
        }
        let declared = case["declared"].as_u64().expect("declared");

        let mut consumer = Consumer::new(Ceilings::lifted(case));
        let got = consumer.feed_all(&case_chunks(case), name);

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
