//! `sofab::seq` — the wrapper-array helpers, exercised directly.
//!
//! Every assertion here runs against the corelib's own [`SeqVec`] impls
//! (`Vec<T>` and `heapless::Vec<T, N>`), never a container the test defines:
//! a test that brings its own container only tests its own container.
//!
//! The rules pinned are MESSAGE_SPEC §5.1 (gaps, length = highest id + 1),
//! §7.1 (an id past the schema `count` is INVALID), §7.4 (a repeated id
//! replaces, a re-opened element merges, a row wrapper replaces),
//! CORELIB_PLAN §6.2.1 (a receiver cap, passed in, is LIMIT_EXCEEDED and
//! exclusive with the schema bound) and §7.2 item 8 (a rejected id extends
//! nothing, so a lower id delivered afterwards still lands).
//!
//! The same file runs in `corelib-rs` and `corelib-rs-no-std`; the container
//! suites are gated on the feature that provides the impl.

use sofab::seq::{self, Bound};
use sofab::Error;

// --- the comparisons on their own --------------------------------------------

#[test]
fn check_index_accepts_below_the_schema_count_and_rejects_at_it() {
    // `count: 4` is a capacity: indices 0..=3 are legal, 4 is not.
    assert_eq!(seq::check_index(0, Bound::Schema(4)), Ok(()));
    assert_eq!(seq::check_index(3, Bound::Schema(4)), Ok(()));
    assert_eq!(
        seq::check_index(4, Bound::Schema(4)),
        Err(Error::InvalidMsg)
    );
    assert_eq!(
        seq::check_index(u32::MAX, Bound::Schema(4)),
        Err(Error::InvalidMsg)
    );
}

#[test]
fn check_index_against_a_receiver_cap_is_limit_exceeded_not_invalid() {
    assert_eq!(seq::check_index(7, Bound::Cap(8)), Ok(()));
    assert_eq!(
        seq::check_index(8, Bound::Cap(8)),
        Err(Error::LimitExceeded)
    );
    assert_eq!(
        seq::check_index(u32::MAX, Bound::Cap(8)),
        Err(Error::LimitExceeded)
    );
}

#[test]
fn a_cap_of_zero_states_no_cap_and_is_a_caller_error() {
    // §6.2.1: no unset state and no unlimited mode — a missing cap is neither a
    // policy rejection nor a pass.
    assert_eq!(seq::check_index(0, Bound::Cap(0)), Err(Error::Argument));
    assert_eq!(seq::check_len(0, Bound::Cap(0)), Err(Error::Argument));
    // A schema count of zero is a statement about the schema, not a caller
    // error: no index fits.
    assert_eq!(
        seq::check_index(0, Bound::Schema(0)),
        Err(Error::InvalidMsg)
    );
}

#[test]
fn check_len_accepts_up_to_maxlen_and_rejects_past_it() {
    // An element `maxlen: 16`, checked at the length word.
    assert_eq!(seq::check_len(0, Bound::Schema(16)), Ok(()));
    assert_eq!(seq::check_len(16, Bound::Schema(16)), Ok(()));
    assert_eq!(
        seq::check_len(17, Bound::Schema(16)),
        Err(Error::InvalidMsg)
    );
}

#[test]
fn check_len_against_a_receiver_cap_is_limit_exceeded() {
    // A schema-unbounded string element under `max_dyn_string_len: 64`.
    assert_eq!(seq::check_len(64, Bound::Cap(64)), Ok(()));
    assert_eq!(
        seq::check_len(65, Bound::Cap(64)),
        Err(Error::LimitExceeded)
    );
}

#[test]
fn the_verdict_follows_the_bound_kind_never_the_number() {
    // Same id, same number: the schema says INVALID, the receiver says
    // LIMIT_EXCEEDED. Exactly one of the two is ever passed.
    assert_ne!(
        seq::check_index(5, Bound::Schema(5)),
        seq::check_index(5, Bound::Cap(5))
    );
    assert_ne!(
        seq::check_len(6, Bound::Schema(5)),
        seq::check_len(6, Bound::Cap(5))
    );
}

// --- one suite, every container ---------------------------------------------

/// The index rules, for any container of `u32` elements that can hold at
/// least 8 slots.
macro_rules! container_suite {
    ($name:ident, $ty:ty) => {
        mod $name {
            use sofab::seq::{self, Bound, SeqVec};
            use sofab::Error;

            type C = $ty;

            fn contents(c: &C) -> Vec<u32> {
                c.iter().copied().collect()
            }

            #[test]
            fn places_at_the_id() {
                let mut c = C::default();
                assert!(c.is_empty());
                seq::place_elem(&mut c, 0, Bound::Schema(8), 10).unwrap();
                assert_eq!(contents(&c), [10]);
                seq::place_elem(&mut c, 1, Bound::Schema(8), 11).unwrap();
                assert_eq!(contents(&c), [10, 11]);
            }

            #[test]
            fn a_gap_left_by_omitted_interior_elements_holds_the_default() {
                // Ids 1, 2 and 4 were equal to their default and never written.
                let mut c = C::default();
                seq::place_elem(&mut c, 0, Bound::Schema(8), 10).unwrap();
                seq::place_elem(&mut c, 3, Bound::Schema(8), 13).unwrap();
                seq::place_elem(&mut c, 5, Bound::Schema(8), 15).unwrap();
                assert_eq!(contents(&c), [10, 0, 0, 13, 0, 15]);
                assert_eq!(c.len(), 6, "length is highest present id + 1");
            }

            #[test]
            fn out_of_order_delivery_lands_unshifted() {
                let mut c = C::default();
                seq::place_elem(&mut c, 4, Bound::Schema(8), 14).unwrap();
                seq::place_elem(&mut c, 1, Bound::Schema(8), 11).unwrap();
                assert_eq!(contents(&c), [0, 11, 0, 0, 14]);
            }

            #[test]
            fn the_last_index_below_the_bound_is_accepted() {
                let mut c = C::default();
                seq::place_elem(&mut c, 7, Bound::Schema(8), 17).unwrap();
                assert_eq!(c.len(), 8);
                let mut c = C::default();
                seq::place_elem(&mut c, 7, Bound::Cap(8), 17).unwrap();
                assert_eq!(c.len(), 8);
            }

            #[test]
            fn an_id_at_the_schema_bound_is_invalid_and_extends_nothing() {
                let mut c = C::default();
                seq::place_elem(&mut c, 0, Bound::Schema(5), 10).unwrap();
                assert_eq!(
                    seq::place_elem(&mut c, 5, Bound::Schema(5), 15),
                    Err(Error::InvalidMsg)
                );
                assert_eq!(contents(&c), [10], "no partial extension toward id 5");
                assert_eq!(
                    seq::reserve_elem(&mut c, 6, Bound::Schema(5)).map(|_| ()),
                    Err(Error::InvalidMsg)
                );
                assert_eq!(contents(&c), [10]);
                // A lower id after the rejection still lands where it belongs.
                seq::place_elem(&mut c, 2, Bound::Schema(5), 12).unwrap();
                assert_eq!(contents(&c), [10, 0, 12]);
            }

            #[test]
            fn an_id_far_past_the_bound_costs_a_comparison_not_an_allocation() {
                let mut c = C::default();
                assert_eq!(
                    seq::place_elem(&mut c, u32::MAX, Bound::Schema(5), 1),
                    Err(Error::InvalidMsg)
                );
                assert_eq!(
                    seq::place_elem(&mut c, u32::MAX, Bound::Cap(8), 1),
                    Err(Error::LimitExceeded)
                );
                assert!(c.is_empty());
            }

            #[test]
            fn an_id_past_the_receiver_cap_is_limit_exceeded_not_invalid() {
                // A schema-unbounded array under max_dyn_array_count = 4.
                let mut c = C::default();
                seq::place_elem(&mut c, 3, Bound::Cap(4), 13).unwrap();
                assert_eq!(
                    seq::place_elem(&mut c, 4, Bound::Cap(4), 14),
                    Err(Error::LimitExceeded)
                );
                assert_eq!(c.len(), 4, "no partial extension toward the refused id");
                seq::place_elem(&mut c, 1, Bound::Cap(4), 11).unwrap();
                assert_eq!(contents(&c), [0, 11, 0, 13]);
            }

            #[test]
            fn a_cap_of_zero_is_a_caller_error_and_grows_nothing() {
                let mut c = C::default();
                assert_eq!(
                    seq::place_elem(&mut c, 0, Bound::Cap(0), 1),
                    Err(Error::Argument)
                );
                assert!(c.is_empty());
            }

            #[test]
            fn a_repeated_id_replaces() {
                let mut c = C::default();
                seq::place_elem(&mut c, 2, Bound::Schema(8), 1).unwrap();
                seq::place_elem(&mut c, 2, Bound::Schema(8), 2).unwrap();
                assert_eq!(contents(&c), [0, 0, 2]);
            }

            #[test]
            fn reserve_elem_hands_back_the_slot_and_keeps_what_is_there() {
                let mut c = C::default();
                *seq::reserve_elem(&mut c, 1, Bound::Schema(8)).unwrap() = 41;
                assert_eq!(contents(&c), [0, 41]);
                // Re-opening the element merges into it rather than resetting it.
                let slot = seq::reserve_elem(&mut c, 1, Bound::Schema(8)).unwrap();
                assert_eq!(*slot, 41);
                *slot += 1;
                assert_eq!(contents(&c), [0, 42]);
                // Reserving a lower id does not shorten or touch the rest.
                *seq::reserve_elem(&mut c, 0, Bound::Schema(8)).unwrap() = 40;
                assert_eq!(contents(&c), [40, 42]);
            }

            #[test]
            fn clear_empties_the_container() {
                let mut c = C::default();
                seq::place_elem(&mut c, 3, Bound::Schema(8), 1).unwrap();
                SeqVec::clear(&mut c);
                assert!(c.is_empty());
                seq::place_elem(&mut c, 0, Bound::Schema(8), 2).unwrap();
                assert_eq!(contents(&c), [2]);
            }
        }
    };
}

/// The row rules, for any container of rows of `u32`.
macro_rules! row_suite {
    ($name:ident, $ty:ty, $push:expr) => {
        mod $name {
            use sofab::seq::{self, Bound};
            use sofab::Error;

            type C = $ty;

            #[test]
            fn a_row_is_reserved_at_its_id_with_gaps_as_empty_rows() {
                let mut c = C::default();
                let row = seq::reserve_row(&mut c, 2, Bound::Schema(4)).unwrap();
                $push(row, 7);
                assert_eq!(c.len(), 3);
                assert!(c[0].is_empty() && c[1].is_empty());
                assert_eq!(&c[2][..], &[7]);
            }

            #[test]
            fn a_repeated_row_id_replaces_the_row() {
                let mut c = C::default();
                let row = seq::reserve_row(&mut c, 1, Bound::Schema(4)).unwrap();
                $push(row, 1);
                $push(row, 2);
                let row = seq::reserve_row(&mut c, 1, Bound::Schema(4)).unwrap();
                assert!(row.is_empty(), "the row wrapper replaces (§7.4)");
                $push(row, 3);
                assert_eq!(&c[1][..], &[3]);
            }

            #[test]
            fn a_rejected_row_id_extends_nothing() {
                let mut c = C::default();
                $push(seq::reserve_row(&mut c, 0, Bound::Schema(2)).unwrap(), 5);
                assert_eq!(
                    seq::reserve_row(&mut c, 2, Bound::Schema(2)).map(|_| ()),
                    Err(Error::InvalidMsg)
                );
                assert_eq!(
                    seq::reserve_row(&mut c, 3, Bound::Cap(3)).map(|_| ()),
                    Err(Error::LimitExceeded)
                );
                assert_eq!(c.len(), 1);
                assert_eq!(&c[0][..], &[5], "a refused row leaves the others alone");
            }
        }
    };
}

// --- Vec<T> ------------------------------------------------------------------

container_suite!(vec_u32, Vec<u32>);

row_suite!(vec_rows, Vec<Vec<u32>>, |r: &mut Vec<u32>, v| r.push(v));

mod vec_specific {
    use sofab::seq::{self, Bound};
    use sofab::Error;

    #[test]
    fn string_elements_place_owned_values_and_gaps_are_empty_strings() {
        let mut c: Vec<String> = Vec::new();
        seq::place_elem(&mut c, 0, Bound::Cap(8), "a".to_string()).unwrap();
        seq::place_elem(&mut c, 2, Bound::Cap(8), "c".to_string()).unwrap();
        assert_eq!(c, ["a", "", "c"]);
    }

    #[test]
    fn blob_elements_place_owned_values() {
        let mut c: Vec<Vec<u8>> = Vec::new();
        seq::place_elem(&mut c, 1, Bound::Schema(3), vec![1, 2]).unwrap();
        assert_eq!(c, [vec![], vec![1, 2]]);
        assert_eq!(
            seq::place_elem(&mut c, 3, Bound::Schema(3), vec![9]),
            Err(Error::InvalidMsg)
        );
        assert_eq!(c.len(), 2);
    }

    #[derive(Debug, Default, PartialEq)]
    struct Elem {
        a: u32,
        b: String,
    }

    #[test]
    fn struct_elements_get_a_fresh_default_per_slot_and_merge_when_reopened() {
        let mut c: Vec<Elem> = Vec::new();
        seq::reserve_elem(&mut c, 2, Bound::Schema(4)).unwrap().a = 7;
        seq::reserve_elem(&mut c, 2, Bound::Schema(4)).unwrap().b = "x".into();
        assert_eq!(
            c,
            [
                Elem::default(),
                Elem::default(),
                Elem {
                    a: 7,
                    b: "x".into()
                }
            ]
        );
    }

    #[test]
    fn a_nested_wrapper_row_of_strings_is_reserved_then_filled() {
        // array<array<string>>: a wrapper row inside a wrapper array.
        let mut rows: Vec<Vec<String>> = Vec::new();
        let row = seq::reserve_row(&mut rows, 1, Bound::Schema(2)).unwrap();
        seq::place_elem(row, 2, Bound::Schema(3), "z".to_string()).unwrap();
        assert_eq!(
            rows,
            [vec![], vec![String::new(), String::new(), "z".into()]]
        );
    }

    #[test]
    fn a_sparse_array_grows_geometrically_not_one_slot_at_a_time() {
        // ARCHITECTURE §9.5 shape B: growth to id + 1 on every element must not
        // cost a reallocation per element.
        let mut c: Vec<u32> = Vec::new();
        let mut reallocations = 0;
        let mut cap = c.capacity();
        for id in 0..4096u32 {
            seq::place_elem(&mut c, id, Bound::Cap(1 << 20), id).unwrap();
            if c.capacity() != cap {
                reallocations += 1;
                cap = c.capacity();
            }
        }
        assert_eq!(c.len(), 4096);
        assert!(
            reallocations <= 16,
            "{reallocations} reallocations for 4096 appended elements"
        );
    }
}

// --- heapless::Vec<T, N> -----------------------------------------------------

#[cfg(feature = "heapless")]
container_suite!(heapless_u32, heapless::Vec<u32, 8>);

#[cfg(feature = "heapless")]
row_suite!(
    heapless_rows,
    heapless::Vec<heapless::Vec<u32, 4>, 4>,
    |r: &mut heapless::Vec<u32, 4>, v| r.push(v).unwrap()
);

#[cfg(feature = "heapless")]
mod heapless_specific {
    use sofab::seq::{self, Bound, SeqVec};
    use sofab::Error;

    #[test]
    fn a_full_container_refuses_with_argument_and_grows_by_nothing() {
        // Storage shorter than the bound it is checked against — the
        // destination is too short for the message, never a truncated array.
        let mut c: heapless::Vec<u32, 3> = heapless::Vec::new();
        seq::place_elem(&mut c, 1, Bound::Schema(8), 11).unwrap();
        assert_eq!(
            seq::place_elem(&mut c, 5, Bound::Schema(8), 15),
            Err(Error::Argument)
        );
        assert_eq!(&c[..], &[0, 11], "no partial extension toward id 5");
        assert_eq!(
            seq::reserve_elem(&mut c, 3, Bound::Cap(8)).map(|_| ()),
            Err(Error::Argument)
        );
        assert_eq!(c.len(), 2);
        // The last slot still fits.
        seq::place_elem(&mut c, 2, Bound::Schema(8), 12).unwrap();
        assert_eq!(&c[..], &[0, 11, 12]);
    }

    #[test]
    fn grow_to_refuses_past_capacity_up_front() {
        let mut c: heapless::Vec<u32, 2> = heapless::Vec::new();
        assert!(!c.grow_to(3));
        assert!(c.is_empty());
        assert!(c.grow_to(2));
        assert_eq!(&c[..], &[0, 0]);
    }

    #[test]
    fn fixed_string_elements_are_reserved_then_written_in_place() {
        // What generated code does with `heapless::String<N>` elements: reserve
        // the slot, then write the payload into it.
        let mut c: heapless::Vec<heapless::String<8>, 4> = heapless::Vec::new();
        let s = seq::reserve_elem(&mut c, 2, Bound::Schema(4)).unwrap();
        s.clear();
        s.push_str("hi").unwrap();
        assert_eq!(c.len(), 3);
        assert_eq!(c[0], "");
        assert_eq!(c[2], "hi");
        assert_eq!(
            seq::reserve_elem(&mut c, 4, Bound::Schema(4)).map(|_| ()),
            Err(Error::InvalidMsg)
        );
        assert_eq!(c.len(), 3);
    }

    #[test]
    fn an_element_payload_over_maxlen_is_refused_at_the_length_word() {
        // `array<string, count: 4, maxlen: 8>`: the length word announces 9
        // bytes. The verdict comes before any slot is reserved or any byte
        // accumulated, so the container is untouched.
        let c: heapless::Vec<heapless::String<8>, 4> = heapless::Vec::new();
        let (id, total) = (1u32, 9usize);
        assert_eq!(seq::check_index(id, Bound::Schema(4)), Ok(()));
        assert_eq!(
            seq::check_len(total, Bound::Schema(8)),
            Err(Error::InvalidMsg)
        );
        assert!(c.is_empty());
    }
}
