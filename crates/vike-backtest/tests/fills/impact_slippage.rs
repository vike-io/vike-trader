//! Gate for the opt-in market-impact slippage model (`EngineParams::impact`, `vike_backtest::impact`).
//!
//! Three properties, in the order they matter:
//! 1. **The default path is byte-identical.** `impact: None` (the default) must produce trades
//!    bit-for-bit equal to the same run on the frozen flat-slippage engine. This is the whole
//!    opt-in claim; the parity/golden suites cover the same ground from the other direction.
//! 2. The formula reaches the FILL — an active model widens the fill price adversely on both
//!    sides, by exactly the amount [`AlmgrenChriss`] computes from the measured window.
//! 3. Monotonicity survives the wire-in: a bigger order fills worse, end to end.
//!
//! 4. The estimate cannot invert a price. `impact_frac` is bounded below but not above, so
//!    `slippage_for` can exceed 1.0 and the multiplicative haircut in `adverse_fill_price` would
//!    turn a SELL of a positive quote into a negative fill. It saturates on
//!    [`vike_backtest::broker_sim::MIN_ADVERSE_FACTOR`] instead, and the saturation is COUNTED.
//!
//! Mirrors the run-construction pattern in `properties_fills.rs` (a tiny `Strategy<SimBroker>`
//! over synthetic bars).

use std::sync::Arc;

use vike_backtest::{
    adverse_fill_price, broker_sim::MIN_ADVERSE_FACTOR, window_stats, AlmgrenChriss, EngineParams,
    ImpactInputs, ImpactModel, SimBroker, StrategyEngine,
};
use vike_model::{Bar, Strategy};

/// Bars with a deterministic zig-zag close (so the measured sigma is > 0) and a flat volume.
fn mk_bars(symbol: &str, n: usize, volume: f64) -> Vec<Bar> {
    (0..n)
        .map(|i| {
            // 100, 101, 100.5, 101.5, ... — nonzero, bounded per-bar returns
            let close = 100.0 + (i % 2) as f64 + (i / 2) as f64 * 0.5;
            Bar {
                ts: 1_700_000_000_000 + i as i64 * 86_400_000,
                open: close,
                high: close + 0.5,
                low: close - 0.5,
                close,
                volume,
                funding: None,
                bid: None,
                ask: None,
                symbol: Some(symbol.to_string()),
            }
        })
        .collect()
}

/// Opens `size` on `side` at bar `open_at` (market). The fill lands at the FOLLOWING bar's open.
struct OpenOnce {
    symbol: String,
    open_at: usize,
    side: i32,
    size: f64,
}

impl Strategy<SimBroker> for OpenOnce {
    fn on_bar(&mut self, ctx: &mut SimBroker, _bar: &Bar) {
        if ctx.index == self.open_at {
            ctx.submit(&self.symbol, self.side, self.size, 0.0, true, None);
        }
    }
}

const SYM: &str = "SYM";
const VOL: f64 = 100_000.0;
const N_BARS: usize = 40;
const OPEN_AT: usize = 30;

/// Run one open-only backtest and return the entry fill price recorded on the position.
fn entry_price(impact: Option<Arc<dyn ImpactModel>>, slippage: f64, side: i32, size: f64) -> f64 {
    let bars = vec![(SYM.to_string(), mk_bars(SYM, N_BARS, VOL))];
    let params = EngineParams { cash: 100_000_000.0, slippage, impact, ..Default::default() };
    let strat = OpenOnce { symbol: SYM.into(), open_at: OPEN_AT, side, size };
    let mut engine = StrategyEngine::new(bars, strat, params);
    engine.run();
    let si = engine.core.symbols.iter().position(|s| s == SYM).expect("symbol registered");
    let pos = engine.core.sym[si].pos;
    assert!(pos.size != 0.0, "the test order must have filled");
    pos.avg_price
}

/// (1) `impact: None` is the frozen path — identical to a run built without ever naming the field.
#[test]
fn default_none_is_byte_identical_to_the_flat_slippage_path() {
    for &slippage in &[0.0, 0.0005, 0.01] {
        for &side in &[1, -1] {
            let with_field = entry_price(None, slippage, side, 500.0);
            // The reference: the exact flat-slippage arithmetic the engine froze.
            let raw = mk_bars(SYM, N_BARS, VOL)[OPEN_AT + 1].open;
            let expect = raw * (1.0 + side as f64 * slippage);
            assert_eq!(
                with_field.to_bits(),
                expect.to_bits(),
                "flat path moved: slippage={slippage} side={side}"
            );
        }
    }
}

/// (2) An active model widens the fill by exactly the formula's estimate, measured off the same
/// window the engine measures — and it is ADVERSE on both sides (buys up, sells down).
#[test]
fn an_active_model_widens_the_fill_by_the_computed_impact() {
    let model = AlmgrenChriss::default();
    let size = 5_000.0;
    let series = mk_bars(SYM, N_BARS, VOL);
    let fill_idx = OPEN_AT + 1;
    let raw = series[fill_idx].open;

    // Reproduce the engine's own window: everything up to but EXCLUDING the bar the fill ts falls
    // inside (no lookahead), last DEFAULT_IMPACT_WINDOW of them. On this coarse lane the fill ts
    // IS the landing bar's ts, so `partition_point(<= ts) - 1` lands on `fill_idx` — see
    // `a_granular_sub_bar_fill_cannot_see_its_own_coarse_bar` for the lane where the distinction
    // between this and `partition_point(< ts)` actually bites.
    let hi = series.partition_point(|b| b.ts <= series[fill_idx].ts) - 1;
    assert_eq!(hi, fill_idx, "the coarse lane's window ends at the landing bar");
    let stats = window_stats(&series[..hi], vike_backtest::DEFAULT_IMPACT_WINDOW)
        .expect("40 synthetic bars measure fine");
    let extra = model.impact_frac(&ImpactInputs {
        qty: size,
        avg_volume: stats.avg_volume,
        sigma: stats.sigma,
    });
    assert!(extra > 0.0, "the synthetic series must produce a real impact estimate");

    for &side in &[1, -1] {
        let flat = entry_price(None, 0.0, side, size);
        let with_impact =
            entry_price(Some(Arc::new(model) as Arc<dyn ImpactModel>), 0.0, side, size);
        let expect = raw * (1.0 + side as f64 * extra);
        assert_eq!(with_impact.to_bits(), expect.to_bits(), "side={side}");
        if side > 0 {
            assert!(with_impact > flat, "a buy must fill HIGHER under impact");
        } else {
            assert!(with_impact < flat, "a sell must fill LOWER under impact");
        }
    }
}

/// The model composes ADDITIVELY with the flat `slippage` rather than replacing it.
#[test]
fn impact_adds_to_the_flat_slippage_it_does_not_replace_it() {
    let model: Arc<dyn ImpactModel> = Arc::new(AlmgrenChriss::default());
    let size = 5_000.0;
    let flat_only = entry_price(None, 0.001, 1, size);
    let impact_only = entry_price(Some(model.clone()), 0.0, 1, size);
    let both = entry_price(Some(model), 0.001, 1, size);
    let raw = mk_bars(SYM, N_BARS, VOL)[OPEN_AT + 1].open;
    // Each cost is raw*frac above raw; together they must be the sum of the two increments.
    let expect = (flat_only - raw) + (impact_only - raw);
    assert!(((both - raw) - expect).abs() < 1e-9, "both={both} expect={}", raw + expect);
}

/// (3) Monotonicity end to end: through the engine, a bigger order fills worse.
#[test]
fn bigger_orders_fill_worse_through_the_engine() {
    let model: Arc<dyn ImpactModel> = Arc::new(AlmgrenChriss::default());
    let mut prev = f64::NEG_INFINITY;
    for size in [100.0, 1_000.0, 5_000.0, 20_000.0, 50_000.0] {
        let px = entry_price(Some(model.clone()), 0.0, 1, size);
        assert!(px > prev, "buy fill must worsen with size at {size}: {px} <= {prev}");
        prev = px;
    }
}

/// A longer execution horizon works the order more slowly, so it costs less on the temporary
/// term — the knob must actually reach the engine.
#[test]
fn a_longer_execution_horizon_fills_better() {
    let fast: Arc<dyn ImpactModel> = Arc::new(AlmgrenChriss::with_exec_time(1.0));
    let slow: Arc<dyn ImpactModel> = Arc::new(AlmgrenChriss::with_exec_time(5.0));
    assert!(entry_price(Some(slow), 0.0, 1, 20_000.0) < entry_price(Some(fast), 0.0, 1, 20_000.0));
}

/// A window the model cannot measure (too few bars to date) must fall back to the flat slippage
/// rather than fabricate a cost — the guard that keeps the first bars of a run sane.
#[test]
fn an_unmeasurable_window_falls_back_to_flat_slippage() {
    // Fill lands on bar 1 (submitted on bar 0), so only ONE closed bar precedes it: no returns.
    let bars = vec![(SYM.to_string(), mk_bars(SYM, N_BARS, VOL))];
    let params = EngineParams {
        cash: 100_000_000.0,
        slippage: 0.002,
        impact: Some(Arc::new(AlmgrenChriss::default())),
        ..Default::default()
    };
    let strat = OpenOnce { symbol: SYM.into(), open_at: 0, side: 1, size: 5_000.0 };
    let mut e = StrategyEngine::new(bars, strat, params);
    e.run();
    let si = e.core.symbols.iter().position(|s| s == SYM).unwrap();
    let raw = mk_bars(SYM, N_BARS, VOL)[1].open;
    assert_eq!(e.core.sym[si].pos.avg_price.to_bits(), (raw * 1.002).to_bits());
}

// --- maker exemption: impact is a TAKER cost ---------------------------------------------------

/// Rests a buy limit at `price` on bar `open_at`; it is hit on the following bar.
struct RestLimitOnce {
    symbol: String,
    open_at: usize,
    side: i32,
    size: f64,
    price: f64,
}

impl Strategy<SimBroker> for RestLimitOnce {
    fn on_bar(&mut self, ctx: &mut SimBroker, _bar: &Bar) {
        if ctx.index == self.open_at {
            ctx.submit_limit(&self.symbol, self.side, self.size, self.price, 0.0, true, None);
        }
    }
}

fn maker_entry_price(
    impact: Option<Arc<dyn ImpactModel>>,
    slippage: f64,
    size: f64,
    px: f64,
) -> f64 {
    let bars = vec![(SYM.to_string(), mk_bars(SYM, N_BARS, VOL))];
    let params = EngineParams { cash: 100_000_000.0, slippage, impact, ..Default::default() };
    let strat = RestLimitOnce { symbol: SYM.into(), open_at: OPEN_AT, side: 1, size, price: px };
    let mut engine = StrategyEngine::new(bars, strat, params);
    engine.run();
    let si = engine.core.symbols.iter().position(|s| s == SYM).expect("symbol registered");
    let pos = engine.core.sym[si].pos;
    assert!(pos.size != 0.0, "the resting limit must have been hit");
    pos.avg_price
}

/// A PASSIVE fill is not charged impact — the load-bearing property for every maker/HFT lane.
///
/// Almgren–Chriss temporary impact is the concession paid for DEMANDING liquidity. An order that
/// rested and got hit SUPPLIED it, so charging it impact would fill the limit THROUGH its own
/// price — an impossible fill. Sized deliberately at half the bar's entire volume, where an
/// unguarded model would charge hundreds of bp: the maker fill must still land exactly on its
/// limit, bit-for-bit, while the same size taken aggressively is charged in full.
#[test]
fn a_passive_maker_fill_is_never_charged_impact() {
    let model: Arc<dyn ImpactModel> = Arc::new(AlmgrenChriss::default());
    let size = 50_000.0; // half of VOL — an enormous participation rate
    let series = mk_bars(SYM, N_BARS, VOL);
    let hit_bar = &series[OPEN_AT + 1];
    // Resting between the next bar's low and its open, so it is hit and fills AT the limit
    // (`price.min(bar.open)` — the limit is below the open, so the limit is the fill).
    let limit = hit_bar.low + 0.2;
    assert!(limit < hit_bar.open, "the limit must be passive relative to the open");

    let flat = maker_entry_price(None, 0.0, size, limit);
    let with_impact = maker_entry_price(Some(model.clone()), 0.0, size, limit);
    assert_eq!(with_impact.to_bits(), flat.to_bits(), "an impact model moved a PASSIVE fill");
    assert_eq!(with_impact.to_bits(), limit.to_bits(), "a maker must fill at its own limit");

    // The control: the identical size, taken aggressively, IS charged — so the exemption above
    // is the maker gate doing work, not the model quietly returning zero at this size.
    let taker_flat = entry_price(None, 0.0, 1, size);
    let taker_impact = entry_price(Some(model), 0.0, 1, size);
    assert!(taker_impact > taker_flat, "a TAKER of the same size must still pay impact");
}

/// The flat `slippage` is unchanged for makers — only the size-dependent addend is taker-only.
/// (Whether a flat maker slippage is itself realistic is a pre-existing question this branch
/// deliberately does not reopen; what matters is that turning `impact` on does not change it.)
#[test]
fn the_flat_slippage_still_applies_to_makers() {
    let series = mk_bars(SYM, N_BARS, VOL);
    let limit = series[OPEN_AT + 1].low + 0.2;
    let flat = maker_entry_price(None, 0.001, 5_000.0, limit);
    // Mirrors `adverse_fill_price`'s own expression (`raw * (1.0 + side * slippage)`) rather than
    // folding it to `* 1.001` — the folded literal is not guaranteed to be the same double.
    assert_eq!(flat.to_bits(), (limit * (1.0 + 0.001)).to_bits());
    let with_impact =
        maker_entry_price(Some(Arc::new(AlmgrenChriss::default())), 0.001, 5_000.0, limit);
    assert_eq!(with_impact.to_bits(), flat.to_bits());
}

// --- lookahead freedom on the granular sub-bar lane --------------------------------------------

/// The sub-bar lane must NOT see the coarse bar it is trading inside.
///
/// `dispatch_fill` is called with `sub.ts`, which is strictly LATER than the containing coarse
/// bar's `ts`. A window taken as "bars with `ts < sub.ts`" therefore includes that coarse bar's
/// realised close and full-period volume — neither knowable intrabar, and the volume would in
/// part be this very order. The engine excludes the CONTAINING bar instead, so the fill must
/// price off the window ending at the coarse bar, not through it.
///
/// The discriminator: the containing coarse bar carries 100x the volume of every other bar, so
/// leaking it would collapse the participation rate and make the fill markedly cheaper.
#[test]
fn a_granular_sub_bar_fill_cannot_see_its_own_coarse_bar() {
    let mut coarse = mk_bars(SYM, N_BARS, VOL);
    let fill_idx = OPEN_AT + 1;
    coarse[fill_idx].volume = VOL * 100.0; // the bar that must NOT be visible

    // One sub-bar strictly inside coarse[fill_idx], with its own open as the fill price.
    let sub = Bar {
        ts: coarse[fill_idx].ts + 3_600_000,
        open: 200.0,
        high: 201.0,
        low: 199.0,
        close: 200.0,
        volume: VOL,
        funding: None,
        bid: None,
        ask: None,
        symbol: Some(SYM.to_string()),
    };
    assert!(sub.ts > coarse[fill_idx].ts && sub.ts < coarse[fill_idx + 1].ts);

    let size = 5_000.0;
    let model = AlmgrenChriss::default();
    let stats_of = |hi: usize| {
        window_stats(&coarse[..hi], vike_backtest::DEFAULT_IMPACT_WINDOW).expect("measurable")
    };
    let frac = |hi: usize| {
        model.impact_frac(&ImpactInputs {
            qty: size,
            avg_volume: stats_of(hi).avg_volume,
            sigma: stats_of(hi).sigma,
        })
    };
    let honest = frac(fill_idx); // window ENDS at the coarse bar — what the engine must use
    let leaked = frac(fill_idx + 1); // window THROUGH it — the lookahead this pins out
    assert!(honest > leaked, "the volume spike must make the leaked estimate cheaper");

    let params = EngineParams {
        cash: 100_000_000.0,
        slippage: 0.0,
        impact: Some(Arc::new(model)),
        granular_by_symbol: vec![(SYM.to_string(), vec![sub.clone()])],
        ..Default::default()
    };
    let strat = OpenOnce { symbol: SYM.into(), open_at: OPEN_AT, side: 1, size };
    let mut e = StrategyEngine::new(vec![(SYM.to_string(), coarse.clone())], strat, params);
    e.run();
    let si = e.core.symbols.iter().position(|s| s == SYM).unwrap();
    let got = e.core.sym[si].pos.avg_price;
    assert_eq!(
        got.to_bits(),
        (sub.open * (1.0 + honest)).to_bits(),
        "granular fill priced off the wrong window (leaked would be {})",
        sub.open * (1.0 + leaked)
    );
}

// --- the shared-cash gate must price the fill it is about to make ------------------------------

/// `fill_step_gated`'s cash pre-check has to charge the SAME price `apply_fill` will, or an
/// impact-priced fill passes a gate computed on the flat slippage and overdraws the shared pool.
/// Funded to sit BETWEEN the flat notional and the impacted one: the flat run must fill, the
/// impacted run must be dropped.
#[test]
fn the_shared_cash_gate_prices_the_fill_at_its_impacted_price() {
    let series = mk_bars(SYM, N_BARS, VOL);
    let fill_idx = OPEN_AT + 1;
    let raw = series[fill_idx].open;
    let size = 5_000.0;

    let hi = series.partition_point(|b| b.ts <= series[fill_idx].ts) - 1;
    let stats = window_stats(&series[..hi], vike_backtest::DEFAULT_IMPACT_WINDOW).expect("ok");
    let extra = AlmgrenChriss::default().impact_frac(&ImpactInputs {
        qty: size,
        avg_volume: stats.avg_volume,
        sigma: stats.sigma,
    });
    assert!(extra > 0.0);

    // Halfway between what the flat price costs and what the impacted price costs.
    let flat_cost = size * raw;
    let impacted_cost = size * raw * (1.0 + extra);
    let cash = 0.5 * (flat_cost + impacted_cost);

    let run = |impact: Option<Arc<dyn ImpactModel>>| {
        let params =
            EngineParams { cash, slippage: 0.0, cash_gate: true, impact, ..Default::default() };
        let strat = OpenOnce { symbol: SYM.into(), open_at: OPEN_AT, side: 1, size };
        let mut e =
            StrategyEngine::new(vec![(SYM.to_string(), mk_bars(SYM, N_BARS, VOL))], strat, params);
        e.run();
        let si = e.core.symbols.iter().position(|s| s == SYM).unwrap();
        (e.core.sym[si].pos.size, e.core.dropped.len())
    };

    let (flat_pos, flat_dropped) = run(None);
    assert!(flat_pos > 0.0, "the flat-priced order is affordable and must fill");
    assert_eq!(flat_dropped, 0);

    let (impact_pos, impact_dropped) = run(Some(Arc::new(AlmgrenChriss::default())));
    assert_eq!(impact_pos, 0.0, "the impacted order is unaffordable and must be dropped");
    assert!(impact_dropped > 0, "and it must be RECORDED as dropped, not silently skipped");
}

// --- the adverse move may not carry a fill price through zero ----------------------------------

/// A model reporting a FIXED impact fraction whatever the market context.
///
/// The direct way to drive `slippage_for` past 1.0 without depending on exactly how steep
/// Almgren–Chriss happens to be at some contrived size — what is under test here is the ENGINE's
/// behaviour at an oversized estimate, not the model's calibration. (`AlmgrenChriss` gets there on
/// its own: at a measured per-bar sigma of 20% it crosses 1.0 near 18x the window's mean bar
/// volume, and a thin resampled series supplies both halves.)
#[derive(Debug)]
struct FixedImpact(f64);

impl ImpactModel for FixedImpact {
    fn impact_frac(&self, _input: &ImpactInputs) -> f64 {
        self.0
    }
}

/// [`entry_price`]'s twin, also handing back the saturation counter that `entry_price` drops with
/// the engine.
fn entry_price_and_saturations(
    impact: Option<Arc<dyn ImpactModel>>,
    slippage: f64,
    side: i32,
    size: f64,
) -> (f64, u64) {
    let bars = vec![(SYM.to_string(), mk_bars(SYM, N_BARS, VOL))];
    let params = EngineParams { cash: 100_000_000.0, slippage, impact, ..Default::default() };
    let strat = OpenOnce { symbol: SYM.into(), open_at: OPEN_AT, side, size };
    let mut engine = StrategyEngine::new(bars, strat, params);
    engine.run();
    let si = engine.core.symbols.iter().position(|s| s == SYM).expect("symbol registered");
    let pos = engine.core.sym[si].pos;
    assert!(pos.size != 0.0, "the test order must have filled");
    (pos.avg_price, engine.core.slippage_saturations)
}

/// A SELL whose total adverse move exceeds the whole price fills POSITIVE, on the floor — and the
/// saturation is recorded rather than swallowed.
///
/// Before the floor this fill priced at `raw * (1 - 1.5)` = a negative price out of a positive
/// quote, which `vike_model::round_to` then preserved into the position's basis, the fee, the cash
/// flow and the equity curve.
#[test]
fn a_sell_whose_impact_exceeds_the_whole_price_fills_positive_and_is_counted() {
    let raw = mk_bars(SYM, N_BARS, VOL)[OPEN_AT + 1].open;
    assert!(raw > 0.0, "the synthetic quote is positive; only the MOVE is oversized");
    let model: Arc<dyn ImpactModel> = Arc::new(FixedImpact(1.5));
    let (px, saturations) = entry_price_and_saturations(Some(model), 0.0, -1, 500.0);
    assert!(px > 0.0, "a positive quote must not sell for a negative price, got {px}");
    assert_eq!(px.to_bits(), (raw * MIN_ADVERSE_FACTOR).to_bits(), "not on the floor: {px}");
    assert_eq!(saturations, 1, "a saturated cost model must SAY so, not fill silently");
}

/// The floor covers the frozen flat `slippage` field too, not just the impact addend — this is the
/// literal case `vike-ops/tests/duplicate_shape_gate.rs`'s `sim_broker.rs` ALLOWLIST row cites
/// (sell 2 @ raw 100 with slippage 1.5 -> -50), which is what made that below-min floor's SIGNED
/// notional product unconditionally trip on the sell side.
#[test]
fn the_frozen_flat_slippage_field_is_floored_too() {
    // At the scalar the row quotes.
    let scalar = adverse_fill_price(100.0, -1, 1.5);
    assert_eq!(scalar.to_bits(), 1.0f64.to_bits());
    assert!(2.0 * scalar > 0.0, "the SIGNED notional that floor computes is positive again");
    // And end to end through the engine, with NO impact model configured at all.
    let raw = mk_bars(SYM, N_BARS, VOL)[OPEN_AT + 1].open;
    let (px, saturations) = entry_price_and_saturations(None, 1.5, -1, 2.0);
    assert!(px > 0.0, "got {px}");
    assert_eq!(px.to_bits(), (raw * MIN_ADVERSE_FACTOR).to_bits());
    assert_eq!(saturations, 1);
}

/// The BUY side of that same oversized estimate is UNTOUCHED: a buy's factor is `1 + slippage`,
/// which cannot cross zero for any non-negative cost, so nothing saturates and the fill is exactly
/// what the frozen arithmetic produced.
#[test]
fn the_buy_side_of_the_same_oversized_impact_is_untouched() {
    let raw = mk_bars(SYM, N_BARS, VOL)[OPEN_AT + 1].open;
    let model: Arc<dyn ImpactModel> = Arc::new(FixedImpact(1.5));
    let (px, saturations) = entry_price_and_saturations(Some(model), 0.0, 1, 500.0);
    assert_eq!(px.to_bits(), (raw * (1.0 + 1.5)).to_bits(), "the buy side moved");
    assert_eq!(saturations, 0, "nothing saturated on the buy side");
}

/// The control that keeps the floor honest: a NORMAL impacted run — the one
/// `an_active_model_widens_the_fill_by_the_computed_impact` pins bit-for-bit — must be byte-
/// identical AND must never touch the counter, on either side. If the floor could bind here it
/// would be silently re-pricing every ordinary fill.
#[test]
fn a_normal_impacted_fill_is_byte_identical_and_never_counts_a_saturation() {
    let series = mk_bars(SYM, N_BARS, VOL);
    let fill_idx = OPEN_AT + 1;
    let raw = series[fill_idx].open;
    let hi = series.partition_point(|b| b.ts <= series[fill_idx].ts) - 1;
    let stats = window_stats(&series[..hi], vike_backtest::DEFAULT_IMPACT_WINDOW).expect("ok");
    let extra = AlmgrenChriss::default().impact_frac(&ImpactInputs {
        qty: 5_000.0,
        avg_volume: stats.avg_volume,
        sigma: stats.sigma,
    });
    for &side in &[1, -1] {
        let model: Arc<dyn ImpactModel> = Arc::new(AlmgrenChriss::default());
        let (px, saturations) = entry_price_and_saturations(Some(model), 0.001, side, 5_000.0);
        assert_eq!(
            px.to_bits(),
            (raw * (1.0 + side as f64 * (0.001 + extra))).to_bits(),
            "a normal impacted fill moved on side={side}"
        );
        assert_eq!(saturations, 0, "a normal fill must not count as saturated (side={side})");
    }
}

/// A genuinely NEGATIVE quoted price is legitimate (futures, EOD backfills — the case
/// `vike_model::gross_notional`'s doc explicitly refuses to `.abs()` away) and must flow through
/// COMPLETELY untouched. The floor is on the adverse FACTOR, not on the price, so the fill keeps
/// the sign of the quote it was priced off instead of being "repaired" upward.
#[test]
fn a_negative_quoted_price_flows_through_untouched() {
    // The same synthetic series with every price negated (high/low swap under negation).
    let negated: Vec<Bar> = mk_bars(SYM, N_BARS, VOL)
        .into_iter()
        .map(|b| Bar { open: -b.open, high: -b.low, low: -b.high, close: -b.close, ..b })
        .collect();
    let fill_idx = OPEN_AT + 1;
    let raw = negated[fill_idx].open;
    assert!(raw < 0.0, "the quote under test must really be negative, got {raw}");
    // A negative close cannot support a return, so `window_stats` declines and the impact model
    // never contributes here — the flat slippage is the whole adverse move.
    assert!(window_stats(&negated[..fill_idx], vike_backtest::DEFAULT_IMPACT_WINDOW).is_none());

    for &side in &[1, -1] {
        let params = EngineParams {
            cash: 100_000_000.0,
            slippage: 0.01,
            impact: Some(Arc::new(AlmgrenChriss::default())),
            ..Default::default()
        };
        let strat = OpenOnce { symbol: SYM.into(), open_at: OPEN_AT, side, size: 500.0 };
        let mut e = StrategyEngine::new(vec![(SYM.to_string(), negated.clone())], strat, params);
        e.run();
        let si = e.core.symbols.iter().position(|s| s == SYM).unwrap();
        let px = e.core.sym[si].pos.avg_price;
        assert_eq!(
            px.to_bits(),
            (raw * (1.0 + side as f64 * 0.01)).to_bits(),
            "a negative quote was re-priced on side={side}"
        );
        assert!(px < 0.0, "a negative quote must stay negative, got {px} on side={side}");
        assert_eq!(e.core.slippage_saturations, 0, "nothing saturated (side={side})");
    }
}

/// Zero-volume bars cannot price impact at all — the estimate is undefined, so the flat cost
/// stands. (Bar series assembled from tick resamples routinely carry no volume.)
#[test]
fn zero_volume_bars_charge_only_the_flat_slippage() {
    let bars = vec![(SYM.to_string(), mk_bars(SYM, N_BARS, 0.0))];
    let params = EngineParams {
        cash: 100_000_000.0,
        slippage: 0.002,
        impact: Some(Arc::new(AlmgrenChriss::default())),
        ..Default::default()
    };
    let strat = OpenOnce { symbol: SYM.into(), open_at: OPEN_AT, side: 1, size: 5_000.0 };
    let mut e = StrategyEngine::new(bars, strat, params);
    e.run();
    let si = e.core.symbols.iter().position(|s| s == SYM).unwrap();
    let raw = mk_bars(SYM, N_BARS, 0.0)[OPEN_AT + 1].open;
    assert_eq!(e.core.sym[si].pos.avg_price.to_bits(), (raw * 1.002).to_bits());
}
