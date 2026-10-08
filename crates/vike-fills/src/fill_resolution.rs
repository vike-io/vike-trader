//! Shared intrabar fill-resolution (adverse-first ordering + SL/TP bracket cap).
//! Exact port of `core/fill_resolution.py::resolve_intrabar_fills`.
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
    // Python `sorted(key=0/1)` is stable — sort_by_key is stable too.
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
