//! THE READ HALF of the multi-symbol lane, live side: three gaps closed here, and the fourth —
//! `bars` — closed since, in its own PR.
//!
//! `crates/vike-backtest/tests/multi_symbol_read_parity.rs` established the law — **a read names a
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

use vike_core::{spawn_core, spawn_core_multi, CoreConfig, LiveBroker, MountLeg, StrategyMount};
use vike_exec::{
    Account, BalanceMode, BarUpdate, ExecutionClient, ExecutionEngine, RiskGate, RiskLimits,
    StreamStatusUpdate,
};
use vike_model::events::{Event, FillEvent, OrderAccepted, OrderCanceled, OrderSubmitted, TradeId};
use vike_model::strategy::{FeedStatus, OrderLifecycle};
use vike_model::{Bar, Broker, Fill, OrderRequest, Strategy};

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

fn bar(ts: i64, px: f64) -> Bar {
    Bar {
        ts,
        open: px,
        high: px,
        low: px,
        close: px,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        // Live bars arrive symbol-less; the runtime's `Ingest::BarClose` arm stamps the series
        // symbol (#924).
        symbol: None,
    }
}

fn close_bar(h: &vike_core::CoreHandle, symbol: &str, ts: i64, px: f64) {
    close_bar_on(h, VENUE, symbol, ts, px);
}

fn close_bar_on(h: &vike_core::CoreHandle, venue: &str, symbol: &str, ts: i64, px: f64) {
    h.bar_sender()
        .close(BarUpdate {
            venue: venue.into(),
            symbol: symbol.into(),
            interval: INTERVAL.into(),
            bar: bar(ts, px),
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

// ---------------------------------------------------------------------------------------------
// GAP 3 — the non-dispatch lanes carried EMPTY per-symbol tables
// ---------------------------------------------------------------------------------------------

/// Open both legs on ONE venue, then poke the three lanes that carry no series of their own.
///
/// `declare` is the knob the byte-identity guard flips: with an empty declaration this is an
/// ordinary single-symbol mount and every table must stay empty. Everything else — the bars, the
/// fills, the status poke — is identical between the two runs on purpose, so the guard compares
/// like with like.
fn run_same_venue(declare: bool) -> Vec<Look> {
    let looks = Arc::new(Mutex::new(Vec::new()));
    let symbols = if declare { vec![MountLeg::same_venue(LEG_B)] } else { Vec::new() };
    let probe = Probe::new(
        &looks,
        vec![(60_000, Act::SeedLegs), (120_000, Act::Nothing), (180_000, Act::SubmitUndeclared)],
    );
    let w = wires();
    let handle = spawn_core(engine(VENUE, LEG_A, w.client), config(mount(symbols, probe)));

    // Bar 1 prices leg A and buffers both legs' orders (leg A first, leg B second, in push order).
    close_bar(&handle, LEG_A, 60_000, PX_A);
    // Leg B's own bar prices it in its own band. A declared mount also RECEIVES this one (scripted
    // to do nothing); an undeclared mount does not — but the mark/price-board write happens either
    // way, which is what keeps the two runs comparable.
    close_bar(&handle, LEG_B, 120_000, PX_B);

    let coids = wait_for_submissions(&w.submissions, 2);

    // The venue reports both orders filled — ATTRIBUTED, by the coids just observed. Leg B's fill is
    // what makes the mount's own symbol a NON-dispatching one on the fill lane.
    let ev = handle.event_sender();
    ev.blocking_send(Event::Fill(fill(&coids[0], VENUE, LEG_A, 1, POS_A, PX_A))).unwrap();
    ev.blocking_send(Event::Fill(fill(&coids[1], VENUE, LEG_B, -1, -POS_B, PX_B))).unwrap();

    // A feed-health transition on the mount's OWN pair — no series, no price, just the hook.
    handle
        .tick_sender()
        .stream_status(StreamStatusUpdate {
            venue: VENUE.into(),
            symbol: LEG_A.into(),
            stream: "trade".into(),
            status: FeedStatus::Disconnected,
        })
        .unwrap();

    // Bar 3 asks for an UNDECLARED symbol: refused, and the refusal comes back as a `Denied` order
    // event whose dispatch symbol is neither leg.
    close_bar(&handle, LEG_A, 180_000, PX_A + 1.0);
    handle.shutdown_and_join();
    let out = looks.lock().unwrap().clone();
    out
}

/// **THE `on_fill` GATE.** A declared mount's `position`/`price` inside `on_fill` must answer about
/// the symbol NAMED, not about the symbol that filled.
///
/// FAILS ON THE PRE-CHANGE CODE: `dispatch_applied_fills` built its `LiveBroker` with
/// `positions: Vec::new(), prices: Vec::new()`, so on leg B's fill `position(LEG_A)` fell through to
/// the `af.position_after` scalar and returned **leg B's** signed size (`POS_B` — opposite sign AND
/// a different magnitude), while `price(LEG_A)` returned leg B's price, outside leg A's band.
///
/// `on_fill` is where a hedging strategy decides how much of the other leg it still owes, so this
/// was the wrong number arriving in the one hook whose entire job is inventory.
#[test]
fn on_fill_reads_are_symbol_addressed_on_a_declared_mount() {
    let looks = run_same_venue(true);
    let fills: Vec<&Look> = looks.iter().filter(|l| l.lane == "fill").collect();
    assert_eq!(fills.len(), 2, "both legs' fills must reach the mount: {looks:#?}");
    // The SECOND fill is leg B's — the dispatch whose scalar describes the OTHER instrument.
    let l = fills[1];
    assert_eq!(
        l.pos_a, POS_A,
        "position({LEG_A}) during leg B's fill must be leg A's own ({POS_A}); {} is leg B's, i.e. \
         the answer came from the dispatching scalar: {l:?}",
        l.pos_a
    );
    assert_eq!(l.pos_b, POS_B, "position({LEG_B}) is the filling leg's own: {l:?}");
    assert!(
        (100.0..200.0).contains(&l.px_a),
        "price({LEG_A}) = {} is outside leg A's band — a price answered about leg B: {l:?}",
        l.px_a
    );
    assert!((50.0..100.0).contains(&l.px_b), "price({LEG_B}) stays in leg B's band: {l:?}");
}

/// **THE `on_order_event` GATE**, and the sharpest one: here the dispatching symbol is NEITHER leg
/// (it is the undeclared symbol the order path refused), so the scalar fallback describes an
/// instrument the account holds nothing in.
///
/// FAILS ON THE PRE-CHANGE CODE with `position(LEG_A) == 0.0` and `position(LEG_B) == 0.0`: the
/// tables were empty and the scalar was `position_size_of(SOLUSDT)`. A position manager reacting to
/// a rejection by re-reading its inventory would have seen a FLAT book and re-entered.
#[test]
fn on_order_event_reads_are_symbol_addressed_on_a_declared_mount() {
    let looks = run_same_venue(true);
    let l = last_of(&looks, "order_event");
    assert_eq!(l.pos_a, POS_A, "position({LEG_A}) inside on_order_event: {l:?}");
    assert_eq!(l.pos_b, POS_B, "position({LEG_B}) inside on_order_event: {l:?}");
    assert!((100.0..200.0).contains(&l.px_a), "price({LEG_A}) inside on_order_event: {l:?}");
    assert!((50.0..100.0).contains(&l.px_b), "price({LEG_B}) inside on_order_event: {l:?}");
}

/// **THE `on_feed_status` GATE.** A feed death is precisely when a two-leg strategy asks what it is
/// still holding on the OTHER leg — and that read was the dispatching leg's.
///
/// FAILS ON THE PRE-CHANGE CODE with `position(LEG_B) == POS_A`: the status update names the mount's
/// OWN pair, so the empty table sent `position(ETHUSDT)` to leg A's scalar — a strategy pulling
/// quotes on a disconnect saw itself LONG 3 of a leg it is SHORT 7 of.
#[test]
fn on_feed_status_reads_are_symbol_addressed_on_a_declared_mount() {
    let looks = run_same_venue(true);
    let l = last_of(&looks, "feed_status");
    assert_eq!(
        l.pos_b, POS_B,
        "position({LEG_B}) inside on_feed_status must be leg B's ({POS_B}); {} is leg A's: {l:?}",
        l.pos_b
    );
    assert_eq!(l.pos_a, POS_A, "position({LEG_A}) inside on_feed_status: {l:?}");
    assert!((50.0..100.0).contains(&l.px_b), "price({LEG_B}) inside on_feed_status: {l:?}");
}

/// ⚠ THE BYTE-IDENTITY GUARD for every lane above: a mount that declared NOTHING must still see
/// EMPTY tables, i.e. every per-symbol read answers with the dispatching scalar whatever symbol is
/// named — even a symbol the ENGINE genuinely holds a position in.
///
/// It passes before and after the change, on purpose: it is not the regression proof, it is the
/// acceptance condition. It FAILS on the tempting over-reach — building the tables from the ENGINE's
/// symbols instead of from `StrategyMount::symbols`, or dropping `declared_views`' early return —
/// because the account here really does hold leg B (the second injected fill names it), so
/// `position(LEG_B)` would start returning `POS_B` on a leg-A dispatch where the contract says it
/// must return leg A's number.
#[test]
fn an_undeclared_mount_still_reads_only_the_dispatch_scalar() {
    let looks = run_same_venue(false);
    for l in &looks {
        assert_eq!(
            l.pos_a, l.pos_b,
            "an undeclared mount answers EVERY symbol with the dispatch scalar, so two reads that \
             name different symbols cannot differ: {l:?}"
        );
        assert_eq!(
            l.px_a, l.px_b,
            "same for price — a per-symbol table must not exist on this mount at all: {l:?}"
        );
    }
    assert!(
        looks.iter().any(|l| l.lane == "fill"),
        "the sweep must observe a NON-bar lane, else it holds vacuously over the one lane that \
         always had tables: {looks:#?}"
    );
    assert!(
        looks.iter().any(|l| l.lane == "feed_status"),
        "...and the feed-status lane too: {looks:#?}"
    );
}

// ---------------------------------------------------------------------------------------------
// GAP 4 — a declared leg resolved against the DISPATCHING venue's engine
// ---------------------------------------------------------------------------------------------

/// **THE CROSS-VENUE GATE.** A leg declared with `MountLeg::at(sym, other_venue)` must report THAT
/// venue's book.
///
/// FAILS ON THE PRE-CHANGE CODE: `push_view` read every leg out of `self.eng(eidx)` — the
/// DISPATCHING venue's engine, i.e. the maker's — so `position(LEG_B)` was `0.0` (the maker engine
/// holds no hedge position) and `price(LEG_B)` was leg A's bar close (the maker engine cannot price
/// the hedge symbol at all, so no price row was written and the read fell through to the dispatch
/// scalar). An xEMM maker reading its hedge inventory through the `Broker` seam therefore saw itself
/// permanently unhedged; `vike_mm::xemm` works around it by folding the hedge leg from its own
/// `on_fill` stream, which is why the defect survived this long.
///
/// It is also the byte-identity companion for the SAME-venue case: a `MountLeg::same_venue` leg
/// still resolves the mount's own engine, which the `on_fill`/`on_feed_status` gates above exercise
/// (they read a same-venue leg and expect the mount engine's numbers).
#[test]
fn a_cross_venue_leg_reads_its_own_venues_engine() {
    let looks = Arc::new(Mutex::new(Vec::new()));
    let probe = Probe::new(&looks, Vec::new());
    let maker = wires();
    let hedge = wires();

    let handle = spawn_core_multi(
        engine(VENUE, LEG_A, maker.client),
        vec![(1_000_000.0, engine(VENUE_B, LEG_B, hedge.client))],
        config(mount(vec![MountLeg::at(LEG_B, VENUE_B)], probe)),
    );

    // The hedge venue's book: a position and a price, BOTH only on venue B's engine. The maker
    // engine is left holding nothing in leg B and pricing nothing in it, so an answer of `0.0` or of
    // leg A's price is unambiguously the maker engine's.
    handle
        .event_sender()
        .blocking_send(Event::Fill(fill("", VENUE_B, LEG_B, -1, -POS_B, PX_B)))
        .unwrap();
    close_bar_on(&handle, VENUE_B, LEG_B, 60_000, PX_B);
    // Now the MAKER venue's own bar — the dispatch the probe reads from.
    close_bar(&handle, LEG_A, 60_000, PX_A);
    handle.shutdown_and_join();

    let out = looks.lock().unwrap().clone();
    let l = last_of(&out, "bar");
    assert_eq!(
        l.pos_b, POS_B,
        "position({LEG_B}) must be the HEDGE venue's ({POS_B}); {} is the maker engine's book: \
         {l:?}",
        l.pos_b
    );
    assert!(
        (50.0..100.0).contains(&l.px_b),
        "price({LEG_B}) = {} must come from {VENUE_B}'s price board; leg A's band means the maker \
         engine answered — or could not, and the dispatch scalar did: {l:?}",
        l.px_b
    );
    // The mount's own leg is untouched by any of this.
    assert_eq!(l.pos_a, 0.0, "the maker leg was never filled: {l:?}");
    assert!((100.0..200.0).contains(&l.px_a), "price({LEG_A}) is the maker bar's close: {l:?}");
}

// ---------------------------------------------------------------------------------------------
// GAP 5 — the tag registry keyed on the DISPATCHING series
// ---------------------------------------------------------------------------------------------

/// Place a tagged quote from one series' dispatch and pull it from another's. The mount is always
/// `(binance, LEG_A)`; `place_on`/`cancel_on` name the series whose bar drives each step.
///
/// Returns `(submissions, cancels)` as the CLIENT saw them — a cancel that never resolved shows up
/// as an EMPTY `cancels`, which is precisely the "quote left resting at the venue" failure.
fn run_tag(declare: bool, place_on: &str, cancel_on: &str) -> (Vec<(String, String)>, Vec<String>) {
    let looks = Arc::new(Mutex::new(Vec::new()));
    let symbols = if declare { vec![MountLeg::same_venue(LEG_B)] } else { Vec::new() };
    let probe = Probe::new(&looks, vec![(60_000, Act::SubmitTagged), (120_000, Act::CancelTagged)]);
    let w = wires();
    let handle = spawn_core(engine(VENUE, LEG_A, w.client), config(mount(symbols, probe)));

    close_bar(&handle, place_on, 60_000, PX_A);
    close_bar(&handle, cancel_on, 120_000, PX_A);
    handle.shutdown_and_join();

    let subs = w.submissions.lock().unwrap().clone();
    let cans = w.cancels.lock().unwrap().clone();
    (subs, cans)
}

fn assert_the_quote_was_pulled(subs: &[(String, String)], cans: &[String], how: &str) {
    assert_eq!(subs.len(), 1, "{how}: exactly one tagged quote must reach the venue: {subs:?}");
    assert_eq!(
        cans,
        &[subs[0].0.clone()],
        "{how}: the tagged quote ({}) must be CANCELED. An EMPTY list is the silent failure this \
         block exists for — the lookup missed, `drain_broker` did nothing, and a real order is \
         still resting at the venue with no error, no event and no ring line",
        subs[0].0
    );
}

/// **THE LOOKUP HALF.** A quote placed from the MOUNT's own series must be cancellable from a
/// DECLARED LEG's dispatch.
///
/// FAILS ON THE PRE-CHANGE CODE with `cancels == []`: the insert used the drain's
/// `0|binance|BTCUSDT|q` while the cancel, drained on leg B's bar, looked up `0|binance|ETHUSDT|q` —
/// a key nothing ever wrote.
///
/// This is the direction that stays RED if only the INSERT were moved onto the mount's series, which
/// the spec named as the tempting half-fix.
#[test]
fn a_tag_placed_on_the_mount_series_is_cancellable_from_a_leg_dispatch() {
    let (subs, cans) = run_tag(true, LEG_A, LEG_B);
    assert_the_quote_was_pulled(
        &subs,
        &cans,
        "placed on the mount's series, pulled from the leg's",
    );
}

/// **THE INSERT HALF.** The mirror: a quote placed from a DECLARED LEG's dispatch must be
/// cancellable from the mount's own series.
///
/// FAILS ON THE PRE-CHANGE CODE with `cancels == []` for the opposite reason — the insert was
/// `0|binance|ETHUSDT|q` (leg B's bar was draining) and the lookup `0|binance|BTCUSDT|q`. Together
/// with the test above, BOTH sides are proven to have moved: a fix that changed only one leaves
/// exactly one of these two red.
///
/// (The quote itself rests on leg B — a tagged verb names no symbol, so `resolve_intent_symbol`
/// gives it the DRAIN's series. That is unchanged and deliberate; only the KEY moved, which is why
/// the first assertion below is worth making.)
#[test]
fn a_tag_placed_on_a_leg_dispatch_is_cancellable_from_the_mount_series() {
    let (subs, cans) = run_tag(true, LEG_B, LEG_A);
    assert_eq!(subs[0].1, LEG_B, "the tagged quote rests on the DRAIN's series: {subs:?}");
    assert_the_quote_was_pulled(
        &subs,
        &cans,
        "placed on the leg's series, pulled from the mount's",
    );
}

/// ⚠ THE BYTE-IDENTITY GUARD for the registry: a single-symbol mount places and pulls on its own
/// series across two dispatches, exactly as every shipped maker does.
///
/// Passes before and after — that is the point. It is what goes red if the mount-keyed rewrite ever
/// resolves the mount slot wrongly (a lane draining while the slot is taken, say), which would
/// silently take EVERY maker's `cancel_tagged` down with it rather than just the multi-symbol ones.
#[test]
fn a_single_symbol_mounts_tag_round_trip_is_unchanged() {
    let (subs, cans) = run_tag(false, LEG_A, LEG_A);
    assert_the_quote_was_pulled(&subs, &cans, "single-symbol mount, own series both times");
}

/// ⚠ **`mass_cancel()` pulls the MOUNT's book, not the book the dispatch happened to be about.**
///
/// `Broker::mass_cancel` is documented as "cancel ALL of THIS ENGINE's live orders", and
/// `runtime/apply.rs`'s `(Some(v), sym)` arm makes the VENUE the load-bearing half: it resolves that
/// venue's engine and mass-cancels the whole thing, using the symbol only to scope held exits and
/// conditional books. `drain_broker` stamped its own `venue`/`symbol` arguments onto the intent —
/// the DRAIN SERIES, which its doc explicitly says are not the mount's.
///
/// On the fill lane the drain series is the FILL's. So a maker calling `mass_cancel()` from inside
/// `on_fill` — the canonical use, pulling quotes on adverse selection — pulled the book of whichever
/// venue the fill arrived from. On a cross-venue mount that is routinely the HEDGE venue: its own
/// quotes stayed live at the maker venue while an unrelated book was cleared. The same class as
/// `a_same_venue_leg_reads_and_writes_the_same_book` and `tag_key`'s registry bug — a verb keyed on
/// the drain instead of the mount — and the last verb still keyed that way.
///
/// NON-VACUOUS in both directions, which is why both halves are asserted. The resting quote is
/// placed on the MAKER venue and the fill is delivered on the HEDGE venue precisely so the two
/// candidate venues differ: pre-fix the maker's cancel list is EMPTY (the real order keeps resting,
/// silently — no error, no event) and the hedge's engine is the one pulled. A same-venue fill would
/// make both readings agree by accident and prove nothing.
#[test]
fn mass_cancel_pulls_the_mounts_book_not_the_drains() {
    let looks = Arc::new(Mutex::new(Vec::new()));
    let probe = Probe::new(&looks, vec![(60_000, Act::QuoteAndHedge)]).mass_cancelling();
    let maker = wires();
    let hedge = wires();

    let handle = spawn_core_multi(
        engine(VENUE, LEG_A, maker.client),
        vec![(1_000_000.0, engine(VENUE_B, LEG_B, hedge.client))],
        config(mount(vec![MountLeg::at(LEG_B, VENUE_B)], probe)),
    );

    // A resting LIMIT on the mount's OWN venue, then ACCEPT it.
    //
    // ⚠ The accept is load-bearing, and it is the one place this fixture differs from its
    // `cancel_tagged` siblings above. `ExecutionEngine::mass_cancel` collects
    // `registry.filter(|mo| mo.status.is_live())`, and a SUBMITTED order is not live — so without an
    // accept the engine finds nothing to pull and BOTH venues' cancel lists come back empty, which
    // looks exactly like the bug this test is for. `cancel_tagged` has no such filter, which is why
    // those tests need no accept and this one does.
    close_bar(&handle, LEG_A, 60_000, PX_A);
    let maker_coids = wait_for_submissions(&maker.submissions, 1);
    let hedge_coids = wait_for_submissions(&hedge.submissions, 1);
    let ev = handle.event_sender();
    ev.blocking_send(Event::OrderAccepted(OrderAccepted {
        client_order_id: maker_coids[0].clone(),
        venue_order_id: None,
        ts: 1,
    }))
    .unwrap();

    // ...then a fill on the HEDGE venue. This is the dispatch whose drain venue differs from the
    // mount's, and the only place the two spellings could diverge.
    //
    // ⚠ ATTRIBUTED to the hedge order's own coid, which is required rather than tidy: `Act::SeedLegs`
    // documents why — `dispatch_applied_fills` routes by minted coid and falls back to a
    // `(venue, symbol)` match that knows nothing about declared legs, so an UNattributed leg-B fill
    // never reaches a mount whose own symbol is leg A. With `fill("")` this test ran to green
    // assertions having never entered `on_fill` at all.
    ev.blocking_send(Event::Fill(fill(&hedge_coids[0], VENUE_B, LEG_B, -1, -POS_B, PX_B))).unwrap();
    close_bar(&handle, LEG_A, 120_000, PX_A);
    handle.shutdown_and_join();

    let maker_submits = maker.submissions.lock().unwrap().clone();
    let maker_cancels = maker.cancels.lock().unwrap().clone();
    let hedge_cancels = hedge.cancels.lock().unwrap().clone();
    // ⚠ The FIRST thing asserted, because everything below is vacuous without it: if `on_fill` never
    // ran, `mass_cancel()` was never called and both cancel lists are empty for a reason that has
    // nothing to do with routing. An earlier draft of this test sat in exactly that state.
    let lanes: Vec<&str> = looks.lock().unwrap().iter().map(|l| l.lane).collect();
    assert!(
        lanes.contains(&"fill"),
        "fixture: the {VENUE_B} fill must reach `on_fill`, or `mass_cancel()` was never called and \
         this test asserts nothing: lanes={lanes:?}"
    );

    // Fixture check first: the quote this test is about must actually have reached the maker venue,
    // or "it got cancelled" and "it was never placed" are the same observation.
    assert!(
        maker_submits.iter().any(|(_, sym)| sym == LEG_A),
        "fixture: the tagged quote must rest on {VENUE}: {maker_submits:?}"
    );
    assert!(
        !maker_cancels.is_empty(),
        "mass_cancel() during a {VENUE_B} fill must pull the MOUNT's book on {VENUE} — an empty \
         cancel list is the pre-fix behaviour, where a REAL quote stays resting with no error and \
         no event: maker_submits={maker_submits:?} hedge_cancels={hedge_cancels:?}"
    );
    assert!(
        hedge_cancels.is_empty(),
        "...and NOT the book of the venue the fill happened to arrive on: {hedge_cancels:?}"
    );
}

/// ⚠ **A DECLARED CROSS-VENUE LEG'S FILL MUST REACH `on_fill`.** It did not.
///
/// `runtime/mod.rs`'s `assemble_core` arms each extra engine's applied-fill capture with
/// `mounts.iter().flatten().any(|m| m.venue == e.venue)` — the mount's OWN series venue, and
/// nothing else. Its own comment says "only when a mount TRADES their venue", which is the right
/// rule; the code implemented a narrower one. A mount that declares `MountLeg::at(sym, other)` —
/// the shape an xEMM hedge requires by construction — therefore left the hedge engine's capture
/// DISARMED.
///
/// The failure is silent and asymmetric: the hedge engine folds the fill into its account
/// normally, so positions and PnL stay correct, but it records no `AppliedFill`. With nothing
/// buffered, `dispatch_applied_fills` delivers nothing and `Strategy::on_fill` never runs for that
/// leg. A cross-venue maker is never told its hedge filled — no error, no event, no ring line —
/// while its own inventory view silently diverges from the account the venue agrees with.
///
/// ⚠ WHY IT SURVIVED: no test in the workspace paired `spawn_core_multi` with a hook assertion on a
/// SECONDARY engine's venue. Every existing multi-engine test either mounts nothing on the extra
/// venue or never asks whether the hook ran, so the path had never been executed once.
///
/// FAILS ON THE PRE-FIX CODE at the first assertion, with `lanes == ["bar", "bar"]` — the hook
/// simply never runs. The read assertions after it are what prove the delivery is also CORRECT and
/// not merely present.
#[test]
fn a_cross_venue_leg_fill_reaches_on_fill() {
    let looks = Arc::new(Mutex::new(Vec::new()));
    let probe = Probe::new(&looks, vec![(60_000, Act::QuoteAndHedge)]);
    let maker = wires();
    let hedge = wires();

    let handle = spawn_core_multi(
        engine(VENUE, LEG_A, maker.client),
        vec![(1_000_000.0, engine(VENUE_B, LEG_B, hedge.client))],
        config(mount(vec![MountLeg::at(LEG_B, VENUE_B)], probe)),
    );

    close_bar(&handle, LEG_A, 60_000, PX_A);
    let hedge_coids = wait_for_submissions(&hedge.submissions, 1);
    // Price leg B in its own band, so a read that answered about leg A is identifiable.
    close_bar_on(&handle, VENUE_B, LEG_B, 90_000, PX_B);
    handle
        .event_sender()
        .blocking_send(Event::Fill(fill(&hedge_coids[0], VENUE_B, LEG_B, -1, -POS_B, PX_B)))
        .unwrap();
    close_bar(&handle, LEG_A, 120_000, PX_A);
    handle.shutdown_and_join();

    let out = looks.lock().unwrap().clone();
    let lanes: Vec<&str> = out.iter().map(|l| l.lane).collect();
    assert!(
        lanes.contains(&"fill"),
        "a fill on the DECLARED leg's venue ({VENUE_B}) must reach `on_fill`. lanes={lanes:?} — \
         `[\"bar\", \"bar\"]` is the pre-fix signature: the hedge engine's `collect_applied_fills` \
         was never armed, so the fill was folded into the account and never delivered"
    );

    // ...and the delivery is CORRECT, not merely present: the hook's per-symbol reads must answer
    // about the instrument NAMED, which is the law the rest of this file establishes.
    let l = last_of(&out, "fill");
    assert_eq!(l.pos_b, POS_B, "position({LEG_B}) inside the hedge leg's own fill: {l:?}");
    assert!(
        (50.0..100.0).contains(&l.px_b),
        "price({LEG_B}) = {} must sit in leg B's band — outside it means the read answered about \
         leg A: {l:?}",
        l.px_b
    );
}

/// ⚠ **THE ORDER-EVENT HALF of the cross-venue capture flag** — and the reason it looked broken for
/// a while when it is not.
///
/// `crates/vike-exec/src/execution_engine/mod.rs`'s `order_events` is gated by the SAME
/// `collect_applied_fills` flag the fill lane uses, so a declared cross-venue leg's lifecycle events
/// were lost by the identical mechanism and are repaired by the identical one-line arming fix. This
/// asserts that second lane directly, so a future narrowing of that condition cannot silently
/// re-break one of the two.
///
/// ⚠ **`Event::OrderSubmitted` FIRST IS LOAD-BEARING**, and omitting it is why an earlier version of
/// this test failed against a correct runtime. `ManagedOrder::new` starts at `Initialized`, and
/// nothing local publishes `OrderSubmitted` — the echo comes FROM THE ADAPTER, and `ProbeClient`
/// implements only `submit`/`cancel`. `order.rs`'s `transition` allows `OrderAccepted` only from
/// `Submitted`, so without the prefix `mo.apply` returns `Err(InvalidOrderTransition)` and
/// `on_event` takes its `Fold::Dropped` path — which sits ABOVE the capture block, making the flag's
/// value irrelevant and the whole lane untestable. `runtime/apply.rs`'s `fill_events` carries the
/// same warning for the same reason.
///
/// The drop is also SILENT for the accept hop specifically: `is_kill_terminal(Initialized)` is
/// false, so no counter moves and nothing is logged. Only the cancel bumps
/// `dropped_terminal_on_live`. That asymmetry is what made the earlier failure read as a runtime
/// bug rather than a fixture one.
///
/// ⚠ Note the already-passing `on_order_event_reads_are_symbol_addressed_on_a_declared_mount` proves
/// nothing about this path: it drives a RiskGate/undeclared REFUSAL, i.e. the `Denied` push inside
/// `gate_and_register`, which never touches the FSM at all.
///
/// FAILS with the arming fix reverted, with no `"order_event"` lane at all.
#[test]
fn a_cross_venue_leg_order_event_reaches_on_order_event() {
    let looks = Arc::new(Mutex::new(Vec::new()));
    let probe = Probe::new(&looks, vec![(60_000, Act::QuoteAndHedge)]);
    let maker = wires();
    let hedge = wires();

    let handle = spawn_core_multi(
        engine(VENUE, LEG_A, maker.client),
        vec![(1_000_000.0, engine(VENUE_B, LEG_B, hedge.client))],
        config(mount(vec![MountLeg::at(LEG_B, VENUE_B)], probe)),
    );

    close_bar(&handle, LEG_A, 60_000, PX_A);
    let hedge_coids = wait_for_submissions(&hedge.submissions, 1);

    // The full adapter sequence: Initialized -> Submitted -> Accepted -> Canceled. Every real
    // adapter emits `[OrderSubmitted, OrderAccepted|OrderRejected]` synchronously at submit.
    let ev = handle.event_sender();
    ev.blocking_send(Event::OrderSubmitted(OrderSubmitted {
        client_order_id: hedge_coids[0].clone(),
        ts: 0,
    }))
    .unwrap();
    ev.blocking_send(Event::OrderAccepted(OrderAccepted {
        client_order_id: hedge_coids[0].clone(),
        venue_order_id: None,
        ts: 1,
    }))
    .unwrap();
    ev.blocking_send(Event::OrderCanceled(OrderCanceled {
        client_order_id: hedge_coids[0].clone(),
        reason: "venue".into(),
        ts: 2,
    }))
    .unwrap();
    close_bar(&handle, LEG_A, 120_000, PX_A);
    handle.shutdown_and_join();

    let lanes: Vec<&str> = looks.lock().unwrap().iter().map(|l| l.lane).collect();
    assert!(
        lanes.contains(&"order_event"),
        "a lifecycle event on the DECLARED leg's venue ({VENUE_B}) must reach `on_order_event` — \
         without it a strategy believes a hedge is still resting when the venue has already \
         canceled it. lanes={lanes:?}"
    );
}

/// ⚠ **A `same_venue` DECLARED LEG LIVES ON THE MOUNT'S VENUE — both halves must say so.**
///
/// `strategy_drive.rs`'s `leg_venue` names this test as its gate. The test did not exist: the doc
/// landed and the test did not, so the rule had no regression protection — and writing it found
/// that the rule itself was wrong.
///
/// The original defect was a DISAGREEMENT: the read side fell back to the mount's venue, the write
/// side to the drain's. `leg_venue` unified both onto the DRAIN's venue, which makes them agree and
/// leaves both WRONG. `MountLeg::same_venue` is documented as "a leg on the mount's OWN venue", so
/// during a foreign-venue fill that leg resolved to a book it was never declared on. Measured, with
/// `LEG_C` seeded to 11.0 on `binance` and the fill arriving on `bybit`:
///
/// ```text
///   read:   position(SOLUSDT) == 0.0      resolved against bybit, which holds none
///   write:  SOLUSDT order      -> bybit   an exchange the mount never named
/// ```
///
/// ⚠ The write half is the serious one: a REAL order routed to the wrong exchange, with no error.
///
/// NON-VACUOUS in both directions, which is why BOTH halves are asserted and why the fill is
/// delivered on `bybit` — that is the only dispatch whose venue differs from the mount's, so a
/// same-venue fill would make every spelling agree by accident. Agreement alone proves nothing
/// here: the pre-fix code already had it, which is exactly how it passed review.
///
/// ⚠ Reachable only because of the cross-venue capture fix — before it, `on_fill` never ran for a
/// declared foreign leg at all, so this gate could not have been written even by someone who tried.
/// That is the likeliest reason it never was.
#[test]
fn a_same_venue_leg_reads_and_writes_the_same_book() {
    let looks = Arc::new(Mutex::new(Vec::new()));
    let probe = Probe::new(&looks, vec![(60_000, Act::QuoteAndHedge)]).submitting_on_fill(LEG_C);
    let maker = wires();
    let hedge = wires();

    let handle = spawn_core_multi(
        engine(VENUE, LEG_A, maker.client),
        vec![(1_000_000.0, engine(VENUE_B, LEG_B, hedge.client))],
        config(mount(vec![MountLeg::at(LEG_B, VENUE_B), MountLeg::same_venue(LEG_C)], probe)),
    );

    // Seed the MOUNT's venue with a position in the same-venue leg, so a read served from the WRONG
    // engine (bybit, which holds nothing in it) is distinguishable from the right one. Routed by
    // venue, so it needs no attribution and never enters `on_fill`.
    handle
        .event_sender()
        .blocking_send(Event::Fill(fill("", VENUE, LEG_C, 1, POS_C, PX_C)))
        .unwrap();
    close_bar(&handle, LEG_A, 60_000, PX_A);
    let hedge_coids = wait_for_submissions(&hedge.submissions, 1);

    // ...then a fill on the HEDGE venue: the one dispatch whose venue differs from the mount's.
    handle
        .event_sender()
        .blocking_send(Event::Fill(fill(&hedge_coids[0], VENUE_B, LEG_B, -1, -POS_B, PX_B)))
        .unwrap();
    close_bar(&handle, LEG_A, 120_000, PX_A);
    handle.shutdown_and_join();

    let out = looks.lock().unwrap().clone();
    // Fixture check FIRST, and it is what separates the two ways this can fail. The BAR lane's
    // per-symbol read is already correct and already gated above, so if the seed landed at all the
    // last bar look shows it. Zero HERE means the seed never applied (a fixture fault); zero on the
    // fill lane alone means the read resolved against the wrong engine, which is the subject.
    let last_bar = last_of(&out, "bar");
    assert_eq!(
        last_bar.pos_c, POS_C,
        "fixture: the {LEG_C} seed must reach {VENUE}'s engine, or the assertion below cannot \
         distinguish a routing bug from an empty book. looks={out:#?}"
    );

    let l = last_of(&out, "fill");
    assert_eq!(
        l.pos_c, POS_C,
        "position({LEG_C}) read during a {VENUE_B} fill must come from {VENUE}'s book — the \
         mount's own venue, which is what `MountLeg::same_venue` MEANS. {} means the leg resolved \
         against the dispatching venue instead: {l:?}",
        l.pos_c
    );

    // The WRITE half, and the one that moves real money.
    let maker_sent: Vec<String> =
        maker.submissions.lock().unwrap().iter().map(|(_, s)| s.clone()).collect();
    let hedge_sent: Vec<String> =
        hedge.submissions.lock().unwrap().iter().map(|(_, s)| s.clone()).collect();
    assert!(
        maker_sent.iter().any(|s| s == LEG_C),
        "the {LEG_C} order must route to {VENUE}, the mount's own venue: \
         maker={maker_sent:?} hedge={hedge_sent:?}"
    );
    assert!(
        !hedge_sent.iter().any(|s| s == LEG_C),
        "...and NOT to {VENUE_B} — routing a declared same-venue leg's order to the venue a fill \
         happened to arrive from sends a REAL order to an exchange the mount never named: \
         hedge={hedge_sent:?}"
    );
}

/// ⚠ **AN UNATTRIBUTED EVENT ON A DECLARED CROSS-VENUE LEG REACHED NO STRATEGY** — in BOTH lanes.
///
/// When no mount minted an order's coid, `dispatch_applied_fills` and `dispatch_order_events` each
/// fell back to a `(venue, symbol)` scan. The comment above each names exactly what reaches it —
/// "an operator ticket, a liquidation, an adopted venue order" — but the scan compared against the
/// MOUNT'S OWN series:
///
/// ```text
///     m.venue == f.venue && m.symbol == f.symbol
/// ```
///
/// On a cross-venue mount that can never match a declared leg, so the `else { continue; }` dropped
/// the event entirely. A LIQUIDATION on the hedge venue — the case a hedging strategy most needs to
/// hear about — reached `on_fill` in neither lane, with no error and no ring line.
///
/// ⚠ The predicate was written out TWICE and was wrong identically in both, which is the same shape
/// as the three bugs fixed before it. `CoreThread::mount_owning` is now the single spelling, and it
/// resolves a leg's venue through `leg_venue` rather than adding a fourth.
///
/// NON-VACUOUS: the fill carries an EMPTY coid, so `mount_for_coid` misses by construction and the
/// fallback is the only path that can deliver it — which is what makes this test about the fallback
/// rather than about attribution. Its symbol/venue are the DECLARED LEG's, never the mount's own, so
/// the pre-fix scan cannot match. FAILS on the pre-fix code with no `"fill"` lane at all.
#[test]
fn an_unattributed_fill_on_a_declared_leg_reaches_the_mount() {
    let looks = Arc::new(Mutex::new(Vec::new()));
    let probe = Probe::new(&looks, vec![]);
    let maker = wires();
    let hedge = wires();

    let handle = spawn_core_multi(
        engine(VENUE, LEG_A, maker.client),
        vec![(1_000_000.0, engine(VENUE_B, LEG_B, hedge.client))],
        config(mount(vec![MountLeg::at(LEG_B, VENUE_B)], probe)),
    );

    close_bar(&handle, LEG_A, 60_000, PX_A);
    // A fill on the DECLARED leg's venue that NO mount minted — an empty coid is the shape a
    // liquidation, an operator ticket or a reconcile-adopted order arrives with.
    handle
        .event_sender()
        .blocking_send(Event::Fill(fill("", VENUE_B, LEG_B, -1, -POS_B, PX_B)))
        .unwrap();
    close_bar(&handle, LEG_A, 120_000, PX_A);
    handle.shutdown_and_join();

    let out = looks.lock().unwrap().clone();
    let lanes: Vec<&str> = out.iter().map(|l| l.lane).collect();
    assert!(
        lanes.contains(&"fill"),
        "an unattributed fill on the declared leg ({VENUE_B}/{LEG_B}) must reach `on_fill` through \
         the ownership fallback — a liquidation on a hedge venue is exactly this shape, and \
         dropping it leaves the strategy believing it still holds the leg. lanes={lanes:?}"
    );
    let l = last_of(&out, "fill");
    assert_eq!(l.pos_b, POS_B, "and it must be delivered with the leg's own position: {l:?}");
}
