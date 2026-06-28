//! SofaBuffers Rust — combined per-operation cost benchmark.
//!
//! Mirror of `bench/c/perf.c` and `bench/cpp/perf.cpp`: encodes/decodes the
//! identical message (same field ids, types and values) through the streaming
//! API and prints the identical report, so the C, C++ and Rust implementations
//! can be compared directly. Two complementary metrics per workload:
//!
//!   1. CPU cycles/op  -- cost of the code itself, read off the hardware cycle
//!      counter (x86 TSC via `_rdtsc`, AArch64 virtual count register). Tracks
//!      code changes rather than the host's clock speed.
//!
//!   2. Throughput MB/s -- a "speedtest" for this machine, derived from process
//!      CPU time (`clock()`, not wall-clock). MB = 1e6 bytes.
//!
//! Both metrics are gathered over the same adaptive ~1 s CPU-time loop, so they
//! describe the exact same work.
//!
//! Run with:  `cargo bench --bench perf`

// The float workload values (3.14159…, 6.28318…, e, …) are fixed payload bytes
// chosen to match the C/C++ bench tools exactly. They are deliberately *not*
// `std::f64::consts::{PI,TAU,E}` — using those would change the encoded bytes
// and break cross-language comparison — so silence clippy's approx-constant lint.
#![allow(clippy::approx_constant)]

use sofab::{IStream, Id, OStream, Signed, Unsigned, Visitor};
use std::hint::black_box;

// ---------------------------------------------------------------------------
// hardware cycle counter (same idea as the C/C++ benchmark)
// ---------------------------------------------------------------------------
#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
mod cycles {
    pub const HAVE: bool = true;
    #[inline]
    pub fn read() -> u64 {
        #[cfg(target_arch = "x86")]
        use core::arch::x86::_rdtsc;
        #[cfg(target_arch = "x86_64")]
        use core::arch::x86_64::_rdtsc;
        // SAFETY: rdtsc is part of the baseline x86/x86_64 instruction set.
        unsafe { _rdtsc() }
    }
}
#[cfg(target_arch = "aarch64")]
mod cycles {
    pub const HAVE: bool = true;
    #[inline]
    pub fn read() -> u64 {
        let v: u64;
        // SAFETY: cntvct_el0 is readable from EL0 on AArch64.
        unsafe { core::arch::asm!("mrs {}, cntvct_el0", out(reg) v) };
        v
    }
}
#[cfg(not(any(target_arch = "x86_64", target_arch = "x86", target_arch = "aarch64")))]
mod cycles {
    pub const HAVE: bool = false;
    pub fn read() -> u64 {
        0
    }
}

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

// ---------------------------------------------------------------------------
// message under test (identical to perf.c / perf.cpp)
// ---------------------------------------------------------------------------
const PERF_STRING: &str = "perf-benchmark-message";

const PERF_SAMPLES: [u32; 8] = [
    1_000_000, 2_000_000, 3_000_000, 4_000_000, 5_000_000, 6_000_000, 7_000_000, 8_000_000,
];
const PERF_DELTAS: [i32; 8] = [
    -100_000, -200_000, -300_000, -400_000, -500_000, -600_000, -700_000, -800_000,
];
const PERF_FP64: [f64; 4] = [3.14159265, 6.28318530, 9.42477795, 12.56637060];

fn perf_encode(buf: &mut [u8]) -> usize {
    let mut os = OStream::new(buf);
    os.write_unsigned(1, 0xDEAD_BEEF).unwrap();
    os.write_signed(2, -12345).unwrap();
    os.write_unsigned(3, 0x0123_4567_89AB_CDEF).unwrap();
    os.write_signed(4, -5_000_000_000_000).unwrap();
    os.write_boolean(5, true).unwrap();
    os.write_fp32(6, 3.14159).unwrap();
    os.write_fp64(7, 2.718281828459045).unwrap();
    os.write_str(8, PERF_STRING).unwrap();
    os.write_array_unsigned(9, &PERF_SAMPLES).unwrap();
    os.write_array_signed(10, &PERF_DELTAS).unwrap();
    os.write_array_fp64(11, &PERF_FP64).unwrap();
    os.write_sequence_begin(12).unwrap();
    os.write_unsigned(1, 99).unwrap();
    os.write_signed(2, -7).unwrap();
    os.write_sequence_end().unwrap();
    os.bytes_used()
}

/// Decode sink: folds every value into a checksum (so nothing is elided) and
/// captures the top-level `u32` (id 1) and the string (id 8) for the self-check.
/// Fixed-size string buffer — no per-iteration heap allocation, like the C tool.
#[derive(Default)]
struct PerfOut {
    acc: u64,
    depth: i32,
    u32_top: u32,
    str_len: usize,
    str_buf: [u8; 32],
}

impl Visitor for PerfOut {
    fn unsigned(&mut self, id: Id, v: Unsigned) {
        self.acc = self.acc.wrapping_add(v ^ id as u64);
        if self.depth == 0 && id == 1 {
            self.u32_top = v as u32;
        }
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
    fn string(&mut self, id: Id, _total: usize, offset: usize, chunk: &[u8]) {
        self.acc = self.acc.wrapping_add(chunk.len() as u64);
        if id == 8 && offset < self.str_buf.len() {
            let end = (offset + chunk.len()).min(self.str_buf.len());
            self.str_buf[offset..end].copy_from_slice(&chunk[..end - offset]);
            self.str_len = end;
        }
    }
    fn blob(&mut self, _id: Id, _total: usize, _offset: usize, chunk: &[u8]) {
        self.acc = self.acc.wrapping_add(chunk.len() as u64);
    }
    fn sequence_begin(&mut self, _id: Id) {
        self.depth += 1;
    }
    fn sequence_end(&mut self) {
        self.depth -= 1;
    }
}

fn perf_decode(buf: &[u8], out: &mut PerfOut) {
    let mut is = IStream::new();
    is.feed(buf, out).unwrap();
}

// ---------------------------------------------------------------------------
// measurement
// ---------------------------------------------------------------------------
struct PerfResult {
    iters: u64,
    cycles_op: f64, // hardware cycles per operation
    ns_op: f64,     // CPU nanoseconds per operation
    mb_s: f64,      // throughput, MB/s (MB = 1e6 bytes)
}

fn perf_report(what: &str, r: &PerfResult, bytes: usize) {
    println!("\n--- perf: {what} ---");
    println!("  iterations    : {}", r.iters);
    println!("  message size  : {bytes} bytes");
    if cycles::HAVE {
        println!(
            "  cycles/op     : {:.1}  (hardware cycle counter)",
            r.cycles_op
        );
    } else {
        println!("  cycles/op     : (cycle counter unavailable on this arch)");
    }
    println!(
        "  CPU time/op   : {:.1} ns  (process CPU time, not wall-clock)",
        r.ns_op
    );
    println!(
        "  throughput    : {:.1} MB/s  (speedtest, MB = 1e6 bytes)",
        r.mb_s
    );
}

fn measure_encode(buf: &mut [u8]) -> (PerfResult, usize) {
    let mut msg = 0;
    for _ in 0..1000 {
        msg = perf_encode(buf); // warmup
    }

    let mut sink: usize = 0;
    let mut it: u64 = 0;
    let c0 = cycles::read();
    let t0 = cpu_now();
    let mut el;
    loop {
        sink = sink.wrapping_add(perf_encode(buf));
        it += 1;
        el = cpu_now() - t0;
        if el >= 1.0 {
            break;
        }
    }
    let c1 = cycles::read();
    black_box(sink);

    let r = PerfResult {
        iters: it,
        cycles_op: (c1 - c0) as f64 / it as f64,
        ns_op: el / it as f64 * 1e9,
        mb_s: msg as f64 * it as f64 / el / 1e6,
    };
    (r, msg)
}

fn measure_decode(buf: &[u8]) -> PerfResult {
    let mut out = PerfOut::default();
    for _ in 0..1000 {
        out = PerfOut::default();
        perf_decode(buf, &mut out); // warmup
    }
    black_box(out.acc);

    let mut sink: u64 = 0;
    let mut it: u64 = 0;
    let c0 = cycles::read();
    let t0 = cpu_now();
    let mut el;
    loop {
        let mut o = PerfOut::default();
        perf_decode(black_box(buf), &mut o);
        sink = sink.wrapping_add(o.acc);
        it += 1;
        el = cpu_now() - t0;
        if el >= 1.0 {
            break;
        }
    }
    let c1 = cycles::read();
    black_box(sink);

    PerfResult {
        iters: it,
        cycles_op: (c1 - c0) as f64 / it as f64,
        ns_op: el / it as f64 * 1e9,
        mb_s: buf.len() as f64 * it as f64 / el / 1e6,
    }
}

fn main() {
    let mut buffer = [0u8; 512];

    println!("=== SofaBuffers Rust per-op cost (cycles/op + throughput MB/s) ===");

    let (enc, msg_size) = measure_encode(&mut buffer);
    perf_report("serialize (stream API)", &enc, msg_size);

    // Sanity check that the decode actually reproduced the data.
    let mut out = PerfOut::default();
    perf_decode(&buffer[..msg_size], &mut out);
    if out.u32_top != 0xDEAD_BEEF || &out.str_buf[..out.str_len] != PERF_STRING.as_bytes() {
        eprintln!("perf: decode self-check failed");
        std::process::exit(1);
    }

    let dec = measure_decode(&buffer[..msg_size]);
    perf_report("deserialize (stream API)", &dec, msg_size);

    println!("\ncycles/op tracks code cost; MB/s is this machine's throughput.");
}
