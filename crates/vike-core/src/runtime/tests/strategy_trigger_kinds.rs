//! A MOUNTED STRATEGY'S STOP-LIMIT AND TAKE-PROFIT (`LiveBroker::submit_stop_limit`,
//! `LiveBroker::submit_take_profit`): ORDERS through the capability row in decision 0237's shape, and the
//! drain's order-terms rule (`vike_model::order_terms_refusal`) in front of them. White-box, driven the way the
//! runtime drives a strategy (`drive_strategy_feed_status` -> the hook -> `drain_broker` ->
//! `apply_strategy_intent` -> `lower_submit`), so the verb, the drain, the preflight and the lifecycle the
//! strategy hears are all in the path.
//!
//! What each test pins:
//!
//! * the verbs buffer an untagged, plain (not reduce-only) order on the mount's own symbol, in the ORDER lane;
//! * a row that lists the kind (deribit) gets it NATIVELY, with its trigger and its limit, owned by the mount;
//! * a row that does not (polymarket) refuses it by name, `TRIGGER_UNSUPPORTED`, as a terminal `Rejected` the
//!   strategy hears, never a plain stop or a market;
//! * a stated `paper` engine sends it to the simulator where the row lists the kind, and is refused where it
//!   does not (decision 0201 relaxes the reduce-only axis alone);
//! * terms or a lifetime the drain's two rules refuse (`order_terms_refusal`, `lifetime_refusal`) are a
//!   `Denied` the strategy hears, before anything is minted, sent or registered; a lifetime they accept passes.

use std::sync::{Arc, Mutex};

use super::*;
use vike_execution::testing::{RecordingClient, engine_on};
use vike_model::{AccountTier, OrderEvent, OrderEventKind, OrderStatus, OrderType, TimeInForce};

use crate::runtime::test_support::core_of;

type Core = CoreThread<RecordingClient>;
type Heard = Arc<Mutex<Vec<OrderEvent>>>;

const SYM: &str = "X";
const TRIGGER: f64 = 95.0;
const LIMIT: f64 = 94.5;
/// A `gtd` expiry far after any dispatch's `now` (2100-01-01).
const FAR: i64 = 4_102_444_800_000;

/// Which verb the strategy calls, with its prices.
#[derive(Clone, Copy, Debug)]
enum Verb {
    StopLimit {
        trigger: f64,
        limit: f64,
    },
    TakeProfit {
        trigger: f64,
        limit: Option<f64>,
    },
    /// `LiveBroker::submit_limit_time_in_force`, the one verb that names a lifetime and an expiry.
    LimitTimeInForce {
        price: f64,
        tif: TimeInForce,
        expiry: Option<i64>,
    },
}

impl Verb {
    /// What the buffered order is: its kind, its limit (`price`) and its trigger.
    fn terms(self) -> (OrderType, Option<f64>, Option<f64>) {
        match self {
            Verb::StopLimit { trigger, limit } => {
                (OrderType::StopLimit, Some(limit), Some(trigger))
            }
            Verb::TakeProfit { trigger, limit } => (OrderType::TakeProfit, limit, Some(trigger)),
            Verb::LimitTimeInForce { price, .. } => (OrderType::Limit, Some(price), None),
        }
    }
}

/// The verbs under test with good terms: a stop-limit, a limit-if-touched and a market-if-touched.
const GOOD: [Verb; 3] = [
    Verb::StopLimit { trigger: TRIGGER, limit: LIMIT },
    Verb::TakeProfit { trigger: TRIGGER, limit: Some(LIMIT) },
    Verb::TakeProfit { trigger: TRIGGER, limit: None },
];

/// Calls its ONE verb (a SELL of 2) on its first feed-status call, and records every lifecycle event it hears.
struct Places {
    verb: Verb,
    heard: Heard,
}

impl Strategy<LiveBroker> for Places {
    fn on_feed_status(&mut self, broker: &mut LiveBroker, _status: FeedStatus) {
        match self.verb {
            Verb::StopLimit { trigger, limit } => broker.submit_stop_limit(-1, 2.0, trigger, limit),
            Verb::TakeProfit { trigger, limit } => {
                broker.submit_take_profit(-1, 2.0, trigger, limit)
            }
            Verb::LimitTimeInForce { price, tif, expiry } => {
                broker.submit_limit_time_in_force("", -1, 2.0, price, tif, expiry)
            }
        }
    }

    fn on_order_event(&mut self, _broker: &mut LiveBroker, event: &OrderEvent) {
        self.heard.lock().unwrap().push(event.clone());
    }
}

/// A core whose one engine is `venue`'s (a ROSTER venue, so its capability row is enforced), told `tier`
/// when one is given, with one mount on `X` running [`Places`]; then the strategy runs once and every
/// lifecycle event is delivered.
fn run(venue: &str, tier: Option<AccountTier>, verb: Verb) -> (Core, Vec<OrderEvent>) {
    let mut engine = engine_on(venue, SYM, RecordingClient::default());
    if let Some(tier) = tier {
        engine.set_tier(tier);
    }
    let heard: Heard = Arc::new(Mutex::new(Vec::new()));
    let mount = Mount {
        order_client_id_code: None,
        account: None,
        symbols: Vec::new(),
        controller_id: Some("tk".to_string()),
        underlying_symbol: None,
        venue: venue.into(),
        symbol: SYM.into(),
        interval: "1m".into(),
        strategy: Box::new(Places { verb, heard: Arc::clone(&heard) }),
    };
    let mut c =
        core_of(engine, Vec::new(), CoreConfig { strategy: Some(mount), ..CoreConfig::default() });
    // What `spawn_core` turns on for a mounted primary, which `core_of` does not: the lifecycle buffer.
    c.engine.collect_applied_fills = true;
    c.drive_strategy_feed_status(venue, SYM, FeedStatus::Disconnected);
    c.dispatch_applied_fills();
    let heard = heard.lock().unwrap().clone();
    (c, heard)
}

fn ring(c: &Core, needle: &str) -> usize {
    c.recent.iter().filter(|l| l.contains(needle)).count()
}

fn armed(c: &Core) -> usize {
    c.emulated.order_emulation_arms.values().map(|b| b.len()).sum()
}

/// The two verbs buffer an UNTAGGED order on the mount's own symbol (the producer half of
/// `a_tagged_order_carries_only_the_symbol_its_verb_named`'s sweep), plain, `Gtc`, in the order lane and not the core's own
/// conditional book: a stop-limit carries its limit as `price`, a take-profit its limit or none.
#[test]
fn the_verbs_buffer_an_untagged_plain_order_on_the_mounts_own_symbol() {
    let mut b = LiveBroker {
        data_delay: None,
        inputs: None,
        closed_trades: None,
        position: 0.0,
        average_price: 0.0,
        price: 100.0,
        equity: 10_000.0,
        bars: Arc::new(Vec::new()),
        index: 0,
        now: 0,
        contract_value: vike_model::ContractValue::default(),
        quote_rate: 1.0,
        lot_size: 0.0,
        tick_size: 0.0,
        min_qty: 0.0,
        min_notional: 0.0,
        submissions: Vec::new(),
        modifications: Vec::new(),
        cancels: Vec::new(),
        positions: Vec::new(),
        prices: Vec::new(),
        bar_views: Vec::new(),
        leg_values: None,
        brackets: Vec::new(),
        order_lists: Vec::new(),
        combos: Vec::new(),
        order_emulation_arms: Vec::new(),
        mass_cancel: false,
    };
    b.submit_stop_limit(-1, 2.0, TRIGGER, LIMIT);
    b.submit_take_profit(1, 3.0, TRIGGER, Some(LIMIT));
    b.submit_take_profit(1, 4.0, TRIGGER, None);

    assert!(b.order_emulation_arms.is_empty(), "not the core's own book");
    let got: Vec<_> = b
        .submissions
        .iter()
        .map(|s| (s.order_type.clone(), s.side, s.qty, s.price, s.trigger))
        .collect();
    assert_eq!(
        got,
        vec![
            (OrderType::StopLimit, -1, 2.0, Some(LIMIT), Some(TRIGGER)),
            (OrderType::TakeProfit, 1, 3.0, Some(LIMIT), Some(TRIGGER)),
            (OrderType::TakeProfit, 1, 4.0, None, Some(TRIGGER)),
        ]
    );
    for s in &b.submissions {
        assert!(s.symbol.is_none() && s.tag.is_none() && !s.reduce_only, "{}", s.order_type);
        assert_eq!((s.time_in_force, s.gtd_expiry), (vike_model::TimeInForce::Gtc, None));
    }
}

/// **A row that lists the kind rests it NATIVELY** (deribit lists `stop_limit` and `take_profit`): the order
/// reaches the client as that kind with its trigger and its limit, not reduce-only, owned by the mount, and the
/// core arms nothing.
#[test]
fn on_a_row_that_lists_the_kind_the_order_is_native_and_the_mounts() {
    for verb in GOOD {
        let (c, heard) = run("deribit", None, verb);
        let (kind, limit, trigger) = verb.terms();

        assert_eq!(armed(&c), 0, "{verb:?}: the venue holds it, not the core");
        let sent = &c.engine.client.submissions;
        assert_eq!(sent.len(), 1, "{verb:?}: one native order: {sent:?}");
        let r = &sent[0];
        assert_eq!(
            (r.order_type.clone(), r.side, r.qty, r.price, r.trigger_price, r.reduce_only),
            (kind, -1, 2.0, limit, trigger, false),
            "{verb:?}"
        );
        assert_eq!(
            c.order_client_id_mount.get(&r.order_client_id),
            Some(&0),
            "{verb:?}: the mount's own"
        );
        assert!(
            heard.iter().all(|e| !e.kind.is_terminal()),
            "{verb:?}: nothing ended it: {heard:?}"
        );
    }
}

/// **A row that does not list the kind REFUSES it by name** (polymarket takes market and limit only): nothing
/// reaches the venue, the order is registered `Rejected` under the mount, the ring says the row's words, and
/// the strategy hears the terminal `Rejected` in them. Never a plain stop, never a market.
#[test]
fn on_a_row_that_does_not_list_the_kind_it_is_refused_by_name() {
    for verb in GOOD {
        let (c, heard) = run("polymarket", None, verb);
        let words = format!("TRIGGER_UNSUPPORTED: kind={} venue=polymarket", verb.terms().0);

        assert!(c.engine.client.submissions.is_empty(), "{verb:?}: nothing reaches the venue");
        assert_eq!(armed(&c), 0, "{verb:?}: and the core holds nothing");
        let (order_client_id, mo) =
            c.engine.registry.iter().next().expect("the refused order is registered");
        assert_eq!(mo.status, OrderStatus::Rejected, "{verb:?}: terminal: {mo:?}");
        assert_eq!(
            c.order_client_id_mount.get(order_client_id.as_str()),
            Some(&0),
            "{verb:?}: the mount's order"
        );
        assert_eq!(ring(&c, &words), 1, "{verb:?}: {:?}", c.recent);
        let rejected: Vec<_> = heard
            .iter()
            .filter_map(|e| match &e.kind {
                OrderEventKind::Rejected { reason } => Some(reason.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(rejected, vec![words.as_str()], "{verb:?}: the strategy hears it: {heard:?}");
    }
}

/// **A stated PAPER engine sends it to the simulator** where the row lists the kind, with nothing to say
/// (the order is not reduce-only, so decision 0201's relaxation is not asked). Where the row does not list
/// the kind the paper engine refuses it too: the kinds are the venue's trading rules, which the simulator
/// models, and 0201 relaxes the reduce-only axis alone.
#[test]
fn a_stated_paper_engine_sends_it_to_the_simulator_where_the_row_lists_the_kind() {
    for verb in GOOD {
        let (c, _) = run("deribit", Some(AccountTier::Paper), verb);
        let sent = &c.engine.client.submissions;
        assert_eq!(sent.len(), 1, "{verb:?}: {sent:?}");
        let (kind, limit, trigger) = verb.terms();
        assert_eq!(
            (sent[0].order_type.clone(), sent[0].price, sent[0].trigger_price),
            (kind, limit, trigger)
        );
        assert_eq!(ring(&c, "paper:"), 0, "{verb:?}: nothing to say: {:?}", c.recent);

        let (refused, _) = run("polymarket", Some(AccountTier::Paper), verb);
        assert!(refused.engine.client.submissions.is_empty(), "{verb:?}");
        assert_eq!(
            ring(
                &refused,
                &format!("TRIGGER_UNSUPPORTED: kind={} venue=polymarket", verb.terms().0)
            ),
            1,
            "{verb:?}: {:?}",
            refused.recent
        );
    }
}

/// **Terms or a lifetime the rules refuse never reach the order path**: a trigger at or below zero, a limit
/// at or below zero on a stop-limit, a price that is no finite number (`order_terms_refusal`); a `gtd` with no
/// expiry or one not after the drain's `now`, an expiry on another lifetime (`lifetime_refusal`). On deribit,
/// so for the terms only the rule can refuse: nothing is sent, registered, armed or minted, and the strategy
/// hears a `Denied` in the rule's own words, under a refusal id routed back to the mount.
#[test]
fn terms_the_rule_refuses_are_denied_by_the_drain_before_anything_is_minted() {
    let refused = [
        Verb::StopLimit { trigger: 0.0, limit: LIMIT },
        Verb::StopLimit { trigger: TRIGGER, limit: 0.0 },
        Verb::StopLimit { trigger: TRIGGER, limit: f64::NAN },
        Verb::TakeProfit { trigger: -1.0, limit: None },
        Verb::TakeProfit { trigger: f64::INFINITY, limit: Some(LIMIT) },
        Verb::LimitTimeInForce { price: LIMIT, tif: TimeInForce::Gtd, expiry: None },
        Verb::LimitTimeInForce { price: LIMIT, tif: TimeInForce::Gtd, expiry: Some(0) },
        Verb::LimitTimeInForce { price: LIMIT, tif: TimeInForce::PostOnly, expiry: Some(FAR) },
        Verb::LimitTimeInForce { price: f64::NAN, tif: TimeInForce::Gtc, expiry: None },
    ];
    for verb in refused {
        let (c, heard) = run("deribit", None, verb);
        let (kind, price, trigger) = verb.terms();
        let (tif, expiry) = match verb {
            Verb::LimitTimeInForce { tif, expiry, .. } => (tif, expiry),
            _ => (TimeInForce::Gtc, None),
        };
        let words = vike_model::lifetime_refusal(&kind, tif, expiry, None, c.engine.now_ms)
            .or_else(|| {
                vike_model::order_terms_refusal(&kind, price, trigger, None, None, None, None)
            })
            .unwrap_or_else(|| panic!("{verb:?}: the rules refuse these terms"));

        assert!(c.engine.client.submissions.is_empty(), "{verb:?}: nothing reaches the venue");
        assert!(c.engine.registry.is_empty(), "{verb:?}: nothing was registered");
        assert_eq!(armed(&c), 0, "{verb:?}");
        assert_eq!(
            c.order_client_id_gen.state().1,
            0,
            "{verb:?}: validate-then-mint: no order client id burned"
        );
        let [event] = heard.as_slice() else { panic!("{verb:?}: one event: {heard:?}") };
        assert_eq!(event.kind, OrderEventKind::Denied { reason: words }, "{verb:?}");
        assert_eq!(
            c.order_client_id_mount.get(&event.order_client_id),
            Some(&0),
            "{verb:?}: the mount's"
        );
    }
}

/// A deribit core with a mark of 100 on `X` and one mount running `strategy`, run once (its feed-status call)
/// with every lifecycle event delivered.
fn on_deribit_with_a_mark(strategy: Box<dyn Strategy<LiveBroker> + Send>) -> Core {
    let mut engine = engine_on("deribit", SYM, RecordingClient::default());
    engine.account.set_mark_from("deribit", SYM, 100.0, vike_execution::MarkSource::VenueMark, 0);
    let mount = Mount {
        order_client_id_code: None,
        account: None,
        symbols: Vec::new(),
        controller_id: Some("tr".to_string()),
        underlying_symbol: None,
        venue: "deribit".into(),
        symbol: SYM.into(),
        interval: "1m".into(),
        strategy,
    };
    let mut c =
        core_of(engine, Vec::new(), CoreConfig { strategy: Some(mount), ..CoreConfig::default() });
    c.engine.collect_applied_fills = true;
    c.drive_strategy_feed_status("deribit", SYM, FeedStatus::Disconnected);
    c.dispatch_applied_fills();
    c
}

/// Calls each core-book trailing verb with terms the doors refuse (a trail of zero, a negative trail, a
/// negative offset), and records what it hears.
struct BadTrails {
    heard: Heard,
}

impl Strategy<LiveBroker> for BadTrails {
    fn on_feed_status(&mut self, broker: &mut LiveBroker, _status: FeedStatus) {
        broker.submit_trailing(-1, 2.0, 0.0);
        broker.submit_trailing(-1, 2.0, -5.0);
        broker.submit_trailing_limit(-1, 2.0, 5.0, -1.0);
    }

    fn on_order_event(&mut self, _broker: &mut LiveBroker, event: &OrderEvent) {
        self.heard.lock().unwrap().push(event.clone());
    }
}

/// **The core-book trailing verbs are judged by the doors' terms rule**: a trail not above zero and a
/// negative offset arm NOTHING, and the strategy hears each as a `Denied` in `order_terms_refusal`'s words.
#[test]
fn a_trailing_arm_the_terms_rule_refuses_is_denied_and_arms_nothing() {
    let heard: Heard = Arc::new(Mutex::new(Vec::new()));
    let c = on_deribit_with_a_mark(Box::new(BadTrails { heard: Arc::clone(&heard) }));
    assert_eq!(armed(&c), 0, "nothing armed: {:?}", c.recent);
    let words = |kind, trail, offset| {
        vike_model::order_terms_refusal(&kind, None, None, Some(trail), offset, None, None)
            .expect("refused")
    };
    let want = vec![
        words(OrderType::Trailing, 0.0, None),
        words(OrderType::Trailing, -5.0, None),
        words(OrderType::TrailingLimit, 5.0, Some(-1.0)),
    ];
    let got: Vec<String> = heard
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match &e.kind {
            OrderEventKind::Denied { reason } => Some(reason.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(got, want);
}

/// Calls the two trailing verbs (a SELL of 2 each, trail 5, offset 1) on its first feed-status call.
struct Trails;

impl Strategy<LiveBroker> for Trails {
    fn on_feed_status(&mut self, broker: &mut LiveBroker, _status: FeedStatus) {
        broker.submit_trailing_limit(-1, 2.0, 5.0, 1.0);
        broker.submit_trailing_reduce(-1, 2.0, 5.0, None, Some(1.0));
    }
}

/// **The trailing verbs.** `submit_trailing_limit` arms a trailing-LIMIT in the core's own book (plain, the
/// mount's); `submit_trailing_reduce` is an ORDER through the row, and deribit's row takes no trailing-limit
/// (`TRIGGER_UNSUPPORTED`) but a reduce-only limit, so the CORE holds it as the mount's. Both trail from
/// the mark (100), nothing is sent or minted at the arm, and at 94 both fire LIMITs at 94 (the stop 95 less
/// the offset), never a market: the first closes the long of 2 reduce-only, the second opens the opposite position.
#[test]
fn the_trailing_verbs_arm_in_the_core_and_fire_limits() {
    let mut c = on_deribit_with_a_mark(Box::new(Trails));
    assert_eq!(armed(&c), 2, "both armed in the core: {:?}", c.recent);
    assert!(c.engine.client.submissions.is_empty() && c.engine.registry.is_empty());
    let mut held: Vec<_> =
        c.emulated.cond_engine.values().map(|a| (a.mount, a.reduce_only)).collect();
    held.sort_unstable();
    assert_eq!(held, vec![(Some(0), false), (Some(0), true)], "the mount's: one plain, one held");

    c.engine.account.positions.insert(
        ("deribit".into(), SYM.into(), vike_model::events::PositionSide::Both),
        vike_execution::PositionEntry { size: 2.0, average_price: 100.0, ..Default::default() },
    );
    c.fire_conditionals_at_price("deribit", SYM, 99.0, 1);
    assert!(c.engine.client.submissions.is_empty(), "above the stop: nothing");
    c.fire_conditionals_at_price("deribit", SYM, 94.0, 2);
    let sent: Vec<_> = c
        .engine
        .client
        .submissions
        .iter()
        .map(|r| (r.order_type.clone(), r.price, r.side, r.reduce_only))
        .collect();
    assert_eq!(sent.len(), 2, "{sent:?}: {:?}", c.recent);
    // The held arm fires first and closes the long (reduce-only); its limit is in flight, so the plain arm
    // closes nothing and its whole 2 is the opening order behind it (only one stop closes).
    assert_eq!(
        sent,
        vec![(OrderType::Limit, Some(94.0), -1, true), (OrderType::Limit, Some(94.0), -1, false)],
        "{:?}",
        c.recent
    );
}

/// A lifetime the rule accepts passes the drain untouched: a `gtd` limit with an expiry after `now` reaches a
/// row that takes `gtd` (polymarket) with its lifetime and its expiry, and nothing is denied.
#[test]
fn a_lifetime_the_rule_accepts_passes_the_drain() {
    let (c, heard) = run(
        "polymarket",
        None,
        Verb::LimitTimeInForce { price: 0.45, tif: TimeInForce::Gtd, expiry: Some(FAR) },
    );
    let sent = &c.engine.client.submissions;
    assert_eq!(sent.len(), 1, "{sent:?}: {:?}", c.recent);
    assert_eq!(
        (sent[0].order_type.clone(), sent[0].price, sent[0].time_in_force, sent[0].gtd_expiry),
        (OrderType::Limit, Some(0.45), TimeInForce::Gtd, Some(FAR))
    );
    assert!(heard.iter().all(|e| !e.kind.is_terminal()), "{heard:?}");
}
