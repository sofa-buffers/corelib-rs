//! Round-trip tests: encode with [`OStream`], decode with [`IStream`], and
//! assert the decoded events reconstruct the original values.

mod common;

use common::{Event, Recorder};
use sofab::{ArrayKind, IStream, OStream};

fn roundtrip<F: FnOnce(&mut OStream)>(f: F) -> Vec<Event> {
    let mut buf = [0u8; 256];
    let used = {
        let mut os = OStream::new(&mut buf);
        f(&mut os);
        os.bytes_used()
    };
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    is.feed(&buf[..used], &mut rec).expect("decode failed");
    rec.events
}

#[test]
fn scalars_roundtrip() {
    let ev = roundtrip(|os| {
        os.write_unsigned(1, 0).unwrap();
        os.write_unsigned(2, u64::MAX).unwrap();
        os.write_signed(3, i64::MIN).unwrap();
        os.write_signed(4, i64::MAX).unwrap();
        os.write_boolean(5, true).unwrap();
        os.write_fp32(6, core::f32::consts::PI).unwrap();
        os.write_fp64(7, core::f64::consts::E).unwrap();
    });
    assert_eq!(
        ev,
        [
            Event::Unsigned(1, 0),
            Event::Unsigned(2, u64::MAX),
            Event::Signed(3, i64::MIN),
            Event::Signed(4, i64::MAX),
            Event::Unsigned(5, 1),
            Event::Fp32(6, core::f32::consts::PI.to_bits()),
            Event::Fp64(7, core::f64::consts::E.to_bits()),
        ]
    );
}

#[test]
fn string_and_blob_roundtrip() {
    let ev = roundtrip(|os| {
        os.write_str(10, "SofaBuffers").unwrap();
        os.write_blob(11, &[0xDE, 0xAD, 0xBE, 0xEF]).unwrap();
    });
    assert_eq!(
        ev,
        [
            Event::Str(10, b"SofaBuffers".to_vec()),
            Event::Blob(11, vec![0xDE, 0xAD, 0xBE, 0xEF]),
        ]
    );
}

#[test]
fn arrays_roundtrip() {
    let ev = roundtrip(|os| {
        os.write_array_unsigned(1, &[10u16, 20, 30]).unwrap();
        os.write_array_signed(2, &[-5i64, 5]).unwrap();
        os.write_array_fp64(3, &[1.5f64, -2.5]).unwrap();
    });
    assert_eq!(
        ev,
        [
            Event::ArrayBegin(1, ArrayKind::Unsigned, 3),
            Event::Unsigned(1, 10),
            Event::Unsigned(1, 20),
            Event::Unsigned(1, 30),
            Event::ArrayBegin(2, ArrayKind::Signed, 2),
            Event::Signed(2, -5),
            Event::Signed(2, 5),
            Event::ArrayBegin(3, ArrayKind::Fp64, 2),
            Event::Fp64(3, 1.5f64.to_bits()),
            Event::Fp64(3, (-2.5f64).to_bits()),
        ]
    );
}

#[test]
fn zero_count_arrays_roundtrip() {
    let ev = roundtrip(|os| {
        let eu: [u32; 0] = [];
        let ei: [i32; 0] = [];
        let e32: [f32; 0] = [];
        let e64: [f64; 0] = [];
        os.write_array_unsigned(1, &eu).unwrap();
        os.write_array_signed(2, &ei).unwrap();
        os.write_array_fp32(3, &e32).unwrap();
        os.write_array_fp64(4, &e64).unwrap();
    });
    assert_eq!(
        ev,
        [
            Event::ArrayBegin(1, ArrayKind::Unsigned, 0),
            Event::ArrayBegin(2, ArrayKind::Signed, 0),
            Event::ArrayBegin(3, ArrayKind::Fp32, 0),
            Event::ArrayBegin(4, ArrayKind::Fp64, 0),
        ]
    );
}

/// A wrapper-array **element** that is all-default must keep its frame, and the
/// thing that breaks if it does not is a decoded **length**, not a byte count:
/// a dynamic array's length is *highest present element id + 1* (§5.1), so a
/// dropped trailing element shortens the array.
///
/// This is asserted here, end to end through the decoder, because the shared
/// vectors cannot assert it: every `array/*` vector in
/// `assets/test_vectors.json` has leaf (string) elements, so no vector — in
/// either the `serialized` or the `serialized_sparse` column — puts a *sequence*
/// at element position, and none of them distinguishes `end` from `end_keep`
/// there. Replaying them exercises only the dropping closer.
#[test]
fn an_all_default_array_element_keeps_the_arrays_length() {
    /// Element ids one level inside the wrapper, in order.
    fn element_ids(ev: &[Event]) -> Vec<u32> {
        let mut depth = 0usize;
        let mut ids = Vec::new();
        for e in ev {
            match e {
                Event::SequenceBegin(id) => {
                    if depth == 1 {
                        ids.push(*id);
                    }
                    depth += 1;
                }
                Event::SequenceEnd => depth -= 1,
                _ => {}
            }
        }
        ids
    }

    // Array field id 4, three struct elements; element 2 (the last) is
    // all-default and closed with `end_keep`, so its frame survives.
    let kept = roundtrip(|os| {
        os.write_sequence_begin_lazy(4).unwrap(); // the array wrapper
        for id in 0..2u32 {
            os.write_sequence_begin_lazy(id).unwrap();
            os.write_unsigned(0, 10 + id as u64).unwrap();
            os.write_sequence_end_keep().unwrap();
        }
        os.write_sequence_begin_lazy(2).unwrap(); // all-default element
        os.write_sequence_end_keep().unwrap();
        os.write_sequence_end().unwrap();
    });
    assert_eq!(element_ids(&kept), [0, 1, 2]);
    assert_eq!(
        element_ids(&kept).iter().max().unwrap() + 1,
        3,
        "the array must decode as three elements"
    );

    // The same message with that element closed by the *field* closer — what a
    // generator emitting `end` at element position would produce. The element
    // vanishes and the array decodes one element short: a changed value, not
    // merely changed bytes.
    let dropped = roundtrip(|os| {
        os.write_sequence_begin_lazy(4).unwrap();
        for id in 0..2u32 {
            os.write_sequence_begin_lazy(id).unwrap();
            os.write_unsigned(0, 10 + id as u64).unwrap();
            os.write_sequence_end_keep().unwrap();
        }
        os.write_sequence_begin_lazy(2).unwrap();
        os.write_sequence_end().unwrap(); // wrong closer for an element
        os.write_sequence_end().unwrap();
    });
    assert_eq!(element_ids(&dropped), [0, 1]);
    assert_eq!(
        element_ids(&dropped).iter().max().unwrap() + 1,
        2,
        "dropping the frame is what shortens the array"
    );
}

#[test]
fn deep_nested_sequences_roundtrip() {
    let ev = roundtrip(|os| {
        os.write_unsigned(0, 1).unwrap();
        for _ in 0..5 {
            os.write_sequence_begin_lazy(1).unwrap();
            os.write_unsigned(0, 42).unwrap();
        }
        for _ in 0..5 {
            os.write_sequence_end().unwrap();
        }
    });

    let mut expected = vec![Event::Unsigned(0, 1)];
    for _ in 0..5 {
        expected.push(Event::SequenceBegin(1));
        expected.push(Event::Unsigned(0, 42));
    }
    for _ in 0..5 {
        expected.push(Event::SequenceEnd);
    }
    assert_eq!(ev, expected);
}
