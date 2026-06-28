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
            Event::ArrayBegin(3, ArrayKind::Fixlen, 2),
            Event::Fp64(3, 1.5f64.to_bits()),
            Event::Fp64(3, (-2.5f64).to_bits()),
        ]
    );
}

#[test]
fn deep_nested_sequences_roundtrip() {
    let ev = roundtrip(|os| {
        os.write_unsigned(0, 1).unwrap();
        for _ in 0..5 {
            os.write_sequence_begin(1).unwrap();
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
