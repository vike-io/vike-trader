//! Composition gate for the opt-in engine knobs.
//!
//! Every knob shipped so far has its own gate proving (a) its OFF path is byte-identical and
//! (b) its ON path does what it claims — but each proves that with the OTHER knobs off. Nothing
//! exercised two of them ON at once, so "these features compose" rested on code inspection: the
//! engine↔kernel parity gate is only meaningful with settlement off and explicitly excludes
//! `impact`, and each knob's own gate varies exactly one field from `Default`.
//!
//! This file arms THREE bar-path knobs on ONE run —
//! [`EngineParams::impact`] (market-impact slippage),
//! [`EngineParams::max_price_staleness_ms`] (stale-price wait) and
//! [`EngineParams::settlement_period_ms`] (variation margin) —
//! and proves they neither silently disable nor corrupt each other:
//!
//! 1. **Each knob's effect stays individually observable while the others are on.** Every claim is
//!    made by differencing two runs that BOTH have the other two knobs armed, so a knob that only
//!    worked in isolation would fail here.
//! 2. **A knob's own invariant survives the others.** Settlement's defining law — "same total,
//!    different timing", i.e. bit-identical `final_equity` — is re-proved on a tape where impact
//!    has moved the fill price and staleness has moved the fill BAR.
//! 3. **The composed run is coherent**: finite equity everywhere, no phantom or negative-size
//!    position, and the position matches the quantity actually ordered.
//!
//! Deliberately NOT a numeric-outcome gate: it pins no specific fill price (the single-knob gates
//! own those). It pins the RELATIONSHIPS between runs.
//!
//! The other two knobs of the batch — `latency_model` and TIF expiry — are not composed here
//! because neither is on this lane: `latency_model` is consulted by `run_ticks` only (the bar
//! engine never builds an in-flight queue) and TIF expiry is enforced by the paper book's resting
//! deadline sweep, not by `StrategyEngine`. Composing those two belongs in a tick-lane sibling.

use std::sync::{Arc, Mutex};

use vike_backtest::{
    AlmgrenChriss, EngineParams, ImpactModel, SimBroker, StrategyEngine, VariationSettlement,
};
use vike_model::{Bar, Fill, Strategy};

const SYM: &str = "SYM";
const T0: i64 = 1_700_006_400_000; // an exact UTC day boundary, so daily buckets land cleanly
const DAY_MS: i64 = 86_400_000;
const N_BARS: usize = 40;
/// the strategy submits here; the fill would naturally land on `OPEN_AT + 1`
const OPEN_AT: usize = 30;
/// big enough that the impact model's estimate is unmistakably nonzero
const SIZE: f64 = 5_000.0;
const FRESH_VOL: f64 = 100_000.0;

/// The shared tape: a rising zig-zag on daily boundaries (so `sigma` is nonzero and the held
/// position accrues real day-over-day PnL for settlement to bank), where the bar the order would
/// naturally fill on — `OPEN_AT + 1` — is a FILL-FORWARD bar: zero volume, no quotes. That is the
/// exact shape `max_price_staleness_ms` refuses, so arming staleness pushes the fill one bar out,
/// onto a bar whose open is a DIFFERENT price. That price gap is what makes the staleness knob
/// observable end-to-end.
fn tape() -> Vec<Bar> {
    (0..N_BARS)
        .map(|i| {
            // 100, 101, 100.5, 101.5, 101, 102, ... — bounded nonzero returns, net upward drift
            let close = 100.0 + (i % 2) as f64 + (i / 2) as f64 * 0.5;
            let stale = i == OPEN_AT + 1;
            Bar {
                ts: T0 + i as i64 * DAY_MS,
                open: close,
                high: close + 0.5,
                low: close - 0.5,
                close,
                volume: if stale { 0.0 } else { FRESH_VOL },
                funding: None,
                bid: None,
                ask: None,
                symbol: Some(SYM.to_string()),
            }
        })
        .collect()
}

/// Buys `SIZE` once at `OPEN_AT` (raw market) and holds to the end of the tape, so the position
/// spans many daily settlement boundaries. Records every fill it is told about.
///
/// The fills are captured through `on_fill` rather than read back off the position afterwards
/// because **`Position::avg_price` is not the entry price once settlement is armed**: variation
/// margin resets the cost basis to each settlement mark by design, so the final `avg_price` is the
/// last daily mark and is identical no matter what the fill actually cost. Differencing it would
/// silently compare two constants and pass vacuously — exactly the false green this file exists to
/// rule out.
struct BuyAndHold {
    fills: Arc<Mutex<Vec<Fill>>>,
}

impl Strategy<SimBroker> for BuyAndHold {
    fn on_bar(&mut self, ctx: &mut SimBroker, _bar: &Bar) {
        if ctx.index == OPEN_AT {
            ctx.submit(SYM, 1, SIZE, 0.0, true, None);
        }
    }

    fn on_fill(&mut self, _ctx: &mut SimBroker, fill: &Fill) {
        self.fills.lock().unwrap().push(fill.clone());
    }
}

/// Which knobs one run had armed.
#[derive(Debug, Clone, Copy)]
struct Knobs {
    impact: bool,
    stale: bool,
    settle: bool,
}

impl Knobs {
    const ALL: Knobs = Knobs { impact: true, stale: true, settle: true };
    fn without_impact(self) -> Self {
        Knobs { impact: false, ..self }
    }
    fn without_stale(self) -> Self {
        Knobs { stale: false, ..self }
    }
    fn without_settle(self) -> Self {
        Knobs { settle: false, ..self }
    }
}

/// The run outputs the assertions below difference against.
struct RunOut {
    /// the ENTRY fill — the first fill the strategy was told about
    entry: Fill,
    pos_size: f64,
    final_equity: f64,
    equity_curve: Vec<f64>,
    settled_profit: f64,
    settlements: Vec<VariationSettlement>,
}

impl RunOut {
    fn entry_price(&self) -> f64 {
        self.entry.price
    }
}

fn run(knobs: Knobs) -> RunOut {
    let impact: Option<Arc<dyn ImpactModel>> =
        knobs.impact.then(|| Arc::new(AlmgrenChriss::default()) as Arc<dyn ImpactModel>);
    let params = EngineParams {
        // large enough that no cash/leverage gate can interfere with the knobs under test
        cash: 100_000_000.0,
        slippage: 0.0,
        impact,
        max_price_staleness_ms: knobs.stale.then_some(0),
        settlement_period_ms: knobs.settle.then_some(DAY_MS),
        ..Default::default()
    };
    let fills = Arc::new(Mutex::new(Vec::new()));
    let strat = BuyAndHold { fills: Arc::clone(&fills) };
    let mut engine = StrategyEngine::new(vec![(SYM.to_string(), tape())], strat, params);
    let result = engine.run();
    let captured = fills.lock().unwrap().clone();
    let entry = captured
        .first()
        .unwrap_or_else(|| panic!("the test order never filled under {knobs:?}"))
        .clone();
    assert_eq!(entry.side, 1, "the entry fill is the strategy's buy under {knobs:?}");
    assert_eq!(entry.size, SIZE, "the entry filled in full under {knobs:?}");
    RunOut {
        entry,
        pos_size: engine.core.position_of(SYM).size,
        final_equity: result.final_equity,
        equity_curve: result.equity_curve.clone(),
        settled_profit: engine.core.sym[0].settled_profit,
        settlements: engine.core.variation_settlements.clone(),
    }
}

/// Sanity anchor for every difference below: the two candidate fill bars really do open at
/// different prices, so "the fill moved a bar" and "the fill price moved" are distinguishable.
#[test]
fn the_tape_distinguishes_the_stale_bar_from_the_fresh_one() {
    let bars = tape();
    assert_eq!(bars[OPEN_AT + 1].volume, 0.0, "the natural landing bar is fill-forwarded");
    assert!(bars[OPEN_AT + 2].volume > 0.0, "the next bar is a real print");
    assert_ne!(
        bars[OPEN_AT + 1].open,
        bars[OPEN_AT + 2].open,
        "the two candidate fill bars must differ in price or nothing below is observable"
    );
}

/// STALENESS stays observable with impact and settlement armed: the fill is deferred off the
/// fill-forwarded bar onto the next real print, and lands at THAT print's price.
///
/// Both runs carry impact + settlement, so this isolates staleness alone.
#[test]
fn staleness_still_defers_the_fill_while_impact_and_settlement_are_on() {
    let bars = tape();
    let on = run(Knobs::ALL);
    let off = run(Knobs::ALL.without_stale());

    // The fill BAR moves — the least ambiguous evidence the discipline engaged.
    assert_eq!(
        off.entry.ts,
        bars[OPEN_AT + 1].ts,
        "stale-off fills on the fill-forwarded bar, as it always did"
    );
    assert_eq!(
        on.entry.ts,
        bars[OPEN_AT + 2].ts,
        "stale-on defers to the next real print's bar, with impact + settlement also armed"
    );
    // Impact is armed in BOTH runs, so each price is its own bar's open widened by impact — hence
    // the bracketing comparison rather than an equality against a raw open.
    assert!(
        off.entry_price() >= bars[OPEN_AT + 1].open,
        "stale-off prices off the fill-forwarded bar (impact widens a buy upward): {} vs {}",
        off.entry_price(),
        bars[OPEN_AT + 1].open
    );
    assert!(
        on.entry_price() >= bars[OPEN_AT + 2].open,
        "stale-on prices off the next real print: {} vs {}",
        on.entry_price(),
        bars[OPEN_AT + 2].open
    );
    assert_ne!(
        on.entry_price().to_bits(),
        off.entry_price().to_bits(),
        "arming staleness must change the fill even with the other knobs on"
    );
    // the deferred order is never LOST — the defining staleness property
    assert_eq!(on.pos_size, SIZE, "the deferred order still filled in full");
}

/// IMPACT stays observable with staleness and settlement armed: the same order fills strictly
/// WORSE (higher, for a buy) than the identical run without the model.
///
/// Both runs carry staleness + settlement, so both fill on the SAME (deferred) bar and the delta
/// is impact alone — not a fill-bar confound.
#[test]
fn impact_still_widens_the_fill_while_staleness_and_settlement_are_on() {
    let bars = tape();
    let with_impact = run(Knobs::ALL);
    let without = run(Knobs::ALL.without_impact());

    assert_eq!(
        with_impact.entry.ts, without.entry.ts,
        "both runs fill on the SAME deferred bar, so the price delta is impact alone"
    );
    assert_eq!(
        without.entry_price().to_bits(),
        bars[OPEN_AT + 2].open.to_bits(),
        "with impact off (and zero flat slippage) the fill is the deferred bar's raw open"
    );
    assert!(
        with_impact.entry_price() > without.entry_price(),
        "a buy must fill HIGHER under impact even with the other knobs on: {} vs {}",
        with_impact.entry_price(),
        without.entry_price()
    );
    // and it is a real estimate, not a rounding wobble
    assert!(
        with_impact.entry_price() - without.entry_price() > 1e-9,
        "the impact estimate must be materially nonzero"
    );
}

/// SETTLEMENT stays observable with impact and staleness armed: the held position banks its
/// day-over-day PnL on each daily boundary, and the runs without it bank nothing.
#[test]
fn settlement_still_banks_daily_pnl_while_impact_and_staleness_are_on() {
    let on = run(Knobs::ALL);
    let off = run(Knobs::ALL.without_settle());

    assert!(!on.settlements.is_empty(), "daily boundaries must produce settlements");
    assert!(on.settled_profit != 0.0, "the held position banks real day-over-day PnL");
    assert!(off.settlements.is_empty(), "settlement off records nothing");
    assert_eq!(off.settled_profit, 0.0);
    // every settlement is a mark on the symbol under test, in strictly increasing time
    assert!(
        on.settlements.windows(2).all(|w| w[0].ts < w[1].ts),
        "settlements are ordered and de-duplicated per bucket"
    );
}

/// The composition claim with the most teeth: settlement's OWN defining invariant — "same total,
/// different timing", i.e. a settled run ends with bit-identical `final_equity` — must still hold
/// on a tape where **impact has moved the fill price and staleness has moved the fill bar**.
///
/// If arming impact or staleness could perturb the settlement bookkeeping (double-counting a mark,
/// settling against a stale price, or resetting the basis to a price the position never traded),
/// this equality is where it would surface.
#[test]
fn settlement_stays_equity_neutral_under_impact_and_staleness() {
    let on = run(Knobs::ALL);
    let off = run(Knobs::ALL.without_settle());

    assert_eq!(
        on.final_equity.to_bits(),
        off.final_equity.to_bits(),
        "settling must move PnL between realized and unrealized, never create or destroy it"
    );
    assert_eq!(
        on.equity_curve.len(),
        off.equity_curve.len(),
        "and it must not change the number of steps"
    );
    for (i, (a, b)) in on.equity_curve.iter().zip(off.equity_curve.iter()).enumerate() {
        assert_eq!(a.to_bits(), b.to_bits(), "equity diverged at step {i}: {a} vs {b}");
    }
    // the two runs also agree on the position itself — settlement never trades
    assert_eq!(on.pos_size, off.pos_size);
    assert_eq!(on.pos_size, SIZE);
}

/// Coherence of the fully-armed run: nothing is NaN/inf, the position is neither phantom nor
/// negative-size, and it matches exactly the quantity ordered — i.e. the knobs did not corrupt
/// each other into a half-applied or double-applied fill.
#[test]
fn the_fully_armed_run_stays_coherent() {
    for knobs in [
        Knobs::ALL,
        Knobs::ALL.without_impact(),
        Knobs::ALL.without_stale(),
        Knobs::ALL.without_settle(),
    ] {
        let out = run(knobs);
        assert!(out.final_equity.is_finite(), "non-finite final equity under {knobs:?}");
        assert!(
            out.equity_curve.iter().all(|e| e.is_finite()),
            "non-finite equity step under {knobs:?}"
        );
        assert!(out.entry_price().is_finite(), "non-finite entry price under {knobs:?}");
        assert!(out.entry_price() > 0.0, "a long entry must have a positive basis under {knobs:?}");
        assert_eq!(
            out.pos_size, SIZE,
            "the long position must be exactly the ordered size under {knobs:?} \
             (a negative or partial size means the knobs interfered)"
        );
        assert!(out.settled_profit.is_finite(), "non-finite settled profit under {knobs:?}");
    }
}
