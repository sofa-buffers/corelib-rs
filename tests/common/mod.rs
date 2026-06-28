//! Shared test helpers: a recording [`Visitor`] and a tiny manual varint
//! encoder for crafting malformed inputs.
//!
//! Test vectors throughout the test suite are taken verbatim from the C
//! reference test suite (`test/c/test_ostream.c`).

#![allow(dead_code)]

use sofab::ArrayKind;
use sofab::{Id, Signed, Unsigned, Visitor};

/// One decoded event, recorded in order by [`Recorder`].
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Unsigned(Id, Unsigned),
    Signed(Id, Signed),
    /// Float stored as raw bits so comparisons are exact (incl. NaN payloads).
    Fp32(Id, u32),
    Fp64(Id, u64),
    Str(Id, Vec<u8>),
    Blob(Id, Vec<u8>),
    ArrayBegin(Id, ArrayKind, usize),
    SequenceBegin(Id),
    SequenceEnd,
}

/// A [`Visitor`] that records every decoded field as an [`Event`], reassembling
/// chunked string/blob payloads into whole buffers.
#[derive(Default)]
pub struct Recorder {
    pub events: Vec<Event>,
    // in-progress chunked string/blob accumulator: (id, is_blob, buffer)
    pending: Option<(Id, bool, Vec<u8>)>,
}

impl Recorder {
    pub fn new() -> Self {
        Self::default()
    }

    fn accumulate(&mut self, id: Id, is_blob: bool, total: usize, offset: usize, chunk: &[u8]) {
        if offset == 0 {
            self.pending = Some((id, is_blob, Vec::with_capacity(total)));
        }
        let done = {
            let (_, _, buf) = self.pending.as_mut().expect("chunk without begin");
            buf.extend_from_slice(chunk);
            buf.len() == total
        };
        if done {
            let (pid, pblob, buf) = self.pending.take().unwrap();
            self.events.push(if pblob {
                Event::Blob(pid, buf)
            } else {
                Event::Str(pid, buf)
            });
        }
    }
}

impl Visitor for Recorder {
    fn unsigned(&mut self, id: Id, value: Unsigned) {
        self.events.push(Event::Unsigned(id, value));
    }
    fn signed(&mut self, id: Id, value: Signed) {
        self.events.push(Event::Signed(id, value));
    }
    fn fp32(&mut self, id: Id, value: f32) {
        self.events.push(Event::Fp32(id, value.to_bits()));
    }
    fn fp64(&mut self, id: Id, value: f64) {
        self.events.push(Event::Fp64(id, value.to_bits()));
    }
    fn string(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {
        self.accumulate(id, false, total, offset, chunk);
    }
    fn blob(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {
        self.accumulate(id, true, total, offset, chunk);
    }
    fn array_begin(&mut self, id: Id, kind: ArrayKind, count: usize) {
        self.events.push(Event::ArrayBegin(id, kind, count));
    }
    fn sequence_begin(&mut self, id: Id) {
        self.events.push(Event::SequenceBegin(id));
    }
    fn sequence_end(&mut self) {
        self.events.push(Event::SequenceEnd);
    }
}

/// Append a base-128 varint of `value` to `out` (for crafting raw test inputs).
pub fn push_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let mut b = (value as u8) & 0x7F;
        value >>= 7;
        if value != 0 {
            b |= 0x80;
        }
        out.push(b);
        if value == 0 {
            break;
        }
    }
}
