//! Per-configuration behavioural smoke tests.
//!
//! The conformance suites (`vectors_tests`, `ostream_tests`, …) exercise every
//! wire type and therefore only build with all features on. *This* file is the
//! opposite: it compiles and runs under **any** feature subset, because each
//! feature-specific test is `#[cfg]`-gated to exactly the API it uses. That lets
//! the whole feature powerset be tested for real (encode → decode), not just
//! compiled. Run the matrix with:
//!
//! ```text
//! cargo hack --feature-powerset test --test config_tests
//! ```
//!
//! (Every [`Visitor`] method has a default no-op impl, so each tiny recorder
//! below overrides only the callbacks for the type it checks.)

use sofab::{IStream, OStream, Signed, Unsigned, Visitor};

// Scalars (unsigned / signed / boolean) are always available — no feature gate.
#[test]
fn scalars_roundtrip() {
    #[derive(Default)]
    struct V {
        u: Vec<(u32, Unsigned)>,
        s: Vec<(u32, Signed)>,
    }
    impl Visitor for V {
        fn unsigned(&mut self, id: u32, v: Unsigned) {
            self.u.push((id, v));
        }
        fn signed(&mut self, id: u32, v: Signed) {
            self.s.push((id, v));
        }
    }

    let mut buf = [0u8; 64];
    let used = {
        let mut os = OStream::new(&mut buf);
        os.write_unsigned(1, 42).unwrap();
        os.write_signed(2, -7).unwrap();
        os.write_boolean(3, true).unwrap();
        os.bytes_used()
    };
    let mut v = V::default();
    IStream::new().feed(&buf[..used], &mut v).unwrap();
    assert_eq!(v.u, [(1, 42), (3, 1)]); // boolean decodes as unsigned 1
    assert_eq!(v.s, [(2, -7)]);
}

#[test]
fn value_width_matches_config() {
    #[cfg(feature = "value64")]
    assert_eq!(sofab::config::VALUE_BITS, 64);
    #[cfg(not(feature = "value64"))]
    assert_eq!(sofab::config::VALUE_BITS, 32);
    assert_eq!(sofab::config::VALUE_BITS, Unsigned::BITS);
}

/// With 64-bit values, a value above `u32::MAX` round-trips.
#[cfg(feature = "value64")]
#[test]
fn wide_value_roundtrips() {
    #[derive(Default)]
    struct V {
        u: Vec<Unsigned>,
    }
    impl Visitor for V {
        fn unsigned(&mut self, _id: u32, v: Unsigned) {
            self.u.push(v);
        }
    }
    let big: Unsigned = 5_000_000_000; // > u32::MAX
    let mut buf = [0u8; 16];
    let used = {
        let mut os = OStream::new(&mut buf);
        os.write_unsigned(1, big).unwrap();
        os.bytes_used()
    };
    let mut v = V::default();
    IStream::new().feed(&buf[..used], &mut v).unwrap();
    assert_eq!(v.u, [big]);
}

/// With 32-bit values, a varint whose continuation runs past 32 bits is rejected
/// (matching a 32-bit `sofab_value_t` build of the C reference).
#[cfg(not(feature = "value64"))]
#[test]
fn over_wide_varint_is_rejected() {
    struct Ignore;
    impl Visitor for Ignore {}
    // id 0, unsigned wire type, then five 0x.. continuation bytes (35 bits).
    let bytes = [0x00u8, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
    let mut is = IStream::new();
    assert!(is.feed(&bytes, &mut Ignore).is_err());
}

#[cfg(feature = "fixlen")]
#[test]
fn fixlen_roundtrip() {
    #[derive(Default)]
    struct V {
        fp32: Vec<(u32, u32)>,
        strs: Vec<(u32, Vec<u8>)>,
        blobs: Vec<(u32, Vec<u8>)>,
        pending: Option<(u32, bool, Vec<u8>)>,
    }
    impl V {
        fn acc(&mut self, id: u32, blob: bool, total: usize, off: usize, chunk: &[u8]) {
            if off == 0 {
                self.pending = Some((id, blob, Vec::with_capacity(total)));
            }
            let done = {
                let p = self.pending.as_mut().unwrap();
                p.2.extend_from_slice(chunk);
                p.2.len() == total
            };
            if done {
                let (i, b, buf) = self.pending.take().unwrap();
                if b {
                    self.blobs.push((i, buf));
                } else {
                    self.strs.push((i, buf));
                }
            }
        }
    }
    impl Visitor for V {
        fn fp32(&mut self, id: u32, v: f32) {
            self.fp32.push((id, v.to_bits()));
        }
        fn string(&mut self, id: u32, total: usize, off: usize, c: &[u8]) {
            self.acc(id, false, total, off, c);
        }
        fn blob(&mut self, id: u32, total: usize, off: usize, c: &[u8]) {
            self.acc(id, true, total, off, c);
        }
    }

    let mut buf = [0u8; 64];
    let used = {
        let mut os = OStream::new(&mut buf);
        os.write_fp32(1, 1.5).unwrap();
        os.write_str(2, "hi").unwrap();
        os.write_blob(3, &[9, 8, 7]).unwrap();
        os.bytes_used()
    };
    let mut v = V::default();
    IStream::new().feed(&buf[..used], &mut v).unwrap();
    assert_eq!(v.fp32, [(1, 1.5f32.to_bits())]);
    assert_eq!(v.strs, [(2, b"hi".to_vec())]);
    assert_eq!(v.blobs, [(3, vec![9, 8, 7])]);
}

#[cfg(feature = "fp64")]
#[test]
fn fp64_roundtrip() {
    #[derive(Default)]
    struct V {
        fp64: Vec<(u32, u64)>,
    }
    impl Visitor for V {
        fn fp64(&mut self, id: u32, v: f64) {
            self.fp64.push((id, v.to_bits()));
        }
    }
    let mut buf = [0u8; 32];
    let used = {
        let mut os = OStream::new(&mut buf);
        os.write_fp64(1, 2.5).unwrap();
        os.bytes_used()
    };
    let mut v = V::default();
    IStream::new().feed(&buf[..used], &mut v).unwrap();
    assert_eq!(v.fp64, [(1, 2.5f64.to_bits())]);
}

#[cfg(feature = "array")]
#[test]
fn integer_array_roundtrip() {
    use sofab::ArrayKind;
    #[derive(Default)]
    struct V {
        begins: Vec<(u32, usize)>,
        u: Vec<Unsigned>,
        s: Vec<Signed>,
    }
    impl Visitor for V {
        fn array_begin(&mut self, id: u32, _kind: ArrayKind, n: usize) {
            self.begins.push((id, n));
        }
        fn unsigned(&mut self, _id: u32, v: Unsigned) {
            self.u.push(v);
        }
        fn signed(&mut self, _id: u32, v: Signed) {
            self.s.push(v);
        }
    }
    let mut buf = [0u8; 64];
    let used = {
        let mut os = OStream::new(&mut buf);
        os.write_array_unsigned(1, &[10u32, 20, 30]).unwrap();
        os.write_array_signed(2, &[-1i32, -2]).unwrap();
        os.bytes_used()
    };
    let mut v = V::default();
    IStream::new().feed(&buf[..used], &mut v).unwrap();
    assert_eq!(v.begins, [(1, 3), (2, 2)]);
    assert_eq!(v.u, [10, 20, 30]);
    assert_eq!(v.s, [-1, -2]);
}

#[cfg(all(feature = "array", feature = "fixlen"))]
#[test]
fn float_array_roundtrip() {
    #[derive(Default)]
    struct V {
        fp32: Vec<u32>,
    }
    impl Visitor for V {
        fn fp32(&mut self, _id: u32, v: f32) {
            self.fp32.push(v.to_bits());
        }
    }
    let mut buf = [0u8; 64];
    let used = {
        let mut os = OStream::new(&mut buf);
        os.write_array_fp32(1, &[1.0, 2.0]).unwrap();
        os.bytes_used()
    };
    let mut v = V::default();
    IStream::new().feed(&buf[..used], &mut v).unwrap();
    assert_eq!(v.fp32, [1.0f32.to_bits(), 2.0f32.to_bits()]);
}

#[cfg(feature = "sequence")]
#[test]
fn sequence_roundtrip() {
    #[derive(Default)]
    struct V {
        // Some(id) = begin, None = end
        frames: Vec<Option<u32>>,
        u: Vec<(u32, Unsigned)>,
    }
    impl Visitor for V {
        fn sequence_begin(&mut self, id: u32) {
            self.frames.push(Some(id));
        }
        fn sequence_end(&mut self) {
            self.frames.push(None);
        }
        fn unsigned(&mut self, id: u32, v: Unsigned) {
            self.u.push((id, v));
        }
    }
    let mut buf = [0u8; 64];
    let used = {
        let mut os = OStream::new(&mut buf);
        os.write_sequence_begin(1).unwrap();
        os.write_unsigned(2, 99).unwrap();
        os.write_sequence_end().unwrap();
        os.bytes_used()
    };
    let mut v = V::default();
    IStream::new().feed(&buf[..used], &mut v).unwrap();
    assert_eq!(v.frames, [Some(1), None]);
    assert_eq!(v.u, [(2, 99)]);
}
