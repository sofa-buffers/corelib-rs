//! Streaming output stream encoder.
//!
//! [`OStream`] writes Sofab fields into a caller-owned byte buffer. When the
//! buffer fills it hands the bytes to an optional [`Flush`] sink and resumes at
//! the start of the buffer, so messages larger than the buffer can be streamed
//! out (CORELIB_PLAN §5.1). With no sink, a full buffer yields
//! [`Error::BufferFull`].
//!
//! For the common server case where you just want the bytes in a growable `Vec`,
//! drive a small scratch buffer with a flush closure that appends to the `Vec`
//! — that is the back end of the generated-object `serialize()` helper
//! (CORELIB_PLAN §6.1).

use crate::error::{Error, Result};
use crate::types::*;
use crate::varint::{
    write_varint_unchecked, write_varint_unchecked_narrow, zigzag_encode, MAX_VARINT_LEN,
};
use crate::{Id, Signed, Unsigned};

/// Sink that receives the bytes buffered so far when the output buffer fills (or
/// on an explicit [`OStream::flush`]) and **copies** them out — the common case,
/// and the whole of it for a closure sink.
///
/// ```
/// # use sofab::OStream;
/// let mut out: Vec<u8> = Vec::new();
/// let mut scratch = [0u8; 16];
/// let mut os = OStream::with_flush(&mut scratch, 0, |chunk: &[u8]| {
///     out.extend_from_slice(chunk)
/// });
/// os.write_unsigned(1, 42).unwrap();
/// os.flush().unwrap();
/// ```
///
/// Any `FnMut(&[u8])` implements this trait, so callbacks can be passed directly;
/// implement it by hand for a custom transport/writer that copies.
///
/// A sink that instead **takes** the buffer — queues it for an asynchronous
/// write, hands it to a transport, to DMA — cannot be expressed this way,
/// because it has no way to say what the encoder should write into next. That
/// half of the §5.1 handover is [`FlushTake`], and this trait feeds into it:
/// every `Flush` is a copying `FlushTake` at every lifetime, and an [`OStream`]
/// accepts either.
///
/// This trait is also what the generated layer is written against
/// (CORELIB_PLAN §6.1). `sofabgen` emits
/// `serialize<_F: sofab::Flush>(&self, os: &mut OStream<'_, _F>)`, which names no
/// lifetime — so the sink trait a generated crate can spell must not have one.
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

/// Sink that receives the output **buffer** when it fills (or on an explicit
/// [`OStream::flush`]), and says what the encoder writes into next.
///
/// # The handover contract (CORELIB_PLAN §5.1)
///
/// A sink either **copies** the bytes it was handed or **takes** the buffer —
/// queues it for an asynchronous write, hands it to a transport, to DMA. The
/// encoder cannot tell the two apart, so the sink states which it did by what it
/// returns:
///
/// * **Copied** — return the same buffer with offset `0`. The encoder resumes
///   writing into it from the start.
/// * **Took** — return a *replacement* buffer and the offset to start at. The
///   encoder switches to it and never touches the one it gave away.
///
/// Rust makes the dangerous half impossible rather than merely forbidden: the
/// buffer arrives as an owned `&'a mut [u8]`, so a sink that keeps it has
/// nothing to give back and *must* supply a replacement to compile. There is no
/// "returned without installing anything while secretly retaining the memory"
/// state for this port to guard against.
///
/// The returned buffer is a mid-stream installation like any other, so its
/// capacity (`len - offset`) must be at least [`MIN_OUTPUT_BUFFER`]; a smaller
/// one is refused with [`Error::Argument`] at the flush that returned it. The
/// offset belongs to the installation and is consumed — returning the same
/// buffer with a non-zero offset is how a sink re-arms header room in *every*
/// flushed unit, one framing header per packet.
///
/// # Which of the two to implement
///
/// Implement this trait by hand only for a **taking** sink: keeping the buffer
/// means storing a `&'a mut [u8]`, and the lifetime it is stored under is
/// exactly the `'a` here — which is why the taking half needs a lifetime on the
/// trait and the copying half does not. Everything that copies, closures
/// included, implements [`Flush`] and reaches this trait through the blanket
/// impl below.
pub trait FlushTake<'a> {
    /// Consume the first `used` bytes of `buffer`, then say what to write into
    /// next: `(buffer, 0)` if the bytes were copied, or `(replacement, offset)`
    /// if `buffer` was taken.
    fn flush_take(&mut self, buffer: &'a mut [u8], used: usize) -> (&'a mut [u8], usize);
}

/// A copying sink is a [`FlushTake`] that hands the buffer straight back, at
/// **every** lifetime — which is what lets code bound only by [`Flush`], such as
/// a generated `serialize`, drive a stream over any buffer.
impl<'a, T: Flush> FlushTake<'a> for T {
    #[inline]
    fn flush_take(&mut self, buffer: &'a mut [u8], used: usize) -> (&'a mut [u8], usize) {
        self.flush(&buffer[..used]);
        (buffer, 0)
    }
}

/// A [`Flush`] sink that does nothing. Used as the default type parameter when
/// the stream is constructed without a sink; a full buffer then returns
/// [`Error::BufferFull`] and this sink is never called.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoFlush;

impl Flush for NoFlush {
    #[inline]
    fn flush(&mut self, _data: &[u8]) {}
}

/// Whether a buffer may be installed **with a sink**: its capacity must reach
/// [`MIN_OUTPUT_BUFFER`] (CORELIB_PLAN §5.1). `checked_sub` folds the
/// out-of-range-offset case in — an offset past the end has no capacity at all.
#[inline]
fn check_streaming_capacity(len: usize, offset: usize) -> Result<()> {
    match len.checked_sub(offset) {
        Some(capacity) if capacity >= MIN_OUTPUT_BUFFER => Ok(()),
        _ => Err(Error::Argument),
    }
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
/// unconditional allocation would be one malloc/free per message: setting
/// [`INLINE_PENDING`] to 0, which sends every id to the heap, measures 523
/// Ir/op on the `encode: typical message` Callgrind workload against this
/// crate's 248 — **+275**. The width of the inline array is worth far less than
/// its existence: at `INLINE_PENDING = 1` the same workload measures 243, i.e.
/// the eight slots cost ~5 Ir over one. See the README's Sequences section for
/// what the hold-back costs against pre-feature eager framing.
///
/// (The absolute figures moved when the encoder's varint and capacity handling
/// were reworked; the ratio they were chosen on did not.)
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
        // The whole run committed: the case every successful commit takes. The
        // general path below shifts a run-length range, which lowers to a
        // `memmove` call — a real one, even when the range is empty, because the
        // length is not a compile-time constant.
        if k == self.len() {
            self.n = 0;
            self.spill.clear();
            return;
        }
        if k == 0 {
            return;
        }
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
pub struct OStream<'a, F: FlushTake<'a> = NoFlush> {
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

impl<'a, F: FlushTake<'a>> OStream<'a, F> {
    /// Create an encoder with a flush `sink`, starting at `offset`. When the
    /// buffer fills, it is handed to `sink` under the [`FlushTake`] handover
    /// contract and writing resumes in whatever the sink left behind.
    ///
    /// # Panics
    ///
    /// If the buffer's capacity — `buffer.len() - offset`, which an offset past
    /// the end makes nonexistent — is below [`MIN_OUTPUT_BUFFER`]. A buffer that
    /// cannot hold a single byte would otherwise flush stale content, or none at
    /// all, partway through a message; the minimum is judged **here**, where the
    /// buffer is handed over, and nowhere later (CORELIB_PLAN §5.1).
    ///
    /// The buffer and its offset come from the calling code, never from decoded
    /// input, so this is a precondition in the same class as an out-of-range
    /// slice index — and §5.1 lets a port refuse by "an exception, or an error
    /// status". Use [`OStream::try_with_flush`] for the error-status form when
    /// the size is computed rather than known.
    ///
    /// The minimum applies only because a sink is attached. Use
    /// [`OStream::new`] / [`OStream::with_offset`] for a buffer without one:
    /// no flush can occur there, so no minimum binds it.
    #[inline]
    pub fn with_flush(buffer: &'a mut [u8], offset: usize, sink: F) -> Self {
        match Self::try_with_flush(buffer, offset, sink) {
            Ok(os) => os,
            Err(_) => panic!(
                "output buffer capacity below MIN_OUTPUT_BUFFER ({MIN_OUTPUT_BUFFER}) \
                 for a stream with a flush sink"
            ),
        }
    }

    /// [`OStream::with_flush`], reporting the capacity precondition as an error
    /// status instead of a panic.
    ///
    /// # Errors
    ///
    /// [`Error::Argument`] if `buffer.len() - offset` is below
    /// [`MIN_OUTPUT_BUFFER`], including the case of an offset past the end.
    #[inline]
    pub fn try_with_flush(buffer: &'a mut [u8], offset: usize, sink: F) -> Result<Self> {
        check_streaming_capacity(buffer.len(), offset)?;
        let end = buffer.len();
        Ok(OStream {
            buffer,
            end,
            offset,
            depth: 0,
            pending: PendingRun::default(), // heap-free until nesting spills
            flush: Some(sink),
        })
    }

    /// Number of bytes written to the active buffer since the last flush.
    #[inline]
    pub fn bytes_used(&self) -> usize {
        self.offset
    }

    /// Flush any pending bytes to the sink (if one is set) and report how many
    /// bytes were pending. With no sink the buffer is left intact.
    ///
    /// # Errors
    ///
    /// [`Error::Argument`] if the sink took the buffer and installed a
    /// replacement whose capacity is below [`MIN_OUTPUT_BUFFER`]. The bytes
    /// reached the sink either way; what failed is the next installation.
    pub fn flush(&mut self) -> Result<usize> {
        let used = self.offset;
        if used > 0 && self.flush.is_some() {
            self.hand_over()?;
        }
        Ok(used)
    }

    /// Replace the active buffer, resuming writes at `offset` in the new one.
    ///
    /// This is the mid-stream buffer-set of CORELIB_PLAN §5.1. A sink does not
    /// call it — it returns its replacement from [`FlushTake::flush_take`]
    /// instead, which is the only point at which the encoder is between buffers.
    /// Use this from the *outside*: to install a bigger buffer after a sinkless
    /// stream reported [`Error::BufferFull`], or to re-arm header room between
    /// messages.
    ///
    /// The offset belongs to this installation and is consumed by it.
    ///
    /// # Errors
    ///
    /// On a stream **with** a sink, [`Error::Argument`] if the new buffer's
    /// capacity (`buffer.len() - offset`) is below [`MIN_OUTPUT_BUFFER`]. A
    /// stream without a sink is subject to no minimum and this never fails.
    #[inline]
    pub fn buffer_set(&mut self, buffer: &'a mut [u8], offset: usize) -> Result<()> {
        if self.flush.is_some() {
            check_streaming_capacity(buffer.len(), offset)?;
        }
        self.end = buffer.len();
        self.buffer = buffer;
        self.offset = offset;
        Ok(())
    }

    /// Hand the active buffer to the sink and adopt whatever it leaves behind —
    /// the same buffer if it copied, a replacement if it took ours.
    ///
    /// The buffer is moved out with a `mem::take`, because handing a sink
    /// `&'a mut [u8]` is what lets it *keep* the memory; an empty slice stands in
    /// for the moment in between. Whatever comes back is installed before the
    /// capacity is judged, so a rejected replacement still cannot be written
    /// into: below [`MIN_OUTPUT_BUFFER`] means `offset >= end`, which routes the
    /// next write back through here rather than into the buffer.
    fn hand_over(&mut self) -> Result<()> {
        let used = self.offset;
        let buffer = core::mem::take(&mut self.buffer);
        let sink = self
            .flush
            .as_mut()
            .expect("hand_over is only reached with a sink installed");
        let (buffer, offset) = sink.flush_take(buffer, used);
        self.end = buffer.len();
        self.buffer = buffer;
        self.offset = offset;
        check_streaming_capacity(self.end, offset)
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
        if self.flush.is_none() {
            return Err(Error::BufferFull);
        }
        self.hand_over()
    }

    /// Copy a raw byte slice out, draining the buffer as needed. Uses a bulk
    /// `copy_from_slice` per buffer-sized run rather than a byte-at-a-time loop.
    #[inline]
    fn push_raw(&mut self, data: &[u8]) -> Result<()> {
        // The payload fits as it stands — the case for every string or blob
        // written into a buffer sized for the message. One copy, no loop.
        if self.offset + data.len() <= self.end {
            // SAFETY: `data.len()` writable bytes remain at `offset`, and the
            // caller's slice cannot alias the buffer (both are borrowed here).
            unsafe {
                core::ptr::copy_nonoverlapping(
                    data.as_ptr(),
                    self.buffer.as_mut_ptr().add(self.offset),
                    data.len(),
                );
            }
            self.offset += data.len();
            return Ok(());
        }
        self.push_raw_split(data)
    }

    /// [`OStream::push_raw`] for a payload longer than the room left: emit it in
    /// buffer-sized runs, draining to the sink between them.
    #[inline(never)]
    fn push_raw_split(&mut self, mut data: &[u8]) -> Result<()> {
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

    /// [`OStream::push_raw`] for a payload of statically known size — a float's
    /// wire bytes. When the run fits (it does unless the buffer is nearly full)
    /// this lowers to a single store instead of a `memcpy` call.
    #[inline]
    fn push_raw_fixed<const N: usize>(&mut self, data: &[u8; N]) -> Result<()> {
        if self.offset + N <= self.end {
            // SAFETY: `N` writable bytes remain at `offset`, just checked.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    data.as_ptr(),
                    self.buffer.as_mut_ptr().add(self.offset),
                    N,
                );
            }
            self.offset += N;
            Ok(())
        } else {
            self.push_raw(data)
        }
    }

    /// Encode `value` as a base-128 (LEB128) varint: 7 payload bits per byte,
    /// low byte first, with the high bit set on every byte but the last.
    ///
    /// One bounds check covers the whole varint: with a full varint's worth of
    /// room the bytes go out with no per-byte capacity test, buffer-full branch
    /// or `Result` to thread. Only within the last [`MAX_VARINT_LEN`] bytes of
    /// the buffer does it fall back to the byte-at-a-time writer that can flush
    /// mid-varint.
    #[inline]
    fn write_varint(&mut self, value: Unsigned) -> Result<()> {
        let offset = self.offset;
        if offset + MAX_VARINT_LEN <= self.end {
            // SAFETY: `MAX_VARINT_LEN` writable bytes remain at `offset`.
            let n = unsafe {
                write_varint_unchecked_narrow(self.buffer.as_mut_ptr().add(offset), value)
            };
            self.offset = offset + n;
            Ok(())
        } else {
            self.write_varint_split(value)
        }
    }

    /// Varint writer for the tail of the buffer, where the encoding may have to
    /// be split across a flush.
    #[inline(never)]
    fn write_varint_split(&mut self, mut value: Unsigned) -> Result<()> {
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
    /// suffix of the open sequences, so the run's own bookkeeping survives the
    /// failure intact.
    ///
    /// That is **not** a general recovery guarantee. No writer in this encoder
    /// is atomic on failure, and a header is a varint: the buffer end can fall
    /// *inside* one. The bytes already pushed stay in the buffer while the whole
    /// header stays pending, so a caller that installs a bigger buffer
    /// ([`OStream::buffer_set`]) and retries emits that header's leading bytes
    /// twice — `86 86 01` where id 16's `86 01` was meant, and `86 86 01` is
    /// itself a well-formed header, for sequence id **2144**: the corruption is
    /// silent. Retrying is byte-exact only when the cut fell **between** headers,
    /// which is every cut point when the run's ids are below 16 and their
    /// headers one byte wide. Both halves are pinned by
    /// `tests/ostream_tests.rs::recovery_after_a_cut_is_exact_only_on_a_header_boundary`.
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

    /// Write a field header followed by one varint value — the shape of every
    /// scalar field — under a **single** capacity check.
    ///
    /// A header and its value are two varints, and checking them separately
    /// costs the whole cursor round trip twice: load `offset`, load `end`,
    /// compare, store `offset`. Reserving both up front leaves the field as two
    /// register-to-memory encodings and one cursor update.
    ///
    /// This is a second choke point for field writes, so it carries
    /// [`OStream::write_id_type`]'s obligation too: writing a field is content,
    /// which proves every enclosing held-back sequence non-default. Only content
    /// wire types reach here — the sequence markers keep going through
    /// `write_id_type` — so the commit is unconditional rather than
    /// type-dependent.
    #[inline]
    fn write_field_varint(&mut self, id: Id, wire_type: u8, value: Unsigned) -> Result<()> {
        debug_assert!(wire_type != T_SEQUENCE_START && wire_type != T_SEQUENCE_END);
        if id > ID_MAX {
            return Err(Error::Argument);
        }
        if !self.pending.is_empty() {
            self.commit_pending()?;
        }
        let header = ((id as Unsigned) << 3) | wire_type as Unsigned;
        let offset = self.offset;
        if offset + 2 * MAX_VARINT_LEN <= self.end {
            let base = self.buffer.as_mut_ptr();
            // SAFETY: two full varints' worth of writable bytes remain.
            let mut off = offset;
            unsafe {
                off += write_varint_unchecked_narrow(base.add(off), header);
                off += write_varint_unchecked(base.add(off), value);
            }
            self.offset = off;
            return Ok(());
        }
        self.write_varint_split(header)?;
        self.write_varint_split(value)
    }

    // --- scalar writers -----------------------------------------------------

    /// Write an unsigned-integer field.
    #[inline]
    pub fn write_unsigned(&mut self, id: Id, value: Unsigned) -> Result<()> {
        self.write_field_varint(id, T_VARINT_UNSIGNED, value)
    }

    /// Write a signed-integer field (ZigZag + varint).
    #[inline]
    pub fn write_signed(&mut self, id: Id, value: Signed) -> Result<()> {
        self.write_field_varint(id, T_VARINT_SIGNED, zigzag_encode(value))
    }

    /// Write a boolean as an unsigned `0` / `1`.
    #[inline]
    pub fn write_boolean(&mut self, id: Id, value: bool) -> Result<()> {
        self.write_unsigned(id, value as Unsigned)
    }

    // --- fixed-length writers ----------------------------------------------

    /// Write a fixed-length field: header, `(len << 3) | subtype` varint, then
    /// the raw `data` bytes (already in wire/little-endian order for floats).
    ///
    /// The payload is validated **against the requested subtype** before any byte
    /// is written, so this byte-level entry point cannot produce a message a
    /// conformant decoder must reject (`Error::Argument`, CORELIB_PLAN §6.3):
    ///
    /// * `Fp32` / `Fp64` — the payload is **exactly** 4 / 8 bytes; a
    ///   `fixlen_word` declaring any other length for these subtypes is
    ///   malformed, the `INVALID` decode outcome (§4.6, §5.2).
    /// * `Str` — the payload must be valid UTF-8; a `string` that is not is
    ///   refused on encode, symmetrically with the decode-side rejection (§6.4,
    ///   MESSAGE_SPEC §8). Put arbitrary bytes in a `Blob` instead.
    /// * `Blob` — opaque, no constraint beyond the length ceiling.
    ///
    /// The typed writers ([`OStream::write_fp32`], [`OStream::write_fp64`],
    /// [`OStream::write_str`], [`OStream::write_blob`]) are correct by
    /// construction — a `&str` is UTF-8, `to_le_bytes` is the right width — and
    /// pay none of this: they go through the unchecked path.
    pub fn write_fixlen(&mut self, id: Id, data: &[u8], subtype: FixlenType) -> Result<()> {
        match subtype {
            FixlenType::Fp32 if data.len() != 4 => return Err(Error::Argument),
            FixlenType::Fp64 if data.len() != 8 => return Err(Error::Argument),
            FixlenType::Str if core::str::from_utf8(data).is_err() => return Err(Error::Argument),
            _ => {}
        }
        self.write_fixlen_unchecked(id, data, subtype)
    }

    /// [`OStream::write_fixlen`] without the subtype/payload agreement check —
    /// for callers whose payload the type system already constrains. The length
    /// ceiling is still enforced here; it is the one bound a `&[u8]` can break.
    #[inline]
    fn write_fixlen_unchecked(&mut self, id: Id, data: &[u8], subtype: FixlenType) -> Result<()> {
        if data.len() as u64 > FIXLEN_MAX {
            return Err(Error::Argument);
        }
        self.write_field_varint(
            id,
            T_FIXLEN,
            ((data.len() as Unsigned) << 3) | subtype as Unsigned,
        )?;
        self.push_raw(data)
    }

    /// A whole fixlen field whose payload has a statically known size — header,
    /// `(len << 3) | subtype` word and the raw bytes — under a single capacity
    /// check. Same bytes as [`OStream::write_fixlen`], minus the length check an
    /// `N`-byte payload cannot fail.
    ///
    /// Like [`OStream::write_field_varint`], this bypasses
    /// [`OStream::write_id_type`] and so carries its held-back-sequence
    /// obligation directly: a float field is content, and a struct whose first
    /// member is a float is exactly the case that proves the enclosing sequence
    /// non-default.
    #[inline]
    fn write_fixlen_fixed<const N: usize>(
        &mut self,
        id: Id,
        subtype: FixlenType,
        data: &[u8; N],
    ) -> Result<()> {
        if id > ID_MAX {
            return Err(Error::Argument);
        }
        if !self.pending.is_empty() {
            self.commit_pending()?;
        }
        let header = ((id as Unsigned) << 3) | T_FIXLEN as Unsigned;
        let word = ((N as Unsigned) << 3) | subtype as Unsigned;
        let offset = self.offset;
        if offset + 2 * MAX_VARINT_LEN + N <= self.end {
            let base = self.buffer.as_mut_ptr();
            let mut off = offset;
            // SAFETY: header, word and payload all fit in the reserved run.
            unsafe {
                off += write_varint_unchecked_narrow(base.add(off), header);
                off += write_varint_unchecked_narrow(base.add(off), word);
                core::ptr::copy_nonoverlapping(data.as_ptr(), base.add(off), N);
            }
            self.offset = off + N;
            return Ok(());
        }
        self.write_varint_split(header)?;
        self.write_varint_split(word)?;
        self.push_raw(data)
    }

    /// Write a 32-bit float field.
    #[inline]
    pub fn write_fp32(&mut self, id: Id, value: f32) -> Result<()> {
        self.write_fixlen_fixed(id, FixlenType::Fp32, &value.to_le_bytes())
    }

    /// Write a 64-bit float field.
    #[inline]
    pub fn write_fp64(&mut self, id: Id, value: f64) -> Result<()> {
        self.write_fixlen_fixed(id, FixlenType::Fp64, &value.to_le_bytes())
    }

    /// Write a string field (raw UTF-8 bytes, no NUL on the wire).
    ///
    /// The input is `&str`, so it is **already valid UTF-8** by the type system
    /// — encode is strict by construction here and costs nothing at runtime
    /// (MESSAGE_SPEC §8, CORELIB_PLAN §6.4); the byte-level
    /// [`OStream::write_fixlen`] pays a validation pass instead. For arbitrary
    /// bytes use [`OStream::write_blob`]. Embedded `U+0000` is permitted and
    /// written verbatim (the wire is length-framed, no NUL terminator).
    #[inline]
    pub fn write_str(&mut self, id: Id, text: &str) -> Result<()> {
        self.write_fixlen_unchecked(id, text.as_bytes(), FixlenType::Str)
    }

    /// Write a binary blob field.
    #[inline]
    pub fn write_blob(&mut self, id: Id, data: &[u8]) -> Result<()> {
        self.write_fixlen_unchecked(id, data, FixlenType::Blob)
    }

    // --- array writers ------------------------------------------------------

    /// Write `data.len()` varints, in runs sized to the room left in the buffer.
    ///
    /// The capacity test is per *run*, not per element: with `k` full varints'
    /// worth of space free, the next `k` elements go out through a local cursor
    /// with no bounds check, no buffer reload and no `Result` between them. Only
    /// the element that straddles the end of the buffer takes the byte-at-a-time
    /// path that can flush mid-varint.
    #[inline]
    fn write_varint_run<T: Copy, W: Fn(T) -> Unsigned>(
        &mut self,
        data: &[T],
        to_wire: W,
    ) -> Result<()> {
        // Whole array fits with room to spare: one multiply decides it, and the
        // element loop then carries no bookkeeping at all. This is the case for
        // any array written into a buffer sized for the message.
        if data.len().saturating_mul(MAX_VARINT_LEN) <= self.end - self.offset {
            let base = self.buffer.as_mut_ptr();
            let mut off = self.offset;
            for &e in data {
                // SAFETY: every element has `MAX_VARINT_LEN` bytes of headroom,
                // checked in bulk above.
                off += unsafe { write_varint_unchecked(base.add(off), to_wire(e)) };
            }
            self.offset = off;
            return Ok(());
        }

        self.write_varint_run_chunked(data, to_wire)
    }

    /// [`OStream::write_varint_run`] when the array does not fit in what is left
    /// of the buffer: write it in runs sized to the room available, draining to
    /// the sink in between. Outlined so its bookkeeping stays out of the
    /// fits-in-one-go loop above.
    #[inline(never)]
    fn write_varint_run_chunked<T: Copy, W: Fn(T) -> Unsigned>(
        &mut self,
        data: &[T],
        to_wire: W,
    ) -> Result<()> {
        let mut i = 0;
        while i < data.len() {
            let room = (self.end - self.offset) / MAX_VARINT_LEN;
            if room == 0 {
                // Within a varint's reach of the end: let the splitting writer
                // flush (or report `BufferFull`) for this one element.
                self.write_varint_split(to_wire(data[i]))?;
                i += 1;
                continue;
            }
            let run = room.min(data.len() - i);
            let base = self.buffer.as_mut_ptr();
            let mut off = self.offset;
            for &e in &data[i..i + run] {
                // SAFETY: `off` has at least `MAX_VARINT_LEN` writable bytes —
                // `run` was capped at the number of whole varints that fit.
                off += unsafe { write_varint_unchecked(base.add(off), to_wire(e)) };
            }
            self.offset = off;
            i += run;
        }
        Ok(())
    }

    /// Write an array of unsigned integers (`u8`/`u16`/`u32`/`u64` elements).
    ///
    /// A zero-count array is a valid empty array on the wire — it encodes as
    /// exactly `[ header ][ element_count = 0 ]` with no elements (§4.7).
    pub fn write_array_unsigned<T: UnsignedElem>(&mut self, id: Id, data: &[T]) -> Result<()> {
        if data.len() as u64 > ARRAY_MAX {
            return Err(Error::Argument);
        }
        self.write_field_varint(id, T_VARINTARRAY_UNSIGNED, data.len() as Unsigned)?;
        self.write_varint_run(data, T::widen)
    }

    /// Write an array of signed integers (`i8`/`i16`/`i32`/`i64` elements).
    ///
    /// A zero-count array encodes as exactly `[ header ][ element_count = 0 ]`
    /// with no elements (§4.7).
    pub fn write_array_signed<T: SignedElem>(&mut self, id: Id, data: &[T]) -> Result<()> {
        if data.len() as u64 > ARRAY_MAX {
            return Err(Error::Argument);
        }
        self.write_field_varint(id, T_VARINTARRAY_SIGNED, data.len() as Unsigned)?;
        self.write_varint_run(data, |e: T| zigzag_encode(e.widen()))
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
        self.write_field_varint(id, T_FIXLENARRAY, data.len() as Unsigned)?;
        self.write_varint((4 << 3) | FixlenType::Fp32 as Unsigned)?;
        for &e in data {
            self.push_raw_fixed(&e.to_le_bytes())?;
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
        self.write_field_varint(id, T_FIXLENARRAY, data.len() as Unsigned)?;
        self.write_varint((8 << 3) | FixlenType::Fp64 as Unsigned)?;
        for &e in data {
            self.push_raw_fixed(&e.to_le_bytes())?;
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
