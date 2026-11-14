//! PR-2a Task 2 gate: `apply_fill` snaps fills to the PIT instrument-properties grid and gates
//! opening/increasing fills below `min_qty`/`min_notional` (closing fills always execute so a
//! position is never stranded below-min). Mirrors the run-construction pattern in
//! `engine_kernel_parity.rs` (a tiny `Strategy<OrderBrokerSimulator>` over synthetic bars).
use std::sync::Arc;

use vike_marketdata::Bar;
use vike_model::{ContractValue, OrderDrop, Strategy, SymbolProperties};
use vike_simulator::{OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig};

/// A properties closure returning a fixed grid for any (venue, symbol, ts).
#[expect(clippy::type_complexity)]
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
            timestamp: 1_700_000_000_000 + i as i64 * 60_000,
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
/// "SYM0.TEST"), which is a different string from the `OrderBrokerSimulator::symbols` index key.
/// `funding_source` holds its long with it too.
pub(crate) struct OpenClose {
    pub(crate) symbol: String,
    pub(crate) open_at: usize,
    pub(crate) close_at: usize,
    pub(crate) side: i32,
    pub(crate) size: f64,
}

impl Strategy<OrderBrokerSimulator> for OpenClose {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, _bar: &Bar) {
        let index = ctx.index;
        if index == self.open_at {
            ctx.submit(&self.symbol, self.side, self.size, 0.0, true, None);
        } else if index == self.close_at {
            ctx.submit_close(&self.symbol);
        }
    }
}

#[expect(clippy::type_complexity)]
fn base_params(
    properties: Option<Arc<dyn Fn(&str, &str, i64) -> Option<SymbolProperties> + Send + Sync>>,
) -> StrategyEngineConfig {
    StrategyEngineConfig {
        cash: 1_000_000.0,
        default_venue: Some("TEST".into()),
        properties,
        ..Default::default()
    }
}

#[test]
fn rounding_snaps_price_and_size() {
    let symbol_name = "SYM0";
    // idx0 bar is the "now" bar when the open order is submitted; idx1's open is the open-fill
    // price; index2's open is the close-fill price.
    let bars = mk_bars(symbol_name, &[100.0, 100.23, 105.0]);
    let series = vec![(symbol_name.to_string(), bars)];
    let grid = SymbolProperties { tick_size: 0.5, step_size: 0.1, ..Default::default() };
    let strat =
        OpenClose { symbol: symbol_name.to_string(), open_at: 0, close_at: 1, side: 1, size: 1.07 };
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

/// A size-1 opening order at 100 under `grid` never opens: no trade, a flat position, and a dropped
/// entry whose reason is `reason`.
fn assert_opening_dropped(grid: SymbolProperties, reason: &str) {
    let symbol_name = "SYM0";
    let bars = mk_bars(symbol_name, &[100.0, 100.0, 100.0]);
    let series = vec![(symbol_name.to_string(), bars)];
    let strat =
        OpenClose { symbol: symbol_name.to_string(), open_at: 0, close_at: 1, side: 1, size: 1.0 };
    let mut e = StrategyEngine::new(series, strat, base_params(Some(fixed(grid))));
    let r = e.run();

    assert_eq!(r.trades.len(), 0, "opening fill below {reason} must not open a position");
    assert_eq!(e.core.symbol[0].pos.size, 0.0);
    assert!(
        e.core.dropped.iter().any(|d| d.reason == reason),
        "expected a dropped entry with reason={reason}, got {:?}",
        e.core.dropped
    );
}

#[test]
fn opening_below_min_notional_rejected() {
    assert_opening_dropped(
        SymbolProperties { min_notional: 1e9, ..Default::default() },
        "min_notional",
    );
}

#[test]
fn closing_below_min_notional_still_executes() {
    let symbol_name = "SYM0";
    // idx1 open = 5.0 -> open-fill notional = 1000 * 5.0 = 5000 (>= min_notional, opens fine).
    // index2 open = 0.5 -> close-fill notional = 1000 * 0.5 = 500 (< min_notional) but this is a
    // CLOSING fill, so it must execute anyway (anti-stranding).
    let bars = mk_bars(symbol_name, &[5.0, 5.0, 0.5]);
    let series = vec![(symbol_name.to_string(), bars)];
    let grid = SymbolProperties { min_notional: 1000.0, ..Default::default() };
    let strat = OpenClose {
        symbol: symbol_name.to_string(),
        open_at: 0,
        close_at: 1,
        side: 1,
        size: 1000.0,
    };
    let mut e = StrategyEngine::new(series, strat, base_params(Some(fixed(grid))));
    let r = e.run();

    assert_eq!(r.trades.len(), 1, "closing fill below min_notional must still execute");
    let t = &r.trades[0];
    assert!((t.exit_price - 0.5).abs() < 1e-9, "exit_price={}", t.exit_price);
    assert_eq!(e.core.symbol[0].pos.size, 0.0, "position must be fully closed");
}

/// A symbol LISTED in `StrategyEngineConfig::contract_values` at a linear value books bit for bit what it
/// books UNLISTED at that multiplier, the min-notional floor and the margin gate included: the engine
/// keeps one factor per symbol, its `ContractValue`. One unit at 100 is 100 at multiplier 1 and 1,000
/// at 10, so a floor of 500 tells the two factors apart.
#[test]
fn a_listed_linear_value_books_bit_for_bit_as_its_multiplier() {
    let symbol_name = "SYM0";
    let grid = SymbolProperties { min_notional: 500.0, ..Default::default() };
    let run = |p: StrategyEngineConfig| {
        let series =
            vec![(symbol_name.to_string(), mk_bars(symbol_name, &[100.0, 100.0, 103.0, 101.0]))];
        let strat = OpenClose {
            symbol: symbol_name.to_string(),
            open_at: 0,
            close_at: 2,
            side: 1,
            size: 1.0,
        };
        let mut e = StrategyEngine::new(series, strat, p);
        // Debug prints every f64 in its shortest round-trip form: equal text is equal bits.
        format!("{:?}", e.run())
    };
    for leverage in [None, Some(3.0)] {
        let base = || StrategyEngineConfig { leverage, ..base_params(Some(fixed(grid))) };
        let ten = || vec![(symbol_name.to_string(), 10.0)];
        let listed = || vec![(symbol_name.to_string(), ContractValue::linear(10.0))];
        let unlisted = run(StrategyEngineConfig { multipliers: ten(), ..base() });
        assert!(unlisted.contains("trades: [Trade"), "the floor passes at 10: {unlisted}");
        let only_listed = run(StrategyEngineConfig { contract_values: listed(), ..base() });
        assert_eq!(only_listed, unlisted, "listed, multiplier left at 1 ({leverage:?})");
        let both =
            run(StrategyEngineConfig { multipliers: ten(), contract_values: listed(), ..base() });
        assert_eq!(both, unlisted, "listed and multiplied ({leverage:?})");
    }
}

#[test]
fn min_quantity_rejects_opening() {
    assert_opening_dropped(SymbolProperties { min_qty: 1e9, ..Default::default() }, "min_qty");
}

/// A grid refusal records the order's sizer weight in `dropped`, never its fill price.
#[test]
fn grid_refusal_records_the_order_weight() {
    struct Weighted;
    impl Strategy<OrderBrokerSimulator> for Weighted {
        fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, _bar: &Bar) {
            if ctx.index == 0 {
                ctx.submit("SYM0", 1, 1.0, 0.25, true, None);
            }
        }
    }
    let series = vec![("SYM0".to_string(), mk_bars("SYM0", &[100.0, 100.0]))];
    let grid = SymbolProperties { min_qty: 1e9, ..Default::default() };
    let mut e = StrategyEngine::new(series, Weighted, base_params(Some(fixed(grid))));
    e.run();
    let refused: Vec<_> = e.core.dropped.iter().filter(|d| d.reason == "min_qty").collect();
    assert_eq!(
        refused,
        [&OrderDrop {
            symbol: "SYM0".to_string(),
            reason: "min_qty".to_string(),
            size: 1.0,
            weight: 0.25
        }]
    );
}

#[test]
fn closing_dust_after_step_widens_still_closes() {
    // The PIT step_size can WIDEN over a position's life: open on a fine grid, close on a coarse
    // one. A full close whose remainder step-rounds to 0.0 must STILL execute (never strand a
    // position / defeat a force-close), by falling back to the raw size.
    let symbol_name = "SYM0";
    let bars = mk_bars(symbol_name, &[10.0, 10.0, 10.0]);
    let series = vec![(symbol_name.to_string(), bars)];
    // bar[i].ts = 1_700_000_000_000 + i*60_000. Fine step 0.001 up to the open-fill bar (idx1),
    // coarse step 0.01 from the close-fill bar (index2) on — so round(0.005, 0.01) = 0.0.
    let close_timestamp = 1_700_000_000_000 + 2 * 60_000;
    #[expect(clippy::type_complexity)]
    let grid_source: Arc<dyn Fn(&str, &str, i64) -> Option<SymbolProperties> + Send + Sync> =
        Arc::new(move |_v, _s, ts| {
            let step = if ts >= close_timestamp { 0.01 } else { 0.001 };
            Some(SymbolProperties { step_size: step, ..Default::default() })
        });
    let strat = OpenClose {
        symbol: symbol_name.to_string(),
        open_at: 0,
        close_at: 1,
        side: 1,
        size: 0.005,
    };
    let mut e = StrategyEngine::new(series, strat, base_params(Some(grid_source)));
    let r = e.run();

    assert_eq!(r.trades.len(), 1, "closing dust (rounded to 0 by a widened step) must still close");
    assert_eq!(e.core.symbol[0].pos.size, 0.0, "position must be fully flat, not stranded");
}

/// ⚠ **The cash gate judges the size and price the grid will fill, not the raw ones.** Raw, 1.07 @
/// 100.4 is 107.43 and fits 109 of cash; on this grid it fills 1.1 @ 100 = 110, and a pre-check on
/// the raw numbers let it through and took cash to -1.0.
#[test]
fn the_cash_gate_judges_what_the_grid_will_fill() {
    let symbol_name = "SYM0";
    let bars = mk_bars(symbol_name, &[100.0, 100.4]);
    let grid = SymbolProperties { tick_size: 1.0, step_size: 0.1, ..Default::default() };
    let strat =
        OpenClose { symbol: symbol_name.to_string(), open_at: 0, close_at: 9, side: 1, size: 1.07 };
    let params =
        StrategyEngineConfig { cash: 109.0, cash_gate: true, ..base_params(Some(fixed(grid))) };
    let mut e = StrategyEngine::new(vec![(symbol_name.to_string(), bars)], strat, params);
    e.run();

    assert_eq!(e.core.cash, 109.0, "the fill would have overdrawn");
    assert_eq!(e.core.symbol[0].pos.size, 0.0);
    assert_eq!(
        e.core.dropped,
        vec![OrderDrop {
            symbol: symbol_name.to_string(),
            reason: "market".to_string(),
            size: 1.07,
            weight: 0.0
        }]
    );
}

/// ⚠ **A fill lands on the tick the venue trades at that price.** The real Deribit BTC-option grid:
/// base `0.0001`, `0.0005` above `0.005`. On the scalar tick a `0.01234` fill books `0.0123`, a price
/// the wire could never carry; below the boundary the base tick stands.
#[test]
fn a_fill_lands_on_the_tier_of_its_price() {
    let symbol_name = "SYM0";
    let scheme = vike_model::GridTickScheme::new(
        0.0001,
        &[vike_model::GridTickTier { above_price: 0.005, tick_size: 0.0005 }],
    )
    .expect("valid grid");
    let grid =
        SymbolProperties { tick_size: 0.0001, ..Default::default() }.with_tick_scheme(scheme);
    let bars = mk_bars(symbol_name, &[0.01234, 0.01234, 0.00123]);
    let strat =
        OpenClose { symbol: symbol_name.to_string(), open_at: 0, close_at: 1, side: 1, size: 1.0 };
    let mut e = StrategyEngine::new(
        vec![(symbol_name.to_string(), bars)],
        strat,
        base_params(Some(fixed(grid))),
    );
    let r = e.run();

    assert_eq!(r.trades.len(), 1);
    let t = &r.trades[0];
    assert!((t.entry_price - 0.0125).abs() < 1e-12, "the 0.0005 tier: {}", t.entry_price);
    assert!((t.exit_price - 0.0012).abs() < 1e-12, "the base tick: {}", t.exit_price);
}

#[test]
fn no_properties_is_byte_identical() {
    let symbol_name = "SYM0";
    let bars = mk_bars(symbol_name, &[100.0, 100.23, 105.0]);

    let strategy_none =
        OpenClose { symbol: symbol_name.to_string(), open_at: 0, close_at: 1, side: 1, size: 1.07 };
    let mut e_none = StrategyEngine::new(
        vec![(symbol_name.to_string(), bars.clone())],
        strategy_none,
        base_params(None),
    );
    let r_none = e_none.run();

    // An all-0.0 grid is the "unconstrained" sentinel (nz(0.0) -> None, gates never trip) — the
    // same fill path must produce byte-identical results as properties: None.
    let strategy_grid =
        OpenClose { symbol: symbol_name.to_string(), open_at: 0, close_at: 1, side: 1, size: 1.07 };
    let mut e_grid = StrategyEngine::new(
        vec![(symbol_name.to_string(), bars)],
        strategy_grid,
        base_params(Some(fixed(SymbolProperties::default()))),
    );
    let r_grid = e_grid.run();

    assert_eq!(r_none.trades, r_grid.trades);
    assert_eq!(r_none.final_equity, r_grid.final_equity);
}
