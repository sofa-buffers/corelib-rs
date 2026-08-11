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
//! `generators/rust/backend.go` (`serialize`, both `encode()` shapes §5.1
//! names: bounded schema → one exactly-sized buffer with no sink; unbounded
//! schema → a scratch buffer with a flush sink appending into a growing `Vec`;
//! and the streaming-in half, `decoder()` → `<Name>Decoder` with `feed`/`finish`).
//! It is a *compile* test first and an assertion test second: if this file stops
//! building, every crate `sofabgen` emits has stopped building too.
//!
//! The last block guards the same surface where a *reader* meets it: §9.5 makes
//! the README's Generator example show the one-shot `encode()` / `decode()`
//! helpers **and** the streaming `serialize` / `decoder()` path, and §6.1.1
//! closes the name set they may be spelled with.

use sofab::{Error, IStream, Id, OStream, Unsigned, Visitor};

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

    /// An incremental decoder for this message: hold it and feed chunks as they
    /// arrive, instead of buffering the whole message first (backend.go,
    /// `decoder()`; the generator re-exports the module-private `Decoder` as
    /// `BoundedDecoder`).
    pub fn decoder() -> Decoder {
        Decoder::new()
    }
}

/// The visitor half of the emitted decoder. It borrows the message for the
/// duration of one `feed` and owns the state that must survive a chunk
/// boundary — `acc` reassembles a string payload split across feeds — so the
/// message itself stays a plain typed struct with no decode state on it.
struct V<'a> {
    m: &'a mut Bounded,
    acc: Vec<u8>,
    inv: bool,
}

impl Visitor for V<'_> {
    fn unsigned(&mut self, id: Id, value: Unsigned) {
        if id == 1 {
            self.m.x = value;
        }
    }

    fn string(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {
        if id != 2 {
            return;
        }
        if offset == 0 {
            self.acc.clear();
        }
        let s = if offset == 0 && chunk.len() >= total {
            match core::str::from_utf8(&chunk[..total]) {
                Ok(v) => v,
                Err(_) => {
                    self.inv = true;
                    return;
                }
            }
        } else {
            self.acc.extend_from_slice(chunk);
            if self.acc.len() < total {
                return;
            }
            match core::str::from_utf8(&self.acc[..total]) {
                Ok(v) => v,
                Err(_) => {
                    self.inv = true;
                    return;
                }
            }
        };
        self.m.tag.clear();
        self.m.tag.push_str(s);
    }
}

/// `<Name>Decoder`: the generated reader `decoder()` hands back. `feed` reports
/// the verdict for the bytes handed in — `Err(Incomplete)` means they ended
/// mid-field and is not a failure — and `finish` gives the verdict for the
/// message once the caller's own framing says the input is over.
struct Decoder {
    m: Bounded,
    is: IStream,
    acc: Vec<u8>,
    inv: bool,
}

impl Decoder {
    pub fn new() -> Self {
        Self {
            m: Bounded::default(),
            is: IStream::new(),
            acc: Vec::new(),
            inv: false,
        }
    }

    pub fn feed(&mut self, chunk: &[u8]) -> Result<(), Error> {
        let fed = {
            let mut v = V {
                m: &mut self.m,
                acc: core::mem::take(&mut self.acc),
                inv: self.inv,
            };
            let r = self.is.feed(chunk, &mut v);
            // `..` covers `m`, ending its borrow before the write-back.
            let V { acc, inv, .. } = v;
            self.acc = acc;
            self.inv = inv;
            r
        };
        // INVALID dominates a truncated tail (§5.2), so it is reported ahead of
        // feed's own Incomplete verdict.
        if self.inv {
            return Err(Error::InvalidMsg);
        }
        fed
    }

    pub fn finish(mut self) -> Result<Bounded, Error> {
        if self.inv {
            return Err(Error::InvalidMsg);
        }
        // An empty chunk probes end-of-input without supplying any: Ok only when
        // nothing is half-read, which is what makes a truncated stream an error
        // here rather than a silently partial value.
        self.feed(&[])?;
        Ok(self.m)
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
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
                OStream::with_flush(&mut scratch, 0, |_d: &[u8]| out.extend_from_slice(_d)).unwrap();
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
        }).unwrap();
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

/// §6.1's streaming-in half: `decoder()` returns a reader that consumes
/// arbitrarily small `feed` chunks and binds each decoded field straight into
/// the object, so the message is never fully buffered. One byte at a time is
/// the worst case the corelib has to survive, and it splits the string payload
/// across as many feeds as it has bytes.
#[test]
fn the_generated_decoder_assembles_the_message_from_one_byte_chunks() {
    let m = Bounded {
        x: 70_000,
        tag: "fed one byte at a time".into(),
    };
    let wire = m.encode();

    let mut dec = Bounded::decoder();
    for chunk in wire.chunks(1) {
        match dec.feed(chunk) {
            // Mid-field is not a failure: feed more.
            Ok(()) | Err(Error::Incomplete) => {}
            Err(e) => panic!("malformed: {e}"),
        }
    }
    let got = dec.finish().unwrap();

    assert_eq!(got.x, m.x);
    assert_eq!(got.tag, m.tag);
}

/// The caller owns end-of-input, so a tail that stops mid-field is truncation,
/// and `finish` — not `feed` — is where that becomes an error rather than a
/// half-filled value.
#[test]
fn the_generated_decoder_rejects_a_truncated_message_at_finish() {
    let wire = Bounded {
        x: 1,
        tag: "truncated".into(),
    }
    .encode();

    let mut dec = Bounded::decoder();
    let _ = dec.feed(&wire[..wire.len() - 1]);
    assert!(matches!(dec.finish(), Err(Error::Incomplete)));
}

// ---------------------------------------------------------------------------
// The same surface as the README teaches it (§9.5 + §6.1.1)
// ---------------------------------------------------------------------------

/// The README, embedded at compile time so the test needs no filesystem layout
/// at runtime.
const README: &str = include_str!("../README.md");

/// Spellings §6.1.1 names as the ones a port must not invent. The set is closed
/// precisely so that a developer learns one name per operation across the whole
/// family, and a README that teaches an extra one defeats that as surely as
/// emitting it would — with the added cost that the reader's code will not
/// compile against what `sofabgen` actually emits.
const FORBIDDEN: [&str; 7] = [
    "marshal",
    "unmarshal",
    "serialize_to",
    "to_bytes",
    "from_bytes",
    "decode_from",
    "decode_into",
];

/// Does `hay` contain `needle` as a whole identifier (so `into_bytes` does not
/// count as `to_bytes`)?
fn contains_identifier(hay: &str, needle: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    hay.match_indices(needle).any(|(i, _)| {
        let before = hay[..i].chars().next_back().is_some_and(ident);
        let after = hay[i + needle.len()..].chars().next().is_some_and(ident);
        !before && !after
    })
}

/// The `### Code generator` section — the one §9.5 calls the most common
/// real-world use case — up to the next heading.
fn generator_section() -> &'static str {
    let start = README
        .find("\n### Code generator\n")
        .expect("README has a `### Code generator` section (§9.5 Generator)");
    let body = &README[start + 1..];
    // Past the heading line, then on to the next heading — a line of `#`s
    // followed by a space, which `#[derive(...)]` inside a code block is not.
    let head_end = body.find('\n').map_or(body.len(), |i| i + 1);
    let rest = &body[head_end..];
    let end = rest
        .match_indices('\n')
        .map(|(i, _)| i + 1)
        .find(|&i| {
            let line = &rest[i..];
            line.starts_with('#') && line.trim_start_matches('#').starts_with(' ')
        })
        .unwrap_or(rest.len());
    &body[..head_end + end]
}

#[test]
fn the_readme_never_teaches_a_name_outside_the_closed_set() {
    for name in FORBIDDEN {
        assert!(
            !contains_identifier(README, name),
            "README names `{name}`, which §6.1.1 excludes from the generated-object \
             surface; the canonical spellings are encode/decode/try_decode/serialize/\
             deserialize/decoder"
        );
    }
}

#[test]
fn the_readme_generator_example_shows_the_one_shot_and_the_streaming_pair() {
    let section = generator_section();
    // §9.5: the one-shot helpers AND the streaming path, in §6.1.1's names.
    for needed in [
        "fn encode(",
        "fn decode(",
        "fn try_decode(",
        "fn serialize",
        "fn decoder(",
        "::decoder()",
    ] {
        assert!(
            section.contains(needed),
            "the README's Generator example never shows `{needed}`; §9.5 requires the \
             one-shot encode()/decode() helpers *and* the streaming serialize / \
             decoder() path (§6.1.1)"
        );
    }
}
