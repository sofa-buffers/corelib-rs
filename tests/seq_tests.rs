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
use std::cell::Cell;

/// The reset hook of a `u32` element: back to its default.
fn zero(x: &mut u32) {
    *x = 0;
}

/// [`zero`], counting its calls into `n`.
fn counting(n: &Cell<usize>) -> impl Fn(&mut u32) + Copy + '_ {
    move |x| {
        n.set(n.get() + 1);
        zero(x);
    }
}

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
            use super::{counting, zero};
            use sofab::seq::{self, Bound, SeqVec};
            use sofab::Error;
            use std::cell::Cell;

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

            // --- reuse: a destination holding an earlier decode ---------

            /// A destination an earlier decode left as `[90, 91, 92, 93]`.
            fn earlier() -> C {
                let mut c = C::default();
                for id in 0..4 {
                    seq::place_elem(&mut c, id, Bound::Schema(8), 90 + id).unwrap();
                }
                c
            }

            #[test]
            fn reuse_resets_existing_slots_and_truncate_drops_the_old_tail() {
                let mut c = earlier();
                let mut live = 0;
                *seq::reuse_elem(&mut c, &mut live, 0, Bound::Schema(8), zero).unwrap() += 10;
                assert_eq!(live, 1);
                // Id 1 is an elided default: slot 1 is reset as a gap, not kept as 91.
                *seq::reuse_elem(&mut c, &mut live, 2, Bound::Schema(8), zero).unwrap() += 12;
                assert_eq!(live, 3);
                SeqVec::truncate(&mut c, live);
                assert_eq!(contents(&c), [10, 0, 12]);
            }

            #[test]
            fn reuse_decodes_to_what_a_fresh_destination_holds() {
                // The same deliveries, into an empty and into a used destination.
                let ids: [(u32, u32); 4] = [(5, 15), (1, 11), (5, 25), (3, 13)];
                let mut fresh = C::default();
                for (id, v) in ids {
                    *seq::reserve_elem(&mut fresh, id, Bound::Cap(8)).unwrap() += v;
                }
                let mut used = earlier();
                let mut live = 0;
                for (id, v) in ids {
                    *seq::reuse_elem(&mut used, &mut live, id, Bound::Cap(8), zero).unwrap() += v;
                }
                SeqVec::truncate(&mut used, live);
                assert_eq!(contents(&used), contents(&fresh));
                assert_eq!(contents(&used), [0, 11, 0, 13, 0, 40]);
            }

            #[test]
            fn reuse_merges_a_reopened_element_and_resets_each_slot_once() {
                let mut c = earlier();
                let mut live = 0;
                let resets = Cell::new(0);
                let count = counting(&resets);
                *seq::reuse_elem(&mut c, &mut live, 1, Bound::Schema(8), count).unwrap() = 7;
                // Re-opened below `live`: merged, not reset.
                *seq::reuse_elem(&mut c, &mut live, 1, Bound::Schema(8), count).unwrap() += 1;
                *seq::reuse_elem(&mut c, &mut live, 0, Bound::Schema(8), count).unwrap() += 3;
                assert_eq!(resets.get(), 2, "slots 0 and 1, once each");
                assert_eq!(live, 2);
                SeqVec::truncate(&mut c, live);
                assert_eq!(contents(&c), [3, 8]);
            }

            #[test]
            fn reuse_pushes_past_the_old_length_and_resets_only_what_existed() {
                let mut c = earlier();
                let mut live = 0;
                let resets = Cell::new(0);
                let slot =
                    seq::reuse_elem(&mut c, &mut live, 6, Bound::Schema(8), counting(&resets))
                        .unwrap();
                *slot = 16;
                assert_eq!(
                    resets.get(),
                    4,
                    "the four old slots; 4..=6 were pushed fresh"
                );
                assert_eq!(live, 7);
                assert_eq!(contents(&c), [0, 0, 0, 0, 0, 0, 16]);
            }

            #[test]
            fn a_rejected_reuse_changes_neither_the_destination_nor_live() {
                let mut c = earlier();
                let mut live = 1;
                let resets = Cell::new(0);
                let count = counting(&resets);
                assert_eq!(
                    seq::reuse_elem(&mut c, &mut live, 5, Bound::Schema(5), count).map(|_| ()),
                    Err(Error::InvalidMsg)
                );
                assert_eq!(
                    seq::reuse_elem(&mut c, &mut live, 4, Bound::Cap(4), count).map(|_| ()),
                    Err(Error::LimitExceeded)
                );
                assert_eq!(
                    seq::reuse_elem(&mut c, &mut live, 0, Bound::Cap(0), count).map(|_| ()),
                    Err(Error::Argument)
                );
                assert_eq!(resets.get(), 0);
                assert_eq!(live, 1);
                assert_eq!(contents(&c), [90, 91, 92, 93]);
                // A lower id after the rejection still lands.
                *seq::reuse_elem(&mut c, &mut live, 2, Bound::Schema(5), count).unwrap() = 2;
                assert_eq!(live, 3);
            }

            #[test]
            fn reuse_leaf_replaces_a_repeated_id_instead_of_merging() {
                // §7.4: last occurrence wins for a string / blob element.
                let mut c = earlier();
                let mut live = 0;
                let resets = Cell::new(0);
                *seq::reuse_leaf(&mut c, &mut live, 1, Bound::Schema(8), counting(&resets))
                    .unwrap() += 5;
                *seq::reuse_leaf(&mut c, &mut live, 1, Bound::Schema(8), counting(&resets))
                    .unwrap() += 6;
                assert_eq!(resets.get(), 3, "gap 0, slot 1, slot 1 again");
                SeqVec::truncate(&mut c, live);
                assert_eq!(contents(&c), [0, 6]);
            }

            #[test]
            fn the_steady_state_resets_each_slot_in_order_once() {
                let mut c = earlier();
                let mut live = 0;
                let resets = Cell::new(0);
                for id in 0..6 {
                    *seq::reuse_leaf(&mut c, &mut live, id, Bound::Cap(8), counting(&resets))
                        .unwrap() += id;
                }
                assert_eq!(resets.get(), 4, "the four old slots; 4 and 5 pushed");
                assert_eq!(contents(&c), [0, 1, 2, 3, 4, 5]);
            }

            #[test]
            fn reuse_gives_the_verdicts_reserve_elem_gives() {
                let cases = [
                    (0, Bound::Schema(0)),
                    (3, Bound::Schema(3)),
                    (u32::MAX, Bound::Schema(8)),
                    (4, Bound::Cap(4)),
                    (u32::MAX, Bound::Cap(8)),
                    (0, Bound::Cap(0)),
                    (7, Bound::Cap(8)),
                    (2, Bound::Schema(8)),
                ];
                for (id, bound) in cases {
                    let want = seq::reserve_elem(&mut earlier(), id, bound).map(|_| ());
                    let mut live = 0;
                    assert_eq!(
                        seq::reuse_elem(&mut earlier(), &mut live, id, bound, zero).map(|_| ()),
                        want,
                        "reuse_elem, id {id}, {bound:?}"
                    );
                    let mut live = 0;
                    assert_eq!(
                        seq::reuse_leaf(&mut earlier(), &mut live, id, bound, zero).map(|_| ()),
                        want,
                        "reuse_leaf, id {id}, {bound:?}"
                    );
                }
            }

            #[test]
            fn truncate_shortens_and_never_grows() {
                let mut c = earlier();
                SeqVec::truncate(&mut c, 9);
                assert_eq!(contents(&c), [90, 91, 92, 93]);
                SeqVec::truncate(&mut c, 2);
                assert_eq!(contents(&c), [90, 91]);
                SeqVec::truncate(&mut c, 0);
                assert!(c.is_empty());
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
            use sofab::seq::{self, Bound, SeqVec};
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

            /// Rows an earlier decode left: `[[1], [2, 2], [3, 3, 3]]`.
            fn earlier() -> C {
                let mut c = C::default();
                for id in 0..3u32 {
                    let row = seq::reserve_row(&mut c, id, Bound::Schema(4)).unwrap();
                    for _ in 0..=id {
                        $push(row, id + 1);
                    }
                }
                c
            }

            #[test]
            fn reuse_row_empties_gap_rows_and_the_reserved_row() {
                let mut c = earlier();
                let mut live = 0;
                let row = seq::reuse_row(&mut c, &mut live, 1, Bound::Schema(4)).unwrap();
                assert!(row.is_empty());
                $push(row, 7);
                assert_eq!(live, 2);
                SeqVec::truncate(&mut c, live);
                assert_eq!(c.len(), 2);
                assert!(c[0].is_empty(), "gap row 0 no longer holds the old [1]");
                assert_eq!(&c[1][..], &[7]);
            }

            #[test]
            fn reuse_row_replaces_a_repeated_row_and_pushes_new_ones() {
                let mut c = earlier();
                let mut live = 0;
                $push(
                    seq::reuse_row(&mut c, &mut live, 0, Bound::Cap(8)).unwrap(),
                    4,
                );
                let row = seq::reuse_row(&mut c, &mut live, 0, Bound::Cap(8)).unwrap();
                assert!(row.is_empty(), "the row wrapper replaces (§7.4)");
                $push(row, 5);
                $push(
                    seq::reuse_row(&mut c, &mut live, 3, Bound::Cap(8)).unwrap(),
                    6,
                );
                SeqVec::truncate(&mut c, live);
                assert_eq!(c.len(), 4);
                assert_eq!(&c[0][..], &[5]);
                assert!(c[1].is_empty() && c[2].is_empty());
                assert_eq!(&c[3][..], &[6]);
            }

            #[test]
            fn a_rejected_reuse_row_changes_nothing() {
                let mut c = earlier();
                let mut live = 1;
                assert_eq!(
                    seq::reuse_row(&mut c, &mut live, 4, Bound::Schema(4)).map(|_| ()),
                    Err(Error::InvalidMsg)
                );
                assert_eq!(live, 1);
                assert_eq!(&c[2][..], &[3, 3, 3]);
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
    fn reused_string_elements_keep_their_heap_storage() {
        // The point of reuse_elem (#117): a steady-state decode into the same
        // destination writes into the strings already there.
        let mut c: Vec<String> = vec!["x".repeat(64), "y".repeat(64), "z".repeat(64)];
        let ptrs: Vec<*const u8> = c.iter().map(|s| s.as_ptr()).collect();
        let outer = c.as_ptr();
        let mut live = 0;
        for (id, v) in [(0u32, "a"), (2, "c")] {
            seq::reuse_leaf(&mut c, &mut live, id, Bound::Cap(8), String::clear)
                .unwrap()
                .push_str(v);
        }
        c.truncate(live);
        assert_eq!(c, ["a", "", "c"]);
        assert_eq!(c.as_ptr(), outer, "no new element storage");
        for (s, p) in c.iter().zip(ptrs) {
            assert_eq!(s.as_ptr(), p, "the string kept its buffer");
            assert!(s.capacity() >= 64);
        }
    }

    #[test]
    fn reused_struct_elements_are_reset_by_the_callers_hook() {
        let mut c = vec![
            Elem {
                a: 1,
                b: "old".into(),
            },
            Elem {
                a: 2,
                b: "old".into(),
            },
        ];
        let reset = |e: &mut Elem| {
            e.a = 0;
            e.b.clear();
        };
        let mut live = 0;
        seq::reuse_elem(&mut c, &mut live, 1, Bound::Schema(4), reset)
            .unwrap()
            .a = 7;
        seq::reuse_elem(&mut c, &mut live, 1, Bound::Schema(4), reset)
            .unwrap()
            .b
            .push('x');
        c.truncate(live);
        assert_eq!(
            c,
            [
                Elem::default(),
                Elem {
                    a: 7,
                    b: "x".into()
                }
            ]
        );
    }

    #[test]
    fn reused_rows_keep_their_heap_storage() {
        let mut rows: Vec<Vec<u32>> = vec![vec![1; 32], vec![2; 32]];
        let ptrs: Vec<*const u32> = rows.iter().map(|r| r.as_ptr()).collect();
        let mut live = 0;
        seq::reuse_row(&mut rows, &mut live, 1, Bound::Schema(2))
            .unwrap()
            .push(9);
        rows.truncate(live);
        assert_eq!(rows, [vec![], vec![9]]);
        assert_eq!(rows[0].as_ptr(), ptrs[0]);
        assert_eq!(rows[1].as_ptr(), ptrs[1]);
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
//
// Two majors of heapless, one suite: `heapless` is 0.8, `heapless09` is 0.9
// (a renamed dependency). Each feature carries its own `SeqVec` impl, so each
// gets the full set of tests; both may be on at once.

macro_rules! heapless_suites {
    ($hl:ident, $u32s:ident, $rows:ident, $specific:ident) => {
        container_suite!($u32s, $hl::Vec<u32, 8>);

        row_suite!(
            $rows,
            $hl::Vec<$hl::Vec<u32, 4>, 4>,
            |r: &mut $hl::Vec<u32, 4>, v| r.push(v).unwrap()
        );

        mod $specific {
            use super::counting;
            use sofab::seq::{self, Bound, SeqVec};
            use sofab::Error;
            use std::cell::Cell;

            #[test]
            fn a_full_container_refuses_with_argument_and_grows_by_nothing() {
                // Storage shorter than the bound it is checked against — the
                // destination is too short for the message, never a truncated array.
                let mut c: $hl::Vec<u32, 3> = $hl::Vec::new();
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
            fn a_full_container_refuses_reuse_without_resetting_anything() {
                let mut c: $hl::Vec<u32, 3> = $hl::Vec::new();
                c.push(81).unwrap();
                c.push(82).unwrap();
                let mut live = 0;
                let resets = Cell::new(0);
                for reuse in [seq::reuse_elem::<_>, seq::reuse_leaf::<_>] {
                    assert_eq!(
                        reuse(&mut c, &mut live, 3, Bound::Schema(8), counting(&resets))
                            .map(|_| ()),
                        Err(Error::Argument),
                        "the verdict reserve_elem gives a full container"
                    );
                }
                assert_eq!(resets.get(), 0, "refused before any slot was reset");
                assert_eq!(live, 0);
                assert_eq!(&c[..], &[81, 82]);
            }

            #[test]
            fn grow_to_refuses_past_capacity_up_front() {
                let mut c: $hl::Vec<u32, 2> = $hl::Vec::new();
                assert!(!c.grow_to(3));
                assert!(c.is_empty());
                assert!(c.grow_to(2));
                assert_eq!(&c[..], &[0, 0]);
            }

            #[test]
            fn fixed_string_elements_are_reserved_then_written_in_place() {
                // What generated code does with `heapless::String<N>` elements: reserve
                // the slot, then write the payload into it.
                let mut c: $hl::Vec<$hl::String<8>, 4> = $hl::Vec::new();
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
                let c: $hl::Vec<$hl::String<8>, 4> = $hl::Vec::new();
                let (id, total) = (1u32, 9usize);
                assert_eq!(seq::check_index(id, Bound::Schema(4)), Ok(()));
                assert_eq!(
                    seq::check_len(total, Bound::Schema(8)),
                    Err(Error::InvalidMsg)
                );
                assert!(c.is_empty());
            }
        }
    };
}

#[cfg(feature = "heapless")]
heapless_suites!(heapless, heapless_u32, heapless_rows, heapless_specific);

#[cfg(feature = "heapless09")]
heapless_suites!(
    heapless09,
    heapless09_u32,
    heapless09_rows,
    heapless09_specific
);

// heapless 0.9 adds a length-type parameter, `Vec<T, N, LenT = usize>`. The
// impl is generic over it, so a narrower length type is a destination too.
#[cfg(feature = "heapless09")]
container_suite!(heapless09_u8_len, heapless09::Vec<u32, 8, u8>);
