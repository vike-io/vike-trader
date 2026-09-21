//! `BacktestResult` → the wire DTO, in the crate that OWNS `BacktestResult`.
//!
//! # Why this is here rather than where it was
//!
//! [`to_wire_result`] lived in `vike_studio_core::wire_run` and was correct there for as long as
//! the Studio verbs were the only thing that produced a [`WireRunResult`]. The NAMED RUN
//! (`docs/decisions/0064-a-named-run-carries-no-source.md`) answers with the same DTO from
//! `crate::compute_server`, which sits at layer 50 and cannot name `vike-studio-core` at 55 — that
//! is the same layer seam `compute_server`'s `StudioRunTable` exists for.
//!
//! So the conversion MOVED rather than being copied, per the workspace's rule: *when two sides must
//! not disagree, the cure is a shared crate BELOW both.* `vike_studio_core::wire_run` calls it here
//! now. A second copy would be two renderings of one `BacktestResult` and the first divergence
//! would be a Studio and a named run reporting different equity curves for the same run.
//!
//! It is a MOVE and not a re-export shim: every call site names the canonical path
//! (`vike_backtest::wire_result::to_wire_result`), which is the convention this workspace states
//! for a symbol that changes homes.

use vike_datahub_client::wire_studio::{WireRunResult, WireTrade};

use crate::BacktestResult;

/// `BacktestResult` → [`WireRunResult`]: copy ONLY the rendered fields (NOT the whole result — a
/// documented growing superset), so a wire answer stays stable across engine refactors.
///
/// The equity curve is the PARITY ANCHOR: a local `run_slice` and a remote one must produce a
/// bit-identical curve (the PR-3 parity gate), and a named run answers with the identical fields for
/// the same reason — one rendering, whichever verb asked.
pub fn to_wire_result(r: &BacktestResult) -> WireRunResult {
    WireRunResult {
        equity_curve: r.equity_curve.clone(),
        equity_ts: r.equity_ts.clone(),
        final_equity: r.final_equity,
        n_trades: r.n_trades,
        per_symbol_pnl: r.per_symbol_pnl.clone(),
        trades: r.trades.iter().map(WireTrade::from_trade).collect(),
        stale_deferrals: r.stale_deferrals,
        session_deferrals: r.session_deferrals,
        // The stamp is attached by the CALLER to the TOP-LEVEL answer, never here: this function
        // also renders each row of a ranked sweep, where one cost model priced every point and
        // repeating it per row would invite a reader to look for a difference that cannot exist.
        // A NAMED RUN leaves it `None` outright — 0064 sends no engine params, so nothing prices it.
        cost_model: None,
    }
}
