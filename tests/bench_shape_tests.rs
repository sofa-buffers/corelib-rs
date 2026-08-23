//! The benchmark tools against BENCH_SPEC (CORELIB_PLAN §10).
//!
//! This repo is BENCH_SPEC's *reference implementation*: "`corelib-rs/benches/
//! bench.rs` and `corelib-rs/benches/perf.rs` are the textual golden reference
//! for the `bench`/`perf` format", and the encoded size of a dataset is taken
//! from here and then "must match on every port". Two things follow, and both
//! are asserted below.
//!
//! **The parity sizes are a contract, not an observation.** BENCH_SPEC pins the
//! `perf` message at 170 bytes and the `blob 1MB` message at 1,000,005, and says
//! of `composite` only that the number comes from the reference implementation.
//! A port that encodes it differently is diverging, and the way that gets caught
//! is by every port comparing against one written-down figure — so the figure
//! has to be written down here, checked against what this crate actually
//! encodes, and repeated in the README where another port's author will look for
//! it.
//!
//! **A benchmark row must measure the work it names.** `bench` prints a number
//! for every row whatever happens: a decode that goes `INVALID` on the first
//! chunk, or a streaming encode whose sink is never called, still prints a
//! plausible-looking figure — a faster one, in fact, which is exactly the
//! direction that gets a broken row believed. The rows that can degenerate
//! quietly are the ones whose end-to-end behaviour is pinned here: the flush
//! handover the `blob 1MB streaming` row exists to measure, the chunked feed
//! behind `decode: blob 1MB`, and the five paths `composite` was added for.
//!
//! The tools themselves carry the same checks at runtime (`bench` asserts its
//! parity sizes and self-checks each workload before measuring); these tests
//! keep the two tools, the spec's row set and the README in agreement without
//! anyone having to run a benchmark to find out.

// The `composite` string field is a fixed byte pattern shared with every other
// port, not a Unicode identity crisis; and the perf message's floats are payload
// bytes chosen to match the C/C++ tools, deliberately not `std::f64::consts`.
#![allow(clippy::approx_constant)]

use sofab::{IStream, Id, OStream, Signed, Unsigned, Visitor};
use std::fmt::Write as _;

const BENCH_RS: &str = include_str!("../benches/bench.rs");
const CALLGRIND_SH: &str = include_str!("../benches/run_callgrind.sh");

/// Every row BENCH_SPEC's throughput table requires, in the order it prints
/// them. `encode: blob 1MB passthrough` is deliberately absent: it is the one
/// optional row, and a port that does not implement pass-through "omits it
/// entirely rather than printing a placeholder".
const ROWS: [&str; 10] = [
    "encode: u64 array (1000)",
    "encode: typical message",
    "encode: blob 1MB one-shot",
    "encode: blob 1MB streaming",
    "encode: composite",
    "decode: u64 array (1000)",
    "decode: typical message",
    "decode: blob 1MB",
    "decode: composite",
    "decode: composite skip-all",
];

/// Encoded size of the `blob 1MB` message: a 1-byte header, a 4-byte
/// `fixlen_word` and 1,000,000 payload bytes (BENCH_SPEC).
const BLOB_SIZE: usize = 1_000_005;

/// Encoded size of the `composite` message. BENCH_SPEC takes this number from
/// the reference implementation — this crate — and every port must match it.
const COMPOSITE_SIZE: usize = 956;

/// Encoded size of the `perf` message, pinned by BENCH_SPEC at 170 bytes: "if
/// your `perf` prints a different `message size`, your encoding diverges".
const PERF_SIZE: usize = 170;

/// The buffer the `blob 1MB streaming` row is driven through on every port.
const BLOB_CHUNK: usize = 4096;

// ---------------------------------------------------------------------------
// the datasets, written out from BENCH_SPEC rather than shared with the tools
// ---------------------------------------------------------------------------
// Deliberately a second transcription: a parity check that reads its expected
// bytes from the thing under test checks nothing. These are the spec's tables,
// typed from the spec.

fn blob_payload() -> Vec<u8> {
    (0..1_000_000u64)
        .map(|i| i.wrapping_mul(0x9E37_79B9_7F4A_7C15) as u8)
        .collect()
}

fn encode_composite(os: &mut OStream) {
    os.write_sequence_begin_lazy(1).unwrap();
    let mut element = String::new();
    for i in 0..64u32 {
        element.clear();
        element.push_str("item-");
        write!(element, "{i}").unwrap();
        os.write_str(i, &element).unwrap();
    }
    os.write_sequence_end().unwrap();

    os.write_str(2, &"a\u{e4}\u{20ac}\u{1d11e}".repeat(32))
        .unwrap();

    os.write_sequence_begin_lazy(3).unwrap();
    os.write_sequence_begin_lazy(1).unwrap();
    os.write_sequence_begin_lazy(1).unwrap();
    os.write_unsigned(1, 7).unwrap();
    os.write_sequence_end().unwrap();
    os.write_sequence_end().unwrap();
    os.write_signed(2, -1).unwrap();
    os.write_sequence_end().unwrap();

    // Equal to its declared default: opened, and dropped without a byte.
    os.write_sequence_begin_lazy(4).unwrap();
    os.write_sequence_end().unwrap();

    os.write_unsigned(130, 0xDEAD_BEEF).unwrap();
}

fn encode_perf_message(os: &mut OStream) {
    os.write_unsigned(1, 0xDEAD_BEEF).unwrap();
    os.write_signed(2, -12345).unwrap();
    os.write_unsigned(3, 0x0123_4567_89AB_CDEF).unwrap();
    os.write_signed(4, -5_000_000_000_000).unwrap();
    os.write_boolean(5, true).unwrap();
    os.write_fp32(6, 3.14159).unwrap();
    os.write_fp64(7, 2.718281828459045).unwrap();
    os.write_str(8, "perf-benchmark-message").unwrap();
    os.write_array_unsigned(
        9,
        &[
            1_000_000u32,
            2_000_000,
            3_000_000,
            4_000_000,
            5_000_000,
            6_000_000,
            7_000_000,
            8_000_000,
        ],
    )
    .unwrap();
    os.write_array_signed(
        10,
        &[
            -100_000i32,
            -200_000,
            -300_000,
            -400_000,
            -500_000,
            -600_000,
            -700_000,
            -800_000,
        ],
    )
    .unwrap();
    os.write_array_fp64(11, &[3.14159265, 6.28318530, 9.42477795, 12.56637060])
        .unwrap();
    os.write_sequence_begin_lazy(12).unwrap();
    os.write_unsigned(1, 99).unwrap();
    os.write_signed(2, -7).unwrap();
    os.write_sequence_end().unwrap();
}

/// Encode with `build` into a fresh buffer and return the bytes.
fn encoded(cap: usize, build: impl FnOnce(&mut OStream)) -> Vec<u8> {
    let mut buf = vec![0u8; cap];
    let used = {
        let mut os = OStream::new(&mut buf);
        build(&mut os);
        os.bytes_used()
    };
    buf.truncate(used);
    buf
}

// ---------------------------------------------------------------------------
// what the datasets encode to — the numbers every port is compared against
// ---------------------------------------------------------------------------

#[test]
fn the_blob_1mb_message_encodes_to_the_size_every_port_must_match() {
    let blob = blob_payload();
    let wire = encoded(BLOB_SIZE + 64, |os| os.write_blob(1, &blob).unwrap());
    assert_eq!(
        wire.len(),
        BLOB_SIZE,
        "BENCH_SPEC pins the blob 1MB message at {BLOB_SIZE} bytes — a 1-byte \
         header, a 4-byte fixlen_word and a megabyte of payload — as a \
         cross-port parity check"
    );
    // The framing, spelled out, so a header-width regression is named rather
    // than showing up as an off-by-a-few in the total.
    assert_eq!(wire[0], (1 << 3) | 2, "field header (id 1, fixlen)");
    let mut word = (1_000_000u64 << 3) | 3;
    let expected: Vec<u8> = std::iter::from_fn(|| {
        if word == 0 {
            return None;
        }
        let b = (word & 0x7F) as u8;
        word >>= 7;
        Some(if word == 0 { b } else { b | 0x80 })
    })
    .collect();
    assert_eq!(
        &wire[1..5],
        &expected[..],
        "fixlen_word = (1000000 << 3) | 3, base-128"
    );
    assert_eq!(&wire[5..], &blob[..], "payload, byte for byte");
}

#[test]
fn the_composite_message_encodes_to_the_size_every_port_must_match() {
    let wire = encoded(4096, encode_composite);
    assert_eq!(
        wire.len(),
        COMPOSITE_SIZE,
        "BENCH_SPEC takes the composite message's encoded size from the \
         reference implementation — this crate — and every port must then match \
         it; changing what this encodes to changes a number other ports check \
         themselves against"
    );
}

#[test]
fn the_perf_message_encodes_to_170_bytes() {
    let wire = encoded(512, encode_perf_message);
    assert_eq!(
        wire.len(),
        PERF_SIZE,
        "BENCH_SPEC: the perf message is 170 bytes on every implementation, and \
         a different `message size` means the encoding diverged"
    );
}

// ---------------------------------------------------------------------------
// the rows measure what they are named after
// ---------------------------------------------------------------------------

#[test]
fn the_streaming_blob_row_drives_the_flush_handover_it_claims_to_measure() {
    let blob = blob_payload();
    let mut scratch = [0u8; BLOB_CHUNK];

    // A closure sink, the form the crate documents and the bench tool uses:
    // it copies (here, counts) and the blanket impl hands the buffer straight
    // back, which is the §5.1 handover the streaming row exists to measure.
    let (mut calls, mut bytes, mut widest) = (0usize, 0usize, 0usize);
    {
        let mut os = OStream::with_flush(&mut scratch, 0, |data: &[u8]| {
            calls += 1;
            bytes += data.len();
            widest = widest.max(data.len());
        })
        .unwrap();
        os.write_blob(1, &blob).unwrap();
        os.flush().unwrap();
    }

    assert_eq!(
        bytes, BLOB_SIZE,
        "the streaming row must put the whole message through the sink; \
         anything less and the row is timing a truncated encode"
    );
    assert!(
        calls >= BLOB_SIZE / BLOB_CHUNK,
        "the streaming row went through {calls} flush(es) for {BLOB_SIZE} bytes \
         of a {BLOB_CHUNK}-byte buffer; BENCH_SPEC's point is that this row pays \
         ~245 of them and the one-shot row pays none"
    );
    assert!(
        widest <= BLOB_CHUNK,
        "a flush handed out {widest} bytes from a {BLOB_CHUNK}-byte buffer — the \
         payload reached the sink without passing through the buffer, which is \
         the pass-through path BENCH_SPEC requires this row *not* to take"
    );
}

/// Folds what a decode delivered, so a row that stops early is visible.
#[derive(Default)]
struct Delivered {
    payload: usize,
    strings: usize,
    seq_begin: usize,
    seq_end: usize,
    scalars: Vec<(Id, i64)>,
}

impl Visitor for Delivered {
    fn unsigned(&mut self, id: Id, v: Unsigned) {
        self.scalars.push((id, v as i64));
    }
    fn signed(&mut self, id: Id, v: Signed) {
        self.scalars.push((id, v));
    }
    fn string(&mut self, _id: Id, _total: usize, offset: usize, chunk: &[u8]) {
        if offset == 0 {
            self.strings += 1;
        }
        self.payload += chunk.len();
    }
    fn blob(&mut self, _id: Id, _total: usize, _offset: usize, chunk: &[u8]) {
        self.payload += chunk.len();
    }
    fn sequence_begin(&mut self, _id: Id) {
        self.seq_begin += 1;
    }
    fn sequence_end(&mut self) {
        self.seq_end += 1;
    }
}

#[test]
fn the_blob_decode_row_feeds_every_chunk_and_ends_complete() {
    let blob = blob_payload();
    let wire = encoded(BLOB_SIZE + 64, |os| os.write_blob(1, &blob).unwrap());

    let mut sink = Delivered::default();
    let mut is = IStream::new();
    let mut chunks = 0;
    let mut last = Err(sofab::Error::Incomplete);
    for chunk in wire.chunks(BLOB_CHUNK) {
        chunks += 1;
        last = is.feed(chunk, &mut sink);
    }

    assert_eq!(chunks, BLOB_SIZE.div_ceil(BLOB_CHUNK));
    assert!(
        last.is_ok(),
        "the last chunk of the blob 1MB message left the decode at {last:?} \
         rather than COMPLETE; a row that gives up on chunk 1 still prints a \
         number, and a much better-looking one"
    );
    assert_eq!(
        sink.payload, 1_000_000,
        "the chunked decode delivered {} of 1,000,000 payload bytes",
        sink.payload
    );
}

#[test]
fn the_composite_dataset_exercises_the_five_paths_it_was_added_for() {
    let wire = encoded(4096, encode_composite);

    let mut sink = Delivered::default();
    let mut is = IStream::new();
    is.feed(&wire, &mut sink)
        .expect("composite decodes COMPLETE");

    // Field 1 is a wrapper array of 64 string elements; field 2 is the 65th
    // string.
    assert_eq!(
        sink.strings, 65,
        "64 wrapper elements plus the UTF-8 string"
    );
    assert_eq!(
        sink.payload,
        64 * 6 + 54 * 7 - 54 * 6 + 320,
        "\"item-0\"..\"item-63\" plus 320 bytes of multi-width UTF-8"
    );

    // Field 4 is the one field the encoder must *not* write: four sequences on
    // the wire (the wrapper array, and the depth-3 nest), not five.
    assert_eq!(
        sink.seq_begin, 4,
        "the all-default field 4 was framed instead of omitted, so the \
         hold-back's discard path is not on this workload's hot path any more"
    );
    assert_eq!(sink.seq_end, sink.seq_begin);

    assert_eq!(
        sink.scalars,
        vec![(1, 7), (2, -1), (130, 0xDEAD_BEEF)],
        "the depth-3 nest and the two-byte-header field 130"
    );

    // Field 130 is the suite's only two-byte field header, and the wrapper
    // array's element ids straddle the one-byte boundary on their own.
    // `(130 << 3) | T_VARINT_UNSIGNED`, and that wire type is 0.
    let header: u32 = 130 << 3;
    assert_eq!(header, 1040);
    assert!(
        wire.windows(2)
            .any(|w| w == [(header & 0x7F) as u8 | 0x80, (header >> 7) as u8]),
        "the two-byte field header for id 130 is not on the wire"
    );
    assert_eq!(
        wire[0],
        (1 << 3) | 6,
        "id 1 opens a sequence (wrapper array)"
    );
}

#[test]
fn a_skip_all_decode_still_walks_the_whole_composite_message() {
    struct SkipAll;
    impl Visitor for SkipAll {}

    let wire = encoded(4096, encode_composite);
    let mut is = IStream::new();
    is.feed(&wire, &mut SkipAll)
        .expect("skipping every field is still a COMPLETE decode");
}

// ---------------------------------------------------------------------------
// the three tools agree with each other and with BENCH_SPEC's row set
// ---------------------------------------------------------------------------

/// Byte offset of the quoted literal `"<needle>"` in `haystack`.
fn quoted_at(haystack: &str, needle: &str) -> Option<usize> {
    haystack.find(&format!("\"{needle}\""))
}

#[test]
fn bench_prints_every_required_row_in_spec_order() {
    let mut previous = 0usize;
    for row in ROWS {
        let at = quoted_at(BENCH_RS, row)
            .unwrap_or_else(|| panic!("`bench` never prints BENCH_SPEC's `{row}` row"));
        assert!(
            at > previous,
            "`{row}` is printed out of order; the harness tolerates it but a \
             reader comparing two ports' tables side by side does not"
        );
        previous = at;
    }
}

/// The workload names `run_callgrind.sh` runs, in list order.
fn callgrind_workloads() -> Vec<String> {
    let start = CALLGRIND_SH
        .find("WORKLOADS=(")
        .expect("the script lists its workloads");
    let rest = &CALLGRIND_SH[start + "WORKLOADS=(".len()..];
    let end = rest.find(')').expect("the workload list closes");
    rest[..end].split_whitespace().map(str::to_string).collect()
}

#[test]
fn callgrind_runs_exactly_the_workloads_bench_can_dispatch() {
    let workloads = callgrind_workloads();
    assert_eq!(
        workloads.len(),
        ROWS.len(),
        "the Callgrind table and the throughput table must cover the same \
         workloads; got {workloads:?}"
    );
    for w in &workloads {
        assert!(
            quoted_at(BENCH_RS, w).is_some(),
            "`run_callgrind.sh` runs `{w}`, which the bench binary cannot \
             dispatch — the row would print as `-` and the tool would still \
             exit 0"
        );
        assert!(
            BENCH_RS.contains(&format!("pub fn run_{w}")),
            "the bench binary exposes no `run_{w}` entry point, so Callgrind \
             would collect nothing for it"
        );
    }
    assert!(
        CALLGRIND_SH.contains("--toggle-collect=\"run_$1\""),
        "the script must toggle collection around the per-workload `run_*` \
         symbol; without it Callgrind reports the whole process, setup included"
    );
}

#[test]
fn callgrind_labels_its_rows_exactly_as_the_throughput_table_does() {
    for (workload, row) in callgrind_workloads().iter().zip(ROWS) {
        let arm = format!("{workload})");
        let at = CALLGRIND_SH
            .find(&arm)
            .unwrap_or_else(|| panic!("`label()` has no arm for `{workload}`"));
        let line_end = CALLGRIND_SH[at..].find('\n').unwrap_or(0) + at;
        assert!(
            CALLGRIND_SH[at..line_end].contains(row),
            "`run_callgrind.sh` labels `{workload}` as something other than \
             `{row}`; the two tables are read against each other"
        );
    }
}

#[test]
fn a_callgrind_workload_that_measured_nothing_is_an_error_not_a_dash() {
    assert!(
        CALLGRIND_SH.contains("exit 1") && CALLGRIND_SH.contains("no instruction count"),
        "`run_callgrind.sh` prints `-` for a workload whose run produced no \
         summary and still exits 0; BENCH_SPEC wants a dataset that runs end to \
         end and prints a real number, so a missing one has to fail the tool"
    );
}

#[test]
fn the_optional_passthrough_row_is_omitted_rather_than_stubbed() {
    assert!(
        quoted_at(BENCH_RS, "encode: blob 1MB passthrough").is_none(),
        "this port copies string/blob runs through the output buffer, so \
         BENCH_SPEC's optional pass-through row must be absent — not printed \
         with a placeholder value"
    );
    for (doc, what) in [(BENCH_RS, "bench"), (CALLGRIND_SH, "run_callgrind.sh")] {
        assert!(
            doc.contains("passthrough") || doc.contains("pass-through"),
            "{what} never says why BENCH_SPEC's optional pass-through row is \
             missing; absent-and-explained is a port property, absent-and-silent \
             looks like an oversight"
        );
    }
}
