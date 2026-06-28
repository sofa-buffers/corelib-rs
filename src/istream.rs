//! Streaming input stream decoder.
//!
//! Two ways in, one [`Visitor`]:
//!
//! * [`decode`] — the **fast contiguous path**. Hand it a complete message and
//!   it advances a cursor over the buffer, decoding every field with no copies;
//!   string/blob payloads are delivered as a single borrowed slice straight out
//!   of your buffer. This is the 90 % case on a server and the speed showcase.
//! * [`IStream`] — the **streaming path** (ARCHITECTURE §5.2). Feed it bytes in
//!   arbitrarily small chunks with [`IStream::feed`]; a single field header or
//!   payload may be split across any number of `feed` calls and the decoder
//!   suspends/resumes at any byte boundary. When the whole message is fed in one
//!   call it takes the same zero-copy fast path internally; only the few bytes of
//!   a field that genuinely straddles a chunk boundary are ever copied (into a
//!   small carry buffer).
//!
//! Both drive the same [`Visitor`]: a field handler with a default no-op for
//! every method, so an implementor overrides only the field kinds it cares about
//! and unhandled fields are skipped automatically.

use crate::error::{Error, Result};
use crate::types::*;
use crate::varint::{read_varint, zigzag_decode};
use crate::{ArrayKind, FixlenType, Id, Signed, Unsigned};

/// Receives decoded fields from [`IStream`] / [`decode`].
///
/// Every method has a default empty implementation, so an implementor overrides
/// only the field kinds it cares about. Fields that are not handled are simply
/// dropped (the equivalent of "not interested" / skip in the C API).
#[allow(unused_variables)]
pub trait Visitor {
    /// An unsigned integer field, or an unsigned array element.
    fn unsigned(&mut self, id: Id, value: Unsigned) {}

    /// A signed integer field, or a signed array element.
    fn signed(&mut self, id: Id, value: Signed) {}

    /// A 32-bit float field, or an `fp32` array element.
    fn fp32(&mut self, id: Id, value: f32) {}

    /// A 64-bit float field, or an `fp64` array element.
    fn fp64(&mut self, id: Id, value: f64) {}

    /// A chunk of a string field. `total` is the full field length; `offset` is
    /// the byte position of this `chunk` within the field. For an empty string
    /// this is called once with `total == 0` and an empty `chunk`. The
    /// contiguous [`decode`] path always delivers the whole string in a single
    /// call (`offset == 0`, `chunk.len() == total`).
    fn string(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {}

    /// A chunk of a blob field. See [`Visitor::string`] for the chunking model.
    fn blob(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {}

    /// Start of an array field with `count` elements of the given `kind`. The
    /// elements follow via the scalar / float callbacks with the same `id`.
    fn array_begin(&mut self, id: Id, kind: ArrayKind, count: usize) {}

    /// Start of a nested sequence with the given field `id`.
    fn sequence_begin(&mut self, id: Id) {}

    /// End of the current nested sequence.
    fn sequence_end(&mut self) {}
}

/// What the decoder was in the middle of when the previous chunk ran out.
///
/// Small payload items (a split varint or float) are not represented here — they
/// are carried as raw bytes and re-parsed; this enum captures only the
/// coarse-grained "I am partway through a long thing" states whose progress must
/// survive across chunks without re-delivery.
#[derive(Clone, Copy)]
enum Resume {
    None,
    /// Mid string/blob payload (delivered incrementally).
    Payload {
        id: Id,
        is_blob: bool,
        total: usize,
        remaining: usize,
    },
    /// Mid integer array: `remaining` elements still to read.
    ArrayInt {
        id: Id,
        signed: bool,
        remaining: usize,
    },
    /// Mid fixlen (float) array: `remaining` elements of `elem_len` bytes each.
    ArrayFix {
        id: Id,
        fp64: bool,
        elem_len: usize,
        remaining: usize,
    },
}

/// Streaming Sofab decoder. Reusable across messages via [`IStream::reset`].
pub struct IStream {
    /// Bytes of an item that straddled a chunk boundary, carried to the next
    /// `feed`. Only ever holds a partial small item (header / varint / float),
    /// so it stays tiny; large payloads are streamed, not buffered.
    carry: Vec<u8>,
    resume: Resume,
    /// Nested sequence depth, for balanced start/end validation.
    depth: u32,
}

impl Default for IStream {
    fn default() -> Self {
        Self::new()
    }
}

impl IStream {
    /// Create a fresh decoder ready to accept a new message.
    pub const fn new() -> Self {
        IStream {
            carry: Vec::new(),
            resume: Resume::None,
            depth: 0,
        }
    }

    /// Reset to the initial state so the decoder can be reused for a new message
    /// without reallocating its carry buffer.
    pub fn reset(&mut self) {
        self.carry.clear();
        self.resume = Resume::None;
        self.depth = 0;
    }

    /// Feed a chunk of encoded bytes, pushing decoded fields to `visitor`.
    ///
    /// Returns [`Error::InvalidMsg`] on malformed input. Decoding can continue
    /// across many `feed` calls; the decoder keeps all state internally.
    pub fn feed<V: Visitor>(&mut self, chunk: &[u8], visitor: &mut V) -> Result<()> {
        if self.carry.is_empty() {
            // Fast path: parse straight from the caller's slice, no copy.
            let consumed = self.parse(chunk, visitor)?;
            if consumed < chunk.len() {
                self.carry.extend_from_slice(&chunk[consumed..]);
            }
        } else {
            // A small item straddled the previous boundary: stitch it together.
            let mut buf = core::mem::take(&mut self.carry);
            buf.extend_from_slice(chunk);
            let consumed = self.parse(&buf, visitor)?;
            buf.drain(..consumed);
            self.carry = buf;
        }
        Ok(())
    }

    /// Assert the decoder is at a clean message boundary: no half-read field, no
    /// open sequence. Used by [`decode`] to reject truncated input.
    pub fn finish(&self) -> Result<()> {
        let clean = self.carry.is_empty() && matches!(self.resume, Resume::None) && self.depth == 0;
        if clean {
            Ok(())
        } else {
            Err(Error::InvalidMsg)
        }
    }

    /// Parse as many complete fields as possible from `buf`, returning the number
    /// of bytes fully consumed. Whatever follows the returned offset is an
    /// incomplete small item the caller must carry to the next chunk. Long
    /// payloads (string/blob) and array progress are committed via `self.resume`,
    /// so they are never re-delivered.
    fn parse<V: Visitor>(&mut self, buf: &[u8], v: &mut V) -> Result<usize> {
        let mut pos = 0usize;
        loop {
            // 1) Finish anything left in progress from a previous chunk.
            match self.resume {
                Resume::None => {}
                Resume::Payload { .. } => {
                    pos = self.deliver_payload(buf, pos, v);
                    if matches!(self.resume, Resume::Payload { .. }) {
                        return Ok(pos); // still hungry for payload bytes
                    }
                    continue;
                }
                Resume::ArrayInt {
                    id,
                    signed,
                    remaining,
                } => {
                    let mut rem = remaining;
                    while rem > 0 {
                        let elem_start = pos;
                        match read_varint(buf, &mut pos)? {
                            Some(val) => {
                                if signed {
                                    v.signed(id, zigzag_decode(val));
                                } else {
                                    v.unsigned(id, val);
                                }
                                rem -= 1;
                            }
                            None => {
                                self.resume = Resume::ArrayInt {
                                    id,
                                    signed,
                                    remaining: rem,
                                };
                                return Ok(elem_start);
                            }
                        }
                    }
                    self.resume = Resume::None;
                    continue;
                }
                Resume::ArrayFix {
                    id,
                    fp64,
                    elem_len,
                    remaining,
                } => {
                    let mut rem = remaining;
                    while rem > 0 {
                        if buf.len() - pos < elem_len {
                            self.resume = Resume::ArrayFix {
                                id,
                                fp64,
                                elem_len,
                                remaining: rem,
                            };
                            return Ok(pos);
                        }
                        emit_fixlen_value(buf, pos, fp64, id, v);
                        pos += elem_len;
                        rem -= 1;
                    }
                    self.resume = Resume::None;
                    continue;
                }
            }

            // 2) Read the next field header.
            if pos >= buf.len() {
                return Ok(pos);
            }
            let field_start = pos;
            let header = match read_varint(buf, &mut pos)? {
                Some(h) => h,
                None => return Ok(field_start),
            };
            let wire = (header & 0x07) as u8;
            let id_raw = header >> 3;
            if id_raw > ID_MAX as Unsigned {
                return Err(Error::InvalidMsg);
            }
            let id = id_raw as Id;

            match wire {
                T_VARINT_UNSIGNED => match read_varint(buf, &mut pos)? {
                    Some(val) => v.unsigned(id, val),
                    None => return Ok(field_start),
                },
                T_VARINT_SIGNED => match read_varint(buf, &mut pos)? {
                    Some(zz) => v.signed(id, zigzag_decode(zz)),
                    None => return Ok(field_start),
                },

                T_FIXLEN => {
                    let word = match read_varint(buf, &mut pos)? {
                        Some(w) => w,
                        None => return Ok(field_start),
                    };
                    let subtype = FixlenType::from_raw((word & 0x07) as u8)?;
                    if (word >> 3) as u64 > ARRAY_MAX {
                        return Err(Error::InvalidMsg);
                    }
                    let len = (word >> 3) as usize;
                    match subtype {
                        FixlenType::Fp32 => {
                            if len != 4 {
                                return Err(Error::InvalidMsg);
                            }
                            if buf.len() - pos < 4 {
                                return Ok(field_start); // carry header+word+partial
                            }
                            emit_fixlen_value(buf, pos, false, id, v);
                            pos += 4;
                        }
                        FixlenType::Fp64 => {
                            if len != 8 {
                                return Err(Error::InvalidMsg);
                            }
                            if buf.len() - pos < 8 {
                                return Ok(field_start);
                            }
                            emit_fixlen_value(buf, pos, true, id, v);
                            pos += 8;
                        }
                        FixlenType::Str | FixlenType::Blob => {
                            let is_blob = matches!(subtype, FixlenType::Blob);
                            if len == 0 {
                                if is_blob {
                                    v.blob(id, 0, 0, &[]);
                                } else {
                                    v.string(id, 0, 0, &[]);
                                }
                            } else {
                                self.resume = Resume::Payload {
                                    id,
                                    is_blob,
                                    total: len,
                                    remaining: len,
                                };
                                pos = self.deliver_payload(buf, pos, v);
                                if matches!(self.resume, Resume::Payload { .. }) {
                                    return Ok(pos);
                                }
                            }
                        }
                    }
                }

                T_VARINTARRAY_UNSIGNED => {
                    let count = match read_varint(buf, &mut pos)? {
                        Some(c) => c,
                        None => return Ok(field_start),
                    };
                    if count == 0 || count > ARRAY_MAX {
                        return Err(Error::InvalidMsg);
                    }
                    v.array_begin(id, ArrayKind::Unsigned, count as usize);
                    self.resume = Resume::ArrayInt {
                        id,
                        signed: false,
                        remaining: count as usize,
                    };
                }
                T_VARINTARRAY_SIGNED => {
                    let count = match read_varint(buf, &mut pos)? {
                        Some(c) => c,
                        None => return Ok(field_start),
                    };
                    if count == 0 || count > ARRAY_MAX {
                        return Err(Error::InvalidMsg);
                    }
                    v.array_begin(id, ArrayKind::Signed, count as usize);
                    self.resume = Resume::ArrayInt {
                        id,
                        signed: true,
                        remaining: count as usize,
                    };
                }
                T_FIXLENARRAY => {
                    let count = match read_varint(buf, &mut pos)? {
                        Some(c) => c,
                        None => return Ok(field_start),
                    };
                    if count == 0 || count > ARRAY_MAX {
                        return Err(Error::InvalidMsg);
                    }
                    let word = match read_varint(buf, &mut pos)? {
                        Some(w) => w,
                        None => return Ok(field_start),
                    };
                    let subtype = FixlenType::from_raw((word & 0x07) as u8)?;
                    let elem_len = (word >> 3) as usize;
                    // Only fixed-width float subtypes are valid in a fixlen array;
                    // string/blob must use a sequence instead.
                    let fp64 = match subtype {
                        FixlenType::Fp32 => {
                            if elem_len != 4 {
                                return Err(Error::InvalidMsg);
                            }
                            false
                        }
                        FixlenType::Fp64 => {
                            if elem_len != 8 {
                                return Err(Error::InvalidMsg);
                            }
                            true
                        }
                        _ => return Err(Error::InvalidMsg),
                    };
                    v.array_begin(id, ArrayKind::Fixlen, count as usize);
                    self.resume = Resume::ArrayFix {
                        id,
                        fp64,
                        elem_len,
                        remaining: count as usize,
                    };
                }

                T_SEQUENCE_START => {
                    if self.depth == u32::MAX {
                        return Err(Error::InvalidMsg);
                    }
                    self.depth += 1;
                    v.sequence_begin(id);
                }
                T_SEQUENCE_END => {
                    if self.depth == 0 {
                        return Err(Error::InvalidMsg);
                    }
                    self.depth -= 1;
                    v.sequence_end();
                }

                _ => return Err(Error::InvalidMsg),
            }
        }
    }

    /// Deliver as much of an in-progress string/blob payload as `buf` holds,
    /// updating `self.resume`. Returns the new cursor position.
    fn deliver_payload<V: Visitor>(&mut self, buf: &[u8], mut pos: usize, v: &mut V) -> usize {
        if let Resume::Payload {
            id,
            is_blob,
            total,
            remaining,
        } = self.resume
        {
            let avail = (buf.len() - pos).min(remaining);
            if avail > 0 {
                let offset = total - remaining;
                let chunk = &buf[pos..pos + avail];
                if is_blob {
                    v.blob(id, total, offset, chunk);
                } else {
                    v.string(id, total, offset, chunk);
                }
                pos += avail;
                let rem = remaining - avail;
                self.resume = if rem == 0 {
                    Resume::None
                } else {
                    Resume::Payload {
                        id,
                        is_blob,
                        total,
                        remaining: rem,
                    }
                };
            }
        }
        pos
    }
}

/// Decode `elem_len` (4 or 8) little-endian float bytes at `buf[pos..]` and push
/// them to the visitor. Caller guarantees the bytes are present.
#[inline]
fn emit_fixlen_value<V: Visitor>(buf: &[u8], pos: usize, fp64: bool, id: Id, v: &mut V) {
    if fp64 {
        let b: [u8; 8] = buf[pos..pos + 8].try_into().unwrap();
        v.fp64(id, f64::from_le_bytes(b));
    } else {
        let b: [u8; 4] = buf[pos..pos + 4].try_into().unwrap();
        v.fp32(id, f32::from_le_bytes(b));
    }
}

/// Decode a complete, contiguous message in one shot — the fast zero-copy path.
///
/// `buf` must contain the entire message. Every field is pushed to `visitor`;
/// string/blob payloads are delivered as a single borrowed slice with no copy.
/// Returns [`Error::InvalidMsg`] if the bytes are truncated or malformed.
///
/// ```
/// use sofab::{OStream, decode, Visitor, Id, Unsigned};
/// let mut buf = [0u8; 16];
/// let n = { let mut os = OStream::new(&mut buf); os.write_unsigned(1, 42).unwrap(); os.bytes_used() };
///
/// #[derive(Default)]
/// struct Sink(Unsigned);
/// impl Visitor for Sink { fn unsigned(&mut self, _id: Id, v: Unsigned) { self.0 = v; } }
/// let mut sink = Sink::default();
/// decode(&buf[..n], &mut sink).unwrap();
/// assert_eq!(sink.0, 42);
/// ```
pub fn decode<V: Visitor>(buf: &[u8], visitor: &mut V) -> Result<()> {
    let mut is = IStream::new();
    is.feed(buf, visitor)?;
    is.finish()
}
