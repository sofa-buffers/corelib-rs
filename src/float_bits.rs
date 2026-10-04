//! Bit-pattern equality for float arrays — the compare behind "is this array
//! field equal to its declared default?" (MESSAGE_SPEC §2).
//!
//! A generated encoder omits a field whose value equals its default. For a
//! float that comparison must be on the **bit pattern**, not on IEEE `==`:
//! CORELIB_PLAN §4.6 says floats round-trip bit-for-bit, so an array holding
//! `-0.0` is *not* the default `[0.0, …]` and must be written. IEEE `==` says
//! `-0.0 == 0.0` and would silently drop the sign. It also says `NaN != NaN`,
//! which would make a NaN default never compare equal to itself.
//!
//! [`bits_equal`] has the same shape for every schema — the element type is a
//! type parameter, the default arrives as an argument — so it lives here once
//! instead of being emitted into every generated crate (generator#587,
//! ARCHITECTURE §8).
//!
//! It allocates nothing and mutates nothing; its one `unsafe` block is the
//! byte view behind the block compare.

/// A float element type whose raw IEEE-754 bit pattern can be compared:
/// [`f32`] (32-bit pattern) and [`f64`] (64-bit pattern).
///
/// Sealed: the two impls below are the only ones, and the set is not meant to
/// grow. The block compare in [`bits_equal`] relies on both being padding-free
/// plain-old-data.
pub trait FloatBits: Copy + sealed::Sealed {}

mod sealed {
    pub trait Sealed {}
    impl Sealed for f32 {}
    impl Sealed for f64 {}
}

impl FloatBits for f32 {}
impl FloatBits for f64 {}

/// `true` iff `a` and `b` have the same length and every pair of elements has
/// the **same IEEE-754 bit pattern** (32 bits for `f32`, 64 bits for `f64`).
///
/// There is no IEEE `==` anywhere: `+0.0` and `-0.0` differ, and a NaN equals
/// another NaN exactly when the patterns are identical, payload included. The
/// lengths are compared first and a mismatch returns at once. Nothing is
/// allocated and nothing is mutated.
///
/// Both a container and a literal work, because both borrow as a slice:
///
/// ```
/// use sofab::float_bits::bits_equal;
///
/// let field: Vec<f32> = vec![-0.0, 1.5];
///
/// // IEEE `==` says these are equal; they are not, so the field is not at its
/// // default and the encoder must write it.
/// assert!(field[..] == [0.0, 1.5][..]);
/// assert!(!bits_equal(&field[..], &[0.0, 1.5]));
///
/// assert!(bits_equal(&field, &[-0.0, 1.5]));
/// assert!(!bits_equal(&field, &[-0.0]));
/// ```
#[inline]
pub fn bits_equal<T: FloatBits>(a: &[T], b: &[T]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    // One block compare (lowers to `bcmp`/`memcmp`, which early-exits and is
    // vectorised by libc). Equal bit patterns are exactly equal bytes, whatever
    // the endianness, so no per-element `to_bits` is needed.
    //
    // SAFETY: `f32`/`f64` have no padding and every byte is initialised, so
    // viewing them as `u8` is valid; `u8` has alignment 1; the byte length
    // `len * size_of::<T>()` cannot overflow because the source slice already
    // fits in `isize::MAX` bytes; the borrows keep the memory alive and
    // unmutated for the duration of the compare.
    let (x, y) = unsafe {
        let n = core::mem::size_of_val(a);
        (
            core::slice::from_raw_parts(a.as_ptr().cast::<u8>(), n),
            core::slice::from_raw_parts(b.as_ptr().cast::<u8>(), n),
        )
    };
    x == y
}
