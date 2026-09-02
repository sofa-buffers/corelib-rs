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
//! and the streaming-in half, `decoder()` → `<Name>Decoder` with `feed`).
//! It is a *compile* test first and an assertion test second: if this file stops
//! building, every crate `sofabgen` emits has stopped building too.
//!
//! The last block guards the same surface where a *reader* meets it: §9.5 makes
//! the README's Generator example show the one-shot `encode()` / `decode()`
//! helpers **and** the streaming `serialize` / `decoder()` path, and §6.1.1
//! closes the name set they may be spelled with.

use sofab::{Error, IStream, Id, OStream, Status, Unsigned, Visitor};

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
/// the verdict for the bytes handed in — `Ok(Status::Incomplete)` means they
/// ended mid-field and is not a failure — and that status **is** the verdict: §6.0
/// admits "**No** `finish`/`finalize` step", and §6.1's own worked example spells
/// the assembled message `dec.value`.
///
/// **This deviates from `sofabgen`'s Rust backend as it stands today**, which
/// emits a `finish(self) -> Result<Name, Error>` here. `finish` is a name outside
/// §6.1.1's closed set and a finalize step §6.0 forbids, so the pin is written to
/// the specification rather than to the current emitted text; the generator owes
/// the same change (`A2-0104`). What it did *not* do is reclassify: the emitted
/// `finish` fed an empty chunk and passed the `INCOMPLETE` outcome through, so
/// §5.2.4's specific prohibition was never breached — only the name and the step.
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

    pub fn feed(&mut self, chunk: &[u8]) -> Result<Status, Error> {
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

    /// The message assembled so far (§6.1: "`person = dec.value` — assembled
    /// incrementally"). There is no verdict here and none is needed: the caller
    /// already has one from the last `feed`, and asking for the value is not what
    /// decides whether the bytes were complete (§5.2.4, §6.0).
    ///
    /// To probe end-of-input without supplying any bytes, `feed(&[])`:
    /// `Ok(Status::Complete)` only when nothing is half-read, which is what makes
    /// a truncated stream visible rather than a silently partial value.
    pub fn value(self) -> Bounded {
        self.m
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
                OStream::with_flush(&mut scratch, 0, |_d: &[u8]| out.extend_from_slice(_d))
                    .unwrap();
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
        })
        .unwrap();
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
    let mut st = Ok(Status::Complete);
    for chunk in wire.chunks(1) {
        st = dec.feed(chunk);
        match st {
            // Mid-field is not a failure: feed more.
            Ok(Status::Complete) | Ok(Status::Incomplete) => {}
            Err(e) => panic!("malformed: {e}"),
        }
    }
    // `st` is the outcome so far, and at end-of-input that is the verdict —
    // there is no finalize step to ask (§5.2.4, §6.0).
    assert_eq!(st, Ok(Status::Complete));
    let got = dec.value();

    assert_eq!(got.x, m.x);
    assert_eq!(got.tag, m.tag);
}

/// The caller owns end-of-input, so a tail that stops mid-field is truncation —
/// and `feed`'s own status is where that shows, not a finalize step (§5.2.4:
/// "no `finish` step promotes it to an error"; §6.0: "**No** `finish`/`finalize`
/// step").
///
/// The last `feed` already says `Incomplete`; an empty chunk asks the same
/// question again without supplying any bytes, for a caller whose framing ended
/// between two `feed` calls.
#[test]
fn the_generated_decoder_reports_a_truncated_message_from_feed() {
    let wire = Bounded {
        x: 1,
        tag: "truncated".into(),
    }
    .encode();

    let mut dec = Bounded::decoder();
    assert!(matches!(
        dec.feed(&wire[..wire.len() - 1]),
        Ok(Status::Incomplete)
    ));
    // The end-of-input probe: still incomplete, and still not INVALID.
    assert!(matches!(dec.feed(&[]), Ok(Status::Incomplete)));

    // The missing byte completes it — a truncated stream was never rejected,
    // only unfinished.
    assert_eq!(dec.feed(&wire[wire.len() - 1..]), Ok(Status::Complete));
    assert_eq!(dec.value().tag, "truncated");
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
/// Spellings §6.1.1 excludes from the generated-object surface: "the set is
/// closed. Adapt only casing/idiom, never the words."
const FORBIDDEN: [&str; 7] = [
    "marshal",
    "unmarshal",
    "serialize_to",
    "to_bytes",
    "from_bytes",
    "decode_from",
    "decode_into",
];

/// The step §6.0 rules out by name, on top of §6.1.1's closed set:
/// "`feed(bytes)` … **No** `finish`/`finalize` step."
///
/// Checked as a *call or definition* rather than as a bare word, because saying
/// there is **no** finalize step is exactly what §9.6 and §5.2.4 ask the README
/// to say — the prohibition is on teaching the operation, not on naming it.
const FORBIDDEN_STEPS: [&str; 2] = ["finish", "finalize"];

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

/// §6.0's flat "**No** `finish`/`finalize` step" — the rule `A2-0104` found the
/// README teaching, and the one the list above did not cover.
#[test]
fn the_readme_never_teaches_a_finalize_step() {
    for name in FORBIDDEN_STEPS {
        for spelling in [
            format!("fn {name}("),
            format!(".{name}("),
            format!("::{name}("),
        ] {
            assert!(
                !README.contains(&spelling),
                "README teaches `{spelling}`; §6.0 admits no finish/finalize step — \
                 `feed`'s own status is the verdict for the bytes so far (§5.2.4)"
            );
        }
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
