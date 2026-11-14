//! Two-leg order routing: the opt-in lane, and the single-symbol contract it preserves.
//!
//! `impl Broker for LiveBroker` used to DISCARD the symbol argument on every order verb,
//! because `BufferedSubmit` had no symbol field and `drain_broker` stamped the MOUNT's
//! `(venue, symbol)` onto every intent. A two-leg strategy — a pairs trade, an xEMM hedge —
//! submitting `("BTCUSDT", BUY)` and `("ETHUSDT", SELL)` therefore sent BOTH orders to the
//! ONE mounted instrument in opposite directions, netting to a residual instead of a
//! spread, with no error and no denial.
//!
//! It was a LIVE-ONLY divergence: `OrderBrokerSimulator` routes by symbol and PANICS on an unknown one,
//! so the same strategy backtested CORRECTLY and traded wrongly.
//!
//! ## The fix, and why it is opt-in
//!
//! A mount now DECLARES the extra symbols it may trade (`Mount::symbols`). Declared,
//! the `symbol` argument becomes AUTHORITATIVE and an UNDECLARED symbol is REFUSED rather
//! than silently rewritten. Undeclared — every mount that exists today — the old contract
//! holds exactly, which shipped strategies depend on: they pass `""` (live bars carry
//! `Bar::symbol == None`) and rely on it being ignored. Honouring the argument
//! unconditionally would have sent `symbol: ""` to venues.
//!
//! `both_legs_route_to_their_own_symbols` was a CHARACTERIZATION test asserting the bug
//! (added in the commit that pinned it). It is now INVERTED to assert the fix; that
//! inversion is the regression proof.
//!
//! ## ⚠ What this does NOT make possible
//!
//! These tests cover the ORDER path and the BAR-DELIVERY path (the two blocks below). They do NOT
//! cover the strategy's READS, and "the routing bug is fixed" reads like "two-leg strategies work
//! now" without them: a strategy decides from `position`/`price`/`bars` and only then submits, so
//! routing a submission correctly while answering the reads about the wrong instrument moves the
//! divergence one step upstream instead of removing it.
//!
//! `crates/vike-simulator/tests/parity/multi_symbol_read_parity.rs` is that half — the same portable probe run
//! through `OrderBrokerSimulator` and `LiveBroker`, checking one law. `position`/`price` went first; `bars`,
//! which discarded its `symbol` argument outright, followed once `LiveBroker::bar_views` gave it a
//! per-symbol table (`live_bars_are_symbol_addressed` is the inverted pin, exactly as
//! `both_legs_route_to_their_own_symbols` is here).
//!
//! (An earlier revision of this section claimed the BAR lane still matched the full
//! `(venue, symbol, interval)` triple, that live bars carried `Bar::symbol == None`, and that
//! `PairsZScore` therefore could not distinguish the legs' bars. All three went stale with #924 and
//! are disproven by `a_declared_mount_receives_both_legs_bars_distinguishably` in this very file.)

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use vike_core::{CoreConfig, LiveBroker, Mount, MountLeg, spawn_core};
use vike_execution::testing::RecordingClient;
use vike_execution::{Account, BalanceMode, Engine, RiskGate};
use vike_marketdata::Bar;
use vike_marketdata::test_support::flat_bar_unit_volume;
use vike_model::RiskLimits;
use vike_model::{OrderBroker, OrderPlacement, Strategy};

use crate::kit::handle::close_binance_bar;

const VENUE: &str = "binance";
const LEG_A: &str = "BTCUSDT"; // the MOUNTED symbol
const LEG_B: &str = "ETHUSDT"; // the second leg — declared by the mount below
const UNDECLARED: &str = "SOLUSDT"; // never declared: must be refused, never rewritten

/// The minimal shape of every two-leg strategy: on the first bar, buy one named symbol and
/// sell another. Deliberately NOT `PairsZScore` — this needs no signal, no window and no
/// cross-crate dependency to exercise the routing, and a probe cannot drift with a
/// strategy's parameters.
struct TwoLegProbe {
    done: bool,
    /// When set, ALSO submit for a symbol the mount never declared.
    also_undeclared: bool,
}

impl Strategy<LiveBroker> for TwoLegProbe {
    fn on_bar(&mut self, broker: &mut LiveBroker, _bar: &Bar) {
        if self.done {
            return;
        }
        self.done = true;
        broker.place(LEG_A, "", 1, 1.0, OrderPlacement::Market, false); // BUY leg A
        broker.place(LEG_B, "", -1, 2.0, OrderPlacement::Market, false); // SELL leg B — distinct symbol AND qty
        if self.also_undeclared {
            broker.place(UNDECLARED, "", 1, 3.0, OrderPlacement::Market, false);
        }
    }
}

fn engine_accepting_both() -> Engine<RecordingClient> {
    let mut engine = Engine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        VENUE,
        LEG_A,
    );
    engine.extra_symbols = vec![LEG_B.into(), UNDECLARED.into()];
    engine
}

fn run(mount_symbols: Vec<MountLeg>, also_undeclared: bool) -> Arc<vike_execution::CoreSnapshot> {
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 1_000_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        strategy: Some(Mount {
            order_client_id_code: None,
            account: None,
            symbols: mount_symbols,
            controller_id: None,
            underlying_symbol: None,
            venue: VENUE.into(),
            symbol: LEG_A.into(),
            interval: "1m".into(),
            strategy: Box::new(TwoLegProbe { done: false, also_undeclared }),
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine_accepting_both(), config);
    let cell = handle.snapshot_cell();
    close_binance_bar(&handle, LEG_A, flat_bar_unit_volume(60_000, 100.0));
    handle.shutdown_and_join();
    Arc::clone(&cell.load())
}

/// THE REGRESSION PROOF (inverted from the characterization test that pinned the bug): a
/// mount that DECLARES the second leg routes each leg to its OWN symbol.
///
/// Before the lane, both legs reached the venue carrying the MOUNTED symbol in opposite
/// directions and netted to a residual — silently. The assertions below are the exact
/// inverse of what this test asserted then.
#[test]
fn both_legs_route_to_their_own_symbols() {
    let snap = run(vec![MountLeg::same_venue(LEG_B)], false);

    let mut legs: Vec<(String, i32, f64)> =
        snap.orders.iter().map(|o| (o.symbol.clone(), o.side, o.qty)).collect();
    legs.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap());

    assert_eq!(legs.len(), 2, "the probe submits exactly two legs, got {legs:?}");
    assert_eq!(legs[0], (LEG_A.to_string(), 1, 1.0), "leg A routes to its own symbol");

    // THE INVERSION: leg B was submitted for ETHUSDT and now REACHES the venue as ETHUSDT.
    assert_eq!(
        legs[1],
        (LEG_B.to_string(), -1, 2.0),
        "leg B must route to {LEG_B}; before the lane it collapsed onto {LEG_A}"
    );

    // The harm the old behaviour caused, asserted as its own condition: the legs must NOT
    // net out on one instrument.
    let net_on_mount: f64 = legs
        .iter()
        .filter(|(s, _, _)| s == LEG_A)
        .map(|(_, side, qty)| f64::from(*side) * qty)
        .sum();
    assert!(
        (net_on_mount - 1.0).abs() < 1e-12,
        "leg A alone must remain on {LEG_A} (+1.0), got {net_on_mount} — the legs are netting again"
    );
}

/// A mount that OPTED IN must REFUSE an undeclared symbol rather than silently rewriting it
/// to its own — silent rewriting is the original bug in miniature. The declared legs still
/// route; only the undeclared one is dropped.
#[test]
fn a_declared_mount_refuses_an_undeclared_symbol() {
    let snap = run(vec![MountLeg::same_venue(LEG_B)], true);

    assert_eq!(
        snap.orders.len(),
        2,
        "the two DECLARED legs route; the undeclared third is refused, not rewritten"
    );
    assert!(
        !snap.orders.iter().any(|o| o.symbol == UNDECLARED),
        "an undeclared symbol must never reach a venue — silently rewriting it to the mount's \
         own symbol is the original bug"
    );
}

/// On a LEG's bar, buy `""` and the mount's own symbol by name, sell the leg.
struct OnLegBar {
    done: bool,
}

impl Strategy<LiveBroker> for OnLegBar {
    fn on_bar(&mut self, broker: &mut LiveBroker, bar: &Bar) {
        if self.done || bar.symbol.as_deref() != Some(LEG_B) {
            return;
        }
        self.done = true;
        broker.place("", "", 1, 1.0, OrderPlacement::Market, false);
        broker.place(LEG_A, "", 1, 2.0, OrderPlacement::Market, false);
        broker.place(LEG_B, "", -1, 3.0, OrderPlacement::Market, false);
    }
}

/// **On a LEG's bar the mount's own symbol is still the mount's** (one broker plan, step 12; decision
/// 0277's rule 1). A two-leg strategy decides on whichever leg's bar completes its pair, so its
/// orders on the mount's own symbol come from a dispatch whose series is the LEG's. `""` used to
/// resolve to that dispatching leg (an order meant for A went to B), and the own symbol by NAME was
/// refused as undeclared (it is the mount's, never a declared leg).
#[test]
fn on_a_legs_bar_the_empty_symbol_and_the_own_name_trade_the_mount() {
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 1_000_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        strategy: Some(Mount {
            order_client_id_code: None,
            account: None,
            symbols: vec![MountLeg::same_venue(LEG_B)],
            controller_id: None,
            underlying_symbol: None,
            venue: VENUE.into(),
            symbol: LEG_A.into(),
            interval: "1m".into(),
            strategy: Box::new(OnLegBar { done: false }),
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine_accepting_both(), config);
    let cell = handle.snapshot_cell();
    close_binance_bar(&handle, LEG_B, flat_bar_unit_volume(60_000, 50.0));
    handle.shutdown_and_join();

    let mut orders: Vec<(String, i32, f64)> =
        cell.load().orders.iter().map(|o| (o.symbol.clone(), o.side, o.qty)).collect();
    orders.sort_by(|a, b| a.2.total_cmp(&b.2));
    assert_eq!(
        orders,
        vec![
            (LEG_A.to_string(), 1, 1.0),
            (LEG_A.to_string(), 1, 2.0),
            (LEG_B.to_string(), -1, 3.0)
        ],
        "`\"\"` and the own name trade the mount's own symbol on the leg's bar; the leg trades the leg"
    );
}

/// A mount that declares NOTHING trades its own symbol and nothing else: an order naming another
/// is REFUSED, never rewritten to the mount's own (it used to be, so `on("ETHUSDT")` then a buy
/// bought BTCUSDT). Inverted from the characterization test that pinned the rewrite.
#[test]
fn an_undeclared_mount_trades_its_own_symbol_and_refuses_another() {
    let snap = run(Vec::new(), true);

    assert_eq!(snap.orders.len(), 1, "only the order on the mount's own symbol routes");
    assert_eq!((snap.orders[0].symbol.as_str(), snap.orders[0].qty), (LEG_A, 1.0));
}

/// The strings shipped code passes: the `""` that means "my mount" and the mount's own symbol (a
/// live bar's, stamped with its series) route to it; any other symbol is refused, not rewritten.
#[test]
fn a_single_leg_strategy_routes_its_own_symbol_and_nothing_else() {
    struct OneLeg {
        done: bool,
        arg: &'static str,
    }
    impl Strategy<LiveBroker> for OneLeg {
        fn on_bar(&mut self, broker: &mut LiveBroker, _bar: &Bar) {
            if !self.done {
                self.done = true;
                broker.place(self.arg, "", 1, 1.0, OrderPlacement::Market, false);
            }
        }
    }

    for arg in ["", "literally-anything", LEG_A] {
        let t = Arc::new(AtomicI64::new(0));
        let config = CoreConfig {
            seed_cash: 1_000_000.0,
            clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
            strategy: Some(Mount {
                order_client_id_code: None,
                account: None,
                symbols: Vec::new(),
                controller_id: None,
                underlying_symbol: None,
                venue: VENUE.into(),
                symbol: LEG_A.into(),
                interval: "1m".into(),
                strategy: Box::new(OneLeg { done: false, arg }),
            }),
            ..CoreConfig::default()
        };
        let handle = spawn_core(engine_accepting_both(), config);
        let cell = handle.snapshot_cell();
        close_binance_bar(&handle, LEG_A, flat_bar_unit_volume(60_000, 100.0));
        handle.shutdown_and_join();

        let snap = cell.load();
        if arg == "literally-anything" {
            assert!(snap.orders.is_empty(), "another symbol is refused: {:?}", snap.orders);
            continue;
        }
        assert_eq!(snap.orders.len(), 1, "arg {arg:?}: one order");
        assert_eq!(
            snap.orders[0].symbol, LEG_A,
            "arg {arg:?}: a single-symbol mount routes to its own symbol"
        );
    }
}

/// Each `Denied` a strategy heard: its tag and its words.
type Denials = Arc<std::sync::Mutex<Vec<(Option<String>, String)>>>;

/// What a strategy hears refused ([`Denials`]).
struct HeardDenials<S> {
    inner: S,
    heard: Denials,
}

impl<S: Strategy<LiveBroker>> Strategy<LiveBroker> for HeardDenials<S> {
    fn on_bar(&mut self, broker: &mut LiveBroker, bar: &Bar) {
        self.inner.on_bar(broker, bar);
    }
    fn on_order_event(&mut self, broker: &mut LiveBroker, event: &vike_model::OrderEvent) {
        if let vike_model::OrderEventKind::Denied { reason } = &event.kind {
            self.heard.lock().unwrap().push((event.tag.clone(), reason.clone()));
        }
        self.inner.on_order_event(broker, event);
    }
}

/// The order language's `on` on a LIVE mount that declares no other symbol: a script that makes
/// ETHUSDT current and buys it (an order, and an entry with its stop, which goes as one order
/// list) must send NOTHING on the mount's own BTCUSDT. Each order is refused by name instead,
/// under its own tag, in words naming both symbols, and the operator's ring says so.
#[test]
fn on_another_symbol_is_refused_live_never_sent_on_the_mounts_own() {
    let src = r#"fn trades() { ["ETHUSDT"] }
                 fn on_bar(strategy) {
                     strategy.on("ETHUSDT").entry("L", OrderSide::Long);
                     strategy.exit("L").stop(90.0);
                     strategy.order("M", OrderSide::Long);
                 }"#;
    let inner = vike_rhai::RhaiStrategy::<LiveBroker>::compile(src).expect("compiles");
    let heard: Denials = Arc::default();
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 1_000_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        strategy: Some(Mount {
            order_client_id_code: None,
            account: None,
            symbols: Vec::new(),
            controller_id: None,
            underlying_symbol: None,
            venue: VENUE.into(),
            symbol: LEG_A.into(),
            interval: "1m".into(),
            strategy: Box::new(HeardDenials { inner, heard: Arc::clone(&heard) }),
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine_accepting_both(), config);
    let cell = handle.snapshot_cell();
    close_binance_bar(&handle, LEG_A, flat_bar_unit_volume(60_000, 100.0));
    handle.shutdown_and_join();
    let snap = Arc::clone(&cell.load());

    assert!(snap.orders.is_empty(), "nothing reached a venue: {:?}", snap.orders);
    let heard = heard.lock().unwrap().clone();
    let tags: Vec<Option<&str>> = heard.iter().map(|(tag, _)| tag.as_deref()).collect();
    assert_eq!(
        tags,
        [Some("ETHUSDT/M#3"), Some("ETHUSDT/L#1"), Some("ETHUSDT/L#2")],
        "each order is denied under its own tag, the list's legs each: {heard:?}"
    );
    assert!(
        heard.iter().all(|(_, why)| why.contains(LEG_A) && why.contains(LEG_B)),
        "the words name both symbols: {heard:?}"
    );
    assert!(
        snap.recent_events.iter().any(|l| l.contains("OrderDenied") && l.contains(LEG_B)),
        "the operator's ring names the refusal: {:?}",
        snap.recent_events
    );
}

/// **The same script on a mount that DECLARES ETHUSDT trades it** (one broker plan, step 12): the
/// script's `fn trades()` is its `Strategy::symbols`, which the daemon declares as the mount's leg, and
/// then the order and the entry with its stop (one order list, naming ETHUSDT) are lowered on ETHUSDT,
/// nothing refused and nothing sent on the mount's own BTCUSDT.
#[test]
fn on_a_declared_symbol_trades_it_live() {
    let src = r#"fn trades() { ["ETHUSDT"] }
                 fn on_bar(strategy) {
                     strategy.on("ETHUSDT").entry("L", OrderSide::Long);
                     strategy.exit("L").stop(90.0);
                     strategy.order("M", OrderSide::Long);
                 }"#;
    let inner = vike_rhai::RhaiStrategy::<LiveBroker>::compile(src).expect("compiles");
    assert_eq!(Strategy::<LiveBroker>::symbols(&inner), vec![LEG_B.to_string()], "it declares");
    let heard: Denials = Arc::default();
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 1_000_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        strategy: Some(Mount {
            order_client_id_code: None,
            account: None,
            symbols: vec![MountLeg::same_venue(LEG_B)],
            controller_id: None,
            underlying_symbol: None,
            venue: VENUE.into(),
            symbol: LEG_A.into(),
            interval: "1m".into(),
            strategy: Box::new(HeardDenials { inner, heard: Arc::clone(&heard) }),
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine_accepting_both(), config);
    let cell = handle.snapshot_cell();
    close_binance_bar(&handle, LEG_A, flat_bar_unit_volume(60_000, 100.0));
    handle.shutdown_and_join();
    let snap = Arc::clone(&cell.load());

    assert!(heard.lock().unwrap().is_empty(), "nothing refused: {:?}", heard.lock().unwrap());
    let symbols: Vec<&str> = snap.orders.iter().map(|o| o.symbol.as_str()).collect();
    assert!(
        !symbols.is_empty() && symbols.iter().all(|s| *s == LEG_B),
        "every order is on the declared ETHUSDT, none on the mount's own: {:?}",
        snap.orders
    );
}

// ---- the DATA half: bars carry their series symbol, and declared legs arrive ----

/// Records the `bar.symbol` of every bar the strategy is handed.
struct SymbolRecorder(Arc<std::sync::Mutex<Vec<Option<String>>>>);

impl Strategy<LiveBroker> for SymbolRecorder {
    fn on_bar(&mut self, _b: &mut LiveBroker, bar: &Bar) {
        self.0.lock().unwrap().push(bar.symbol.clone());
    }
}

fn run_recorder(mount_symbols: Vec<MountLeg>, closes: &[(&str, f64)]) -> Vec<Option<String>> {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 1_000_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        strategy: Some(Mount {
            order_client_id_code: None,
            account: None,
            symbols: mount_symbols,
            controller_id: None,
            underlying_symbol: None,
            venue: VENUE.into(),
            symbol: LEG_A.into(),
            interval: "1m".into(),
            strategy: Box::new(SymbolRecorder(Arc::clone(&seen))),
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine_accepting_both(), config);
    for (i, (symbol_name, px)) in closes.iter().enumerate() {
        close_binance_bar(
            &handle,
            symbol_name,
            flat_bar_unit_volume(60_000 + i as i64 * 60_000, *px),
        );
    }
    handle.shutdown_and_join();

    seen.lock().unwrap().clone()
}

/// A LIVE bar now carries its series symbol.
///
/// Live bars arrive with `symbol: None` (`kline_to_bar` builds them from a venue kline that
/// carries no vike symbol); the runtime is the first place that knows it, and used to stamp it
/// only onto the COPY handed to the paper client. Withholding it from the strategy made a mount
/// structurally unable to tell one series from another.
///
/// FAILS ON THE PRE-STAMP CODE: the recorded symbol was `None`.
#[test]
fn a_live_bar_carries_its_series_symbol() {
    let seen = run_recorder(Vec::new(), &[(LEG_A, 100.0)]);
    assert_eq!(seen.len(), 1, "one bar dispatched");
    assert_eq!(
        seen[0].as_deref(),
        Some(LEG_A),
        "the strategy must be able to tell WHICH series this bar belongs to"
    );
}

/// ⚠ THE BEHAVIOUR CHANGE THIS STAMP CAUSES, PINNED DELIBERATELY.
///
/// Any live strategy that compares `bar.symbol` to something could never match before, because
/// the value was always `None` — the file `strategies/prediction_markets/cheap_catch_updown_fair_value.rs`'s
/// `CheapNp` spot branch (`if bar.symbol.as_deref() == Some(self.spot_symbol)`) is exactly that
/// shape, and was dead on
/// the live path. It now fires when the mounted series IS that symbol.
///
/// That is a latent bug being fixed rather than a regression, but it is a real change to a
/// shipped strategy's live behaviour, so it is recorded here rather than left to be discovered:
/// a `Some(_)` comparison that used to be unreachable live is now reachable.
#[test]
fn a_symbol_comparison_that_was_dead_live_now_matches() {
    let seen = run_recorder(Vec::new(), &[(LEG_A, 100.0)]);
    let matches_mounted = seen.iter().any(|s| s.as_deref() == Some(LEG_A));
    assert!(
        matches_mounted,
        "a `bar.symbol == Some(mounted)` comparison is now reachable live; before the stamp it \
         could never be true"
    );
}

/// THE DATA HALF OF TWO-LEG TRADING: a DECLARED mount receives BOTH legs' bars, and they are
/// distinguishable. This is what the file `strategies/pairs/pairs_zscore.rs`'s `PairsZScore` needs — it
/// buffers closes keyed on `bar.symbol`.
///
/// FAILS ON THE PRE-CHANGE CODE twice over: leg B's bars never reached the mount (the bar-lane
/// predicate matched the full `(venue, symbol, interval)` triple), and both bars carried `None`.
#[test]
fn a_declared_mount_receives_both_legs_bars_distinguishably() {
    let seen = run_recorder(vec![MountLeg::same_venue(LEG_B)], &[(LEG_A, 100.0), (LEG_B, 50.0)]);
    assert_eq!(seen.len(), 2, "both legs' bars must reach the mount, got {seen:?}");
    assert!(seen.iter().any(|s| s.as_deref() == Some(LEG_A)), "leg A bar missing: {seen:?}");
    assert!(seen.iter().any(|s| s.as_deref() == Some(LEG_B)), "leg B bar missing: {seen:?}");
}

/// An UNDECLARED mount still receives ONLY its own series — the widening is inert without a
/// declaration, so no existing mount starts seeing a neighbour's bars.
#[test]
fn an_undeclared_mount_still_receives_only_its_own_bars() {
    let seen = run_recorder(Vec::new(), &[(LEG_A, 100.0), (LEG_B, 50.0)]);
    assert_eq!(seen.len(), 1, "only the mounted series may reach an undeclared mount: {seen:?}");
    assert_eq!(seen[0].as_deref(), Some(LEG_A));
}

// ---- the refusal is OBSERVABLE: the strategy hears it and the operator sees it ----

/// The two-leg probe plus an `on_order_event` recorder — the refusal only counts as observable if
/// the strategy that asked actually receives something.
struct DeniedRecorder {
    done: bool,
    seen: Arc<std::sync::Mutex<Vec<(String, String)>>>,
}

impl Strategy<LiveBroker> for DeniedRecorder {
    fn on_bar(&mut self, broker: &mut LiveBroker, _bar: &Bar) {
        if self.done {
            return;
        }
        self.done = true;
        broker.place(LEG_A, "", 1, 1.0, OrderPlacement::Market, false); // routes (the mount's own symbol)
        broker.place(UNDECLARED, "", 1, 3.0, OrderPlacement::Market, false); // REFUSED
        broker.place(LEG_B, "", -1, 2.0, OrderPlacement::Market, false); // routes (declared)
    }

    fn on_order_event(&mut self, _broker: &mut LiveBroker, event: &vike_model::OrderEvent) {
        if let vike_model::OrderEventKind::Denied { reason } = &event.kind {
            self.seen.lock().unwrap().push((event.order_client_id.clone(), reason.clone()));
        }
    }
}

/// **THE OBSERVABILITY GATE.** A declaring mount that names an undeclared symbol gets a REFUSAL —
/// and the refusal is now announced on every channel a `RiskGate` veto is:
///
/// - the STRATEGY receives an `OrderEventKind::Denied` naming the symbol it asked for;
/// - the operator's recent-events ring carries an `OrderDenied` line;
/// - and the refusal consumes NO order-client-id, so the orders that DID route keep the order client ids they
///   would have had.
///
/// Before this, the drain simply `continue`d: the order vanished with no event, no callback and no
/// ring line — a strategy asked for something and the platform quietly did nothing.
///
/// The order client id assertion is the load-bearing one for restart safety. A refusal journals nothing and
/// submits nothing, so an order client id spent on it would put every later order one step ahead of the
/// `order_client_id_sequence` a journal `Snapshot` records — and a restart resuming that journal would re-mint an id
/// the live session had already used. The refusal id therefore comes from its own counter and
/// carries an `r` infix, which is also what makes it distinguishable here.
#[test]
fn a_refused_intent_is_denied_visibly_and_consumes_no_order_client_id() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 1_000_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        // a PINNED order client id session, so the ids below are exact rather than merely consecutive-looking
        order_client_id_session: Some(("sess".to_string(), 0)),
        strategy: Some(Mount {
            order_client_id_code: None,
            account: None,
            symbols: vec![MountLeg::same_venue(LEG_B)],
            controller_id: None,
            underlying_symbol: None,
            venue: VENUE.into(),
            symbol: LEG_A.into(),
            interval: "1m".into(),
            strategy: Box::new(DeniedRecorder { done: false, seen: Arc::clone(&seen) }),
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine_accepting_both(), config);
    let cell = handle.snapshot_cell();
    close_binance_bar(&handle, LEG_A, flat_bar_unit_volume(60_000, 100.0));
    handle.shutdown_and_join();
    let snap = Arc::clone(&cell.load());

    // (1) the refusal still refuses — nothing reached the venue for the undeclared symbol
    assert_eq!(snap.orders.len(), 2, "the two declared legs route: {:?}", snap.orders);
    assert!(
        !snap.orders.iter().any(|o| o.symbol == UNDECLARED),
        "an undeclared symbol must never reach a venue"
    );

    // (2) THE STRATEGY HEARD IT
    let denials = seen.lock().unwrap().clone();
    assert_eq!(denials.len(), 1, "exactly one denial reached the strategy: {denials:?}");
    assert!(
        denials[0].1.contains(UNDECLARED),
        "the denial names the symbol the strategy asked for: {:?}",
        denials[0].1
    );

    // (3) THE OPERATOR SEES IT — the same recent-events ring a RiskGate veto lands in
    let denied_line = snap
        .recent_events
        .iter()
        .find(|l| l.contains("OrderDenied"))
        .unwrap_or_else(|| panic!("no OrderDenied line in the ring: {:?}", snap.recent_events));
    assert!(
        denied_line.contains(UNDECLARED),
        "the ring line names the refused symbol: {denied_line}"
    );
    assert!(
        denied_line.contains(&denials[0].0),
        "the ring line and the strategy callback name the SAME refusal: {denied_line} vs {:?}",
        denials[0].0
    );

    // (4) NO ORDER CLIENT ID WAS CONSUMED — the routed orders keep the ids they would have had without the
    // refusal at all, and the refusal id comes from its own (`r`-infixed) sequence.
    let mut order_client_ids: Vec<&str> =
        snap.orders.iter().map(|o| o.order_client_id.as_str()).collect();
    order_client_ids.sort_unstable();
    assert_eq!(
        order_client_ids,
        vec!["sess0", "sess1"],
        "the refusal must not burn an order-client-id: an order client id spent with no journal record puts \
         every later order out of phase with the `order_client_id_sequence` a Snapshot records"
    );
    assert_eq!(denials[0].0, "sessr0", "the refusal id is minted from its OWN counter");
}

// ---- CROSS-VENUE: a declared leg may live on a DIFFERENT exchange ----

/// THE xEMM UNBLOCK: a mount declaring a leg with `MountLeg::at(symbol, other_venue)` routes that
/// leg's orders to THAT venue, while its own leg stays on the mount's.
///
/// This is what a cross-exchange strategy requires and could not have before: an xEMM maker rests
/// on one venue and hedges on another, so its two legs cannot share a venue by construction. The
/// order path used to stamp the MOUNT's venue onto every intent, so the hedge silently landed back
/// on the maker venue — the venue-level twin of the symbol collapse this file already pins.
///
/// Downstream never needed changing: `apply_intent` already routes by
/// `engine_index_for_route_key(&req.venue)`, so once the request carries the right venue the existing
/// machinery delivers it to the right engine and client.
#[test]
fn a_leg_declared_on_another_venue_routes_there() {
    const VENUE_B: &str = "bybit";

    let mut primary = Engine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        VENUE,
        LEG_A,
    );
    primary.extra_symbols = vec![LEG_B.into()];
    let mut second = Engine::new(
        Account::new(1.0, VENUE_B, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        VENUE_B,
        LEG_B,
    );
    second.extra_symbols = vec![LEG_A.into()];

    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 1_000_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        strategy: Some(Mount {
            order_client_id_code: None,
            account: None,
            // LEG_B trades on the OTHER venue — the cross-exchange declaration.
            symbols: vec![MountLeg::at(LEG_B, VENUE_B)],
            controller_id: None,
            underlying_symbol: None,
            venue: VENUE.into(),
            symbol: LEG_A.into(),
            interval: "1m".into(),
            strategy: Box::new(TwoLegProbe { done: false, also_undeclared: false }),
        }),
        ..CoreConfig::default()
    };
    let handle = vike_core::spawn_core_multi(primary, vec![(1_000_000.0, second)], config);
    let cell = handle.snapshot_cell();
    close_binance_bar(&handle, LEG_A, flat_bar_unit_volume(60_000, 100.0));
    handle.shutdown_and_join();

    let snap = cell.load();
    let mut legs: Vec<(String, String, i32)> =
        snap.orders.iter().map(|o| (o.venue.clone(), o.symbol.clone(), o.side)).collect();
    legs.sort();

    assert_eq!(legs.len(), 2, "both legs must reach a venue, got {legs:?}");
    assert!(
        legs.contains(&(VENUE.to_string(), LEG_A.to_string(), 1)),
        "leg A stays on the mount's own venue: {legs:?}"
    );
    assert!(
        legs.contains(&(VENUE_B.to_string(), LEG_B.to_string(), -1)),
        "leg B must route to {VENUE_B}; before this it collapsed onto {VENUE}: {legs:?}"
    );
}

/// A declared leg WITHOUT a venue stays on the mount's own — the same-exchange two-leg case is
/// untouched by adding cross-venue support.
#[test]
fn a_leg_declared_without_a_venue_stays_on_the_mounts_own() {
    let snap = run(vec![MountLeg::same_venue(LEG_B)], false);
    assert_eq!(snap.orders.len(), 2);
    assert!(
        snap.orders.iter().all(|o| o.venue == VENUE),
        "no venue override ⇒ every leg on the mount's venue: {:?}",
        snap.orders.iter().map(|o| (&o.venue, &o.symbol)).collect::<Vec<_>>()
    );
}
