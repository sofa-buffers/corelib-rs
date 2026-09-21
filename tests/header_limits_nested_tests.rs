//! The shared `header_limits_nested` block: the ceiling answers at the length or
//! count word **at any depth** (CORELIB_PLAN §6.2.1, §6.3).
//!
//! This is `header_limits` one or two sequence frames deeper, and that is the
//! only difference. Every case in the flat block puts its field at `field_id 0`
//! in the top-level scope, so one axis stayed untested: the identical
//! over-ceiling header delivered **inside an open sequence**.
//!
//! ```text
//! 3e 1e 02 a2 06   then EOF
//! ^^ id 7, wire type 6 — sequence open
//!    ^^ id 3, wire type 6 — sequence open (the depth-2 cases)
//!       ^^ id 0, wire type 2 (fixlen)
//!          ^^^^^ length word (100 << 3) | 2  ->  a 100-byte string is declared
//!                  ... and the message ends, with both frames still open.
//! ```
//!
//! # Why the depth is its own axis
//!
//! A port can reach the enforcement point at the top level and lose it inside a
//! frame — the cap is held by the top-level receiver, the nested scope is decoded
//! by something else, and the declared size is never measured. The forward pass
//! catches exactly that: the case demands `limit_exceeded` where such a port
//! answers `incomplete`.
//!
//! And these bytes make `incomplete` *plausible*. The message ends with a frame
//! still open, so a decoder has a **second, fully independent reason** to say
//! "more bytes, please" — one that has nothing to do with the ceiling under test.
//! That cuts both ways, which is why
//! [`lifting_the_ceilings_stops_the_rejection`] is not decoration here: a port
//! that rejects for some unrelated reason (a depth guard, a refusal of unclosed
//! frames) would pass the forward pass while never having consulted the ceiling
//! at all. Only lifting the ceiling and watching the answer *change* tells the
//! two apart.
//!
//! # How this port runs them
//!
//! The corelib announces a nested sequence through [`sofab::Visitor::sequence_begin`] /
//! [`sofab::Visitor::sequence_end`], and announces the bound-bearing word through
//! `fixlen_begin` / `array_begin` — before any payload byte, at every depth. The
//! receiver therefore follows the frame chain the case names in `frames` and
//! applies the ceiling at the innermost depth and nowhere else; that receiver,
//! and the leaf that judges the declared size, are `tests/common/header_limits.rs`,
//! shared verbatim with `tests/header_limits_tests.rs`. A nested leaf of its own
//! would assert this file's arithmetic instead of the corelib's enforcement
//! point.
//!
//! [`binding_the_ceiling_at_the_top_level_caps_nothing`] states the same thing
//! from the failing side: the same ceiling bound at depth 0 never fires on these
//! bytes.
//!
//! # Gating
//!
//! `requires` means SKIP here, for every tag, exactly as in the flat block. This
//! build compiles every wire type in and declares the `receiver_caps` profile
//! capability, so all eight cases run; [`every_nested_header_limits_case_conforms`]
//! prints `ran` and `gated` so a silently disabled block cannot look green.

#[path = "common/header_limits.rs"]
mod support;

use serde_json::Value;
use support::{
    admitted, block, case_chunks, completing_payload, frames_of, hex_to_bytes, requires, Ceilings,
    Consumer, Outcome, SEQUENCE_END_MARKER,
};

/// The ceiling the negative control raises every stated ceiling to: far above
/// every `declared` in this block (the largest is 100), and small enough that
/// lifting it cannot provoke an absurd allocation.
const LIFTED_CEILING: u64 = 65536;

fn header_limits_nested() -> Vec<Value> {
    block("header_limits_nested")
}

// --- the block is well formed ------------------------------------------------

#[test]
fn the_nested_block_is_present_and_well_formed() {
    let cases = header_limits_nested();
    assert!(
        !cases.is_empty(),
        "the header_limits_nested block is empty; a missing or empty block is a \
         failure here, not a skip",
    );

    let mut rejections = 0;
    let mut controls = 0;
    let mut deepest = 0;
    for case in &cases {
        let name = case["name"].as_str().expect("name");
        for key in [
            "group",
            "description",
            "field_id",
            "declared",
            "serialized",
            "frames",
        ] {
            assert!(!case[key].is_null(), "[{name}] the case has no `{key}`");
        }

        // `frames` is the block. An empty chain would make the case a flat one
        // wearing the nested block's name, and a runner that read it as "no
        // frames" would bind its ceiling at the top level and cap nothing.
        let frames = frames_of(case);
        assert!(
            !frames.is_empty(),
            "[{name}] carries an empty `frames` chain; a nested case names at \
             least one sequence frame",
        );
        deepest = deepest.max(frames.len());

        // The bytes must actually open the frames the case claims, outermost
        // first — otherwise the chain the receiver builds and the chain on the
        // wire are two different things and the ceiling binds nowhere.
        let bytes = hex_to_bytes(case["serialized"].as_str().expect("serialized hex"));
        let mut pos = 0;
        for (depth, id) in frames.iter().enumerate() {
            let header = support::read_varint(&bytes, &mut pos).expect("a frame header");
            assert_eq!(
                (header >> 3, header & 0x07),
                (u64::from(*id), 0x6),
                "[{name}] `frames[{depth}]` says sequence id {id}, but the bytes \
                 open something else there",
            );
        }

        let outcome = Outcome::named(case["expect"]["outcome"].as_str().expect("expect.outcome"));

        // One ceiling or the other, never both: §6.2.1 forbids a receiver cap on
        // a field the schema already bounds, and the two answer differently, so
        // a case carrying both would have no defined verdict at all.
        assert!(
            !(case["limits"].is_object() && case["schema"].is_object()),
            "[{name}] states both `limits` and `schema`; §6.2.1 forbids a receiver \
             cap on a schema-bounded field",
        );
        assert!(
            Ceilings::of(case).count() <= 1,
            "[{name}] configures more than one ceiling; the case would not say \
             which one is under test",
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

    // Every rejection is paired with an in-cap control on the same ceiling *at
    // the same depth*: a port that rejects everything nested would otherwise
    // pass all four rejections and be badly broken.
    for case in &cases {
        let outcome = Outcome::named(case["expect"]["outcome"].as_str().unwrap());
        if !outcome.is_rejection() {
            continue;
        }
        let name = case["name"].as_str().unwrap();
        let signature = |c: &Value| -> (Vec<String>, Vec<u32>) {
            let keys = ["limits", "schema"]
                .iter()
                .filter_map(|k| c[*k].as_object().map(|o| (k, o)))
                .flat_map(|(k, o)| o.keys().map(move |f| format!("{k}.{f}")))
                .collect();
            (keys, frames_of(c))
        };
        let want = signature(case);
        assert!(
            cases.iter().any(|other| {
                !Outcome::named(other["expect"]["outcome"].as_str().unwrap()).is_rejection()
                    && signature(other) == want
            }),
            "[{name}] rejects on {want:?} with no in-cap control on the same ceiling \
             at the same depth; without one the block proves nothing — a port that \
             rejects every nested short read would pass it",
        );
    }

    // "One level may be special-cased": the depth-2 cases are what stop a chain
    // builder that is off by one from passing.
    assert!(
        deepest >= 2,
        "no case nests deeper than one frame; the depth-2 pair is what catches a \
         runner that descends once and then treats the inner sequence header as \
         the target field",
    );

    println!(
        "header_limits_nested: {} cases ({rejections} rejections, {controls} in-cap \
         controls), deepest chain {deepest} frames",
        cases.len()
    );
    assert!(rejections > 0 && controls > 0);
}

// --- the block itself --------------------------------------------------------

#[test]
fn every_nested_header_limits_case_conforms() {
    let cases = header_limits_nested();
    let mut ran = 0;
    let mut gated = 0;
    let mut checks = 0;
    let mut deepest_run = 0;

    for case in &cases {
        let name = case["name"].as_str().expect("name");
        if !admitted(case) {
            // Unsatisfied `requires` means SKIP in this block, for every tag —
            // and the tag that gated it is named, because a case skipped in
            // silence looks exactly like one that passed.
            gated += 1;
            let blocking: Vec<&str> = requires(case)
                .into_iter()
                .filter(|t| !support::capability_supported(t))
                .collect();
            println!("header_limits_nested: [{name}] gated by {blocking:?}");
            continue;
        }
        ran += 1;

        let ceilings = Ceilings::of(case);
        let frames = ceilings.frames.clone();
        let field_id = ceilings.field_id;
        deepest_run = deepest_run.max(frames.len());
        let chunks = case_chunks(case);
        let expected = Outcome::named(case["expect"]["outcome"].as_str().expect("expect.outcome"));

        // (a) feed `serialized` — or `chunks` where present — with the ceiling
        //     bound at the innermost depth of `frames`, and (b) assert
        //     `expect.outcome`. Nothing is appended and no frame is closed: the
        //     truncation is the case.
        let mut consumer = Consumer::new(ceilings);
        let got = consumer.feed_all(&chunks, name);
        checks += 1;
        assert_eq!(
            got,
            expected,
            "[{name}] outcome mismatch for {} (declared {}, frames {frames:?}): {}",
            case["serialized"].as_str().unwrap_or("?"),
            case["declared"],
            case["description"].as_str().unwrap_or(""),
        );

        if case["expect"]["terminal"] == Value::Bool(true) {
            // (c) a further feed re-raises rather than consuming. The bytes fed
            //     are the payload the header promised — what would finish the
            //     field if anything could — plus the empty end-of-input probe.
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
            // (d) rejected, never clamped (§6.2.1) — checked after the further
            //     feeds so a late materialization is caught too.
            checks += 1;
            assert_eq!(
                consumer.receiver.materialized, 0,
                "[{name}] the rejected field materialized payload; §6.2.1 rejects, \
                 it does not clamp",
            );
        } else {
            // The in-cap control, driven further: `INCOMPLETE` claims more bytes
            // can change the verdict (§5.2.1). Here it takes two steps — the
            // declared payload, which still leaves the frames open, and then one
            // end marker per frame, which completes the message. Both steps are
            // run under the case's own ceiling.
            assert_eq!(expected, Outcome::Incomplete);
            let whole = hex_to_bytes(case["serialized"].as_str().unwrap());
            if let Some(payload) = completing_payload(&whole, frames.len(), field_id) {
                checks += 1;
                assert_eq!(
                    Outcome::of(consumer.feed(&payload)),
                    Outcome::Incomplete,
                    "[{name}] the ceiling admits this size, so its declared payload \
                     must be accepted — and {} frame(s) are still open, so the \
                     message is not finished either",
                    frames.len(),
                );
                let closers = vec![SEQUENCE_END_MARKER; frames.len()];
                checks += 1;
                assert_eq!(
                    Outcome::of(consumer.feed(&closers)),
                    Outcome::Complete,
                    "[{name}] with its payload delivered and its frames closed the \
                     message must complete; an `incomplete` that cannot be lifted \
                     was never `incomplete`",
                );
            }
        }
    }

    println!(
        "header_limits_nested: {ran} of {} cases ran ({gated} gated out by \
         `requires`), {checks} checks, deepest chain run {deepest_run} frames",
        cases.len(),
    );
    assert_eq!(
        ran + gated,
        cases.len(),
        "every case is either run or gated, and counted as exactly one of them",
    );
    assert!(ran > 0, "no header_limits_nested case ran");
    assert!(
        deepest_run >= 2,
        "no depth-2 case ran; the one-frame cases alone cannot catch a chain \
         builder that is off by one",
    );
}

// --- the pair the block exists for -------------------------------------------

#[test]
fn the_identical_bytes_pair_keeps_the_two_categories_apart_one_frame_down() {
    let cases = header_limits_nested();
    let find = |name: &str| -> Value {
        cases
            .iter()
            .find(|c| c["name"] == name)
            .unwrap_or_else(|| panic!("the block carries `{name}`"))
            .clone()
    };
    let capped = find("nested_string_over_cap");
    let bounded = find("nested_string_schema_bounded");

    // The premise: the same bytes at the same depth, differing only in the
    // ceiling the case configures.
    assert_eq!(
        capped["serialized"], bounded["serialized"],
        "the pair must carry identical bytes, or it tests nothing",
    );
    assert_eq!(
        frames_of(&capped),
        frames_of(&bounded),
        "the pair must sit at the same depth, or the difference is not the ceiling",
    );

    let run = |case: &Value| {
        let mut consumer = Consumer::new(Ceilings::of(case));
        consumer.feed_all(&case_chunks(case), case["name"].as_str().unwrap_or("?"))
    };
    // A receiver cap: the bytes are well-formed, this receiver declines to hold
    // that much (§6.2.1). A schema bound: the bytes are not a legal value for
    // this field at all (MESSAGE_SPEC §7.1). A port that routes both to one
    // rejection category passes every other case in the block and fails here.
    assert_eq!(run(&capped), Outcome::LimitExceeded);
    assert_eq!(run(&bounded), Outcome::Invalid);
    assert_ne!(run(&capped), run(&bounded));
}

// --- the mis-binding the block is about --------------------------------------

#[test]
fn binding_the_ceiling_at_the_top_level_caps_nothing() {
    // The first way to be wrong while green elsewhere: read `frames`, then bind
    // the ceiling to the top-level scope anyway. These bytes never deliver the
    // field there, so the cap is never consulted and the answer falls back to
    // the `incomplete` the open frame justifies. Asserting it here keeps the
    // forward pass above honest — it shows those verdicts come from the *depth*
    // the ceiling was bound at, not merely from having configured one.
    let cases = header_limits_nested();
    let mut checked = 0;

    for case in &cases {
        let expected = Outcome::named(case["expect"]["outcome"].as_str().unwrap());
        if !expected.is_rejection() || !admitted(case) {
            continue;
        }
        let name = case["name"].as_str().unwrap();
        let mut consumer = Consumer::new(Ceilings::misbound_at_top_level(case));
        checked += 1;
        assert_eq!(
            consumer.feed_all(&case_chunks(case), name),
            Outcome::Incomplete,
            "[{name}] rejected with the ceiling bound at the top level, where this \
             field never arrives; the verdict cannot be the ceiling's",
        );
    }

    println!("header_limits_nested mis-binding control: {checked} rejections checked");
    assert!(
        checked > 0,
        "no rejection was re-run with a mis-bound ceiling"
    );
}

// --- the negative control ----------------------------------------------------

#[test]
fn lifting_the_ceilings_stops_the_rejection() {
    // The load-bearing control. Each rejection is run again with **the same kind
    // of ceiling the case states** raised to `LIFTED_CEILING` — a `schema` case
    // gets a lifted schema bound, a `limits` case a lifted receiver cap — and
    // the answer must *change*. What it changes to is deliberately not asserted:
    // the claim is that the ceiling caused the rejection, not what the
    // alternative answer is. (In practice it is `incomplete`, because a frame is
    // open — which is precisely the unrelated reason a decoder could have
    // rejected or accepted for, and the reason this control exists.)
    let cases = header_limits_nested();
    let mut checked = 0;
    let mut skipped_too_large = 0;

    for case in &cases {
        let expected = Outcome::named(case["expect"]["outcome"].as_str().unwrap());
        if !expected.is_rejection() {
            continue;
        }
        if !admitted(case) {
            continue;
        }
        let name = case["name"].as_str().unwrap();
        let declared = case["declared"].as_u64().expect("declared");
        if declared >= LIFTED_CEILING {
            // No such case today — this block carries no amplification case, so
            // the control has no exemption. If one is added, it is named here
            // rather than silently passing through the loop.
            skipped_too_large += 1;
            println!(
                "header_limits_nested negative control: [{name}] declares {declared}, \
                 at or above the lifted ceiling {LIFTED_CEILING} — not coverable"
            );
            continue;
        }

        let stated = Ceilings::of(case);
        assert_eq!(
            stated.count(),
            1,
            "[{name}] states {} ceilings; the control would not know which to lift",
            stated.count(),
        );
        let mut consumer = Consumer::new(Ceilings::raised_to(case, LIFTED_CEILING));
        let got = consumer.feed_all(&case_chunks(case), name);
        checked += 1;
        println!(
            "header_limits_nested negative control: [{name}] {expected:?} -> {got:?} \
             with the ceiling raised from {} to {LIFTED_CEILING}",
            case["limits"]
                .as_object()
                .or_else(|| case["schema"].as_object())
                .and_then(|o| o.values().next())
                .map(|v| v.to_string())
                .unwrap_or_else(|| "?".into()),
        );
        assert_ne!(
            got, expected,
            "[{name}] still answers {expected:?} with its ceiling raised to \
             {LIFTED_CEILING}, well above the {declared} it declares — the verdict \
             in the forward pass did not come from the ceiling, so this case \
             proves nothing about it",
        );
    }

    let rejections = cases
        .iter()
        .filter(|c| {
            Outcome::named(c["expect"]["outcome"].as_str().unwrap()).is_rejection() && admitted(c)
        })
        .count();
    println!(
        "header_limits_nested negative control: {checked} of {rejections} admitted \
         rejections changed their answer with the ceiling lifted to \
         {LIFTED_CEILING} ({skipped_too_large} not coverable)",
    );
    // Counting is what stops the control degenerating into a loop that examines
    // nothing: a mis-spelled outcome name or a gate evaluated differently here
    // would leave this at zero while every assertion above still "passed".
    assert_eq!(
        checked + skipped_too_large,
        rejections,
        "the control did not visit every admitted rejection",
    );
    assert!(
        checked > 0,
        "the negative control examined nothing; without it a runner that never \
         reached the ceiling is indistinguishable from one that did",
    );
}
