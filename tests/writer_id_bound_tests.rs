//! `ID_MAX` binds **every** writer (CORELIB_PLAN §4.3, §6.3).
//!
//! A field header is `(id << 3) | wire_type`, so an id above `ID_MAX`
//! (`INT32_MAX`) is not a header this format can carry: the encoder must refuse
//! it with `Error::Argument` (§6.3's `InvalidArgument`) rather than emit a tag
//! every conformant decoder — this port's own included — answers `InvalidMsg` to.
//!
//! The check is not inherited from one funnel. `write_field_varint` carries a
//! copy for the scalar and array writers, `write_fixlen_unchecked` a second one
//! for the `fixlen` family (`write_str` / `write_blob` / `write_fp32` /
//! `write_fp64` / `write_fixlen`), and `write_sequence_begin_lazy` a third,
//! because a held-back sequence header is written later by a commit path that
//! bounds nothing. Delete any one of the three and only the writers routed
//! through it lose the bound — silently, on the wire. So the obligation is pinned
//! per writer, in both directions: `ID_MAX + 1` is refused with nothing written,
//! and `ID_MAX` itself round-trips.

mod common;

use common::{Event, Recorder};
use sofab::{decode, Error, FixlenType, Id, OStream, Result, ID_MAX};

/// One public writer, applied to a caller-chosen id.
type Writer = fn(&mut OStream, Id) -> Result<()>;

/// Every entry point that puts an id on the wire, with the event its `ID_MAX`
/// form decodes to.
fn writers() -> Vec<(&'static str, Writer, Event)> {
    vec![
        (
            "write_unsigned",
            |os, id| os.write_unsigned(id, 42),
            Event::Unsigned(ID_MAX, 42),
        ),
        (
            "write_signed",
            |os, id| os.write_signed(id, -42),
            Event::Signed(ID_MAX, -42),
        ),
        (
            "write_boolean",
            |os, id| os.write_boolean(id, true),
            Event::Unsigned(ID_MAX, 1),
        ),
        (
            "write_fp32",
            |os, id| os.write_fp32(id, 1.5),
            Event::Fp32(ID_MAX, 1.5f32.to_bits()),
        ),
        (
            "write_fp64",
            |os, id| os.write_fp64(id, -2.5),
            Event::Fp64(ID_MAX, (-2.5f64).to_bits()),
        ),
        (
            "write_str",
            |os, id| os.write_str(id, "hi"),
            Event::Str(ID_MAX, b"hi".to_vec()),
        ),
        (
            "write_blob",
            |os, id| os.write_blob(id, &[1, 2, 3]),
            Event::Blob(ID_MAX, vec![1, 2, 3]),
        ),
        (
            "write_fixlen(Str)",
            |os, id| os.write_fixlen(id, b"hi", FixlenType::Str),
            Event::Str(ID_MAX, b"hi".to_vec()),
        ),
        (
            "write_fixlen(Blob)",
            |os, id| os.write_fixlen(id, &[1, 2, 3], FixlenType::Blob),
            Event::Blob(ID_MAX, vec![1, 2, 3]),
        ),
        (
            "write_fixlen(Fp32)",
            |os, id| os.write_fixlen(id, &1.5f32.to_le_bytes(), FixlenType::Fp32),
            Event::Fp32(ID_MAX, 1.5f32.to_bits()),
        ),
        (
            "write_fixlen(Fp64)",
            |os, id| os.write_fixlen(id, &(-2.5f64).to_le_bytes(), FixlenType::Fp64),
            Event::Fp64(ID_MAX, (-2.5f64).to_bits()),
        ),
        (
            "write_array_unsigned",
            |os, id| os.write_array_unsigned(id, &[1u64, 2]),
            Event::ArrayBegin(ID_MAX, sofab::ArrayKind::Unsigned, 2),
        ),
        (
            "write_array_signed",
            |os, id| os.write_array_signed(id, &[-1i64, 2]),
            Event::ArrayBegin(ID_MAX, sofab::ArrayKind::Signed, 2),
        ),
        (
            "write_array_fp32",
            |os, id| os.write_array_fp32(id, &[1.5f32]),
            Event::ArrayBegin(ID_MAX, sofab::ArrayKind::Fp32, 1),
        ),
        (
            "write_array_fp64",
            |os, id| os.write_array_fp64(id, &[1.5f64]),
            Event::ArrayBegin(ID_MAX, sofab::ArrayKind::Fp64, 1),
        ),
        (
            "write_sequence_begin_lazy",
            |os, id| {
                os.write_sequence_begin_lazy(id)?;
                os.write_unsigned(0, 7)?;
                os.write_sequence_end()
            },
            Event::SequenceBegin(ID_MAX),
        ),
    ]
}

#[test]
fn every_writer_refuses_an_id_above_id_max_and_writes_nothing() {
    for (name, write, _) in writers() {
        for id in [ID_MAX + 1, ID_MAX + 2, u32::MAX] {
            let mut buf = [0u8; 64];
            let mut os = OStream::new(&mut buf);
            assert_eq!(
                write(&mut os, id),
                Err(Error::Argument),
                "{name}: id {id} is above ID_MAX and must be InvalidArgument"
            );
            assert_eq!(
                os.bytes_used(),
                0,
                "{name}: a refused id must leave the stream untouched"
            );
        }
    }
}

#[test]
fn a_refused_id_does_not_disturb_the_stream() {
    // §6.3 rejections are not terminal: the encoder is a usable stream after one,
    // and what it goes on to write is byte-identical to a stream that never saw
    // the bad call. (For the fixlen family the check sits *before* the held-back
    // sequence run is committed, so a refused write cannot commit a header either.)
    for (name, write, _) in writers() {
        let mut clean = [0u8; 64];
        let clean_used = {
            let mut os = OStream::new(&mut clean);
            os.write_unsigned(1, 99).unwrap();
            os.bytes_used()
        };

        let mut buf = [0u8; 64];
        let used = {
            let mut os = OStream::new(&mut buf);
            assert_eq!(write(&mut os, ID_MAX + 1), Err(Error::Argument));
            os.write_unsigned(1, 99).unwrap();
            os.bytes_used()
        };
        assert_eq!(&buf[..used], &clean[..clean_used], "{name}");
    }
}

#[test]
fn every_writer_accepts_id_max_itself() {
    // The bound is `>`, not `>=`: `ID_MAX` is a legal id and must survive the
    // round trip, five header bytes and all.
    for (name, write, want) in writers() {
        let mut buf = [0u8; 64];
        let used = {
            let mut os = OStream::new(&mut buf);
            write(&mut os, ID_MAX).unwrap_or_else(|e| panic!("{name}: ID_MAX refused: {e}"));
            os.bytes_used()
        };
        // A five-byte header: (ID_MAX << 3) fills 34 bits.
        assert!(used >= 5, "{name}: unexpected encoding");

        let mut rec = Recorder::new();
        decode(&buf[..used], &mut rec).unwrap_or_else(|e| panic!("{name}: decode failed: {e}"));
        assert_eq!(rec.events.first(), Some(&want), "{name}");
    }
}

#[test]
fn the_decoder_rejects_the_ids_the_encoder_refuses_to_write() {
    // The two halves must agree, which is *why* the encoder refuses: a header
    // carrying `ID_MAX + 1` is malformed on the wire (§4.3), so if any writer let
    // one through the result would be a message this very port cannot read.
    for wire in 0u64..=7 {
        let mut msg = Vec::new();
        common::push_varint(&mut msg, ((ID_MAX as u64 + 1) << 3) | wire);
        common::push_varint(&mut msg, 0); // a value/word/count, if the type takes one
        let mut rec = Recorder::new();
        assert_eq!(
            decode(&msg, &mut rec),
            Err(Error::InvalidMsg),
            "wire type {wire}: an id above ID_MAX must be INVALID"
        );
    }
}
