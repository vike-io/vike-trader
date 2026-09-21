//! PR-2a Task 2 gate: `apply_fill` snaps fills to the PIT instrument-properties grid and gates
//! opening/increasing fills below `min_qty`/`min_notional` (closing fills always execute so a
//! position is never stranded below-min). Mirrors the run-construction pattern in
//! `engine_kernel_parity.rs` (a tiny `Strategy<SimBroker>` over synthetic bars).
use std::sync::Arc;

use vike_backtest::{EngineParams, SimBroker, StrategyEngine};
use vike_model::{Bar, Strategy, SymbolProperties};

/// A properties closure returning a fixed grid for any (venue, symbol, ts).
#[allow(clippy::type_complexity)]
fn fixed(
    g: SymbolProperties,
) -> Arc<dyn Fn(&str, &str, i64) -> Option<SymbolProperties> + Send + Sync> {
    Arc::new(move |_v, _s, _t| Some(g))
}

fn mk_bars(symbol: &str, opens: &[f64]) -> Vec<Bar> {
    opens
        .iter()
        .enumerate()
        .map(|(i, &o)| Bar {
            ts: 1_700_000_000_000 + i as i64 * 60_000,
            open: o,
            high: o + 0.01,
            low: o - 0.01,
            close: o,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: Some(symbol.to_string()),
        })
        .collect()
}

/// Opens `size` at `open_at` (raw market), closes the full position at `close_at`. Fills land at
/// the FOLLOWING bar's open (pending -> fill_pending happens before the next bar's on_bar).
/// Uses the (unqualified) `symbol` key the caller registered with `StrategyEngine::new`, NOT
/// `bar.symbol` — the engine re-tags bars with `format_instrument(venue, symbol)` (e.g.
/// "SYM0.TEST"), which is a different string from the `SimBroker::symbols` index key.
struct OpenClose {
    symbol: String,
    open_at: usize,
    close_at: usize,
    side: i32,
    size: f64,
}

impl Strategy<SimBroker> for OpenClose {
    fn on_bar(&mut self, ctx: &mut SimBroker, _bar: &Bar) {
        let idx = ctx.index;
        if idx == self.open_at {
            ctx.submit(&self.symbol, self.side, self.size, 0.0, true, None);
        } else if idx == self.close_at {
            ctx.submit_close(&self.symbol);
        }
    }
}

#[allow(clippy::type_complexity)]
fn base_params(
    properties: Option<Arc<dyn Fn(&str, &str, i64) -> Option<SymbolProperties> + Send + Sync>>,
) -> EngineParams {
    EngineParams {
        cash: 1_000_000.0,
        default_venue: Some("TEST".into()),
        properties,
        ..Default::default()
    }
}

#[test]
fn rounding_snaps_price_and_size() {
    let sym = "SYM0";
    // idx0 bar is the "now" bar when the open order is submitted; idx1's open is the open-fill
    // price; idx2's open is the close-fill price.
    let bars = mk_bars(sym, &[100.0, 100.23, 105.0]);
    let series = vec![(sym.to_string(), bars)];
    let grid = SymbolProperties { tick_size: 0.5, step_size: 0.1, ..Default::default() };
    let strat = OpenClose { symbol: sym.to_string(), open_at: 0, close_at: 1, side: 1, size: 1.07 };
    let mut e = StrategyEngine::new(series, strat, base_params(Some(fixed(grid))));
    let r = e.run();

    assert_eq!(r.trades.len(), 1, "expected exactly one closed trade");
    let t = &r.trades[0];
    // 100.23 / 0.5 = 200.46 -> round-half-even -> 200 -> *0.5 = 100.0
    assert!((t.entry_price - 100.0).abs() < 1e-9, "entry_price={}", t.entry_price);
    // 105.0 is already on the 0.5 grid
    assert!((t.exit_price - 105.0).abs() < 1e-9, "exit_price={}", t.exit_price);
    // 1.07 / 0.1 = 10.7 -> round-half-even -> 11 -> *0.1 = 1.1
    assert!((t.size - 1.1).abs() < 1e-9, "size={}", t.size);
}

#[test]
fn opening_below_min_notional_rejected() {
    let sym = "SYM0";
    let bars = mk_bars(sym, &[100.0, 100.0, 100.0]);
    let series = vec![(sym.to_string(), bars)];
    let grid = SymbolProperties { min_notional: 1e9, ..Default::default() };
    let strat = OpenClose { symbol: sym.to_string(), open_at: 0, close_at: 1, side: 1, size: 1.0 };
    let mut e = StrategyEngine::new(series, strat, base_params(Some(fixed(grid))));
    let r = e.run();

    assert_eq!(r.trades.len(), 0, "opening fill below min_notional must not open a position");
    assert_eq!(e.core.sym[0].pos.size, 0.0);
    assert!(
        e.core.dropped.iter().any(|(_, reason, _, _)| reason == "min_notional"),
        "expected a dropped entry with reason=min_notional, got {:?}",
        e.core.dropped
    );
}

#[test]
fn closing_below_min_notional_still_executes() {
    let sym = "SYM0";
    // idx1 open = 5.0 -> open-fill notional = 1000 * 5.0 = 5000 (>= min_notional, opens fine).
    // idx2 open = 0.5 -> close-fill notional = 1000 * 0.5 = 500 (< min_notional) but this is a
    // CLOSING fill, so it must execute anyway (anti-stranding).
    let bars = mk_bars(sym, &[5.0, 5.0, 0.5]);
    let series = vec![(sym.to_string(), bars)];
    let grid = SymbolProperties { min_notional: 1000.0, ..Default::default() };
    let strat =
        OpenClose { symbol: sym.to_string(), open_at: 0, close_at: 1, side: 1, size: 1000.0 };
    let mut e = StrategyEngine::new(series, strat, base_params(Some(fixed(grid))));
    let r = e.run();

    assert_eq!(r.trades.len(), 1, "closing fill below min_notional must still execute");
    let t = &r.trades[0];
    assert!((t.exit_price - 0.5).abs() < 1e-9, "exit_price={}", t.exit_price);
    assert_eq!(e.core.sym[0].pos.size, 0.0, "position must be fully closed");
}

#[test]
fn min_qty_rejects_opening() {
    let sym = "SYM0";
    let bars = mk_bars(sym, &[100.0, 100.0, 100.0]);
    let series = vec![(sym.to_string(), bars)];
    let grid = SymbolProperties { min_qty: 1e9, ..Default::default() };
    let strat = OpenClose { symbol: sym.to_string(), open_at: 0, close_at: 1, side: 1, size: 1.0 };
    let mut e = StrategyEngine::new(series, strat, base_params(Some(fixed(grid))));
    let r = e.run();

    assert_eq!(r.trades.len(), 0, "opening fill below min_qty must not open a position");
    assert_eq!(e.core.sym[0].pos.size, 0.0);
    assert!(
        e.core.dropped.iter().any(|(_, reason, _, _)| reason == "min_qty"),
        "expected a dropped entry with reason=min_qty, got {:?}",
        e.core.dropped
    );
}

#[test]
fn closing_dust_after_step_widens_still_closes() {
    // The PIT step_size can WIDEN over a position's life: open on a fine grid, close on a coarse
    // one. A full close whose remainder step-rounds to 0.0 must STILL execute (never strand a
    // position / defeat a force-close), by falling back to the raw size.
    let sym = "SYM0";
    let bars = mk_bars(sym, &[10.0, 10.0, 10.0]);
    let series = vec![(sym.to_string(), bars)];
    // bar[i].ts = 1_700_000_000_000 + i*60_000. Fine step 0.001 up to the open-fill bar (idx1),
    // coarse step 0.01 from the close-fill bar (idx2) on — so round(0.005, 0.01) = 0.0.
    let close_ts = 1_700_000_000_000 + 2 * 60_000;
    #[allow(clippy::type_complexity)]
    let grid_src: Arc<dyn Fn(&str, &str, i64) -> Option<SymbolProperties> + Send + Sync> =
        Arc::new(move |_v, _s, ts| {
            let step = if ts >= close_ts { 0.01 } else { 0.001 };
            Some(SymbolProperties { step_size: step, ..Default::default() })
        });
    let strat =
        OpenClose { symbol: sym.to_string(), open_at: 0, close_at: 1, side: 1, size: 0.005 };
    let mut e = StrategyEngine::new(series, strat, base_params(Some(grid_src)));
    let r = e.run();

    assert_eq!(r.trades.len(), 1, "closing dust (rounded to 0 by a widened step) must still close");
    assert_eq!(e.core.sym[0].pos.size, 0.0, "position must be fully flat, not stranded");
}

#[test]
fn no_properties_is_byte_identical() {
    let sym = "SYM0";
    let bars = mk_bars(sym, &[100.0, 100.23, 105.0]);

    let strat_none =
        OpenClose { symbol: sym.to_string(), open_at: 0, close_at: 1, side: 1, size: 1.07 };
    let mut e_none =
        StrategyEngine::new(vec![(sym.to_string(), bars.clone())], strat_none, base_params(None));
    let r_none = e_none.run();

    // An all-0.0 grid is the "unconstrained" sentinel (nz(0.0) -> None, gates never trip) — the
    // same fill path must produce byte-identical results as properties: None.
    let strat_grid =
        OpenClose { symbol: sym.to_string(), open_at: 0, close_at: 1, side: 1, size: 1.07 };
    let mut e_grid = StrategyEngine::new(
        vec![(sym.to_string(), bars)],
        strat_grid,
        base_params(Some(fixed(SymbolProperties::default()))),
    );
    let r_grid = e_grid.run();

    assert_eq!(r_none.trades, r_grid.trades);
    assert_eq!(r_none.final_equity, r_grid.final_equity);
}
