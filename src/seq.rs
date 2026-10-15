//! Wrapper-array slots for generated decode destinations — the **support
//! layer** beside the codec, not the codec.
//!
//! Nothing here touches the wire. These are the operations a generated visitor
//! performs *around* a [`Visitor`](crate::Visitor) callback when the scope it is
//! in is a wrapper array (MESSAGE_SPEC §5.1): check an element id or an announced
//! length against the bound that governs it, and make the slot an element lands
//! in. Every one of them has the same shape for every schema — the bound arrives
//! as an argument, the element type as a type parameter, the element default as
//! that type's [`Default`] — which is why they live here once instead of being
//! emitted, rationale and all, into every generated crate (generator#587,
//! ARCHITECTURE §8).
//!
//! `corelib-rs` and `corelib-rs-no-std` carry this module with the same API,
//! the same semantics and the same name, `sofab::seq`: they are two builds of
//! one design, and generated code moves between them unchanged.
//!
//! # Three reservations, one shape
//!
//! Every wrapper array a schema can declare reaches this module through one of
//! three calls — or, decoding into a reused destination, their `reuse_*` twins —
//! which differ only in what a slot holds:
//!
//! * [`place_elem`] puts a decoded `string` / `blob` element at the index its id
//!   names;
//! * [`reserve_elem`] makes the slot a `struct`, `union` or nested-array element
//!   is routed into, and hands it back. The helper owns the growth and the bound;
//!   the generated code keeps the routing — it records the index and sends the
//!   element's own fields there;
//! * [`reserve_row`] does the same for a matrix row and empties it, because the
//!   array wrapper *replaces* (MESSAGE_SPEC §7.4).
//!
//! [`reuse_leaf`], [`reuse_elem`] and [`reuse_row`] are the same three for a
//! destination that already holds an earlier decode; see
//! [Decoding into a reused destination](#decoding-into-a-reused-destination).
//!
//! [`check_index`] and [`check_len`] are the comparisons on their own, for the
//! places that decide before a slot exists: the length word of a `string` /
//! `blob` element (`Visitor::fixlen_begin`), and the row id and element count of
//! a matrix row.
//!
//! # The index rules
//!
//! Four rules hold for any element type, and nothing in the shared vectors can
//! tell an implementation that breaks them from one that does not — they are
//! properties of the *decoded value*, not of the bytes, which is why
//! CORELIB_PLAN §7.2 item 8 asks for them separately:
//!
//! * **Gaps are legal.** An interior element equal to its default is not
//!   written at all, so ids `0, 2, 3` are well-formed. The missing index is
//!   filled with the element default — a fresh `Default::default()` per slot, so
//!   no two struct elements ever share state — and never skipped over: every
//!   element after a gap keeps its index.
//! * **Length is highest present id + 1.** The wrapper carries no length and the
//!   last element is never elided, so growing to `id + 1` is exactly right and
//!   no trailing fill is ever needed. A schema `count` is a **capacity**, not a
//!   length: the container starts empty and the wire carries the length.
//! * **A repeated id replaces; a re-opened element merges.** Last occurrence
//!   wins per id (§7.4). [`place_elem`] overwrites the slot; [`reserve_elem`]
//!   leaves an existing slot alone, so a struct element framed twice keeps the
//!   fields its first frame set; [`reserve_row`] clears the row.
//! * **A rejected id extends nothing.** The bound is compared *before* the
//!   container grows (§6.2.1: "before the container it indexes into is
//!   extended"), so a rejection leaves it exactly as it was and a lower id
//!   delivered afterwards still lands (§7.2 item 8).
//!
//! # Two bounds, never both
//!
//! An index — and an element length, and a row's element count — carries a
//! schema bound and a receiver cap, and exactly one of the pair applies
//! (CORELIB_PLAN §6.2.1). [`Bound`] makes that a type rule: a value is one or the
//! other, never both and never neither.
//!
//! * [`Bound::Schema`] — the schema declared `count:` / `maxlen:`. A breach
//!   contradicts the schema both peers agreed on: [`Error::InvalidMsg`] (§7.1),
//!   a validity verdict.
//! * [`Bound::Cap`] — the schema declared none, so the receiver's
//!   `max_dyn_array_count` / `max_dyn_string_len` / `max_dyn_blob_len` governs.
//!   A breach is [`Error::LimitExceeded`]: policy on well-formed bytes, which
//!   decode under a looser cap (§6.3).
//!
//! **The cap is passed in, never held.** This module compares against the number
//! it is handed for one call and keeps nothing: it holds no limit of its own,
//! supplies no default, reads no value as *unlimited* and clamps to none
//! (§6.2.1). A cap of `0` states no cap at all — a declared cap is at least 1 —
//! so `Bound::Cap(0)` is a caller defect and answers [`Error::Argument`], never
//! [`Error::LimitExceeded`] against a ceiling nobody configured.
//!
//! # Decoding into a reused destination
//!
//! [`reserve_elem`] assumes the container starts empty: a slot past the
//! current length is pushed fresh, so a decode into a destination that is
//! emptied first re-allocates every element, and every `String` / `Vec` it
//! holds directly. The `reuse_*` twins keep that storage instead. They work on
//! a **live count** the caller keeps next to the container — how many slots
//! this decode has made so far:
//!
//! * a slot below `live` belongs to this decode, and is treated as the
//!   `reserve_*` twin treats an existing slot: [`reuse_elem`] hands it back as
//!   it is (a re-opened element merges), [`reuse_leaf`] and [`reuse_row`] reset
//!   it (a repeated id replaces, MESSAGE_SPEC §7.4);
//! * a slot from `live` to the id that already exists still holds an earlier
//!   decode's value, and is **reset in place** — by the caller's hook, which
//!   leaves it equal to the element default while keeping its heap capacity
//!   (`String::clear`, a struct's capacity-keeping `reset`, …), or for a row by
//!   [`SeqVec::clear`];
//! * only a slot past the container's length is pushed, as by `reserve_elem`.
//!
//! After any of them `live` is at least `id + 1`. The bound check and the gap
//! rule are those of [`reserve_elem`] — the same [`check_index`] first, the same
//! verdicts, a gap slot equal to the element default — so a reused destination
//! decodes to the value a fresh one would, **provided the caller keeps the
//! protocol**:
//!
//! 1. **Before the decode**, set every reused array's live count to `0` — part
//!    of resetting the destination, which happens before the decode and never
//!    from a callback (see the crate docs on absence). Leave the slots alone:
//!    they are the storage to reuse.
//! 2. **At each occurrence of the array's wrapper**, set its live count to `0`
//!    again, so a repeated array field replaces the earlier occurrence.
//! 3. **After the decode, whatever it returned** — `Complete`, `Incomplete` or
//!    an error — call [`SeqVec::truncate`] with each reused array's live count.
//!    An array whose wrapper never arrived (an empty array is omitted whole,
//!    MESSAGE_SPEC §2) has live `0` and empties; one with a non-empty declared
//!    default is then restored to it, as a fresh destination would hold it.
//!    Truncating only at `sequence_end` is not enough: an omitted wrapper,
//!    a cut-off input and an error never reach it, and slots at and past `live`
//!    still hold the earlier message.
//!
//! The reuse is one level deep. [`reuse_row`] empties the rows it reuses, as
//! [`reserve_row`] does, so a row's own `String`s are dropped; a reset hook that
//! empties a nested array does the same. Keeping those too needs a live count
//! per nested array, which is the caller's to keep.
//!
//! ```
//! use sofab::seq::{self, Bound, SeqVec};
//!
//! // An earlier decode left three strings.
//! let mut out = vec!["one".to_string(), "two".to_string(), "three".to_string()];
//! let mut live = 0; // 1. before the decode (and 2. at the wrapper)
//! // This message carries ids 0 and 1; id 0 was an elided default, so only
//! // id 1 arrives. Slot 0 is reset as a gap, slot 1 reset and refilled, in place.
//! let s = seq::reuse_leaf(&mut out, &mut live, 1, Bound::Cap(8), String::clear).unwrap();
//! s.push_str("b");
//! // A repeated id 1 replaces rather than appends.
//! seq::reuse_leaf(&mut out, &mut live, 1, Bound::Cap(8), String::clear).unwrap().push_str("B");
//! SeqVec::truncate(&mut out, live); // 3. after the decode
//! assert_eq!(out, ["", "B"]);
//! ```
//!
//! # The destination: [`SeqVec`]
//!
//! One trait abstracts the container, so the same functions serve a growable
//! `Vec<T>` and a fixed-capacity `heapless::Vec<T, N>` on either profile. The
//! difference between the two is exactly one thing: a fixed container can be
//! **full**. [`SeqVec::grow_to`] therefore reports failure, and a growable
//! container returns `true` unconditionally, which folds away after inlining —
//! the growable build pays nothing for the fixed one's check. A full container
//! is a destination too short for the message, the category
//! [`PayloadAcc`](crate::PayloadAcc) already uses for a payload longer than its
//! buffer: [`Error::Argument`], never a truncated array.
//!
//! Where the implementations come from:
//!
//! * `Vec<T>` — always in `corelib-rs`; behind the `alloc` feature in
//!   `corelib-rs-no-std`. Growth is `Vec`'s own amortised doubling, so a sparse
//!   array does not cost O(n²) copies (ARCHITECTURE §9.5 shape B).
//! * `heapless::Vec<T, N>` — behind the `heapless` feature (heapless 0.8) and
//!   the `heapless09` feature (heapless 0.9, any length type `LenT`) in both
//!   crates. The two majors are distinct types; enable the one the crate's own
//!   `heapless` dependency names, or both. `grow_to` refuses up front when the
//!   target length exceeds `N`, so a refusal never leaves a partial extension
//!   either.
//! * Anything else — implement [`SeqVec`] for it; the functions are generic.
//!
//! # Cost
//!
//! Every function is generic and `#[inline]`, so it is monomorphised into the
//! generated crate. With a literal bound — `Bound::Schema(5)` — the match on the
//! variant and the comparison fold exactly as the literal `id >= 5` the
//! generator used to emit did.
//!
//! # Example
//!
//! A fixed-capacity destination of four `u8` elements, implemented by the
//! caller, filled the way generated code fills a `count: 4` array:
//!
//! ```
//! use sofab::seq::{self, Bound, SeqVec};
//! use sofab::Error;
//!
//! #[derive(Default)]
//! struct Four { slots: [u8; 4], len: usize }
//!
//! impl SeqVec for Four {
//!     type Elem = u8;
//!     fn len(&self) -> usize { self.len }
//!     fn grow_to(&mut self, len: usize) -> bool {
//!         if len > self.slots.len() { return false; }
//!         for s in &mut self.slots[self.len..len] { *s = 0; }
//!         self.len = len;
//!         true
//!     }
//!     fn slot_mut(&mut self, i: usize) -> Option<&mut u8> {
//!         self.slots[..self.len].get_mut(i)
//!     }
//!     fn truncate(&mut self, len: usize) { self.len = self.len.min(len); }
//! }
//!
//! let mut out = Four::default();
//! // Elements 0 and 2 arrive; 1 was equal to its default and never written.
//! seq::place_elem(&mut out, 0, Bound::Schema(4), 7).unwrap();
//! seq::place_elem(&mut out, 2, Bound::Schema(4), 9).unwrap();
//! assert_eq!(&out.slots[..out.len], &[7, 0, 9]);
//!
//! // Id 4 is past the schema count: INVALID, and nothing grew.
//! assert_eq!(seq::place_elem(&mut out, 4, Bound::Schema(4), 1), Err(Error::InvalidMsg));
//! assert_eq!(out.len(), 3);
//!
//! // The same id against a receiver cap is a policy verdict instead.
//! assert_eq!(seq::check_index(4, Bound::Cap(4)), Err(Error::LimitExceeded));
//! ```

use crate::{Error, Id, Result};

/// The one bound that governs an element index, an element length or a row's
/// element count — the schema's, or the receiver's where the schema has none.
///
/// Exactly one applies (CORELIB_PLAN §6.2.1); see the [module docs](self).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    /// The schema's `count:` (a **capacity**) or `maxlen:`. A breach is
    /// [`Error::InvalidMsg`] (MESSAGE_SPEC §7.1).
    Schema(usize),
    /// The receiver cap for a field the schema leaves open —
    /// `max_dyn_array_count`, `max_dyn_string_len` or `max_dyn_blob_len`, as
    /// the caller configured it. A breach is [`Error::LimitExceeded`]; `Cap(0)`
    /// states no cap and is [`Error::Argument`].
    Cap(usize),
}

/// The destination of a wrapper array: a sequence of slots that can grow.
///
/// Implemented for `Vec<T>` and, behind the `heapless` / `heapless09`
/// features, `heapless::Vec<T, N>` of heapless 0.8 / 0.9; see the
/// [module docs](self) for which build carries which. Implement it for any other container a generated message holds.
pub trait SeqVec {
    /// What one slot holds.
    type Elem;

    /// The number of slots.
    fn len(&self) -> usize;

    /// Whether there are no slots.
    #[inline]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Extend to exactly `len` slots, each a fresh element default. Called only
    /// with `len` above the current length.
    ///
    /// Returns `false` when the container cannot hold `len` slots — and then it
    /// must have grown by nothing. A growable container returns `true`.
    fn grow_to(&mut self, len: usize) -> bool;

    /// The slot at `i`, if there is one. Must be `Some` for every `i` below
    /// [`len`](SeqVec::len): the reservations rely on it, and an error from
    /// them leaves the container unchanged only because it holds.
    fn slot_mut(&mut self, i: usize) -> Option<&mut Self::Elem>;

    /// Shorten to `len` slots, dropping the rest; no-op when `len` is not
    /// below the current length. A growable container keeps its capacity.
    ///
    /// Called with the live count after a decode into a reused destination, so
    /// a shorter array does not keep the tail an earlier decode left; see
    /// [Decoding into a reused destination](self#decoding-into-a-reused-destination).
    fn truncate(&mut self, len: usize);

    /// Remove every slot: `truncate(0)` unless the container has a faster way.
    #[inline]
    fn clear(&mut self) {
        self.truncate(0);
    }
}

/// Check an element id against the bound on the array's index.
///
/// `Ok` when `id` is below the bound. Otherwise [`Error::InvalidMsg`] for
/// [`Bound::Schema`], [`Error::LimitExceeded`] for [`Bound::Cap`], and
/// [`Error::Argument`] for `Bound::Cap(0)`.
///
/// Use it where the decision comes before a slot exists: at a `string` / `blob`
/// element's length word, and at a matrix row's id ahead of its element count.
/// Every reservation in this module runs it itself.
#[inline]
pub fn check_index(id: Id, bound: Bound) -> Result<()> {
    // Spelled as one flat match rather than through a shared (limit, verdict)
    // helper: the flat form is what LLVM folds BEFORE it decides whether the
    // generated caller is still small enough to inline, so a constant bound costs
    // exactly the literal compare it replaces. The helper form folded to the same
    // instructions but pushed the generated `fixlen_begin` over the inline
    // threshold -- measured +3% decode Ir on the rust-rs-no-std-dyn bench row.
    match bound {
        Bound::Schema(n) if (id as usize) >= n => Err(Error::InvalidMsg),
        Bound::Cap(0) => Err(Error::Argument),
        Bound::Cap(n) if (id as usize) >= n => Err(Error::LimitExceeded),
        _ => Ok(()),
    }
}

/// Check a length or a count the wire announces — a `string` / `blob`
/// element's byte length at its length word, a matrix row's element count at
/// its count word — against the bound on it.
///
/// The same rule as [`check_index`] with the comparison a size takes: `Ok`
/// while `len` is at most the bound. Called at the length word, it decides
/// before a payload byte is accumulated (CORELIB_PLAN §6.2.1: "at the
/// count/length header, before the allocation it is meant to prevent").
#[inline]
pub fn check_len(len: usize, bound: Bound) -> Result<()> {
    // Flat for the reason check_index gives.
    match bound {
        Bound::Schema(n) if len > n => Err(Error::InvalidMsg),
        Bound::Cap(0) => Err(Error::Argument),
        Bound::Cap(n) if len > n => Err(Error::LimitExceeded),
        _ => Ok(()),
    }
}

/// Reserve the slot element `id` lands in, growing the container to `id + 1`
/// with element defaults where it is shorter, and hand the slot back.
///
/// The bound is checked first, so a rejected id grows nothing. An existing slot
/// is returned as it is — a struct element framed a second time merges into
/// what its first frame set (MESSAGE_SPEC §7.4). The caller binds its own
/// element index and routes the element's fields; this owns only the growth
/// and the bound.
///
/// # Errors
///
/// The verdicts of [`check_index`], or [`Error::Argument`] when the container
/// cannot grow to `id + 1` (a fixed-capacity container that is full).
#[inline]
pub fn reserve_elem<C: SeqVec + ?Sized>(out: &mut C, id: Id, bound: Bound) -> Result<&mut C::Elem> {
    check_index(id, bound)?;
    let i = id as usize;
    if out.len() <= i && !out.grow_to(i + 1) {
        return Err(Error::Argument);
    }
    out.slot_mut(i).ok_or(Error::Argument)
}

/// Place a decoded leaf element — a `string` or `blob` — at the index its id
/// names, filling any gap before it with element defaults. A repeated id
/// replaces the earlier value (MESSAGE_SPEC §7.4).
///
/// # Errors
///
/// As [`reserve_elem`]; on an error `out` is unchanged.
#[inline]
pub fn place_elem<C: SeqVec + ?Sized>(
    out: &mut C,
    id: Id,
    bound: Bound,
    value: C::Elem,
) -> Result<()> {
    *reserve_elem(out, id, bound)? = value;
    Ok(())
}

/// Reserve a matrix row — an element that is itself a sequence — and empty it.
///
/// The row wrapper *replaces* (MESSAGE_SPEC §7.4), so a repeated row id starts
/// over rather than appending to what an earlier occurrence left. `bound` is
/// the bound on the row **index**; the row's own element count is the caller's
/// to check at its count word, with [`check_len`].
///
/// # Errors
///
/// As [`reserve_elem`].
#[inline]
pub fn reserve_row<C: SeqVec + ?Sized>(out: &mut C, id: Id, bound: Bound) -> Result<&mut C::Elem>
where
    C::Elem: SeqVec,
{
    let row = reserve_elem(out, id, bound)?;
    row.clear();
    Ok(row)
}

/// [`reserve_elem`] for a destination that already holds an earlier decode:
/// reuse the storage of the slots that exist instead of pushing fresh ones.
///
/// `live` is the number of slots this decode has made so far; the caller
/// zeroes it before the decode and at each occurrence of the wrapper, and
/// truncates to it after the decode — see the
/// [protocol](self#decoding-into-a-reused-destination). A slot below `live` is
/// handed back as it is, so a re-opened `struct` / `union` element merges
/// (MESSAGE_SPEC §7.4). Each existing slot from `live` to `id` is passed to
/// `reset`, which must leave it equal to the element default; it may keep the
/// slot's heap capacity, which is the point. Slots past the length are pushed
/// as element defaults. On success `live` is at least `id + 1`.
///
/// For a `string` / `blob` element, whose repeated id replaces, use
/// [`reuse_leaf`].
///
/// # Errors
///
/// As [`reserve_elem`]. The bound is checked and the container grown before
/// any slot is reset, so on an error neither `out` nor `live` has changed —
/// the earlier decode's slots are still there, and the caller truncates after
/// the decode on an error as on success.
#[inline]
pub fn reuse_elem<'a, C: SeqVec + ?Sized>(
    out: &'a mut C,
    live: &mut usize,
    id: Id,
    bound: Bound,
    reset: impl FnMut(&mut C::Elem),
) -> Result<&'a mut C::Elem> {
    reuse_slot::<C, false>(out, live, id, bound, reset)
}

/// The leaf twin of [`reuse_elem`], for a `string` / `blob` element written
/// into the slot it hands back: the slot is always reset, so a repeated id
/// replaces the earlier value (MESSAGE_SPEC §7.4), as [`place_elem`] does.
///
/// Write the payload with `push_str` / `extend_from_slice` into the slot; with
/// `String::clear` / `Vec::clear` as `reset` it keeps its buffer.
///
/// # Errors
///
/// As [`reuse_elem`].
#[inline]
pub fn reuse_leaf<'a, C: SeqVec + ?Sized>(
    out: &'a mut C,
    live: &mut usize,
    id: Id,
    bound: Bound,
    reset: impl FnMut(&mut C::Elem),
) -> Result<&'a mut C::Elem> {
    reuse_slot::<C, true>(out, live, id, bound, reset)
}

/// [`reserve_row`] for a destination that already holds an earlier decode:
/// reserve a matrix row as [`reuse_leaf`] does, with [`SeqVec::clear`] as the
/// reset, and hand it back empty.
///
/// The row reserved and every gap row an earlier decode left are emptied in
/// place, so a growable row keeps its capacity; a row past the length is
/// pushed as the row default. That is the same value only because a row's
/// default is empty — the case for every row a schema declares; a row type
/// whose `Default` is not empty does not decode to what [`reserve_row`] gives.
/// The row's own elements are not reused: see the
/// [module docs](self#decoding-into-a-reused-destination).
///
/// # Errors
///
/// As [`reuse_elem`].
#[inline]
pub fn reuse_row<'a, C: SeqVec + ?Sized>(
    out: &'a mut C,
    live: &mut usize,
    id: Id,
    bound: Bound,
) -> Result<&'a mut C::Elem>
where
    C::Elem: SeqVec,
{
    reuse_slot::<C, true>(out, live, id, bound, SeqVec::clear)
}

/// The body of the `reuse_*` functions. `REPLACE` resets a slot this decode
/// already made (a leaf or a row) instead of handing it back to merge into.
///
/// The grow-and-refuse guard is `reserve_elem`'s, repeated rather than shared:
/// reserve_elem's inlining is measured, and the verdicts are pinned equal by
/// the tests instead.
#[inline]
fn reuse_slot<'a, C: SeqVec + ?Sized, const REPLACE: bool>(
    out: &'a mut C,
    live: &mut usize,
    id: Id,
    bound: Bound,
    mut reset: impl FnMut(&mut C::Elem),
) -> Result<&'a mut C::Elem> {
    check_index(id, bound)?;
    let i = id as usize;
    let len = out.len();
    debug_assert!(
        *live <= len,
        "live count {} past the container's {} slots: not zeroed before the decode?",
        *live,
        len
    );
    // The steady state, ids in order into a slot an earlier decode left, ahead
    // of everything else. Measured on 1000 strings + rows into a reused
    // destination: -23% Ir against reaching it through the general path below,
    // at +8% on an array with a gap every 4th id (dense arrays are the common
    // case; a gap is an element equal to its default).
    if i == *live && i < len {
        let slot = out.slot_mut(i).ok_or(Error::Argument)?;
        reset(slot);
        *live = i + 1;
        return Ok(slot);
    }
    if i < *live {
        let slot = out.slot_mut(i).ok_or(Error::Argument)?;
        if REPLACE {
            reset(slot);
        }
        return Ok(slot);
    }
    // Grow before resetting, so a refused growth resets nothing. The slots
    // grow_to adds are fresh defaults; only the ones that were already there
    // hold an earlier decode.
    if len <= i && !out.grow_to(i + 1) {
        return Err(Error::Argument);
    }
    // The gap slots an earlier decode left, then the reserved one outside the
    // loop, so it is looked up once.
    for j in *live..len.min(i) {
        reset(out.slot_mut(j).ok_or(Error::Argument)?);
    }
    let slot = out.slot_mut(i).ok_or(Error::Argument)?;
    if i < len {
        reset(slot);
    }
    *live = i + 1;
    Ok(slot)
}

impl<T: Default> SeqVec for Vec<T> {
    type Elem = T;

    #[inline]
    fn len(&self) -> usize {
        Vec::len(self)
    }

    #[inline]
    fn grow_to(&mut self, len: usize) -> bool {
        // A push loop, not `resize_with`: ids arrive in order, so the common
        // growth is one slot, and resize_with's reserve-then-extend setup cost
        // more than that one push -- measured +2.6% decode Ir on the
        // rust-rs-unbounded bench row. Amortised doubling either way.
        while Vec::len(self) < len {
            self.push(T::default());
        }
        true
    }

    #[inline]
    fn slot_mut(&mut self, i: usize) -> Option<&mut T> {
        self.get_mut(i)
    }

    #[inline]
    fn clear(&mut self) {
        Vec::clear(self);
    }

    #[inline]
    fn truncate(&mut self, len: usize) {
        Vec::truncate(self, len);
    }
}

#[cfg(feature = "heapless")]
impl<T: Default, const N: usize> SeqVec for heapless::Vec<T, N> {
    type Elem = T;

    #[inline]
    fn len(&self) -> usize {
        self.as_slice().len()
    }

    #[inline]
    fn grow_to(&mut self, len: usize) -> bool {
        // Refuse up front, so a full container never grows part of the way.
        if len > N {
            return false;
        }
        while self.as_slice().len() < len {
            if self.push(T::default()).is_err() {
                return false;
            }
        }
        true
    }

    #[inline]
    fn slot_mut(&mut self, i: usize) -> Option<&mut T> {
        self.as_mut_slice().get_mut(i)
    }

    #[inline]
    fn truncate(&mut self, len: usize) {
        heapless::Vec::truncate(self, len);
    }
}

// heapless 0.9: the same impl, behind its own feature over a renamed
// dependency so it can sit next to 0.8. 0.9's `Vec` carries a length-type
// parameter (`Vec<T, N, LenT = usize>`); the impl is generic over it, so
// whichever length type the destination picked is covered.
#[cfg(feature = "heapless09")]
impl<T: Default, LenT: heapless09::LenType, const N: usize> SeqVec for heapless09::Vec<T, N, LenT> {
    type Elem = T;

    #[inline]
    fn len(&self) -> usize {
        self.as_slice().len()
    }

    #[inline]
    fn grow_to(&mut self, len: usize) -> bool {
        // Refuse up front, so a full container never grows part of the way.
        if len > N {
            return false;
        }
        while self.as_slice().len() < len {
            if self.push(T::default()).is_err() {
                return false;
            }
        }
        true
    }

    #[inline]
    fn slot_mut(&mut self, i: usize) -> Option<&mut T> {
        self.as_mut_slice().get_mut(i)
    }

    #[inline]
    fn truncate(&mut self, len: usize) {
        heapless09::Vec::<T, N, LenT>::truncate(self, len);
    }
}
