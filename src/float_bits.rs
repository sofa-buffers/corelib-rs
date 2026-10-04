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
//! It is safe code and allocates nothing.

/// A float element type whose raw IEEE-754 bit pattern can be compared:
/// [`f32`] (32-bit pattern) and [`f64`] (64-bit pattern).
///
/// Sealed: the two impls below are the only ones, and the set is not meant to
/// grow.
pub trait FloatBits: Copy + sealed::Sealed {
    /// The unsigned integer holding the bit pattern.
    type Bits: Copy
        + Eq
        + core::ops::BitXor<Output = Self::Bits>
        + core::ops::BitOr<Output = Self::Bits>;

    /// The all-zero pattern, the identity of the XOR/OR accumulation.
    const ZERO: Self::Bits;

    /// The raw bit pattern of `self` (`f32::to_bits` / `f64::to_bits`).
    fn bits(self) -> Self::Bits;
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for f32 {}
    impl Sealed for f64 {}
}

impl FloatBits for f32 {
    type Bits = u32;
    const ZERO: u32 = 0;
    #[inline(always)]
    fn bits(self) -> u32 {
        self.to_bits()
    }
}

impl FloatBits for f64 {
    type Bits = u64;
    const ZERO: u64 = 0;
    #[inline(always)]
    fn bits(self) -> u64 {
        self.to_bits()
    }
}

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
    // One accumulated difference instead of an early exit per element: the
    // loop has no data-dependent branch, so the compiler can vectorise it.
    let mut diff = T::ZERO;
    for (x, y) in a.iter().zip(b) {
        diff = diff | (x.bits() ^ y.bits());
    }
    diff == T::ZERO
}
