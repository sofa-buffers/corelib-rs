//! Tests for streaming/buffer-management API surface: reserve-offset,
//! buffer swapping, and large chunked payload streaming.

mod common;

use common::{Event, Recorder};
use sofab::{IStream, OStream};

#[test]
fn with_offset_reserves_header_space() {
    let mut buf = [0xAAu8; 16];
    let used = {
        let mut os = OStream::with_offset(&mut buf, 4); // reserve 4 header bytes
        os.write_unsigned(0, 42).unwrap();
        os.bytes_used()
    };
    assert_eq!(used, 6); // 4 reserved + 2 payload bytes
    assert_eq!(&buf[..4], &[0xAA, 0xAA, 0xAA, 0xAA]); // header space untouched
    assert_eq!(&buf[4..6], &[0x00, 0x2A]); // field id0 = 42
}

#[test]
fn buffer_set_switches_buffers() {
    let mut a = [0u8; 8];
    let mut b = [0u8; 8];
    let (used_a, used_b) = {
        let mut os = OStream::new(&mut a);
        os.write_unsigned(0, 1).unwrap();
        let ua = os.bytes_used();
        os.buffer_set(&mut b, 0);
        os.write_unsigned(0, 2).unwrap();
        (ua, os.bytes_used())
    };
    assert_eq!((used_a, used_b), (2, 2));
    assert_eq!(&a[..2], &[0x00, 0x01]);
    assert_eq!(&b[..2], &[0x00, 0x02]);
}

#[test]
fn flush_without_sink_reports_pending_bytes() {
    let mut buf = [0u8; 8];
    let mut os = OStream::new(&mut buf);
    os.write_unsigned(0, 7).unwrap();
    // No sink: flush() reports the count but leaves the buffer in place.
    assert_eq!(os.flush(), 2);
    assert_eq!(os.bytes_used(), 2);
}

#[test]
fn large_blob_streams_in_small_chunks() {
    // 300-byte blob: larger than a typical MCU scratch buffer, exercising the
    // chunked string/blob delivery path across many feed() boundaries.
    let data: Vec<u8> = (0..300).map(|i| i as u8).collect();
    let mut buf = vec![0u8; 400];
    let used = {
        let mut os = OStream::new(&mut buf);
        os.write_blob(7, &data).unwrap();
        os.bytes_used()
    };

    let mut rec = Recorder::new();
    let mut is = IStream::new();
    for chunk in buf[..used].chunks(7) {
        is.feed(chunk, &mut rec).unwrap();
    }
    assert_eq!(rec.events, [Event::Blob(7, data)]);
}

#[test]
fn default_constructors_work() {
    // Exercise IStream::default() and a manual NoFlush-typed stream.
    let mut buf = [0u8; 8];
    let mut os = OStream::new(&mut buf);
    os.write_boolean(1, false).unwrap();
    let used = os.bytes_used();

    let mut rec = Recorder::new();
    let mut is = IStream::default();
    is.feed(&buf[..used], &mut rec).unwrap();
    assert_eq!(rec.events, [Event::Unsigned(1, 0)]);
}

#[test]
fn api_version_is_one() {
    // Normative per the architecture spec: the library must expose version 1.
    assert_eq!(sofab::API_VERSION, 1);
}

#[test]
fn config_constants_reflect_features() {
    // The `config` module is the build-time introspection surface that
    // `require!` is built on; it must mirror how the crate was compiled. This
    // test runs under the default (all wire features, 64-bit) configuration.
    assert_eq!(
        [
            sofab::config::FIXLEN,
            sofab::config::ARRAY,
            sofab::config::SEQUENCE,
            sofab::config::FP64,
        ],
        [true, true, true, true]
    );
    assert_eq!(sofab::config::VALUE_BITS, 64);
    assert_eq!(sofab::config::VALUE_BITS, sofab::Unsigned::BITS);
}

// A satisfied `require!` must compile to nothing. (An unsatisfied one is a
// compile error, exercised in the macro's `compile_fail` doctest.)
sofab::require!(fixlen, array, sequence, fp64, value64);
