//! **Close the positions that face an order, oldest first**: the one rule a hedging venue's close planner applies to
//! the venue's own open positions (cTrader's `positionId`s, FXCM's trades), so a close takes the same legs on every
//! venue that plans its own.
//!
//! [`take_oldest_first`] keeps the positions that face the order (`faces`: the opposite side, the order's
//! instrument), orders them oldest first (`opened_before`: the open time, ties broken by id), and takes from each in
//! turn the smaller of what is left and its size, until `want` is taken or they run out: the legs never add up to
//! more than they hold. Pure, no I/O.
//!
//! The venue facts are the parameters: what faces, how a venue's open time and id compare (centi-volume, epoch ms and
//! a numeric id on cTrader; base units, an OLE date and a string id read as a number first on FXCM) and a position's
//! size. Whether a plan that falls short of `want` is a refusal, a cap or a flip is each venue's to decide from the
//! legs it gets back.

use std::cmp::Ordering;

/// The positions of `open` that `faces` keeps, oldest first by `opened_before`, each with the size taken from it:
/// the smaller of what is left of `want` and its `size`, until `want` is taken. Empty when nothing faces the order or
/// `want` is not positive. A size is positive (a `faces` that can see an empty position leaves it out): the takes then
/// add up to `min(want, what faces)` and never overflow, each being at most what is left of `want`.
pub fn take_oldest_first<T>(
    open: &[T],
    faces: impl Fn(&T) -> bool,
    opened_before: impl Fn(&T, &T) -> Ordering,
    size: impl Fn(&T) -> i64,
    want: i64,
) -> Vec<(&T, i64)> {
    let mut facing: Vec<&T> = open.iter().filter(|p| faces(p)).collect();
    facing.sort_by(|a, b| opened_before(a, b));
    let mut remaining = want;
    let mut legs = Vec::new();
    for p in facing {
        if remaining <= 0 {
            break;
        }
        let take = remaining.min(size(p));
        legs.push((p, take));
        remaining -= take;
    }
    legs
}

#[path = "oldest_first_tests.rs"]
#[cfg(test)]
mod oldest_first_tests;
