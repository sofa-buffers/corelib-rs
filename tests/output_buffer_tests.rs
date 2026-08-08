//! The output-buffer contract of CORELIB_PLAN §5.1, and the tests §7.2 item 4
//! requires for it: the declared [`MIN_OUTPUT_BUFFER`], where it binds and where
//! it must not, and both halves of the flush handover — a sink that copies and
//! returns its buffer, and one that takes the buffer and installs a replacement.

use std::collections::VecDeque;

use sofab::{Error, Flush, OStream, MIN_OUTPUT_BUFFER};

/// The reference byte stream, written into a buffer that cannot fill. Mixes
/// atomic units (headers, counts, scalars, floats) with a divisible run — a
/// string far longer than any streaming buffer used below.
fn script<'a, F: Flush<'a>>(os: &mut OStream<'a, F>) {
    os.write_unsigned(1, 42).unwrap();
    os.write_signed(2, -7).unwrap();
    os.write_str(3, "a string payload that no streaming buffer here can hold")
        .unwrap();
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

    // And at a mid-stream buffer-set, which installs a buffer just as much.
    let mut good = [0u8; 16];
    let mut bad = vec![0u8; short];
    let mut os = OStream::with_flush(&mut good, 0, |c: &[u8]| sunk.extend_from_slice(c)).unwrap();
    os.write_unsigned(1, 42).unwrap();
    assert_eq!(os.buffer_set(&mut bad, 0), Err(Error::Argument));
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

impl<'a, 'o> Flush<'a> for TakingSink<'a, 'o> {
    fn flush(&mut self, buffer: &'a mut [u8], used: usize) -> (&'a mut [u8], usize) {
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
