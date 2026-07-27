//! Encoder tests. Every `expected` byte array is taken verbatim from the C
//! reference suite `test/c/test_ostream.c`.

// Float test vectors are deliberately the literals used by the C suite.
#![allow(clippy::approx_constant, clippy::excessive_precision)]

mod common;

use sofab::{Error, OStream, ID_MAX};

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
/// Note what this does *not* test, because it cannot: a flush landing while a
/// header is still held back is unreachable by construction, not merely
/// untested. Held-back ids are encoder state and occupy no buffer space, and the
/// buffer only fills through a *write* — which commits the whole run before its
/// own first byte. So a pending run can never straddle a flush. What a tiny
/// buffer does exercise is the other half: the commit itself spilling across
/// flushes (with a 1-byte buffer the 3-header run flushes between every header),
/// plus a run that is dropped, re-grown and committed while the buffer keeps
/// draining underneath it.
#[test]
fn run_committed_across_flush_boundary_matches_one_shot() {
    fn script<F: sofab::Flush>(os: &mut OStream<F>) {
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

    let one_shot = encode(script);
    assert_eq!(one_shot, [0x0E, 0x16, 0x26, 0x00, 0x2A, 0x07, 0x07, 0x07]);

    for size in [1usize, 2, 3, 7] {
        let mut out: Vec<u8> = Vec::new();
        let mut buf = vec![0u8; size];
        {
            let mut os =
                sofab::OStream::with_flush(&mut buf, 0, |d: &[u8]| out.extend_from_slice(d));
            script(&mut os);
            os.flush();
        }
        assert_eq!(out, one_shot, "buffer size {size}");
    }
}

/// A commit that runs out of buffer keeps the headers it could not write. With
/// no sink, `BufferFull` stops the run half-written; the sequences still pending
/// are the innermost ones, so installing a fresh buffer and retrying the same
/// write finishes the run and the two buffers concatenate to exactly the
/// one-shot bytes. (Zeroing the whole run before writing it, as an earlier draft
/// did, silently dropped those frames instead.)
#[test]
fn a_commit_cut_short_by_buffer_full_keeps_the_rest_pending() {
    fn script<F: sofab::Flush>(os: &mut OStream<F>) -> Vec<Result<(), Error>> {
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
        os.buffer_set(&mut rest, 0);
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
        os.buffer_set(&mut rest, 0);
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
/// shape §2 creates — must keep encoding. Drop the decrement from either closer
/// and the second round's first `begin` is falsely rejected.
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

#[test]
fn sequence_depth_over_max_is_argument_error() {
    let mut buf = [0u8; 512];
    let mut os = OStream::new(&mut buf);
    // 255 nested sequences are allowed; the 256th must be rejected (§4.9).
    for _ in 0..sofab::MAX_DEPTH {
        os.write_sequence_begin_lazy(0).unwrap();
    }
    assert_eq!(os.write_sequence_begin_lazy(0), Err(Error::Argument));
    // The empty-frame call is bounded by the same ceiling.
    // After closing one, opening one more is allowed again.
    os.write_sequence_end().unwrap();
    os.write_sequence_begin_lazy(0).unwrap();
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
        });
        for i in 0..10u32 {
            os.write_unsigned(i, i as u64).unwrap();
        }
        os.flush();
    }

    // Reference: the same writes into one large buffer.
    let reference = encode(|os| {
        for i in 0..10u32 {
            os.write_unsigned(i, i as u64).unwrap();
        }
    });
    assert_eq!(collected, reference);
}
