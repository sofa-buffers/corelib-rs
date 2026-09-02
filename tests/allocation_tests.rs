//! CORELIB_PLAN §6.6.4 — the **measure** half of "checked both ways".
//!
//! §6.6 forbids the codec to allocate payload storage and requires its bounded
//! working state to be sized at construction; §6.6.4 says outright that reading
//! the source is *not sufficient* to establish it, because an allocation made
//! through a caller-supplied container leaves no `malloc` in the source to find.
//! What it requires alongside is a number: "an allocation count, or the heap
//! high-water mark, over a complete encode and a complete decode, measured after
//! the codec's one-time construction, which **MUST** be zero".
//!
//! Rust does not box the values this codec computes — every scalar it handles is
//! a machine type — so the count here is the strict form: **zero**, with no
//! language-forced handles to itemise (§6.6.2).
//!
//! The measurement is a counting `#[global_allocator]` armed per thread, so the
//! test harness's own allocations (and any other test running in parallel in
//! another binary) stay out of the count.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use sofab::{decode, FlushTake, IStream, Id, OStream, Status, Unsigned, Visitor, MAX_DEPTH};

// ---------------------------------------------------------------------------
// the counting allocator
// ---------------------------------------------------------------------------

thread_local! {
    /// Whether this thread is inside a measured region.
    static ARMED: Cell<bool> = const { Cell::new(false) };
    /// Allocation calls seen while armed.
    static CALLS: Cell<usize> = const { Cell::new(0) };
    /// Bytes requested while armed.
    static BYTES: Cell<usize> = const { Cell::new(0) };
}

/// `System`, plus a per-thread tally of the calls made while armed.
///
/// Every entry point that can hand back *new* storage is counted:
/// `alloc`, `alloc_zeroed` and a `realloc` that grows — which is how a `Vec`
/// that outgrows its capacity allocates, and the shape §6.6 calls "a growable
/// container of its own".
struct Counting;

fn note(size: usize) {
    if ARMED.with(Cell::get) {
        CALLS.with(|c| c.set(c.get() + 1));
        BYTES.with(|b| b.set(b.get() + size));
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        System.alloc(layout)
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        System.alloc_zeroed(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size > layout.size() {
            note(new_size - layout.size());
        }
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Run `body` with the allocator counting, and report `(calls, bytes)`.
///
/// The codec is constructed **inside** `body` only where the test says so;
/// §6.6 puts construction outside the prohibition ("the prohibition binds
/// everything after construction"), and this port happens to make even that
/// free, which the tests below assert rather than assume.
fn measure<R>(body: impl FnOnce() -> R) -> (usize, usize, R) {
    CALLS.with(|c| c.set(0));
    BYTES.with(|b| b.set(0));
    ARMED.with(|a| a.set(true));
    let out = body();
    ARMED.with(|a| a.set(false));
    (CALLS.with(Cell::get), BYTES.with(Cell::get), out)
}

/// The measurement harness must be able to see an allocation at all — otherwise
/// every assertion below passes vacuously.
#[test]
fn the_counter_sees_a_growable_container() {
    let (calls, bytes, len) = measure(|| {
        let mut v: Vec<u8> = Vec::new();
        v.extend_from_slice(&[0u8; 4096]);
        v.len()
    });
    assert_eq!(len, 4096);
    assert!(calls > 0, "the counting allocator saw nothing");
    assert!(bytes >= 4096, "counted {bytes} bytes for a 4096-byte push");
}

// ---------------------------------------------------------------------------
// fixtures that do not allocate themselves
// ---------------------------------------------------------------------------

/// Flush sink with a fixed destination: copies out, allocates nothing, so every
/// allocation the measurement sees belongs to the encoder.
struct FixedSink {
    out: [u8; 8192],
    len: usize,
}

impl FixedSink {
    fn new() -> Self {
        FixedSink {
            out: [0u8; 8192],
            len: 0,
        }
    }
}

impl<'a> FlushTake<'a> for &mut FixedSink {
    fn flush_take(&mut self, buffer: &'a mut [u8], used: usize) -> (&'a mut [u8], usize) {
        self.out[self.len..self.len + used].copy_from_slice(&buffer[..used]);
        self.len += used;
        (buffer, 0)
    }
}

/// Visitor that folds everything it is handed into one accumulator. No storage
/// of its own, so it cannot contribute an allocation.
#[derive(Default)]
struct Fold {
    acc: u64,
    strings: usize,
    seqs: usize,
}

impl Visitor for Fold {
    fn unsigned(&mut self, id: Id, value: Unsigned) {
        self.acc = self.acc.wrapping_add(value ^ id as u64);
    }
    fn signed(&mut self, id: Id, value: i64) {
        self.acc = self.acc.wrapping_add(value as u64 ^ id as u64);
    }
    fn fp32(&mut self, _id: Id, value: f32) {
        self.acc = self.acc.wrapping_add(value.to_bits() as u64);
    }
    fn fp64(&mut self, _id: Id, value: f64) {
        self.acc = self.acc.wrapping_add(value.to_bits());
    }
    fn string(&mut self, _id: Id, _total: usize, _offset: usize, chunk: &[u8]) {
        self.strings += chunk.len();
        for b in chunk {
            self.acc = self.acc.wrapping_add(*b as u64);
        }
    }
    fn blob(&mut self, _id: Id, _total: usize, _offset: usize, chunk: &[u8]) {
        self.strings += chunk.len();
    }
    fn sequence_begin(&mut self, _id: Id) {
        self.seqs += 1;
    }
}

/// A message exercising every wire type, nested one level, written into `buf`.
fn write_message(buf: &mut [u8]) -> usize {
    let mut os = OStream::new(buf);
    os.write_unsigned(1, 42).unwrap();
    os.write_signed(2, -7).unwrap();
    os.write_fp32(3, 1.5).unwrap();
    os.write_fp64(4, -2.25).unwrap();
    os.write_str(5, "a string long enough to straddle several chunks")
        .unwrap();
    os.write_blob(6, &[0xAB; 40]).unwrap();
    os.write_array_unsigned(7, &[1u64, 2, 3, 1 << 40]).unwrap();
    os.write_array_signed(8, &[-1i64, 2, -3]).unwrap();
    os.write_array_fp32(9, &[1.0f32, 2.0, 3.0]).unwrap();
    os.write_array_fp64(10, &[1.0f64, 2.0]).unwrap();
    os.write_sequence_begin_lazy(11).unwrap();
    os.write_unsigned(1, 7).unwrap();
    os.write_sequence_end().unwrap();
    os.bytes_used()
}

// ---------------------------------------------------------------------------
// encode (§6.6.4: "over a complete encode … MUST be zero")
// ---------------------------------------------------------------------------

/// A complete encode of every wire type, into a caller buffer, allocates
/// nothing — construction included.
#[test]
fn a_complete_encode_allocates_nothing() {
    let mut buf = [0u8; 512];
    let (calls, bytes, used) = measure(|| write_message(&mut buf));
    assert!(used > 0);
    assert_eq!((calls, bytes), (0, 0), "encode allocated");
}

/// The same encode driven through a **sink** with a buffer far smaller than the
/// message: every flush, every buffer handover, still zero.
#[test]
fn a_streamed_encode_allocates_nothing() {
    let mut sink = FixedSink::new();
    let mut buf = [0u8; 8];
    let (calls, bytes, used) = measure(|| {
        let mut os = OStream::with_flush(&mut buf, 0, &mut sink).unwrap();
        os.write_unsigned(1, 42).unwrap();
        os.write_str(5, "a string several times the size of the output buffer")
            .unwrap();
        os.write_array_unsigned(7, &[1u64 << 40; 32]).unwrap();
        os.flush().unwrap()
    });
    assert!(used <= 8);
    assert_eq!((calls, bytes), (0, 0), "streamed encode allocated");
}

/// **The `A2-0101` regression.** The run of held-back sequence headers is
/// fixed-size state sized at construction (§6.0.1, §6.6.2): nesting to the full
/// `MAX_DEPTH` must not allocate, and neither must the commit that writes the
/// whole run out.
///
/// Before the fix the run lived inline for eight levels and spilled into a `Vec`
/// beyond: depth 9 cost one allocation, depth 255 cost seven.
#[test]
fn encoding_at_max_depth_allocates_nothing() {
    let mut sink = FixedSink::new();
    let mut buf = [0u8; 16];
    let (calls, bytes, ()) = measure(|| {
        let mut os = OStream::with_flush(&mut buf, 0, &mut sink).unwrap();
        for id in 0..MAX_DEPTH {
            os.write_sequence_begin_lazy(id + 1).unwrap();
        }
        // Content, which commits the entire 255-header run at once.
        os.write_unsigned(1, 42).unwrap();
        for _ in 0..MAX_DEPTH {
            os.write_sequence_end().unwrap();
        }
        os.flush().unwrap();
    });
    assert_eq!(
        (calls, bytes),
        (0, 0),
        "holding {MAX_DEPTH} sequence headers back allocated"
    );
    // 255 one/two-byte headers + a two-byte leaf + 255 end markers.
    assert!(sink.len > 2 * MAX_DEPTH as usize);
}

/// Nesting one level past the point the old implementation spilled — the
/// smallest input that used to allocate at all.
#[test]
fn encoding_nine_levels_deep_allocates_nothing() {
    let mut buf = [0u8; 128];
    let (calls, bytes, used) = measure(|| {
        let mut os = OStream::new(&mut buf);
        for id in 1..=9u32 {
            os.write_sequence_begin_lazy(id).unwrap();
        }
        os.write_unsigned(1, 42).unwrap();
        for _ in 0..9 {
            os.write_sequence_end().unwrap();
        }
        os.bytes_used()
    });
    assert_eq!(used, 9 + 2 + 9);
    assert_eq!(
        (calls, bytes),
        (0, 0),
        "nesting past eight levels allocated"
    );
}

// ---------------------------------------------------------------------------
// decode (§6.6.4: "over a complete decode … MUST be zero")
// ---------------------------------------------------------------------------

/// A complete one-shot decode allocates nothing.
#[test]
fn a_complete_one_shot_decode_allocates_nothing() {
    let mut buf = [0u8; 512];
    let used = write_message(&mut buf);
    let mut fold = Fold::default();
    let (calls, bytes, r) = measure(|| decode(&buf[..used], &mut fold));
    assert_eq!(r, Ok(Status::Complete));
    assert_eq!((calls, bytes), (0, 0), "one-shot decode allocated");
}

/// **The `A2-0100` regression, half one.** Feeding the same message one byte at
/// a time drives every suspend/resume path there is — a header split, a length
/// word split, a payload split, an array element split — and must still
/// allocate nothing.
///
/// Before the fix the carry was a `Vec` and this cost one allocation.
#[test]
fn a_decode_fed_one_byte_at_a_time_allocates_nothing() {
    let mut buf = [0u8; 512];
    let used = write_message(&mut buf);
    let mut fold = Fold::default();
    let (calls, bytes, last) = measure(|| {
        let mut is = IStream::new();
        let mut last = Ok(Status::Complete);
        for i in 0..used {
            last = is.feed(&buf[i..i + 1], &mut fold);
        }
        last
    });
    assert_eq!(
        last,
        Ok(Status::Complete),
        "the byte-at-a-time feed did not complete"
    );
    assert_eq!((calls, bytes), (0, 0), "chunked decode allocated");
}

/// **The `A2-0100` regression, half two — the one that mattered.** A single
/// missing header byte followed by a one-mebibyte chunk used to copy the whole
/// chunk into the carry `Vec`: one allocation of 1,048,577 bytes, retained for
/// the decoder's lifetime and across `reset()`. The caller's chunk size chose
/// the decoder's memory, which is exactly what §6.6 forbids ("can a sender make
/// this allocation bigger by sending different bytes?").
#[test]
fn a_huge_chunk_after_a_split_header_allocates_nothing() {
    // 1 MiB of `00` bytes: a header varint of 0 (id 0, unsigned) followed by a
    // value varint of 0, over and over — a valid message half a million fields
    // long. The leading `0x80` makes the very first header straddle the split.
    let big = vec![0u8; 1 << 20];
    let mut fold = Fold::default();
    let (calls, bytes, r) = measure(|| {
        let mut is = IStream::new();
        assert_eq!(is.feed(&[0x80], &mut fold), Ok(Status::Incomplete));
        is.feed(&big, &mut fold)
    });
    assert_eq!(r, Ok(Status::Complete));
    assert_eq!(
        (calls, bytes),
        (0, 0),
        "a 1 MiB chunk stitched onto a one-byte carry allocated"
    );
}

/// The decoder's memory does not grow with the message: the same field shape at
/// a hundred times the size costs the same — nothing — and the size a hostile
/// length or count word *declares* buys nothing either (§6.6.4's "unchanged by
/// a hostile count or length").
#[test]
fn nothing_a_sender_writes_changes_what_the_decoder_takes() {
    let mut small = [0u8; 64];
    let mut large = vec![0u8; 8192];
    let small_used = {
        let mut os = OStream::new(&mut small);
        os.write_blob(1, &[0x5A; 16]).unwrap();
        os.bytes_used()
    };
    let large_used = {
        let mut os = OStream::new(&mut large);
        os.write_blob(1, &[0x5A; 4096]).unwrap();
        os.bytes_used()
    };

    let mut fold = Fold::default();
    let (small_calls, small_bytes, _) = measure(|| decode(&small[..small_used], &mut fold));
    let (large_calls, large_bytes, _) = measure(|| decode(&large[..large_used], &mut fold));
    assert_eq!((small_calls, small_bytes), (0, 0));
    assert_eq!(
        (large_calls, large_bytes),
        (small_calls, small_bytes),
        "a 4 KiB payload cost more than a 16-byte one"
    );

    // A `fixlen_word` announcing `i32::MAX` bytes, with none of them present:
    // INCOMPLETE, and nothing reserved on the strength of the declared length.
    let hostile: [u8; 6] = [0x0A, 0xFA, 0xFF, 0xFF, 0xFF, 0x0F];
    let (calls, bytes, r) = measure(|| {
        let mut is = IStream::new();
        is.feed(&hostile, &mut fold)
    });
    assert_eq!(r, Ok(Status::Incomplete));
    assert_eq!((calls, bytes), (0, 0), "a declared length reserved memory");
}

/// A reused decoder allocates nothing on any later message either — `reset` has
/// no capacity to keep and none to re-take.
#[test]
fn a_reused_decoder_allocates_nothing_on_any_message() {
    let mut buf = [0u8; 512];
    let used = write_message(&mut buf);
    let mut fold = Fold::default();
    let (calls, bytes, ()) = measure(|| {
        let mut is = IStream::new();
        for _ in 0..8 {
            for i in 0..used {
                is.feed(&buf[i..i + 1], &mut fold).ok();
            }
            is.reset();
        }
    });
    assert_eq!((calls, bytes), (0, 0), "a reused decoder allocated");
}
