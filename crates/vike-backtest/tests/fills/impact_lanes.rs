//! The impact model on the REPLAY lanes — `tests/fills/impact_slippage.rs`'s tick/L2 twin.
//!
//! `impact_slippage.rs` gates the bar lane and stays the authority for the model's own arithmetic
//! and for the saturation floor. What is gated HERE is the lane split, and every test states the
//! property it would lose if the wiring were reverted:
//!
//! 1. **The model reaches the tick lane at all.** It did not: `run_ticks` is routinely driven with
//!    an EMPTY bar series per symbol, so the bar-window read measured `None` and an operator who
//!    configured a model got a silent zero — and the harness refused the combination outright, so
//!    nobody found out. Asserted as a MEASURABLE DIFFERENCE IN A FILLED PRICE, reconstructed from
//!    the same `TickWindow` the engine measures.
//! 2. **The L2 lane is charged the PERMANENT TERM ONLY.** The book walk already priced the
//!    temporary concession off real displayed depth; charging it again would double-count the same
//!    liquidity. Asserted by pinning the fill to the permanent-only price AND showing the
//!    both-terms price is a different, strictly worse number — so a lane wired to
//!    `ImpactTerms::Both` fails here rather than merely being pessimistic.
//! 3. **The opt-in absent is byte-identical**, on both new lanes.
//! 4. **The maker exemption is LANE-shaped.** The engine's `is_maker` flag is set from
//!    `OrderKind::Limit` alone, which says nothing about aggressiveness — on these two lanes a
//!    `Limit` fill is priced by a marketable branch (the L1 tier fills it AT THE ASK for any size,
//!    the L2 tier walks the ladder), so it is charged. Asserted as the equality that closes the
//!    hole: a marketable limit pays EXACTLY what a market order of the same size pays, so the
//!    model cannot be escaped by respelling a taker. The bar lane's exemption — where
//!    `order_fill_price` really does return at-or-better than the limit — is asserted to survive.
//! 5. **No lookahead**: the print of the fill's own tick is not in the window it is priced against.
//!
//! Frozen fixtures are untouched by construction: every run here sets `impact` explicitly, and the
//! `None` arm is asserted equal to the flat arithmetic rather than to a golden.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use vike_backtest::engine::{EngineParams, FillModelKind, SimBroker, StrategyEngine, Tick};
use vike_backtest::{
    AlmgrenChriss, ImpactInputs, ImpactModel, ImpactTerms, TickWindow, adverse_fill_price,
};
use vike_model::{
    Bar, BookUpdate, BookUpdateKind, L2Book, QuoteTick, Strategy, TradeTick, book_taker_price,
};

const SYM: &str = "TOK";
/// Long enough that every print of the tape below fits, so the window is the WHOLE print history
/// and a test can reconstruct it without modelling the eviction rule (which `impact.rs`'s own
/// `the_tick_window_is_bounded_and_keeps_the_newest_prints` covers).
const WINDOW: usize = 64;

fn q(ts: i64, bid: f64, ask: f64) -> Tick {
    Tick::Quote(QuoteTick {
        ts,
        local_ts: 0,
        bid,
        ask,
        bid_size: 1_000.0,
        ask_size: 1_000.0,
        symbol: SYM.to_string(),
    })
}

fn tr(ts: i64, price: f64, size: f64) -> Tick {
    Tick::Trade(TradeTick {
        ts,
        local_ts: 0,
        price,
        size,
        is_buyer_maker: false,
        symbol: SYM.to_string(),
    })
}

/// The prints the window is built from: a deterministic zig-zag (so the measured sigma is > 0)
/// over a flat print size. `PRINTS` is what a test reconstructs the engine's context from.
const PRINTS: [(f64, f64); 8] = [
    (100.0, 40.0),
    (101.0, 60.0),
    (100.5, 50.0),
    (101.5, 30.0),
    (100.75, 70.0),
    (101.25, 45.0),
    (100.25, 55.0),
    (101.75, 35.0),
];

/// The tape: every print, then two quotes. The strategy submits on the FIRST quote and the fill
/// lands on the SECOND — so at fill time the window holds exactly `PRINTS` and nothing else
/// (quotes record no print at all, and the fill's own tick is a quote).
fn tape() -> Vec<Tick> {
    let mut ticks: Vec<Tick> =
        PRINTS.iter().enumerate().map(|(i, (px, sz))| tr(i as i64 + 1, *px, *sz)).collect();
    ticks.push(q(100, BID, ASK));
    ticks.push(q(101, BID, ASK));
    ticks
}

const BID: f64 = 101.0;
const ASK: f64 = 102.0;

/// The market context the engine will have measured at fill time, rebuilt from `PRINTS` through
/// the SAME `TickWindow` the engine uses — so this is a reconstruction, not a second definition.
fn expected_context() -> vike_backtest::MarketStats {
    let mut w = TickWindow::with_capacity(WINDOW);
    for (px, sz) in PRINTS {
        w.push(px, sz);
    }
    w.stats().expect("the synthetic tape must be measurable")
}

/// Submits one MARKET order on the first quote it sees; never trades again.
struct BuyOnFirstQuote {
    side: i32,
    size: f64,
    done: bool,
}

impl Strategy<SimBroker> for BuyOnFirstQuote {
    fn on_quote_tick(&mut self, ctx: &mut SimBroker, _t: &QuoteTick) {
        if !self.done {
            self.done = true;
            ctx.submit(SYM, self.side, self.size, 0.0, true, None);
        }
    }
}

/// Submits a LIMIT the second quote CROSSES. The engine classifies it `is_maker` (that flag reads
/// `OrderKind::Limit` and nothing else), but on both replay lanes it is priced by a marketable
/// branch — the L1 tier hands it the ASK, the L2 tier hands it a walk — so it demanded liquidity.
/// Naming it "passive" is the mistake the two tests below exist to stop anyone making again.
struct RestLimitOnFirstQuote {
    side: i32,
    size: f64,
    price: f64,
    done: bool,
}

impl Strategy<SimBroker> for RestLimitOnFirstQuote {
    fn on_quote_tick(&mut self, ctx: &mut SimBroker, _t: &QuoteTick) {
        if !self.done {
            self.done = true;
            ctx.submit_limit(SYM, self.side, self.size, self.price, 0.0, true, None);
        }
    }
}

/// Run one tick replay and hand back the entry fill price.
fn tick_entry_price<S: Strategy<SimBroker>>(
    strategy: S,
    impact: Option<Arc<dyn ImpactModel>>,
    slippage: f64,
    fill_model: FillModelKind,
    ticks: Vec<Tick>,
) -> f64 {
    let params = EngineParams {
        cash: 100_000_000.0,
        slippage,
        impact,
        impact_window: WINDOW,
        fill_model,
        ..Default::default()
    };
    // The per-symbol BAR series is EMPTY, exactly as every tick-lane caller drives it — which is
    // precisely the configuration in which the bar-window read measured nothing.
    let mut engine = StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], strategy, params);
    engine.run_ticks(&[(SYM.to_string(), ticks)]);
    let si = engine.core.symbols.iter().position(|s| s == SYM).expect("symbol registered");
    let pos = engine.core.sym[si].pos;
    assert!(pos.size != 0.0, "the test order must have filled");
    pos.avg_price
}

// --- 1. the model REACHES the tick lane -------------------------------------------------------

/// The headline: on an L1 tick tape a market order's fill price MOVES when the model is armed,
/// and moves by exactly the amount the model computes from the tape's own prints.
///
/// This is the test that fails if the tick wiring is reverted — before it, `impact_ticks` did not
/// exist, `slippage_for` read an empty bar series, `window_stats` measured `None`, and the armed
/// and unarmed runs produced the identical price.
#[test]
fn an_armed_model_moves_a_tick_lane_fill_by_the_computed_impact() {
    let size = 900.0;
    let ctx = expected_context();
    let extra = AlmgrenChriss::default().impact_frac_for(
        &ImpactInputs { qty: size, avg_volume: ctx.avg_volume, sigma: ctx.sigma },
        ImpactTerms::Both,
    );
    assert!(extra > 0.0, "the synthetic tape must produce a real estimate");

    for &side in &[1, -1] {
        let strat = || BuyOnFirstQuote { side, size, done: false };
        let flat = tick_entry_price(strat(), None, 0.0, FillModelKind::Tick, tape());
        // A market order crosses the real spread on the L1 tier: buy@ask, sell@bid.
        let raw = if side > 0 { ASK } else { BID };
        assert_eq!(flat.to_bits(), raw.to_bits(), "the unarmed L1 fill is the quote itself");

        let armed = tick_entry_price(
            strat(),
            Some(Arc::new(AlmgrenChriss::default())),
            0.0,
            FillModelKind::Tick,
            tape(),
        );
        assert_eq!(
            armed.to_bits(),
            adverse_fill_price(raw, side, extra).to_bits(),
            "side={side}: the tick lane did not charge the computed impact"
        );
        if side > 0 {
            assert!(armed > flat, "a buy must fill HIGHER under impact");
        } else {
            assert!(armed < flat, "a sell must fill LOWER under impact");
        }
    }
}

/// Monotonicity survives the wire-in on this lane too: a bigger taker fills worse, end to end.
#[test]
fn a_bigger_tick_lane_taker_fills_worse() {
    let mut prev = 0.0f64;
    for size in [50.0, 500.0, 5_000.0] {
        let px = tick_entry_price(
            BuyOnFirstQuote { side: 1, size, done: false },
            Some(Arc::new(AlmgrenChriss::default())),
            0.0,
            FillModelKind::Tick,
            tape(),
        );
        assert!(
            px > prev,
            "size={size} did not fill worse than the smaller order ({px} <= {prev})"
        );
        prev = px;
    }
}

/// The opt-in ABSENT is byte-identical on the tick lane: the fill is the frozen L1 arithmetic,
/// with no impact term and no flat-slippage term applied to it.
#[test]
fn an_absent_model_leaves_the_tick_lane_byte_identical() {
    for &slippage in &[0.0, 0.0005, 0.01] {
        for &side in &[1, -1] {
            let px = tick_entry_price(
                BuyOnFirstQuote { side, size: 900.0, done: false },
                None,
                slippage,
                FillModelKind::Tick,
                tape(),
            );
            let raw = if side > 0 { ASK } else { BID };
            assert_eq!(
                px.to_bits(),
                (raw * (1.0 + side as f64 * slippage)).to_bits(),
                "the frozen flat-slippage tick path moved: slippage={slippage} side={side}"
            );
        }
    }
}

// --- 5. no lookahead ---------------------------------------------------------------------------

/// The print of the fill's own tick is NOT in the window it is priced against — the tick-lane
/// statement of the discipline the bar lane enforces by excluding the bar `ts` falls inside.
/// Without it a large order would be charged against a market context that already contains its
/// own execution, which is lookahead of exactly the kind the signal path is forbidden.
///
/// The tape makes the fill land ON a trade print (a market order on a quote-less trade tick fills
/// at the print, by the frozen `order_fill_price` law), so "was that print in the window" is a
/// question with two numerically different answers — and the test pins the one WITHOUT it.
#[test]
fn the_tick_window_excludes_the_print_the_fill_lands_on() {
    let size = 900.0;
    // A violently different final print, so "included" and "excluded" cannot coincide by accident.
    let (fill_px, fill_sz) = (140.0f64, 5.0f64);
    let mut with_it = TickWindow::with_capacity(WINDOW);
    for (px, sz) in PRINTS {
        with_it.push(px, sz);
    }
    with_it.push(fill_px, fill_sz);
    let included = with_it.stats().expect("measurable");
    let excluded = expected_context();

    let charge = |ctx: vike_backtest::MarketStats| {
        AlmgrenChriss::default().impact_frac_for(
            &ImpactInputs { qty: size, avg_volume: ctx.avg_volume, sigma: ctx.sigma },
            ImpactTerms::Both,
        )
    };
    let want = adverse_fill_price(fill_px, 1, charge(excluded));
    let lookahead = adverse_fill_price(fill_px, 1, charge(included));
    assert_ne!(want.to_bits(), lookahead.to_bits(), "the fixture must separate the two answers");

    // prints, the submitting quote, then the print the order fills ON
    let mut ticks: Vec<Tick> =
        PRINTS.iter().enumerate().map(|(i, (px, sz))| tr(i as i64 + 1, *px, *sz)).collect();
    ticks.push(q(100, BID, ASK));
    ticks.push(tr(101, fill_px, fill_sz));

    let armed = tick_entry_price(
        BuyOnFirstQuote { side: 1, size, done: false },
        Some(Arc::new(AlmgrenChriss::default())),
        0.0,
        FillModelKind::Tick,
        ticks,
    );
    assert_eq!(
        armed.to_bits(),
        want.to_bits(),
        "the window must hold every print that CLOSED before the filling tick, and no more"
    );
}

// --- 2. the L2 lane is charged the PERMANENT term only -----------------------------------------

fn book_tick(ts: i64) -> Tick {
    Tick::Book(BookUpdate {
        ts,
        local_ts: 0,
        seq: 1,
        kind: BookUpdateKind::Snapshot,
        tick_size: 0.25,
        // deep enough that the walk always covers the test size
        bids: vec![(101.0, 400.0), (100.5, 400.0), (100.0, 4_000.0)],
        asks: vec![(102.0, 400.0), (102.5, 400.0), (103.0, 4_000.0)],
        symbol: SYM.to_string(),
    })
}

/// The same tape, plus a book snapshot before the quotes so the L2 tier has a ladder to walk.
fn book_tape() -> Vec<Tick> {
    let mut ticks: Vec<Tick> =
        PRINTS.iter().enumerate().map(|(i, (px, sz))| tr(i as i64 + 1, *px, *sz)).collect();
    ticks.push(book_tick(99));
    ticks.push(q(100, BID, ASK));
    ticks.push(q(101, BID, ASK));
    ticks
}

/// The walk price the L2 tier will produce for `size` on `side`, from the same snapshot the tape
/// carries — the shared law (`vike_model::book_taker_price`), not a second copy of the arithmetic.
fn walk_price(side: i32, size: f64) -> f64 {
    let mut b = L2Book::new(0.25);
    b.apply_snapshot(
        1,
        &[(101.0, 400.0), (100.5, 400.0), (100.0, 4_000.0)],
        &[(102.0, 400.0), (102.5, 400.0), (103.0, 4_000.0)],
    );
    book_taker_price(&b, side, size, None).expect("the fixture book must cover the test size")
}

/// THE anti-double-count gate. A taker that WALKED the replayed book pays the permanent term and
/// nothing else — because the walk it just paid IS the temporary concession, measured off real
/// displayed depth instead of estimated.
///
/// Pinned from both sides: the fill must equal the permanent-only price, AND the both-terms price
/// must be a different, strictly worse number. So this fails if the L2 lane is left uncharged
/// (permanent > 0) and it fails again if the L2 lane is wired to `ImpactTerms::Both`.
#[test]
fn an_l2_walk_is_charged_the_permanent_term_and_not_the_temporary_one() {
    let size = 900.0;
    let ctx = expected_context();
    let model = AlmgrenChriss::default();
    let inputs = ImpactInputs { qty: size, avg_volume: ctx.avg_volume, sigma: ctx.sigma };
    let permanent = model.impact_frac_for(&inputs, ImpactTerms::PermanentOnly);
    let both = model.impact_frac_for(&inputs, ImpactTerms::Both);
    assert!(permanent > 0.0 && permanent < both, "{permanent} must sit in (0, {both})");

    let raw = walk_price(1, size);
    assert!(raw > ASK, "the walk must be strictly worse than the top of book, got {raw}");

    let armed = tick_entry_price(
        BuyOnFirstQuote { side: 1, size, done: false },
        Some(Arc::new(AlmgrenChriss::default())),
        0.0,
        FillModelKind::L2Book,
        book_tape(),
    );
    assert_eq!(
        armed.to_bits(),
        adverse_fill_price(raw, 1, permanent).to_bits(),
        "the L2 lane did not charge exactly the permanent term"
    );
    assert_ne!(
        armed.to_bits(),
        adverse_fill_price(raw, 1, both).to_bits(),
        "the L2 lane charged the temporary term the book walk had already paid"
    );
}

/// ...and the L2 charge is genuinely SMALLER than the same order would pay on a bookless tape of
/// the same prints — the double-count claim stated as the comparison an operator would make.
/// Compared as FRACTIONS of each lane's own raw price, because the two lanes start from different
/// raw prices (a walk vs the top of book) and comparing the fills directly would confound the two
/// effects.
#[test]
fn walking_a_book_costs_less_model_than_pricing_off_the_quote() {
    let size = 900.0;
    let l2 = tick_entry_price(
        BuyOnFirstQuote { side: 1, size, done: false },
        Some(Arc::new(AlmgrenChriss::default())),
        0.0,
        FillModelKind::L2Book,
        book_tape(),
    );
    let l1 = tick_entry_price(
        BuyOnFirstQuote { side: 1, size, done: false },
        Some(Arc::new(AlmgrenChriss::default())),
        0.0,
        FillModelKind::Tick,
        tape(),
    );
    let l2_frac = l2 / walk_price(1, size) - 1.0;
    let l1_frac = l1 / ASK - 1.0;
    assert!(l2_frac > 0.0, "the L2 lane must still be charged something: {l2_frac}");
    assert!(
        l2_frac < l1_frac,
        "a lane that already walked real depth must be charged LESS model: {l2_frac} vs {l1_frac}"
    );
}

/// An `L2Book` run on a tape with NO book event degrades to the L1 tier — and is therefore charged
/// BOTH terms, because on that event nothing walked anything. The lane split keys off what
/// actually happened, not off the configured `fill_model` string.
#[test]
fn an_l2_run_with_no_book_is_charged_like_the_tick_lane() {
    let size = 900.0;
    let bookless = tick_entry_price(
        BuyOnFirstQuote { side: 1, size, done: false },
        Some(Arc::new(AlmgrenChriss::default())),
        0.0,
        FillModelKind::L2Book,
        tape(), // no `Tick::Book` at all
    );
    let l1 = tick_entry_price(
        BuyOnFirstQuote { side: 1, size, done: false },
        Some(Arc::new(AlmgrenChriss::default())),
        0.0,
        FillModelKind::Tick,
        tape(),
    );
    assert_eq!(
        bookless.to_bits(),
        l1.to_bits(),
        "a bookless L2 run is the L1 lane, charge included"
    );
}

/// The opt-in absent leaves the L2 lane byte-identical: the fill is the bare walk.
#[test]
fn an_absent_model_leaves_the_l2_lane_byte_identical() {
    let size = 900.0;
    let px = tick_entry_price(
        BuyOnFirstQuote { side: 1, size, done: false },
        None,
        0.0,
        FillModelKind::L2Book,
        book_tape(),
    );
    assert_eq!(px.to_bits(), walk_price(1, size).to_bits(), "the frozen L2 walk moved");
}

// --- 4. the maker exemption is LANE-shaped, and on these lanes it does not apply ---------------

/// A `Limit` on the L1 tick lane CROSSED THE SPREAD, so it pays the model — and the assertion is
/// the strongest form of that claim: it fills at the identical price a MARKET order of the same
/// size pays, charge included. That equality is what closes the avoidance hole. The order is
/// classified `is_maker` (`dispatch_fill` sets that from `OrderKind::Limit`), so an exemption read
/// off that flag would let anyone escape the whole model by respelling a taker as a marketable
/// limit one tick through the touch.
///
/// The fixture makes the crossing explicit rather than assuming it: the limit rests at 103.0 while
/// the ask is 102.0, and `TickFillModel`'s single-quote arm fills a buy limit whenever
/// `ask <= price`, AT THE ASK, for any size. So the fill is 1.0 BELOW its own limit and consumed
/// the touch — the fill an earlier revision of this file described as "passive".
#[test]
fn a_marketable_limit_pays_the_same_impact_as_a_market_order_on_the_tick_lane() {
    let size = 900.0;
    let model: Option<Arc<dyn ImpactModel>> = Some(Arc::new(AlmgrenChriss::default()));
    let limit = |impact: Option<Arc<dyn ImpactModel>>| {
        tick_entry_price(
            RestLimitOnFirstQuote { side: 1, size, price: 103.0, done: false },
            impact,
            0.0,
            FillModelKind::Tick,
            tape(),
        )
    };
    let flat = limit(None);
    assert_eq!(
        flat.to_bits(),
        ASK.to_bits(),
        "the fixture must CROSS: the fill is the ask, not 103"
    );
    assert!(flat < 103.0, "the fill is below its own limit — it took the touch, it did not rest");

    let armed = limit(model.clone());
    assert!(armed > flat, "a spread-crossing limit was exempted from the model it cannot avoid");

    let taker_armed = tick_entry_price(
        BuyOnFirstQuote { side: 1, size, done: false },
        model,
        0.0,
        FillModelKind::Tick,
        tape(),
    );
    assert_eq!(
        armed.to_bits(),
        taker_armed.to_bits(),
        "respelling a taker as a marketable limit changed what it paid"
    );
}

/// The same claim on the L2 lane, where a `Limit` is priced by `book_taker_price` capped at its
/// own price — a WALK down real resting levels. The fixture pins that too: the fill price is the
/// three-level walk average, strictly worse than the touch, with room left under the limit.
///
/// The walk itself predates this model and is not what is being tested; what is tested is that the
/// PERMANENT term is charged on top of it, exactly as it is for a market order of the same size on
/// the same tape.
#[test]
fn a_marketable_limit_pays_the_same_impact_as_a_market_order_on_the_l2_lane() {
    let size = 900.0;
    let model: Option<Arc<dyn ImpactModel>> = Some(Arc::new(AlmgrenChriss::default()));
    let limit = |impact: Option<Arc<dyn ImpactModel>>| {
        tick_entry_price(
            RestLimitOnFirstQuote { side: 1, size, price: 103.0, done: false },
            impact,
            0.0,
            FillModelKind::L2Book,
            book_tape(),
        )
    };
    let flat = limit(None);
    let walked = walk_price(1, size);
    assert_eq!(flat.to_bits(), walked.to_bits(), "the fixture must WALK: the fill is the ladder");
    assert!(
        walked > ASK && walked < 103.0,
        "the walk must cross the touch and stay under the limit"
    );

    let armed = limit(model.clone());
    assert!(armed > flat, "a book-walking limit was exempted from the model it cannot avoid");

    let taker_armed = tick_entry_price(
        BuyOnFirstQuote { side: 1, size, done: false },
        model,
        0.0,
        FillModelKind::L2Book,
        book_tape(),
    );
    assert_eq!(
        armed.to_bits(),
        taker_armed.to_bits(),
        "respelling a taker as a marketable limit changed what it paid"
    );
}

/// ...and the exemption that DOES survive: the BAR lane, where `vike_model::order_fill_price`
/// really does return `price.min(bar.open)` — at-or-better than the limit, because the market came
/// down to a resting order. Charging there would execute the fill through its own price.
///
/// This test lives here rather than beside the bar-lane suite because it is the OTHER HALF of the
/// claim the two tests above make: the exemption was not deleted, it was moved onto the one lane
/// whose price law makes it true. `tests/fills/impact_slippage.rs`'s
/// `a_passive_maker_fill_is_never_charged_impact` gates the bar lane's own arithmetic; what is
/// gated here is that the lane split did not disturb it.
#[test]
fn the_bar_lane_maker_exemption_survives_the_lane_split() {
    /// A zig-zag close (so the measured `sigma` is strictly positive) inside a WIDE bar range, so
    /// a limit well under every open is reachable on some bar and fills at itself whichever bar
    /// reaches it. `volume = 2_000` against a 900-lot is a ~45% participation rate: the model
    /// charges hundreds of bp here if it is consulted at all, which is what makes the equality
    /// below a refusal rather than a rounding coincidence.
    fn series() -> Vec<Bar> {
        (0..40)
            .map(|i| {
                let c = if i % 2 == 0 { 100.0 } else { 101.0 };
                Bar {
                    ts: i as i64 * 60_000,
                    open: c,
                    high: c + 5.0,
                    low: c - 5.0,
                    close: c,
                    volume: 2_000.0,
                    funding: None,
                    bid: None,
                    ask: None,
                    symbol: None,
                }
            })
            .collect()
    }

    const SIZE: f64 = 900.0;
    const LIMIT: f64 = 96.0; // <= (100 - 5) on the even bars, and far below every open

    struct RestLimitAt {
        at: usize,
        done: bool,
    }
    impl Strategy<SimBroker> for RestLimitAt {
        fn on_bar(&mut self, ctx: &mut SimBroker, _b: &Bar) {
            if ctx.index == self.at && !self.done {
                self.done = true;
                ctx.submit_limit(SYM, 1, SIZE, LIMIT, 0.0, true, None);
            }
        }
    }
    struct BuyAt {
        at: usize,
        done: bool,
    }
    impl Strategy<SimBroker> for BuyAt {
        fn on_bar(&mut self, ctx: &mut SimBroker, _b: &Bar) {
            if ctx.index == self.at && !self.done {
                self.done = true;
                ctx.submit(SYM, 1, SIZE, 0.0, true, None);
            }
        }
    }

    fn entry<S: Strategy<SimBroker>>(strategy: S, impact: Option<Arc<dyn ImpactModel>>) -> f64 {
        let params = EngineParams {
            cash: 100_000_000.0,
            impact,
            impact_window: 20,
            ..Default::default() // fill_model = Bar — the lane whose exemption is true
        };
        let mut e = StrategyEngine::new(vec![(SYM.to_string(), series())], strategy, params);
        e.run();
        let si = e.core.symbols.iter().position(|s| s == SYM).expect("symbol registered");
        let pos = e.core.sym[si].pos;
        assert!(pos.size != 0.0, "the test order must have filled");
        pos.avg_price
    }

    let flat = entry(RestLimitAt { at: 20, done: false }, None);
    assert_eq!(flat.to_bits(), LIMIT.to_bits(), "the fixture must fill AT its own limit");
    assert_eq!(
        entry(RestLimitAt { at: 20, done: false }, Some(Arc::new(AlmgrenChriss::default())))
            .to_bits(),
        flat.to_bits(),
        "the bar lane's maker exemption was lost to the lane split"
    );

    // The control the exemption is worthless without: the SAME size taken aggressively on the
    // SAME series must move, or the equality above is the model quietly measuring nothing.
    let taker_flat = entry(BuyAt { at: 20, done: false }, None);
    let taker_armed =
        entry(BuyAt { at: 20, done: false }, Some(Arc::new(AlmgrenChriss::default())));
    assert!(
        taker_armed > taker_flat,
        "the bar-lane taker control did not move ({taker_armed} <= {taker_flat}) — the exemption \
         above proves nothing"
    );
}

// --- the arming is scoped to the replay --------------------------------------------------------

/// Watches `SimBroker::impact_prints` from inside the replay, so the arming can be asserted while
/// it is live rather than inferred from a fill.
#[derive(Default)]
struct PrintCountSpy {
    seen: Rc<RefCell<Vec<Option<usize>>>>,
}

impl Strategy<SimBroker> for PrintCountSpy {
    fn on_quote_tick(&mut self, ctx: &mut SimBroker, _t: &QuoteTick) {
        self.seen.borrow_mut().push(ctx.impact_prints(SYM));
    }
}

/// The tick window is ARMED for the replay and dropped with it — the same discipline as the
/// latency gate, the shadow position and the replay books.
///
/// Both halves matter. Armed: an operator whose tape carries no trade prints must be able to tell
/// "the model was off" from "the model had nothing to measure", which is what `impact_prints`
/// answers. Dropped: a window left installed would let a later BAR run price its fills off the
/// previous tape's PRINTS instead of its own bars — the one route by which this extension could
/// reach a lane it was never wired into.
#[test]
fn the_tick_window_is_armed_for_the_replay_and_dropped_with_it() {
    let spy = PrintCountSpy::default();
    let seen = Rc::clone(&spy.seen);
    let params = EngineParams {
        cash: 100_000_000.0,
        impact: Some(Arc::new(AlmgrenChriss::default())),
        impact_window: WINDOW,
        fill_model: FillModelKind::Tick,
        ..Default::default()
    };
    let mut engine = StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], spy, params);
    engine.run_ticks(&[(SYM.to_string(), tape())]);
    assert_eq!(
        seen.borrow().as_slice(),
        [Some(PRINTS.len()), Some(PRINTS.len())].as_slice(),
        "the replay must expose the print count it is actually measuring"
    );
    assert_eq!(
        engine.core.impact_prints(SYM),
        None,
        "the replay left its tick-lane market context installed on the broker"
    );
}

/// ...and with NO model configured nothing is armed at all, so a default replay allocates no
/// window and reports none. This is the byte-identity claim in its structural form.
#[test]
fn an_unconfigured_replay_arms_no_tick_window() {
    let spy = PrintCountSpy::default();
    let seen = Rc::clone(&spy.seen);
    let mut engine = StrategyEngine::new(
        vec![(SYM.to_string(), Vec::new())],
        spy,
        EngineParams { cash: 100_000_000.0, ..Default::default() },
    );
    engine.run_ticks(&[(SYM.to_string(), tape())]);
    let want: [Option<usize>; 2] = [None, None];
    assert_eq!(seen.borrow().as_slice(), want.as_slice(), "an unconfigured replay armed a window");
}
