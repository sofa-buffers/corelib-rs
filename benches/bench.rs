//! SofaBuffers Rust — throughput benchmark (MB/s, CPU time).
//!
//! Mirror of `bench/c/bench.c` and `bench/cpp/bench.cpp`: encode/decode
//! throughput for two workloads — a 1000-element u64 array and a small
//! "typical" mixed message. Each workload runs in a ~1 s loop and reports MB/s,
//! and the output table matches the C/C++ tools so the implementations can be
//! compared directly.
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
use std::hint::black_box;

const N: usize = 1000;

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

/// A representative small telemetry-style message: a few scalars, a float, a
/// short string and a small array — plus a nested sequence.
fn encode_typical(os: &mut OStream) {
    os.write_unsigned(1, 0xDEAD_BEEF).unwrap();
    os.write_signed(2, -12345).unwrap();
    os.write_boolean(3, true).unwrap();
    os.write_fp32(4, 3.14159).unwrap();
    os.write_str(5, "sofab").unwrap();
    os.write_array_unsigned(6, &[10u16, 20, 30, 40]).unwrap();
    os.write_sequence_begin(7).unwrap();
    os.write_unsigned(1, 99).unwrap();
    os.write_signed(2, -7).unwrap();
    os.write_sequence_end().unwrap();
}

/// Run `body` repeatedly until ~1 s of CPU time has elapsed (after one warm-up
/// call) and return throughput in MB/s for a message of `bytes` bytes.
fn measure(bytes: usize, mut body: impl FnMut()) -> f64 {
    body(); // warmup
    let t0 = cpu_now();
    let mut it: u64 = 0;
    let mut el;
    loop {
        body();
        it += 1;
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

    let ba = enc_u64_used;
    let bt = typ_used;

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

    println!("=== SofaBuffers Rust throughput (CPU time, MB/s) ===");
    println!("{:<26} {:>12}", "Workload", "MB/s");
    println!("{:<26} {:>12}", "--------", "----");
    println!("{:<26} {:>12.2}", "encode: u64 array (1000)", enc_u64);
    println!("{:<26} {:>12.2}", "encode: typical message", enc_typ);
    println!("{:<26} {:>12.2}", "decode: u64 array (1000)", dec_u64);
    println!("{:<26} {:>12.2}", "decode: typical message", dec_typ);
    println!("\nMB = 1e6 bytes. ~1s CPU-time loop per workload.");
}
