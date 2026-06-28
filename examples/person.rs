//! A hand-written stand-in for what the `generator` repo emits.
//!
//! ARCHITECTURE §6.1 says a corelib must be enough to build a **thin
//! generated-object layer** whose API is *dead simple* (`serialize` /
//! `deserialize`) yet which can **also stream in chunks**. This example is that
//! layer for one message type, `Person`, written entirely on top of the public
//! `sofab` primitives — no access to crate internals. A real generator would
//! produce this from a schema; here it is by hand to prove the corelib suffices.
//!
//! Run with: `cargo run --example person`
//!
//! Schema (field ids are an implementation detail the user never sees):
//! ```text
//! message Person {
//!   1: string        name
//!   2: unsigned      age
//!   3: signed[]      scores   // varint-array of i64
//!   4: sequence tags {        // dynamic array of strings: a sequence whose
//!        i: string element    // i-th element carries id = i (unique per scope)
//!   }
//! }
//! ```

use sofab::{decode, Flush, IStream, OStream, Result, Visitor};
use sofab::{ArrayKind, Id, Signed, Unsigned};

/// The "generated" object: ordinary typed fields, no wire concepts in sight.
#[derive(Debug, Default, Clone, PartialEq)]
struct Person {
    name: String,
    age: u64,
    scores: Vec<i64>,
    tags: Vec<String>,
}

// Field ids — hidden inside the generated code.
const ID_NAME: Id = 1;
const ID_AGE: Id = 2;
const ID_SCORES: Id = 3;
const ID_TAGS: Id = 4;

impl Person {
    // ---- streaming OUT -----------------------------------------------------

    /// Write this object into any `sofab` sink. The buffer behind `os` can be
    /// far smaller than the message — this is the chunked-output path.
    fn serialize_to<F: Flush>(&self, os: &mut OStream<F>) -> Result<()> {
        os.write_str(ID_NAME, &self.name)?;
        os.write_unsigned(ID_AGE, self.age)?;
        if !self.scores.is_empty() {
            os.write_array_signed(ID_SCORES, &self.scores)?;
        }
        if !self.tags.is_empty() {
            // A dynamic array of variable-length strings → a sequence, each
            // element under its own (sequence-local) id.
            os.write_sequence_begin(ID_TAGS)?;
            for (i, tag) in self.tags.iter().enumerate() {
                os.write_str(i as Id, tag)?;
            }
            os.write_sequence_end()?;
        }
        Ok(())
    }

    /// One-shot convenience: serialize to a freshly grown `Vec`. Internally it
    /// still drives the streaming encoder through a small scratch buffer + flush
    /// sink, so the heavy lifting is the same code path as `serialize_to`.
    fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut scratch = [0u8; 32]; // deliberately tiny to exercise flushing
        {
            let mut os =
                OStream::with_flush(&mut scratch, 0, |chunk: &[u8]| out.extend_from_slice(chunk));
            self.serialize_to(&mut os).expect("vec sink never fills");
            os.flush();
        }
        out
    }

    // ---- streaming IN ------------------------------------------------------

    /// One-shot convenience: decode a whole message (zero-copy fast path).
    fn deserialize(bytes: &[u8]) -> Result<Person> {
        let mut b = Builder::default();
        decode(bytes, &mut b)?;
        Ok(b.person)
    }

    /// Incremental decoder: feed arbitrarily small chunks, then `finish`.
    fn decoder() -> PersonDecoder {
        PersonDecoder {
            stream: IStream::new(),
            builder: Builder::default(),
        }
    }
}

/// Visitor that assembles a [`Person`] field-by-field. Works identically for the
/// one-shot and chunked paths because it reassembles string payloads itself.
#[derive(Default)]
struct Builder {
    person: Person,
    depth: u32,
    // current string field being reassembled
    sbuf: Vec<u8>,
    sid: Id,
}

impl Visitor for Builder {
    fn unsigned(&mut self, id: Id, value: Unsigned) {
        if self.depth == 0 && id == ID_AGE {
            self.person.age = value;
        }
    }

    fn signed(&mut self, id: Id, value: Signed) {
        // Top-level signed values here are scores[] array elements (id ID_SCORES).
        if self.depth == 0 && id == ID_SCORES {
            self.person.scores.push(value);
        }
    }

    fn array_begin(&mut self, id: Id, _kind: ArrayKind, count: usize) {
        if self.depth == 0 && id == ID_SCORES {
            self.person.scores.reserve(count);
        }
    }

    fn string(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {
        if offset == 0 {
            self.sbuf.clear();
            self.sbuf.reserve(total);
            self.sid = id;
        }
        self.sbuf.extend_from_slice(chunk);
        if self.sbuf.len() == total {
            let s = String::from_utf8_lossy(&self.sbuf).into_owned();
            if self.depth == 0 && self.sid == ID_NAME {
                self.person.name = s;
            } else if self.depth >= 1 {
                // inside the tags sequence
                self.person.tags.push(s);
            }
        }
    }

    fn sequence_begin(&mut self, _id: Id) {
        self.depth += 1;
    }

    fn sequence_end(&mut self) {
        self.depth -= 1;
    }
}

/// A generated incremental reader bound to a `sofab` decoder.
struct PersonDecoder {
    stream: IStream,
    builder: Builder,
}

impl PersonDecoder {
    fn feed(&mut self, chunk: &[u8]) -> Result<()> {
        self.stream.feed(chunk, &mut self.builder)
    }
    fn finish(mut self) -> Result<Person> {
        self.stream.finish()?;
        Ok(core::mem::take(&mut self.builder.person))
    }
}

fn main() {
    let p = Person {
        name: "Ada Lovelace".to_string(),
        age: 36,
        scores: vec![100, -7, 42, -1_000_000],
        tags: vec!["math".into(), "poetry".into(), "engines".into()],
    };

    // 1) one-shot round-trip
    let bytes = p.serialize();
    println!("encoded {} bytes: {}", bytes.len(), hex(&bytes));
    let back = Person::deserialize(&bytes).expect("decode");
    assert_eq!(p, back);
    println!("one-shot round-trip: OK -> {back:?}");

    // 2) chunked round-trip: feed the decoder one byte at a time
    let mut dec = Person::decoder();
    for b in &bytes {
        dec.feed(&[*b]).expect("chunked decode");
    }
    let streamed = dec.finish().expect("finish");
    assert_eq!(p, streamed);
    println!("byte-at-a-time round-trip: OK");

    // 3) streaming OUT through a tiny buffer already happened inside serialize();
    //    show that the produced bytes are byte-identical regardless of path.
    let mut one_big = vec![0u8; 256];
    let n = {
        let mut os = OStream::new(&mut one_big);
        p.serialize_to(&mut os).unwrap();
        os.bytes_used()
    };
    assert_eq!(&one_big[..n], &bytes[..]);
    println!("streamed-out bytes match one-shot bytes: OK");
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
