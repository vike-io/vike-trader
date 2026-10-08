//! THE READ HALF of the multi-symbol lane, live side: three gaps closed here, and the fourth —
//! `bars` — closed since, in its own PR.
//!
//! `crates/vike-sim/tests/multi_symbol_read_parity.rs` established the law — **a read names a
//! symbol, and the answer must be about that symbol, whichever series woke the strategy** — by
//! running one portable probe through `SimBroker` and `LiveBroker`. It could only reach the BAR
//! lane, so it proved the law exactly where `CoreThread::declared_views` was already wired and
//! pinned the rest in `docs/superpowers/specs/2026-08-07-multi-symbol-read-half.md`.
//!
//! This file is the live-only remainder. Each block below drives ONE of the spec's gaps to failure
//! on the pre-change code, so the assertions ARE the regression proof rather than a restatement:
//!
//! 1. **The non-dispatch lanes carried EMPTY per-symbol tables.** `on_fill`, `on_order_event`,
//!    `on_feed_status`, `on_flow`, `on_mark`, `on_params_updated` and `on_schedule` all built their
//!    `LiveBroker` with `positions: Vec::new(), prices: Vec::new()`, so every per-symbol read inside
//!    them fell through to that dispatch's SCALAR — a wrong number inside a hook, not a missing one.
//!    `vike_mm::xemm`'s `HedgeLedger` module doc names it as one of three reasons the maker folds
//!    its own fill stream instead of reading inventory through the `Broker` seam.
//! 2. **A declared leg resolved against the DISPATCHING venue's engine**, so a
//!    `MountLeg::at(sym, other_venue)` leg — the shape an xEMM hedge requires by construction —
//!    reported the MAKER venue's book for the hedge instrument.
//! 3. **The tag registry keyed on the DISPATCHING series.** `drain_broker` built
//!    `{mount_idx}|{venue}|{symbol}|{tag}` from its own arguments, which are the bar's / the tick's /
//!    the FILL's. A `cancel_tagged` issued from a lane whose drain series differs from the one that
//!    placed the quote looked up a key that was never written, found nothing, and silently left a
//!    REAL order resting at the venue.
//!
//! ⚠ The fourth gap — `LiveBroker::bars` ignoring its `symbol` argument on every lane — was pinned
//! here as STILL OPEN, because closing it grows `LiveBroker`, which is built once per MARKET MESSAGE
//! on `drive_strategy_tick`. It is closed: `LiveBroker::bar_views` is the per-symbol table, filled
//! by the same `CoreThread::declared_views` pass and the same per-leg venue rule as the two above,
//! and `multi_symbol_read_parity.rs`'s `live_bars_are_symbol_addressed` is the inverted pin. The
//! same change gave an UNCARRIED symbol the empty answer instead of the dispatching series' numbers;
//! `vike_core`'s `LiveBroker::carries` carries that argument.
//!
//! # How these tests are made non-vacuous
//!
//! The two legs are given opposite-signed positions of different magnitude and DISJOINT price bands
//! (leg A in the 100s, leg B in the 50s), so ONE number identifies which instrument answered. Every
//! `#[test]` doc states what the pre-change code returned instead, and the values are chosen so that
//! number is never coincidentally the right one. The two `⚠ BYTE-IDENTITY GUARD` tests are the
//! exception and say so: they pass before and after, and exist to fail on the over-reach.
//!
//! ⚠ One design decision here is NOT observable and therefore NOT asserted: on the FILL lane
//! `declared_views` writes no row for the DISPATCHING symbol, so `position(f.symbol)` inside
//! `on_fill` keeps answering from `AppliedFill::position_after` (the state after THIS fill) rather
//! than from the account (the state after the whole batch). Proving it would need two fills on ONE
//! symbol inside ONE ingest message, and THIS HARNESS carries one `Event::Fill` per message.
//!
//! ⚠⚠ "so the two values can never differ here" is what this paragraph used to say, and it is true
//! only of the harness — NOT of the runtime, where a bar-close paper fill of two resting orders or
//! a reconcile fold produces a multi-fill batch. It is also only true of the DISPATCHING symbol:
//! every other declared leg reads POST-BATCH state, which is a real residual named on
//! `CoreThread::declared_views`. Untested and — for the non-dispatching legs — not actually
//! guaranteed.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_core::{CoreConfig, LiveBroker, MountLeg, StrategyMount, spawn_core, spawn_core_multi};
use vike_exec::{
    Account, BalanceMode, BarUpdate, ExecutionClient, ExecutionEngine, RiskGate, RiskLimits,
};
use vike_marketdata::test_support::flat_bar_unit_volume;
use vike_model::events::{Event, FillEvent, TradeId};
use vike_model::{Bar, Broker, FeedStatus, Fill, OrderLifecycle, OrderRequest, Strategy};

#[path = "multi_symbol_reads/non_dispatch_lanes.rs"]
mod non_dispatch_lanes;
#[path = "multi_symbol_reads/tag_registry.rs"]
mod tag_registry;

const VENUE: &str = "binance";
/// The HEDGE venue for the cross-venue block — a second ENGINE, not a second symbol.
const VENUE_B: &str = "bybit";
/// The MOUNTED symbol — leg A. Priced in the 100s.
const LEG_A: &str = "BTCUSDT";
/// The DECLARED second leg. Priced in the 50s, so one price identifies the series.
const LEG_B: &str = "ETHUSDT";
/// Never declared by any mount here: the order path REFUSES it, which is how the order-event lane
/// below gets a dispatch whose symbol is NEITHER leg.
const UNDECLARED: &str = "SOLUSDT";
/// The SAME-VENUE declared leg. Deliberately the SAME symbol as `UNDECLARED`: every other test here
/// leaves it undeclared (which is how they get a dispatch whose symbol is neither leg), and
/// `a_same_venue_leg_reads_and_writes_the_same_book` DECLARES it — so one symbol exercises both
/// sides of the declaration rule, and `engine()`'s `extra_symbols` already carries it.
const LEG_C: &str = UNDECLARED;
const INTERVAL: &str = "1m";

/// Leg A ends up LONG this much, leg B SHORT this much — distinct magnitudes AND distinct signs, so
/// a position read that answered about the wrong leg cannot coincidentally look right.
const POS_A: f64 = 3.0;
const POS_B: f64 = -7.0;
const PX_A: f64 = 100.0;
const PX_B: f64 = 50.0;
/// Leg C is seeded LONG on the MOUNT's own venue, at a magnitude shared with neither other leg.
const POS_C: f64 = 11.0;
const PX_C: f64 = 25.0;

const TAG: &str = "q";
/// Well inside leg A's band, so the resting quote is neither collared nor crossed away.
const TAG_PX: f64 = 90.0;

// ---------------------------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------------------------

// Every bar below is `flat_bar_unit_volume`, whose `symbol` is `None`: live bars arrive
// symbol-less, and the runtime's `Ingest::BarClose` arm stamps the series symbol (#924).

fn close_bar(h: &vike_core::CoreHandle, symbol: &str, ts: i64, px: f64) {
    close_bar_on(h, VENUE, symbol, ts, px);
}

fn close_bar_on(h: &vike_core::CoreHandle, venue: &str, symbol: &str, ts: i64, px: f64) {
    h.bar_sender()
        .close(BarUpdate {
            venue: venue.into(),
            symbol: symbol.into(),
            interval: INTERVAL.into(),
            bar: flat_bar_unit_volume(ts, px),
        })
        .unwrap();
}

fn fill(coid: &str, venue: &str, symbol: &str, side: i32, qty: f64, px: f64) -> FillEvent {
    FillEvent {
        // minted by this helper — same `t-<venue>-<symbol>-<side>` bytes as before
        trade_id: TradeId::prefixed("t-", format_args!("{venue}-{symbol}-{side}")),
        client_order_id: coid.to_string(),
        venue: venue.into(),
        symbol: symbol.into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: None,
        position_side: "BOTH".into(),
    }
}

/// What the strategy could SEE at one dispatch, tagged with the LANE it was taken on. Every field is
/// a read the portable `Broker` surface promises to answer about the symbol NAMED.
#[derive(Debug, Clone, PartialEq)]
struct Look {
    lane: &'static str,
    pos_a: f64,
    pos_b: f64,
    /// The SAME-VENUE declared leg. Only `a_same_venue_leg_reads_and_writes_the_same_book` declares
    /// it, so every other test here holds nothing in it and reads `0.0` — no existing assertion moves.
    pos_c: f64,
    px_a: f64,
    px_b: f64,
}

fn look<B: Broker>(lane: &'static str, b: &B) -> Look {
    Look {
        lane,
        pos_a: b.position(LEG_A),
        pos_b: b.position(LEG_B),
        pos_c: b.position(LEG_C),
        px_a: b.price(LEG_A),
        px_b: b.price(LEG_B),
    }
}

/// One scripted action per `on_bar`, keyed on the BAR's `ts` rather than on a dispatch counter —
/// deliberately, because a declared mount receives its leg's bars and an undeclared one does not, so
/// a counter would silently mean different things in the two runs the byte-identity guards compare.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Act {
    Nothing,
    /// Submit both legs, so their fills come back ATTRIBUTED to this mount. Attribution is required,
    /// not decorative: `dispatch_applied_fills` routes by minted coid and falls back to a
    /// `(venue, symbol)` match that knows nothing about declared legs, so an UNattributed leg-B fill
    /// never reaches a mount whose own symbol is leg A at all.
    SeedLegs,
    /// Submit a symbol the mount never declared — refused, and the refusal arrives as a `Denied`
    /// order event whose dispatch symbol is NEITHER leg.
    SubmitUndeclared,
    SubmitTagged,
    CancelTagged,
    /// A resting quote on the mount's own series PLUS a hedge order on the declared cross-venue leg
    /// — the two-venue state `mass_cancel_pulls_the_mounts_book_not_the_drains` needs before it can
    /// ask which book a `mass_cancel()` pulls.
    QuoteAndHedge,
}

/// Records a [`Look`] on every hook it is given, and drives [`Act`]s off the bar lane.
struct Probe {
    looks: Arc<Mutex<Vec<Look>>>,
    script: Vec<(i64, Act)>,
    /// Call `mass_cancel()` from inside `on_fill` — the canonical use of the verb (a maker pulling
    /// its quotes on adverse selection), and the ONE lane where the drain series is the fill's
    /// rather than the mount's.
    mass_cancel_on_fill: bool,
    /// Submit a MARKET order for this symbol from inside `on_fill` — the WRITE half of the
    /// read/write venue split, observable as which venue's client received it.
    submit_on_fill: Option<&'static str>,
}

impl Probe {
    fn new(looks: &Arc<Mutex<Vec<Look>>>, script: Vec<(i64, Act)>) -> Self {
        Probe { looks: Arc::clone(looks), script, mass_cancel_on_fill: false, submit_on_fill: None }
    }

    fn mass_cancelling(mut self) -> Self {
        self.mass_cancel_on_fill = true;
        self
    }

    fn submitting_on_fill(mut self, symbol: &'static str) -> Self {
        self.submit_on_fill = Some(symbol);
        self
    }
}

impl Strategy<LiveBroker> for Probe {
    fn on_bar(&mut self, b: &mut LiveBroker, bar: &Bar) {
        self.looks.lock().unwrap().push(look("bar", b));
        let act = self
            .script
            .iter()
            .find(|(ts, _)| *ts == bar.ts)
            .map(|(_, a)| *a)
            .unwrap_or(Act::Nothing);
        match act {
            Act::Nothing => {}
            Act::SeedLegs => {
                b.submit_market(LEG_A, 1, POS_A);
                b.submit_market(LEG_B, -1, -POS_B);
            }
            Act::SubmitUndeclared => b.submit_market(UNDECLARED, 1, 1.0),
            // A resting quote on the MOUNT's own series (a tagged verb names no symbol, so it takes
            // the drain's — here the mount's own bar) plus a hedge order on the declared
            // cross-venue leg, so the hedge venue has a real order whose coid a fill can name.
            Act::QuoteAndHedge => {
                b.submit_limit_tagged(TAG, 1, 1.0, TAG_PX);
                b.submit_market(LEG_B, -1, 1.0);
            }
            // TAGGED verbs name no symbol by `HftBroker` contract — the order lands on whatever
            // series is DRAINING, which is exactly what makes the registry key load-bearing.
            Act::SubmitTagged => b.submit_limit_tagged(TAG, 1, 1.0, TAG_PX),
            Act::CancelTagged => b.cancel_tagged(TAG),
        }
    }

    fn on_fill(&mut self, b: &mut LiveBroker, _f: &Fill) {
        self.looks.lock().unwrap().push(look("fill", b));
        if self.mass_cancel_on_fill {
            b.mass_cancel();
        }
        if let Some(sym) = self.submit_on_fill {
            b.submit_market(sym, 1, 1.0);
        }
    }

    fn on_order_event(&mut self, b: &mut LiveBroker, _ev: &OrderLifecycle) {
        self.looks.lock().unwrap().push(look("order_event", b));
    }

    fn on_feed_status(&mut self, b: &mut LiveBroker, _s: FeedStatus) {
        self.looks.lock().unwrap().push(look("feed_status", b));
    }
}

/// The `ExecutionClient` the tests observe through: it fills NOTHING, so a limit order RESTS and a
/// `cancel_tagged` that resolves is visible as a `cancel` on the wire — and one that does NOT
/// resolve is visible as its ABSENCE, which is the exact failure mode the tag-registry block is
/// about (no error, no event, a real order still resting).
struct ProbeClient {
    submissions: Arc<Mutex<Vec<(String, String)>>>,
    cancels: Arc<Mutex<Vec<String>>>,
}

impl ExecutionClient for ProbeClient {
    fn submit(&mut self, r: &OrderRequest) {
        self.submissions.lock().unwrap().push((r.client_order_id.clone(), r.symbol.clone()));
    }
    fn cancel(&mut self, coid: &str) {
        self.cancels.lock().unwrap().push(coid.to_string());
    }
}

/// One client plus the two observation cells it writes into.
struct Wires {
    client: ProbeClient,
    submissions: Arc<Mutex<Vec<(String, String)>>>,
    cancels: Arc<Mutex<Vec<String>>>,
}

fn wires() -> Wires {
    let submissions = Arc::new(Mutex::new(Vec::new()));
    let cancels = Arc::new(Mutex::new(Vec::new()));
    let client =
        ProbeClient { submissions: Arc::clone(&submissions), cancels: Arc::clone(&cancels) };
    Wires { client, submissions, cancels }
}

/// Block until the core has pushed `n` submissions to the client.
///
/// The ingest lane is ONE FIFO channel shared by every sender, so this is a plain progress wait, not
/// a race: the bar that buffers the orders was queued first. It exists so the fills injected next
/// can name the coids the mount ACTUALLY minted instead of a guessed session sequence — a guess
/// would silently mis-route (and the test would then assert about an unattributed fill) if the gate
/// ever rejected one of the seed orders.
fn wait_for_submissions(cell: &Arc<Mutex<Vec<(String, String)>>>, n: usize) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        {
            let got = cell.lock().unwrap();
            if got.len() >= n {
                return got.iter().map(|(coid, _)| coid.clone()).collect();
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {n} submissions to reach the client — the RiskGate refused a \
             seed order, so nothing below would be testing what it claims to"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A `(venue, symbol)` engine that also accepts the other two symbols, so a declared leg's orders,
/// fills and bars are not turned away before the lane under test is reached.
fn engine(venue: &str, symbol: &str, client: ProbeClient) -> ExecutionEngine<ProbeClient> {
    let mut e = ExecutionEngine::new(
        Account::new(1.0, venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        venue,
        symbol,
    );
    e.extra_symbols = [LEG_A, LEG_B, UNDECLARED]
        .iter()
        .filter(|s| **s != symbol)
        .map(|s| (*s).to_string())
        .collect();
    e
}

fn mount(symbols: Vec<MountLeg>, probe: Probe) -> StrategyMount {
    StrategyMount {
        account: None,
        symbols,
        controller_id: None,
        underlying_symbol: None,
        venue: VENUE.into(),
        symbol: LEG_A.into(),
        interval: INTERVAL.into(),
        strategy: Box::new(probe),
    }
}

fn config(m: StrategyMount) -> CoreConfig {
    let t = Arc::new(AtomicI64::new(0));
    CoreConfig {
        seed_cash: 1_000_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        strategy: Some(m),
        ..CoreConfig::default()
    }
}

/// The LAST look taken on `lane` — last rather than first because the seed orders can also produce
/// lifecycle events, and it is the final one (the refusal) the order-event test is about.
fn last_of(looks: &[Look], lane: &str) -> Look {
    looks
        .iter()
        .rfind(|l| l.lane == lane)
        .unwrap_or_else(|| panic!("no `{lane}` dispatch reached the strategy: {looks:#?}"))
        .clone()
}
