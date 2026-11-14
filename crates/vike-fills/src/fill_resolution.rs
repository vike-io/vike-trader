//! Shared intrabar fill-resolution (adverse-first ordering + SL/TP bracket cap).
//!
//! Several resting orders triggered in ONE bar — OHLC can't reveal the intrabar sequence.
//! Apply ADVERSE (stop/trailing/stop-limit/trailing-limit) fills before FAVOURABLE (limit,
//! take-profit, trailing take-profit, and the market-priced kinds: market, market-to-limit, the
//! auctions and the opening auction's market-to-limit) fills (pessimistic ordering).
//! When more than one order REDUCES the current position in the same bar (an SL + TP bracket),
//! cap the total reduction to the position size, adverse-first, so the profit target can't
//! also fill after the stop already flattened the position. The ambiguous bar is counted in
//! the returned `both_hit` (surfaced on the Result for honesty). Order-list ENTRIES are the one
//! exception (`resolve_intrabar_fills_path_first`): they take the bar's path order, and the cap
//! in it. An ALL-OR-NONE reducer (`OrderResting::all_or_none`) is never cut: one the cap cannot
//! leave whole comes back at size 0 and leaves the position to the reducers after it; it did not
//! fill, and its caller keeps it resting at its full size.

use vike_marketdata::Bar;
use vike_model::{OrderKind, OrderResting};

use crate::fill_model::path_order;

fn is_adverse(kind: OrderKind) -> bool {
    matches!(
        kind,
        OrderKind::Stop | OrderKind::Trailing | OrderKind::StopLimit | OrderKind::TrailingLimit
    )
}

/// `triggered` = (order, fill_price) pairs that triggered this bar; `position_size` = the
/// position BEFORE any of these fills. Returns the pairs sorted adverse-first with
/// bracket-capped sizes, plus `both_hit` (1 if the cap applied this bar). An all-or-none order
/// the cap would cut comes back at size 0 with its flag: the caller keeps it resting whole.
pub fn resolve_intrabar_fills(
    mut triggered: Vec<(OrderResting, f64)>,
    position_size: f64,
) -> (Vec<(OrderResting, f64)>, u32) {
    // The sort must be STABLE (trigger order kept within each class); `sort_by_key` is.
    triggered.sort_by_key(|t| if is_adverse(t.0.kind) { 0 } else { 1 });
    let both_hit = cap_reducers(&mut triggered, position_size);
    (triggered, both_hit)
}

/// [`resolve_intrabar_fills`] for a bar that holds ORDER-LIST ENTRIES (`entry[i]` says whether
/// `triggered[i]` is one: a leg of an order list with no parent that is not reduce-only, which
/// each book decides): after the adverse-first sort the entries are reordered among their own
/// slots in the order the bar's path reaches their fill prices
/// ([`crate::fill_model::path_order`]), and only THEN is the cap allocated, so the leg the path
/// reaches first gets the first claim on the position. Every other order keeps its adverse-first
/// slot; with fewer than two entries this IS [`resolve_intrabar_fills`], byte for byte. ONE rule
/// for the simulator and the paper book; a missing `entry` flag reads as no entry.
pub fn resolve_intrabar_fills_path_first(
    triggered: Vec<(OrderResting, f64)>,
    position_size: f64,
    entry: &[bool],
    bar: &Bar,
) -> (Vec<(OrderResting, f64)>, u32) {
    let mut rows: Vec<((OrderResting, f64), bool)> = triggered
        .into_iter()
        .enumerate()
        .map(|(i, t)| (t, entry.get(i).copied().unwrap_or(false)))
        .collect();
    // the same STABLE adverse-first sort, the flags riding with their orders
    rows.sort_by_key(|(t, _)| if is_adverse(t.0.kind) { 0 } else { 1 });
    let slots: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].1).collect();
    if slots.len() > 1 {
        let prices: Vec<f64> = slots.iter().map(|&i| rows[i].0.1).collect();
        let picked: Vec<_> =
            path_order(bar, &prices).iter().map(|&k| rows[slots[k]].clone()).collect();
        for (&i, row) in slots.iter().zip(picked) {
            rows[i] = row;
        }
    }
    let mut triggered: Vec<(OrderResting, f64)> = rows.into_iter().map(|(t, _)| t).collect();
    let both_hit = cap_reducers(&mut triggered, position_size);
    (triggered, both_hit)
}

/// The SL/TP cap over `triggered` IN ITS ORDER: when more than one order reduces the position and
/// a stop is among them with a limit, their sizes are cut to the position, the first taking
/// first; an all-or-none one that would be cut takes nothing (size 0) and leaves what is left to the
/// next. Returns `both_hit` (1 when it applied).
fn cap_reducers(triggered: &mut [(OrderResting, f64)], position_size: f64) -> u32 {
    let pos = position_size;
    let closing_side: i32 = if pos > 0.0 {
        -1
    } else if pos < 0.0 {
        1
    } else {
        0
    };
    let mut both_hit = 0u32;
    if closing_side != 0 {
        let reducer_index: Vec<usize> = triggered
            .iter()
            .enumerate()
            .filter(|(_, t)| t.0.side == closing_side)
            .map(|(i, _)| i)
            .collect();
        let has_stop = reducer_index.iter().any(|&i| is_adverse(triggered[i].0.kind));
        let has_limit = reducer_index.iter().any(|&i| !is_adverse(triggered[i].0.kind));
        if reducer_index.len() > 1 && has_stop && has_limit {
            both_hit = 1;
            let mut remaining = pos.abs();
            for &i in &reducer_index {
                // in `triggered`'s order: adverse-first, entries in their path order; an
                // all-or-none order fills whole or not at all
                let (size, whole) = (triggered[i].0.size, triggered[i].0.all_or_none);
                let take = if whole && size > remaining { 0.0 } else { size.min(remaining) };
                triggered[i].0.size = take; // order is consumed this bar -> safe to mutate
                remaining -= take;
            }
        }
    }
    both_hit
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wo(kind: OrderKind, side: i32, size: f64) -> OrderResting {
        OrderResting::new(kind, side, size)
    }

    fn kinds_and_sizes(out: &[(OrderResting, f64)]) -> Vec<(OrderKind, f64)> {
        out.iter().map(|t| (t.0.kind, t.0.size)).collect()
    }

    /// Stop / trailing fills sort before limit fills, keeping trigger order within each class.
    #[test]
    fn adverse_fills_sort_before_favourable_ones() {
        let triggered = vec![
            (wo(OrderKind::Limit, 1, 1.0), 101.0),
            (wo(OrderKind::Stop, 1, 2.0), 102.0),
            (wo(OrderKind::Limit, 1, 3.0), 103.0),
            (wo(OrderKind::Trailing, 1, 4.0), 104.0),
        ];
        let (out, both_hit) = resolve_intrabar_fills(triggered, 0.0);
        let prices: Vec<f64> = out.iter().map(|t| t.1).collect();
        assert_eq!(prices, [102.0, 104.0, 101.0, 103.0]);
        assert_eq!(both_hit, 0);
    }

    /// SL + TP both reducing a long in one bar: capped to the position, adverse first (the stop
    /// takes its size, the limit the remainder). An order on the position's own side is untouched.
    #[test]
    fn a_bracket_hit_in_one_bar_is_capped_to_the_position_adverse_first() {
        let triggered = vec![
            (wo(OrderKind::Limit, -1, 2.0), 110.0),
            (wo(OrderKind::Limit, 1, 5.0), 95.0),
            (wo(OrderKind::Stop, -1, 2.0), 90.0),
        ];
        let (out, both_hit) = resolve_intrabar_fills(triggered, 3.0);
        assert_eq!(both_hit, 1);
        assert_eq!(
            kinds_and_sizes(&out),
            [(OrderKind::Stop, 2.0), (OrderKind::Limit, 1.0), (OrderKind::Limit, 5.0)]
        );

        // The stop alone covers the whole position: the take-profit gets nothing.
        let triggered =
            vec![(wo(OrderKind::Limit, -1, 2.0), 110.0), (wo(OrderKind::Stop, -1, 2.0), 90.0)];
        let (out, both_hit) = resolve_intrabar_fills(triggered, 2.0);
        assert_eq!(both_hit, 1);
        assert_eq!(kinds_and_sizes(&out), [(OrderKind::Stop, 2.0), (OrderKind::Limit, 0.0)]);
    }

    /// An ALL-OR-NONE reducer the cap would cut takes nothing (size 0, its flag kept: its caller keeps
    /// it resting whole) and leaves the position to the reducer after it; one the cap leaves whole fills
    /// whole; the same order without the flag is cut as before. The path-first variant shares the cap.
    #[test]
    fn an_all_or_none_reducer_the_cap_would_cut_takes_nothing_and_leaves_the_position_on() {
        use OrderKind::{Limit, Stop};
        let aon = |size| OrderResting { all_or_none: true, ..wo(Limit, -1, size) };
        // a long of 3: the stop takes 2, the all-or-none 2 cannot fill whole, the plain limit takes 1
        let triggered =
            vec![(aon(2.0), 110.0), (wo(Limit, -1, 2.0), 111.0), (wo(Stop, -1, 2.0), 90.0)];
        let (out, both_hit) = resolve_intrabar_fills(triggered.clone(), 3.0);
        assert_eq!(both_hit, 1);
        assert_eq!(kinds_and_sizes(&out), [(Stop, 2.0), (Limit, 0.0), (Limit, 1.0)]);
        assert!(out[1].0.all_or_none && !out[2].0.all_or_none, "the held one keeps its flag");
        let bar = ohlc(100.0, 112.0, 89.0);
        let (path, _) = resolve_intrabar_fills_path_first(triggered, 3.0, &[], &bar);
        assert_eq!(path, out, "the path-first variant, no entries: the same");
        // a long of 4: 2 left after the stop, so it fills whole
        let (out, _) =
            resolve_intrabar_fills(vec![(aon(2.0), 110.0), (wo(Stop, -1, 2.0), 90.0)], 4.0);
        assert_eq!(kinds_and_sizes(&out), [(Stop, 2.0), (Limit, 2.0)]);
        // without the flag: cut to what is left, byte for byte the old answer
        let plain = vec![(wo(Limit, -1, 2.0), 110.0), (wo(Stop, -1, 2.0), 90.0)];
        let (out, _) = resolve_intrabar_fills(plain, 3.0);
        assert_eq!(kinds_and_sizes(&out), [(Stop, 2.0), (Limit, 1.0)]);
    }

    /// The cap needs a stop AND a limit reducing together: a lone reducer, or two limits, keep
    /// their full sizes.
    #[test]
    fn a_reducer_without_a_bracket_partner_is_not_capped() {
        let (out, both_hit) =
            resolve_intrabar_fills(vec![(wo(OrderKind::Stop, -1, 5.0), 90.0)], 1.0);
        assert_eq!(both_hit, 0);
        assert_eq!(kinds_and_sizes(&out), [(OrderKind::Stop, 5.0)]);

        let triggered =
            vec![(wo(OrderKind::Limit, 1, 2.0), 90.0), (wo(OrderKind::Limit, 1, 2.0), 95.0)];
        let (out, both_hit) = resolve_intrabar_fills(triggered, -1.0);
        assert_eq!(both_hit, 0);
        assert_eq!(kinds_and_sizes(&out), [(OrderKind::Limit, 2.0), (OrderKind::Limit, 2.0)]);
    }

    /// A stop-limit and a trailing-limit are adverse like a stop and a take-profit favourable like a
    /// limit, so either stop exit and a take-profit exit in one bar keep the cap, the stop first.
    #[test]
    fn a_stop_limit_is_adverse_and_a_take_profit_favourable() {
        for stop in [OrderKind::StopLimit, OrderKind::TrailingLimit] {
            let triggered =
                vec![(wo(OrderKind::TakeProfit, -1, 2.0), 110.0), (wo(stop, -1, 2.0), 90.0)];
            let (out, both_hit) = resolve_intrabar_fills(triggered, 3.0);
            assert_eq!(both_hit, 1, "{stop:?}");
            assert_eq!(kinds_and_sizes(&out), [(stop, 2.0), (OrderKind::TakeProfit, 1.0)]);
        }
    }

    /// A market-to-limit and both auction kinds fill at the market's own price, as `Market` does, so
    /// they sort with the favourable class and never ahead of a stop.
    #[test]
    fn the_market_to_limit_and_auction_kinds_are_favourable_like_a_market_order() {
        // and the trailing take-profit is a take-profit: favourable, so a stop exit beside it is first
        for kind in [
            OrderKind::Market,
            OrderKind::MarketToLimit,
            OrderKind::AuctionOpen,
            OrderKind::AuctionClose,
            OrderKind::AuctionOpenToLimit,
            OrderKind::TrailingTakeProfit,
        ] {
            assert!(!is_adverse(kind), "{kind:?}");
            let triggered = vec![(wo(kind, -1, 2.0), 110.0), (wo(OrderKind::Stop, -1, 2.0), 90.0)];
            let (out, both_hit) = resolve_intrabar_fills(triggered, 3.0);
            assert_eq!(both_hit, 1, "{kind:?}");
            assert_eq!(kinds_and_sizes(&out), [(OrderKind::Stop, 2.0), (kind, 1.0)], "{kind:?}");
        }
    }

    /// Flat: nothing reduces, so nothing is capped or counted as a bracket hit.
    #[test]
    fn a_flat_position_capabilities_nothing() {
        let triggered =
            vec![(wo(OrderKind::Limit, -1, 2.0), 110.0), (wo(OrderKind::Stop, -1, 2.0), 90.0)];
        let (out, both_hit) = resolve_intrabar_fills(triggered, 0.0);
        assert_eq!(both_hit, 0);
        assert_eq!(kinds_and_sizes(&out), [(OrderKind::Stop, 2.0), (OrderKind::Limit, 2.0)]);
    }

    fn ohlc(o: f64, h: f64, l: f64) -> Bar {
        Bar::ohlcv(0, o, h, l, o, 1.0)
    }

    /// Two order-list ENTRIES closing a long of 3 on a bar whose high is nearer: the target at 105
    /// is reached first, so it fills first AND takes the cap first (its whole 2), the stop at 95
    /// what is left (1), never the adverse-first answer (the stop 2, the target 1). When it needs
    /// the whole cap (a long of 2) it takes all of it and the stop gets nothing.
    #[test]
    fn the_entry_the_path_reaches_first_takes_the_cap_first() {
        let pair =
            || vec![(wo(OrderKind::Stop, -1, 2.0), 95.0), (wo(OrderKind::Limit, -1, 2.0), 105.0)];
        let bar = ohlc(100.0, 106.0, 93.0);
        let (out, both_hit) = resolve_intrabar_fills_path_first(pair(), 3.0, &[true, true], &bar);
        assert_eq!(both_hit, 1);
        assert_eq!(kinds_and_sizes(&out), [(OrderKind::Limit, 2.0), (OrderKind::Stop, 1.0)]);
        let (out, _) = resolve_intrabar_fills_path_first(pair(), 2.0, &[true, true], &bar);
        assert_eq!(kinds_and_sizes(&out), [(OrderKind::Limit, 2.0), (OrderKind::Stop, 0.0)]);
        // the low nearer: the stop first, which is the adverse-first answer too
        let low = ohlc(100.0, 110.0, 94.0);
        let (out, _) = resolve_intrabar_fills_path_first(pair(), 3.0, &[true, true], &low);
        assert_eq!(kinds_and_sizes(&out), [(OrderKind::Stop, 2.0), (OrderKind::Limit, 1.0)]);
    }

    /// Fewer than two entries (none, one, a missing flag) is `resolve_intrabar_fills` byte for
    /// byte, whatever the bar's path; a non-entry keeps its adverse-first slot among entries.
    #[test]
    fn without_two_entries_the_path_first_resolution_is_the_adverse_first_one() {
        let rows = || {
            vec![
                (wo(OrderKind::Limit, -1, 2.0), 105.0),
                (wo(OrderKind::Limit, 1, 5.0), 95.0),
                (wo(OrderKind::Stop, -1, 2.0), 95.0),
            ]
        };
        let bar = ohlc(100.0, 106.0, 93.0);
        let (want, want_hit) = resolve_intrabar_fills(rows(), 3.0);
        let entries: [&[bool]; 4] =
            [&[], &[false, false, false], &[true, false, false], &[false, false, true]];
        for entry in entries {
            let (out, hit) = resolve_intrabar_fills_path_first(rows(), 3.0, entry, &bar);
            assert_eq!(out, want, "{entry:?}");
            assert_eq!(hit, want_hit, "{entry:?}");
        }
        // a stop exit (no entry) keeps slot 0; the two limit entries reorder behind it by the path
        let rows = vec![
            (wo(OrderKind::Limit, 1, 1.0), 99.0),
            (wo(OrderKind::Stop, -1, 1.0), 95.0),
            (wo(OrderKind::Limit, -1, 1.0), 105.0),
        ];
        let (out, _) = resolve_intrabar_fills_path_first(rows, 0.0, &[true, false, true], &bar);
        let prices: Vec<f64> = out.iter().map(|t| t.1).collect();
        assert_eq!(prices, [95.0, 105.0, 99.0], "the stop, then the high's 105, then 99");
    }
}
