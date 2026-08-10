//! The generated-object layer of CORELIB_PLAN §6.1, compiled against this crate.
//!
//! §6.1 puts a requirement on the corelib rather than on the generator: "the
//! generated layer must be buildable purely from the streaming primitives …
//! let the generator drive encoding through the same flush-callback / sink +
//! buffer swap mechanism (§5.1), so `serialize` works with an output buffer
//! smaller than the object". Nothing else in this suite notices when that stops
//! being true, because every other test is free to spell the API however the
//! API currently reads — the generated layer is not: its text is fixed by
//! `sofabgen`'s Rust backend and only changes when that backend is regenerated
//! and re-released.
//!
//! So this file holds the emitted text itself, copied from
//! `generators/rust/backend.go` (`serialize`, and both `encode()` shapes §5.1
//! names: bounded schema → one exactly-sized buffer with no sink; unbounded
//! schema → a scratch buffer with a flush sink appending into a growing `Vec`).
//! It is a *compile* test first and an assertion test second: if this file stops
//! building, every crate `sofabgen` emits has stopped building too.

use sofab::OStream;

/// A message whose schema bounds every field: `MAX_SIZE` is derived, one
/// exactly-sized buffer always holds it, and no sink is installed
/// (backend.go, bounded `encode()`).
#[derive(Default)]
struct Bounded {
    x: u64,
    tag: String,
}

impl Bounded {
    /// Worst-case encoded size of this message, derived from the schema.
    pub const MAX_SIZE: usize = 64;

    pub fn serialize<_F: sofab::Flush>(&self, os: &mut OStream<'_, _F>) {
        let _ = os.write_unsigned(1, self.x);
        let _ = os.write_str(2, &self.tag);
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut buf = vec![0u8; Self::MAX_SIZE];
        let used = {
            let mut os = OStream::new(&mut buf);
            self.serialize(&mut os);
            os.bytes_used()
        };
        buf.truncate(used);
        buf
    }
}

/// A message with an unbounded field: `MAX_SIZE` is a configured ceiling, so the
/// buffer must not be sized from it. A scratch buffer with a flush sink appends
/// into the growing result instead (backend.go, unbounded `encode()`).
#[derive(Default)]
struct Unbounded {
    x: u64,
    tag: String,
}

impl Unbounded {
    pub fn serialize<_F: sofab::Flush>(&self, os: &mut OStream<'_, _F>) {
        let _ = os.write_unsigned(1, self.x);
        let _ = os.write_str(2, &self.tag);
    }

    // `os.flush();` discards a `Result`: the generator emits it that way, and
    // this file is a copy of what it emits rather than an improvement on it. The
    // lint is about the emitted text — worth a warning in a generated crate, but
    // never a build failure — so it is silenced here instead of edited away.
    #[allow(unused_must_use)]
    pub fn encode(&self) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        {
            let mut scratch = [0u8; 512];
            let mut os =
                OStream::with_flush(&mut scratch, 0, |_d: &[u8]| out.extend_from_slice(_d));
            self.serialize(&mut os);
            os.flush();
        }
        out
    }
}

/// The bytes both shapes must produce, written through the raw API.
fn reference(x: u64, tag: &str) -> Vec<u8> {
    let mut buf = [0u8; 4096];
    let used = {
        let mut os = OStream::new(&mut buf);
        os.write_unsigned(1, x).unwrap();
        os.write_str(2, tag).unwrap();
        os.bytes_used()
    };
    buf[..used].to_vec()
}

#[test]
fn the_generated_bounded_encode_shape_produces_the_wire_bytes() {
    let m = Bounded {
        x: 300,
        tag: "ada".into(),
    };
    assert_eq!(m.encode(), reference(300, "ada"));
}

#[test]
fn the_generated_unbounded_encode_shape_produces_the_wire_bytes() {
    // Longer than nothing in particular — the point is that the sink path and
    // the sinkless path agree byte for byte (§5.1).
    let tag = "a tag with no schema maxlen, so no worst case exists".to_string();
    let m = Unbounded { x: 70_000, tag };
    assert_eq!(m.encode(), reference(70_000, &m.tag));
}

/// §6.1's actual requirement: the generated `serialize` — with the bound the
/// generator writes, over a stream the generator does not own — works with an
/// output buffer **smaller than the object**. The scratch here is a few bytes
/// against a message of a few dozen, so the string payload is split across many
/// flushes, and the result still equals the one-shot bytes.
#[test]
fn the_generated_serialize_streams_through_a_buffer_smaller_than_the_message() {
    let m = Unbounded {
        x: 1,
        tag: "streamed through a scratch buffer far smaller than this string".into(),
    };

    let mut collected: Vec<u8> = Vec::new();
    {
        let mut scratch = [0u8; 4];
        let mut os = OStream::with_flush(&mut scratch, 0, |chunk: &[u8]| {
            collected.extend_from_slice(chunk)
        });
        m.serialize(&mut os);
        os.flush().unwrap();
    }

    assert_eq!(collected, reference(1, &m.tag));
    assert_eq!(collected, m.encode());
}

/// The default-typed stream is part of the emitted surface too: a generated
/// `serialize` is called with `OStream::new(...)` (no sink, `NoFlush`) on the
/// bounded path, so `_F` must be inferrable there without naming a sink type.
#[test]
fn the_generated_serialize_accepts_a_sinkless_stream() {
    let m = Bounded::default();
    let mut buf = [0u8; Bounded::MAX_SIZE];
    let used = {
        let mut os = OStream::new(&mut buf);
        m.serialize(&mut os);
        os.bytes_used()
    };
    assert_eq!(&buf[..used], reference(0, "").as_slice());
}
