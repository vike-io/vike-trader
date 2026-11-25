//! The HELD-FOR-RELEASE table (`held_release.rs`): an order with a `vike_model::OrderRelease` is accepted and
//! preflighted now, held off the venue and out of every registry, published as a held row, and sent through
//! one journaled `OrderOn` when its release holds at the boundary timer. White-box, on the real
//! `lower_submit` and `drive_due_timers`; the restart half is `tests/journal/held_release_restart.rs`.

use std::sync::{Arc, Mutex};

use super::*;
use vike_execution::testing::{RecordingClient, engine_on};
use vike_model::{
    AccountTier, OrderEvent, OrderEventKind, OrderRelease, OrderReleasePrice,
    OrderReleasePriceSide, OrderStatus, OrderType, TimeInForce,
};

use crate::runtime::test_support::core_of;

type Core = CoreThread<RecordingClient>;

const SYM: &str = "X";
/// The accept instant every test submits at (the dispatch clock of a release is the wall clock, later).
const NOW: i64 = 1_000;
/// A `gtd` expiry far after any wall clock (2100-01-01).
const FAR: i64 = 4_102_444_800_000;

fn core_on(venue: &str, tier: Option<AccountTier>) -> Core {
    let mut engine = engine_on(venue, SYM, RecordingClient::default());
    if let Some(t) = tier {
        engine.set_tier(t);
    }
    core_of(engine, Vec::new(), CoreConfig::default())
}

fn at(t: i64) -> Option<OrderRelease> {
    Some(OrderRelease { at: t, price: None })
}

/// A limit BUY of 1 at 100 on `venue`, released at `release`.
fn limit(venue: &str, release: Option<OrderRelease>) -> OrderRequest {
    OrderRequest {
        venue: venue.into(),
        symbol: SYM.into(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Limit,
        price: Some(100.0),
        release,
        ..Default::default()
    }
}

fn submit(c: &mut Core, req: OrderRequest) -> String {
    c.apply_intent(OrderIntent::Submit(Box::new(req)), NOW).pop().expect("an order client id")
}

fn ring(c: &Core, needle: &str) -> usize {
    c.recent.iter().filter(|l| l.contains(needle)).count()
}

/// **Accept holds, the timer releases.** The order is minted and held: nothing reaches the client or the
/// registry before its release, the HELD list shows it with its release (and never `orders`), and the
/// boundary timer sends it at its release as the plain order it is (no release left on it), REGISTERED then
/// (so a `day` lifetime counts from the release: `created_ms`, the day sweep's anchor) and out of the table.
#[test]
fn an_order_with_a_release_is_held_until_its_timer_and_then_sent_as_a_plain_order() {
    let mut c = core_on("deribit", None);
    let mut req = limit("deribit", at(5_000));
    req.time_in_force = TimeInForce::Day;
    let order_client_id = submit(&mut c, req);

    assert!(c.engine.client.submissions.is_empty(), "nothing at the venue yet");
    assert!(c.engine.registry.is_empty(), "and nothing in a registry");
    assert!(c.held_releases.contains_key(&order_client_id));
    assert_eq!(
        ring(&c, &format!("order {order_client_id} HELD: releases at 5000")),
        1,
        "{:?}",
        c.recent
    );
    let views = c.held_release_views();
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].order_client_id, order_client_id);
    assert_eq!(views[0].release, at(5_000), "the HELD row says when it goes");
    assert_eq!((views[0].account.clone(), views[0].parent_order_id.clone()), (None, None));

    c.drive_due_timers(4_999);
    assert!(c.engine.client.submissions.is_empty(), "not before its release");
    c.drive_due_timers(5_000);
    let sent = &c.engine.client.submissions;
    assert_eq!(sent.len(), 1, "released once: {sent:?}");
    assert_eq!((sent[0].order_client_id.as_str(), sent[0].release), (&*order_client_id, None));
    assert_eq!(sent[0].time_in_force, TimeInForce::Day);
    assert!(c.held_releases.is_empty(), "the hold ends with the release");
    let mo = c.engine.registry.get(&order_client_id).expect("registered at the release");
    assert!(mo.created_ms.is_some_and(|t| t >= 5_000), "the day counts from the release: {mo:?}");
}

/// **Preflight at accept (decision 0219's shape).** A kind the row cannot rest is refused at ACCEPT, by
/// name, and nothing is held for a release that could never send it; a reduce-only stop on a row that
/// refuses it is refused too, never armed early as the core's own stop.
#[test]
fn what_the_row_refuses_is_refused_at_accept_and_never_held() {
    let mut c = core_on("polymarket", None);
    let mut req = limit("polymarket", at(5_000));
    req.order_type = OrderType::StopLimit;
    req.trigger_price = Some(101.0);
    let order_client_id = submit(&mut c, req);
    assert!(c.held_releases.is_empty(), "nothing held");
    assert_eq!(
        c.engine.registry.get(&order_client_id).map(|m| m.status),
        Some(OrderStatus::Rejected)
    );
    assert_eq!(ring(&c, "TRIGGER_UNSUPPORTED: kind=stop_limit venue=polymarket"), 1);

    let mut stop = limit("polymarket", at(5_000));
    stop.order_type = OrderType::Stop;
    stop.price = None;
    stop.trigger_price = Some(95.0);
    stop.reduce_only = true;
    submit(&mut c, stop);
    assert!(c.held_releases.is_empty(), "the reduce-only stop is not held");
    assert_eq!(
        c.emulated.order_emulation_arms.values().map(|b| b.len()).sum::<usize>(),
        0,
        "nor armed"
    );
}

/// **A paper engine's order is held by the core too** (the paper book persists nothing), and released to
/// the paper client at its time. And the display axis judges a paper engine like any other: only the
/// reduce-only axis is relaxed for paper (decision 0201). On bybit, whose row takes no display size
/// (deribit's and binance's do since their demo drills), a visible size is EMULATED by the core (decision
/// 0275) and never reaches the paper client whole, and a hidden one keeps the row's refusal.
#[test]
fn a_paper_engine_holds_in_the_core_and_its_display_size_is_judged_by_the_row() {
    let mut c = core_on("bybit", Some(AccountTier::Paper));
    let order_client_id = submit(&mut c, limit("bybit", at(5_000)));
    assert!(
        c.held_releases.contains_key(&order_client_id) && c.engine.client.submissions.is_empty()
    );
    c.drive_due_timers(5_000);
    assert_eq!(c.engine.client.submissions.len(), 1, "released to the paper client");

    let mut iceberg = limit("bybit", None);
    iceberg.display_quantity = Some(0.1);
    let emulated = submit(&mut c, iceberg);
    assert!(c.emulated.working.contains_key(&emulated), "the core works it");
    let mut hidden = limit("bybit", None);
    hidden.display_quantity = Some(0.0);
    let refused = submit(&mut c, hidden);
    assert_eq!(c.engine.registry.get(&refused).map(|m| m.status), Some(OrderStatus::Rejected));
    assert_eq!(
        ring(&c, "ATTRIBUTE_UNSUPPORTED: venue=bybit attribute=hidden"),
        1,
        "{:?}",
        c.recent
    );
    assert_eq!(c.engine.client.submissions.len(), 1, "neither reached the paper client whole");
}

/// **Cancel and modify with no venue call.** A cancel ENDS the held order (no venue cancel; it is
/// registered and takes a terminal `OrderCanceled`, and the operator's "cancel NOT sent" line stays silent:
/// the core holds it); a modify edits it in place, a stated trigger landing on a trigger kind's trigger; a
/// mass-cancel of its venue ends the rest the same way.
#[test]
fn a_held_order_is_cancelled_modified_and_mass_cancelled_without_the_venue() {
    let mut c = core_on("deribit", None);
    let a = submit(&mut c, limit("deribit", at(5_000)));
    c.dispatch(Ingest::Command(Command::Order(OrderIntent::Cancel(a.clone()))));
    assert!(!c.held_releases.contains_key(&a));
    assert!(c.engine.client.cancels.is_empty(), "no venue call");
    assert_eq!(c.engine.registry.get(&a).map(|m| m.status), Some(OrderStatus::Canceled));
    assert_eq!(ring(&c, "canceled before its release"), 1, "{:?}", c.recent);
    assert_eq!(ring(&c, "cancel NOT sent"), 0, "{:?}", c.recent);

    let mut stop_limit = limit("deribit", at(5_000));
    stop_limit.order_type = OrderType::StopLimit;
    stop_limit.trigger_price = Some(101.0);
    let b = submit(&mut c, stop_limit);
    c.apply_intent(
        OrderIntent::Modify {
            order_client_id: b.clone(),
            new_qty: Some(2.0),
            new_price: Some(102.0),
            new_trigger_price: Some(103.0),
        },
        NOW,
    );
    let held = &c.held_releases[&b].request;
    assert_eq!((held.qty, held.price, held.trigger_price), (2.0, Some(102.0), Some(103.0)));

    let d = submit(&mut c, limit("deribit", at(6_000)));
    c.apply_intent(
        OrderIntent::MassCancel { venue: Some("deribit".into()), symbol: None, account: None },
        NOW,
    );
    assert!(c.held_releases.is_empty(), "{d} and {b} dropped");
    for order_client_id in [&b, &d] {
        assert_eq!(
            c.engine.registry.get(order_client_id).map(|m| m.status),
            Some(OrderStatus::Canceled)
        );
    }
    c.drive_due_timers(10_000);
    assert!(c.engine.client.submissions.is_empty(), "nothing released after the cancel");
}

/// **A held ADJUSTABLE stop's price is never changed, its size is** (`ADJUSTABLE_AMEND_REASON`, the words
/// the engine and the paper book refuse it in): a trigger move leaves its terms and says why; a size-only
/// modify moves the size in place. No roster row takes `adjust` yet, so the entry is placed as a restore
/// places it (the table itself).
#[test]
fn a_held_adjustable_stops_price_is_refused_and_its_size_moves() {
    let mut c = core_on("deribit", None);
    let mut stop = limit("deribit", at(5_000));
    (stop.order_type, stop.price, stop.trigger_price, stop.side) =
        (OrderType::Stop, None, Some(95.0), -1);
    stop.adjust = Some(vike_model::OrderStopAdjust {
        at: 105.0,
        to: vike_model::OrderStopAdjustTo::Stop { stop: 101.0 },
    });
    let order_client_id = "adj".to_string();
    stop.order_client_id = order_client_id.clone();
    let held = vike_journal::OrderHeldRelease { route_key: "deribit".into(), request: stop };
    c.held_releases.insert(order_client_id.clone(), held);
    let modify = |qty, trigger| OrderIntent::Modify {
        order_client_id: order_client_id.clone(),
        new_qty: qty,
        new_price: None,
        new_trigger_price: trigger,
    };
    c.apply_intent(modify(None, Some(96.0)), NOW);
    assert_eq!(
        c.held_releases[&order_client_id].request.trigger_price,
        Some(95.0),
        "the trigger stays"
    );
    let why = vike_execution::execution_engine::ADJUSTABLE_AMEND_REASON;
    assert_eq!(
        ring(&c, &format!("held order {order_client_id} NOT modified: {why}")),
        1,
        "{:?}",
        c.recent
    );
    c.apply_intent(modify(Some(3.0), None), NOW);
    assert_eq!(c.held_releases[&order_client_id].request.qty, 3.0, "the size moves");
}

/// **A submit that REUSES a held order's order client id is refused as a duplicate, in words**, and the held order
/// stays held: only the journaled `OrderOn` of its release takes it out of the table.
#[test]
fn a_submit_reusing_a_held_order_client_id_is_refused_and_never_releases_it() {
    let mut c = core_on("deribit", None);
    let order_client_id = submit(&mut c, limit("deribit", at(5_000)));
    let mut again = limit("deribit", None);
    again.order_client_id = order_client_id.clone();
    assert!(c.apply_intent(OrderIntent::Submit(Box::new(again)), NOW).is_empty(), "refused");
    assert!(c.held_releases.contains_key(&order_client_id), "still held");
    assert!(c.engine.client.submissions.is_empty() && c.engine.registry.is_empty());
    assert_eq!(
        ring(
            &c,
            &format!(
                "order client id {order_client_id} is already used by an order held for its release"
            )
        ),
        1,
        "{:?}",
        c.recent
    );
    c.drive_due_timers(5_000);
    assert_eq!(c.engine.client.submissions.len(), 1, "released at its own time, once");
}

/// **A conditional release waits for its price** (time AND price), read from its engine's price board at
/// the timer, never per message: not before the price meets the level, then at the next poll.
#[test]
fn a_conditional_release_waits_for_its_price_at_the_poll() {
    let mut c = core_on("deribit", None);
    let cond = OrderReleasePrice {
        symbol: SYM.into(),
        level: 90.0,
        side: OrderReleasePriceSide::Below,
        venue: None,
    };
    let order_client_id =
        submit(&mut c, limit("deribit", Some(OrderRelease { at: 5_000, price: Some(cond) })));
    assert_eq!(
        c.held_release_views()[0].release.and_then(|r| r.price),
        Some(cond),
        "the HELD row names the price it waits for"
    );
    c.engine.price_board.set_mark("deribit", SYM, 95.0, 0);
    c.drive_due_timers(5_000);
    assert!(c.engine.client.submissions.is_empty(), "the time came, the price did not");
    assert!(c.held_releases.contains_key(&order_client_id));
    assert_eq!(c.release_poll_due, Some(5_000 + super::held_release::RELEASE_PRICE_POLL_MS));

    c.engine.price_board.set_mark("deribit", SYM, 89.5, 0);
    c.drive_due_timers(5_500);
    assert!(c.engine.client.submissions.is_empty(), "not between two polls");
    c.drive_due_timers(6_000);
    assert_eq!(c.engine.client.submissions.len(), 1, "released at the poll after the price met it");
    assert!(c.held_releases.is_empty());
}

/// The risk clock's age bound as the live root sets it (`MAX_PRICE_AGE`): what a release's price is judged by.
fn bound() -> PriceOnlyConfig {
    PriceOnlyConfig {
        venues: std::collections::BTreeSet::new(),
        sweep_interval: std::time::Duration::from_secs(30),
        max_price_age: MAX_PRICE_AGE,
        price_source: std::collections::BTreeMap::new(),
    }
}

/// **A core that configures no `price_only` (the paper rehearsal cores: `vike-tradehub-core`'s `run/paper.rs`,
/// `run/multi.rs` and the xemm paper core) judges a release by the SAME bound**, `MAX_PRICE_AGE`, the one constant
/// the live root sets `PriceOnlyConfig::max_price_age` from: a paper engine's mark 61 s old holds the order.
#[test]
fn a_core_without_price_only_judges_a_release_by_the_same_bound() {
    let mut c = core_on("deribit", Some(AccountTier::Paper));
    assert!(c.config.price_only.is_none(), "the paper rehearsal cores' shape");
    assert_eq!(MAX_PRICE_AGE, std::time::Duration::from_secs(60));
    submit(&mut c, conditional("deribit", OrderReleasePriceSide::Below, 90.0));
    c.engine.price_board.set_mark("deribit", SYM, 89.0, 0);
    c.drive_due_timers(61_000);
    assert!(c.engine.client.submissions.is_empty(), "a mark 61 s old releases nothing here either");
    assert_eq!(
        c.held_release_views()[0].waiting.as_deref(),
        Some("deribit's last X price is 61 s old, and a price older than 60 s never releases")
    );
    c.engine.price_board.set_mark("deribit", SYM, 89.0, 61_500);
    c.drive_due_timers(62_000);
    assert_eq!(c.engine.client.submissions.len(), 1, "a fresh one releases it to the paper client");
}

fn bounded(venue: &str) -> Core {
    let config = CoreConfig { price_only: Some(bound()), ..CoreConfig::default() };
    core_of(engine_on(venue, SYM, RecordingClient::default()), Vec::new(), config)
}

/// A limit BUY on `venue` released at 5 000 once `SYM` is on `side` of `level`.
fn conditional(venue: &str, side: OrderReleasePriceSide, level: f64) -> OrderRequest {
    let price = Some(OrderReleasePrice { symbol: SYM.into(), level, side, venue: None });
    limit(venue, Some(OrderRelease { at: 5_000, price }))
}

/// **A STALE price never releases** (owner rule: no trading without a price): a mark that meets the level
/// but is past the 60 s bound holds the order, and its HELD row and one ring line say why; the account slot
/// the core used to read meets it too and is not read. A fresh mark that does not meet it says nothing more,
/// and a fresh one that does releases it.
#[test]
fn a_stale_mark_never_releases_and_a_fresh_one_does() {
    let mut c = bounded("deribit");
    let order_client_id =
        submit(&mut c, conditional("deribit", OrderReleasePriceSide::Below, 90.0));
    c.engine.price_board.set_mark("deribit", SYM, 89.0, 0);
    c.engine.account.set_mark_from("deribit", SYM, 89.0, MarkSource::VenueMark, 0);
    c.drive_due_timers(61_000);
    assert!(c.engine.client.submissions.is_empty(), "a mark 61 s old releases nothing");
    let why = "deribit's last X price is 61 s old, and a price older than 60 s never releases";
    assert_eq!(c.held_release_views()[0].waiting.as_deref(), Some(why));
    assert_eq!(
        ring(&c, &format!("order {order_client_id} still HELD: {why}")),
        1,
        "{:?}",
        c.recent
    );

    c.engine.price_board.set_mark("deribit", SYM, 91.0, 61_500);
    c.drive_due_timers(62_000);
    assert!(c.engine.client.submissions.is_empty(), "fresh, and above the level");
    assert_eq!(c.held_release_views()[0].waiting, None, "a fresh price leaves nothing to say");

    c.engine.price_board.set_mark("deribit", SYM, 89.0, 62_500);
    c.drive_due_timers(63_000);
    assert_eq!(c.engine.client.submissions.len(), 1, "a fresh mark at the level releases it");
    assert!(c.held_releases.is_empty() && c.release_waiting.is_empty());
}

/// **A quote-only venue releases on its bid or ask** (no venue mark on an FX venue): the side that has to
/// reach the level, the bid for above and the ask for below, so the spread is never read in the order's favour.
#[test]
fn a_quote_only_venue_releases_on_the_side_that_has_to_reach_the_level() {
    let mut c = bounded("oanda");
    let above = submit(&mut c, conditional("oanda", OrderReleasePriceSide::Above, 100.0));
    let below = submit(&mut c, conditional("oanda", OrderReleasePriceSide::Below, 100.0));
    c.engine.price_board.set_quote("oanda", SYM, 100.2, 100.4, 5_000);
    c.drive_due_timers(5_000);
    let sent = |c: &Core| -> Vec<String> {
        c.engine.client.submissions.iter().map(|r| r.order_client_id.clone()).collect()
    };
    assert_eq!(sent(&c), [above.as_str()], "the bid is at 100.2, the ask is not down at 100");
    c.engine.price_board.set_quote("oanda", SYM, 99.4, 99.6, 5_500);
    c.drive_due_timers(6_000);
    assert_eq!(sent(&c), [above.as_str(), below.as_str()], "the ask came down to the level");
}

/// **A MISSING price holds the order with its reason**, said once however many polls pass, and a cancel
/// ends the order and its words.
#[test]
fn a_missing_price_holds_the_order_and_its_row_says_why() {
    let mut c = bounded("deribit");
    let order_client_id =
        submit(&mut c, conditional("deribit", OrderReleasePriceSide::Above, 100.0));
    c.drive_due_timers(5_000);
    c.drive_due_timers(6_000);
    assert!(
        c.engine.client.submissions.is_empty() && c.held_releases.contains_key(&order_client_id)
    );
    let why = "no price of X on deribit on this node yet";
    assert_eq!(c.held_release_views()[0].waiting.as_deref(), Some(why));
    assert_eq!(
        ring(&c, &format!("order {order_client_id} still HELD: {why}")),
        1,
        "{:?}",
        c.recent
    );
    c.apply_intent(OrderIntent::Cancel(order_client_id), NOW);
    assert!(c.held_releases.is_empty() && c.release_waiting.is_empty());
}

/// **Restart: a held conditional order waits for a price the restarted node reads, never one from
/// before.** Assembled from the journal's held orders (`CoreConfig::held_releases`, the restart shape)
/// with the last price before the restart, which meets the level, still on the board and the account
/// slot: it holds and says why at the first boundary; a fresh quote releases it.
#[test]
fn after_a_restart_a_conditional_order_waits_for_a_fresh_price() {
    let req = conditional("deribit", OrderReleasePriceSide::Below, 90.0);
    let req = OrderRequest { order_client_id: "hr".into(), ..req };
    let config = CoreConfig {
        held_releases: vec![vike_journal::OrderHeldRelease {
            route_key: "deribit".into(),
            request: req,
        }],
        price_only: Some(bound()),
        ..CoreConfig::default()
    };
    let mut engine = engine_on("deribit", SYM, RecordingClient::default());
    engine.price_board.set_mark("deribit", SYM, 89.0, 1_000);
    engine.account.set_mark_from("deribit", SYM, 89.0, MarkSource::VenueMark, 1_000);
    let mut c = core_of(engine, Vec::new(), config);
    c.drive_due_timers(121_000); // the first boundary, two minutes after that price
    assert!(c.engine.client.submissions.is_empty(), "a price from before the restart");
    let why = "deribit's last X price is 120 s old, and a price older than 60 s never releases";
    assert_eq!(c.held_release_views()[0].waiting.as_deref(), Some(why));

    c.engine.price_board.set_quote("deribit", SYM, 88.5, 89.0, 121_500);
    c.drive_due_timers(122_000);
    let sent = &c.engine.client.submissions;
    assert_eq!(sent.len(), 1, "the fresh ask at the level releases it: {sent:?}");
    assert_eq!(sent[0].order_client_id, "hr");
}

/// A core whose order engine is ibkr (`SYM`) and whose second engine is binance on `BTCUSDT`: the
/// price venue of decision 0267's 2.13, judged under the 60 s bound.
fn ibkr_and_binance() -> Core {
    let config = CoreConfig { price_only: Some(bound()), ..CoreConfig::default() };
    let binance = engine_on("binance", "BTCUSDT", RecordingClient::default());
    core_of(engine_on("ibkr", SYM, RecordingClient::default()), vec![(1_000.0, binance)], config)
}

/// A limit BUY on ibkr released at 5 000 once `BTCUSDT` on `venue` is at or above `level`.
fn on_venue(venue: &str, level: f64) -> OrderRequest {
    let price = Some(OrderReleasePrice {
        symbol: "BTCUSDT".into(),
        level,
        side: OrderReleasePriceSide::Above,
        venue: Some(venue.into()),
    });
    limit("ibkr", Some(OrderRelease { at: 5_000, price }))
}

/// **A price condition that names another venue reads THAT venue's board** (decision 0267's 2.13): held on
/// ibkr, its condition on binance's `BTCUSDT`. The order's own engine's price of the symbol is never read
/// (missing on binance: it waits and says so); a fresh binance price below the level waits saying nothing;
/// one at the level releases it on ibkr, as the plain order, and its words go with it.
#[test]
fn a_condition_on_another_venue_waits_for_that_venues_fresh_price_and_releases_on_its_own() {
    let mut c = ibkr_and_binance();
    let order_client_id = submit(&mut c, on_venue("binance", 100_000.0));
    assert!(c.held_releases.contains_key(&order_client_id), "held by the core, not refused");
    assert_eq!(ring(&c, "once BTCUSDT on binance is at or above 100000"), 1, "{:?}", c.recent);
    c.engine.price_board.set_mark("ibkr", "BTCUSDT", 200_000.0, 5_000);
    c.drive_due_timers(5_000);
    assert!(c.engine.client.submissions.is_empty(), "the order's own venue is not the price venue");
    let why = "no price of BTCUSDT on binance on this node yet";
    assert_eq!(c.held_release_views()[0].waiting.as_deref(), Some(why));

    c.extra_engines[0].1.price_board.set_mark("binance", "BTCUSDT", 99_000.0, 5_500);
    c.drive_due_timers(6_000);
    assert!(c.engine.client.submissions.is_empty(), "fresh, below the level");
    assert_eq!(c.held_release_views()[0].waiting, None, "a fresh price leaves nothing to say");

    c.extra_engines[0].1.price_board.set_mark("binance", "BTCUSDT", 100_500.0, 6_500);
    c.drive_due_timers(7_000);
    let sent = &c.engine.client.submissions;
    assert_eq!(sent.len(), 1, "released on ibkr: {sent:?}");
    assert_eq!((sent[0].venue.as_str(), sent[0].release), ("ibkr", None));
    assert!(c.extra_engines[0].1.client.submissions.is_empty(), "the price venue is never traded");
    assert!(c.held_releases.is_empty() && c.release_waiting.is_empty());
}

/// **A stale price on the price venue never releases, and no account of that venue says so**: binance's
/// mark meets the level but is 61 s old; a condition naming okx, which runs no engine here, waits with
/// its own words. Neither is ever sent.
#[test]
fn a_stale_or_absent_price_venue_holds_the_order_and_says_why() {
    let mut c = ibkr_and_binance();
    let stale = submit(&mut c, on_venue("binance", 100_000.0));
    let absent = submit(&mut c, on_venue("okx", 100_000.0));
    c.extra_engines[0].1.price_board.set_mark("binance", "BTCUSDT", 101_000.0, 0);
    c.drive_due_timers(61_000);
    assert!(c.engine.client.submissions.is_empty(), "nothing is released");
    let words = |order_client_id: &str| {
        c.held_release_views()
            .into_iter()
            .find(|v| v.order_client_id == order_client_id)
            .and_then(|v| v.waiting)
    };
    let old =
        "binance's last BTCUSDT price is 61 s old, and a price older than 60 s never releases";
    assert_eq!(words(&stale).as_deref(), Some(old));
    let none = "no okx account runs on this node now, so its BTCUSDT price is not read";
    assert_eq!(words(&absent).as_deref(), Some(none));
    assert_eq!(ring(&c, &format!("order {absent} still HELD: {none}")), 1, "{:?}", c.recent);
}

/// **Restart: a cross-venue condition waits for a FRESH price of its venue, never one from before.** The
/// journal's held order comes back (`CoreConfig::held_releases`) beside binance's last price, which meets
/// the level but is two minutes old: it waits and says why; a fresh binance quote releases it on ibkr.
#[test]
fn after_a_restart_a_cross_venue_condition_waits_for_a_fresh_price_of_its_venue() {
    let req = OrderRequest { order_client_id: "xv".into(), ..on_venue("binance", 100_000.0) };
    let config = CoreConfig {
        held_releases: vec![vike_journal::OrderHeldRelease {
            route_key: "ibkr".into(),
            request: req,
        }],
        price_only: Some(bound()),
        ..CoreConfig::default()
    };
    let mut binance = engine_on("binance", "BTCUSDT", RecordingClient::default());
    binance.price_board.set_mark("binance", "BTCUSDT", 101_000.0, 1_000);
    let ibkr = engine_on("ibkr", SYM, RecordingClient::default());
    let mut c = core_of(ibkr, vec![(1_000.0, binance)], config);
    c.drive_due_timers(121_000);
    assert!(c.engine.client.submissions.is_empty(), "a price from before the restart");
    let why =
        "binance's last BTCUSDT price is 120 s old, and a price older than 60 s never releases";
    assert_eq!(c.held_release_views()[0].waiting.as_deref(), Some(why));

    c.extra_engines[0].1.price_board.set_quote("binance", "BTCUSDT", 100_100.0, 100_200.0, 121_500);
    c.drive_due_timers(122_000);
    let sent = &c.engine.client.submissions;
    assert_eq!(sent.len(), 1, "the fresh bid at the level releases it: {sent:?}");
    assert_eq!(sent[0].order_client_id, "xv");
}

/// **A venue that holds a TIME release itself takes it at accept; a price condition never goes there**
/// (decision 0267's 2.6, the owner's answer A, and the coordinator's ruling that it is decided per
/// ENGINE). On a row listing the attribute word `release` (IB's `goodAfterTime`, opened behind its
/// smoke), a time-only release goes to the venue only from an engine whose backend says it holds one (a
/// socket-shaped engine, `native_time_release`); a Client Portal-shaped one, which shares ibkr's row,
/// keeps the core's hold. A condition on the order's own venue or on another one is always the core's.
/// ibkr's row lists the word (its paper smoke, green 2026-11-09), so its time release goes to the venue
/// from a native engine and stays the core's from the other, and the fact rides the engine's snapshot so a
/// replay core decides alike.
#[test]
fn only_a_time_release_from_a_native_engine_on_a_row_with_the_word_goes_to_the_venue() {
    use super::held_release::venue_holds_release;
    use vike_venues::venue_caps::IBKR;
    let row = vike_venues::VenueCapabilities { order_attributes: &["release"], ..IBKR };
    let own = OrderReleasePrice {
        symbol: SYM.into(),
        level: 90.0,
        side: OrderReleasePriceSide::Below,
        venue: None,
    };
    let other = OrderReleasePrice { venue: Some("binance".into()), ..own };
    let time = limit("ibkr", at(5_000));
    assert!(venue_holds_release(&row, true, &time), "a socket-shaped engine: the venue's");
    assert!(!venue_holds_release(&row, false, &time), "a Client Portal-shaped engine: the core's");
    for price in [own, other] {
        let req = limit("ibkr", Some(OrderRelease { at: 5_000, price: Some(price) }));
        assert!(!venue_holds_release(&row, true, &req), "{price:?}: always the core's");
    }
    assert!(!venue_holds_release(&row, true, &limit("ibkr", None)), "no release: nothing to hold");
    assert!(venue_holds_release(&IBKR, true, &time), "ibkr's row lists `release`");

    let mut hashes = Vec::new();
    for native in [false, true] {
        let mut c = core_on("ibkr", None);
        c.engine.set_native_time_release(native);
        // the fact alone, before an order makes the two cores' states differ
        let snap = c.engine.snapshot_state();
        let order_client_id = submit(&mut c, limit("ibkr", at(5_000)));
        let held = c.held_releases.contains_key(&order_client_id);
        assert_eq!(held, !native, "{native}: held off a native engine");
        let sent = c.engine.client.submissions.len();
        assert_eq!(sent, usize::from(native), "{native}: sent from a native one");
        assert_eq!(snap.native_time_release, native, "the fact rides the snapshot");
        let back = vike_execution::Engine::from_snapshot(&snap, RecordingClient::default());
        assert_eq!(back.native_time_release(), native, "and a replay core reads it back");
        // a boot restore onto the engine the mount built keeps the mount's word, whatever was journaled
        let mut mounted = engine_on("ibkr", SYM, RecordingClient::default());
        mounted.set_native_time_release(!native);
        mounted.restore_runtime_state(&snap);
        assert_eq!(mounted.native_time_release(), !native, "the mount's word, not the journal's");
        hashes.push(vike_execution::state_hash(&[snap]));
    }
    assert_eq!(hashes[0], hashes[1], "configuration, not hashed (as the tier is)");
}

/// **Releases that fell due together go out in release-time order, and an expiry that passed wins**: the
/// restart shape (`CoreConfig::held_releases`, the journal's held orders, whose times passed while the node
/// was down). Held `late` before `early`, released `early` first; the `gtd` order whose expiry also passed
/// is refused as EXPIRED, never sent.
#[test]
fn due_releases_go_out_in_release_time_order_and_a_passed_expiry_wins() {
    let held =
        |order_client_id: &str, t: i64, expiry: Option<i64>| vike_journal::OrderHeldRelease {
            route_key: "deribit".into(),
            request: OrderRequest {
                order_client_id: order_client_id.into(),
                time_in_force: if expiry.is_some() { TimeInForce::Gtd } else { TimeInForce::Gtc },
                gtd_expiry: expiry,
                ..limit("deribit", at(t))
            },
        };
    let config = CoreConfig {
        held_releases: vec![
            held("late", 3_000, None),
            held("early", 2_000, None),
            held("gone", 1_000, Some(1_500)),
        ],
        ..CoreConfig::default()
    };
    let mut c = core_of(engine_on("deribit", SYM, RecordingClient::default()), Vec::new(), config);
    assert_eq!(c.held_releases.len(), 3, "held again at assembly");
    assert_eq!(ring(&c, "RESTORE HELD: 3 order(s)"), 1, "{:?}", c.recent);

    c.drive_due_timers(10_000); // the first boundary after the boot: all three are due
    let sent: Vec<&str> =
        c.engine.client.submissions.iter().map(|r| r.order_client_id.as_str()).collect();
    assert_eq!(sent, vec!["early", "late"], "release-time order, the expired one not sent");
    let gone = c.engine.registry.get("gone").expect("the expired order is registered terminal");
    assert_eq!(gone.status, OrderStatus::Rejected);
    assert_eq!(
        ring(&c, "order gone REFUSED: EXPIRED_BEFORE_RELEASE: gtd_expiry=1500"),
        1,
        "{:?}",
        c.recent
    );
    assert!(c.held_releases.is_empty());
}

/// **The replay core runs no release timer** (`CoreConfig::release_timer` false): it holds the order, arms
/// nothing, and so reads no extra clock stamp; its releases are the journaled `OrderOn` records.
#[test]
fn the_replay_core_holds_without_a_timer() {
    let config = CoreConfig { release_timer: false, ..CoreConfig::default() };
    let mut c = core_of(engine_on("deribit", SYM, RecordingClient::default()), Vec::new(), config);
    let order_client_id = submit(&mut c, limit("deribit", at(5_000)));
    assert!(c.held_releases.contains_key(&order_client_id));
    assert!(c.timers.is_empty(), "no timer on the replay core");
    // The journaled release, as the replay core re-applies it: the request with its release taken off.
    let mut released = c.held_releases[&order_client_id].request.clone();
    released.release = None;
    c.apply_routed_order(vike_execution::RoutedOrder {
        route_key: "deribit".into(),
        intent: OrderIntent::Submit(Box::new(released)),
    });
    assert_eq!(c.engine.client.submissions.len(), 1);
    assert!(c.held_releases.is_empty());
}

/// A trigger price stated on a modify of a resting order of a NON-trigger kind is refused in words (it has
/// no trigger), and the order keeps its terms: the routed `Modify` arm reads `new_trigger_price` now.
#[test]
fn a_trigger_move_on_a_resting_limit_is_refused_in_words() {
    let mut c = core_on("deribit", None);
    let order_client_id = submit(&mut c, limit("deribit", None));
    // resting at the venue: only a modifiable order takes a modify at all
    for event in [
        Event::OrderSubmitted(vike_model::events::OrderSubmitted {
            order_client_id: order_client_id.clone(),
            timestamp: 1,
        }),
        Event::OrderAccepted(vike_model::events::OrderAccepted {
            order_client_id: order_client_id.clone(),
            venue_order_id: Some("v-1".into()),
            timestamp: 1,
        }),
    ] {
        c.dispatch(Ingest::Event(event));
    }
    c.apply_intent(
        OrderIntent::Modify {
            order_client_id: order_client_id.clone(),
            new_qty: None,
            new_price: None,
            new_trigger_price: Some(99.0),
        },
        NOW,
    );
    assert_eq!(c.engine.registry[&order_client_id].status, OrderStatus::Accepted, "resting");
    assert!(
        c.engine
            .rejection_reason(&order_client_id)
            .is_some_and(|r| r.contains("has no trigger to move")),
        "the engine's refusal words (`TRIGGER_AMEND_NO_TRIGGER_REASON`): {:?} {:?}",
        c.engine.rejection_reason(&order_client_id),
        c.recent
    );
    assert!(c.engine.client.modifies.is_empty(), "nothing sent");
}

/// Calls `submit_limit_at` once on its first feed-status call and records what it hears.
struct HoldsOne {
    release: OrderRelease,
    heard: Arc<Mutex<Vec<OrderEvent>>>,
}

impl Strategy<LiveBroker> for HoldsOne {
    fn on_feed_status(&mut self, broker: &mut LiveBroker, _status: FeedStatus) {
        broker.submit_limit_at("", 1, 1.0, 100.0, TimeInForce::Gtc, None, self.release);
    }
    fn on_order_event(&mut self, _broker: &mut LiveBroker, event: &OrderEvent) {
        self.heard.lock().unwrap().push(event.clone());
    }
}

/// **The strategy verb.** `LiveBroker::submit_limit_at` buffers a release the drain carries into the
/// request (held, owned by the mount); a release not after the drain's `now` is a `Denied` the mount hears.
#[test]
fn a_strategy_holds_through_its_verb_and_a_past_release_is_denied() {
    for (release_at, held) in [(FAR, true), (0, false)] {
        let heard = Arc::new(Mutex::new(Vec::new()));
        let mount = Mount {
            order_client_id_code: None,
            account: None,
            symbols: Vec::new(),
            controller_id: Some("hr".to_string()),
            underlying_symbol: None,
            venue: "deribit".into(),
            symbol: SYM.into(),
            interval: "1m".into(),
            strategy: Box::new(HoldsOne {
                release: OrderRelease { at: release_at, price: None },
                heard: Arc::clone(&heard),
            }),
        };
        let mut c = core_of(
            engine_on("deribit", SYM, RecordingClient::default()),
            Vec::new(),
            CoreConfig { strategy: Some(mount), ..CoreConfig::default() },
        );
        c.engine.collect_applied_fills = true;
        c.drive_strategy_feed_status("deribit", SYM, FeedStatus::Disconnected);
        c.dispatch_applied_fills();
        assert!(c.engine.client.submissions.is_empty(), "{release_at}");
        assert_eq!(c.held_releases.len(), usize::from(held), "{release_at}");
        if held {
            let order_client_id = c.held_releases.keys().next().unwrap().clone();
            assert_eq!(
                c.order_client_id_mount.get(&order_client_id),
                Some(&0),
                "the mount's order"
            );
            // A cancel ends it where the mount hears it: a terminal `Canceled`, never a silent drop.
            c.apply_intent(OrderIntent::Cancel(order_client_id.clone()), NOW);
            c.dispatch_applied_fills();
            let heard = heard.lock().unwrap();
            assert!(
                heard.iter().any(|e| e.order_client_id == order_client_id
                    && matches!(e.kind, OrderEventKind::Canceled { .. })),
                "{heard:?}"
            );
        } else {
            let heard = heard.lock().unwrap();
            assert!(
                matches!(heard.as_slice(), [e] if matches!(&e.kind, OrderEventKind::Denied { reason } if reason.contains("not in the future"))),
                "{heard:?}"
            );
        }
    }
}
