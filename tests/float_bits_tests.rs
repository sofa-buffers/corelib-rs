//! `sofab::float_bits::bits_equal` — bit-pattern equality for float arrays.
//!
//! The helper backs the generated encoder's "equals its default" test for a
//! float array (MESSAGE_SPEC §2, CORELIB_PLAN §4.6): equal iff the lengths
//! match and every element has the same IEEE-754 bit pattern. No IEEE `==`
//! anywhere, so `-0.0` is not `+0.0` and a NaN is equal to a NaN only when the
//! patterns are identical.
//!
//! Every case runs for both element widths through one macro.

use sofab::float_bits::bits_equal;

/// The reference the helper is checked against: a plain bit loop.
macro_rules! reference {
    ($a:expr, $b:expr) => {{
        let (a, b) = ($a, $b);
        a.len() == b.len()
            && a.iter()
                .zip(b.iter())
                .all(|(x, y)| x.to_bits() == y.to_bits())
    }};
}

macro_rules! suite {
    ($modname:ident, $t:ty, $bits:ty, $width:expr) => {
        mod $modname {
            use super::*;

            type F = $t;
            type B = $bits;

            const SIGN: B = 1 << ($width - 1);

            /// A quiet NaN with the given payload bits.
            fn nan(payload: B) -> F {
                let quiet: B = 1 << ($width - if $width == 32 { 10 } else { 13 });
                let exp: B = if $width == 32 {
                    0xFF << 23
                } else {
                    0x7FF << 52
                };
                F::from_bits(exp | quiet | payload)
            }

            #[test]
            fn empty_arrays_are_equal() {
                let e: [F; 0] = [];
                assert!(bits_equal(&e, &e));
                assert!(bits_equal::<F>(&[], &Vec::<F>::new()));
            }

            #[test]
            fn one_element() {
                assert!(bits_equal(&[1.5 as F], &[1.5]));
                assert!(!bits_equal(&[1.5 as F], &[2.5]));
            }

            #[test]
            fn equal_arrays_and_self_comparison() {
                let a: Vec<F> = vec![0.0, 1.5, -3.25, 1e10];
                let b = a.clone();
                assert!(bits_equal(&a, &b));
                assert!(bits_equal(&a, &a));
            }

            #[test]
            fn negative_zero_differs_at_first_middle_and_last_index() {
                let def: [F; 5] = [0.0, 1.5, 0.0, 2.5, 0.0];
                for i in [0usize, 2, 4] {
                    let mut v = def;
                    v[i] = -0.0;
                    assert!(v[..] == def[..], "IEEE == is blind to the sign");
                    assert!(!bits_equal(&v, &def));
                    assert!(!bits_equal(&def, &v));
                    assert!(bits_equal(&v, &v));
                }
            }

            #[test]
            fn lone_negative_zero_against_positive_zero() {
                assert!(!bits_equal(&[-0.0 as F], &[0.0]));
                assert!(bits_equal(&[-0.0 as F], &[-0.0]));
            }

            #[test]
            fn identical_nan_patterns_are_equal() {
                let n = nan(0);
                assert!(n != n, "IEEE says a NaN is never equal to itself");
                assert!(bits_equal(&[n, 1.0], &[n, 1.0]));
                assert!(bits_equal(&[F::NAN], &[F::NAN]));
            }

            #[test]
            fn nan_with_a_different_payload_differs() {
                assert!(!bits_equal(&[nan(1)], &[nan(2)]));
                assert!(!bits_equal(&[1.0, nan(1)], &[1.0, nan(0)]));
                // Same payload, opposite sign bit.
                let neg = F::from_bits(nan(5).to_bits() | SIGN);
                assert!(!bits_equal(&[nan(5)], &[neg]));
            }

            #[test]
            fn signaling_nan_pattern_is_compared_as_bits() {
                let exp: B = if $width == 32 {
                    0xFF << 23
                } else {
                    0x7FF << 52
                };
                let snan = F::from_bits(exp | 1);
                assert!(snan.is_nan());
                assert!(bits_equal(&[snan], &[snan]));
                assert!(!bits_equal(&[snan], &[nan(0)]));
            }

            #[test]
            fn infinities() {
                assert!(bits_equal(&[F::INFINITY], &[F::INFINITY]));
                assert!(bits_equal(&[F::NEG_INFINITY], &[F::NEG_INFINITY]));
                assert!(!bits_equal(&[F::INFINITY], &[F::NEG_INFINITY]));
                assert!(!bits_equal(&[F::INFINITY], &[F::MAX]));
            }

            #[test]
            fn subnormals() {
                let tiny = F::from_bits(1);
                let big_sub = F::from_bits((1 << ($width - if $width == 32 { 9 } else { 12 })) - 1);
                assert!(tiny > 0.0 && tiny < F::MIN_POSITIVE);
                assert!(bits_equal(&[tiny, big_sub], &[tiny, big_sub]));
                assert!(!bits_equal(&[tiny], &[big_sub]));
                assert!(!bits_equal(&[tiny], &[0.0]));
                assert!(!bits_equal(&[F::from_bits(1 | SIGN)], &[tiny]));
            }

            #[test]
            fn length_mismatch_in_both_directions() {
                let a: [F; 3] = [0.0, 1.5, 2.5];
                assert!(!bits_equal(&a, &a[..2]));
                assert!(!bits_equal(&a[..2], &a));
                assert!(!bits_equal(&a, &[]));
                assert!(!bits_equal(&[], &a));
                // A shorter array that is a prefix is still unequal.
                assert!(!bits_equal(&a[..1], &a[..2]));
            }

            #[test]
            fn long_arrays_with_one_differing_element() {
                for len in [64usize, 65, 100, 257, 1000] {
                    let base: Vec<F> = (0..len).map(|i| i as F * 0.5).collect();
                    assert!(bits_equal(&base, &base.clone()));
                    for i in [0, len / 2, len - 1] {
                        let mut v = base.clone();
                        v[i] = F::from_bits(v[i].to_bits() ^ 1);
                        assert!(!bits_equal(&base, &v), "len {len} idx {i}");
                        assert!(!bits_equal(&v, &base), "len {len} idx {i}");
                    }
                    // Sign-only differences at start, middle and end.
                    let mut z = vec![0.0 as F; len];
                    let zeros = z.clone();
                    for i in [0, len / 2, len - 1] {
                        z[i] = -0.0;
                        assert!(!bits_equal(&z, &zeros), "len {len} idx {i}");
                        z[i] = 0.0;
                    }
                }
            }

            #[test]
            fn slices_vectors_and_literals_all_borrow() {
                let v: Vec<F> = vec![0.0, 1.5];
                let arr: [F; 2] = [0.0, 1.5];
                assert!(bits_equal(&v, &arr));
                assert!(bits_equal(&v[..], &[0.0, 1.5]));
                assert!(bits_equal(&arr[..], &v[..]));
            }

            #[test]
            fn pseudo_random_cross_check_against_a_reference_loop() {
                // xorshift64*, fixed seed: deterministic.
                let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
                let mut next = move || {
                    s ^= s >> 12;
                    s ^= s << 25;
                    s ^= s >> 27;
                    s.wrapping_mul(0x2545_F491_4F6C_DD1D)
                };
                // A small pool of patterns keeps equal elements frequent.
                let pool: [F; 8] = [
                    0.0,
                    -0.0,
                    1.5,
                    F::INFINITY,
                    F::NEG_INFINITY,
                    nan(1),
                    nan(2),
                    F::from_bits(1),
                ];
                let mut equal_seen = 0;
                let mut unequal_seen = 0;
                for _ in 0..4000 {
                    let len = (next() % 80) as usize;
                    let a: Vec<F> = (0..len).map(|_| pool[(next() % 8) as usize]).collect();
                    let mut b = a.clone();
                    match next() % 4 {
                        0 => {}
                        1 if len > 0 => {
                            let i = (next() % len as u64) as usize;
                            b[i] = pool[(next() % 8) as usize];
                        }
                        2 => b.push(pool[(next() % 8) as usize]),
                        _ => b = (0..len).map(|_| F::from_bits(next() as B)).collect(),
                    }
                    let got = bits_equal(&a, &b);
                    assert_eq!(got, reference!(&a, &b), "a={a:?} b={b:?}");
                    if got {
                        equal_seen += 1
                    } else {
                        unequal_seen += 1
                    }
                }
                assert!(equal_seen > 100 && unequal_seen > 100);
            }
        }
    };
}

suite!(fp32, f32, u32, 32);
suite!(fp64, f64, u64, 64);

#[test]
fn works_through_the_generated_call_shape() {
    // The call a generated encoder makes: field storage against a literal.
    let field: Vec<f32> = vec![-0.0, 1.5];
    assert!(!bits_equal(&field[..], &[0.0, 1.5]));
    let field64: Vec<f64> = vec![0.0, 1.5];
    assert!(bits_equal(&field64[..], &[0.0, 1.5]));
}

#[cfg(feature = "heapless")]
#[test]
fn a_heapless_vec_borrows_as_a_slice() {
    let mut v: heapless::Vec<f32, 4> = heapless::Vec::new();
    v.extend_from_slice(&[-0.0, 1.5]).unwrap();
    assert!(!bits_equal(&v[..], &[0.0, 1.5]));
    assert!(bits_equal(&v[..], &[-0.0, 1.5]));
}
