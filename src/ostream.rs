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
//! (see `examples/person.rs`) — that is the back end of the generated-object
//! `serialize()` helper (ARCHITECTURE §6.1).

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

/// Streaming Sofab encoder writing into a caller-provided buffer.
pub struct OStream<'a, F: Flush = NoFlush> {
    buffer: &'a mut [u8],
    end: usize,
    offset: usize,
    /// Number of nested sequences currently open, capped at [`MAX_DEPTH`].
    depth: u32,
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
    #[inline]
    fn write_id_type(&mut self, id: Id, wire_type: u8) -> Result<()> {
        if id > ID_MAX {
            return Err(Error::Argument);
        }
        self.write_varint(((id as Unsigned) << 3) | wire_type as Unsigned)
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
    /// A zero-count fixlen array carries **no `fixlen_word` and no payload** — it
    /// encodes as exactly `[ header ][ element_count = 0 ]` (§4.8).
    pub fn write_array_fp32(&mut self, id: Id, data: &[f32]) -> Result<()> {
        if data.len() as u64 > ARRAY_MAX {
            return Err(Error::Argument);
        }
        self.write_id_type(id, T_FIXLENARRAY)?;
        self.write_varint(data.len() as Unsigned)?;
        if data.is_empty() {
            return Ok(());
        }
        self.write_varint((4 << 3) | FixlenType::Fp32 as Unsigned)?;
        for &e in data {
            self.push_raw(&e.to_le_bytes())?;
        }
        Ok(())
    }

    /// Write an array of 64-bit floats.
    ///
    /// A zero-count fixlen array carries **no `fixlen_word` and no payload** — it
    /// encodes as exactly `[ header ][ element_count = 0 ]` (§4.8).
    pub fn write_array_fp64(&mut self, id: Id, data: &[f64]) -> Result<()> {
        if data.len() as u64 > ARRAY_MAX {
            return Err(Error::Argument);
        }
        self.write_id_type(id, T_FIXLENARRAY)?;
        self.write_varint(data.len() as Unsigned)?;
        if data.is_empty() {
            return Ok(());
        }
        self.write_varint((8 << 3) | FixlenType::Fp64 as Unsigned)?;
        for &e in data {
            self.push_raw(&e.to_le_bytes())?;
        }
        Ok(())
    }

    // --- sequence writers ---------------------------------------------------

    /// Open a nested sequence with the given field `id`.
    ///
    /// Returns [`Error::Argument`] if more than [`MAX_DEPTH`] (255) sequences
    /// would be open at once (§4.9).
    #[inline]
    pub fn write_sequence_begin(&mut self, id: Id) -> Result<()> {
        if self.depth >= MAX_DEPTH {
            return Err(Error::Argument);
        }
        self.write_id_type(id, T_SEQUENCE_START)?;
        self.depth += 1;
        Ok(())
    }

    /// Close the most recently opened nested sequence.
    #[inline]
    pub fn write_sequence_end(&mut self) -> Result<()> {
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
