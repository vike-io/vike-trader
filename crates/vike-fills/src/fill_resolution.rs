//! Shared intrabar fill-resolution (adverse-first ordering + SL/TP bracket cap).
//!
//! Several resting orders triggered in ONE bar — OHLC can't reveal the intrabar sequence.
//! Apply ADVERSE (stop/trailing) fills before FAVOURABLE (limit) fills (pessimistic ordering).
//! When more than one order REDUCES the current position in the same bar (an SL + TP bracket),
//! cap the total reduction to the position size, adverse-first, so the profit target can't
//! also fill after the stop already flattened the position. The ambiguous bar is counted in
//! the returned `both_hit` (surfaced on the Result for honesty).

use vike_model::{OrderKind, WorkingOrder};

fn is_adverse(kind: OrderKind) -> bool {
    matches!(kind, OrderKind::Stop | OrderKind::Trailing)
}

/// `triggered` = (order, fill_price) pairs that triggered this bar; `position_size` = the
/// position BEFORE any of these fills. Returns the pairs sorted adverse-first with
/// bracket-capped sizes, plus `both_hit` (1 if the cap applied this bar).
pub fn resolve_intrabar_fills(
    mut triggered: Vec<(WorkingOrder, f64)>,
    position_size: f64,
) -> (Vec<(WorkingOrder, f64)>, u32) {
    // The sort must be STABLE (trigger order kept within each class); `sort_by_key` is.
    triggered.sort_by_key(|t| if is_adverse(t.0.kind) { 0 } else { 1 });
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
        let reducer_idx: Vec<usize> = triggered
            .iter()
            .enumerate()
            .filter(|(_, t)| t.0.side == closing_side)
            .map(|(i, _)| i)
            .collect();
        let has_stop = reducer_idx.iter().any(|&i| is_adverse(triggered[i].0.kind));
        let has_limit = reducer_idx.iter().any(|&i| !is_adverse(triggered[i].0.kind));
        if reducer_idx.len() > 1 && has_stop && has_limit {
            both_hit = 1;
            let mut remaining = pos.abs();
            for &i in &reducer_idx {
                // adverse-first (triggered is already sorted)
                let take = triggered[i].0.size.min(remaining);
                triggered[i].0.size = take; // order is consumed this bar -> safe to mutate
                remaining -= take;
            }
        }
    }
    (triggered, both_hit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wo(kind: OrderKind, side: i32, size: f64) -> WorkingOrder {
        WorkingOrder::new(kind, side, size)
    }

    fn kinds_and_sizes(out: &[(WorkingOrder, f64)]) -> Vec<(OrderKind, f64)> {
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

    /// Flat: nothing reduces, so nothing is capped or counted as a bracket hit.
    #[test]
    fn a_flat_position_caps_nothing() {
        let triggered =
            vec![(wo(OrderKind::Limit, -1, 2.0), 110.0), (wo(OrderKind::Stop, -1, 2.0), 90.0)];
        let (out, both_hit) = resolve_intrabar_fills(triggered, 0.0);
        assert_eq!(both_hit, 0);
        assert_eq!(kinds_and_sizes(&out), [(OrderKind::Stop, 2.0), (OrderKind::Limit, 2.0)]);
    }
}
