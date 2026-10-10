//! Attribution of a strategy's OWN orders that do not lower through a plain `Submit`: the stop a
//! mount armed and the combo a mount built. Both used to carry no mount, so their fill and their
//! refusal reached whichever mount happened to be FIRST on the pair (bug B's two leftover paths).
//!
//! A child of `multi_mount.rs` on purpose: it reuses that file's private `Listener`, `Heard` and
//! `coid_fill`, and the parent is already near the crate's 1,000-line file cap.

use super::*;
use crate::runtime::test_support::{core_of, engine_on};

/// The `hook`s heard by every mount, in delivery order, as `(label, hook)` pairs.
fn heard_of(heard: &Heard, hook: &str) -> Vec<&'static str> {
    heard.lock().unwrap().iter().filter(|(_, h)| *h == hook).map(|(l, _)| *l).collect()
}

/// A [`Listener`] mount under its own `controller_id` that arms one sell stop at `stop_px`.
fn stopper_mount(
    label: &'static str,
    controller_id: &str,
    stop_px: f64,
    heard: &Heard,
) -> StrategyMount {
    StrategyMount {
        strategy: Box::new(Listener {
            label,
            quote_on_bar: false,
            stop_px: Some(stop_px),
            heard: Arc::clone(heard),
        }),
        ..listener_mount(label, "1m", controller_id, false, heard)
    }
}

/// **A STOP FIRES FOR THE MOUNT THAT ARMED IT.** Two mounts share one pair; B arms a protective stop
/// and A arms nothing. When the stop fires and fills, B's `on_fill` hears it and B's ledger moves; A
/// hears nothing.
///
/// Pre-fix the fire minted its order with no owner (`submit_fired` never wrote `coid_mount`), so the
/// fill was attributed to NO ledger and `mount_for_coid` missed, so the first-mount fallback handed
/// B's stop fill to A.
#[test]
fn a_fired_stop_fill_reaches_the_mount_that_armed_it() {
    let heard: Heard = Arc::new(TestMutex::new(Vec::new()));
    let mut core = listener_core(vec![
        listener_mount("A", "1m", "maker-a", false, &heard),
        stopper_mount("B", "maker-b", 90.0, &heard),
    ]);
    core.engine.collect_applied_fills = true;

    core.drive_strategy_feed_status("sim", "BTCUSDT", FeedStatus::Disconnected);
    assert_eq!(core.cond_engine.len(), 1, "B's stop is armed");
    // a bar whose low takes out the sell stop at 90
    let crash = Bar { low: 89.0, close: 89.0, ..close_bar_ohlc(100.0) };
    core.fire_conditionals_bar("sim", "BTCUSDT", &crash);
    let coid = core.engine.client.submissions[0].client_order_id.clone();

    core.bus.publish(Event::Fill(coid_fill(&coid, -1, 1.0, 90.0)), &mut core.engine);
    core.dispatch_applied_fills();

    assert_eq!(heard_of(&heard, "fill"), vec!["B"], "only the arming mount hears its stop's fill");
    assert_eq!(core.mount_attr[1].size.to_bits(), (-1.0_f64).to_bits(), "B's ledger moved");
    assert_eq!(core.mount_attr[0].size.to_bits(), 0.0_f64.to_bits(), "A's ledger did not");
}

/// A flat bar at `px`, the base of the crash bar above.
fn close_bar_ohlc(px: f64) -> Bar {
    Bar {
        ts: 1,
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

/// **A DENIED COMBO REACHES THE MOUNT THAT BUILT IT.** `lower_submit`'s RiskGate refusal returns the
/// coid it minted so the choke point attributes it; `lower_combo`'s returned nothing, so the
/// `OrderDenied` found its owner only through the first-mount fallback.
///
/// On deribit, the one venue whose caps row takes a combo. Both mounts sit on the combo's first leg,
/// so the fallback would pick A; the combo is built by B.
#[test]
fn a_denied_combo_reaches_the_mount_that_built_it() {
    const LEG: &str = "BTC-27MAR26-100000-C";
    let heard: Heard = Arc::new(TestMutex::new(Vec::new()));
    let on_deribit = |label, id: &str| StrategyMount {
        venue: "deribit".into(),
        symbol: LEG.into(),
        ..listener_mount(label, "1m", id, false, &heard)
    };
    let config = CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(on_deribit("A", "maker-a")),
        extra_mounts: vec![on_deribit("B", "maker-b")],
        ..CoreConfig::default()
    };
    let mut core =
        core_of(engine_on("deribit", LEG, RecordingClient::default()), Vec::new(), config);
    core.engine.collect_applied_fills = true;
    core.engine.trading_state = vike_exec::TradingState::Halted; // the gate's kill switch denies it
    let spec = vike_model::ComboSpec {
        venue: "deribit".into(),
        side: 1,
        qty: 1.0,
        legs: vec![
            vike_model::ComboLeg { symbol: LEG.into(), ratio: 1 },
            vike_model::ComboLeg { symbol: "BTC-27MAR26-120000-C".into(), ratio: -1 },
        ],
        net_limit: Some(-0.0125),
        time_in_force: vike_model::TimeInForce::Gtc,
    };

    // mount B's combo, lowered through the one choke point a strategy's intents take
    let coids = core.apply_strategy_intent(OrderIntent::Combo(Box::new(spec)), 5, 1);
    assert_eq!(coids.len(), 1, "the refusal names the order it refused");
    assert_eq!(core.coid_mount.get(&coids[0]), Some(&1), "...and it is attributed to B");
    core.dispatch_applied_fills();

    assert_eq!(heard_of(&heard, "order_event"), vec!["B"], "only the builder hears the denial");
}

/// **A COMBO ON A VENUE WITH NO COMBO SUPPORT IS REJECTED TO ITS BUILDER.** `lower_combo` registers the
/// order and publishes `OrderSubmitted` + `OrderRejected`, so the rejection is an event for an order a
/// mount minted; the capability reject of `lower_submit` names its coid and so must this. The registry's
/// copy of a combo has an empty symbol, so the old first-mount fallback never matched it either: pre-fix
/// NOBODY heard the rejection.
#[test]
fn an_unsupported_venue_combo_rejection_reaches_the_mount_that_built_it() {
    let heard: Heard = Arc::new(TestMutex::new(Vec::new()));
    let mut core = listener_core(vec![
        listener_mount("A", "1m", "maker-a", false, &heard),
        listener_mount("B", "1m", "maker-b", false, &heard),
    ]);
    core.engine.collect_applied_fills = true;
    let spec = vike_model::ComboSpec {
        venue: "sim".into(), // the sim venue's caps row takes no combo
        side: 1,
        qty: 1.0,
        legs: vec![
            vike_model::ComboLeg { symbol: "BTCUSDT".into(), ratio: 1 },
            vike_model::ComboLeg { symbol: "ETHUSDT".into(), ratio: -1 },
        ],
        net_limit: Some(1.0),
        time_in_force: vike_model::TimeInForce::Gtc,
    };

    let coids = core.apply_strategy_intent(OrderIntent::Combo(Box::new(spec)), 5, 1);
    assert_eq!(coids.len(), 1, "the reject names the order it rejected");
    core.dispatch_applied_fills();

    assert_eq!(heard_of(&heard, "order_event"), vec!["B"], "only the builder hears the rejection");
}

// ---- an event NO mount minted reaches NO mount (decision 0116) --------------------------------

/// Records `broker.position("BTCUSDT")` on every closed bar, so a test can prove a strategy sees an
/// outside move through the broker read instead of through a callback.
struct PositionReader {
    seen: Arc<TestMutex<Vec<f64>>>,
}

impl Strategy<LiveBroker> for PositionReader {
    fn on_bar(&mut self, broker: &mut LiveBroker, _bar: &Bar) {
        self.seen.lock().unwrap().push(vike_model::Broker::position(&*broker, "BTCUSDT"));
    }
}

/// **A FILL NO MOUNT MINTED REACHES NO MOUNT.** An operator ticket, a reconcile-synthesised `EXT-*`
/// fill, a settlement: the coid is not any mount's, so no `on_fill` hears it and no ledger takes it
/// (it is the residual row's), but the ACCOUNT moves and a strategy sees the new position through
/// `broker.position` on its next hook.
///
/// Pre-fix the fill fell back to the FIRST mount on `(venue, symbol)`: mount A heard a fill its own
/// orders never caused (and, across accounts, a fill of another account's).
#[test]
fn an_unminted_fill_reaches_no_mount() {
    let heard: Heard = Arc::new(TestMutex::new(Vec::new()));
    let seen = Arc::new(TestMutex::new(Vec::new()));
    let reader = StrategyMount {
        strategy: Box::new(PositionReader { seen: Arc::clone(&seen) }),
        ..listener_mount("B", "1m", "maker-b", false, &heard)
    };
    // the Listener is FIRST on the pair, so the old fallback would have picked it
    let mut core = listener_core(vec![listener_mount("A", "1m", "maker-a", false, &heard), reader]);
    core.engine.collect_applied_fills = true;

    core.bus.publish(Event::Fill(coid_fill("manual-ticket", 1, 2.0, 100.0)), &mut core.engine);
    core.dispatch_applied_fills();

    assert!(heard_of(&heard, "fill").is_empty(), "no mount hears a fill it did not cause");
    assert_eq!(core.mount_attr[0].size.to_bits(), 0.0_f64.to_bits(), "attributed to NO ledger");
    assert_eq!(core.mount_attr[1].size.to_bits(), 0.0_f64.to_bits());
    assert_eq!(core.engine.position_size_of("BTCUSDT", "BOTH"), 2.0, "the ACCOUNT still moved");
    assert!(
        core.recent.iter().any(|l| l.contains("reached no strategy")),
        "...and recent-events says so: {:?}",
        core.recent
    );

    close_bar(&mut core, "BTCUSDT", "1m", 60_000);
    assert_eq!(*seen.lock().unwrap(), vec![2.0], "...and the strategy reads it from the broker");
}

/// **AN ORDER EVENT NO MOUNT MINTED REACHES NO MOUNT.** An operator order the RiskGate denies is
/// nobody's: pre-fix it reached the first mount on the pair as an `on_order_event` for an order that
/// mount never submitted (a `ControllerHarness` resubmits on exactly that event).
#[test]
fn an_unminted_order_event_reaches_no_mount() {
    let heard: Heard = Arc::new(TestMutex::new(Vec::new()));
    let mut core = listener_core(vec![
        listener_mount("A", "1m", "maker-a", false, &heard),
        listener_mount("B", "1m", "maker-b", false, &heard),
    ]);
    core.engine.collect_applied_fills = true;
    core.engine.trading_state = vike_exec::TradingState::Halted; // the gate denies every order

    let ticket = OrderRequest {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(100.0),
        ts: 1,
        ..Default::default()
    };
    core.apply_intent(OrderIntent::Submit(Box::new(ticket)), 1);
    assert_eq!(core.engine.order_events.len(), 1, "precondition: the denial was captured");
    core.dispatch_applied_fills();

    assert!(heard_of(&heard, "order_event").is_empty(), "no mount minted it, so none hears it");
    assert!(core.recent.iter().any(|l| l.contains("reached no strategy")), "recent-events says so");
}

/// **A STRAGGLER AFTER UNMOUNT REACHES NO SIBLING.** Mount A rests an order and is unmounted; its
/// fill then arrives. `coid_mount` still names A, whose slot is a tombstone, so `mount_for_coid`
/// misses. Pre-fix the miss fell through to the first LIVE mount on the pair, B, which heard a fill
/// for an order that was never its own.
#[test]
fn an_unmounted_mounts_straggler_fill_reaches_no_sibling() {
    let heard: Heard = Arc::new(TestMutex::new(Vec::new()));
    let mut core = listener_core(vec![
        listener_mount("A", "1m", "maker-a", true, &heard),
        listener_mount("B", "1m", "maker-b", false, &heard),
    ]);
    core.engine.collect_applied_fills = true;
    close_bar(&mut core, "BTCUSDT", "1m", 60_000); // A rests a bid
    let coid_a = coid_of(&core, 0);
    core.unmount_strategy_runtime("maker-a");
    assert!(core.mounts[0].is_none(), "precondition: A is unmounted");

    core.bus.publish(Event::Fill(coid_fill(&coid_a, 1, 1.0, 100.0)), &mut core.engine);
    // precondition: the fill really was applied and captured, so the silence below is a routing verdict
    assert_eq!(core.engine.applied_fills.len(), 1, "the straggler fill was captured");
    core.dispatch_applied_fills();

    assert!(heard_of(&heard, "fill").is_empty(), "the straggler reaches no sibling mount");
    assert_eq!(core.engine.position_size_of("BTCUSDT", "BOTH"), 1.0, "...and the account took it");
}

/// The recent-events note is for what used to reach a strategy: an ACCEPTED event (not terminal) for an
/// operator's order leaves the 64-line ring alone, however many mounts sit on the pair.
#[test]
fn an_unminted_accepted_event_leaves_no_note() {
    let heard: Heard = Arc::new(TestMutex::new(Vec::new()));
    let mut core = listener_core(vec![
        listener_mount("A", "1m", "maker-a", false, &heard),
        listener_mount("B", "1m", "maker-b", false, &heard),
    ]);
    core.engine.collect_applied_fills = true;
    let ticket = OrderRequest {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(100.0),
        ts: 1,
        ..Default::default()
    };
    core.apply_intent(OrderIntent::Submit(Box::new(ticket)), 1);
    let coid = core.engine.client.submissions[0].client_order_id.clone();
    core.bus.publish(
        Event::OrderSubmitted(vike_model::events::OrderSubmitted {
            client_order_id: coid.clone(),
            ts: 1,
        }),
        &mut core.engine,
    );
    core.bus.publish(
        Event::OrderAccepted(vike_model::events::OrderAccepted {
            client_order_id: coid,
            venue_order_id: None,
            ts: 2,
        }),
        &mut core.engine,
    );
    assert!(!core.engine.order_events.is_empty(), "precondition: the acceptance was captured");
    core.dispatch_applied_fills();

    assert!(heard_of(&heard, "order_event").is_empty(), "no mount minted it, so none hears it");
    assert!(
        !core.recent.iter().any(|l| l.contains("reached no strategy")),
        "a non-terminal event is silent: {:?}",
        core.recent
    );
}
