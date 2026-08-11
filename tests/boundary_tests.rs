//! Buffer- and chunk-boundary tests.
//!
//! Both codecs have a fast path that runs while a whole item is known to fit —
//! the encoder reserves a full varint's worth of room and writes without
//! per-byte checks, the decoder reads while a full varint is known to be
//! readable — and a slower path that takes over near the end of the buffer,
//! where an item may be split across a flush or a chunk.
//!
//! The split paths are where a divergence would hide, so these tests pin the
//! property that matters: **the bytes and the events do not depend on how the
//! buffer or the stream is cut up** (CORELIB_PLAN §5.1, §5.2, §7.2).

// The float payloads are fixed byte patterns from the cross-language suite,
// deliberately not `std::f32::consts::PI`.
#![allow(clippy::approx_constant)]

mod common;

use common::{Event, Recorder};
use sofab::{Error, IStream, OStream};

/// Write one field of every kind — scalars, floats, a string, a blob, all three
/// array flavours and a nested sequence — so a single pass exercises every
/// writer and every decoder resume state.
fn write_everything<'a, F: sofab::FlushTake<'a>>(os: &mut OStream<'a, F>) {
    os.write_unsigned(1, 0xDEAD_BEEF).unwrap();
    os.write_unsigned(2, u64::MAX).unwrap(); // 10-byte varint
    os.write_signed(3, -12345).unwrap();
    os.write_signed(4, i64::MIN).unwrap();
    os.write_boolean(5, true).unwrap();
    os.write_fp32(6, 3.14159).unwrap();
    os.write_fp64(7, 2.718281828459045).unwrap();
    os.write_str(8, "sofa-buffers").unwrap();
    os.write_blob(9, &[0u8, 1, 2, 253, 254, 255]).unwrap();
    os.write_array_unsigned(10, &[1u64, 1 << 20, u64::MAX, 0])
        .unwrap();
    os.write_array_signed(11, &[-1i64, i64::MIN, 0, 7]).unwrap();
    os.write_array_fp32(12, &[1.5f32, -0.0, f32::MAX]).unwrap();
    os.write_array_fp64(13, &[3.14159265f64, -2.5]).unwrap();
    os.write_sequence_begin_lazy(14).unwrap();
    os.write_unsigned(1, 99).unwrap();
    os.write_signed(2, -7).unwrap();
    os.write_sequence_end().unwrap();
}

/// The reference bytes: everything written into a buffer that always has room.
fn one_shot() -> Vec<u8> {
    let mut buf = vec![0u8; 512];
    let used = {
        let mut os = OStream::new(&mut buf);
        write_everything(&mut os);
        os.bytes_used()
    };
    buf.truncate(used);
    buf
}

// --- encoder: a buffer smaller than the message ------------------------------

#[test]
fn every_buffer_size_produces_the_one_shot_bytes() {
    let reference = one_shot();

    // Size 1 is the extreme: every single byte forces a flush, so no write can
    // take the reserved-run fast path.
    for size in 1..=(reference.len() + 4) {
        let mut collected: Vec<u8> = Vec::new();
        let mut buf = vec![0u8; size];
        {
            let mut os = OStream::with_flush(&mut buf, 0, |chunk: &[u8]| {
                collected.extend_from_slice(chunk);
            })
            .unwrap();
            write_everything(&mut os);
            os.flush().unwrap();
        }
        assert_eq!(
            collected, reference,
            "encoding diverged with a {size}-byte buffer"
        );
    }
}

#[test]
fn a_buffer_one_byte_short_reports_buffer_full() {
    let reference = one_shot();
    let mut buf = vec![0u8; reference.len() - 1];
    let mut os = OStream::new(&mut buf);
    // No sink, so the write that crosses the end must fail rather than flush.
    let err = (|| {
        os.write_unsigned(1, 0xDEAD_BEEF)?;
        os.write_unsigned(2, u64::MAX)?;
        os.write_signed(3, -12345)?;
        os.write_signed(4, i64::MIN)?;
        os.write_boolean(5, true)?;
        os.write_fp32(6, 3.14159)?;
        os.write_fp64(7, 2.718281828459045)?;
        os.write_str(8, "sofa-buffers")?;
        os.write_blob(9, &[0u8, 1, 2, 253, 254, 255])?;
        os.write_array_unsigned(10, &[1u64, 1 << 20, u64::MAX, 0])?;
        os.write_array_signed(11, &[-1i64, i64::MIN, 0, 7])?;
        os.write_array_fp32(12, &[1.5f32, -0.0, f32::MAX])?;
        os.write_array_fp64(13, &[3.14159265f64, -2.5])?;
        os.write_sequence_begin_lazy(14)?;
        os.write_unsigned(1, 99)?;
        os.write_signed(2, -7)?;
        os.write_sequence_end()
    })()
    .unwrap_err();
    assert_eq!(err, Error::BufferFull);
}

#[test]
fn an_array_larger_than_the_buffer_streams_out_whole() {
    // Forces the chunked array writer: 300 ten-byte varints through a 16-byte
    // buffer, so the run is re-sized against the sink over and over.
    let src: Vec<u64> = (0..300u64)
        .map(|i| !i.wrapping_mul(0x9E37_79B9_7F4A_7C15))
        .collect();

    let mut reference = vec![0u8; 4096];
    let used = {
        let mut os = OStream::new(&mut reference);
        os.write_array_unsigned(7, &src).unwrap();
        os.bytes_used()
    };
    reference.truncate(used);

    for size in [1usize, 2, 9, 10, 11, 16, 64, 511] {
        let mut collected: Vec<u8> = Vec::new();
        let mut buf = vec![0u8; size];
        {
            let mut os = OStream::with_flush(&mut buf, 0, |chunk: &[u8]| {
                collected.extend_from_slice(chunk);
            })
            .unwrap();
            os.write_array_unsigned(7, &src).unwrap();
            os.flush().unwrap();
        }
        assert_eq!(
            collected, reference,
            "array diverged with a {size}-byte buffer"
        );
    }
}

// --- decoder: a message split across chunks ----------------------------------

/// Feed `msg` in fixed-size chunks and return the events, asserting that every
/// intermediate outcome is `Ok` or `Incomplete` — never `InvalidMsg`, since a
/// cut in the middle of a field is truncation, not malformedness (§5.2).
fn feed_in_chunks(msg: &[u8], size: usize) -> Vec<Event> {
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    for chunk in msg.chunks(size) {
        match is.feed(chunk, &mut rec) {
            Ok(()) | Err(Error::Incomplete) => {}
            Err(e) => panic!("feed failed at chunk size {size}: {e}"),
        }
    }
    is.feed(&[], &mut rec)
        .expect("stream did not end at a boundary");
    rec.events
}

#[test]
fn every_chunk_size_decodes_to_the_one_shot_events() {
    let msg = one_shot();

    let mut rec = Recorder::new();
    IStream::new().feed(&msg, &mut rec).unwrap();
    let reference = rec.events;

    // Chunk size 1 suspends and resumes at literally every byte boundary, so
    // every resume state (payload, both integer arrays, both float arrays, a
    // varint straddling the cut) is entered and left at least once.
    for size in 1..=msg.len() {
        assert_eq!(
            feed_in_chunks(&msg, size),
            reference,
            "decoding diverged at chunk size {size}"
        );
    }
}

#[test]
fn a_truncated_message_is_incomplete_at_every_cut() {
    let msg = one_shot();
    for cut in 1..msg.len() {
        let mut rec = Recorder::new();
        let mut is = IStream::new();
        // Every prefix of a valid message is either a clean field boundary or
        // an unfinished field — never invalid.
        match is.feed(&msg[..cut], &mut rec) {
            Ok(()) | Err(Error::Incomplete) => {}
            Err(e) => panic!("prefix of length {cut} reported {e}"),
        }
    }
}

// --- the fused writers and the held-back sequence run ------------------------

/// One named field writer, for the table below.
type Case = (&'static str, fn(&mut OStream));

/// Every field writer is a point where a lazily-opened sequence stops being
/// default and its header has to reach the wire (CORELIB_PLAN §6). The commit
/// lives in the two content choke points, `write_field_varint` and
/// `write_fixlen_fixed`, each holding its own copy — there is no single funnel
/// below them to inherit it from. So this pins the obligation on each public
/// writer individually: a field written inside a held-back sequence must produce
/// exactly the header, the field, and the end marker.
#[test]
fn every_writer_commits_a_held_back_sequence_header() {
    fn bytes_of(f: impl Fn(&mut OStream)) -> Vec<u8> {
        let mut buf = vec![0u8; 512];
        let used = {
            let mut os = OStream::new(&mut buf);
            f(&mut os);
            os.bytes_used()
        };
        buf.truncate(used);
        buf
    }

    let cases: Vec<Case> = vec![
        ("unsigned", |os| os.write_unsigned(1, 300).unwrap()),
        ("signed", |os| os.write_signed(1, -300).unwrap()),
        ("boolean", |os| os.write_boolean(1, true).unwrap()),
        ("fp32", |os| os.write_fp32(1, 3.14159).unwrap()),
        ("fp64", |os| os.write_fp64(1, 2.718281828459045).unwrap()),
        ("str", |os| os.write_str(1, "sofab").unwrap()),
        ("blob", |os| os.write_blob(1, &[1u8, 2, 3]).unwrap()),
        ("array_unsigned", |os| {
            os.write_array_unsigned(1, &[10u16, 20]).unwrap()
        }),
        ("array_signed", |os| {
            os.write_array_signed(1, &[-10i16, 20]).unwrap()
        }),
        ("array_fp32", |os| {
            os.write_array_fp32(1, &[1.5f32, 2.5]).unwrap()
        }),
        ("array_fp64", |os| {
            os.write_array_fp64(1, &[1.5f64, 2.5]).unwrap()
        }),
    ];

    for (name, write_field) in cases {
        let bare = bytes_of(write_field);
        let wrapped = bytes_of(|os| {
            os.write_sequence_begin_lazy(2).unwrap();
            write_field(os);
            os.write_sequence_end().unwrap();
        });

        // Sequence id 2, wire type SEQUENCE_START (6) => (2 << 3) | 6 = 0x16;
        // the end marker is the bare 0x07 byte.
        let mut expected = vec![0x16];
        expected.extend_from_slice(&bare);
        expected.push(0x07);
        assert_eq!(
            wrapped, expected,
            "{name} inside a held-back sequence lost the sequence header"
        );
    }
}

/// The mirror: a held-back sequence that receives no content leaves no trace,
/// even though the fast paths now test `pending` on entry.
#[test]
fn a_contentless_held_back_sequence_still_vanishes() {
    let mut buf = [0u8; 32];
    let used = {
        let mut os = OStream::new(&mut buf);
        os.write_sequence_begin_lazy(1).unwrap();
        os.write_sequence_end().unwrap();
        os.write_unsigned(3, 7).unwrap();
        os.bytes_used()
    };
    assert_eq!(&buf[..used], &[0x18, 0x07]); // just the u field, id 3 => 3 << 3
}
