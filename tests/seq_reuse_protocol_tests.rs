//! `sofab::seq`'s reuse protocol, end to end: a visitor that decodes a
//! `array<string>` field into a reused destination with [`seq::reuse_leaf`]
//! must hold, after every decode, exactly what a fresh destination holds —
//! including when the new message omits the array, repeats an element id,
//! is cut off, or fails (corelib-rs#117, generator#755).

use sofab::seq::{self, Bound, SeqVec};
use sofab::{decode, Id, OStream, Result, Status, Unsigned, Visitor};

/// `message { n: u64 = 1; tags: array<string> = 4; }`.
#[derive(Debug, Default)]
struct Msg {
    n: Unsigned,
    tags: Vec<String>,
    tags_live: usize,
    depth: usize,
    in_tags: bool,
    slot: Option<usize>,
}

impl Msg {
    /// The generated `try_decode(data, &mut out)`: reset before the decode
    /// (step 1), truncate after it whatever it returned (step 3).
    fn decode_into(&mut self, buf: &[u8]) -> Result<Status> {
        self.n = 0;
        self.tags_live = 0;
        self.depth = 0;
        self.in_tags = false;
        self.slot = None;
        let r = decode(buf, self);
        SeqVec::truncate(&mut self.tags, self.tags_live);
        r
    }

    fn fresh(buf: &[u8]) -> (Result<Status>, Self) {
        let mut m = Msg::default();
        let r = m.decode_into(buf);
        (r, m)
    }
}

impl Visitor for Msg {
    fn unsigned(&mut self, id: Id, value: Unsigned) {
        if self.depth == 0 && id == 1 {
            self.n = value;
        }
    }

    fn sequence_begin(&mut self, id: Id) {
        self.depth += 1;
        if self.depth == 1 && id == 4 {
            // Step 2: a repeated array field replaces the earlier occurrence.
            self.in_tags = true;
            self.tags_live = 0;
        }
    }

    fn sequence_end(&mut self) {
        if self.depth == 1 {
            self.in_tags = false;
        }
        self.depth -= 1;
    }

    fn string(&mut self, id: Id, _total: usize, offset: usize, chunk: &[u8]) {
        if !(self.in_tags && self.depth == 1) {
            return;
        }
        if offset == 0 {
            let s = seq::reuse_leaf(
                &mut self.tags,
                &mut self.tags_live,
                id,
                Bound::Cap(16),
                String::clear,
            )
            .expect("id within the cap");
            self.slot = Some(id as usize);
            s.push_str(std::str::from_utf8(chunk).unwrap());
        } else if let Some(i) = self.slot {
            self.tags[i].push_str(std::str::from_utf8(chunk).unwrap());
        }
    }
}

fn encode(n: Unsigned, tags: Option<&[(Id, &str)]>) -> Vec<u8> {
    let mut buf = [0u8; 512];
    let mut os = OStream::new(&mut buf);
    if n != 0 {
        os.write_unsigned(1, n).unwrap();
    }
    if let Some(tags) = tags {
        os.write_sequence_begin_lazy(4).unwrap();
        for &(id, s) in tags {
            os.write_str(id, s).unwrap();
        }
        os.write_sequence_end_keep().unwrap();
    }
    let used = os.bytes_used();
    buf[..used].to_vec()
}

/// A destination an earlier, longer message left behind.
fn used() -> Msg {
    let mut m = Msg::default();
    let a = encode(7, Some(&[(0, "xx"), (1, "yy"), (2, "zz"), (3, "ww")]));
    assert_eq!(m.decode_into(&a), Ok(Status::Complete));
    assert_eq!(m.tags, ["xx", "yy", "zz", "ww"]);
    m
}

fn assert_like_fresh(buf: &[u8]) -> Msg {
    let (want_r, want) = Msg::fresh(buf);
    let mut m = used();
    let r = m.decode_into(buf);
    assert_eq!(r, want_r, "same verdict");
    assert_eq!((m.n, &m.tags), (want.n, &want.tags), "same value");
    m
}

#[test]
fn a_shorter_array_drops_the_earlier_tail() {
    let m = assert_like_fresh(&encode(1, Some(&[(0, "a"), (2, "c")])));
    assert_eq!(m.tags, ["a", "", "c"]);
}

#[test]
fn an_omitted_array_empties_the_reused_one() {
    // An empty array is omitted whole: no sequence_begin, no sequence_end.
    let m = assert_like_fresh(&encode(5, None));
    assert!(m.tags.is_empty());
    // The all-default message is the empty byte string.
    assert!(assert_like_fresh(&[]).tags.is_empty());
}

#[test]
fn a_repeated_element_id_replaces_rather_than_appends() {
    let m = assert_like_fresh(&encode(0, Some(&[(0, "first"), (0, "last")])));
    assert_eq!(m.tags, ["last"]);
}

#[test]
fn a_cut_off_message_keeps_no_earlier_tail() {
    let full = encode(1, Some(&[(0, "p"), (1, "q"), (2, "r")]));
    for cut in 0..full.len() {
        assert_like_fresh(&full[..cut]);
    }
}

#[test]
fn a_failed_decode_keeps_no_earlier_message() {
    // An overlong varint before the array: an error, and the array never began.
    let mut bad = encode(3, None);
    bad.extend_from_slice(&[0xFF; 11]);
    let (r, _) = Msg::fresh(&bad);
    assert!(r.is_err(), "the fixture must fail: {r:?}");
    let m = assert_like_fresh(&bad);
    assert!(m.tags.is_empty());
    // And one that fails after a first element arrived.
    let mut bad = encode(3, Some(&[(0, "a")]));
    bad.truncate(bad.len() - 1); // drop the wrapper's end
    bad.extend_from_slice(&[0xFF; 11]);
    assert_like_fresh(&bad);
}

#[test]
fn reused_strings_keep_their_buffers() {
    let mut m = used();
    let ptrs: Vec<*const u8> = m.tags.iter().map(|s| s.as_ptr()).collect();
    assert_eq!(
        m.decode_into(&encode(1, Some(&[(0, "a"), (1, "b")]))),
        Ok(Status::Complete)
    );
    assert_eq!(m.tags, ["a", "b"]);
    assert_eq!(m.tags[0].as_ptr(), ptrs[0]);
    assert_eq!(m.tags[1].as_ptr(), ptrs[1]);
}
