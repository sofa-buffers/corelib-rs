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
//! three calls, which differ only in what a slot holds:
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
//! * `heapless::Vec<T, N>` — behind the `heapless` feature in both crates.
//!   `grow_to` refuses up front when the target length exceeds `N`, so a refusal
//!   never leaves a partial extension either.
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
//!     fn clear(&mut self) { self.len = 0; }
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

impl Bound {
    /// The limit to compare against, and the verdict for a breach of it.
    #[inline(always)]
    fn limit(self) -> Result<(usize, Error)> {
        match self {
            Bound::Schema(n) => Ok((n, Error::InvalidMsg)),
            Bound::Cap(0) => Err(Error::Argument),
            Bound::Cap(n) => Ok((n, Error::LimitExceeded)),
        }
    }
}

/// The destination of a wrapper array: a sequence of slots that can grow.
///
/// Implemented for `Vec<T>` and, behind the `heapless` feature,
/// `heapless::Vec<T, N>`; see the [module docs](self) for which build carries
/// which. Implement it for any other container a generated message holds.
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

    /// The slot at `i`, if there is one.
    fn slot_mut(&mut self, i: usize) -> Option<&mut Self::Elem>;

    /// Remove every slot.
    fn clear(&mut self);
}

/// Check an element id against the bound on the array's index.
///
/// `Ok` when `id` is below the bound. Otherwise [`Error::InvalidMsg`] for
/// [`Bound::Schema`], [`Error::LimitExceeded`] for [`Bound::Cap`], and
/// [`Error::Argument`] for `Bound::Cap(0)`.
///
/// Use it where the decision comes before a slot exists: at a `string` / `blob`
/// element's length word, and at a matrix row's id ahead of its element count.
/// [`place_elem`], [`reserve_elem`] and [`reserve_row`] run it themselves.
#[inline]
pub fn check_index(id: Id, bound: Bound) -> Result<()> {
    let (n, breach) = bound.limit()?;
    if (id as usize) < n {
        Ok(())
    } else {
        Err(breach)
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
    let (n, breach) = bound.limit()?;
    if len <= n {
        Ok(())
    } else {
        Err(breach)
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

impl<T: Default> SeqVec for Vec<T> {
    type Elem = T;

    #[inline]
    fn len(&self) -> usize {
        Vec::len(self)
    }

    #[inline]
    fn grow_to(&mut self, len: usize) -> bool {
        self.resize_with(len, T::default);
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
    fn clear(&mut self) {
        heapless::Vec::clear(self);
    }
}
