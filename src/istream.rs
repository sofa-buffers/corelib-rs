//! Streaming input stream decoder.
//!
//! Two ways in, one [`Visitor`]:
//!
//! * [`decode`] — the **fast contiguous path**. Hand it a complete message and
//!   it advances a cursor over the buffer, decoding every field with no copies;
//!   string/blob payloads are delivered as a single borrowed slice straight out
//!   of your buffer. This is the 90 % case on a server and the speed showcase.
//! * [`IStream`] — the **streaming path** (CORELIB_PLAN §5.2). Feed it bytes in
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
//!
//! **What is absent never calls back.** A field equal to its declared default is
//! omitted by MESSAGE_SPEC §2 — for a sequence-typed field that means the whole
//! frame is gone, so not even [`Visitor::sequence_begin`] runs, and an all-default
//! message is the empty byte string that produces no callbacks whatsoever. §5.1
//! puts the matching duty on the decoding side: initialise the destination to its
//! declared defaults *before* applying a message, never from a callback that a
//! default-valued field will not fire. See [`Visitor::sequence_begin`] for the
//! failure this prevents when a destination is reused across messages.

use crate::error::{Error, Result};
use crate::types::*;
use crate::varint::{read_varint, read_varint_ready, zigzag_decode, MAX_VARINT_LEN};
use crate::{ArrayKind, FixlenType, Id, Signed, Unsigned};

// The fixlen subtype tags as they appear in the low 3 bits of a fixlen word,
// for matching the wire byte directly (see `FixlenType`).
const FX_FP32: u8 = FixlenType::Fp32 as u8;
const FX_FP64: u8 = FixlenType::Fp64 as u8;
const FX_STR: u8 = FixlenType::Str as u8;
const FX_BLOB: u8 = FixlenType::Blob as u8;

/// Receives decoded fields from [`IStream`] / [`decode`].
///
/// Every method has a default empty implementation, so an implementor overrides
/// only the field kinds it cares about. Fields that are not handled are simply
/// dropped (the equivalent of "not interested" / skip in the C API).
///
/// A field equal to its declared default is **not on the wire** (MESSAGE_SPEC
/// §2), so no method fires for it — including whole sequences, see
/// [`Visitor::sequence_begin`]. Initialise the destination to its defaults
/// *before* decoding rather than from a callback (§5.1).
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
    ///
    /// The bytes are delivered **raw**: the corelib does not validate UTF-8 or
    /// build a `str`/`String`. A strict consumer (generated code) materializes
    /// the field with `core::str::from_utf8` and reports invalid bytes as
    /// [`Error::InvalidMsg`] — never replacing them with `U+FFFD` or truncating
    /// (MESSAGE_SPEC §8, CORELIB_PLAN §6.4). `blob` payloads (below) are opaque
    /// and never UTF-8-checked.
    fn string(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {}

    /// A chunk of a blob field. See [`Visitor::string`] for the chunking model.
    fn blob(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {}

    /// Start of a scalar fixlen field, announced after its length word is read
    /// and validated and **before** any payload byte. Fired exactly **once** per
    /// field, `total == 0` included, and never for an array element (an array is
    /// announced through [`Visitor::array_begin`] instead).
    ///
    /// This is the scalar twin of [`Visitor::array_begin`], and exists for the
    /// same reason: a schema bound established by the length word alone — a
    /// `string`/`blob` whose `total` exceeds a `maxlen` — must be latchable *at
    /// the word*. CORELIB_PLAN §5.2 makes INVALID dominate INCOMPLETE, so a
    /// message truncated exactly at the length word cannot be allowed to degrade
    /// to INCOMPLETE while the same bytes read whole are INVALID (a
    /// chunk-boundary-dependent verdict §6.4/§7.2 forbid). Without this callback
    /// the only event carrying `total` is [`Visitor::string`] / [`Visitor::blob`]
    /// on the payload path, which cannot fire for a message that ends there.
    /// Raising from this callback is what a consumer uses to turn the field
    /// INVALID at the word.
    ///
    /// `subtype` is the subtype actually on the wire (`Str` / `Blob` / `Fp32` /
    /// `Fp64`): the corelib knows what *arrived*, not what was *declared*, so a
    /// consumer whose field expects a different subtype treats this as a §7.3
    /// skip rather than measuring `total` against that field's bound. For a
    /// float, `total` is the fixed width (4 or 8), already validated here; a
    /// malformed float width is rejected before this fires, so nothing is
    /// announced for it.
    fn fixlen_begin(&mut self, id: Id, subtype: FixlenType, total: usize) {}

    /// Start of an array field with `count` elements of the given `kind`. The
    /// elements follow via the scalar / float callbacks with the same `id`.
    ///
    /// Called **exactly once** per array field — never per element — and always
    /// before the first element callback. `count == 0` is no exception: an
    /// empty array still reports its kind, and is followed by no element
    /// callback at all.
    ///
    /// For a fixlen array the call is made only after the `fixlen_word` has been
    /// read and validated, so `kind` names the element subtype
    /// ([`ArrayKind::Fp32`] / [`ArrayKind::Fp64`]) rather than "some fixlen
    /// array". A receiver that bounds the array against a schema-declared
    /// element count MUST first check `kind` against the declared element type:
    /// a contradicting kind means the field is skipped under MESSAGE_SPEC §7.3
    /// and the schema bound MUST NOT be applied, because the field was never
    /// this array's value (CORELIB_PLAN §4.8 step 3). Integer arrays carry no
    /// second word, so their call is made right after the count varint.
    ///
    /// The **format** ceiling `ARRAY_MAX` (2^31-1) is enforced by the corelib on
    /// the count word, before this call and before the `fixlen_word` is read;
    /// `count` is therefore always within that ceiling here, and nothing has
    /// been allocated on the strength of it.
    fn array_begin(&mut self, id: Id, kind: ArrayKind, count: usize) {}

    /// Start of a nested sequence with the given field `id`.
    ///
    /// **Absence is meaningful: this is not a reset hook.** MESSAGE_SPEC §2 omits
    /// a sequence-typed *field* whose value equals its declared default, so an
    /// all-default `struct`/`union`/array field arrives as **no callback at
    /// all** — no `sequence_begin`, no [`Visitor::sequence_end`], no children.
    /// (An all-default message is in turn the empty byte string, which decodes to
    /// zero callbacks of any kind.) Clearing or preparing a destination slot from
    /// inside this method therefore silently keeps the previous message's data
    /// whenever the field is default, because the method never runs.
    ///
    /// The duty is on the decoding side, and MESSAGE_SPEC §5.1 states it
    /// unconditionally: *before* applying a message, initialise every destination
    /// slot to its declared default — decode into a fresh, default-constructed
    /// destination, or reset it explicitly before [`decode`] / the first
    /// [`IStream::feed`]. With that in place the omission is lossless by
    /// construction: absent reconstructs to the default. Prepare it from a
    /// callback instead and only non-default fields ever get prepared.
    ///
    /// A wrapper-array **element** is the one case that cuts the other way: it
    /// keeps its frame even when all-default, because element presence is what
    /// carries a dynamic array's length (§5.1). So `sequence_begin` does fire once
    /// per present element — it is the enclosing *field* that can disappear.
    ///
    /// ```
    /// use sofab::{decode, Id, OStream, Unsigned, Visitor};
    ///
    /// #[derive(Default)]
    /// struct Dest { elems: Vec<Unsigned> }
    /// impl Visitor for Dest {
    ///     fn unsigned(&mut self, _id: Id, v: Unsigned) { self.elems.push(v); }
    /// }
    ///
    /// // Message A: array field id 4 carrying two elements.
    /// let mut buf = [0u8; 32];
    /// let n = {
    ///     let mut os = OStream::new(&mut buf);
    ///     os.write_sequence_begin_lazy(4).unwrap();
    ///     os.write_sequence_begin_lazy(0).unwrap();
    ///     os.write_unsigned(0, 10).unwrap();
    ///     os.write_sequence_end_keep().unwrap();   // elements keep their frame
    ///     os.write_sequence_begin_lazy(1).unwrap();
    ///     os.write_unsigned(0, 11).unwrap();
    ///     os.write_sequence_end_keep().unwrap();
    ///     os.write_sequence_end().unwrap();
    ///     os.bytes_used()
    /// };
    /// let a = &buf[..n];
    /// // Message B: the same field all-default. §2 omits it, so B is *empty*.
    /// let b: &[u8] = &[];
    ///
    /// // Reusing a destination across messages: B calls nothing at all, so A's
    /// // elements are still sitting there. Nothing the visitor does can fix this.
    /// let mut reused = Dest::default();
    /// decode(a, &mut reused).unwrap();
    /// decode(b, &mut reused).unwrap();
    /// assert_eq!(reused.elems, [10, 11]);   // stale: not what B means
    ///
    /// // Correct: the destination starts at its defaults for each message.
    /// let mut fresh = Dest::default();
    /// decode(b, &mut fresh).unwrap();
    /// assert!(fresh.elems.is_empty());      // absent reconstructs to the default
    /// ```
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
    /// Mid fixlen (float) array: `remaining` elements still to read. The element
    /// width is implied by `fp64` (4 or 8 bytes — §4.8 admits no other).
    ArrayFix {
        id: Id,
        fp64: bool,
        remaining: usize,
    },
}

/// Capacity of the carry buffer: the widest prefix of a single wire item that
/// can outlive a chunk boundary, derived from this format's own constants
/// (CORELIB_PLAN §6.6, "bounded working state … MUST be sized at construction").
///
/// `parse` backs the cursor up to the start of the *field* whenever a small item
/// runs out of bytes, so what is carried is a whole field prefix, not a lone
/// varint. The widest such prefix is `ARRAY_FIXLEN`'s: a field header, the
/// element count, and a `fixlen_word` cut before its terminator — three varints,
/// hence `3 × MAX_VARINT_LEN`. (A truncated varint is at most
/// `MAX_VARINT_LEN - 1` bytes — with ten readable the value is decided, valid or
/// not — so the real worst case is 29 and this leaves a byte of slack.)
///
/// Nothing else reaches the carry: string/blob payloads and array elements
/// suspend through [`Resume`] and are handed over in pieces, never buffered.
pub(crate) const CARRY_CAP: usize = 3 * MAX_VARINT_LEN;

/// Streaming Sofab decoder. Reusable across messages via [`IStream::reset`].
pub struct IStream {
    /// Bytes of an item that straddled a chunk boundary, carried to the next
    /// `feed`. A fixed inline array sized at construction — the decoder
    /// allocates nothing, on any path, and the caller's chunking cannot choose
    /// how much memory it holds (CORELIB_PLAN §6.6). Only ever holds a partial
    /// small item (field header / length or count word / partial fixed-width
    /// scalar); large payloads are streamed, not buffered.
    carry: [u8; CARRY_CAP],
    /// How many bytes of `carry` are live.
    carry_len: usize,
    resume: Resume,
    /// Nested sequence depth, for balanced start/end validation.
    depth: u32,
    /// Latched `INVALID` verdict. §5.2 marks that outcome **terminal**: once the
    /// consumed bytes are malformed no continuation can make them well-formed,
    /// so every later [`IStream::feed`] repeats the rejection instead of parsing
    /// a fresh field out of the following bytes. [`IStream::reset`] clears it.
    invalid: bool,
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
            carry: [0u8; CARRY_CAP],
            carry_len: 0,
            resume: Resume::None,
            depth: 0,
            invalid: false,
        }
    }

    /// Reset to the initial state so the decoder can be reused for a new message.
    ///
    /// This is also the only way out of a latched `INVALID` verdict: a decoder
    /// that has rejected its input stays rejecting until it is reset (§5.2).
    pub fn reset(&mut self) {
        self.carry_len = 0;
        self.resume = Resume::None;
        self.depth = 0;
        self.invalid = false;
    }

    /// Feed a chunk of encoded bytes, pushing decoded fields to `visitor`.
    ///
    /// Surfaces the three decode outcomes of MESSAGE_SPEC §7 for the bytes
    /// consumed so far — **there is no separate finalize step**:
    ///
    /// * `Ok(())` — **complete**: the consumed bytes end exactly at a field
    ///   boundary (a whole, valid message so far).
    /// * [`Err(Error::Incomplete)`](Error::Incomplete) — the bytes end **inside**
    ///   a field: a partial varint, a payload shorter than declared, or an open
    ///   sequence. This is *not* a rejection; the decoder keeps all state
    ///   internally, so the caller simply feeds the next chunk to continue.
    /// * [`Err(Error::InvalidMsg)`](Error::InvalidMsg) — the bytes are malformed
    ///   regardless of what follows and decoding cannot continue.
    ///
    /// The distinction matters: a truncated tail returns `Incomplete`, never
    /// `InvalidMsg`. The caller — not the decoder — owns end-of-input.
    ///
    /// `InvalidMsg` is **terminal for this decoder** (§5.2, "can more bytes
    /// change it? — no"). Once a `feed` has reported it, every further `feed`
    /// reports it again — a complete valid message, an empty end-of-input probe
    /// and a truncated prefix alike — rather than resynchronizing on the bytes
    /// that follow the malformed construct. Without that latch the verdict would
    /// depend on where the chunk boundaries fell, which §7.2 forbids: the same
    /// bytes must decode to the same outcome fed whole or one byte at a time.
    /// [`reset`](Self::reset) clears the latch and readies the decoder for a new
    /// message.
    pub fn feed<V: Visitor>(&mut self, chunk: &[u8], visitor: &mut V) -> Result<()> {
        if self.invalid {
            return Err(Error::InvalidMsg);
        }
        // A small item straddled the previous boundary: complete it out of the
        // carry, then parse whatever is left of the chunk in place. Only the few
        // bytes the pending item still needs are ever copied — the caller's
        // chunk size never decides how much memory this decoder touches.
        let rest = if self.carry_len == 0 {
            chunk
        } else {
            match self.resume_carried(chunk, visitor)? {
                Some(rest) => rest,
                // The whole chunk went into the carry: nothing left to parse.
                None => return self.verdict(),
            }
        };

        // Fast path: parse straight from the caller's slice, no copy.
        //
        // A chunk that continues a long string/blob payload — every chunk of
        // a streamed payload but the first — is handed over **here** rather
        // than by the field parser: delivering one is a dozen instructions,
        // and routing it through `parse` spends that parser's whole call
        // frame on a chunk that holds no field at all. Only if the payload
        // ends inside the chunk does the parser see the remainder.
        //
        // (`carry` and `resume` are never both set: an item suspends only by
        // running out of bytes, which means it consumed the chunk whole.
        // `parse` resumes a payload for itself all the same, so nothing
        // rests on that.)
        let consumed = if matches!(self.resume, Resume::Payload { .. }) {
            let pos = self.deliver_payload(rest, 0, visitor);
            if matches!(self.resume, Resume::None) {
                pos + self
                    .parse(&rest[pos..], visitor)
                    .map_err(|e| self.latch(e))?
            } else {
                pos // the whole chunk went into the payload
            }
        } else {
            self.parse(rest, visitor).map_err(|e| self.latch(e))?
        };
        if consumed < rest.len() {
            self.stash(&rest[consumed..])?;
        }
        self.verdict()
    }

    /// Complete the item held in `carry` using the head of `chunk`, and report
    /// what is left of `chunk` for the in-place fast path — or `None` when the
    /// chunk was absorbed whole and the item is still short.
    ///
    /// At most [`CARRY_CAP`] bytes are copied, whatever the chunk's size: the
    /// carry is topped up to its fixed capacity and parsed out of a stack copy,
    /// so the remainder of the chunk is still parsed in place.
    ///
    /// Cold: it runs only for a chunk boundary that fell inside a field header
    /// or a length word, never for a whole-message `feed`.
    #[cold]
    #[inline(never)]
    fn resume_carried<'c, V: Visitor>(
        &mut self,
        chunk: &'c [u8],
        visitor: &mut V,
    ) -> Result<Option<&'c [u8]>> {
        let held = self.carry_len;
        let take = (CARRY_CAP - held).min(chunk.len());
        self.carry[held..held + take].copy_from_slice(&chunk[..take]);
        let stitched = held + take;

        // A `[u8; CARRY_CAP]` is `Copy`, so the stitched bytes can be parsed out
        // of a local without holding a borrow of `self` across the call.
        let buf = self.carry;
        // On the error path the carry stays as it is: the verdict is terminal,
        // so there is nothing left to stitch the next chunk onto.
        let consumed = self
            .parse(&buf[..stitched], visitor)
            .map_err(|e| self.latch(e))?;

        if consumed >= held {
            // The carried prefix reached the wire in full; `consumed - held`
            // bytes of the caller's chunk went with it.
            self.carry_len = 0;
            return Ok(Some(&chunk[consumed - held..]));
        }

        // Still short of a complete item. `parse` consumes nothing until the
        // item it is carrying completes, so the chunk went in whole.
        if consumed > 0 {
            self.carry.copy_within(consumed..stitched, 0);
        }
        self.carry_len = stitched - consumed;
        debug_assert!(
            take == chunk.len(),
            "CARRY_CAP is wider than any carryable item, so a full carry always \
             completes or rejects"
        );
        if take < chunk.len() {
            // Unreachable, and deliberately not silent: a carry that filled to
            // CARRY_CAP without the item completing would mean an item wider
            // than this format admits, which is malformed either way.
            return Err(self.latch(Error::InvalidMsg));
        }
        Ok(None)
    }

    /// Hold `tail` — the prefix of an item the chunk ran out inside — until the
    /// next `feed`.
    #[inline]
    fn stash(&mut self, tail: &[u8]) -> Result<()> {
        if tail.len() > CARRY_CAP {
            // Unreachable: `parse` returns short only at an incomplete small
            // item, whose widest prefix is what CARRY_CAP is derived from.
            debug_assert!(false, "carried item wider than CARRY_CAP");
            return Err(self.latch(Error::InvalidMsg));
        }
        self.carry[..tail.len()].copy_from_slice(tail);
        self.carry_len = tail.len();
        Ok(())
    }

    /// The three-valued outcome for the bytes consumed so far.
    #[inline]
    fn verdict(&self) -> Result<()> {
        if self.at_boundary() {
            Ok(())
        } else {
            // Ended mid-field / mid-payload / inside an open sequence: distinct
            // from both COMPLETE (Ok) and INVALID (InvalidMsg). Not a rejection.
            Err(Error::Incomplete)
        }
    }

    /// Record a terminal verdict and hand the error straight back.
    ///
    /// Only `InvalidMsg` latches: it is the one outcome §5.2 calls terminal.
    /// `Incomplete` is not an error on this path (it is derived from
    /// `at_boundary` after a successful parse), and a receiver-side
    /// `LimitExceeded` is policy rather than malformation, so neither may
    /// poison a decoder here.
    #[cold]
    fn latch(&mut self, e: Error) -> Error {
        if e == Error::InvalidMsg {
            self.invalid = true;
        }
        e
    }

    /// True when the decoder sits at a clean message boundary: no half-read
    /// field carried over, no in-progress payload/array, no open sequence.
    ///
    /// There is deliberately **no** public `finish`/`finalize`: the three-valued
    /// verdict is obtained solely from [`feed`](Self::feed)'s return value at
    /// every byte boundary, keeping the decode surface identical across every
    /// `corelib-*` port (MESSAGE_SPEC §7). To probe end-of-input without more
    /// bytes, feed an empty chunk — `feed(&[], v)` returns `Ok(())` iff the
    /// stream ended at a clean boundary, `Err(Incomplete)` otherwise.
    fn at_boundary(&self) -> bool {
        self.carry_len == 0 && matches!(self.resume, Resume::None) && self.depth == 0
    }

    /// Parse as many complete fields as possible from `buf`, returning the number
    /// of bytes fully consumed. Whatever follows the returned offset is an
    /// incomplete small item the caller must carry to the next chunk. Long
    /// payloads (string/blob) and array progress are committed via `self.resume`,
    /// so they are never re-delivered.
    fn parse<V: Visitor>(&mut self, buf: &[u8], v: &mut V) -> Result<usize> {
        let mut pos = 0usize;

        // 1) Finish anything left in progress from a previous chunk. At most one
        //    item can ever be in progress, so this is a one-time preamble rather
        //    than a per-field dispatch inside the loop below.
        if !matches!(self.resume, Resume::None) {
            pos = self.resume_item(buf, pos, v)?;
            if !matches!(self.resume, Resume::None) {
                // This chunk did not finish it either.
                return Ok(pos);
            }
        }

        // 2) Field loop. `self.resume` is `None` throughout — an item that runs
        //    out of bytes sets it and returns immediately.
        loop {
            if pos >= buf.len() {
                return Ok(pos);
            }
            let field_start = pos;
            let header = match read_varint(buf, &mut pos) {
                Ok(h) => h,
                Err(Error::Incomplete) => return Ok(field_start),
                Err(e) => return Err(e),
            };
            let wire = (header & 0x07) as u8;
            let id_raw = header >> 3;
            if id_raw > ID_MAX as Unsigned {
                return Err(Error::InvalidMsg);
            }
            let id = id_raw as Id;

            match wire {
                T_VARINT_UNSIGNED => match read_varint(buf, &mut pos) {
                    Ok(val) => v.unsigned(id, val),
                    Err(Error::Incomplete) => return Ok(field_start),
                    Err(e) => return Err(e),
                },
                T_VARINT_SIGNED => match read_varint(buf, &mut pos) {
                    Ok(zz) => v.signed(id, zigzag_decode(zz)),
                    Err(Error::Incomplete) => return Ok(field_start),
                    Err(e) => return Err(e),
                },

                T_FIXLEN => {
                    let word = match read_varint(buf, &mut pos) {
                        Ok(w) => w,
                        Err(Error::Incomplete) => return Ok(field_start),
                        Err(e) => return Err(e),
                    };
                    // A scalar fixlen field's declared length is bounded by
                    // `FIXLEN_MAX` (§4.6, §6.2) — the fixlen ceiling, not the
                    // array one: §6.2 lists the two as independently settable,
                    // and a count word is what `ARRAY_MAX` bounds (§4.7/§4.8).
                    if (word >> 3) as u64 > FIXLEN_MAX {
                        return Err(Error::InvalidMsg);
                    }
                    let len = (word >> 3) as usize;
                    // Dispatch straight on the 3-bit subtype tag. Going through
                    // `FixlenType::from_raw` first would decode the tag into an
                    // enum only to branch on it again — two jump tables where
                    // the wire needs one. An unknown tag falls through to the
                    // `InvalidMsg` arm exactly as `from_raw` would report it.
                    //
                    // Each arm announces the field at its length word via
                    // `fixlen_begin` — after the word is read and validated,
                    // before any payload byte — so a schema `maxlen` violation is
                    // latchable *here*: a message ending exactly at this word must
                    // stay INVALID rather than degrade to INCOMPLETE (§5.2, and see
                    // [`Visitor::fixlen_begin`]). It is the scalar twin of the
                    // `array_begin` on the array arms below; the array wire types
                    // dispatch elsewhere, so this fires once per scalar fixlen
                    // field, `total == 0` included. A malformed float width is
                    // rejected first, so nothing is announced for it.
                    match (word & 0x07) as u8 {
                        FX_FP32 => {
                            if len != 4 {
                                return Err(Error::InvalidMsg);
                            }
                            v.fixlen_begin(id, FixlenType::Fp32, len);
                            // A scalar float is a one-element fixed-width run on
                            // the wire; deliver it through `fix_array` (count 1)
                            // so a value straddling the chunk boundary resumes via
                            // `Resume::ArrayFix` instead of re-parsing the header —
                            // which would re-fire `fixlen_begin`.
                            pos = self.fix_array::<false, V>(buf, pos, id, 1, v);
                            if !matches!(self.resume, Resume::None) {
                                return Ok(pos);
                            }
                        }
                        FX_FP64 => {
                            if len != 8 {
                                return Err(Error::InvalidMsg);
                            }
                            v.fixlen_begin(id, FixlenType::Fp64, len);
                            pos = self.fix_array::<true, V>(buf, pos, id, 1, v);
                            if !matches!(self.resume, Resume::None) {
                                return Ok(pos);
                            }
                        }
                        tag @ (FX_STR | FX_BLOB) => {
                            let is_blob = tag == FX_BLOB;
                            let subtype = if is_blob {
                                FixlenType::Blob
                            } else {
                                FixlenType::Str
                            };
                            v.fixlen_begin(id, subtype, len);
                            if buf.len() - pos >= len {
                                // Whole payload present: hand it over as one
                                // borrowed slice, no `resume` bookkeeping. This
                                // is the contiguous zero-copy case, including
                                // the empty payload (`len == 0`).
                                let chunk = &buf[pos..pos + len];
                                if is_blob {
                                    v.blob(id, len, 0, chunk);
                                } else {
                                    v.string(id, len, 0, chunk);
                                }
                                pos += len;
                            } else {
                                // Straddles the chunk boundary: deliver what is
                                // here and suspend for the rest.
                                self.resume = Resume::Payload {
                                    id,
                                    is_blob,
                                    total: len,
                                    remaining: len,
                                };
                                return Ok(self.deliver_payload(buf, pos, v));
                            }
                        }
                        _ => return Err(Error::InvalidMsg),
                    }
                }

                T_VARINTARRAY_UNSIGNED => {
                    let count = match read_varint(buf, &mut pos) {
                        Ok(c) => c,
                        Err(Error::Incomplete) => return Ok(field_start),
                        Err(e) => return Err(e),
                    };
                    if count > ARRAY_MAX {
                        return Err(Error::InvalidMsg);
                    }
                    v.array_begin(id, ArrayKind::Unsigned, count as usize);
                    pos = self.int_array::<false, V>(buf, pos, id, count as usize, v)?;
                    if !matches!(self.resume, Resume::None) {
                        return Ok(pos);
                    }
                }
                T_VARINTARRAY_SIGNED => {
                    let count = match read_varint(buf, &mut pos) {
                        Ok(c) => c,
                        Err(Error::Incomplete) => return Ok(field_start),
                        Err(e) => return Err(e),
                    };
                    if count > ARRAY_MAX {
                        return Err(Error::InvalidMsg);
                    }
                    v.array_begin(id, ArrayKind::Signed, count as usize);
                    pos = self.int_array::<true, V>(buf, pos, id, count as usize, v)?;
                    if !matches!(self.resume, Resume::None) {
                        return Ok(pos);
                    }
                }
                T_FIXLENARRAY => {
                    let count = match read_varint(buf, &mut pos) {
                        Ok(c) => c,
                        Err(Error::Incomplete) => return Ok(field_start),
                        Err(e) => return Err(e),
                    };
                    if count > ARRAY_MAX {
                        return Err(Error::InvalidMsg);
                    }
                    // A fixlen array **always** carries its `fixlen_word`, even
                    // when empty (count == 0) — this is what distinguishes an
                    // empty fp32 array from an empty fp64 array on the wire
                    // (§4.8).
                    let word = match read_varint(buf, &mut pos) {
                        Ok(w) => w,
                        Err(Error::Incomplete) => return Ok(field_start),
                        Err(e) => return Err(e),
                    };
                    // Only fixed-width float subtypes are valid in a fixlen
                    // array; string/blob must use a sequence instead. Subtype
                    // and element width are one test: each float subtype admits
                    // exactly one width (§4.8).
                    let fp64 = match ((word & 0x07) as u8, word >> 3) {
                        (FX_FP32, 4) => false,
                        (FX_FP64, 8) => true,
                        _ => return Err(Error::InvalidMsg),
                    };
                    // The hook fires only here, past the `fixlen_word`, and
                    // names the element subtype: generated code has to decide
                    // fp32-vs-fp64 *before* it may apply a schema `count` bound,
                    // because a contradicting subtype means the field is skipped
                    // and was never this array's value (§4.8 step 3, §7.3).
                    // Fires exactly once per array field, `count == 0` included.
                    let kind = if fp64 {
                        ArrayKind::Fp64
                    } else {
                        ArrayKind::Fp32
                    };
                    v.array_begin(id, kind, count as usize);
                    pos = if fp64 {
                        self.fix_array::<true, V>(buf, pos, id, count as usize, v)
                    } else {
                        self.fix_array::<false, V>(buf, pos, id, count as usize, v)
                    };
                    if !matches!(self.resume, Resume::None) {
                        return Ok(pos);
                    }
                }

                T_SEQUENCE_START => {
                    // Reject nesting beyond MAX_DEPTH (255) rather than risk
                    // unbounded recursion / stack growth (§4.9, §6.2).
                    if self.depth >= MAX_DEPTH {
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

    /// Continue the one item that was in progress when the previous chunk ran
    /// out, returning the cursor position reached. `self.resume` is left `None`
    /// if this chunk finished it and set to the new progress if it did not.
    ///
    /// Only ever called with a non-`None` resume state, and never from inside
    /// the field loop: a single item is in progress at a time, so this cost is
    /// paid once per `feed`, not once per field.
    #[inline(never)]
    fn resume_item<V: Visitor>(&mut self, buf: &[u8], pos: usize, v: &mut V) -> Result<usize> {
        match self.resume {
            Resume::None => Ok(pos),
            Resume::Payload { .. } => Ok(self.deliver_payload(buf, pos, v)),
            Resume::ArrayInt {
                id,
                signed,
                remaining,
            } => {
                self.resume = Resume::None;
                if signed {
                    self.int_array::<true, V>(buf, pos, id, remaining, v)
                } else {
                    self.int_array::<false, V>(buf, pos, id, remaining, v)
                }
            }
            Resume::ArrayFix {
                id,
                fp64,
                remaining,
            } => {
                self.resume = Resume::None;
                Ok(if fp64 {
                    self.fix_array::<true, V>(buf, pos, id, remaining, v)
                } else {
                    self.fix_array::<false, V>(buf, pos, id, remaining, v)
                })
            }
        }
    }

    /// Read `rem` varint array elements from `buf` at `pos`, pushing each to
    /// `v`, and return the cursor position reached. If the buffer ran out
    /// mid-array, `self.resume` holds the remaining count and the returned
    /// position is the start of the unfinished element.
    ///
    /// The cursor is taken and returned **by value**: threading a `&mut usize`
    /// through a call the optimizer may not inline forces every varint to write
    /// the cursor back to memory.
    ///
    /// `SIGNED` is a const parameter so the ZigZag decision is compiled out of
    /// the element loop rather than re-tested per element.
    #[inline(always)]
    fn int_array<const SIGNED: bool, V: Visitor>(
        &mut self,
        buf: &[u8],
        pos: usize,
        id: Id,
        mut rem: usize,
        v: &mut V,
    ) -> Result<usize> {
        let mut p = pos;

        // Bulk run — while a full varint is guaranteed readable, no element can
        // be truncated, so the loop carries no bounds check beyond its own.
        while rem > 0 && p + MAX_VARINT_LEN <= buf.len() {
            // SAFETY: `MAX_VARINT_LEN` bytes are readable at `p`, per the guard.
            let val = unsafe { read_varint_ready(buf.as_ptr(), &mut p) }?;
            if SIGNED {
                v.signed(id, zigzag_decode(val));
            } else {
                v.unsigned(id, val);
            }
            rem -= 1;
        }

        // Tail — inside the last few bytes an element may straddle the chunk.
        while rem > 0 {
            let elem_start = p;
            match read_varint(buf, &mut p) {
                Ok(val) => {
                    if SIGNED {
                        v.signed(id, zigzag_decode(val));
                    } else {
                        v.unsigned(id, val);
                    }
                    rem -= 1;
                }
                Err(Error::Incomplete) => {
                    self.resume = Resume::ArrayInt {
                        id,
                        signed: SIGNED,
                        remaining: rem,
                    };
                    return Ok(elem_start);
                }
                Err(e) => return Err(e),
            }
        }
        Ok(p)
    }

    /// [`IStream::int_array`] for fixed-width float elements. Cannot fail: the
    /// element width is already validated, so running out of bytes is the only
    /// non-completing outcome.
    #[inline(always)]
    fn fix_array<const FP64: bool, V: Visitor>(
        &mut self,
        buf: &[u8],
        pos: usize,
        id: Id,
        mut rem: usize,
        v: &mut V,
    ) -> usize {
        let elem_len = if FP64 { 8 } else { 4 };
        let mut p = pos;
        while rem > 0 {
            if buf.len() - p < elem_len {
                self.resume = Resume::ArrayFix {
                    id,
                    fp64: FP64,
                    remaining: rem,
                };
                return p;
            }
            // SAFETY: `elem_len` bytes are readable at `p`, just checked.
            unsafe { emit_fixlen_value::<FP64, V>(buf, p, id, v) };
            p += elem_len;
            rem -= 1;
        }
        p
    }

    /// Deliver as much of an in-progress string/blob payload as `buf` holds,
    /// updating `self.resume`. Returns the new cursor position.
    ///
    /// Inlined into its callers: it is the whole of a `feed` that continues a
    /// streamed payload, and an outlined body would put a second call frame
    /// under one that is already only a dozen instructions of work.
    #[inline]
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

/// Decode 4 (`FP64 == false`) or 8 little-endian float bytes at `buf[pos..]` and
/// push them to the visitor.
///
/// # Safety
///
/// `buf` must hold 4 (resp. 8) readable bytes at `pos`.
#[inline]
unsafe fn emit_fixlen_value<const FP64: bool, V: Visitor>(
    buf: &[u8],
    pos: usize,
    id: Id,
    v: &mut V,
) {
    let p = buf.as_ptr().add(pos);
    if FP64 {
        debug_assert!(pos + 8 <= buf.len());
        v.fp64(id, f64::from_le_bytes(core::ptr::read_unaligned(p.cast())));
    } else {
        debug_assert!(pos + 4 <= buf.len());
        v.fp32(id, f32::from_le_bytes(core::ptr::read_unaligned(p.cast())));
    }
}

/// Decode a contiguous message in one shot — the fast zero-copy path.
///
/// `buf` is treated as the bytes available so far. Every field is pushed to
/// `visitor`; string/blob payloads are delivered as a single borrowed slice with
/// no copy. Surfaces the three outcomes of MESSAGE_SPEC §7 — there is no separate
/// finalize step:
///
/// * `Ok(())` — the buffer is a complete message ending at a field boundary.
/// * [`Err(Error::Incomplete)`](Error::Incomplete) — the buffer ends inside a
///   field or with an open sequence (truncated). Not malformed; more bytes would
///   complete it.
/// * [`Err(Error::InvalidMsg)`](Error::InvalidMsg) — the bytes are malformed.
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
    // `feed` itself surfaces all three outcomes for the bytes consumed, so a
    // single feed of the whole buffer is the complete verdict — no separate
    // finish step (which would only re-report the same state).
    IStream::new().feed(buf, visitor)
}

// Which §6.2 ceiling bounds which wire word is a choice the integration tests
// cannot see: `FIXLEN_MAX` and `ARRAY_MAX` are crate-internal, and this build
// sets both to `i32::MAX`, so a test written over literals passes either way.
// These unit tests state the bound in terms of the constant itself, so that the
// pairing survives a profile that takes §6.2's allowance to set the two ceilings
// differently (`FIXLEN_MAX` = 65,535 with an unchanged `ARRAY_MAX`, say).
#[cfg(test)]
mod ceiling_tests {
    use super::*;

    struct Sink;
    impl Visitor for Sink {}

    fn push_varint(out: &mut Vec<u8>, mut value: Unsigned) {
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

    /// A field header (id 0) for the given wire type, followed by `word`.
    fn field(wire: u8, word: Unsigned) -> Vec<u8> {
        let mut bytes = vec![wire];
        push_varint(&mut bytes, word);
        bytes
    }

    /// §4.6 bounds a **scalar fixlen** field's declared length, and §6.2 names
    /// that ceiling `FIXLEN_MAX` — not `ARRAY_MAX`, which bounds an element
    /// *count* (§4.7/§4.8) and is separately settable.
    #[test]
    fn scalar_fixlen_length_is_bounded_by_fixlen_max() {
        for subtype in [FX_STR, FX_BLOB] {
            // One past `FIXLEN_MAX`: INVALID, and rejected on the length word
            // itself — before a single payload byte is read or reserved. Only
            // the variable-width subtypes discriminate: a float length is
            // additionally pinned to exactly 4 / 8 bytes.
            let bytes = field(T_FIXLEN, ((FIXLEN_MAX + 1) << 3) | subtype as Unsigned);
            assert_eq!(
                IStream::new().feed(&bytes, &mut Sink),
                Err(Error::InvalidMsg),
                "fixlen subtype {subtype} above FIXLEN_MAX must be INVALID"
            );
        }

        // `FIXLEN_MAX` itself is a legal length: the message is merely short of
        // its payload, which is INCOMPLETE, not malformed — and reaching that
        // verdict allocates nothing on the declared length.
        for subtype in [FX_STR, FX_BLOB] {
            let mut bytes = field(T_FIXLEN, (FIXLEN_MAX << 3) | subtype as Unsigned);
            bytes.extend_from_slice(b"hi");
            assert_eq!(
                IStream::new().feed(&bytes, &mut Sink),
                Err(Error::Incomplete),
                "fixlen subtype {subtype} at FIXLEN_MAX must stay decodable"
            );
        }
    }

    /// `CARRY_CAP` is the *derived* bound of §6.6, so it is asserted against the
    /// widest prefix the parser can actually hand back, not against a literal.
    ///
    /// The widest is `ARRAY_FIXLEN`'s: `parse` backs up to `field_start` over the
    /// header **and** the element count **and** the `fixlen_word`
    /// (`istream.rs`'s `T_FIXLENARRAY` arm), so three varints have to be
    /// carryable — a shape §5.1.4's two-varint reservation does not describe.
    #[test]
    fn the_carry_holds_the_widest_prefix_the_parser_backs_up_over() {
        // The widest *reachable* prefix: a 5-byte header (id `ID_MAX`, the
        // largest a valid header can carry), a 5-byte count (`ARRAY_MAX`, the
        // largest a valid count can carry), and a `fixlen_word` cut one byte
        // before its terminator (9 bytes — with ten readable the value is
        // decided, valid or not).
        let mut bytes = Vec::new();
        push_varint(
            &mut bytes,
            ((ID_MAX as Unsigned) << 3) | T_FIXLENARRAY as Unsigned,
        );
        push_varint(&mut bytes, ARRAY_MAX);
        bytes.extend_from_slice(&[0x80; MAX_VARINT_LEN - 1]);

        let mut is = IStream::new();
        assert_eq!(is.feed(&bytes, &mut Sink), Err(Error::Incomplete));
        assert_eq!(
            is.carry_len,
            bytes.len(),
            "the whole field prefix must be carried"
        );
        assert!(
            bytes.len() <= CARRY_CAP,
            "CARRY_CAP ({CARRY_CAP}) is below the widest carryable prefix ({})",
            bytes.len()
        );
    }

    /// The same bound as a property: over a corpus covering every wire type,
    /// fed at every split point, the carry never exceeds its fixed capacity —
    /// and the byte-at-a-time verdict matches the one-shot one.
    #[test]
    fn no_split_of_any_message_overruns_the_carry() {
        let mut messages: Vec<Vec<u8>> = Vec::new();
        for wire in [
            T_VARINT_UNSIGNED,
            T_VARINT_SIGNED,
            T_FIXLEN,
            T_VARINTARRAY_UNSIGNED,
            T_VARINTARRAY_SIGNED,
            T_FIXLENARRAY,
            T_SEQUENCE_START,
            T_SEQUENCE_END,
        ] {
            for id in [0u64, 1, 300, 70_000, ID_MAX as Unsigned] {
                for word in [0u64, 1, 4, 8, 300, 70_000, 1 << 40] {
                    let mut m = Vec::new();
                    push_varint(&mut m, (id << 3) | wire as Unsigned);
                    push_varint(&mut m, word);
                    m.extend_from_slice(&[0x5A; 16]);
                    messages.push(m);
                }
            }
        }

        for m in &messages {
            let one_shot = IStream::new().feed(m, &mut Sink);
            let mut is = IStream::new();
            let mut last = Ok(());
            for i in 0..m.len() {
                last = is.feed(&m[i..i + 1], &mut Sink);
                assert!(
                    is.carry_len <= CARRY_CAP,
                    "carry grew to {} at byte {i}",
                    is.carry_len
                );
            }
            // INCOMPLETE is the one verdict that legitimately differs: fed whole,
            // a message whose trailing bytes are garbage may reject earlier.
            if one_shot != Err(Error::Incomplete) || last != Err(Error::Incomplete) {
                assert_eq!(last, one_shot, "chunked verdict differs for {m:02X?}");
            }
        }
    }

    /// The array wire types bound their **element count** against `ARRAY_MAX`,
    /// the twin of the assertion above.
    #[test]
    fn array_element_count_is_bounded_by_array_max() {
        for wire in [T_VARINTARRAY_UNSIGNED, T_VARINTARRAY_SIGNED, T_FIXLENARRAY] {
            let bytes = field(wire, ARRAY_MAX + 1);
            assert_eq!(
                IStream::new().feed(&bytes, &mut Sink),
                Err(Error::InvalidMsg),
                "wire type {wire} count above ARRAY_MAX must be INVALID"
            );
        }
    }
}
