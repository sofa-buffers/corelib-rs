//! Smoke test for the *packaged* crate, run by `.github/workflows/release.yml`.
//!
//! The repository's own test suite proves the code in the working tree is
//! correct. This proves something the test suite structurally cannot: that the
//! artifact `cargo package` produced — the exact file set that reaches
//! crates.io — still builds and works when consumed as an ordinary dependency
//! from outside the repository. A source file left out of the package, or a
//! public item that only resolves through a dev-dependency, fails here and
//! nowhere else.
//!
//! Wire-level conformance is not this file's job: that is the shared
//! `assets/test_vectors.json` suite the release gate runs. Keep this a
//! dependency-free round trip through the public API.

use sofab::{decode, Id, OStream, Signed, Unsigned, Visitor};

#[derive(Default)]
struct Probe {
    a: Unsigned,
    b: Signed,
    s: String,
}

impl Visitor for Probe {
    fn unsigned(&mut self, id: Id, v: Unsigned) {
        if id == 1 {
            self.a = v;
        }
    }
    fn signed(&mut self, id: Id, v: Signed) {
        if id == 2 {
            self.b = v;
        }
    }
    fn string(&mut self, id: Id, _total: usize, _off: usize, chunk: &[u8]) {
        if id == 3 {
            self.s
                .push_str(std::str::from_utf8(chunk).expect("field 3 is UTF-8"));
        }
    }
}

fn main() {
    // Encode into a caller-owned buffer.
    let mut buf = [0u8; 64];
    let used = {
        let mut os = OStream::new(&mut buf);
        os.write_unsigned(1, 42).expect("write field 1");
        os.write_signed(2, -7).expect("write field 2");
        os.write_str(3, "hi").expect("write field 3");
        os.bytes_used()
    };
    let message = &buf[..used];
    assert!(
        !message.is_empty(),
        "three non-default fields must produce bytes"
    );

    // Decode it back through the push-based visitor.
    let mut probe = Probe::default();
    decode(message, &mut probe).expect("decode the message just encoded");
    assert_eq!(probe.a, 42, "field 1 round-tripped");
    assert_eq!(probe.b, -7, "field 2 round-tripped");
    assert_eq!(probe.s, "hi", "field 3 round-tripped");

    // A default-valued message carries no bytes at all (MESSAGE_SPEC §5.1),
    // and decoding nothing must fire no callback rather than fail.
    let mut empty = Probe::default();
    decode(&[], &mut empty).expect("the empty message is valid");
    assert_eq!((empty.a, empty.b, empty.s.as_str()), (0, 0, ""));

    println!("smoke ok — {used} bytes: {message:02x?}");
}
