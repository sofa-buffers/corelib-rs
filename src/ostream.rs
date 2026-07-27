//! Streaming output stream encoder.
//!
//! [`OStream`] writes Sofab fields into a caller-owned byte buffer. When the
//! buffer fills it hands the bytes to an optional [`Flush`] sink and resumes at
//! the start of the buffer, so messages larger than the buffer can be streamed
//! out (ARCHITECTURE §5.1). With no sink, a full buffer yields
//! [`Error::BufferFull`].
//!
//! For the common server case where you just want the bytes in a growable `Vec`,
//! drive a small scratch buffer with a flush closure that appends to the `Vec`
//! — that is the back end of the generated-object `serialize()` helper
//! (ARCHITECTURE §6.1).

use crate::error::{Error, Result};
use crate::types::*;
use crate::varint::zigzag_encode;
use crate::{Id, Signed, Unsigned};

/// Sink that receives buffered bytes when the output buffer is flushed.
///
/// Any `FnMut(&[u8])` closure implements this trait, so callbacks can be passed
/// directly. Implement it manually to plug into a custom transport/writer.
pub trait Flush {
    /// Consume `data` (e.g. push to a transport or storage). Called with the
    /// bytes accumulated since the last flush.
    fn flush(&mut self, data: &[u8]);
}

impl<T: FnMut(&[u8])> Flush for T {
    #[inline]
    fn flush(&mut self, data: &[u8]) {
        self(data)
    }
}

/// A [`Flush`] sink that does nothing. Used as the default when the stream is
/// constructed without a sink; a full buffer then returns [`Error::BufferFull`].
#[derive(Debug, Clone, Copy, Default)]
pub struct NoFlush;

impl Flush for NoFlush {
    #[inline]
    fn flush(&mut self, _data: &[u8]) {}
}

/// How many held-back sequence headers fit without touching the heap. Nesting
/// deeper than this spills into a `Vec` and the run keeps growing — this is a
/// storage split, **not** a bound on the hold-back: past it the encoder still
/// holds back, it just pays an allocation (see [`PendingRun`]).
const INLINE_PENDING: usize = 8;

/// The run of held-back sequence ids: the ids of the innermost open sequences
/// whose header has not been written yet (MESSAGE_SPEC §2 lazy framing).
///
/// A stack that lives inline for the first [`INLINE_PENDING`] levels and spills
/// to the heap beyond, so it is **unbounded** up to the format's [`MAX_DEPTH`]
/// ceiling — which is what makes this port canonical at every legal depth.
/// CORELIB_PLAN §6 ("How deep the hold-back reaches") lets only a heap-free
/// profile bound the run and frame eagerly past the bound; this crate has a
/// heap, so it must not.
///
/// The spill is grown on demand: an encoder that never opens a sequence, or
/// never nests deeper than [`INLINE_PENDING`], allocates nothing at all. That
/// matters because a fresh `OStream` is normally built per message, so an
/// unconditional allocation would be one malloc/free per message (measured at
/// +280 instructions/op on the `encode: typical message` Callgrind workload).
///
/// Storage layout: ids `0..n` sit in `inline`, the rest follow in `spill`, in
/// wire order (outermost first). `spill` may be non-empty while `n` is below
/// [`INLINE_PENDING`] — after a partial commit — so `push` must consult `spill`
/// first to keep the order.
#[derive(Default)]
struct PendingRun {
    inline: [Id; INLINE_PENDING],
    n: usize,
    spill: Vec<Id>,
}

impl PendingRun {
    /// Number of held-back headers.
    #[inline]
    fn len(&self) -> usize {
        self.n + self.spill.len()
    }

    /// Whether any header is held back. On the hot path: every field write tests
    /// it once.
    #[inline]
    fn is_empty(&self) -> bool {
        self.n == 0 && self.spill.is_empty()
    }

    /// Append an id (the innermost open sequence).
    #[inline]
    fn push(&mut self, id: Id) {
        if self.n < INLINE_PENDING && self.spill.is_empty() {
            self.inline[self.n] = id;
            self.n += 1;
        } else {
            self.spill.push(id);
        }
    }

    /// Remove the innermost held-back id, if the innermost open sequence is one.
    #[inline]
    fn pop(&mut self) -> Option<Id> {
        if let Some(id) = self.spill.pop() {
            return Some(id);
        }
        self.n = self.n.checked_sub(1)?;
        Some(self.inline[self.n])
    }

    /// The `i`th held-back id, outermost first.
    #[inline]
    fn get(&self, i: usize) -> Id {
        if i < self.n {
            self.inline[i]
        } else {
            self.spill[i - self.n]
        }
    }

    /// Drop the outermost `k` ids — the prefix that has reached the wire. The
    /// remainder keeps its order, so it stays the innermost contiguous suffix of
    /// the open sequences.
    fn drop_front(&mut self, k: usize) {
        if k <= self.n {
            self.inline.copy_within(k..self.n, 0);
            self.n -= k;
        } else {
            self.spill.drain(..k - self.n);
            self.n = 0;
        }
    }
}

/// Streaming Sofab encoder writing into a caller-provided buffer.
pub struct OStream<'a, F: Flush = NoFlush> {
    buffer: &'a mut [u8],
    end: usize,
    offset: usize,
    /// Number of nested sequences currently open, capped at [`MAX_DEPTH`].
    depth: u32,
    /// Sequence headers held back so far. Always a contiguous suffix of the open
    /// sequences: writing any field commits the whole run at once.
    pending: PendingRun,
    /// `None` means "no sink": a full buffer is an error rather than a flush.
    flush: Option<F>,
}

impl<'a> OStream<'a, NoFlush> {
    /// Create an encoder over `buffer` with no flush sink. Writing past the end
    /// of the buffer returns [`Error::BufferFull`].
    #[inline]
    pub fn new(buffer: &'a mut [u8]) -> Self {
        Self::with_offset(buffer, 0)
    }

    /// Like [`OStream::new`] but begin writing at `offset` bytes into the
    /// buffer, reserving space for a lower-layer protocol header.
    #[inline]
    pub fn with_offset(buffer: &'a mut [u8], offset: usize) -> Self {
        let end = buffer.len();
        OStream {
            buffer,
            end,
            offset,
            depth: 0,
            pending: PendingRun::default(), // heap-free until nesting spills
            flush: None,
        }
    }
}

impl<'a, F: Flush> OStream<'a, F> {
    /// Create an encoder with a flush `sink`, starting at `offset`. When the
    /// buffer fills, the accumulated bytes are passed to `sink` and writing
    /// resumes at the start of the buffer.
    #[inline]
    pub fn with_flush(buffer: &'a mut [u8], offset: usize, sink: F) -> Self {
        let end = buffer.len();
        OStream {
            buffer,
            end,
            offset,
            depth: 0,
            pending: PendingRun::default(), // heap-free until nesting spills
            flush: Some(sink),
        }
    }

    /// Number of bytes written to the active buffer since the last flush.
    #[inline]
    pub fn bytes_used(&self) -> usize {
        self.offset
    }

    /// Flush any pending bytes to the sink (if one is set) and report how many
    /// bytes were pending. With no sink the buffer is left intact.
    pub fn flush(&mut self) -> usize {
        let used = self.offset;
        if used > 0 {
            if let Some(sink) = self.flush.as_mut() {
                sink.flush(&self.buffer[..used]);
                self.offset = 0;
            }
        }
        used
    }

    /// Replace the active buffer (typically called from within a flush sink),
    /// resuming writes at `offset` in the new buffer.
    #[inline]
    pub fn buffer_set(&mut self, buffer: &'a mut [u8], offset: usize) {
        self.end = buffer.len();
        self.buffer = buffer;
        self.offset = offset;
    }

    // --- primitives ---------------------------------------------------------

    /// Append a single byte, draining the buffer to the sink first if it is full.
    #[inline]
    fn push_byte(&mut self, b: u8) -> Result<()> {
        if self.offset >= self.end {
            self.drain_full()?;
        }
        // SAFETY: `offset < end <= buffer.len()` guaranteed by the check above.
        unsafe { *self.buffer.get_unchecked_mut(self.offset) = b };
        self.offset += 1;
        Ok(())
    }

    /// Cold path: the buffer is full — flush it or report `BufferFull`.
    #[cold]
    #[inline(never)]
    fn drain_full(&mut self) -> Result<()> {
        match self.flush.as_mut() {
            Some(sink) => {
                sink.flush(&self.buffer[..self.offset]);
                self.offset = 0;
                Ok(())
            }
            None => Err(Error::BufferFull),
        }
    }

    /// Copy a raw byte slice out, draining the buffer as needed. Uses a bulk
    /// `copy_from_slice` per buffer-sized run rather than a byte-at-a-time loop.
    fn push_raw(&mut self, mut data: &[u8]) -> Result<()> {
        while !data.is_empty() {
            if self.offset >= self.end {
                self.drain_full()?;
            }
            let n = (self.end - self.offset).min(data.len());
            self.buffer[self.offset..self.offset + n].copy_from_slice(&data[..n]);
            self.offset += n;
            data = &data[n..];
        }
        Ok(())
    }

    /// Encode `value` as a base-128 (LEB128) varint: 7 payload bits per byte,
    /// low byte first, with the high bit set on every byte but the last.
    #[inline]
    fn write_varint(&mut self, mut value: Unsigned) -> Result<()> {
        loop {
            let mut b = (value as u8) & 0x7F;
            value >>= 7;
            if value != 0 {
                b |= 0x80;
            }
            self.push_byte(b)?;
            if value == 0 {
                return Ok(());
            }
        }
    }

    /// Write a field header: the `(id << 3) | wire_type` tag as a varint.
    /// Returns [`Error::Argument`] for an `id` above [`ID_MAX`].
    ///
    /// This is the single choke point every field write passes through, so it is
    /// also where a held-back sequence run is committed: the field about to be
    /// written is content, which means every enclosing sequence is non-default and
    /// must be framed after all.
    #[inline]
    fn write_id_type(&mut self, id: Id, wire_type: u8) -> Result<()> {
        if id > ID_MAX {
            return Err(Error::Argument);
        }
        let is_content = wire_type != T_SEQUENCE_START && wire_type != T_SEQUENCE_END;
        if is_content && !self.pending.is_empty() {
            self.commit_pending()?;
        }
        self.write_varint(((id as Unsigned) << 3) | wire_type as Unsigned)
    }

    /// Write out the held-back sequence headers, outermost first.
    ///
    /// Cold: it runs at most once per non-default sequence, never per field.
    ///
    /// Only the headers that actually reached the buffer are dropped from the
    /// run: if the buffer fills mid-run with no sink to drain it, the sequences
    /// that were not written yet stay pending — still the innermost contiguous
    /// suffix of the open sequences, so the invariant holds and a caller that
    /// installs a bigger buffer ([`OStream::buffer_set`]) can carry on. No
    /// writer in this encoder is atomic on failure, though: a varint can be cut
    /// in half by the same buffer end, so `BufferFull` without a sink still
    /// leaves a partial message behind.
    #[cold]
    #[inline(never)]
    fn commit_pending(&mut self) -> Result<()> {
        let mut written = 0;
        let mut result = Ok(());
        for i in 0..self.pending.len() {
            let tag = ((self.pending.get(i) as Unsigned) << 3) | T_SEQUENCE_START as Unsigned;
            if let Err(e) = self.write_varint(tag) {
                result = Err(e);
                break;
            }
            written += 1;
        }
        self.pending.drop_front(written);
        result
    }

    // --- scalar writers -----------------------------------------------------

    /// Write an unsigned-integer field.
    #[inline]
    pub fn write_unsigned(&mut self, id: Id, value: Unsigned) -> Result<()> {
        self.write_id_type(id, T_VARINT_UNSIGNED)?;
        self.write_varint(value)
    }

    /// Write a signed-integer field (ZigZag + varint).
    #[inline]
    pub fn write_signed(&mut self, id: Id, value: Signed) -> Result<()> {
        self.write_id_type(id, T_VARINT_SIGNED)?;
        self.write_varint(zigzag_encode(value))
    }

    /// Write a boolean as an unsigned `0` / `1`.
    #[inline]
    pub fn write_boolean(&mut self, id: Id, value: bool) -> Result<()> {
        self.write_unsigned(id, value as Unsigned)
    }

    // --- fixed-length writers ----------------------------------------------

    /// Write a fixed-length field: header, `(len << 3) | subtype` varint, then
    /// the raw `data` bytes (already in wire/little-endian order for floats).
    pub fn write_fixlen(&mut self, id: Id, data: &[u8], subtype: FixlenType) -> Result<()> {
        if data.len() as u64 > FIXLEN_MAX {
            return Err(Error::Argument);
        }
        self.write_id_type(id, T_FIXLEN)?;
        self.write_varint(((data.len() as Unsigned) << 3) | subtype as Unsigned)?;
        self.push_raw(data)
    }

    /// Write a 32-bit float field.
    #[inline]
    pub fn write_fp32(&mut self, id: Id, value: f32) -> Result<()> {
        self.write_fixlen(id, &value.to_le_bytes(), FixlenType::Fp32)
    }

    /// Write a 64-bit float field.
    #[inline]
    pub fn write_fp64(&mut self, id: Id, value: f64) -> Result<()> {
        self.write_fixlen(id, &value.to_le_bytes(), FixlenType::Fp64)
    }

    /// Write a string field (raw UTF-8 bytes, no NUL on the wire).
    ///
    /// The input is `&str`, so it is **already valid UTF-8** by the type system
    /// — encode is strict by construction and can never emit non-UTF-8 bytes
    /// (MESSAGE_SPEC §8, CORELIB_PLAN §6.4). For arbitrary bytes use
    /// [`OStream::write_blob`]. Embedded `U+0000` is permitted and written
    /// verbatim (the wire is length-framed, no NUL terminator).
    #[inline]
    pub fn write_str(&mut self, id: Id, text: &str) -> Result<()> {
        self.write_fixlen(id, text.as_bytes(), FixlenType::Str)
    }

    /// Write a binary blob field.
    #[inline]
    pub fn write_blob(&mut self, id: Id, data: &[u8]) -> Result<()> {
        self.write_fixlen(id, data, FixlenType::Blob)
    }

    // --- array writers ------------------------------------------------------

    /// Write an array of unsigned integers (`u8`/`u16`/`u32`/`u64` elements).
    ///
    /// A zero-count array is a valid empty array on the wire — it encodes as
    /// exactly `[ header ][ element_count = 0 ]` with no elements (§4.7).
    pub fn write_array_unsigned<T: UnsignedElem>(&mut self, id: Id, data: &[T]) -> Result<()> {
        if data.len() as u64 > ARRAY_MAX {
            return Err(Error::Argument);
        }
        self.write_id_type(id, T_VARINTARRAY_UNSIGNED)?;
        self.write_varint(data.len() as Unsigned)?;
        for e in data {
            self.write_varint(e.widen())?;
        }
        Ok(())
    }

    /// Write an array of signed integers (`i8`/`i16`/`i32`/`i64` elements).
    ///
    /// A zero-count array encodes as exactly `[ header ][ element_count = 0 ]`
    /// with no elements (§4.7).
    pub fn write_array_signed<T: SignedElem>(&mut self, id: Id, data: &[T]) -> Result<()> {
        if data.len() as u64 > ARRAY_MAX {
            return Err(Error::Argument);
        }
        self.write_id_type(id, T_VARINTARRAY_SIGNED)?;
        self.write_varint(data.len() as Unsigned)?;
        for e in data {
            self.write_varint(zigzag_encode(e.widen()))?;
        }
        Ok(())
    }

    /// Write an array of 32-bit floats.
    ///
    /// A fixlen array **always** carries its `fixlen_word` (the shared element
    /// subtype/width word), even when the array is empty — a zero-count fixlen
    /// array encodes as `[ header ][ element_count = 0 ][ fixlen_word ]` with no
    /// payload, so an empty fp32 array is distinguishable from an empty fp64
    /// array on the wire (§4.8).
    pub fn write_array_fp32(&mut self, id: Id, data: &[f32]) -> Result<()> {
        if data.len() as u64 > ARRAY_MAX {
            return Err(Error::Argument);
        }
        self.write_id_type(id, T_FIXLENARRAY)?;
        self.write_varint(data.len() as Unsigned)?;
        self.write_varint((4 << 3) | FixlenType::Fp32 as Unsigned)?;
        for &e in data {
            self.push_raw(&e.to_le_bytes())?;
        }
        Ok(())
    }

    /// Write an array of 64-bit floats.
    ///
    /// A fixlen array **always** carries its `fixlen_word` (the shared element
    /// subtype/width word), even when the array is empty — a zero-count fixlen
    /// array encodes as `[ header ][ element_count = 0 ][ fixlen_word ]` with no
    /// payload, so an empty fp64 array is distinguishable from an empty fp32
    /// array on the wire (§4.8).
    pub fn write_array_fp64(&mut self, id: Id, data: &[f64]) -> Result<()> {
        if data.len() as u64 > ARRAY_MAX {
            return Err(Error::Argument);
        }
        self.write_id_type(id, T_FIXLENARRAY)?;
        self.write_varint(data.len() as Unsigned)?;
        self.write_varint((8 << 3) | FixlenType::Fp64 as Unsigned)?;
        for &e in data {
            self.push_raw(&e.to_le_bytes())?;
        }
        Ok(())
    }

    // --- sequence writers ---------------------------------------------------

    /// Open a nested sequence whose header is **held back** until the sequence
    /// turns out to have content.
    ///
    /// MESSAGE_SPEC §2 omits a sequence-typed field whose value equals its declared
    /// default, and "not one child was written" is exactly that condition —
    /// evaluated per child field, recursively, for free. A sequence closed with
    /// nothing in it therefore emits **nothing** instead of a two-byte empty frame,
    /// and an all-default message becomes the empty byte string.
    ///
    /// The predicate is never a byte image of the object, so struct padding cannot
    /// influence it and a non-zero nested default is handled by the caller's
    /// ordinary per-field test.
    ///
    /// This is the only way to open a sequence. How it closes decides whether a
    /// contentless one survives: [`OStream::write_sequence_end`] drops it,
    /// [`OStream::write_sequence_end_keep`] forces the frame out.
    ///
    /// There is **no depth window**: the run of held-back headers grows on
    /// demand to the format's [`MAX_DEPTH`] ceiling, so the omission is
    /// canonical at every legal nesting level (CORELIB_PLAN §6, "How deep the
    /// hold-back reaches" — only a heap-free profile may bound the run and frame
    /// eagerly past the bound). The run is the encoder's only heap use, and even
    /// that is deferred: it stays inline until nesting passes
    /// `INLINE_PENDING` (8) levels.
    ///
    /// ```
    /// use sofab::OStream;
    ///
    /// let mut buf = [0u8; 32];
    /// let used = {
    ///     let mut os = OStream::new(&mut buf);
    ///     os.write_sequence_begin_lazy(1).unwrap();  // header held back
    ///     os.write_sequence_end().unwrap();          // no content → nothing is written
    ///     os.write_sequence_begin_lazy(2).unwrap();  // header held back
    ///     os.write_unsigned(0, 42).unwrap();         // content → commits header id 2 first
    ///     os.write_sequence_end().unwrap();
    ///     os.bytes_used()
    /// };
    /// assert_eq!(&buf[..used], &[0x16, 0x00, 0x2A, 0x07]);
    /// ```
    #[inline]
    pub fn write_sequence_begin_lazy(&mut self, id: Id) -> Result<()> {
        if self.depth >= MAX_DEPTH {
            return Err(Error::Argument);
        }
        if id > ID_MAX {
            return Err(Error::Argument);
        }
        self.pending.push(id);
        self.depth += 1;
        Ok(())
    }

    /// Close the most recently opened nested sequence, letting it **vanish** if it
    /// received no content.
    ///
    /// Use it wherever absence encodes the same value as an empty frame: a
    /// `struct`/`union` field, and an array field whose declared `default` is the
    /// empty collection (MESSAGE_SPEC §2). Where the frame must be visible, close
    /// with [`OStream::write_sequence_end_keep`] instead.
    #[inline]
    pub fn write_sequence_end(&mut self) -> Result<()> {
        if self.pending.pop().is_some() {
            // The innermost open sequence was the last held-back one: dropped.
            self.depth = self.depth.saturating_sub(1);
            return Ok(());
        }
        self.write_id_type(0, T_SEQUENCE_END)?;
        self.depth = self.depth.saturating_sub(1);
        Ok(())
    }

    /// Close the most recently opened nested sequence, **keeping** its frame even
    /// when it received no content.
    ///
    /// Behaves like a write: it first emits any held-back headers — this frame's
    /// and every enclosing one's — and then the end marker, so an empty sequence
    /// reaches the wire as `begin` + `end`.
    ///
    /// Required wherever the frame carries information beyond its contents:
    /// - a **wrapper-array element** (`struct`/`union`/nested row): element
    ///   presence is what carries a dynamic array's length — *highest present id +
    ///   1* (§5.1) — so dropping an all-default element would change the decoded
    ///   length, not just the bytes;
    /// - an array field already known to **differ from a non-empty declared
    ///   `default`**: absence would reconstruct that default, so the empty frame is
    ///   the only encoding of "explicitly empty" (§2, §3).
    ///
    /// The two failure directions are not symmetric, which is why this is the safe
    /// choice when in doubt: using it where [`OStream::write_sequence_end`] would
    /// do costs one non-canonical empty frame that a decoder normalizes away, while
    /// the reverse silently changes an array's length.
    #[inline]
    pub fn write_sequence_end_keep(&mut self) -> Result<()> {
        if !self.pending.is_empty() {
            self.commit_pending()?;
        }
        self.write_id_type(0, T_SEQUENCE_END)?;
        self.depth = self.depth.saturating_sub(1);
        Ok(())
    }
}

/// Unsigned integer element that can be widened to the wire value type.
pub trait UnsignedElem: Copy {
    /// Zero-extend to [`Unsigned`].
    fn widen(self) -> Unsigned;
}

/// Signed integer element that can be widened to the wire value type.
pub trait SignedElem: Copy {
    /// Sign-extend to [`Signed`].
    fn widen(self) -> Signed;
}

macro_rules! impl_unsigned_elem {
    ($($t:ty),*) => {$(
        impl UnsignedElem for $t {
            #[inline]
            fn widen(self) -> Unsigned { self as Unsigned }
        }
    )*};
}

macro_rules! impl_signed_elem {
    ($($t:ty),*) => {$(
        impl SignedElem for $t {
            #[inline]
            fn widen(self) -> Signed { self as Signed }
        }
    )*};
}

impl_unsigned_elem!(u8, u16, u32, u64);
impl_signed_elem!(i8, i16, i32, i64);
