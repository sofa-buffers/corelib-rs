//! Trailing-default-run trimming for fixed-length arrays (MESSAGE_SPEC §3).
//!
//! A `count: N` array is **fixed-length**: it always holds exactly `N` logical
//! elements. Its canonical wire form carries only the first `M'` of them, where
//! `M'` is one past the last element that differs from the element default —
//! the trailing run of defaults is not emitted, and the decoder rebuilds it from
//! the schema count. A dynamic (count-less) array has no `N` to rebuild from and
//! is therefore never trimmed.
//!
//! The generated encode path needs this on every fixed-length array, so it lives
//! here rather than being emitted into each generated crate.

/// Returns `&a[..M']`, dropping the trailing run of elements equal to `zero`.
///
/// `M'` is one past the last element that differs from `zero`, or `0` when every
/// element equals it.
///
/// ```
/// # use sofab::trim_tail;
/// assert_eq!(trim_tail(&[7u32, 8, 0, 0], 0), &[7, 8]);
/// assert_eq!(trim_tail(&[0u32, 0], 0), &[] as &[u32]);
/// ```
#[inline]
pub fn trim_tail<T: PartialEq + Copy>(a: &[T], zero: T) -> &[T] {
    let mut n = a.len();
    while n > 0 && a[n - 1] == zero {
        n -= 1;
    }
    &a[..n]
}

/// [`trim_tail`] for `f32`, comparing by **bit pattern** rather than by `==`.
///
/// `-0.0 == 0.0` is true, so an `==` comparison would trim a trailing `-0.0` and
/// change the bytes a round-trip produces — a §4.6 bit-exactness violation. A
/// `NaN` compares unequal to everything including itself, so it is never
/// mistaken for the default either way; the bit test states that intent
/// directly.
///
/// ```
/// # use sofab::trim_tail_f32;
/// assert_eq!(trim_tail_f32(&[1.0f32, 0.0]), &[1.0]);
/// assert_eq!(trim_tail_f32(&[1.0f32, -0.0]).len(), 2); // -0.0 is not the default
/// ```
#[inline]
pub fn trim_tail_f32(a: &[f32]) -> &[f32] {
    let mut n = a.len();
    while n > 0 && f32::to_bits(a[n - 1]) == 0 {
        n -= 1;
    }
    &a[..n]
}

/// [`trim_tail`] for `f64`, comparing by bit pattern — see [`trim_tail_f32`].
///
/// ```
/// # use sofab::trim_tail_f64;
/// assert_eq!(trim_tail_f64(&[1.0f64, 0.0, 0.0]), &[1.0]);
/// assert_eq!(trim_tail_f64(&[f64::NAN, 0.0]).len(), 1);
/// ```
#[inline]
pub fn trim_tail_f64(a: &[f64]) -> &[f64] {
    let mut n = a.len();
    while n > 0 && f64::to_bits(a[n - 1]) == 0 {
        n -= 1;
    }
    &a[..n]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_only_the_trailing_run() {
        assert_eq!(trim_tail(&[1u32, 0, 2, 0, 0], 0), &[1, 0, 2]);
    }

    #[test]
    fn an_all_default_array_trims_to_empty() {
        assert_eq!(trim_tail(&[0u32; 4], 0), &[] as &[u32]);
        assert_eq!(trim_tail_f32(&[0.0f32; 3]).len(), 0);
        assert_eq!(trim_tail_f64(&[0.0f64; 3]).len(), 0);
    }

    #[test]
    fn an_empty_slice_stays_empty() {
        assert_eq!(trim_tail(&[] as &[u32], 0).len(), 0);
        assert_eq!(trim_tail_f32(&[]).len(), 0);
    }

    #[test]
    fn a_non_zero_default_is_the_trim_target() {
        // The element default is not necessarily zero.
        assert_eq!(trim_tail(&[7u8, 1, 7, 7], 7), &[7, 1]);
    }

    #[test]
    fn negative_zero_is_not_the_default() {
        // -0.0 == 0.0, so an `==` trim would drop these and change the encoded
        // bytes. The bit test keeps them (§4.6).
        assert_eq!(trim_tail_f32(&[1.0, -0.0]).len(), 2);
        assert_eq!(trim_tail_f64(&[1.0, -0.0]).len(), 2);
    }

    #[test]
    fn nan_is_never_the_default() {
        assert_eq!(trim_tail_f32(&[f32::NAN]).len(), 1);
        assert_eq!(trim_tail_f64(&[f64::NAN]).len(), 1);
    }
}
