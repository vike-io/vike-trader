//! Queue-position gating through `run_ticks` (steal-list: hftbacktest / Nautilus matching-engine
//! mechanism, reimplemented in `vike_backtest::queue_model`).
//!
//! Scripted book + trade + quote sequences prove, end to end:
//! (a) `queue_model: None` (the default) keeps the frozen "price touched = filled" optimism —
//!     and the SAME stream under `RiskAdverse` withholds that fill (the A/B contrast);
//! (b) `RiskAdverse` delays a resting limit until cumulative trades at its price exceed the
//!     seeded front;
//! (c) partial-fill accounting: only each trade's excess beyond the front fills, the remainder
//!     keeps resting at the (now cleared) front;
//! (d) recorded depth CANCELLATIONS clear the front (the `on_depth_change` channel), after
//!     which a mere quote touch fills — and without the cancellation it does not;
//! (e) the probabilistic model's `est_front` wiring (depth decrease re-attributed, then a
//!     trade's excess fills);
//! (f) the tagged (HFT maker) lane is gated the same way, with in-place partial fills;
//! (g) with no book at all, a new order seeds its front from the matching L1 quote size.
//!
//! Plus the adversarial-review regressions — each one an A/B against the control stream that
//! isolates the mutation under test:
//! (h) a canceled order's queue state is NEVER adopted by a re-submit at the same (side, price)
//!     — the `cancel_all` + re-quote pattern makers actually use, in both lanes;
//! (i) a `modify_tagged` qty INCREASE forfeits priority (re-seeds) while a DECREASE keeps it;
//! (j) a partial fill erodes the resting remainder by what ACTUALLY filled, so a PIT-grid-gated
//!     partial leaves the order fully intact instead of bleeding it away;
//! (k) a tag re-priced MID-PASS by an `on_fill` callback is gated at its NEW level;
//! (l) two untagged orders at the same (side, price) keep INDEPENDENT queue states;
//! (m) the bookless quote seed matches on the tick grid, not on bit-exact f64 equality;
//! (n) the gate composes with the `FillModelKind::Tick` price condition, not just the Bar tier;
//! (o) a book-integrity break (dropped book) leaves the states alone and the re-anchoring
//!     Snapshot applies only the conservative min-clamp.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use vike_backtest::engine::{EngineParams, SimBroker, StrategyEngine, Tick};
use vike_backtest::{FillModelKind, QueueModelKind};
use vike_model::{
    Broker, Fill, HftBroker, L2Book, QuoteTick, Strategy, SymbolProperties, TradeTick,
};

const SYM: &str = "TOK";

/// Shared fill log: `(ts, size, price)` per fill, in delivery order.
type FillLog = Rc<RefCell<Vec<(i64, f64, f64)>>>;
/// Tagged-lane fill log: `(ts, size, price, is_maker)`.
type MakerFillLog = Rc<RefCell<Vec<(i64, f64, f64, bool)>>>;

fn snap(ts: i64, seq: u64, bids: Vec<(f64, f64)>, asks: Vec<(f64, f64)>) -> Tick {
    Tick::Book(vike_model::BookUpdate {
        ts,
        local_ts: 0,
        seq,
        kind: vike_model::BookUpdateKind::Snapshot,
        tick_size: 0.01,
        bids,
        asks,
        symbol: SYM.to_string(),
    })
}

fn delta(ts: i64, seq: u64, bids: Vec<(f64, f64)>, asks: Vec<(f64, f64)>) -> Tick {
    Tick::Book(vike_model::BookUpdate {
        ts,
        local_ts: 0,
        seq,
        kind: vike_model::BookUpdateKind::Delta,
        tick_size: 0.01,
        bids,
        asks,
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

fn q(ts: i64, bid: f64, ask: f64, bid_size: f64, ask_size: f64) -> Tick {
    Tick::Quote(QuoteTick {
        ts,
        local_ts: 0,
        bid,
        ask,
        bid_size,
        ask_size,
        symbol: SYM.to_string(),
    })
}

/// Rests ONE untagged limit on the first `on_order_book` delivery and records every fill.
#[derive(Clone)]
struct LimitOnBook {
    side: i32,
    price: f64,
    size: f64,
    submitted: bool,
    fills: FillLog,
}

impl LimitOnBook {
    fn new(side: i32, price: f64, size: f64) -> Self {
        LimitOnBook { side, price, size, submitted: false, fills: Rc::default() }
    }
}

impl Strategy<SimBroker> for LimitOnBook {
    fn on_order_book(&mut self, b: &mut SimBroker, _book: &L2Book) {
        if !self.submitted {
            self.submitted = true;
            Broker::submit_limit(b, SYM, self.side, self.size, self.price);
        }
    }

    fn on_fill(&mut self, _b: &mut SimBroker, f: &Fill) {
        self.fills.borrow_mut().push((f.ts, f.size, f.price));
    }
}

fn run(
    strategy: LimitOnBook,
    ticks: Vec<Tick>,
    queue_model: Option<QueueModelKind>,
) -> (StrategyEngine<LimitOnBook>, FillLog) {
    let fills = Rc::clone(&strategy.fills);
    let params = EngineParams { queue_model, ..Default::default() };
    let mut eng = StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], strategy, params);
    eng.run_ticks(&[(SYM.to_string(), ticks)]);
    (eng, fills)
}

// ---- (a) None = frozen optimism; RiskAdverse withholds the same fill ----

#[test]
fn none_keeps_touch_equals_fill_and_risk_adverse_withholds_it() {
    // 10 resting at 99 ahead of us; a dust trade (0.5) prints at 99.
    let ticks = || vec![snap(1, 1, vec![(99.0, 10.0)], vec![(101.0, 5.0)]), tr(2, 99.0, 0.5)];

    // None (default): price touched → the WHOLE order fills instantly (the frozen optimism).
    let (eng, fills) = run(LimitOnBook::new(1, 99.0, 2.0), ticks(), None);
    assert_eq!(*fills.borrow(), vec![(2, 2.0, 99.0)], "None must keep touch-equals-fill");
    assert_eq!(eng.core.sym[0].pos.size, 2.0);

    // RiskAdverse on the SAME stream: 0.5 traded < 10 ahead → nothing fills, order still rests.
    let (eng, fills) =
        run(LimitOnBook::new(1, 99.0, 2.0), ticks(), Some(QueueModelKind::RiskAdverse));
    assert!(fills.borrow().is_empty(), "queue gate must withhold the touch fill");
    assert_eq!(eng.core.sym[0].pos.size, 0.0);
    assert_eq!(eng.core.sym[0].pending.len(), 1, "the limit keeps resting");
}

// ---- (b) RiskAdverse delays until cumulative trades exceed the seeded front ----

#[test]
fn risk_adverse_fills_only_after_cumulative_trades_consume_the_front() {
    // Front seeded 10 from the book. Trades at 99: 4 (front→6), 5 (front→1), 3 (excess 2 ≥ our 2).
    let ticks = vec![
        snap(1, 1, vec![(99.0, 10.0)], vec![(101.0, 5.0)]),
        tr(2, 99.0, 4.0),
        tr(3, 99.0, 5.0),
        tr(4, 99.0, 3.0),
    ];
    let (eng, fills) =
        run(LimitOnBook::new(1, 99.0, 2.0), ticks, Some(QueueModelKind::RiskAdverse));
    assert_eq!(
        *fills.borrow(),
        vec![(4, 2.0, 99.0)],
        "fill fires only once trades beyond the front cover the order"
    );
    assert_eq!(eng.core.sym[0].pos.size, 2.0);
    assert!(eng.core.sym[0].pending.is_empty(), "fully filled → nothing rests");
}

// ---- (c) partial fills: only each trade's excess beyond the front fills ----

#[test]
fn partial_fills_account_only_the_excess_beyond_the_front() {
    // Front 5; order size 4. Trade 7 → excess 2 fills, 2 rest at the cleared front;
    // trade 1 → fills 1; trade 5 → fills the last 1.
    let ticks = vec![
        snap(1, 1, vec![(99.0, 5.0)], vec![(101.0, 5.0)]),
        tr(2, 99.0, 7.0),
        tr(3, 99.0, 1.0),
        tr(4, 99.0, 5.0),
    ];
    let (eng, fills) =
        run(LimitOnBook::new(1, 99.0, 4.0), ticks, Some(QueueModelKind::RiskAdverse));
    assert_eq!(
        *fills.borrow(),
        vec![(2, 2.0, 99.0), (3, 1.0, 99.0), (4, 1.0, 99.0)],
        "each trade fills exactly its excess beyond the (then-remaining) front"
    );
    assert_eq!(eng.core.sym[0].pos.size, 4.0, "partials sum to the full order");
    assert!(eng.core.sym[0].pending.is_empty());
}

// ---- crossing still fills in full (the level traded through) ----

#[test]
fn a_trade_through_the_level_fills_in_full_despite_the_front() {
    let ticks = vec![
        snap(1, 1, vec![(99.0, 50.0)], vec![(101.0, 5.0)]),
        tr(2, 98.5, 0.1), // prints BELOW our 99 bid → the whole level (front included) cleared
    ];
    let (eng, fills) =
        run(LimitOnBook::new(1, 99.0, 2.0), ticks, Some(QueueModelKind::RiskAdverse));
    // gap-through improvement preserved from the frozen fill model: fp = min(price, open) = 98.5
    assert_eq!(*fills.borrow(), vec![(2, 2.0, 98.5)]);
    assert_eq!(eng.core.sym[0].pos.size, 2.0);
}

// ---- (d) depth cancellations clear the front; a quote touch then fills ----

#[test]
fn depth_cancellation_clears_the_front_then_a_quote_touch_fills() {
    let with_cancel = vec![
        snap(1, 1, vec![(99.0, 10.0)], vec![(101.0, 5.0)]),
        q(2, 99.5, 100.5, 1.0, 1.0), // seeds the resting order: front 10 (no trigger @ mid 100)
        delta(3, 2, vec![(99.0, 0.0)], vec![]), // the level ahead cancels → front clamps to 0
        q(4, 98.9, 99.1, 1.0, 1.0),  // mid 99 touches the limit; front cleared → fills
    ];
    let (eng, fills) =
        run(LimitOnBook::new(1, 99.0, 2.0), with_cancel, Some(QueueModelKind::RiskAdverse));
    assert_eq!(*fills.borrow(), vec![(4, 2.0, 99.0)], "cleared front + touch → fill");
    assert_eq!(eng.core.sym[0].pos.size, 2.0);

    // Control: the SAME stream without the cancellation delta — the touch alone must NOT fill.
    let without_cancel = vec![
        snap(1, 1, vec![(99.0, 10.0)], vec![(101.0, 5.0)]),
        q(2, 99.5, 100.5, 1.0, 1.0),
        q(4, 98.9, 99.1, 1.0, 1.0),
    ];
    let (eng, fills) =
        run(LimitOnBook::new(1, 99.0, 2.0), without_cancel, Some(QueueModelKind::RiskAdverse));
    assert!(fills.borrow().is_empty(), "10 still ahead → a quote touch cannot fill");
    assert_eq!(eng.core.sym[0].pos.size, 0.0);
    assert_eq!(eng.core.sym[0].pending.len(), 1);
}

// ---- (e) probabilistic model wiring (est_front through a recorded depth decrease) ----

#[test]
fn prob_power_model_reestimates_front_on_depth_decrease_then_trade_excess_fills() {
    // Seed front 10 (we join behind the whole level → back 0). The level then shrinks 10 → 2:
    // prob = f(0)/(f(0)+f(10)) = 0 → est_front = 10 − 8 = 2, clamped to [0, 2] → 2.
    // A trade of 3 at the price then has excess 1 ≥ our size 1 → full fill.
    let ticks = vec![
        snap(1, 1, vec![(99.0, 10.0)], vec![(101.0, 5.0)]),
        q(2, 99.5, 100.5, 1.0, 1.0), // seeding pass (no trigger)
        delta(3, 2, vec![(99.0, 2.0)], vec![]),
        tr(4, 99.0, 3.0),
    ];
    let (eng, fills) =
        run(LimitOnBook::new(1, 99.0, 1.0), ticks, Some(QueueModelKind::ProbPower(1.0)));
    assert_eq!(*fills.borrow(), vec![(4, 1.0, 99.0)]);
    assert_eq!(eng.core.sym[0].pos.size, 1.0);
}

// ---- (f) the tagged (HFT maker) lane is queue-gated with in-place partials ----

/// Rests ONE tagged bid on the first `on_order_book` delivery; never re-quotes.
#[derive(Clone)]
struct TaggedOnBook {
    price: f64,
    size: f64,
    submitted: bool,
    fills: MakerFillLog,
}

impl Strategy<SimBroker> for TaggedOnBook {
    fn on_order_book(&mut self, b: &mut SimBroker, _book: &L2Book) {
        if !self.submitted {
            self.submitted = true;
            b.submit_limit_tagged("bid", 1, self.size, self.price);
        }
    }

    fn on_fill(&mut self, _b: &mut SimBroker, f: &Fill) {
        self.fills.borrow_mut().push((f.ts, f.size, f.price, f.is_maker));
    }
}

#[test]
fn tagged_maker_lane_is_queue_gated_with_in_place_partial_fills() {
    let strategy = TaggedOnBook { price: 99.0, size: 2.0, submitted: false, fills: Rc::default() };
    let fills = Rc::clone(&strategy.fills);
    let ticks = vec![
        snap(1, 1, vec![(99.0, 10.0)], vec![(101.0, 5.0)]),
        tr(2, 99.0, 4.0), // seeds front 10, consumes to 6 — no fill
        tr(3, 99.0, 7.0), // excess 1 → PARTIAL maker fill 1; tag shrinks to 1, front cleared
        tr(4, 99.0, 9.0), // excess 9 ≥ 1 → fills the remaining 1, tag retired
    ];
    let params =
        EngineParams { queue_model: Some(QueueModelKind::RiskAdverse), ..Default::default() };
    let mut eng = StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], strategy, params);
    eng.run_ticks(&[(SYM.to_string(), ticks)]);

    assert_eq!(
        *fills.borrow(),
        vec![(3, 1.0, 99.0, true), (4, 1.0, 99.0, true)],
        "tagged fills are queue-gated, partial, and MAKER"
    );
    assert_eq!(eng.core.sym[0].pos.size, 2.0);
    assert!(eng.core.sym[0].tagged.is_empty(), "fully filled tag is retired");
}

// ---- (g) bookless seeding falls back to the matching L1 quote size ----

/// Rests ONE untagged limit on the first `on_quote_tick` (no book events in this stream).
#[derive(Clone)]
struct LimitOnQuote {
    price: f64,
    size: f64,
    submitted: bool,
    fills: FillLog,
}

impl Strategy<SimBroker> for LimitOnQuote {
    fn on_quote_tick(&mut self, b: &mut SimBroker, _q: &QuoteTick) {
        if !self.submitted {
            self.submitted = true;
            Broker::submit_limit(b, SYM, 1, self.size, self.price);
        }
    }

    fn on_fill(&mut self, _b: &mut SimBroker, f: &Fill) {
        self.fills.borrow_mut().push((f.ts, f.size, f.price));
    }
}

#[test]
fn bookless_seed_uses_the_matching_quote_size() {
    // The bid quote at OUR price carries size 3 → the order seeds front 3 (not 0). A trade of 2
    // (< 3) must NOT fill anything (with a zero seed it would have); the next trade's excess does.
    let strategy = LimitOnQuote { price: 99.0, size: 1.0, submitted: false, fills: Rc::default() };
    let fills = Rc::clone(&strategy.fills);
    let ticks = vec![
        q(1, 99.0, 100.0, 3.0, 1.0), // strategy submits AFTER this tick's fill pass
        q(2, 99.0, 100.0, 3.0, 1.0), // seeding pass: no book → bid@99 size 3 → front 3
        tr(3, 99.0, 2.0),            // 2 < 3 ahead → no fill (front → 1)
        tr(4, 99.0, 4.0),            // excess 3 ≥ 1 → fill
    ];
    let params =
        EngineParams { queue_model: Some(QueueModelKind::RiskAdverse), ..Default::default() };
    let mut eng = StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], strategy, params);
    eng.run_ticks(&[(SYM.to_string(), ticks)]);

    assert_eq!(
        *fills.borrow(),
        vec![(4, 1.0, 99.0)],
        "the quote-size seed (3) blocks the size-2 trade; only the later excess fills"
    );
    assert_eq!(eng.core.sym[0].pos.size, 1.0);
}

// =============================================================================================
// Adversarial-review regressions
// =============================================================================================

/// The stream every re-quote regression below runs on: front seeded 10 from the book, then
/// trades of 4 and 5 grind it to 1, so ts-4's trade of 3 has an excess of exactly 2 — the
/// resting size. An order that KEPT its accumulated priority fills there; one that (correctly)
/// rejoined the back of the 10-deep level cannot.
fn ground_down_front_stream() -> Vec<Tick> {
    vec![
        snap(1, 1, vec![(99.0, 10.0)], vec![(101.0, 5.0)]),
        tr(2, 99.0, 4.0),
        tr(3, 99.0, 5.0),
        tr(4, 99.0, 3.0),
    ]
}

// ---- (h) untagged: a re-submit at the same (side, price) must NOT adopt the canceled state ----

/// Rests ONE untagged bid on the first book; on the trade at `requote_at` it does the classic
/// maker re-quote — `cancel_all` then re-submit the SAME (side, price, size). Untagged orders
/// have no modify verb, so cancel+resubmit IS the only re-quote mechanism. `requote_at: None`
/// is the control arm (never re-quotes).
#[derive(Clone)]
struct RequoteUntagged {
    price: f64,
    size: f64,
    requote_at: Option<i64>,
    submitted: bool,
    fills: FillLog,
}

impl Strategy<SimBroker> for RequoteUntagged {
    fn on_order_book(&mut self, b: &mut SimBroker, _book: &L2Book) {
        if !self.submitted {
            self.submitted = true;
            Broker::submit_limit(b, SYM, 1, self.size, self.price);
        }
    }

    fn on_trade_tick(&mut self, b: &mut SimBroker, t: &TradeTick) {
        if self.requote_at == Some(t.ts) {
            b.cancel_all(SYM);
            Broker::submit_limit(b, SYM, 1, self.size, self.price);
        }
    }

    fn on_fill(&mut self, _b: &mut SimBroker, f: &Fill) {
        self.fills.borrow_mut().push((f.ts, f.size, f.price));
    }
}

fn run_untagged_requote(requote_at: Option<i64>) -> (StrategyEngine<RequoteUntagged>, FillLog) {
    let strategy = RequoteUntagged {
        price: 99.0,
        size: 2.0,
        requote_at,
        submitted: false,
        fills: Rc::default(),
    };
    let fills = Rc::clone(&strategy.fills);
    let params =
        EngineParams { queue_model: Some(QueueModelKind::RiskAdverse), ..Default::default() };
    let mut eng = StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], strategy, params);
    eng.run_ticks(&[(SYM.to_string(), ground_down_front_stream())]);
    (eng, fills)
}

#[test]
fn untagged_resubmit_at_the_same_price_rejoins_the_back_of_the_queue() {
    // Control: the order rests untouched, so its ground-down front (1) lets ts-4's trade fill it.
    let (eng, fills) = run_untagged_requote(None);
    assert_eq!(*fills.borrow(), vec![(4, 2.0, 99.0)], "control: the aged order fills");
    assert_eq!(eng.core.sym[0].pos.size, 2.0);

    // The SAME stream with `cancel_all` + a re-submit at the SAME (side, price) on ts 3. The
    // replacement is a different order on any real venue — it re-seeds behind the level's 10, so
    // ts-4's trade of 3 cannot reach it. Keying states by a (side, price) fingerprint instead of
    // per-order identity would have handed it the canceled order's front and filled it.
    let (eng, fills) = run_untagged_requote(Some(3));
    assert!(fills.borrow().is_empty(), "a re-submit must NOT inherit the canceled order's front");
    assert_eq!(eng.core.sym[0].pos.size, 0.0);
    assert_eq!(eng.core.sym[0].pending.len(), 1, "the fresh order rests at the back");
}

// ---- (h/i) tagged: replace-at-same-price re-seeds; amend-up forfeits, amend-down keeps ----

/// What the tagged maker does to its live "bid" quote when the trade at `act_at` prints.
#[derive(Clone, Copy, PartialEq)]
enum TagAction {
    /// control — leave the resting quote alone
    Hold,
    /// `cancel_tagged` + `submit_limit_tagged` at the SAME price (the maker re-quote)
    CancelResubmit,
    /// `submit_limit_tagged` straight OVER the live tag — its documented "insert REPLACES the
    /// prior resting order" path, same price
    ResubmitOver,
    /// `modify_tagged` shrinking the quote — an amend DOWN keeps priority on a real venue
    AmendDown(f64),
    /// `modify_tagged` growing the quote — an amend UP is a new order at the back
    AmendUp(f64),
}

#[derive(Clone)]
struct RequoteTagged {
    price: f64,
    size: f64,
    act_at: i64,
    action: TagAction,
    submitted: bool,
    fills: MakerFillLog,
}

impl Strategy<SimBroker> for RequoteTagged {
    fn on_order_book(&mut self, b: &mut SimBroker, _book: &L2Book) {
        if !self.submitted {
            self.submitted = true;
            b.submit_limit_tagged("bid", 1, self.size, self.price);
        }
    }

    fn on_trade_tick(&mut self, b: &mut SimBroker, t: &TradeTick) {
        if t.ts != self.act_at {
            return;
        }
        match self.action {
            TagAction::Hold => {}
            TagAction::CancelResubmit => {
                b.cancel_tagged("bid");
                b.submit_limit_tagged("bid", 1, self.size, self.price);
            }
            TagAction::ResubmitOver => b.submit_limit_tagged("bid", 1, self.size, self.price),
            TagAction::AmendDown(q) | TagAction::AmendUp(q) => {
                b.modify_tagged("bid", Some(q), None)
            }
        }
    }

    fn on_fill(&mut self, _b: &mut SimBroker, f: &Fill) {
        self.fills.borrow_mut().push((f.ts, f.size, f.price, f.is_maker));
    }
}

fn run_tagged_requote(action: TagAction) -> (StrategyEngine<RequoteTagged>, MakerFillLog) {
    let strategy = RequoteTagged {
        price: 99.0,
        size: 2.0,
        act_at: 3,
        action,
        submitted: false,
        fills: Rc::default(),
    };
    let fills = Rc::clone(&strategy.fills);
    let params =
        EngineParams { queue_model: Some(QueueModelKind::RiskAdverse), ..Default::default() };
    let mut eng = StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], strategy, params);
    eng.run_ticks(&[(SYM.to_string(), ground_down_front_stream())]);
    (eng, fills)
}

#[test]
fn tagged_replace_at_the_same_price_rejoins_the_back_of_the_queue() {
    // Control: the untouched quote keeps its ground-down front and fills on ts 4.
    let (eng, fills) = run_tagged_requote(TagAction::Hold);
    assert_eq!(*fills.borrow(), vec![(4, 2.0, 99.0, true)], "control: the aged quote fills");
    assert_eq!(eng.core.sym[0].pos.size, 2.0);

    // A tag names a quote SLOT, not an order: BOTH ways of putting a new order under the same
    // (tag, side, price) must re-seed at the back, so ts-4's trade of 3 leaves them resting.
    for action in [TagAction::CancelResubmit, TagAction::ResubmitOver] {
        let (eng, fills) = run_tagged_requote(action);
        assert!(fills.borrow().is_empty(), "a replaced tag must not inherit the old front");
        assert_eq!(eng.core.sym[0].pos.size, 0.0);
        assert_eq!(eng.core.sym[0].tagged.len(), 1, "the fresh quote rests at the back");
    }
}

#[test]
fn amend_up_forfeits_queue_priority_and_amend_down_keeps_it() {
    // Amend DOWN (2 → 1): identity survives, so the ground-down front (1) still fills it — the
    // trade's excess of 2 covers the shrunk size of 1.
    let (eng, fills) = run_tagged_requote(TagAction::AmendDown(1.0));
    assert_eq!(*fills.borrow(), vec![(4, 1.0, 99.0, true)], "an amend-down keeps its place");
    assert_eq!(eng.core.sym[0].pos.size, 1.0);

    // Amend UP (2 → 3): every real venue puts an amended-up order at the BACK. The state
    // re-seeds behind the level's 10 and nothing fills.
    let (eng, fills) = run_tagged_requote(TagAction::AmendUp(3.0));
    assert!(fills.borrow().is_empty(), "an amend-up must forfeit the accumulated priority");
    assert_eq!(eng.core.sym[0].pos.size, 0.0);
    assert_eq!(eng.core.sym[0].tagged["bid"].size, 3.0, "the grown quote rests at its new size");
}

// ---- (j) a partial the fill path rejects must not erode the resting order ----

#[test]
fn a_grid_rejected_partial_leaves_the_resting_order_intact() {
    // Front 5, order 4; the trade of 5.3 has an excess of 0.3 — below the PIT grid's min_qty of
    // 1.0, so `apply_fill` rejects it outright. Queue partials are small BY CONSTRUCTION, so
    // deducting the INTENDED qty before the fill was validated would bleed the order away with
    // zero fills and no `on_fill` ever firing. It must still rest at the full 4.
    let ticks = vec![snap(1, 1, vec![(99.0, 5.0)], vec![(101.0, 5.0)]), tr(2, 99.0, 5.3)];
    let grid = SymbolProperties { step_size: 0.1, min_qty: 1.0, ..Default::default() };
    let strategy = LimitOnBook::new(1, 99.0, 4.0);
    let fills = Rc::clone(&strategy.fills);
    let params = EngineParams {
        queue_model: Some(QueueModelKind::RiskAdverse),
        default_venue: Some("TEST".into()),
        properties: Some(Arc::new(move |_v: &str, _s: &str, _t: i64| Some(grid))),
        ..Default::default()
    };
    let mut eng = StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], strategy, params);
    eng.run_ticks(&[(SYM.to_string(), ticks)]);

    assert!(fills.borrow().is_empty(), "0.3 < min_qty 1.0 → the grid rejects the partial");
    assert_eq!(eng.core.sym[0].pos.size, 0.0);
    assert_eq!(eng.core.sym[0].pending.len(), 1);
    assert_eq!(eng.core.sym[0].pending[0].size, 4.0, "a rejected partial must not erode the order");
}

#[test]
fn a_volume_clamped_partial_erodes_only_by_what_actually_filled() {
    // Same shape through the OTHER downstream shrink: `volume_limit` caps the fill at
    // 0.1 · bar volume = 0.7 of the intended 2.0 excess. The remainder must lose exactly the
    // 0.7 that filled — not the 2.0 that was merely intended.
    let ticks = vec![snap(1, 1, vec![(99.0, 5.0)], vec![(101.0, 5.0)]), tr(2, 99.0, 7.0)];
    let strategy = LimitOnBook::new(1, 99.0, 4.0);
    let fills = Rc::clone(&strategy.fills);
    let params = EngineParams {
        queue_model: Some(QueueModelKind::RiskAdverse),
        volume_limit: Some(0.1),
        ..Default::default()
    };
    let mut eng = StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], strategy, params);
    eng.run_ticks(&[(SYM.to_string(), ticks)]);

    // `0.1 * 7.0` is 0.7000000000000001 in binary f64 — the fill and the deduction must be that
    // EXACT value, which is the point: the remainder loses precisely what the fill applied.
    let capped = 0.1 * 7.0;
    assert_eq!(*fills.borrow(), vec![(2, capped, 99.0)], "the volume cap clamps the partial");
    assert_eq!(eng.core.sym[0].pos.size, capped);
    assert_eq!(eng.core.sym[0].pending.len(), 1);
    assert_eq!(eng.core.sym[0].pending[0].size, 4.0 - capped, "remainder = 4 − what filled");
}

// ---- (k) a tag re-priced MID-PASS is gated at its NEW level ----

/// A two-sided maker: an ask at 100 (registered FIRST, so it is gated first) and a bid at 99.5.
/// The instant the ask fills it re-prices the bid UP to 100 from inside `on_fill` — the classic
/// fill-one-side-requote-the-other flow, which moves a tag AFTER this pass's `sync_tags` ran.
#[derive(Clone)]
struct RequoteOnFill {
    submitted: bool,
    repriced: bool,
    fills: MakerFillLog,
}

impl Strategy<SimBroker> for RequoteOnFill {
    fn on_order_book(&mut self, b: &mut SimBroker, _book: &L2Book) {
        if !self.submitted {
            self.submitted = true;
            b.submit_limit_tagged("ask", -1, 1.0, 100.0);
            b.submit_limit_tagged("bid", 1, 1.0, 99.5);
        }
    }

    fn on_fill(&mut self, b: &mut SimBroker, f: &Fill) {
        self.fills.borrow_mut().push((f.ts, f.size, f.price, f.is_maker));
        if !self.repriced {
            self.repriced = true;
            b.modify_tagged("bid", None, Some(100.0));
        }
    }
}

#[test]
fn a_tag_repriced_mid_pass_is_gated_at_its_new_level() {
    // Book: 8 resting on the BID at 100 and 1 on the ASK at 100 (a synthetic locked level, so a
    // single trade at 100 can trigger both tags). The bid tag starts at 99.5 — a price the book
    // has NO level for, so its state seeds an empty (already cleared) front.
    //
    // One trade of 3 at 100: it clears the ask's front of 1 and the excess 2 ≥ 1 fills the ask,
    // whose `on_fill` re-prices the bid to 100. The bid then triggers on that SAME tick. Gated
    // at its NEW level it sits behind 8 and cannot fill; gated with the stale 99.5 state (front
    // 0 = cleared) it would fill instantly at a level it joined microseconds earlier.
    let strategy = RequoteOnFill { submitted: false, repriced: false, fills: Rc::default() };
    let fills = Rc::clone(&strategy.fills);
    let ticks = vec![snap(1, 1, vec![(100.0, 8.0)], vec![(100.0, 1.0)]), tr(2, 100.0, 3.0)];
    let params =
        EngineParams { queue_model: Some(QueueModelKind::RiskAdverse), ..Default::default() };
    let mut eng = StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], strategy, params);
    eng.run_ticks(&[(SYM.to_string(), ticks)]);

    assert_eq!(
        *fills.borrow(),
        vec![(2, 1.0, 100.0, true)],
        "only the ask fills; the mid-pass re-priced bid re-seeds behind the new level's 8"
    );
    assert_eq!(eng.core.sym[0].pos.size, -1.0);
    assert_eq!(eng.core.sym[0].tagged.len(), 1, "the re-priced bid still rests");
}

// ---- (l) two untagged orders at the same (side, price) keep INDEPENDENT states ----

/// Rests one untagged bid on the first book and a SECOND at the SAME (side, price) on the trade
/// at `second_at` — the same-price multiplicity a per-level fingerprint used to serve.
#[derive(Clone)]
struct TwoAtOneLevel {
    second_at: i64,
    submitted: bool,
    fills: FillLog,
}

impl Strategy<SimBroker> for TwoAtOneLevel {
    fn on_order_book(&mut self, b: &mut SimBroker, _book: &L2Book) {
        if !self.submitted {
            self.submitted = true;
            Broker::submit_limit(b, SYM, 1, 1.0, 99.0);
        }
    }

    fn on_trade_tick(&mut self, b: &mut SimBroker, t: &TradeTick) {
        if t.ts == self.second_at {
            Broker::submit_limit(b, SYM, 1, 1.0, 99.0);
        }
    }

    fn on_fill(&mut self, _b: &mut SimBroker, f: &Fill) {
        self.fills.borrow_mut().push((f.ts, f.size, f.price));
    }
}

#[test]
fn two_orders_at_one_level_keep_independent_queue_states() {
    // A joins at ts 1 behind 10; ts-2's trade of 6 grinds A's front to 4 and B joins after it,
    // seeding behind the level's (still 10) depth — price-time priority, the venue's tiebreak.
    // ts-3's trade of 5 therefore fills A (excess 1) and leaves B resting (5 < 10), and only
    // ts-4's trade of 7 reaches B (its front is 5 by then). A single shared per-level state
    // would have filled both at ts 3.
    let strategy = TwoAtOneLevel { second_at: 2, submitted: false, fills: Rc::default() };
    let fills = Rc::clone(&strategy.fills);
    let ticks = vec![
        snap(1, 1, vec![(99.0, 10.0)], vec![(101.0, 5.0)]),
        tr(2, 99.0, 6.0),
        tr(3, 99.0, 5.0),
        tr(4, 99.0, 7.0),
    ];
    let params =
        EngineParams { queue_model: Some(QueueModelKind::RiskAdverse), ..Default::default() };
    let mut eng = StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], strategy, params);
    eng.run_ticks(&[(SYM.to_string(), ticks)]);

    assert_eq!(
        *fills.borrow(),
        vec![(3, 1.0, 99.0), (4, 1.0, 99.0)],
        "the later order waits out its own, deeper front"
    );
    assert_eq!(eng.core.sym[0].pos.size, 2.0);
    assert!(eng.core.sym[0].pending.is_empty());
}

// ---- (m) the bookless quote seed matches on the tick grid, not bit-exact f64 equality ----

#[test]
fn bookless_seed_tolerates_quote_price_drift() {
    // The quote's bid is ONE ULP off the order's price — the arithmetic drift a strategy price
    // picks up routinely (`mid − k·tick`). Bit-exact matching would miss the level and seed
    // `queue_seed_depth`, whose 0.0 default means front-of-queue: exactly the frozen optimism
    // the model exists to remove, and silently so. On the tick grid it still seeds the quote's 3,
    // which blocks the size-2 trade at ts 3; only ts-4's excess fills.
    let drifted = f64::from_bits(99.0f64.to_bits() + 1);
    assert_ne!(drifted, 99.0, "the drifted quote must be a different f64");
    let strategy = LimitOnQuote { price: 99.0, size: 1.0, submitted: false, fills: Rc::default() };
    let fills = Rc::clone(&strategy.fills);
    let ticks = vec![
        q(1, drifted, 100.0, 3.0, 1.0),
        q(2, drifted, 100.0, 3.0, 1.0),
        tr(3, 99.0, 2.0),
        tr(4, 99.0, 4.0),
    ];
    let params =
        EngineParams { queue_model: Some(QueueModelKind::RiskAdverse), ..Default::default() };
    let mut eng = StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], strategy, params);
    eng.run_ticks(&[(SYM.to_string(), ticks)]);

    assert_eq!(*fills.borrow(), vec![(4, 1.0, 99.0)], "an ulp of drift must not lose the seed");
    assert_eq!(eng.core.sym[0].pos.size, 1.0);
}

// ---- (n) the gate composes with the Tick fill model's price condition ----

#[test]
fn tick_fill_model_quote_triggering_composes_with_the_queue_gate() {
    // `FillModelKind::Tick` triggers a resting BUY off the ASK (`ask <= price`) where the Bar
    // tier triggers off the mid — a DIFFERENT price condition with the SAME queue gate on top.
    // A quote whose ask sits exactly ON our 99 bid triggers but must not fill while 10 rest
    // ahead; once a recorded cancellation clears the level, the same quote does fill.
    let ticks = || {
        vec![
            snap(1, 1, vec![(99.0, 10.0)], vec![(101.0, 5.0)]),
            q(2, 98.0, 99.0, 1.0, 1.0),
            delta(3, 2, vec![(99.0, 0.0)], vec![]),
            q(4, 98.0, 99.0, 1.0, 1.0),
        ]
    };
    let params = |queue_model| EngineParams {
        fill_model: FillModelKind::Tick,
        queue_model,
        ..Default::default()
    };

    // Control: the Tick model alone fills the instant its quote condition is met (ts 2).
    let strategy = LimitOnBook::new(1, 99.0, 2.0);
    let fills = Rc::clone(&strategy.fills);
    let mut eng = StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], strategy, params(None));
    eng.run_ticks(&[(SYM.to_string(), ticks())]);
    assert_eq!(*fills.borrow(), vec![(2, 2.0, 99.0)], "Tick model + no queue = instant fill");

    // Gated: the same trigger waits for the front to clear.
    let strategy = LimitOnBook::new(1, 99.0, 2.0);
    let fills = Rc::clone(&strategy.fills);
    let mut eng = StrategyEngine::new(
        vec![(SYM.to_string(), Vec::new())],
        strategy,
        params(Some(QueueModelKind::RiskAdverse)),
    );
    eng.run_ticks(&[(SYM.to_string(), ticks())]);
    assert_eq!(
        *fills.borrow(),
        vec![(4, 2.0, 99.0)],
        "the ts-2 trigger is queue-gated; only the post-cancellation ts-4 quote fills"
    );
    assert_eq!(eng.core.sym[0].pos.size, 2.0);
}

// ---- (o) a book-integrity break leaves the queue states alone ----

#[test]
fn a_book_integrity_break_leaves_states_alone_then_min_clamps_on_re_anchor() {
    // A seq-gapped Delta drops the replay book (the gap-sentinel rule). While there is no book
    // the tracked front must simply survive — `pre_depth` reports None and `apply_depth` has no
    // level to fold. The re-anchoring Snapshot then applies only the conservative min-clamp:
    // a level that came back SMALLER than the front pulls the front down to it.
    let ticks = vec![
        snap(1, 1, vec![(99.0, 10.0)], vec![(101.0, 5.0)]),
        tr(2, 99.0, 2.0),                                   // front 10 → 8, no fill
        delta(3, 9, vec![(99.0, 4.0)], vec![]), // seq 9 after 1 → integrity break, book dropped
        tr(4, 99.0, 3.0),                       // bookless: front 8 → 5, still no fill
        snap(5, 10, vec![(99.0, 1.0)], vec![(101.0, 5.0)]), // re-anchor: min-clamp 5 → 1
        tr(6, 99.0, 2.0),                       // excess 1 ≥ our 1 → fills
    ];
    let (eng, fills) =
        run(LimitOnBook::new(1, 99.0, 1.0), ticks, Some(QueueModelKind::RiskAdverse));
    assert_eq!(
        *fills.borrow(),
        vec![(6, 1.0, 99.0)],
        "the state survives the dropped book and the re-anchor only clamps it down"
    );
    assert_eq!(eng.core.sym[0].pos.size, 1.0);
}
