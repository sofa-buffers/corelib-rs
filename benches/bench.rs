//! SofaBuffers Rust — throughput benchmark (MB/s, CPU time).
//!
//! Mirror of `bench/c/bench.c` and `bench/cpp/bench.cpp`: encode/decode
//! throughput for the four BENCH_SPEC datasets — a 1000-element u64 array, a
//! small "typical" mixed message, an unbounded 1 MB `blob`, and the `composite`
//! message that exercises the paths the flat three never reach (wrapper array,
//! multi-byte UTF-8, depth-3 nesting, an omitted default, a two-byte field
//! header). Each workload runs in a ~1 s loop and reports MB/s, and the output
//! table matches the C/C++ tools so the implementations can be compared directly.
//!
//! **Read the `blob 1MB` rows against each other, not against the others.** Five
//! bytes of that message are metadata and a million are payload, so its MB/s is
//! the platform's `memcpy` and the machine's memory bandwidth. The signal is the
//! *difference* between one-shot and streaming — the cost of the divisible-run
//! path (CORELIB_PLAN §5.1) — and under MB/s that difference is a low-single-digit
//! fraction of a bandwidth-bound row. Read it as Callgrind `Ir/op`
//! (`benches/run_callgrind.sh`), where instruction counts do not care about
//! bandwidth.
//!
//! Throughput is measured against *process CPU time* (`clock()`, not
//! wall-clock), so the number reflects the cost of the implementation rather
//! than OS scheduling noise or the wall-clock speed of the host. MB = 1e6 bytes.
//!
//! Run with:  `cargo bench --bench bench`

// The float workload value (3.14159) is a fixed payload byte pattern matching
// the C/C++ bench tools, deliberately not `std::f32::consts::PI`; silence the
// approx-constant lint so the cross-language byte comparison stays intact.
#![allow(clippy::approx_constant)]

use sofab::{IStream, Id, OStream, Signed, Unsigned, Visitor};
use std::fmt::Write as _;
use std::hint::black_box;

const N: usize = 1000;

/// `blob 1MB` payload length. Encoded size is `BLOB_LEN + 5` on every port — a
/// 1-byte header `(1 << 3) | 2` and a 4-byte `fixlen_word` `(1000000 << 3) | 3`.
const BLOB_LEN: usize = 1_000_000;

/// The streaming `blob 1MB` row is driven through a buffer of exactly this size
/// on every port, so the rows stay comparable across languages. `MIN_OUTPUT_BUFFER`
/// does not enter into it — it is at most 20, so 4096 always satisfies it.
const BLOB_CHUNK: usize = 4096;

/// One cycle of the `composite` string field: 1-, 2-, 3- and 4-byte UTF-8.
const COMPOSITE_TEXT: &str = "a\u{e4}\u{20ac}\u{1d11e}";

/// Process CPU time in seconds (not wall-clock), via
/// `clock_gettime(CLOCK_PROCESS_CPUTIME_ID)` — the higher-resolution equivalent
/// of the C tool's `clock()`.
fn cpu_now() -> f64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: ts is a valid, writable timespec; the clock id is valid on Linux.
    unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9
}

/// Decode sink that folds every value into a checksum so the optimizer cannot
/// elide the decode work.
#[derive(Default)]
struct Checksum {
    acc: u64,
}

impl Visitor for Checksum {
    fn unsigned(&mut self, id: Id, v: Unsigned) {
        self.acc = self.acc.wrapping_add(v ^ id as u64);
    }
    fn signed(&mut self, id: Id, v: Signed) {
        self.acc = self.acc.wrapping_add((v as u64) ^ id as u64);
    }
    fn fp32(&mut self, _id: Id, v: f32) {
        self.acc = self.acc.wrapping_add(v.to_bits() as u64);
    }
    fn fp64(&mut self, _id: Id, v: f64) {
        self.acc = self.acc.wrapping_add(v.to_bits());
    }
    fn string(&mut self, _id: Id, _total: usize, _offset: usize, chunk: &[u8]) {
        self.acc = self.acc.wrapping_add(chunk.len() as u64);
    }
    fn blob(&mut self, _id: Id, _total: usize, _offset: usize, chunk: &[u8]) {
        self.acc = self.acc.wrapping_add(chunk.len() as u64);
    }
}

/// A spread of unsigned values exercising 1..10-byte varints.
fn make_src() -> Vec<u64> {
    (0..N as u64)
        .map(|i| i.wrapping_mul(0x9E37_79B9_7F4A_7C15))
        .collect()
}

/// Payload of the `blob 1MB` dataset: exactly 1,000,000 bytes, so MB/s reads
/// directly against the `MB = 1e6` convention. Same constant as the u64 array, so
/// there is one magic number in this file rather than two.
fn make_blob() -> Vec<u8> {
    (0..BLOB_LEN as u64)
        .map(|i| i.wrapping_mul(0x9E37_79B9_7F4A_7C15) as u8)
        .collect()
}

/// The `composite` message (BENCH_SPEC): every path the flat datasets miss.
///
/// * id 1 — the suite's only **wrapper array** (MESSAGE_SPEC §5.1): one field
///   header per element, element id = array index, so ids 0–15 take a one-byte
///   header and 16–63 take two.
/// * id 2 — 320 UTF-8 bytes covering 1-, 2-, 3- and 4-byte sequences.
/// * id 3 — nesting at **depth 3**, so the lazy hold-back run grows past the
///   single level `typical` and `perf` reach.
/// * id 4 — equal to its declared default, so the encoder must **not** write it.
///   This is the hold-back's discard path: opened lazily, closed with the field
///   closer, gone from the wire.
/// * id 130 — the suite's only **two-byte field header**, `(130 << 3) | 0`.
fn encode_composite(os: &mut OStream) {
    // id 1: wrapper array of 64 strings, "item-0" ..= "item-63".
    os.write_sequence_begin_lazy(1).unwrap();
    let mut element = String::new();
    for i in 0..64u32 {
        element.clear();
        element.push_str("item-");
        write!(element, "{i}").unwrap();
        os.write_str(i, &element).unwrap();
    }
    os.write_sequence_end().unwrap();

    // id 2: 32 repetitions of a 10-byte, four-width UTF-8 cycle.
    os.write_str(2, &COMPOSITE_TEXT.repeat(32)).unwrap();

    // id 3: { 1: { 1: { 1: unsigned 7 } }, 2: signed -1 }
    os.write_sequence_begin_lazy(3).unwrap();
    os.write_sequence_begin_lazy(1).unwrap();
    os.write_sequence_begin_lazy(1).unwrap();
    os.write_unsigned(1, 7).unwrap();
    os.write_sequence_end().unwrap();
    os.write_sequence_end().unwrap();
    os.write_signed(2, -1).unwrap();
    os.write_sequence_end().unwrap();

    // id 4: all-default struct — opened and dropped, emitting nothing.
    os.write_sequence_begin_lazy(4).unwrap();
    os.write_sequence_end().unwrap();

    // id 130: the two-byte header.
    os.write_unsigned(130, 0xDEAD_BEEF).unwrap();
}

/// A representative small telemetry-style message: a few scalars, a float, a
/// short string and a small array — plus a nested sequence.
fn encode_typical(os: &mut OStream) {
    os.write_unsigned(1, 0xDEAD_BEEF).unwrap();
    os.write_signed(2, -12345).unwrap();
    os.write_boolean(3, true).unwrap();
    os.write_fp32(4, 3.14159).unwrap();
    os.write_str(5, "sofab").unwrap();
    os.write_array_unsigned(6, &[10u16, 20, 30, 40]).unwrap();
    os.write_sequence_begin_lazy(7).unwrap();
    os.write_unsigned(1, 99).unwrap();
    os.write_signed(2, -7).unwrap();
    os.write_sequence_end().unwrap();
}

// ---- Callgrind workload entry points --------------------------------------
// Each function performs *exactly one* operation and is `#[inline(never)]` +
// `#[unsafe(no_mangle)]`, so `bench/run_callgrind.sh` can run
//   valgrind --tool=callgrind --collect-atstart=no --toggle-collect=run_<w>
// and collect the instructions retired (Ir) for a single op — a deterministic,
// machine-independent per-op cost. `black_box` keeps the op from being elided
// or const-folded. Setup (encoding the decode inputs) happens in `main` before
// the call, so it stays outside the collected region.

#[inline(never)]
#[unsafe(no_mangle)]
pub fn run_encode_u64_array(src: &[u64], out: &mut [u8]) -> usize {
    let mut os = OStream::new(out);
    os.write_array_unsigned(1, black_box(src)).unwrap();
    black_box(os.bytes_used())
}

#[inline(never)]
#[unsafe(no_mangle)]
pub fn run_encode_typical(out: &mut [u8]) -> usize {
    let mut os = OStream::new(out);
    encode_typical(&mut os);
    black_box(os.bytes_used())
}

#[inline(never)]
#[unsafe(no_mangle)]
pub fn run_decode_u64_array(wire: &[u8]) -> u64 {
    let mut sink = Checksum::default();
    let mut is = IStream::new();
    is.feed(black_box(wire), &mut sink).unwrap();
    black_box(sink.acc)
}

#[inline(never)]
#[unsafe(no_mangle)]
pub fn run_decode_typical(wire: &[u8]) -> u64 {
    let mut sink = Checksum::default();
    let mut is = IStream::new();
    is.feed(black_box(wire), &mut sink).unwrap();
    black_box(sink.acc)
}

/// Sink for the streaming `blob 1MB` row. BENCH_SPEC is explicit that it
/// **consumes and discards**: accumulating would add to the streaming row a copy
/// the one-shot row never pays, and I/O is not deterministic under Callgrind.
/// Folding one byte per call is the minimum that keeps the call from being
/// optimised away.
#[derive(Default)]
struct Discard {
    acc: u8,
}

// A copying sink: it consumes the bytes and hands the buffer straight back
// through the blanket `FlushTake` impl, which is the shape the streaming row is
// meant to measure.
impl sofab::Flush for Discard {
    fn flush(&mut self, data: &[u8]) {
        self.acc ^= data.first().copied().unwrap_or(0);
    }
}

#[inline(never)]
#[unsafe(no_mangle)]
pub fn run_encode_blob_oneshot(blob: &[u8], out: &mut [u8]) -> usize {
    let mut os = OStream::new(out);
    os.write_blob(1, black_box(blob)).unwrap();
    black_box(os.bytes_used())
}

#[inline(never)]
#[unsafe(no_mangle)]
pub fn run_encode_blob_streaming(blob: &[u8], scratch: &mut [u8]) -> usize {
    let mut os = OStream::with_flush(scratch, 0, Discard::default());
    os.write_blob(1, black_box(blob)).unwrap();
    black_box(os.flush().unwrap())
}

#[inline(never)]
#[unsafe(no_mangle)]
pub fn run_decode_blob(wire: &[u8]) -> u64 {
    let mut sink = Checksum::default();
    let mut is = IStream::new();
    for chunk in black_box(wire).chunks(BLOB_CHUNK) {
        let _ = is.feed(chunk, &mut sink);
    }
    black_box(sink.acc)
}

#[inline(never)]
#[unsafe(no_mangle)]
pub fn run_encode_composite(out: &mut [u8]) -> usize {
    let mut os = OStream::new(out);
    encode_composite(&mut os);
    black_box(os.bytes_used())
}

#[inline(never)]
#[unsafe(no_mangle)]
pub fn run_decode_composite(wire: &[u8]) -> u64 {
    let mut sink = Checksum::default();
    let mut is = IStream::new();
    is.feed(black_box(wire), &mut sink).unwrap();
    black_box(sink.acc)
}

/// `decode: composite skip-all` — walk the message, materialize nothing.
///
/// In a push/visitor port that is a visitor which overrides no callback: the
/// decoder still walks every header, count and payload length, but nothing is
/// read into a destination. Its distance from `run_decode_composite` is what
/// not-decoding is worth here.
#[inline(never)]
#[unsafe(no_mangle)]
pub fn run_decode_composite_skip(wire: &[u8]) -> bool {
    struct SkipAll;
    impl Visitor for SkipAll {}

    let mut sink = SkipAll;
    let mut is = IStream::new();
    black_box(is.feed(black_box(wire), &mut sink).is_ok())
}

/// How long one batch of operations should take before the clock is read again.
/// `clock_gettime(CLOCK_PROCESS_CPUTIME_ID)` is a real syscall — never
/// vDSO-accelerated — costing on the order of a microsecond, so reading it once
/// per operation would time the clock rather than the codec: at ~22 ns/op for
/// the typical message, ~98 % of each iteration would be the measurement. Ten
/// milliseconds of work per read pushes that below 0.01 %.
const BATCH_SECS: f64 = 0.01;

/// Run `body` repeatedly until ~1 s of CPU time has elapsed (after one warm-up
/// call) and return throughput in MB/s for a message of `bytes` bytes.
///
/// The clock is read once per **batch**, never per operation, so the reported
/// time is the work and not the measurement (BENCH_SPEC "Timing": one warmup
/// before starting the timer, then a ~1 s CPU-time loop).
fn measure(bytes: usize, mut body: impl FnMut()) -> f64 {
    body(); // warmup

    // Grow the batch until it spans `BATCH_SECS`, so the clock read that ends
    // it is a rounding error against the work it timed.
    let mut batch: u64 = 1;
    loop {
        let t0 = cpu_now();
        for _ in 0..batch {
            body();
        }
        if cpu_now() - t0 >= BATCH_SECS {
            break;
        }
        batch = batch.saturating_mul(2);
    }

    let t0 = cpu_now();
    let mut it: u64 = 0;
    let mut el;
    loop {
        for _ in 0..batch {
            body();
        }
        it += batch;
        el = cpu_now() - t0;
        if el >= 1.0 {
            break;
        }
    }
    bytes as f64 * it as f64 / el / 1e6 // MB/s, MB = 1e6 bytes
}

fn main() {
    let src = make_src();

    // Pre-encode the messages (to learn their byte sizes and as decode input).
    let mut u64_buf = vec![0u8; N * 11 + 16];
    let enc_u64_used = {
        let mut os = OStream::new(&mut u64_buf);
        os.write_array_unsigned(1, &src).unwrap();
        os.bytes_used()
    };
    u64_buf.truncate(enc_u64_used);

    let mut typ_buf = vec![0u8; 256];
    let typ_used = {
        let mut os = OStream::new(&mut typ_buf);
        encode_typical(&mut os);
        os.bytes_used()
    };
    typ_buf.truncate(typ_used);

    let blob = make_blob();
    let mut blob_buf = vec![0u8; BLOB_LEN + 16];
    let blob_used = {
        let mut os = OStream::new(&mut blob_buf);
        os.write_blob(1, &blob).unwrap();
        os.bytes_used()
    };
    blob_buf.truncate(blob_used);
    assert_eq!(
        blob_used,
        BLOB_LEN + 5,
        "the blob 1MB encoded size is a cross-port parity check"
    );

    let mut comp_buf = vec![0u8; 4096];
    let comp_used = {
        let mut os = OStream::new(&mut comp_buf);
        encode_composite(&mut os);
        os.bytes_used()
    };
    comp_buf.truncate(comp_used);

    let ba = enc_u64_used;
    let bt = typ_used;
    let bb = blob_used;
    let bc = comp_used;

    // Callgrind mode: `bench <workload>` performs exactly one op of <workload>
    // and exits, so run_callgrind.sh can toggle collection around the run_*
    // symbol. `BYTES=<n>` on stderr feeds the table's size column. The decode
    // inputs (u64_buf/typ_buf) were encoded above — outside the collected op.
    // Cargo passes its own `--bench` through to the harness, so skip anything
    // that looks like a flag: only a bare workload name selects Callgrind mode,
    // and `cargo bench --bench bench` prints the table as documented.
    if let Some(w) = std::env::args().skip(1).find(|a| !a.starts_with('-')) {
        let mut enc_u64_out = vec![0u8; N * 11 + 16];
        let mut enc_typ_out = [0u8; 256];
        let mut enc_blob_out = vec![0u8; blob_used];
        let mut enc_blob_scratch = vec![0u8; BLOB_CHUNK];
        let mut enc_comp_out = vec![0u8; comp_used];
        let bytes = match w.as_str() {
            "encode_u64_array" => run_encode_u64_array(&src, &mut enc_u64_out),
            "encode_typical" => run_encode_typical(&mut enc_typ_out),
            "encode_blob_oneshot" => run_encode_blob_oneshot(&blob, &mut enc_blob_out),
            "encode_blob_streaming" => {
                run_encode_blob_streaming(&blob, &mut enc_blob_scratch);
                blob_used
            }
            "encode_composite" => run_encode_composite(&mut enc_comp_out),
            "decode_u64_array" => {
                run_decode_u64_array(&u64_buf);
                u64_buf.len()
            }
            "decode_typical" => {
                run_decode_typical(&typ_buf);
                typ_buf.len()
            }
            "decode_blob" => {
                run_decode_blob(&blob_buf);
                blob_buf.len()
            }
            "decode_composite" => {
                run_decode_composite(&comp_buf);
                comp_buf.len()
            }
            "decode_composite_skip" => {
                run_decode_composite_skip(&comp_buf);
                comp_buf.len()
            }
            other => {
                eprintln!("unknown workload: {other}");
                std::process::exit(2);
            }
        };
        eprintln!("BYTES={bytes}");
        return;
    }

    // Encode targets (reused across iterations; allocation is outside the loop).
    let mut enc_u64_out = vec![0u8; N * 11 + 16];
    let mut enc_typ_out = [0u8; 256];

    let enc_u64 = measure(ba, || {
        let mut os = OStream::new(&mut enc_u64_out);
        os.write_array_unsigned(1, black_box(&src)).unwrap();
        black_box(os.bytes_used());
    });
    let enc_typ = measure(bt, || {
        let mut os = OStream::new(&mut enc_typ_out);
        encode_typical(&mut os);
        black_box(os.bytes_used());
    });
    let dec_u64 = measure(ba, || {
        let mut sink = Checksum::default();
        let mut is = IStream::new();
        is.feed(black_box(&u64_buf), &mut sink).unwrap();
        black_box(sink.acc);
    });
    let dec_typ = measure(bt, || {
        let mut sink = Checksum::default();
        let mut is = IStream::new();
        is.feed(black_box(&typ_buf), &mut sink).unwrap();
        black_box(sink.acc);
    });

    // `blob 1MB`: the one-shot row is the floor — one contiguous write, no flush
    // logic — and the streaming row is the same bytes through ~245 flushes into a
    // 4096-byte buffer. Their difference is the divisible-run path.
    let mut enc_blob_out = vec![0u8; blob_used];
    let mut enc_blob_scratch = vec![0u8; BLOB_CHUNK];
    let enc_blob_1 = measure(bb, || {
        let mut os = OStream::new(&mut enc_blob_out);
        os.write_blob(1, black_box(&blob)).unwrap();
        black_box(os.bytes_used());
    });
    let enc_blob_s = measure(bb, || {
        let mut os = OStream::with_flush(&mut enc_blob_scratch, 0, Discard::default());
        os.write_blob(1, black_box(&blob)).unwrap();
        black_box(os.flush().unwrap());
    });
    let dec_blob = measure(bb, || {
        let mut sink = Checksum::default();
        let mut is = IStream::new();
        for chunk in black_box(&blob_buf).chunks(BLOB_CHUNK) {
            let _ = is.feed(chunk, &mut sink);
        }
        black_box(sink.acc);
    });

    let mut enc_comp_out = vec![0u8; comp_used];
    let enc_comp = measure(bc, || {
        let mut os = OStream::new(&mut enc_comp_out);
        encode_composite(&mut os);
        black_box(os.bytes_used());
    });
    let dec_comp = measure(bc, || {
        let mut sink = Checksum::default();
        let mut is = IStream::new();
        is.feed(black_box(&comp_buf), &mut sink).unwrap();
        black_box(sink.acc);
    });
    let dec_comp_skip = measure(bc, || {
        black_box(run_decode_composite_skip(black_box(&comp_buf)));
    });

    println!("=== SofaBuffers Rust throughput (CPU time, MB/s) ===");
    println!("{:<26} {:>12}", "Workload", "MB/s");
    println!("{:<26} {:>12}", "--------", "----");
    println!("{:<26} {:>12.2}", "encode: u64 array (1000)", enc_u64);
    println!("{:<26} {:>12.2}", "encode: typical message", enc_typ);
    println!("{:<26} {:>12.2}", "encode: blob 1MB one-shot", enc_blob_1);
    println!("{:<26} {:>12.2}", "encode: blob 1MB streaming", enc_blob_s);
    // `encode: blob 1MB passthrough` is BENCH_SPEC's one optional row and this
    // port implements no pass-through (CORELIB_PLAN §5.1 makes it a MAY), so the
    // row is omitted entirely rather than printed as a placeholder.
    println!("{:<26} {:>12.2}", "encode: composite", enc_comp);
    println!("{:<26} {:>12.2}", "decode: u64 array (1000)", dec_u64);
    println!("{:<26} {:>12.2}", "decode: typical message", dec_typ);
    println!("{:<26} {:>12.2}", "decode: blob 1MB", dec_blob);
    println!("{:<26} {:>12.2}", "decode: composite", dec_comp);
    println!(
        "{:<26} {:>12.2}",
        "decode: composite skip-all", dec_comp_skip
    );
    println!("\nMB = 1e6 bytes. ~1s CPU-time loop per workload.");
    println!("blob 1MB is bandwidth-bound: read one-shot vs streaming, not either alone.");
}
