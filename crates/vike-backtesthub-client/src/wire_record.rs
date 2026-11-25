//! `TradingRecord` ↔ the wire DTO, in the crate that owns the DTO.
//!
//! # Why this is here rather than where it was
//!
//! [`to_wire_record`] lived in `vike_studio_core::wire_run` and was correct there for as long as
//! the Studio verbs were the only thing that produced a [`TradingRecordWire`]. The NAMED RUN
//! (`docs/decisions/0064-a-named-run-carries-no-source.md`) answers with the same DTO from
//! `crates/vike-backtesthub/src/named_run.rs`, which the Studio's own runners have no business
//! reaching into, and which is deliberately resolved in a crate that cannot reach them.
//!
//! So the conversion MOVED rather than being copied, per the workspace's rule: *when two sides must
//! not disagree, the cure is a shared crate BELOW both.* A second copy would be two renderings of one
//! `TradingRecord` and the first divergence would be a Studio and a named run reporting different
//! equity curves for the same run.
//!
//! ⚠ It moved TWICE. From 2026-09-16 it was `vike_backtest::wire_record`, "in the crate that owns
//! `TradingRecord`" — which stopped being true when the analytics left the harness for
//! `vike-analytics`. On 2026-11-10 it came here, below the harness and beside the DTO, because the
//! desktop decodes every Studio answer with [`from_wire_record`] and links no harness
//! (`docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`'s amendment of that
//! day). Both halves stay together, so a field added to one is in front of whoever adds it to the
//! other. A MOVE and not a re-export shim: every call site names
//! `vike_backtesthub_client::wire_record`.

use std::collections::HashMap;

use crate::wire_studio::TradingRecordWire;

use vike_analytics::{TradingRecord, TradingRecordSimulator, TradingRecordSource};
use vike_model::OrderDrop;

/// `TradingRecord` → [`TradingRecordWire`]: copy ONLY the rendered fields (NOT the whole result — a
/// documented growing superset), so a wire answer stays stable across engine refactors.
///
/// The equity curve is the PARITY ANCHOR: a local `run_slice` and a remote one must produce a
/// bit-identical curve (the PR-3 parity gate), and a named run answers with the identical fields for
/// the same reason — one rendering, whichever verb asked.
pub fn to_wire_record(r: &TradingRecord) -> TradingRecordWire {
    // The wire's counters are a simulator's: a record no simulator produced sends zeros and no refusal.
    let none = TradingRecordSimulator::default();
    let simulator = r.simulator().unwrap_or(&none);
    TradingRecordWire {
        equity_curve: r.equity_curve.clone(),
        equity_timestamp: r.equity_timestamp.clone(),
        final_equity: r.final_equity,
        n_trades: r.n_trades,
        per_symbol_pnl: r.per_symbol_pnl.clone(),
        trades: r.trades.clone(),
        stale_deferrals: simulator.stale_deferrals,
        session_deferrals: simulator.session_deferrals,
        // The stamp is attached by the CALLER to the TOP-LEVEL answer, never here: this function
        // also renders each row of a ranked sweep, where one cost model priced every point and
        // repeating it per row would invite a reader to look for a difference that cannot exist.
        // A NAMED RUN leaves it `None` outright — 0064 sends no engine params, so nothing prices it.
        cost_model: None,
        data_warnings: r.data_warnings.clone(),
        funding_paid: r.funding_paid,
        settle_price: r.settle_price,
        refused_orders: refused_rows(&simulator.dropped),
        // Attached by the caller to the top-level answer, as the stamp is: a sweep row repeats
        // the one file every point ran.
        strategy_file: String::new(),
        strategy_sha256: String::new(),
    }
}

/// `dropped` grouped for the wire ([`TradingRecordWire::refused_orders`]): each distinct drop once, with
/// how many times the run refused it, in first-seen order. Two drops are one row only when every
/// field is the same bits, so [`from_wire_record`] gives back the same drops.
fn refused_rows(dropped: &[OrderDrop]) -> Vec<(OrderDrop, u64)> {
    let mut row_of: HashMap<(&str, &str, u64, u64), usize> = HashMap::new();
    let mut rows: Vec<(OrderDrop, u64)> = Vec::new();
    for d in dropped {
        let key = (d.symbol.as_str(), d.reason.as_str(), d.size.to_bits(), d.weight.to_bits());
        let i = *row_of.entry(key).or_insert_with(|| {
            rows.push((d.clone(), 0));
            rows.len() - 1
        });
        rows[i].1 += 1;
    }
    rows
}

/// [`TradingRecordWire`] → `TradingRecord`, the DECODE half of [`to_wire_record`] and kept beside it,
/// so a field added to one is in front of whoever adds it to the other: every rendered field comes
/// back, the rest of the (growing) result stays at its `Default`, and each trade is the
/// `vike_model::Trade` the wire carried.
///
/// ⚠ The cost-model stamp does not survive, and cannot: `TradingRecord` has no field for it. A
/// reader that needs it reads `TradingRecordWire::cost_model` before decoding.
pub fn from_wire_record(wire: TradingRecordWire) -> TradingRecord {
    let TradingRecordWire {
        equity_curve,
        equity_timestamp: equity_ts,
        final_equity,
        n_trades,
        per_symbol_pnl,
        trades,
        stale_deferrals,
        session_deferrals,
        cost_model: _,
        data_warnings,
        funding_paid,
        settle_price,
        refused_orders,
        strategy_file: _,
        strategy_sha256: _,
    } = wire;
    let dropped = expand_refused(refused_orders);
    TradingRecord {
        equity_curve,
        equity_timestamp: equity_ts,
        final_equity,
        n_trades,
        per_symbol_pnl,
        trades,
        data_warnings,
        funding_paid,
        settle_price,
        // The wire answers a backtest, so the record it decodes is the simulator's own.
        source: TradingRecordSource::Backtest(TradingRecordSimulator {
            stale_deferrals,
            session_deferrals,
            dropped,
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// The most drops [`from_wire_record`] expands the grouped refusals back into. The count beside each
/// row is the SERVER's word, and the desktop allocates one `OrderDrop` per unit of it, so an
/// unbounded count from a faulty or hostile peer would be an unbounded allocation.
const MAX_EXPANDED_REFUSALS: usize = 10_000;

/// [`TradingRecordWire::refused_orders`] back to `dropped`: each row as many drops as it counts, in
/// first-seen order, when the counts sum to at most [`MAX_EXPANDED_REFUSALS`]; above that, each row
/// ONCE (the grouping kept, the counts not expanded), so the reasons still show and nothing is
/// allocated past what the frame itself carried.
fn expand_refused(rows: Vec<(OrderDrop, u64)>) -> Vec<OrderDrop> {
    let total = rows.iter().fold(0u64, |sum, (_, n)| sum.saturating_add(*n));
    if total > MAX_EXPANDED_REFUSALS as u64 {
        return rows.into_iter().map(|(d, _)| d).collect();
    }
    rows.into_iter().flat_map(|(d, n)| std::iter::repeat_n(d, n as usize)).collect()
}

#[cfg(test)]
#[path = "wire_record_tests.rs"]
mod wire_record_tests;
