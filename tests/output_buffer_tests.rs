//! The output-buffer contract of CORELIB_PLAN §5.1, and the tests §7.2 item 4
//! requires for it: the declared [`MIN_OUTPUT_BUFFER`], where it binds and where
//! it must not, and both halves of the flush handover — a sink that copies and
//! returns its buffer, and one that takes the buffer and installs a replacement.

use std::collections::VecDeque;

use sofab::{Error, FlushTake, OStream, MIN_OUTPUT_BUFFER};

/// The reference byte stream, written into a buffer that cannot fill. Mixes
/// atomic units (headers, counts, scalars, floats) with a divisible run — a
/// string far longer than any streaming buffer used below.
fn script<'a, F: FlushTake<'a>>(os: &mut OStream<'a, F>) {
    script_head(os);
    script_tail(os);
}

/// The first half of [`script`], up to and including the long divisible run.
fn script_head<'a, F: FlushTake<'a>>(os: &mut OStream<'a, F>) {
    os.write_unsigned(1, 42).unwrap();
    os.write_signed(2, -7).unwrap();
    os.write_str(3, "a string payload that no streaming buffer here can hold")
        .unwrap();
}

/// The second half of [`script`].
fn script_tail<'a, F: FlushTake<'a>>(os: &mut OStream<'a, F>) {
    os.write_blob(4, &[0xDE, 0xAD, 0xBE, 0xEF]).unwrap();
    os.write_fp64(5, core::f64::consts::PI).unwrap();
    os.write_array_unsigned(6, &[1u64, 300, 70_000]).unwrap();
    os.write_sequence_begin_lazy(7).unwrap();
    os.write_unsigned(8, 1).unwrap();
    os.write_sequence_end().unwrap();
}

fn one_shot() -> Vec<u8> {
    let mut buf = [0u8; 256];
    let used = {
        let mut os = OStream::new(&mut buf);
        script(&mut os);
        os.bytes_used()
    };
    buf[..used].to_vec()
}

/// §7.2 item 4: encode into a buffer of **exactly** `MIN_OUTPUT_BUFFER` bytes and
/// assert the concatenated output is byte-identical to the one-shot output.
///
/// This is the test that makes the constant real. At the declared value of 1 no
/// write lands contiguously: every header, count, scalar and float is split
/// across a flush, and so is the string payload — the divisible run §5.1 requires
/// an encoder to be able to split whatever else it reserves.
#[test]
fn encode_at_exactly_min_output_buffer_matches_one_shot() {
    let reference = one_shot();

    let mut collected: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; MIN_OUTPUT_BUFFER];
    {
        let mut os = OStream::with_flush(&mut buf, 0, |chunk: &[u8]| {
            collected.extend_from_slice(chunk);
        })
        .unwrap();
        script(&mut os);
        os.flush().unwrap();
    }

    assert_eq!(collected, reference);
}

/// §5.1 caps the declaration at 20 — a header varint and its value. A ceiling
/// above that would let a port demand more than a whole message can occupy.
/// Checked at compile time, since both sides are constants.
const _: () = assert!(MIN_OUTPUT_BUFFER <= 20 && MIN_OUTPUT_BUFFER >= 1);

/// §7.2 item 4: a buffer one byte short of the minimum, installed **with a sink**,
/// is rejected **where it is handed over** — not partway through a message.
///
/// At `MIN_OUTPUT_BUFFER == 1` that is the zero-capacity buffer, and it is the
/// case that used to reach `get_unchecked_mut` on an empty slice.
///
/// §5.1 lets a port refuse "by an exception, or an error status". This port
/// reports the **status**, from every installation path — `with_flush`,
/// `with_offset` and `buffer_set` alike — so that the spelling means the same
/// thing here and in `corelib-rs-no-std`, which cannot panic at all.
#[test]
fn a_sink_buffer_below_the_minimum_is_rejected_at_handover() {
    let short = MIN_OUTPUT_BUFFER - 1;

    // At construction.
    let mut buf = vec![0u8; short];
    let mut sunk: Vec<u8> = Vec::new();
    assert_eq!(
        OStream::with_flush(&mut buf, 0, |c: &[u8]| sunk.extend_from_slice(c))
            .err()
            .map(|e| e == Error::Argument),
        Some(true),
        "a {short}-byte capacity must not be accepted with a sink"
    );

    // The same shortfall produced by the start offset rather than the length.
    let mut buf = vec![0u8; 8];
    assert_eq!(
        OStream::with_flush(&mut buf, 8 - short, |c: &[u8]| sunk.extend_from_slice(c)).err(),
        Some(Error::Argument)
    );

    // An offset past the end has no capacity at all — same rejection, same place.
    let mut buf = vec![0u8; 4];
    assert_eq!(
        OStream::with_flush(&mut buf, 99, |c: &[u8]| sunk.extend_from_slice(c)).err(),
        Some(Error::Argument)
    );

    // And at a mid-stream buffer-set, which installs a buffer just as much. The
    // capacity is judged *before* anything is drained, so a rejected
    // installation leaves the stream exactly as it was: the pending bytes are
    // still in the active buffer and reach the sink at the next flush.
    let mut good = [0u8; 16];
    let mut bad = vec![0u8; short];
    {
        let mut os =
            OStream::with_flush(&mut good, 0, |c: &[u8]| sunk.extend_from_slice(c)).unwrap();
        os.write_unsigned(1, 42).unwrap();
        assert_eq!(os.buffer_set(&mut bad, 0), Err(Error::Argument));
        assert_eq!(os.bytes_used(), 2, "a refused install must drain nothing");
        os.flush().unwrap();
    }
    assert_eq!(sunk, [0x08, 0x2A]);
}

/// §5.1's MUST NOT list: an encoder must not "return partial output as if it
/// were complete". Replacing the active buffer on a stream that **has** a sink
/// used to drop whatever was already written into it — every call in the
/// sequence answered `Ok` and the message came out truncated.
///
/// There is somewhere to drain to here, so the bytes go to the sink first. (On a
/// sinkless stream there is not: they stay in the buffer the caller still owns,
/// which is the documented recovery from `BufferFull` — see
/// `the_pending_bytes_of_a_sinkless_stream_stay_in_the_callers_buffer`.)
#[test]
fn a_buffer_set_with_a_sink_drains_the_pending_bytes_first() {
    let mut a = [0u8; 32];
    let mut b = [0u8; 32];
    let mut out: Vec<u8> = Vec::new();
    {
        let mut os = OStream::with_flush(&mut a, 0, |c: &[u8]| out.extend_from_slice(c)).unwrap();
        os.write_unsigned(1, 42).unwrap(); // 08 2A, unflushed in `a`
        os.buffer_set(&mut b, 0).unwrap();
        os.write_unsigned(2, 7).unwrap();
        assert_eq!(os.flush(), Ok(2));
    }
    assert_eq!(
        out,
        [0x08, 0x2A, 0x10, 0x07],
        "the one-shot bytes, in order"
    );
}

/// The same rule over a whole message, against the reference §5.1 requires the
/// streaming path to reproduce: a `buffer_set` dropped into the middle of the
/// script — including one that re-arms header room, offset and all — must leave
/// the output byte-identical to the one-shot encode.
#[test]
fn a_mid_message_buffer_set_with_a_sink_matches_one_shot() {
    let reference = one_shot();

    // Where the switch falls in the byte stream: everything `script_head` writes
    // has reached the sink by then, drained by the buffer-set itself.
    let head_len = {
        let mut buf = [0u8; 128];
        let mut os = OStream::new(&mut buf);
        script_head(&mut os);
        os.bytes_used()
    };

    for offset in [0usize, 4] {
        let mut a = [0u8; 6];
        let mut b = [0u8; 96]; // holds the whole tail, so `b` is flushed once
        let mut collected: Vec<u8> = Vec::new();
        {
            let mut os = OStream::with_flush(&mut a, 0, |chunk: &[u8]| {
                collected.extend_from_slice(chunk);
            })
            .unwrap();
            script_head(&mut os);
            os.buffer_set(&mut b, offset).unwrap();
            script_tail(&mut os);
            os.flush().unwrap();
        }
        // The reserved head of `b` is the caller's framing room; it travels to
        // the sink with the unit it belongs to, and the offset is consumed by
        // that one installation.
        let mut message = collected;
        message.drain(head_len..head_len + offset);
        assert_eq!(message, reference, "offset {offset}");
    }
}

/// A **taking** sink gets the pending bytes the same way, and the buffer the
/// caller installs wins over the replacement the sink returned from that
/// handover: the caller's `buffer_set` is the later word, and its offset is the
/// one that binds.
#[test]
fn a_buffer_set_hands_the_pending_bytes_to_a_taking_sink() {
    let mut first = [0u8; 16];
    let mut spare_a = [0u8; 16];
    let mut spare_b = [0u8; 16];
    let mut mine = [0u8; 16];
    let mut out: Vec<u8> = Vec::new();
    let mut swaps = 0usize;
    let mut last: Option<*const u8> = None;
    let used = {
        let mut pool = VecDeque::new();
        pool.push_back(&mut spare_a[..]);
        pool.push_back(&mut spare_b[..]);
        let sink = TakingSink {
            out: &mut out,
            pool,
            swaps: &mut swaps,
            last: &mut last,
        };
        let mut os = OStream::with_flush(&mut first, 0, sink).unwrap();
        os.write_unsigned(1, 42).unwrap();
        os.buffer_set(&mut mine, 1).unwrap();
        os.write_unsigned(2, 7).unwrap();
        os.bytes_used()
    };

    // No `flush()` here: the bytes below can only have reached the sink at the
    // buffer-set, and exactly once.
    assert_eq!(swaps, 1, "the buffer-set is one handover, not two");
    assert_eq!(out, [0x08, 0x2A]);
    assert_eq!(
        &mine[1..used],
        &[0x10, 0x07],
        "written into the caller's buffer, at the caller's offset"
    );
}

/// The converse half, unchanged: **without** a sink there is nowhere to drain
/// to, so `buffer_set` leaves the bytes in the buffer the caller owns and still
/// holds. That is the documented recovery from `Error::BufferFull` — read
/// `bytes_used()`, install the next buffer, concatenate.
#[test]
fn the_pending_bytes_of_a_sinkless_stream_stay_in_the_callers_buffer() {
    let mut a = [0u8; 2];
    let mut b = [0u8; 8];
    let (used_a, used_b) = {
        let mut os = OStream::new(&mut a);
        os.write_unsigned(1, 42).unwrap();
        assert_eq!(os.write_unsigned(2, 7), Err(Error::BufferFull));
        let used_a = os.bytes_used();
        os.buffer_set(&mut b, 0).unwrap();
        os.write_unsigned(2, 7).unwrap();
        (used_a, os.bytes_used())
    };
    let mut streamed = a[..used_a].to_vec();
    streamed.extend_from_slice(&b[..used_b]);
    assert_eq!(streamed, [0x08, 0x2A, 0x10, 0x07]);
}

/// The other refusal mechanism: `with_flush` reports the capacity precondition
/// as `Error::Argument`, at the handover and before a single byte of the message
/// exists. A status rather than a panic, so that this crate and
/// `corelib-rs-no-std` spell the installation identically — that crate cannot
/// panic at all (no `core::panicking`; on bare metal a panic is a hard fault).
#[test]
fn with_flush_refuses_a_buffer_below_the_minimum() {
    let mut buf = vec![0u8; MIN_OUTPUT_BUFFER - 1];
    let mut sunk: Vec<u8> = Vec::new();
    let r = OStream::with_flush(&mut buf, 0, |c: &[u8]| sunk.extend_from_slice(c));
    assert!(r.is_err(), "a buffer below the minimum must be refused");
    drop(r);
    assert!(
        sunk.is_empty(),
        "a refused installation must not reach the sink"
    );
}

/// And the same refusal when the shortfall comes from the start offset, which is
/// where an out-of-range offset lands too.
#[test]
fn with_flush_refuses_an_offset_past_the_end() {
    let mut buf = vec![0u8; 4];
    let mut sunk: Vec<u8> = Vec::new();
    let r = OStream::with_flush(&mut buf, 99, |c: &[u8]| sunk.extend_from_slice(c));
    assert!(r.is_err(), "an offset past the end must be refused");
    drop(r);
    assert!(
        sunk.is_empty(),
        "a refused installation must not reach the sink"
    );
}

/// The converse, and the half that keeps the minimum from leaking onto the
/// one-shot path: the *same* undersized buffer **without** a sink is accepted, and
/// a message that fits encodes into it. No flush can occur there, so no atomic
/// unit can be split and the constant has nothing to say (§5.1).
#[test]
fn the_same_undersized_buffer_without_a_sink_is_accepted() {
    // The buffer the sink case above rejected outright.
    let mut buf = vec![0u8; MIN_OUTPUT_BUFFER - 1];
    let os = OStream::new(&mut buf);
    assert_eq!(os.bytes_used(), 0, "the empty message fits and encodes");

    // A buffer sized exactly to the message stays exact, whatever the port
    // declares: two bytes for a two-byte message.
    let mut exact = [0u8; 2];
    let used = {
        let mut os = OStream::new(&mut exact);
        os.write_unsigned(1, 42).unwrap();
        os.bytes_used()
    };
    assert_eq!(&exact[..used], &[0x08, 0x2A]);

    // One byte less and it is buffer-full, not a precondition failure.
    let mut tight = [0u8; 1];
    let mut os = OStream::new(&mut tight);
    assert_eq!(os.write_unsigned(1, 42), Err(Error::BufferFull));
}

/// What the sinkless waiver does **not** cover: §5.1 waives `MIN_OUTPUT_BUFFER`
/// for a buffer installed without a sink, not the *range* of the start offset —
/// the offset is a position in the buffer, and one past its end names no
/// installation at all. §5.1 describes the minimum as being refused "by the same
/// mechanism the port uses for an out-of-range offset", so both installation
/// paths must have that mechanism and must agree.
///
/// The boundary case `offset == len` is a real installation: capacity zero, no
/// minimum binds it, and the first write reports `BufferFull`.
#[test]
fn a_start_offset_past_the_end_is_refused_without_a_sink() {
    // Construction, error-status form.
    let mut buf = [0u8; 4];
    assert_eq!(
        OStream::with_offset(&mut buf, 5).err(),
        Some(Error::Argument)
    );

    // The zero-capacity boundary is accepted: a buffer that holds nothing, not an
    // offset outside the buffer.
    let mut buf = [0u8; 4];
    let mut os = OStream::with_offset(&mut buf, 4).expect("offset == len is in range");
    assert_eq!(os.bytes_used(), 4);
    assert_eq!(os.write_unsigned(1, 42), Err(Error::BufferFull));

    // A mid-stream buffer-set on a sinkless stream, which is where the two paths
    // used to disagree: the sink path folds the range into its capacity check and
    // this one had no check at all, so the offset was accepted and surfaced only
    // as a late `BufferFull` — or as a caller reading `bytes_used()` past the end
    // of its own buffer.
    let mut a = [0u8; 8];
    let mut b = [0u8; 4];
    let mut os = OStream::new(&mut a);
    os.write_unsigned(1, 42).unwrap();
    assert_eq!(os.buffer_set(&mut b, 5), Err(Error::Argument));

    // A refused installation leaves the stream exactly as it was: still on the
    // first buffer, still holding its two bytes, still writable.
    assert_eq!(os.bytes_used(), 2);
    os.write_signed(2, -7).unwrap();
    let used = os.bytes_used();
    drop(os);
    assert_eq!(&a[..used], &[0x08, 0x2A, 0x11, 0x0D]);

    // And the boundary again, this time installed mid-stream.
    let mut a = [0u8; 8];
    let mut b = [0u8; 4];
    let mut os = OStream::new(&mut a);
    assert_eq!(os.buffer_set(&mut b, 4), Ok(()));
    assert_eq!(os.bytes_used(), 4);
    assert_eq!(os.write_unsigned(1, 42), Err(Error::BufferFull));
}

/// The same refusal on the sink-less installation: an offset outside the buffer
/// names no installation, so `with_offset` reports `Error::Argument`. `new` stays
/// infallible — offset 0 is in range for every buffer, including an empty one.
#[test]
fn with_offset_refuses_an_offset_past_the_end() {
    let mut buf = [0u8; 4];
    assert!(
        OStream::with_offset(&mut buf, 99).is_err(),
        "an offset past the end must be refused"
    );
    // offset == len is in range: a capacity of zero, and bytes_used counts from
    // the buffer's start, so the reserved room is already accounted for.
    assert_eq!(
        OStream::with_offset(&mut buf, 4).map(|os| os.bytes_used()),
        Ok(4)
    );
}

/// A sink that **takes** every buffer it is handed: it copies the bytes out,
/// scrubs the storage, retains it, and installs a different buffer before
/// returning (§5.1 take-and-replace).
///
/// The scrub is the point: an encoder that kept writing into the buffer it gave
/// away would have its output read back as the fill pattern.
struct TakingSink<'a, 'o> {
    out: &'o mut Vec<u8>,
    /// Buffers this sink owns. The one it is handed goes to the back; the
    /// replacement comes off the front, so it is never the same storage twice in
    /// a row.
    pool: VecDeque<&'a mut [u8]>,
    swaps: &'o mut usize,
    /// Address of the buffer handed over last, to prove the swap is real.
    last: &'o mut Option<*const u8>,
}

impl<'a, 'o> FlushTake<'a> for TakingSink<'a, 'o> {
    fn flush_take(&mut self, buffer: &'a mut [u8], used: usize) -> (&'a mut [u8], usize) {
        self.out.extend_from_slice(&buffer[..used]);
        let handed = buffer.as_ptr();
        assert_ne!(
            Some(handed),
            *self.last,
            "the encoder handed back the buffer we took"
        );
        *self.last = Some(handed);

        buffer.fill(0xA5); // scrub what we are about to keep
        self.pool.push_back(buffer);
        *self.swaps += 1;
        (self.pool.pop_front().expect("pool never empties"), 0)
    }
}

#[test]
fn a_taking_sink_that_swaps_buffers_every_flush_matches_one_shot() {
    let reference = one_shot();

    let mut first = [0u8; 4];
    let mut spare_a = [0u8; 4];
    let mut spare_b = [0u8; 4];
    let mut out: Vec<u8> = Vec::new();
    let mut swaps = 0usize;
    let mut last: Option<*const u8> = None;
    {
        let mut pool = VecDeque::new();
        pool.push_back(&mut spare_a[..]);
        pool.push_back(&mut spare_b[..]);
        let sink = TakingSink {
            out: &mut out,
            pool,
            swaps: &mut swaps,
            last: &mut last,
        };
        let mut os = OStream::with_flush(&mut first, 0, sink).unwrap();
        script(&mut os);
        os.flush().unwrap();
    }

    assert_eq!(out, reference);
    assert!(swaps > 1, "expected repeated handovers, got {swaps}");
}

/// The other half of the returning-callback contract: a sink that **copies** and
/// returns the buffer it was handed. The encoder resumes in that same buffer at
/// offset 0, and the bytes are the same ones (§5.1).
#[test]
fn a_copying_sink_that_returns_its_buffer_matches_one_shot() {
    let reference = one_shot();

    let mut buf = [0u8; 4];
    let mut collected: Vec<u8> = Vec::new();
    {
        let mut os = OStream::with_flush(&mut buf, 0, |chunk: &[u8]| {
            collected.extend_from_slice(chunk);
        })
        .unwrap();
        script(&mut os);
        os.flush().unwrap();
    }
    assert_eq!(collected, reference);
}

/// A sink that hands back a replacement whose capacity is below
/// `MIN_OUTPUT_BUFFER`, pre-filled so anything the encoder emits out of it is
/// visible. Every buffer it is handed is *taken*: the bytes are copied out and
/// the storage retained, so the replacement is the only thing the encoder could
/// write into next.
struct ShortReplacementSink<'a, 'o> {
    /// Every unit the sink was handed, in order.
    units: &'o mut Vec<Vec<u8>>,
    /// Retained buffers, so the encoder can never be handed one back.
    kept: Vec<&'a mut [u8]>,
    /// The undersized replacement, handed out at the first flush.
    replacement: Option<&'a mut [u8]>,
    /// Where the replacement starts — `replacement.len() - offset` is below the
    /// minimum.
    offset: usize,
}

impl<'a, 'o> FlushTake<'a> for ShortReplacementSink<'a, 'o> {
    fn flush_take(&mut self, buffer: &'a mut [u8], used: usize) -> (&'a mut [u8], usize) {
        self.units.push(buffer[..used].to_vec());
        self.kept.push(buffer);
        match self.replacement.take() {
            Some(short) => (short, self.offset),
            // The stream must never come back here: the replacement above was
            // refused, so the handover that returned it is the last one.
            None => panic!("the sink was called again after its replacement was refused"),
        }
    }
}

/// §5.1: a replacement below `MIN_OUTPUT_BUFFER` is refused **where it is handed
/// over**, and a corelib "MUST NOT return partial output as if it were complete".
///
/// The refusal used to be a bare return value: the rejected buffer was installed
/// anyway, so the next write routed straight back into a handover that handed the
/// sink `offset` bytes of a buffer no write had touched — the replacement's own
/// fill pattern, emitted into the stream — and every write after the rejection
/// answered `Ok(())` while its field went missing.
///
/// So the rejection is terminal: the stream is dead, no later call reports
/// success, and the sink is never handed the rejected buffer's contents.
#[test]
fn a_refused_replacement_kills_the_stream_and_never_reaches_the_sink() {
    const FILL: u8 = 0xAA;
    let short = MIN_OUTPUT_BUFFER - 1;

    let mut first = [0u8; 2];
    let mut replacement = vec![FILL; 4];
    let offset = replacement.len() - short; // capacity one byte short
    let mut units: Vec<Vec<u8>> = Vec::new();
    let (w2, w3, flushed) = {
        let sink = ShortReplacementSink {
            units: &mut units,
            kept: Vec::new(),
            replacement: Some(&mut replacement[..]),
            offset,
        };
        let mut os = OStream::with_flush(&mut first, 0, sink).unwrap();
        os.write_unsigned(1, 42).unwrap(); // fills the 2-byte buffer

        // The write that triggers the handover: the sink takes the buffer and
        // returns one the encoder must refuse.
        let w2 = os.write_unsigned(2, 7);
        // ...and every call after it.
        let w3 = os.write_unsigned(3, 9);
        let flushed = os.flush();
        (w2, w3, flushed)
    };

    assert_eq!(w2, Err(Error::Argument), "the refusal is reported");
    assert_eq!(w3, Err(Error::Argument), "and the stream stays refused");
    assert_eq!(flushed, Err(Error::Argument), "flush too — no silent tail");

    assert_eq!(
        units,
        vec![vec![0x08, 0x2A]],
        "only the bytes the encoder actually wrote reach the sink"
    );
    assert!(
        !units.iter().any(|u| u.contains(&FILL)),
        "a refused buffer's contents must never reach the sink: {units:?}"
    );
    assert_eq!(
        replacement,
        vec![FILL; 4],
        "the refused buffer must not be written into either"
    );
}

/// The same refusal reached through an explicit `flush()` rather than a full
/// buffer: the bytes handed over are the caller's, the replacement is refused,
/// and the stream is dead from there on.
#[test]
fn a_replacement_refused_at_an_explicit_flush_is_terminal_too() {
    const FILL: u8 = 0xAA;
    let short = MIN_OUTPUT_BUFFER - 1;

    let mut first = [0u8; 16];
    let mut replacement = vec![FILL; 4];
    let offset = replacement.len() - short;
    let mut units: Vec<Vec<u8>> = Vec::new();
    let (first_flush, after, second_flush) = {
        let sink = ShortReplacementSink {
            units: &mut units,
            kept: Vec::new(),
            replacement: Some(&mut replacement[..]),
            offset,
        };
        let mut os = OStream::with_flush(&mut first, 0, sink).unwrap();
        os.write_unsigned(1, 42).unwrap();
        let first_flush = os.flush();
        let after = os.write_unsigned(2, 7);
        let second_flush = os.flush();
        (first_flush, after, second_flush)
    };

    assert_eq!(first_flush, Err(Error::Argument), "the bytes went out, the");
    assert_eq!(after, Err(Error::Argument));
    assert_eq!(second_flush, Err(Error::Argument));
    assert_eq!(units, vec![vec![0x08, 0x2A]]);
    assert_eq!(replacement, vec![FILL; 4]);
}

/// A dead stream stays dead: `buffer_set` cannot revive it either. The write that
/// hit the refusal never landed, so resuming into a fresh buffer would splice a
/// message with a hole in it — "partial output as if it were complete". Start a
/// new `OStream` instead.
#[test]
fn a_buffer_set_cannot_revive_a_refused_stream() {
    let short = MIN_OUTPUT_BUFFER - 1;

    let mut first = [0u8; 2];
    let mut replacement = [0xAAu8; 4];
    let offset = replacement.len() - short;
    let mut fresh = [0u8; 32];
    let mut units: Vec<Vec<u8>> = Vec::new();
    {
        let sink = ShortReplacementSink {
            units: &mut units,
            kept: Vec::new(),
            replacement: Some(&mut replacement[..]),
            offset,
        };
        let mut os = OStream::with_flush(&mut first, 0, sink).unwrap();
        os.write_unsigned(1, 42).unwrap();
        assert_eq!(os.write_unsigned(2, 7), Err(Error::Argument));
        assert_eq!(os.buffer_set(&mut fresh, 0), Err(Error::Argument));
        assert_eq!(os.write_unsigned(3, 9), Err(Error::Argument));
    }
    assert_eq!(units, vec![vec![0x08, 0x2A]]);
}

/// The one place a refused replacement is *not* terminal, and the reason it is
/// not: `buffer_set` supersedes it. The drain it performs hands the sink every
/// byte written so far, and the buffer the caller installs — judged before
/// anything is drained — replaces whatever the sink left behind, so the stream
/// never holds the refused buffer. Nothing is lost and nothing is written into a
/// refused buffer, so the stream continues (commit 308295b's contract).
#[test]
fn a_buffer_set_supersedes_a_replacement_the_sink_returned() {
    let short = MIN_OUTPUT_BUFFER - 1;

    let mut first = [0u8; 16];
    let mut replacement = vec![0xAAu8; 4];
    let offset = replacement.len() - short;
    let mut mine = [0u8; 16];
    let mut units: Vec<Vec<u8>> = Vec::new();
    let used = {
        let sink = ShortReplacementSink {
            units: &mut units,
            kept: Vec::new(),
            replacement: Some(&mut replacement[..]),
            offset,
        };
        let mut os = OStream::with_flush(&mut first, 0, sink).unwrap();
        os.write_unsigned(1, 42).unwrap();
        os.buffer_set(&mut mine, 0).unwrap(); // drains, then supersedes
        os.write_unsigned(2, 7).unwrap();
        os.bytes_used()
    };

    assert_eq!(units, vec![vec![0x08, 0x2A]], "drained at the buffer-set");
    assert_eq!(&mine[..used], &[0x10, 0x07], "and the stream carries on");
    assert_eq!(replacement, vec![0xAAu8; 4]);
}

/// §7.2 item 4: no foreign memory reaches a sink that was not granted
/// pass-through. This port implements no pass-through at all, so it holds by
/// construction — but a blob several times the buffer size is where it would
/// break first, and this pins it.
#[test]
fn no_foreign_memory_reaches_a_sink() {
    let blob: Vec<u8> = (0..64u8).collect();

    let mut buf = [0u8; 8];
    let span = {
        let p = buf.as_ptr() as usize;
        p..p + buf.len()
    };

    let mut seen = 0usize;
    {
        let mut os = OStream::with_flush(&mut buf, 0, |chunk: &[u8]| {
            let p = chunk.as_ptr() as usize;
            assert!(
                span.contains(&p),
                "sink received memory outside the installed buffer"
            );
            seen += 1;
        })
        .unwrap();
        os.write_blob(1, &blob).unwrap();
        os.flush().unwrap();
    }
    assert!(seen > 1, "the blob should have spanned several flushes");
}

/// `NoFlush` is public, and it is a `Flush` — so it can be *attached* rather than
/// only inferred as the sinkless default, and then it is a sink that throws the
/// bytes away.
///
/// Worth pinning because the two spellings over the same buffer behave in
/// opposite ways where it matters: without a sink, a message larger than the
/// buffer stops with `BufferFull` and nothing is lost; with `NoFlush` attached,
/// every write keeps reporting `Ok` while the buffer is recycled underneath and
/// the flushed bytes go nowhere. That is the sink doing exactly what it says, not
/// a §5.1 violation — the encoder handed its bytes to a sink, and where the sink
/// puts them is the caller's business — but it is the one sink for which "every
/// call returned Ok" does not mean the message survived, so it must not be
/// reachable by accident.
#[test]
fn an_attached_no_flush_sink_discards_what_it_is_handed() {
    use sofab::NoFlush;

    let blob: Vec<u8> = (0..200u8).collect();

    // Sinkless: the same buffer refuses the oversized message outright.
    let mut sinkless = [0u8; 16];
    {
        let mut os = OStream::new(&mut sinkless);
        assert_eq!(os.write_blob(1, &blob), Err(Error::BufferFull));
    }

    // With `NoFlush` attached: the writes succeed, the buffer is reused, and only
    // the tail that never reached a flush is still in it.
    let mut buf = [0u8; 16];
    let (used, flushed) = {
        let mut os = OStream::with_flush(&mut buf, 0, NoFlush).unwrap();
        os.write_blob(1, &blob).unwrap();
        let used = os.bytes_used();
        (used, os.flush().unwrap())
    };
    assert_eq!(flushed, used, "flush reports the bytes it dropped");
    assert!(
        used < blob.len(),
        "the message is longer than the buffer, so most of it was discarded"
    );
}
