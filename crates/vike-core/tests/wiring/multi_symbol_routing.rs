//! Two-leg order routing: the opt-in lane, and the single-symbol contract it preserves.
//!
//! `impl Broker for LiveBroker` used to DISCARD the symbol argument on every order verb,
//! because `BufferedSubmit` had no symbol field and `drain_broker` stamped the MOUNT's
//! `(venue, symbol)` onto every intent. A two-leg strategy — a pairs trade, an xEMM hedge —
//! submitting `("BTCUSDT", BUY)` and `("ETHUSDT", SELL)` therefore sent BOTH orders to the
//! ONE mounted instrument in opposite directions, netting to a residual instead of a
//! spread, with no error and no denial.
//!
//! It was a LIVE-ONLY divergence: `SimBroker` routes by symbol and PANICS on an unknown one,
//! so the same strategy backtested CORRECTLY and traded wrongly.
//!
//! ## The fix, and why it is opt-in
//!
//! A mount now DECLARES the extra symbols it may trade (`StrategyMount::symbols`). Declared,
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
//! `crates/vike-backtest/tests/multi_symbol_read_parity.rs` is that half — the same portable probe run
//! through `SimBroker` and `LiveBroker`, checking one law. `position`/`price` went first; `bars`,
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
use vike_core::{CoreConfig, LiveBroker, MountLeg, StrategyMount, spawn_core};
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, BarUpdate, ExecutionEngine, RiskGate, RiskLimits};
use vike_model::{Bar, Broker, Strategy};

const VENUE: &str = "binance";
const LEG_A: &str = "BTCUSDT"; // the MOUNTED symbol
const LEG_B: &str = "ETHUSDT"; // the second leg — declared by the mount below
const UNDECLARED: &str = "SOLUSDT"; // never declared: must be refused, never rewritten

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
        symbol: None,
    }
}

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
        broker.submit_market(LEG_A, 1, 1.0); // BUY leg A
        broker.submit_market(LEG_B, -1, 2.0); // SELL leg B — distinct symbol AND qty
        if self.also_undeclared {
            broker.submit_market(UNDECLARED, 1, 3.0);
        }
    }
}

fn close(handle: &vike_core::CoreHandle, symbol: &str, b: Bar) {
    handle
        .bar_sender()
        .close(BarUpdate {
            venue: VENUE.into(),
            symbol: symbol.into(),
            interval: "1m".into(),
            bar: b,
        })
        .unwrap();
}

fn engine_accepting_both() -> ExecutionEngine<RecordingClient> {
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        VENUE,
        LEG_A,
    );
    engine.extra_symbols = vec![LEG_B.into(), UNDECLARED.into()];
    engine
}

fn run(mount_symbols: Vec<MountLeg>, also_undeclared: bool) -> Arc<vike_core::CoreSnapshot> {
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 1_000_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        strategy: Some(StrategyMount {
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
    close(&handle, LEG_A, bar(60_000, 100.0));
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

/// The contract shipped strategies depend on: a mount that declares NOTHING ignores the
/// argument entirely, exactly as before. This is why the lane is opt-in — live bars carry
/// `Bar::symbol == None`, so strategies pass `""` and mean "my mount".
#[test]
fn an_undeclared_mount_ignores_the_argument_entirely() {
    let snap = run(Vec::new(), true);

    assert_eq!(snap.orders.len(), 3, "all three submissions route (none refused)");
    assert!(
        snap.orders.iter().all(|o| o.symbol == LEG_A),
        "an undeclared mount routes every order to its own symbol, whatever was named"
    );
}

/// The same, driven through the exact strings shipped code passes — including the `""` that
/// a live bar yields. None may reach a venue as a symbol.
#[test]
fn a_single_leg_strategy_is_unaffected_by_the_argument() {
    struct OneLeg {
        done: bool,
        arg: &'static str,
    }
    impl Strategy<LiveBroker> for OneLeg {
        fn on_bar(&mut self, broker: &mut LiveBroker, _bar: &Bar) {
            if !self.done {
                self.done = true;
                broker.submit_market(self.arg, 1, 1.0);
            }
        }
    }

    for arg in ["", "literally-anything", LEG_A] {
        let t = Arc::new(AtomicI64::new(0));
        let config = CoreConfig {
            seed_cash: 1_000_000.0,
            clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
            strategy: Some(StrategyMount {
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
        close(&handle, LEG_A, bar(60_000, 100.0));
        handle.shutdown_and_join();

        let snap = cell.load();
        assert_eq!(snap.orders.len(), 1, "arg {arg:?}: one order");
        assert_eq!(
            snap.orders[0].symbol, LEG_A,
            "arg {arg:?}: a single-symbol mount always routes to its own symbol"
        );
    }
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
        strategy: Some(StrategyMount {
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
    for (i, (sym, px)) in closes.iter().enumerate() {
        close(&handle, sym, bar(60_000 + i as i64 * 60_000, *px));
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
/// the value was always `None` — `vike_backtest::cheap_np`'s spot branch
/// (`if bar.symbol.as_deref() == Some(self.spot_symbol)`) is exactly that shape, and was dead on
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
/// distinguishable. This is what `PairsZScore` needs — it buffers closes keyed on `bar.symbol`.
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
        broker.submit_market(LEG_A, 1, 1.0); // routes (the mount's own symbol)
        broker.submit_market(UNDECLARED, 1, 3.0); // REFUSED
        broker.submit_market(LEG_B, -1, 2.0); // routes (declared)
    }

    fn on_order_event(
        &mut self,
        _broker: &mut LiveBroker,
        ev: &vike_model::strategy::OrderLifecycle,
    ) {
        if let vike_model::strategy::OrderEventKind::Denied { reason } = &ev.kind {
            self.seen.lock().unwrap().push((ev.client_order_id.clone(), reason.clone()));
        }
    }
}

/// **THE OBSERVABILITY GATE.** A declaring mount that names an undeclared symbol gets a REFUSAL —
/// and the refusal is now announced on every channel a `RiskGate` veto is:
///
/// - the STRATEGY receives an `OrderEventKind::Denied` naming the symbol it asked for;
/// - the operator's recent-events ring carries an `OrderDenied` line;
/// - and the refusal consumes NO client-order-id, so the orders that DID route keep the coids they
///   would have had.
///
/// Before this, the drain simply `continue`d: the order vanished with no event, no callback and no
/// ring line — a strategy asked for something and the platform quietly did nothing.
///
/// The coid assertion is the load-bearing one for restart safety. A refusal journals nothing and
/// submits nothing, so a coid spent on it would put every later order one step ahead of the
/// `coid_seq` a journal `Snap` records — and a restart resuming that journal would re-mint an id
/// the live session had already used. The refusal id therefore comes from its own counter and
/// carries an `r` infix, which is also what makes it distinguishable here.
#[test]
fn a_refused_intent_is_denied_visibly_and_consumes_no_coid() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 1_000_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        // a PINNED coid session, so the ids below are exact rather than merely consecutive-looking
        coid_session: Some(("sess".to_string(), 0)),
        strategy: Some(StrategyMount {
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
    close(&handle, LEG_A, bar(60_000, 100.0));
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

    // (4) NO COID WAS CONSUMED — the routed orders keep the ids they would have had without the
    // refusal at all, and the refusal id comes from its own (`r`-infixed) sequence.
    let mut coids: Vec<&str> = snap.orders.iter().map(|o| o.client_order_id.as_str()).collect();
    coids.sort_unstable();
    assert_eq!(
        coids,
        vec!["sess0", "sess1"],
        "the refusal must not burn a client-order-id: a coid spent with no journal record puts \
         every later order out of phase with the `coid_seq` a Snap records"
    );
    assert_eq!(denials[0].0, "sessr0", "the refusal id is minted from its OWN counter");
}

// ---- CROSS-VENUE: a declared leg may live on a DIFFERENT exchange ----

/// THE xEMM UNBLOCK: a mount declaring a leg with `MountLeg::at(sym, other_venue)` routes that
/// leg's orders to THAT venue, while its own leg stays on the mount's.
///
/// This is what a cross-exchange strategy requires and could not have before: an xEMM maker rests
/// on one venue and hedges on another, so its two legs cannot share a venue by construction. The
/// order path used to stamp the MOUNT's venue onto every intent, so the hedge silently landed back
/// on the maker venue — the venue-level twin of the symbol collapse this file already pins.
///
/// Downstream never needed changing: `apply_intent` already routes by
/// `engine_idx_for_route_key(&req.venue)`, so once the request carries the right venue the existing
/// machinery delivers it to the right engine and client.
#[test]
fn a_leg_declared_on_another_venue_routes_there() {
    const VENUE_B: &str = "bybit";

    let mut primary = ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        VENUE,
        LEG_A,
    );
    primary.extra_symbols = vec![LEG_B.into()];
    let mut second = ExecutionEngine::new(
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
        strategy: Some(StrategyMount {
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
    close(&handle, LEG_A, bar(60_000, 100.0));
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
