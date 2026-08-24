//! CORELIB_PLAN §6.5, "Testing (normative)" — the fp32 signaling-NaN hazard.
//!
//! "Assert that a signaling, a quiet and a negative `fp32` NaN each round-trip
//! **bit-for-bit** at both a scalar and an array position, across decode →
//! re-encode **and** any materialized walk, on **every** decode surface."
//!
//! Rust is one of §6.5's *native `fp32`* targets: `f32` holds the payload
//! end-to-end, `istream.rs` reads it with `f32::from_le_bytes` and `ostream.rs`
//! writes it with `to_le_bytes`, so nothing widens and there is no raw-bytes
//! channel to provide. That is what makes the test the whole obligation here —
//! the guard against a *future* widening on the round-trip path.
//!
//! The suite's other float tests only encode and compare bytes
//! (`ostream_tests::a_float_array_is_bit_exact_at_every_buffer_size`); this one
//! closes the loop: decode → re-encode from the value the **visitor** received.
//!
//! Both decode surfaces are covered: `decode` (one-shot) and `IStream::feed` at
//! chunk size 1, where the payload is reassembled across four `feed` calls.
//!
//! JSON cannot represent NaN (§4.6, §7.1), which is why this is an
//! implementation-level suite rather than a shared vector.

use sofab::{decode, IStream, Id, OStream, Visitor};

/// The three payloads §6.5 names, plus the value the hazard destroys first.
const NANS: [(&str, u32); 4] = [
    // Signaling: quiet bit clear, payload non-zero. Widening to `f64` and back
    // returns 0x7FC0_0001 — the failure this section exists to prevent.
    ("signaling", 0x7F80_0001),
    ("quiet", 0x7FC0_0001),
    ("negative signaling", 0xFF80_0001),
    ("negative quiet", 0xFFC0_0001),
];

/// Records the `f32`s the decoder handed over, as raw bits.
#[derive(Default)]
struct Floats {
    bits: Vec<u32>,
}

impl Visitor for Floats {
    fn fp32(&mut self, _id: Id, value: f32) {
        // `to_bits` on the value the visitor was given — a widening anywhere on
        // the decode path would already have quieted the payload by here.
        self.bits.push(value.to_bits());
    }
}

/// Decode `wire` one whole buffer at a time.
fn one_shot(wire: &[u8]) -> Vec<u32> {
    let mut sink = Floats::default();
    decode(wire, &mut sink).unwrap();
    sink.bits
}

/// Decode `wire` one byte at a time — every float split across four `feed`s.
fn byte_at_a_time(wire: &[u8]) -> Vec<u32> {
    let mut sink = Floats::default();
    let mut is = IStream::new();
    let mut last = Ok(());
    for i in 0..wire.len() {
        last = is.feed(&wire[i..i + 1], &mut sink);
    }
    assert_eq!(last, Ok(()), "the chunked feed did not complete");
    sink.bits
}

/// Encode `values` at a **scalar** `fp32` position, one field each.
fn encode_scalars(values: &[f32], buf: &mut [u8]) -> usize {
    let mut os = OStream::new(buf);
    for (i, v) in values.iter().enumerate() {
        os.write_fp32(i as Id + 1, *v).unwrap();
    }
    os.bytes_used()
}

/// Encode `values` as the elements of one `fp32` **array** field.
fn encode_array(values: &[f32], buf: &mut [u8]) -> usize {
    let mut os = OStream::new(buf);
    os.write_array_fp32(1, values).unwrap();
    os.bytes_used()
}

/// The payload bytes of every `fp32` on the wire, in order — what §4.6 says must
/// be reproduced exactly. Both encodings put each element in the last four bytes
/// of its own run, so slicing them out per value is unnecessary: re-encoding the
/// decoded values and comparing the whole message is the stronger assertion.
#[test]
fn every_fp32_nan_round_trips_bit_for_bit_at_both_positions() {
    let values: Vec<f32> = NANS.iter().map(|(_, b)| f32::from_bits(*b)).collect();
    let expected: Vec<u32> = NANS.iter().map(|(_, b)| *b).collect();

    for (position, encode) in [
        ("scalar", encode_scalars as fn(&[f32], &mut [u8]) -> usize),
        ("array", encode_array),
    ] {
        let mut buf = [0u8; 128];
        let used = encode(&values, &mut buf);
        let wire = &buf[..used];

        for (surface, bits) in [
            ("decode", one_shot(wire)),
            ("feed(1)", byte_at_a_time(wire)),
        ] {
            assert_eq!(
                bits, expected,
                "[{position} / {surface}] a NaN payload was altered on decode"
            );

            // Decode -> re-encode, from the `f32` the visitor received.
            let decoded: Vec<f32> = bits.iter().map(|b| f32::from_bits(*b)).collect();
            let mut again = [0u8; 128];
            let again_used = encode(&decoded, &mut again);
            assert_eq!(
                &again[..again_used],
                wire,
                "[{position} / {surface}] re-encoding changed the wire bytes"
            );
        }
    }
}

/// Named individually so a failure says which payload broke, and so the
/// signaling case — the only one the IEEE widening actually destroys — cannot be
/// lost in a loop that still passes on the other three.
#[test]
fn each_nan_payload_survives_the_scalar_round_trip() {
    for (name, bits) in NANS {
        let mut buf = [0u8; 16];
        let used = {
            let mut os = OStream::new(&mut buf);
            os.write_fp32(1, f32::from_bits(bits)).unwrap();
            os.bytes_used()
        };
        assert_eq!(
            &buf[..used],
            &[
                0x0A,
                4 << 3, // header (id 1, FIXLEN), fixlen word (len 4, fp32)
                bits as u8,
                (bits >> 8) as u8,
                (bits >> 16) as u8,
                (bits >> 24) as u8
            ],
            "[{name}] the encoder did not write the payload verbatim"
        );

        assert_eq!(one_shot(&buf[..used]), [bits], "[{name}] decode");
        assert_eq!(byte_at_a_time(&buf[..used]), [bits], "[{name}] feed(1)");
    }
}

/// The hazard itself, as an executable statement of what the tests above guard
/// against: a payload that passes through an `f64` may come back **quiet**, and
/// "no later code can recover it" (§6.5).
///
/// What a widening does to a signaling NaN is the *platform's* choice, not the
/// port's, and the family's two CI hosts disagree: on x86-64 the IEEE widening
/// sets the quiet bit, exactly as §6.5's diagram shows; on s390x (the
/// `test-big-endian` job) the payload survives the round trip. So the assertion
/// is the one statement true on both — the result is still a NaN, and if it moved
/// at all it moved **only** by the quiet bit being set. Either way it is a reason
/// never to route an `fp32` through a double, which this port does not: the two
/// tests above assert the port's own path keeps the four wire bytes at both
/// positions and on both decode surfaces, which is why this one is an observation
/// rather than a requirement.
#[test]
fn widening_through_a_double_can_quiet_the_payload() {
    let sig = f32::from_bits(0x7F80_0001);
    let widened = (sig as f64 as f32).to_bits();

    assert!(
        f32::from_bits(widened).is_nan(),
        "a NaN stopped being a NaN"
    );
    assert!(
        widened == sig.to_bits() || widened == sig.to_bits() | 0x0040_0000,
        "widening changed more than the quiet bit: {:#010X} -> {widened:#010X}",
        sig.to_bits()
    );
}
