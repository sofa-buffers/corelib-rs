//! Encoder tests. Every `expected` byte array is taken verbatim from the C
//! reference suite `test/c/test_ostream.c`.

// Float test vectors are deliberately the literals used by the C suite.
#![allow(clippy::approx_constant, clippy::excessive_precision)]

mod common;

use sofab::{Error, FixlenType, OStream, ID_MAX};

/// Encode with a fresh stack buffer and return the produced bytes.
fn encode<F: FnOnce(&mut OStream)>(f: F) -> Vec<u8> {
    let mut buf = [0u8; 128];
    let used = {
        let mut os = OStream::new(&mut buf);
        f(&mut os);
        os.bytes_used()
    };
    buf[..used].to_vec()
}

// --- ids --------------------------------------------------------------------

#[test]
fn id_min() {
    assert_eq!(encode(|os| os.write_unsigned(0, 0).unwrap()), [0x00, 0x00]);
}

#[test]
fn id_max() {
    assert_eq!(
        encode(|os| os.write_unsigned(ID_MAX, 0).unwrap()),
        [0xF8, 0xFF, 0xFF, 0xFF, 0x3F, 0x00]
    );
}

#[test]
fn id_overflow_is_argument_error() {
    let mut buf = [0u8; 16];
    let mut os = OStream::new(&mut buf);
    assert_eq!(os.write_unsigned(ID_MAX + 1, 0), Err(Error::Argument));
}

// --- unsigned varint (subset of the C boundary table) -----------------------

#[test]
fn write_unsigned_boundaries() {
    let cases: &[(u64, &[u8])] = &[
        (0, &[0x00, 0x00]),
        (127, &[0x00, 0x7F]),
        (128, &[0x00, 0x80, 0x01]),
        (0x3FFF, &[0x00, 0xFF, 0x7F]),
        (0x4000, &[0x00, 0x80, 0x80, 0x01]),
        (
            0x8000_0000_0000_0000,
            &[
                0x00, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01,
            ],
        ),
        (
            u64::MAX,
            &[
                0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01,
            ],
        ),
    ];
    for (value, expected) in cases {
        assert_eq!(
            encode(|os| os.write_unsigned(0, *value).unwrap()),
            *expected
        );
    }
}

// --- signed -----------------------------------------------------------------

#[test]
fn write_signed_min() {
    assert_eq!(
        encode(|os| os.write_signed(0, i64::MIN).unwrap()),
        [0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01]
    );
}

#[test]
fn write_signed_max() {
    assert_eq!(
        encode(|os| os.write_signed(0, i64::MAX).unwrap()),
        [0x01, 0xFE, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01]
    );
}

#[test]
fn write_boolean() {
    assert_eq!(
        encode(|os| os.write_boolean(0, true).unwrap()),
        [0x00, 0x01]
    );
}

// --- fixed length -----------------------------------------------------------

#[test]
fn write_fp32() {
    assert_eq!(
        encode(|os| os.write_fp32(0, 3.1415).unwrap()),
        [0x02, 0x20, 0x56, 0x0E, 0x49, 0x40]
    );
}

#[test]
fn write_fp64() {
    // The C test passes a float literal promoted to double: write_fp64(3.14159265f)
    assert_eq!(
        encode(|os| os.write_fp64(0, 3.14159265_f32 as f64).unwrap()),
        [0x02, 0x41, 0x00, 0x00, 0x00, 0x60, 0xFB, 0x21, 0x09, 0x40]
    );
}

#[test]
fn write_string() {
    assert_eq!(
        encode(|os| os.write_str(0, "Hello Couch!").unwrap()),
        [0x02, 0x62, 0x48, 0x65, 0x6C, 0x6C, 0x6F, 0x20, 0x43, 0x6F, 0x75, 0x63, 0x68, 0x21]
    );
}

#[test]
fn write_string_empty() {
    assert_eq!(encode(|os| os.write_str(0, "").unwrap()), [0x02, 0x02]);
}

#[test]
fn write_blob() {
    assert_eq!(
        encode(|os| os.write_blob(0, &[0x01, 0x02, 0x03, 0x04, 0x05]).unwrap()),
        [0x02, 0x2B, 0x01, 0x02, 0x03, 0x04, 0x05]
    );
}

#[test]
fn write_blob_empty() {
    assert_eq!(encode(|os| os.write_blob(0, &[]).unwrap()), [0x02, 0x03]);
}

/// §4.6: an `fp32`/`fp64` payload is **exactly** 4 / 8 bytes, and a `fixlen_word`
/// declaring any other length for those subtypes is malformed — the `INVALID`
/// decode outcome. The byte-level `write_fixlen` is public (§6.1), so it is the
/// one encode path able to put such a word on the wire; it must refuse the call
/// (`InvalidArgument`, §6.3) instead of reporting success for a message every
/// conformant decoder — including this port's own — has to reject.
#[test]
fn write_fixlen_rejects_wrong_float_width() {
    for (subtype, ok_len) in [(FixlenType::Fp32, 4usize), (FixlenType::Fp64, 8usize)] {
        for bad_len in [0usize, 1, 2, 3, 5, 7, 8, 9, 16] {
            if bad_len == ok_len {
                continue;
            }
            let mut buf = [0u8; 64];
            let mut os = OStream::new(&mut buf);
            assert_eq!(
                os.write_fixlen(0, &vec![0u8; bad_len], subtype),
                Err(Error::Argument),
                "{subtype:?} must refuse a {bad_len}-byte payload",
            );
            // Refused means *nothing written*: no header, no length word.
            assert_eq!(os.bytes_used(), 0);
        }

        // The exact width still encodes, byte-identically to write_fp32/fp64.
        let bytes = encode(|os| os.write_fixlen(0, &vec![0u8; ok_len], subtype).unwrap());
        let reference = if ok_len == 4 {
            encode(|os| os.write_fp32(0, 0.0).unwrap())
        } else {
            encode(|os| os.write_fp64(0, 0.0).unwrap())
        };
        assert_eq!(bytes, reference);
    }
}

/// `blob` constrains neither width nor content — only the `FIXLEN_MAX` ceiling —
/// so the subtype check must leave it alone.
#[test]
fn write_fixlen_blob_accepts_any_bytes() {
    for len in [0usize, 1, 3, 5, 8, 33] {
        let payload = vec![0xFFu8; len];
        let bytes = encode(|os| os.write_fixlen(7, &payload, FixlenType::Blob).unwrap());
        let reference = encode(|os| os.write_blob(7, &payload).unwrap());
        assert_eq!(bytes, reference);
    }
}

// --- varint arrays ----------------------------------------------------------

#[test]
fn write_array_of_u32() {
    let a: [u32; 5] = [1, 2, 3, 0x8000_0000, u32::MAX];
    assert_eq!(
        encode(|os| os.write_array_unsigned(0, &a).unwrap()),
        [
            0x03, 0x05, 0x01, 0x02, 0x03, 0x80, 0x80, 0x80, 0x80, 0x08, 0xFF, 0xFF, 0xFF, 0xFF,
            0x0F
        ]
    );
}

#[test]
fn write_array_of_i32() {
    let a: [i32; 5] = [-1, -2, -3, i32::MIN, i32::MAX];
    assert_eq!(
        encode(|os| os.write_array_signed(0, &a).unwrap()),
        [
            0x04, 0x05, 0x01, 0x03, 0x05, 0xFF, 0xFF, 0xFF, 0xFF, 0x0F, 0xFE, 0xFF, 0xFF, 0xFF,
            0x0F
        ]
    );
}

#[test]
fn write_array_of_i8() {
    let a: [i8; 5] = [-1, -2, -3, i8::MIN, i8::MAX];
    assert_eq!(
        encode(|os| os.write_array_signed(0, &a).unwrap()),
        [0x04, 0x05, 0x01, 0x03, 0x05, 0xFF, 0x01, 0xFE, 0x01]
    );
}

#[test]
fn write_array_of_u8() {
    let a: [u8; 5] = [1, 2, 3, 0, u8::MAX];
    assert_eq!(
        encode(|os| os.write_array_unsigned(0, &a).unwrap()),
        [0x03, 0x05, 0x01, 0x02, 0x03, 0x00, 0xFF, 0x01]
    );
}

#[test]
fn write_array_of_i16() {
    let a: [i16; 5] = [-1, -2, -3, i16::MIN, i16::MAX];
    assert_eq!(
        encode(|os| os.write_array_signed(0, &a).unwrap()),
        [0x04, 0x05, 0x01, 0x03, 0x05, 0xFF, 0xFF, 0x03, 0xFE, 0xFF, 0x03]
    );
}

#[test]
fn write_array_of_u16() {
    let a: [u16; 5] = [1, 2, 3, 0, u16::MAX];
    assert_eq!(
        encode(|os| os.write_array_unsigned(0, &a).unwrap()),
        [0x03, 0x05, 0x01, 0x02, 0x03, 0x00, 0xFF, 0xFF, 0x03]
    );
}

#[test]
fn write_array_of_i64() {
    let a: [i64; 5] = [-1, -2, -3, i64::MIN, i64::MAX];
    assert_eq!(
        encode(|os| os.write_array_signed(0, &a).unwrap()),
        [
            0x04, 0x05, 0x01, 0x03, 0x05, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
            0x01, 0xFE, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01
        ]
    );
}

#[test]
fn write_array_of_u64() {
    let a: [u64; 5] = [1, 2, 3, 0, u64::MAX];
    assert_eq!(
        encode(|os| os.write_array_unsigned(0, &a).unwrap()),
        [
            0x03, 0x05, 0x01, 0x02, 0x03, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
            0xFF, 0x01
        ]
    );
}

// --- fixlen arrays ----------------------------------------------------------

#[test]
fn write_array_of_fp32() {
    let a: [f32; 5] = [1.0, 2.0, 3.0, -f32::MAX, f32::MAX];
    assert_eq!(
        encode(|os| os.write_array_fp32(0, &a).unwrap()),
        [
            0x05, 0x05, 0x20, 0x00, 0x00, 0x80, 0x3F, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x40,
            0x40, 0xFF, 0xFF, 0x7F, 0xFF, 0xFF, 0xFF, 0x7F, 0x7F
        ]
    );
}

#[test]
fn write_array_of_fp64() {
    let a: [f64; 5] = [1.0, 2.0, 3.0, -f64::MAX, f64::MAX];
    assert_eq!(
        encode(|os| os.write_array_fp64(0, &a).unwrap()),
        [
            0x05, 0x05, 0x41, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xF0, 0x3F, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x40, 0xFF,
            0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xEF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xEF,
            0x7F
        ]
    );
}

/// A float array's payload is the elements' little-endian images back to back,
/// and the encoder emits that run in **bulk** rather than element by element.
/// Two properties have to survive that, and neither is visible in the fixed
/// vectors above:
///
/// * **Bit-exactness.** The wire bytes are the bit pattern, not the value: a
///   signalling NaN keeps its payload, `-0.0` its sign, a subnormal its
///   mantissa. `to_le_bytes` is the reference, so the assertion holds on a
///   big-endian host too — where the bulk run has to byte-swap after all.
/// * **Buffer-independence.** §5.1 lets that run be divided at any byte, so the
///   size of the buffer it is streamed through must not show in the output —
///   down to `MIN_OUTPUT_BUFFER`, where every element is split across a flush.
#[test]
fn a_float_array_is_bit_exact_at_every_buffer_size() {
    // A signalling NaN (quiet bit clear, non-zero payload), a quiet NaN, both
    // infinities, both zeros, the smallest subnormal and both extremes.
    let a32: [f32; 9] = [
        f32::from_bits(0x7F80_0001),
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        0.0,
        -0.0,
        f32::from_bits(1),
        f32::MIN,
        f32::MAX,
    ];
    let a64: [f64; 9] = [
        f64::from_bits(0x7FF0_0000_0000_0001),
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        0.0,
        -0.0,
        f64::from_bits(1),
        f64::MIN,
        f64::MAX,
    ];

    // `[ header id 0 = FIXLENARRAY ][ count ][ fixlen_word ]` then the payload.
    let mut expect32 = vec![0x05, 0x09, 0x20];
    for e in a32 {
        expect32.extend_from_slice(&e.to_le_bytes());
    }
    let mut expect64 = vec![0x05, 0x09, 0x41];
    for e in a64 {
        expect64.extend_from_slice(&e.to_le_bytes());
    }

    assert_eq!(encode(|os| os.write_array_fp32(0, &a32).unwrap()), expect32);
    assert_eq!(encode(|os| os.write_array_fp64(0, &a64).unwrap()), expect64);

    for size in [1usize, 2, 3, 5, 8, 16] {
        let mut out32: Vec<u8> = Vec::new();
        let mut buf = vec![0u8; size];
        {
            let mut os =
                OStream::with_flush(&mut buf, 0, |d: &[u8]| out32.extend_from_slice(d)).unwrap();
            os.write_array_fp32(0, &a32).unwrap();
            os.flush().unwrap();
        }
        assert_eq!(out32, expect32, "fp32 array through a {size}-byte buffer");

        let mut out64: Vec<u8> = Vec::new();
        let mut buf = vec![0u8; size];
        {
            let mut os =
                OStream::with_flush(&mut buf, 0, |d: &[u8]| out64.extend_from_slice(d)).unwrap();
            os.write_array_fp64(0, &a64).unwrap();
            os.flush().unwrap();
        }
        assert_eq!(out64, expect64, "fp64 array through a {size}-byte buffer");
    }
}

// --- sequences --------------------------------------------------------------

#[test]
fn write_nested_sequence() {
    let bytes = encode(|os| {
        os.write_unsigned(0, 42).unwrap();
        os.write_sequence_begin_lazy(1).unwrap();
        os.write_unsigned(0, 42).unwrap();
        os.write_signed(2, -42).unwrap();
        os.write_sequence_end().unwrap();
        os.write_signed(2, -42).unwrap();
    });
    assert_eq!(
        bytes,
        [0x00, 0x2A, 0x0E, 0x00, 0x2A, 0x11, 0x53, 0x07, 0x11, 0x53]
    );
}

#[test]
fn write_nested_sequence_with_array() {
    let bytes = encode(|os| {
        os.write_unsigned(0, 42).unwrap();
        os.write_sequence_begin_lazy(3).unwrap();
        os.write_unsigned(0, 42).unwrap();
        os.write_array_signed(3, &[-42_i32, -43, -44]).unwrap();
        os.write_sequence_end().unwrap();
        os.write_signed(2, -42).unwrap();
    });
    assert_eq!(
        bytes,
        [0x00, 0x2A, 0x1E, 0x00, 0x2A, 0x1C, 0x03, 0x53, 0x55, 0x57, 0x07, 0x11, 0x53]
    );
}

// --- lazy sequence framing (MESSAGE_SPEC §2) --------------------------------

/// An all-default sequence carries no information, so the field is omitted --
/// where the eager API would have written the two-byte empty frame `0E 07`.
#[test]
fn lazy_sequence_without_content_emits_nothing() {
    let bytes = encode(|os| {
        os.write_sequence_begin_lazy(1).unwrap();
        os.write_sequence_end().unwrap();
    });
    assert!(bytes.is_empty(), "got {bytes:02x?}");
}

/// `end_keep` forces a contentless frame onto the wire — the array element and
/// explicit-empty cases of §2/§5.1.
#[test]
fn end_keep_frames_a_contentless_sequence() {
    let bytes = encode(|os| {
        os.write_sequence_begin_lazy(1).unwrap();
        os.write_sequence_end_keep().unwrap();
    });
    assert_eq!(bytes, [0x0E, 0x07]);
}

/// Forcing a frame forces its ancestors too: the outer sequence got content (the
/// inner frame), so it is framed as well.
#[test]
fn end_keep_commits_the_enclosing_run() {
    let bytes = encode(|os| {
        os.write_sequence_begin_lazy(1).unwrap();
        os.write_sequence_begin_lazy(2).unwrap();
        os.write_sequence_end_keep().unwrap();
        os.write_sequence_end().unwrap();
    });
    assert_eq!(bytes, [0x0E, 0x16, 0x07, 0x07]);
}

/// With content it makes no difference — the headers are already out.
#[test]
fn end_keep_matches_end_once_content_exists() {
    let with_keep = encode(|os| {
        os.write_sequence_begin_lazy(1).unwrap();
        os.write_unsigned(0, 42).unwrap();
        os.write_sequence_end_keep().unwrap();
    });
    let with_end = encode(|os| {
        os.write_sequence_begin_lazy(1).unwrap();
        os.write_unsigned(0, 42).unwrap();
        os.write_sequence_end().unwrap();
    });
    assert_eq!(with_keep, [0x0E, 0x00, 0x2A, 0x07]);
    assert_eq!(with_keep, with_end);
}

/// One child field commits the whole held-back run, outermost header first, so a
/// non-default leaf deep inside brings every enclosing frame back in wire order.
#[test]
fn lazy_sequence_commits_the_whole_run_on_first_content() {
    let bytes = encode(|os| {
        os.write_sequence_begin_lazy(1).unwrap();
        os.write_sequence_begin_lazy(2).unwrap();
        os.write_unsigned(0, 42).unwrap();
        os.write_sequence_end().unwrap();
        os.write_sequence_end().unwrap();
    });
    assert_eq!(bytes, [0x0E, 0x16, 0x00, 0x2A, 0x07, 0x07]);
}

/// Only the empty inner sequence drops; the outer one has content (the leaf) and
/// is framed. This is the interleaving the naive "drop the whole run" would get
/// wrong.
#[test]
fn lazy_sequence_drops_only_the_empty_inner_one() {
    let bytes = encode(|os| {
        os.write_sequence_begin_lazy(1).unwrap();
        os.write_sequence_begin_lazy(2).unwrap();
        os.write_sequence_end().unwrap();
        os.write_unsigned(0, 42).unwrap();
        os.write_sequence_end().unwrap();
    });
    assert_eq!(bytes, [0x0E, 0x00, 0x2A, 0x07]);
}

/// A lazily framed sequence *after* content in the same scope, and the sibling
/// order, stay intact.
#[test]
fn lazy_sequence_after_content_is_independent() {
    let bytes = encode(|os| {
        os.write_unsigned(0, 1).unwrap();
        os.write_sequence_begin_lazy(1).unwrap();
        os.write_sequence_end().unwrap();
        os.write_unsigned(2, 3).unwrap();
    });
    assert_eq!(bytes, [0x00, 0x01, 0x10, 0x03]);
}

/// A run **committed across a flush boundary** produces exactly the one-shot
/// bytes, at every output-buffer size.
///
/// Note what this does *not* test, and why: a *buffer-full* flush cannot land
/// while a header is still held back. Held-back ids are encoder state and occupy
/// no buffer space, and the buffer only fills through a *write* — which commits
/// the whole run before its own first byte. What a tiny buffer does exercise is
/// the other half: the commit itself spilling across flushes (with a 1-byte
/// buffer the 3-header run flushes between every header), plus a run that is
/// dropped, re-grown and committed while the buffer keeps draining underneath
/// it.
///
/// A flush mid-run is reachable the *explicit* way, by calling
/// [`OStream::flush`] between two writes, and that case is covered by
/// `an_explicit_flush_mid_run_matches_one_shot` below rather than left to the
/// argument above.
#[test]
fn run_committed_across_flush_boundary_matches_one_shot() {
    fn script<'a, F: sofab::FlushTake<'a>>(os: &mut OStream<'a, F>) {
        os.write_sequence_begin_lazy(1).unwrap();
        os.write_sequence_begin_lazy(2).unwrap();
        os.write_sequence_begin_lazy(3).unwrap();
        os.write_sequence_end().unwrap(); // contentless: id 3 vanishes
        os.write_sequence_begin_lazy(4).unwrap();
        os.write_unsigned(0, 42).unwrap(); // commits the run 1, 2, 4
        os.write_sequence_end().unwrap();
        os.write_sequence_end().unwrap();
        os.write_sequence_end().unwrap();
    }

    // Not a redundant closure: `script` is generic over the buffer lifetime, and
    // passing it bare pins that lifetime to one instantiation while `encode`
    // wants a higher-ranked `FnOnce(&mut OStream<'_>)`. The closure is what makes
    // it higher-ranked again.
    #[allow(clippy::redundant_closure)]
    let one_shot = encode(|os| script(os));
    assert_eq!(one_shot, [0x0E, 0x16, 0x26, 0x00, 0x2A, 0x07, 0x07, 0x07]);

    for size in [1usize, 2, 3, 7] {
        let mut out: Vec<u8> = Vec::new();
        let mut buf = vec![0u8; size];
        {
            let mut os =
                sofab::OStream::with_flush(&mut buf, 0, |d: &[u8]| out.extend_from_slice(d))
                    .unwrap();
            script(&mut os);
            os.flush().unwrap();
        }
        assert_eq!(out, one_shot, "buffer size {size}");
    }
}

/// The reachable mid-run flush: the caller invokes [`OStream::flush`] itself
/// while headers are still held back. A pending run is encoder state, not buffer
/// content, so the flush drains what is in the buffer and the run is untouched —
/// the bytes are the one-shot bytes, with the drained prefix landing in the sink
/// early. Committing the run *after* the flush also proves the commit does not
/// depend on anything the flush reset.
#[test]
fn an_explicit_flush_mid_run_matches_one_shot() {
    // The same script with no flush in it, for the reference bytes.
    let one_shot = encode(|os| {
        os.write_unsigned(9, 1).unwrap();
        os.write_sequence_begin_lazy(1).unwrap();
        os.write_sequence_begin_lazy(2).unwrap();
        os.write_unsigned(0, 42).unwrap();
        os.write_sequence_end().unwrap();
        os.write_sequence_end().unwrap();
    });
    assert_eq!(one_shot, [0x48, 0x01, 0x0E, 0x16, 0x00, 0x2A, 0x07, 0x07]);

    let mut out: Vec<u8> = Vec::new();
    let mut buf = [0u8; 64];
    let flushed_at = {
        let mut os = OStream::with_flush(&mut buf, 0, |d: &[u8]| out.extend_from_slice(d)).unwrap();
        os.write_unsigned(9, 1).unwrap();
        os.write_sequence_begin_lazy(1).unwrap();
        os.write_sequence_begin_lazy(2).unwrap();
        let n = os.flush().unwrap();
        os.write_unsigned(0, 42).unwrap();
        os.write_sequence_end().unwrap();
        os.write_sequence_end().unwrap();
        os.flush().unwrap();
        n
    };
    // The flush saw only the leaf written before the run — held-back headers
    // occupy no buffer space.
    assert_eq!(flushed_at, 2);
    assert_eq!(out, one_shot);
}

/// A commit that runs out of buffer keeps the headers it could not write. With
/// no sink, `BufferFull` stops the run half-written; the sequences still pending
/// are the innermost ones, so installing a fresh buffer and retrying the same
/// write finishes the run and the two buffers concatenate to exactly the
/// one-shot bytes. (Zeroing the whole run before writing it, as an earlier draft
/// did, silently dropped those frames instead.)
///
/// Scope, precisely: the ids here are 1..=4, so every header is a single byte
/// and the cut necessarily falls *between* headers. Retrying is byte-exact only
/// under that precondition — a cut inside a multi-byte header is not
/// recoverable, which is what
/// `recovery_after_a_cut_is_exact_only_on_a_header_boundary` pins.
#[test]
fn a_commit_cut_short_on_a_header_boundary_keeps_the_rest_pending() {
    fn script<'a, F: sofab::FlushTake<'a>>(os: &mut OStream<'a, F>) -> Vec<Result<(), Error>> {
        let mut r = Vec::new();
        for id in 1..=4 {
            r.push(os.write_sequence_begin_lazy(id));
        }
        r.push(os.write_unsigned(0, 42));
        for _ in 0..4 {
            r.push(os.write_sequence_end());
        }
        r
    }

    // Reference: the same script with room to spare.
    let mut big = [0u8; 64];
    let one_shot = {
        let mut os = OStream::new(&mut big);
        assert!(script(&mut os).iter().all(Result::is_ok));
        os.bytes_used()
    };
    let one_shot = big[..one_shot].to_vec();
    assert_eq!(
        one_shot,
        [0x0E, 0x16, 0x1E, 0x26, 0x00, 0x2A, 0x07, 0x07, 0x07, 0x07]
    );

    // Two bytes of room: the run of four headers stops after two.
    let mut small = [0u8; 2];
    let mut rest = [0u8; 64];
    let (first, second) = {
        let mut os = OStream::new(&mut small);
        for id in 1..=4 {
            os.write_sequence_begin_lazy(id).unwrap();
        }
        assert_eq!(os.write_unsigned(0, 42), Err(Error::BufferFull));
        let first = os.bytes_used();

        // Recover: a fresh buffer, and re-issue exactly the failed write. The
        // two headers that never made it are still pending, so they lead.
        os.buffer_set(&mut rest, 0).unwrap();
        os.write_unsigned(0, 42).unwrap();
        for _ in 0..4 {
            os.write_sequence_end().unwrap();
        }
        (first, os.bytes_used())
    };

    let mut streamed = small[..first].to_vec();
    streamed.extend_from_slice(&rest[..second]);
    assert_eq!(streamed, one_shot);
}

/// Where the recovery above stops. Every other cut-short test uses ids below
/// 16, whose headers are one byte, so no cut can slice a header in half — and
/// the recovery then looks unconditional. It is not: no writer here is atomic on
/// failure, so a cut *inside* a header's varint leaves that prefix in the buffer
/// while the whole header stays pending, and the retry writes it again.
///
/// Ids 16..=27 need a two-byte header (id 16 → tag `0x86 0x01`), so the sweep
/// below hits both cases. Even cut points land between headers and recover
/// byte-exactly; odd ones tear a header and the reassembled stream is corrupt —
/// e.g. cut 1 leaves a stray `0x86` in front of the retried `0x86 0x01`, and
/// `86 86 01` decodes as sequence id 2144 instead of 16.
///
/// Both directions are asserted, so the day a writer *does* become atomic on
/// failure this test fails and says so, rather than silently over-promising.
#[test]
fn recovery_after_a_cut_is_exact_only_on_a_header_boundary() {
    const DEPTH: u32 = 12;
    const FIRST: u32 = 16; // 16 << 3 | 6 = 0x86 0x01 — two bytes, unlike 1..=15
    const RUN_BYTES: usize = 2 * DEPTH as usize;

    fn open_all<'a, F: sofab::FlushTake<'a>>(os: &mut OStream<'a, F>) {
        for id in FIRST..FIRST + DEPTH {
            os.write_sequence_begin_lazy(id).unwrap();
        }
    }

    // Reference: the same script with room to spare.
    let mut big = [0u8; 128];
    let one_shot = {
        let mut os = OStream::new(&mut big);
        open_all(&mut os);
        os.write_unsigned(0, 42).unwrap();
        for _ in 0..DEPTH {
            os.write_sequence_end().unwrap();
        }
        os.bytes_used()
    };
    let one_shot = big[..one_shot].to_vec();
    // 12 two-byte headers + the 2-byte leaf + 12 end markers.
    assert_eq!(one_shot.len(), RUN_BYTES + 2 + DEPTH as usize);

    for cut in 1..=RUN_BYTES {
        let mut small = vec![0u8; cut];
        let mut rest = vec![0u8; 128];
        let (first, second) = {
            let mut os = OStream::new(&mut small);
            open_all(&mut os);
            assert_eq!(
                os.write_unsigned(0, 42),
                Err(Error::BufferFull),
                "cut {cut}"
            );
            let first = os.bytes_used();
            assert_eq!(first, cut, "cut {cut}: the buffer should be filled exactly");

            // Recover exactly as the doc on `commit_pending` describes: fresh
            // buffer, re-issue the failed write.
            os.buffer_set(&mut rest, 0).unwrap();
            os.write_unsigned(0, 42).unwrap();
            for _ in 0..DEPTH {
                os.write_sequence_end().unwrap();
            }
            (first, os.bytes_used())
        };

        let mut streamed = small[..first].to_vec();
        streamed.extend_from_slice(&rest[..second]);

        if cut % 2 == 0 {
            assert_eq!(streamed, one_shot, "cut {cut} fell on a header boundary");
        } else {
            assert_ne!(
                streamed, one_shot,
                "cut {cut} sliced a header varint: recovery cannot be exact"
            );
            // Precisely: the torn header's leading byte is left behind and the
            // whole header written again, so the stream is one byte longer.
            assert_eq!(
                streamed.len(),
                one_shot.len() + 1,
                "cut {cut}: expected exactly the re-written prefix byte"
            );
            if cut == 1 {
                // And the wreckage is well-formed, which is what makes it
                // dangerous: `86 86 01` is a valid header — sequence id 2144
                // ((0x86,0x86,0x01) = 17158; 17158 >> 3 = 2144, type 6) — where
                // id 16's `86 01` was meant.
                assert_eq!(&streamed[..3], &[0x86, 0x86, 0x01]);
            }
        }
    }
}

/// The same recovery, but with the run split across the encoder's inline slots
/// and its heap spill (12 levels > the 8 that stay inline), and with a *further*
/// sequence opened while the run is half-committed. The ids must still reach the
/// wire outermost-first, in one order, however the storage is split.
#[test]
fn a_cut_short_commit_keeps_its_order_across_the_spill_boundary() {
    const DEPTH: u32 = 12;
    const EXTRA: u32 = 13;

    // Reference: open 12, open one more, then content, then close all 13.
    let mut big = [0u8; 128];
    let one_shot = {
        let mut os = OStream::new(&mut big);
        for id in 1..=DEPTH {
            os.write_sequence_begin_lazy(id).unwrap();
        }
        os.write_sequence_begin_lazy(EXTRA).unwrap();
        os.write_unsigned(0, 42).unwrap();
        for _ in 0..=DEPTH {
            os.write_sequence_end().unwrap();
        }
        os.bytes_used()
    };
    let one_shot = big[..one_shot].to_vec();

    // Three bytes of room: the commit stops after three headers, leaving five
    // inline and four spilled — then id 13 is opened on top of that remainder.
    let mut small = [0u8; 3];
    let mut rest = [0u8; 128];
    let (first, second) = {
        let mut os = OStream::new(&mut small);
        for id in 1..=DEPTH {
            os.write_sequence_begin_lazy(id).unwrap();
        }
        assert_eq!(os.write_unsigned(0, 42), Err(Error::BufferFull));
        let first = os.bytes_used();
        assert_eq!(first, 3, "expected three single-byte headers to fit");

        os.write_sequence_begin_lazy(EXTRA).unwrap();
        os.buffer_set(&mut rest, 0).unwrap();
        os.write_unsigned(0, 42).unwrap();
        for _ in 0..=DEPTH {
            os.write_sequence_end().unwrap();
        }
        (first, os.bytes_used())
    };

    let mut streamed = small[..first].to_vec();
    streamed.extend_from_slice(&rest[..second]);
    assert_eq!(streamed, one_shot);
}

/// "Is anything held back?" must consult **both** halves of the run's storage,
/// and the cut point that proves it is the inline/spill split itself.
///
/// The test above cuts the commit at three bytes, which leaves five ids inline
/// and four spilled — `n != 0`, so answering from `n` alone still happens to be
/// right and the bug hides. Cut instead at exactly `INLINE_PENDING` (8) of the 12
/// headers and the surviving remainder is *entirely* in the heap spill: `n` is
/// back to zero while `spill` still holds four ids. An encoder that reads
/// emptiness off `n` then concludes the run is finished, so those four sequence
/// headers never reach the wire — and later four of the `end` markers pair with
/// them and vanish too, shortening the message from 26 bytes to 18 and changing
/// the nesting structure a decoder sees.
///
/// Swept over every cut point rather than pinned to 8, so the split cannot drift
/// out from under the test if `INLINE_PENDING` changes.
#[test]
fn a_cut_short_commit_knows_it_is_pending_at_every_cut_point() {
    const DEPTH: u32 = 12; // 8 inline + 4 spilled

    // Reference: the same script with room to spare.
    let mut big = [0u8; 128];
    let one_shot = {
        let mut os = OStream::new(&mut big);
        for id in 1..=DEPTH {
            os.write_sequence_begin_lazy(id).unwrap();
        }
        os.write_unsigned(0, 42).unwrap();
        for _ in 0..DEPTH {
            os.write_sequence_end().unwrap();
        }
        os.bytes_used()
    };
    let one_shot = big[..one_shot].to_vec();
    // 12 single-byte headers + the 2-byte leaf + 12 end markers.
    assert_eq!(one_shot.len(), 26);

    for cut in 1..=DEPTH as usize {
        let mut small = vec![0u8; cut];
        let mut rest = vec![0u8; 128];
        let (first, second) = {
            let mut os = OStream::new(&mut small);
            for id in 1..=DEPTH {
                os.write_sequence_begin_lazy(id).unwrap();
            }
            // The run needs `DEPTH` bytes and the leaf one more, so every cut in
            // 1..=DEPTH stops the write short.
            assert_eq!(
                os.write_unsigned(0, 42),
                Err(Error::BufferFull),
                "cut {cut}"
            );
            let first = os.bytes_used();
            assert_eq!(first, cut, "cut {cut}: expected {cut} headers to fit");

            // Recover into a fresh buffer and re-issue the failed write. Whatever
            // is left of the run — inline, spilled, or purely spilled — must lead.
            os.buffer_set(&mut rest, 0).unwrap();
            os.write_unsigned(0, 42).unwrap();
            for _ in 0..DEPTH {
                os.write_sequence_end().unwrap();
            }
            (first, os.bytes_used())
        };

        let mut streamed = small[..first].to_vec();
        streamed.extend_from_slice(&rest[..second]);
        assert_eq!(streamed, one_shot, "cut {cut} lost part of the run");
    }
}

/// Nesting far deeper than the 32-level hold-back window this port used to have:
/// all 40 frames are contentless, so the message is **zero bytes**. The eager
/// fallback beyond the old window got exactly this wrong — levels 33..40 kept
/// the empty `begin`+`end` pair §2 omits. There is no window any more (this
/// crate has a heap, so CORELIB_PLAN §6 requires holding back to `MAX_DEPTH`),
/// and the ceiling itself is canonical too.
#[test]
fn contentless_sequences_vanish_at_any_depth() {
    for depth in [40u32, sofab::MAX_DEPTH] {
        let mut buf = vec![0u8; 4 * depth as usize];
        let mut os = OStream::new(&mut buf);
        for _ in 0..depth {
            os.write_sequence_begin_lazy(1).unwrap();
        }
        for _ in 0..depth {
            os.write_sequence_end().unwrap();
        }
        assert_eq!(os.bytes_used(), 0, "depth {depth} left bytes behind");
    }
}

/// The mirror image: content at the bottom of a 40-deep run brings back every
/// enclosing header, outermost first and in id order — the run is not truncated
/// at any window.
#[test]
fn deep_run_commits_every_level_outermost_first() {
    const DEPTH: u32 = 40;
    let mut buf = vec![0u8; 256];
    let used = {
        let mut os = OStream::new(&mut buf);
        for id in 0..DEPTH {
            os.write_sequence_begin_lazy(id).unwrap();
        }
        os.write_unsigned(0, 42).unwrap();
        for _ in 0..DEPTH {
            os.write_sequence_end().unwrap();
        }
        os.bytes_used()
    };

    let mut expect: Vec<u8> = Vec::new();
    for id in 0..DEPTH {
        common::push_varint(&mut expect, ((id as u64) << 3) | 0x6);
    }
    expect.extend_from_slice(&[0x00, 0x2A]); // the leaf
    expect.extend(std::iter::repeat(0x07).take(DEPTH as usize));
    assert_eq!(&buf[..used], &expect[..]);
}

/// Both closers must give their depth back. `MAX_DEPTH` bounds how many
/// sequences are open *at once*, not how many a message may contain, so opening
/// and closing the full ceiling several times over — contentlessly, which is the
/// shape §2 creates — must keep encoding.
///
/// Scope, precisely: every `end` round below closes its frames **contentlessly**,
/// so `write_sequence_end` always finds a held-back header to pop and takes its
/// *drop* path. That is one of the two depth-decrement sites in that function;
/// this test pins that one, plus `end_keep`'s. The other site — the *emit* path,
/// reached when the sequence had content and there is no pending header left to
/// pop — is unreachable from here and belongs to
/// `content_bearing_end_gives_the_depth_back` below. (An earlier commit message
/// claimed "deleting the depth decrement from either closer fails it"; that
/// holds for the drop site and for `end_keep`, and is false for the emit site,
/// which this test cannot reach.)
#[test]
fn both_closers_give_the_depth_back() {
    let mut buf = vec![0u8; 8192];
    let mut os = OStream::new(&mut buf);

    // Rounds closed with `end`: every frame vanishes, so nothing is written.
    for _ in 0..4 {
        for _ in 0..sofab::MAX_DEPTH {
            os.write_sequence_begin_lazy(1).unwrap();
        }
        for _ in 0..sofab::MAX_DEPTH {
            os.write_sequence_end().unwrap();
        }
    }
    assert_eq!(os.bytes_used(), 0);

    // Rounds closed with `end_keep`: every frame reaches the wire, and the
    // depth still comes back.
    for _ in 0..4 {
        for _ in 0..sofab::MAX_DEPTH {
            os.write_sequence_begin_lazy(1).unwrap();
        }
        for _ in 0..sofab::MAX_DEPTH {
            os.write_sequence_end_keep().unwrap();
        }
    }
    let kept = 4 * 2 * sofab::MAX_DEPTH as usize; // one begin + one end byte each
    assert_eq!(os.bytes_used(), kept);

    // The ceiling is intact, not merely un-hit: still exactly MAX_DEPTH free.
    for _ in 0..sofab::MAX_DEPTH {
        os.write_sequence_begin_lazy(1).unwrap();
    }
    assert_eq!(os.write_sequence_begin_lazy(1), Err(Error::Argument));
}

/// The **content-bearing** `end` must give its depth back too — the other half of
/// the test above, and the busiest path in the encoder: a sequence that received
/// content has no held-back header left to pop, so `write_sequence_end` falls
/// through to the *emit* path, writes the end marker, and decrements there.
///
/// Miss that decrement and the depth ratchets up by one per closed sequence
/// while at most one is ever open, so the counter reaches `MAX_DEPTH` after 255
/// sequences and the 256th `begin` is falsely rejected with `Error::Argument` —
/// on a message that nests exactly one level deep. Both shapes below fail that
/// way: the flat one at round 255, the nested one at the second round's first
/// `begin`.
#[test]
fn content_bearing_end_gives_the_depth_back() {
    // Flat: a thousand sequences opened and closed one after another, each with
    // a child, so never more than one is open at a time. Far past MAX_DEPTH
    // rounds — the point being that rounds are not a depth.
    const ROUNDS: u32 = 1000;
    let mut buf = vec![0u8; 8192];
    let mut os = OStream::new(&mut buf);
    for round in 0..ROUNDS {
        assert_eq!(
            os.write_sequence_begin_lazy(1),
            Ok(()),
            "flat round {round} rejected: the depth was not given back"
        );
        os.write_unsigned(0, 42).unwrap();
        os.write_sequence_end().unwrap();
    }
    // Each round: held-back header committed by the child (0x0E), the child
    // (0x00 0x2A), the end marker (0x07).
    assert_eq!(os.bytes_used(), 4 * ROUNDS as usize);

    // The ceiling is intact, not merely un-hit: still exactly MAX_DEPTH free.
    for _ in 0..sofab::MAX_DEPTH {
        os.write_sequence_begin_lazy(1).unwrap();
    }
    assert_eq!(os.write_sequence_begin_lazy(1), Err(Error::Argument));

    // Nested: the full ceiling several times over, with content at the leaf so
    // every one of the 255 `end`s takes the emit path rather than the drop path.
    let mut buf = vec![0u8; 8192];
    let mut os = OStream::new(&mut buf);
    for round in 0..4 {
        for level in 0..sofab::MAX_DEPTH {
            assert_eq!(
                os.write_sequence_begin_lazy(1),
                Ok(()),
                "nested round {round} level {level} rejected"
            );
        }
        os.write_unsigned(0, 42).unwrap(); // commits all 255 held-back headers
        for _ in 0..sofab::MAX_DEPTH {
            os.write_sequence_end().unwrap();
        }
    }
    // Per round: 255 committed begin bytes + the 2-byte leaf + 255 end bytes.
    let per_round = 2 * sofab::MAX_DEPTH as usize + 2;
    assert_eq!(os.bytes_used(), 4 * per_round);
}

// --- the sequence-end marker ------------------------------------------------

/// A sequence end is matched positionally against the innermost open start
/// (CORELIB_PLAN §4.9), so the marker carries **no id**: whatever id the
/// sequence was opened with — up to and including [`ID_MAX`], whose start header
/// is five bytes wide — the closer is the bare canonical `0x07`, on both closers.
///
/// The marker writer takes no id parameter at all, which is what makes that
/// true by construction; this pins the wire side of it, so a closer that started
/// echoing the sequence's id shows up here as an encoder failure rather than as
/// a decoder-tolerance surprise on the far end.
#[test]
fn every_sequence_end_marker_is_the_bare_canonical_byte() {
    // (id, its `(id << 3) | T_SEQUENCE_START` header as a varint)
    let cases: &[(u32, &[u8])] = &[
        (0, &[0x06]),
        (1, &[0x0E]),
        (15, &[0x7E]),
        (16, &[0x86, 0x01]),
        (2047, &[0xFE, 0x7F]),
        (ID_MAX, &[0xFE, 0xFF, 0xFF, 0xFF, 0x3F]),
    ];

    for &(id, header) in cases {
        let with_content = encode(|os| {
            os.write_sequence_begin_lazy(id).unwrap();
            os.write_unsigned(0, 42).unwrap();
            os.write_sequence_end().unwrap();
        });
        let mut expected = header.to_vec();
        expected.extend_from_slice(&[0x00, 0x2A, 0x07]);
        assert_eq!(with_content, expected, "sequence id {id}");

        let empty_kept = encode(|os| {
            os.write_sequence_begin_lazy(id).unwrap();
            os.write_sequence_end_keep().unwrap();
        });
        let mut expected = header.to_vec();
        expected.push(0x07);
        assert_eq!(empty_kept, expected, "sequence id {id}, kept");
    }
}

/// The marker is a write like any other and its one failure mode is running out
/// of buffer. Without a sink there is nowhere to drain to, so both closers report
/// [`Error::BufferFull`] — the only error path the marker writer has, now that
/// the id bound and the hold-back commit (neither of which a sequence end can
/// ever reach) are gone from it.
///
/// The second half also pins the documented non-atomicity: `end_keep` commits
/// the held-back header first, so the header is on the wire even though the
/// marker that should follow it never made it.
#[test]
fn a_sequence_end_marker_that_does_not_fit_is_buffer_full() {
    // Three bytes hold exactly the committed header, the field and its value.
    let mut buf = [0u8; 3];
    let mut os = OStream::new(&mut buf);
    os.write_sequence_begin_lazy(1).unwrap();
    os.write_unsigned(0, 42).unwrap();
    assert_eq!(os.write_sequence_end(), Err(Error::BufferFull));
    assert_eq!(os.bytes_used(), 3);
    drop(os);
    assert_eq!(buf, [0x0E, 0x00, 0x2A]);

    // One byte holds the header `end_keep` commits, and nothing more.
    let mut buf = [0u8; 1];
    let mut os = OStream::new(&mut buf);
    os.write_sequence_begin_lazy(1).unwrap();
    assert_eq!(os.write_sequence_end_keep(), Err(Error::BufferFull));
    assert_eq!(os.bytes_used(), 1);
    drop(os);
    assert_eq!(buf, [0x0E]);
}

// --- error / overflow behavior ---------------------------------------------

#[test]
fn buffer_full_without_sink() {
    let mut buf = [0u8; 2];
    let mut os = OStream::new(&mut buf);
    assert_eq!(os.write_unsigned(0, u64::MAX), Err(Error::BufferFull));
}

#[test]
fn zero_count_arrays_encode_to_header_plus_count() {
    // A zero-count integer array is exactly [ header ][ count = 0 ] (§4.7). A
    // zero-count fixlen array still writes its fixlen_word (but no payload), so
    // an empty fp32 array is distinguishable from an empty fp64 array (§4.8).
    let empty_u: [u32; 0] = [];
    assert_eq!(
        encode(|os| os.write_array_unsigned(0, &empty_u).unwrap()),
        [0x03, 0x00]
    );
    let empty_i: [i32; 0] = [];
    assert_eq!(
        encode(|os| os.write_array_signed(0, &empty_i).unwrap()),
        [0x04, 0x00]
    );
    let empty_f32: [f32; 0] = [];
    assert_eq!(
        encode(|os| os.write_array_fp32(0, &empty_f32).unwrap()),
        [0x05, 0x00, 0x20]
    );
    let empty_f64: [f64; 0] = [];
    assert_eq!(
        encode(|os| os.write_array_fp64(0, &empty_f64).unwrap()),
        [0x05, 0x00, 0x41]
    );
}

/// `write_sequence_begin_lazy` does its own id check — its header is not written
/// by a field writer at all but held back and later emitted by `commit_pending`,
/// which bounds nothing, so the rejection cannot be inherited, and
/// `id_overflow_is_argument_error` above only covers `write_unsigned`.
///
/// Delete the check and the call returns `Ok`, the id joins the pending run, and
/// the next field write commits an out-of-range tag onto the wire — silently,
/// long after the offending call returned.
#[test]
fn sequence_id_overflow_is_argument_error() {
    let mut buf = [0u8; 32];
    let mut os = OStream::new(&mut buf);
    assert_eq!(
        os.write_sequence_begin_lazy(ID_MAX + 1),
        Err(Error::Argument)
    );

    // Rejected means *not held back*: the following field commits nothing but
    // itself, so the bad id cannot surface on the wire later.
    os.write_unsigned(0, 42).unwrap();
    let used = os.bytes_used();
    assert_eq!(&buf[..used], &[0x00, 0x2A]);

    // ID_MAX itself is legal, and frames as the five-byte tag (ID_MAX << 3) | 6.
    assert_eq!(
        encode(|os| {
            os.write_sequence_begin_lazy(ID_MAX).unwrap();
            os.write_unsigned(0, 42).unwrap();
            os.write_sequence_end().unwrap();
        }),
        [0xFE, 0xFF, 0xFF, 0xFF, 0x3F, 0x00, 0x2A, 0x07]
    );
}

#[test]
fn sequence_depth_over_max_is_argument_error() {
    let mut buf = [0u8; 512];
    let mut os = OStream::new(&mut buf);
    // 255 nested sequences are allowed; the 256th must be rejected (§4.9).
    for _ in 0..sofab::MAX_DEPTH {
        os.write_sequence_begin_lazy(0).unwrap();
    }
    assert_eq!(os.write_sequence_begin_lazy(0), Err(Error::Argument));
    // After closing one, opening one more is allowed again.
    os.write_sequence_end().unwrap();
    os.write_sequence_begin_lazy(0).unwrap();
}

/// Closing a sequence that was never opened is a caller mistake, not output.
///
/// A lone `0x07` is a sequence-end marker with no open sequence — CORELIB_PLAN
/// §5.2 lists it among the byte sequences that are malformed *regardless of what
/// follows*, and this port's own decoder rejects it (`IStream::feed(&[0x07])` →
/// `Error::InvalidMsg`, `dangling_sequence_end_is_invalid`). An encoder that
/// emits it and answers `Ok(())` therefore reports success for bytes every
/// conformant decoder in the family must refuse. §6.3 leaves exactly one code
/// for such a mistake — `InvalidArgument` — which is what the encoder already
/// returns for the other two structural arguments it checks (`id > ID_MAX`,
/// `depth >= MAX_DEPTH`).
///
/// Both closers are pinned, at the top level and after a balanced frame, and in
/// both cases the rejection must be total: not one byte reaches the buffer, and
/// the encoder's depth accounting is left as it was — the guard is what makes
/// the decrement's underflow unreachable, so the closers may subtract plainly
/// instead of hiding the mistake behind a `saturating_sub`. The last block
/// therefore reopens the full `MAX_DEPTH` ceiling after two rejected closes:
/// neither may have spent a level the caller never opened.
#[test]
fn unbalanced_sequence_end_is_argument_error() {
    fn close(os: &mut OStream, keep: bool) -> Result<(), Error> {
        if keep {
            os.write_sequence_end_keep()
        } else {
            os.write_sequence_end()
        }
    }

    for keep in [false, true] {
        // At the top level, with nothing ever opened.
        let mut buf = [0u8; 16];
        let mut os = OStream::new(&mut buf);
        assert_eq!(close(&mut os, keep), Err(Error::Argument));
        assert_eq!(os.bytes_used(), 0, "a rejected close still wrote bytes");

        // One frame too many after a balanced one, both with and without
        // content in it (the drop path and the emit path of `end`).
        for content in [false, true] {
            let mut buf = [0u8; 16];
            let mut os = OStream::new(&mut buf);
            os.write_sequence_begin_lazy(1).unwrap();
            if content {
                os.write_unsigned(0, 42).unwrap();
            }
            os.write_sequence_end().unwrap();
            let balanced = os.bytes_used();
            assert_eq!(close(&mut os, keep), Err(Error::Argument));
            assert_eq!(
                os.bytes_used(),
                balanced,
                "a rejected close still wrote bytes"
            );
        }
    }

    // The depth counter survived: a rejected close must not have consumed a
    // level, so the full ceiling is still available afterwards.
    let mut buf = vec![0u8; 1024];
    let mut os = OStream::new(&mut buf);
    assert_eq!(os.write_sequence_end(), Err(Error::Argument));
    assert_eq!(os.write_sequence_end_keep(), Err(Error::Argument));
    for _ in 0..sofab::MAX_DEPTH {
        os.write_sequence_begin_lazy(1).unwrap();
    }
    assert_eq!(os.write_sequence_begin_lazy(1), Err(Error::Argument));
}

// --- streaming flush sink ---------------------------------------------------

#[test]
fn flush_sink_streams_large_message() {
    // A 4-byte buffer cannot hold the whole message; the flush sink must
    // receive the overflow so the full byte stream is reconstructed.
    let mut collected: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4];
    {
        let mut os = OStream::with_flush(&mut buf, 0, |chunk: &[u8]| {
            collected.extend_from_slice(chunk);
        })
        .unwrap();
        for i in 0..10u32 {
            os.write_unsigned(i, i as u64).unwrap();
        }
        os.flush().unwrap();
    }

    // Reference: the same writes into one large buffer.
    let reference = encode(|os| {
        for i in 0..10u32 {
            os.write_unsigned(i, i as u64).unwrap();
        }
    });
    assert_eq!(collected, reference);
}
