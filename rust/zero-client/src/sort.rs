//! `Array.prototype.sort` as V8 implements it.
//!
//! Some comparators the client ports are not consistent (the AST condition
//! comparator orders subqueries before conjunctions from both sides), so the
//! result depends on the exact sequence of comparisons. V8's TimSort, for
//! arrays shorter than its minimum run of 64, finds the leading run
//! (reversing it if strictly descending) and binary-insertion-sorts the rest;
//! that is reproduced here exactly. Longer arrays use the same insertion
//! sort, which matches V8 for consistent comparators (it is stable).

use std::cmp::Ordering;

pub fn sort<T>(a: &mut [T], mut cmp: impl FnMut(&T, &T) -> Ordering) {
    let n = a.len();
    if n < 2 {
        return;
    }
    // CountAndMakeRun
    let descending = cmp(&a[1], &a[0]).is_lt();
    let mut run = 2;
    while run < n {
        let order = cmp(&a[run], &a[run - 1]);
        if if descending {
            order.is_ge()
        } else {
            order.is_lt()
        } {
            break;
        }
        run += 1;
    }
    if descending {
        a[..run].reverse();
    }
    // BinaryInsertionSort(low = 0, startArg = run, high = n)
    for start in run..n {
        let (mut left, mut right) = (0, start);
        while left < right {
            let mid = left + ((right - left) >> 1);
            if cmp(&a[start], &a[mid]).is_lt() {
                right = mid;
            } else {
                left = mid + 1;
            }
        }
        a[left..=start].rotate_right(1);
    }
}
