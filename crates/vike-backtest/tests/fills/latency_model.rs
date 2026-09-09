//! Order-latency gating through `run_ticks` (steal-list: hftbacktest's two-leg order-latency
//! mechanism, reimplemented in `vike_backtest::latency`).
//!
//! The headline proof is the A/B on a maker's reflexes: with ZERO latency a strategy can cancel
//! its resting limit on the very tick that would have filled it and always escape; with a
//! non-zero ENTRY latency that same cancel arrives at the matching engine too late and the fill
//! happens anyway. Also covered end to end:
//!
//! (a) `latency_model: None` (the default) is deterministic AND pinned to hard-coded expected
//!     values, so a regression in the frozen zero-latency path fails HERE and not only in the
//!     distant golden fixtures — plus the documented one-tick difference against `constant(0,0)`;
//! (b) the too-late cancel A/B above (the reflex fix), on the untagged `pending` lane;
//! (c) a NEW order is not matchable until `entry()` has elapsed — a price touch inside the
//!     in-flight window does not fill it, the next touch after delivery does;
//! (d) the response leg delays `Strategy::on_fill` delivery without changing the fill itself,
//!     AND with it the strategy-visible shadow inventory (`HftBroker::position`) — the read
//!     `SpreadMaker` steers on;
//! (e) a negative entry latency (the recorded-rejection encoding) drops the order outright and
//!     records it in `dropped` under `"latency_reject"`;
//! (f) the tagged (HFT maker) lane is gated the same way, `modify_tagged` included;
//! (g) fills still in flight at the end of the tape are FLUSHED to the strategy before
//!     `on_stop`, so nothing is silently swallowed;
//! (h) the pre-trade leverage cap counts orders still IN FLIGHT — without that every submit
//!     inside one entry-latency window is granted the full leverage room;
//! (i) BOOK ticks advance the in-flight clock: a delivery stamp falling in the middle of a
//!     book-only stretch of tape lands there, rather than freezing until the next quote.

use std::cell::RefCell;
use std::rc::Rc;

use vike_backtest::engine::{EngineParams, SimBroker, StrategyEngine, Tick};
use vike_backtest::{LatencyModelKind, LatencyRow};
use vike_model::{
    BookUpdate, BookUpdateKind, Fill, HftBroker, L2Book, QuoteTick, Strategy, TradeTick,
};

const SYM: &str = "TOK";

/// `(delivery_ts, size, price)` per `on_fill`, in DELIVERY order.
type FillLog = Rc<RefCell<Vec<(i64, f64, f64)>>>;

fn q(ts: i64, bid: f64, ask: f64) -> Tick {
    Tick::Quote(QuoteTick {
        ts,
        local_ts: 0,
        bid,
        ask,
        bid_size: 100.0,
        ask_size: 100.0,
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

fn params(latency: Option<LatencyModelKind>) -> EngineParams {
    EngineParams { cash: 10_000.0, latency_model: latency, ..Default::default() }
}

// --------------------------------------------------------------------------------------------
// The maker under test: rest a buy limit at `px` on the FIRST tick, then cancel it on the tick
// stamped `cancel_at` — the classic "pull the quote as the market comes to me" reflex.
// --------------------------------------------------------------------------------------------

struct ReflexMaker {
    px: f64,
    cancel_at: i64,
    submitted: bool,
    canceled: bool,
    log: FillLog,
    /// `on_fill` timestamps recorded against the engine clock AT DELIVERY (`ctx.now`), which is
    /// what makes the response leg observable.
    delivered_at: Rc<RefCell<Vec<i64>>>,
}

impl ReflexMaker {
    fn new(px: f64, cancel_at: i64, log: FillLog, delivered_at: Rc<RefCell<Vec<i64>>>) -> Self {
        ReflexMaker { px, cancel_at, submitted: false, canceled: false, log, delivered_at }
    }

    fn step(&mut self, ctx: &mut SimBroker, ts: i64) {
        if !self.submitted {
            self.submitted = true;
            ctx.submit_limit(SYM, 1, 1.0, self.px, 0.0, true, None);
        }
        if !self.canceled && ts >= self.cancel_at {
            self.canceled = true;
            ctx.cancel_all(SYM);
        }
    }
}

impl Strategy<SimBroker> for ReflexMaker {
    fn on_quote_tick(&mut self, ctx: &mut SimBroker, t: &QuoteTick) {
        self.step(ctx, t.ts);
    }

    fn on_trade_tick(&mut self, ctx: &mut SimBroker, t: &TradeTick) {
        self.step(ctx, t.ts);
    }

    fn on_fill(&mut self, ctx: &mut SimBroker, f: &Fill) {
        self.log.borrow_mut().push((f.ts, f.size, f.price));
        self.delivered_at.borrow_mut().push(ctx.now);
    }
}

/// The tape: a quote well above the limit, then a trade that touches it (the fill trigger),
/// then two more quotes so any delayed action still has ticks to land on.
fn reflex_tape() -> Vec<(String, Vec<Tick>)> {
    vec![(
        SYM.to_string(),
        vec![
            q(1_000, 100.0, 100.02), // t0: the maker rests its bid at 99.00
            q(1_010, 99.50, 99.52),  // t1: market walking down; the cancel fires here
            tr(1_020, 99.00, 5.0),   // t2: touches 99.00 — fills unless the cancel landed
            q(1_030, 98.90, 98.92),
            q(1_100, 98.90, 98.92),
        ],
    )]
}

fn run_reflex(
    latency: Option<LatencyModelKind>,
    cancel_at: i64,
) -> (Vec<(i64, f64, f64)>, Vec<i64>, f64) {
    let log: FillLog = Rc::new(RefCell::new(Vec::new()));
    let delivered: Rc<RefCell<Vec<i64>>> = Rc::new(RefCell::new(Vec::new()));
    let strat = ReflexMaker::new(99.00, cancel_at, log.clone(), delivered.clone());
    let mut e = StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], strat, params(latency));
    let res = e.run_ticks(&reflex_tape());
    let fills = log.borrow().clone();
    let at = delivered.borrow().clone();
    (fills, at, res.final_equity)
}

// --------------------------------------------------------------------------------------------
// (a) None is the frozen path
// --------------------------------------------------------------------------------------------

#[test]
fn none_is_deterministic_and_pinned_to_the_frozen_values() {
    // Two runs of the identical tape with `latency_model: None` must agree exactly...
    let (a_fills, _, a_eq) = run_reflex(None, 1_010);
    let (b_fills, _, b_eq) = run_reflex(None, 1_010);
    assert_eq!(a_fills, b_fills);
    assert_eq!(a_eq.to_bits(), b_eq.to_bits());
    // ...and agree with HARD-CODED expected values, which is what makes this a parity pin and not
    // merely a determinism check: the maker that cancels BEFORE the touch escapes the fill (the
    // pre-latency reflex) and the account ends untouched at its starting cash, to the bit.
    assert!(a_fills.is_empty(), "zero-latency cancel at t=1010 escapes the t=1020 touch");
    assert_eq!(a_eq.to_bits(), 10_000.0f64.to_bits(), "no fill => starting cash, exactly");
}

#[test]
fn constant_zero_differs_from_none_only_by_the_documented_one_tick_drain() {
    // `constant(0, 0)` is NOT the same thing as `None`: it still builds the in-flight queue, so an
    // action submitted on the tick at `T` only applies at the NEXT drain (the next tick stamped
    // `>= T`). That is the ONE documented difference — pin it in both directions.
    //
    // On the reflex tape it is invisible in the OUTCOME: the submit at t=1000 lands at t=1010 and
    // the cancel issued at t=1010 lands at t=1020, still before that tick's fill phase, so the
    // order is pulled in time either way.
    let (none_fills, _, none_eq) = run_reflex(None, 1_010);
    let (zero_fills, _, zero_eq) = run_reflex(Some(LatencyModelKind::constant(0, 0)), 1_010);
    assert_eq!(none_fills, zero_fills);
    assert_eq!(none_eq.to_bits(), zero_eq.to_bits());

    // The ORDER leg can never diverge on outcomes at zero latency, and that is worth stating
    // precisely: a strategy submits from a callback that runs AFTER its tick's fill phase, and the
    // next tick's drain runs BEFORE that tick's fill phase, so the order is resting by the first
    // moment it could possibly match either way.
    //
    // The RESPONSE leg is where the extra tick shows: a fill matched at T is deferred with
    // `visible_ns == T`, but T's `deliver_due_fills` has already run by then, so it reaches the
    // strategy at T+1 instead of synchronously. Pin exactly that — same fill, later delivery.
    let (n_fills, n_at, _) = run_reflex(None, i64::MAX);
    let (z_fills, z_at, _) = run_reflex(Some(LatencyModelKind::constant(0, 0)), i64::MAX);
    assert_eq!(n_fills, z_fills, "the fill itself is identical");
    assert_eq!(n_fills.len(), 1);
    assert_eq!(n_fills[0], (1_020, 1.0, 99.00));
    assert_eq!(n_at, vec![1_020], "None: delivered synchronously on the matching tick");
    assert_eq!(z_at, vec![1_030], "constant(0,0): delivered on the next tick's drain");
}

// --------------------------------------------------------------------------------------------
// (b) THE HEADLINE: a cancel issued too late no longer saves the order
// --------------------------------------------------------------------------------------------

#[test]
fn a_too_late_cancel_no_longer_prevents_the_fill() {
    // Control: zero latency, cancel issued on the touching tick itself (t=1020). The strategy's
    // `on_trade_tick` runs AFTER the fill phase, so at zero latency this is the LAST tick it
    // could still have escaped on — establish the baseline on the tick before instead.
    let (zero, _, _) = run_reflex(None, 1_010);
    assert!(zero.is_empty(), "control: an instant cancel at t=1010 saves the order");

    // Same strategy, same tape — but a 15ms entry latency. The cancel issued at t=1010 is only
    // delivered at t=1025, AFTER the t=1020 trade has already matched the resting bid.
    let (late, _, _) = run_reflex(Some(LatencyModelKind::constant(15_000_000, 0)), 1_010);
    assert_eq!(late.len(), 1, "the cancel arrived too late — the fill stands");
    assert_eq!(late[0].2, 99.00);

    // And the mechanism is genuinely the DELAY, not the gate merely dropping cancels: with an
    // entry latency short enough to land before the touch (5ms → delivered at t=1015), the very
    // same cancel saves the order again.
    let (in_time, _, _) = run_reflex(Some(LatencyModelKind::constant(5_000_000, 0)), 1_010);
    assert!(in_time.is_empty(), "a cancel that arrives in time still works");
}

// --------------------------------------------------------------------------------------------
// (c) a new order is not matchable inside its in-flight window
// --------------------------------------------------------------------------------------------

/// Rests a buy limit ONCE at `px` on the first tick and never cancels.
struct RestOnce {
    px: f64,
    submitted: bool,
    log: FillLog,
}

impl Strategy<SimBroker> for RestOnce {
    fn on_quote_tick(&mut self, ctx: &mut SimBroker, _t: &QuoteTick) {
        if !self.submitted {
            self.submitted = true;
            ctx.submit_limit(SYM, 1, 1.0, self.px, 0.0, true, None);
        }
    }

    fn on_trade_tick(&mut self, ctx: &mut SimBroker, _t: &TradeTick) {
        if !self.submitted {
            self.submitted = true;
            ctx.submit_limit(SYM, 1, 1.0, self.px, 0.0, true, None);
        }
    }

    fn on_fill(&mut self, _ctx: &mut SimBroker, f: &Fill) {
        self.log.borrow_mut().push((f.ts, f.size, f.price));
    }
}

fn run_rest_once(latency: Option<LatencyModelKind>, ticks: Vec<Tick>) -> Vec<(i64, f64, f64)> {
    let log: FillLog = Rc::new(RefCell::new(Vec::new()));
    let strat = RestOnce { px: 99.00, submitted: false, log: log.clone() };
    let mut e = StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], strat, params(latency));
    e.run_ticks(&[(SYM.to_string(), ticks)]);

    log.borrow().clone()
}

#[test]
fn a_new_order_is_not_matchable_until_entry_latency_elapses() {
    // Submitted at t=1000; a touch at t=1005 is INSIDE the 20ms in-flight window, the touch at
    // t=1030 is after delivery (t=1020).
    let ticks = vec![
        q(1_000, 100.0, 100.02),
        tr(1_005, 99.00, 5.0), // too early — the order has not reached the venue
        tr(1_030, 99.00, 5.0), // now it is resting
        q(1_040, 98.9, 98.92),
    ];
    let zero = run_rest_once(None, ticks.clone());
    assert_eq!(zero.len(), 1, "zero latency: the FIRST touch fills it");
    assert_eq!(zero[0].0, 1_005);

    let laggy = run_rest_once(Some(LatencyModelKind::constant(20_000_000, 0)), ticks);
    assert_eq!(laggy.len(), 1, "20ms latency: the first touch misses, the second fills");
    assert_eq!(laggy[0].0, 1_030, "filled by the post-delivery touch, not the early one");
}

// --------------------------------------------------------------------------------------------
// (e) a negative entry latency drops the action
// --------------------------------------------------------------------------------------------

#[test]
fn a_recorded_rejection_drops_the_order_entirely() {
    let ticks = vec![
        q(1_000, 100.0, 100.02),
        tr(1_030, 99.00, 5.0),
        tr(1_060, 99.00, 5.0),
        q(1_090, 98.9, 98.92),
    ];
    // A rejection row (`exch_ts <= 0`) reports a NEGATIVE entry latency: the request never
    // reached the matching engine, so no order ever rests and nothing can fill.
    let rejecting = LatencyModelKind::intp(vec![LatencyRow::new(0, 0, 4_000_000)]);
    assert!(run_rest_once(Some(rejecting), ticks.clone()).is_empty());
    // Control: the same tape with an ACCEPTING recorded row does fill.
    let accepting = LatencyModelKind::intp(vec![LatencyRow::new(0, 1_000_000, 2_000_000)]);
    assert_eq!(run_rest_once(Some(accepting), ticks).len(), 1);
}

// --------------------------------------------------------------------------------------------
// (d) + (g) the response leg
// --------------------------------------------------------------------------------------------

#[test]
fn the_response_leg_delays_fill_delivery_without_changing_the_fill() {
    // Entry 0 so the order rests immediately; response 25ms so the t=1030 fill is only handed
    // to `on_fill` at the first tick stamped >= 1055.
    let log: FillLog = Rc::new(RefCell::new(Vec::new()));
    let delivered: Rc<RefCell<Vec<i64>>> = Rc::new(RefCell::new(Vec::new()));
    let strat = ReflexMaker::new(99.00, i64::MAX, log.clone(), delivered.clone());
    let ticks = vec![
        q(1_000, 100.0, 100.02),
        tr(1_030, 99.00, 5.0), // fills here...
        q(1_040, 98.9, 98.92), // ...not yet visible (1030 + 25 = 1055)
        q(1_060, 98.9, 98.92), // ...delivered here
    ];
    let mut e = StrategyEngine::new(
        vec![(SYM.to_string(), Vec::new())],
        strat,
        params(Some(LatencyModelKind::constant(0, 25_000_000))),
    );
    e.run_ticks(&[(SYM.to_string(), ticks)]);
    let fills = log.borrow().clone();
    assert_eq!(fills.len(), 1);
    // The FILL itself is untouched — it happened at t=1030 at the limit price.
    assert_eq!(fills[0].0, 1_030);
    assert_eq!(fills[0].2, 99.00);
    // ...but the strategy only learned about it at t=1060, the first tick past 1055.
    assert_eq!(delivered.borrow().clone(), vec![1_060]);
}

#[test]
fn a_fill_still_in_flight_at_the_end_of_the_tape_is_flushed() {
    // Response latency far longer than the remaining tape: without the end-of-run flush the
    // strategy would never be told about its own fill.
    let log: FillLog = Rc::new(RefCell::new(Vec::new()));
    let delivered: Rc<RefCell<Vec<i64>>> = Rc::new(RefCell::new(Vec::new()));
    let strat = ReflexMaker::new(99.00, i64::MAX, log.clone(), delivered.clone());
    let ticks = vec![q(1_000, 100.0, 100.02), tr(1_030, 99.00, 5.0)];
    let mut e = StrategyEngine::new(
        vec![(SYM.to_string(), Vec::new())],
        strat,
        params(Some(LatencyModelKind::constant(0, 60_000_000_000))),
    );
    e.run_ticks(&[(SYM.to_string(), ticks)]);
    assert_eq!(log.borrow().len(), 1, "the held fill is flushed before on_stop");
}

// --------------------------------------------------------------------------------------------
// (f) the tagged (HFT maker) lane
// --------------------------------------------------------------------------------------------

/// Rests a tagged buy quote on the first tick, then pulls it at `cancel_at`.
struct TaggedReflex {
    px: f64,
    cancel_at: i64,
    submitted: bool,
    canceled: bool,
    log: FillLog,
}

impl TaggedReflex {
    fn step(&mut self, ctx: &mut SimBroker, ts: i64) {
        if !self.submitted {
            self.submitted = true;
            ctx.submit_limit_tagged("b0", 1, 1.0, self.px);
        }
        if !self.canceled && ts >= self.cancel_at {
            self.canceled = true;
            ctx.cancel_tagged("b0");
        }
    }
}

impl Strategy<SimBroker> for TaggedReflex {
    fn on_quote_tick(&mut self, ctx: &mut SimBroker, t: &QuoteTick) {
        self.step(ctx, t.ts);
    }

    fn on_trade_tick(&mut self, ctx: &mut SimBroker, t: &TradeTick) {
        self.step(ctx, t.ts);
    }

    fn on_fill(&mut self, _ctx: &mut SimBroker, f: &Fill) {
        self.log.borrow_mut().push((f.ts, f.size, f.price));
    }
}

fn run_tagged(latency: Option<LatencyModelKind>, cancel_at: i64) -> Vec<(i64, f64, f64)> {
    let log: FillLog = Rc::new(RefCell::new(Vec::new()));
    let strat =
        TaggedReflex { px: 99.00, cancel_at, submitted: false, canceled: false, log: log.clone() };
    let mut e = StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], strat, params(latency));
    e.run_ticks(&reflex_tape());

    log.borrow().clone()
}

#[test]
fn the_tagged_maker_lane_is_gated_the_same_way() {
    assert!(run_tagged(None, 1_010).is_empty(), "control: the instant pull works");
    let late = run_tagged(Some(LatencyModelKind::constant(15_000_000, 0)), 1_010);
    assert_eq!(late.len(), 1, "the tagged pull arrived too late — the maker got filled");
    assert_eq!(late[0].2, 99.00);
}

/// Rests a tagged buy quote at `px` on the first tick, then RE-PRICES it (never cancels) at
/// `modify_at` — the requote-in-place path, whose in-flight arm is a hand-copied duplicate of the
/// immediate body and was previously untested.
struct TaggedAmender {
    px: f64,
    new_px: f64,
    modify_at: i64,
    submitted: bool,
    modified: bool,
    log: FillLog,
}

impl TaggedAmender {
    fn step(&mut self, ctx: &mut SimBroker, ts: i64) {
        if !self.submitted {
            self.submitted = true;
            ctx.submit_limit_tagged("b0", 1, 1.0, self.px);
        }
        if !self.modified && ts >= self.modify_at {
            self.modified = true;
            ctx.modify_tagged("b0", None, Some(self.new_px));
        }
    }
}

impl Strategy<SimBroker> for TaggedAmender {
    fn on_quote_tick(&mut self, ctx: &mut SimBroker, t: &QuoteTick) {
        self.step(ctx, t.ts);
    }

    fn on_trade_tick(&mut self, ctx: &mut SimBroker, t: &TradeTick) {
        self.step(ctx, t.ts);
    }

    fn on_fill(&mut self, _ctx: &mut SimBroker, f: &Fill) {
        self.log.borrow_mut().push((f.ts, f.size, f.price));
    }
}

#[test]
fn a_tagged_modify_is_held_in_flight_too() {
    // The quote rests at 99.50 and is re-priced DOWN to an out-of-reach 98.00 at t=1010 — the
    // "step away, the market is coming at me" amend. The tape's t=1020 trade prints exactly 99.50,
    // so whether the amend landed in time is the whole difference.
    let ticks = vec![
        q(1_000, 100.0, 100.02), // mid 100.01
        q(1_010, 99.60, 99.62),  // mid 99.61 — above the 99.50 quote, no touch
        tr(1_020, 99.50, 5.0),   // touches 99.50, but NOT 98.00
        q(1_030, 98.9, 98.92),
        q(1_100, 98.9, 98.92),
    ];
    let run = |latency: Option<LatencyModelKind>| {
        let log: FillLog = Rc::new(RefCell::new(Vec::new()));
        let strat = TaggedAmender {
            px: 99.50,
            new_px: 98.00,
            modify_at: 1_010,
            submitted: false,
            modified: false,
            log: log.clone(),
        };
        let mut e =
            StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], strat, params(latency));
        e.run_ticks(&[(SYM.to_string(), ticks.clone())]);

        log.borrow().clone()
    };
    // Control: the amend applies instantly at t=1010, so by the t=1020 print the quote has already
    // stepped down to 98.00 and escapes.
    assert!(run(None).is_empty(), "control: the instant re-price escapes the touch");
    // 15ms entry latency: the amend issued at t=1010 only lands at t=1025, so the t=1020 trade
    // found the quote still at 99.50 and took it. (The original submit, issued at t=1000, was
    // delivered by t=1020's drain — which runs before that tick's fill phase — so it WAS resting;
    // the amend is the only difference.)
    let late = run(Some(LatencyModelKind::constant(15_000_000, 0)));
    assert_eq!(late.len(), 1, "the re-price arrived too late — the stale quote got hit");
    assert_eq!(late[0].2, 99.50, "filled at the ORIGINAL, un-amended price");
}

// --------------------------------------------------------------------------------------------
// (d cont.) the SHADOW inventory — the read `SpreadMaker` actually steers on
// --------------------------------------------------------------------------------------------

/// Rests a buy limit once, then polls BOTH views of its inventory on every tick:
/// `HftBroker::position` (the strategy's view — response-latency shadowed) and
/// `SimBroker::position_of` (exchange truth — never shadowed).
struct InventoryPoller {
    px: f64,
    submitted: bool,
    /// `(ts, hft_position, true_position)` per tick
    seen: Rc<RefCell<Vec<(i64, f64, f64)>>>,
}

impl InventoryPoller {
    fn step(&mut self, ctx: &mut SimBroker, ts: i64) {
        if !self.submitted {
            self.submitted = true;
            ctx.submit_limit(SYM, 1, 1.0, self.px, 0.0, true, None);
        }
        let hft = HftBroker::position(ctx);
        let truth = ctx.position_of(SYM).size;
        self.seen.borrow_mut().push((ts, hft, truth));
    }
}

impl Strategy<SimBroker> for InventoryPoller {
    fn on_quote_tick(&mut self, ctx: &mut SimBroker, t: &QuoteTick) {
        self.step(ctx, t.ts);
    }

    fn on_trade_tick(&mut self, ctx: &mut SimBroker, t: &TradeTick) {
        self.step(ctx, t.ts);
    }
}

#[test]
fn the_response_leg_also_delays_the_strategys_view_of_its_inventory() {
    // THE POINT OF THE SHADOW. `vike-mm`'s `SpreadMaker` never accumulates inventory in `on_fill`
    // — it polls `HftBroker::position` on every requote to drive its skew and its A-S reservation
    // price. Before the shadow existed that poll read `sym[si].pos`, which `apply_fill` has
    // already updated at match time, so the maker reacted to its own fill with ZERO response
    // latency and the whole response leg was inert for the crate's flagship consumer.
    let ticks = vec![
        q(1_000, 100.0, 100.02),
        tr(1_030, 99.00, 5.0), // matches here — exchange truth moves to +1 immediately
        q(1_040, 98.9, 98.92), // ...strategy still sees 0 (1030 + 25 = 1055 not reached)
        q(1_060, 98.9, 98.92), // ...delivered at this tick's drain: strategy now sees +1
    ];
    let seen = Rc::new(RefCell::new(Vec::new()));
    let strat = InventoryPoller { px: 99.00, submitted: false, seen: seen.clone() };
    let mut e = StrategyEngine::new(
        vec![(SYM.to_string(), Vec::new())],
        strat,
        params(Some(LatencyModelKind::constant(0, 25_000_000))),
    );
    e.run_ticks(&[(SYM.to_string(), ticks.clone())]);
    assert_eq!(
        seen.borrow().clone(),
        vec![
            (1_000, 0.0, 0.0),
            (1_030, 0.0, 1.0), // matched this tick: truth +1, the strategy is not told yet
            (1_040, 0.0, 1.0), // still in the response window
            (1_060, 1.0, 1.0), // delivered — the two views agree again
        ]
    );

    // Control: with NO latency model the two views are identical on every tick, which is also the
    // proof that `shadow_pos` is inert (empty) off the opt-in path.
    let seen2 = Rc::new(RefCell::new(Vec::new()));
    let strat2 = InventoryPoller { px: 99.00, submitted: false, seen: seen2.clone() };
    let mut e2 = StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], strat2, params(None));
    e2.run_ticks(&[(SYM.to_string(), ticks)]);
    for (ts, hft, truth) in seen2.borrow().iter() {
        assert_eq!(hft, truth, "unarmed: the shadow does not exist (ts={ts})");
    }
}

// --------------------------------------------------------------------------------------------
// (h) the pre-trade leverage cap must see the orders it just sent
// --------------------------------------------------------------------------------------------

/// Fires a fixed-size MARKET buy on every tick up to and including `until` — four submits inside
/// one 20ms entry-latency window.
struct BurstBuyer {
    size: f64,
    until: i64,
    log: FillLog,
}

impl Strategy<SimBroker> for BurstBuyer {
    fn on_quote_tick(&mut self, ctx: &mut SimBroker, t: &QuoteTick) {
        if t.ts <= self.until {
            ctx.submit(SYM, 1, self.size, 0.0, true, None);
        }
    }

    fn on_fill(&mut self, _ctx: &mut SimBroker, f: &Fill) {
        self.log.borrow_mut().push((f.ts, f.size, f.price));
    }
}

#[test]
fn the_leverage_cap_counts_orders_still_in_flight() {
    // equity 10_000 at price 100 with leverage 1.0 => room for exactly 100 units, TOTAL.
    // The strategy asks for 100 units on each of the four ticks at t=1000..1015, all of which are
    // inside the 20ms entry-latency window, so at the 2nd..4th submit the first order is not in
    // `pending` — it is in flight. If the cap does not look there it grants the full 100-unit room
    // four times over and the account ends at 4x its configured leverage.
    let ticks = vec![
        q(1_000, 100.0, 100.0),
        q(1_005, 100.0, 100.0),
        q(1_010, 100.0, 100.0),
        q(1_015, 100.0, 100.0),
        q(1_020, 100.0, 100.0), // the first order is delivered and fills here
        q(1_040, 100.0, 100.0),
    ];
    let run = |latency: Option<LatencyModelKind>| {
        let log: FillLog = Rc::new(RefCell::new(Vec::new()));
        let strat = BurstBuyer { size: 100.0, until: 1_015, log: log.clone() };
        let p = EngineParams {
            cash: 10_000.0,
            leverage: Some(1.0),
            latency_model: latency,
            ..Default::default()
        };
        let mut e = StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], strat, p);
        e.run_ticks(&[(SYM.to_string(), ticks.clone())]);

        log.borrow().clone()
    };

    // Control (zero latency): the 1st submit rests 100 units in `pending`; the 2nd..4th see that
    // pending notional, get capped to 0 and never become orders.
    let instant = run(None);
    let instant_qty: f64 = instant.iter().map(|f| f.1).sum();
    assert_eq!(instant_qty, 100.0, "control: the cap allows exactly one 100-unit entry");

    // Armed: the same total. Anything more means the cap was computed against stale state.
    let armed = run(Some(LatencyModelKind::constant(20_000_000, 0)));
    let armed_qty: f64 = armed.iter().map(|f| f.1).sum();
    assert_eq!(armed_qty, 100.0, "the cap counted the in-flight order, not just `pending`");
}

// --------------------------------------------------------------------------------------------
// (i) BOOK ticks advance the in-flight clock
// --------------------------------------------------------------------------------------------

fn snap(ts: i64, seq: u64, bid: f64, ask: f64) -> Tick {
    Tick::Book(BookUpdate {
        ts,
        local_ts: 0,
        seq,
        kind: BookUpdateKind::Snapshot,
        tick_size: 0.01,
        bids: vec![(bid, 100.0)],
        asks: vec![(ask, 100.0)],
        symbol: SYM.to_string(),
    })
}

/// Rests a buy limit on the first tick, records what it can see of its own order on every BOOK
/// tick, and records the engine clock at each `on_fill` delivery.
struct BookWatcher {
    px: f64,
    submitted: bool,
    /// `(book_ts, resting_pending, in_flight)` per book tick
    book_seen: Rc<RefCell<Vec<(i64, usize, usize)>>>,
    /// `ctx.now` per delivered fill
    delivered_at: Rc<RefCell<Vec<i64>>>,
}

impl Strategy<SimBroker> for BookWatcher {
    fn on_quote_tick(&mut self, ctx: &mut SimBroker, _t: &QuoteTick) {
        if !self.submitted {
            self.submitted = true;
            ctx.submit_limit(SYM, 1, 1.0, self.px, 0.0, true, None);
        }
    }

    fn on_trade_tick(&mut self, ctx: &mut SimBroker, _t: &TradeTick) {
        if !self.submitted {
            self.submitted = true;
            ctx.submit_limit(SYM, 1, 1.0, self.px, 0.0, true, None);
        }
    }

    fn on_order_book(&mut self, ctx: &mut SimBroker, _book: &L2Book) {
        let seen = (ctx.now, ctx.pending_of(SYM).len(), ctx.in_flight_of(SYM));
        self.book_seen.borrow_mut().push(seen);
    }

    fn on_fill(&mut self, ctx: &mut SimBroker, _f: &Fill) {
        self.delivered_at.borrow_mut().push(ctx.now);
    }
}

#[test]
fn book_ticks_advance_the_in_flight_clock_on_both_legs() {
    // A book-ONLY stretch of tape must not freeze the exchange's view. Both legs are exercised:
    //   ORDER leg  — submitted at t=1000 with a 20ms entry latency, so its delivery stamp (1020)
    //                falls on a BOOK tick. By the t=1020 book it must be resting, not in flight.
    //   RESPONSE leg — the fill matches at t=1030 with a 25ms response latency (visible at 1055),
    //                and the only tick past that stamp before the tape's last quote is a BOOK
    //                tick at t=1060, so that is where `on_fill` must land.
    let ticks = vec![
        q(1_000, 100.0, 100.02),
        snap(1_010, 1, 99.90, 99.92), // still in flight here (1000 + 20 = 1020)
        snap(1_020, 2, 99.80, 99.82), // delivered by THIS book tick's drain
        tr(1_030, 99.00, 5.0),        // matches
        snap(1_040, 3, 98.90, 98.92),
        snap(1_060, 4, 98.90, 98.92), // response visible at 1055 => delivered here
        q(1_200, 98.9, 98.92),
    ];
    let book_seen = Rc::new(RefCell::new(Vec::new()));
    let delivered_at = Rc::new(RefCell::new(Vec::new()));
    let strat = BookWatcher {
        px: 99.00,
        submitted: false,
        book_seen: book_seen.clone(),
        delivered_at: delivered_at.clone(),
    };
    let mut e = StrategyEngine::new(
        vec![(SYM.to_string(), Vec::new())],
        strat,
        params(Some(LatencyModelKind::constant(20_000_000, 25_000_000))),
    );
    e.run_ticks(&[(SYM.to_string(), ticks)]);

    let seen = book_seen.borrow().clone();
    assert_eq!(seen.len(), 4, "one observation per book tick");
    assert_eq!(seen[0], (1_010, 0, 1), "t=1010: still in flight, invisible to `pending_of`");
    assert_eq!(seen[1], (1_020, 1, 0), "t=1020: a BOOK tick delivered it");
    // ...and after the t=1030 match the order is gone from `pending` either way.
    assert_eq!(seen[2].0, 1_040);
    assert_eq!(seen[3].0, 1_060);

    // The response leg landed on the book tick at t=1060, not deferred to the t=1200 quote.
    assert_eq!(delivered_at.borrow().clone(), vec![1_060]);
}

// --------------------------------------------------------------------------------------------
// (e cont.) a rejected action is DIAGNOSABLE, not silently swallowed
// --------------------------------------------------------------------------------------------

/// One dropped-order record as `SimBroker::dropped` reports it: `(symbol, reason, qty, price)`.
/// Named so the shared handle below stays under clippy's `type_complexity` bar.
type DroppedLog = Rc<RefCell<Vec<(String, String, f64, f64)>>>;

/// Submits once, then reports `SimBroker::dropped` out of `on_stop`.
struct RejectWatcher {
    px: f64,
    submitted: bool,
    dropped: DroppedLog,
}

impl Strategy<SimBroker> for RejectWatcher {
    fn on_quote_tick(&mut self, ctx: &mut SimBroker, _t: &QuoteTick) {
        if !self.submitted {
            self.submitted = true;
            ctx.submit_limit(SYM, 1, 3.0, self.px, 0.0, true, None);
        }
    }

    fn on_stop(&mut self, ctx: &mut SimBroker) {
        *self.dropped.borrow_mut() = ctx.dropped.clone();
    }
}

#[test]
fn a_rejected_action_is_recorded_in_the_dropped_diagnostics() {
    // A calibrated series with a rejection burst can swallow a large fraction of a strategy's
    // orders; without a trace, "why did my maker stop quoting" is undiagnosable.
    let dropped = Rc::new(RefCell::new(Vec::new()));
    let strat = RejectWatcher { px: 99.00, submitted: false, dropped: dropped.clone() };
    let rejecting = LatencyModelKind::intp(vec![LatencyRow::new(0, 0, 4_000_000)]);
    let mut e =
        StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], strat, params(Some(rejecting)));
    e.run_ticks(&[(SYM.to_string(), vec![q(1_000, 100.0, 100.02), q(1_010, 99.0, 99.02)])]);
    let got = dropped.borrow().clone();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].0, SYM);
    assert_eq!(got[0].1, "latency_reject");
    assert_eq!(got[0].2, 3.0, "the rejected size is recorded");
}
