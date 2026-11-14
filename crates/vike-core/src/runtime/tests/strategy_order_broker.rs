//! A MOUNTED STRATEGY ON THE PORTABLE ORDER SEAM (`vike_model::OrderBroker` on `LiveBroker`, decision
//! 0269's first wave). White-box, driven the way the runtime drives a strategy
//! (`drive_strategy_feed_status` -> the hook -> `drain_broker` -> `apply_strategy_intent`), then the
//! venue's answer folded and delivered (`dispatch_applied_fills`).
//!
//! What each test pins:
//!
//! * each verb buffers into the queue its inherent twin fills: a tagged order in the ORDER lane on the
//!   mount's own symbol (no symbol, `Gtc`, the kind's prices), a group with each leg's tag, an amend and a
//!   cancel by tag; the average-price read is `0.0` flat;
//! * the drain files a tagged order's tag, and every tag of a group, against the order client id it mints;
//! * a FILL of a tagged order names its tag on `on_fill`, a partial fill and the completing one alike, and
//!   the completion's `Filled` lifecycle names it too as the tag is retired;
//! * a TRAILING leg of a group (decision 0269's W3c) is lowered with its trail and its links: in an OCO it
//!   is sent at once and its sibling's fill cancels it, in an OTO the core holds it until its entry fills.

use std::sync::{Arc, Mutex};

use super::*;
use vike_execution::testing::{RecordingClient, engine_on};
use vike_model::events::{
    FillEvent, OrderAccepted, OrderFilled, OrderPartiallyFilled, OrderSubmitted, PositionSide,
    TradeId,
};
use vike_model::{
    ContingencyType, Fill, OrderBroker, OrderEvent, OrderEventKind, OrderListLeg, OrderPeg,
    OrderPegAnchor, OrderPlacement, OrderType, TimeInForce,
};

use crate::runtime::test_support::core_of;

type Core = CoreThread<RecordingClient>;
type Heard = Arc<Mutex<(Vec<Fill>, Vec<OrderEvent>)>>;
/// What the strategy does on its first feed-status call.
type Calls = fn(&mut LiveBroker);

const VENUE: &str = "deribit";
const SYM: &str = "X";
const PEG: OrderPeg = OrderPeg { anchor: OrderPegAnchor::Best, offset: 0.0 };

/// An empty-buffer, flat `LiveBroker`.
fn flat() -> LiveBroker {
    LiveBroker {
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
    }
}

/// An unlinked `gtc` leg selling 1, plain.
fn leg(order_type: OrderType, price: Option<f64>, trigger_price: Option<f64>) -> OrderListLeg {
    OrderListLeg {
        side: -1,
        qty: 1.0,
        order_type,
        price,
        trigger_price,
        trail: None,
        reduce_only: false,
        time_in_force: TimeInForce::Gtc,
        parent: None,
        contingency: None,
        linked: Vec::new(),
        order_client_id: None,
    }
}

/// A take-profit limit at 110 and a stop at 95, one-cancels-other, tagged `tp` and `sl`.
fn oco() -> Vec<(String, OrderListLeg)> {
    let link = |mut l: OrderListLeg, other: usize| {
        l.contingency = Some(ContingencyType::Oco);
        l.linked = vec![other];
        l
    };
    vec![
        ("tp".to_string(), link(leg(OrderType::Limit, Some(110.0), None), 1)),
        ("sl".to_string(), link(leg(OrderType::Stop, None, Some(95.0)), 0)),
    ]
}

#[test]
fn each_verb_buffers_into_the_queue_its_twin_fills() {
    let mut b = flat();
    let orders = [
        OrderPlacement::Market,
        OrderPlacement::Limit { price: 99.0 },
        OrderPlacement::Stop { trigger: 105.0 },
        OrderPlacement::StopLimit { trigger: 105.0, limit: 106.0 },
        OrderPlacement::TakeProfit { trigger: 95.0, limit: None },
        OrderPlacement::Trailing { trail: 2.0, activation: Some(103.0) },
        OrderPlacement::Pegged { peg: PEG },
        OrderPlacement::Iceberg { price: 99.0, display: 0.25 },
    ];
    for (i, order) in orders.into_iter().enumerate() {
        OrderBroker::place(&mut b, "", &format!("t{i}"), 1, 1.0, order, i % 2 == 1);
    }
    let got: Vec<_> = b
        .submissions
        .iter()
        .map(|s| (s.order_type.clone(), s.price, s.trigger, s.trail, s.reduce_only))
        .collect();
    assert_eq!(
        got,
        vec![
            (OrderType::Market, None, None, None, false),
            (OrderType::Limit, Some(99.0), None, None, true),
            (OrderType::Stop, None, Some(105.0), None, false),
            (OrderType::StopLimit, Some(106.0), Some(105.0), None, true),
            (OrderType::TakeProfit, None, Some(95.0), None, false),
            (OrderType::Trailing, None, Some(103.0), Some(2.0), true),
            (OrderType::Pegged, None, None, None, false),
            (OrderType::Limit, Some(99.0), None, None, true),
        ],
        "each kind as its inherent twin buffers it: the limit as `price`, the trigger (a trailing \
         order's activation) as `trigger`"
    );
    for (i, s) in b.submissions.iter().enumerate() {
        assert_eq!(s.tag.as_deref(), Some(format!("t{i}").as_str()));
        assert!(s.symbol.is_none(), "the mount's own symbol, \"\", rides as none");
        assert_eq!(s.time_in_force, TimeInForce::Gtc);
        assert!(s.release.is_none() && s.attributes.is_none() && s.adjust.is_none());
        let (peg, display) = match i {
            6 => (Some(PEG), None),
            7 => (None, Some(0.25)),
            _ => (None, None),
        };
        assert_eq!((s.peg, s.display_quantity), (peg, display), "a peg rides a pegged order alone");
    }

    OrderBroker::place_group(&mut b, "", oco());
    assert_eq!(b.order_lists.len(), 1);
    assert_eq!(b.order_lists[0].tags, vec!["tp".to_string(), "sl".to_string()]);
    assert_eq!(b.order_lists[0].legs.as_ref().map(Vec::len), Ok(2));

    OrderBroker::amend(&mut b, "t2", Some(2.0), None, Some(104.0));
    OrderBroker::amend(&mut b, "t1", None, Some(98.0), None);
    let amends: Vec<_> = b
        .modifications
        .iter()
        .map(|m| (m.tag.as_str(), m.new_qty, m.new_price, m.new_trigger_price))
        .collect();
    assert_eq!(amends, vec![("t2", Some(2.0), None, Some(104.0)), ("t1", None, Some(98.0), None)]);

    OrderBroker::cancel(&mut b, "t3");
    assert_eq!(b.cancels, vec!["t3".to_string()]);

    assert_eq!(OrderBroker::average_price(&b, ""), 0.0, "flat");
    let held = LiveBroker { position: 2.0, average_price: 101.5, ..flat() };
    assert_eq!(OrderBroker::average_price(&held, ""), 101.5);
    let stale = LiveBroker { position: 0.0, average_price: 101.5, ..flat() };
    assert_eq!(
        OrderBroker::average_price(&stale, ""),
        0.0,
        "a flat book answers 0.0, whatever it last held"
    );
}

/// The fields a buffered submission routes and prices by, written out to compare two rows.
fn row(s: &BufferedSubmit) -> String {
    let route = (&s.symbol, &s.tag, s.side, s.qty, &s.order_type, s.reduce_only, &s.refused);
    let terms = (s.price, s.trigger, s.trail, s.limit_offset, &s.time_in_force, s.gtd_expiry);
    let rest = (&s.release, &s.peg, s.display_quantity, &s.attributes, s.trail_percent, &s.adjust);
    format!("{route:?} {terms:?} {rest:?}")
}

/// An UNTAGGED seam order (`""`) buffers the row its inherent twin buffers, field for field (decision
/// 0277's rule 2): no tag, the symbol as the twin records it, `Gtc` (a plain market or limit: the row
/// the deleted `Broker::submit_market` / `submit_limit` buffered, written out below); an untagged
/// pegged order or iceberg, which only a tag can work, is buffered as a refusal in words.
#[test]
fn an_untagged_order_buffers_its_inherent_twins_row() {
    let mut seam = flat();
    let mut twin = flat();
    OrderBroker::place(&mut seam, "ETHUSDT", "", 1, 1.0, OrderPlacement::Market, false);
    twin.submissions.push(BufferedSubmit {
        symbol: LiveBroker::named("ETHUSDT"),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Market,
        ..Default::default()
    });
    OrderBroker::place(&mut seam, "", "", -1, 2.0, OrderPlacement::Limit { price: 101.0 }, false);
    twin.submissions.push(BufferedSubmit {
        symbol: LiveBroker::named(""),
        side: -1,
        qty: 2.0,
        order_type: OrderType::Limit,
        price: Some(101.0),
        ..Default::default()
    });
    OrderBroker::place(&mut seam, "", "", -1, 2.0, OrderPlacement::Stop { trigger: 95.0 }, true);
    twin.submit_stop_reduce(-1, 2.0, 95.0);
    let take = OrderPlacement::TakeProfit { trigger: 105.0, limit: Some(105.5) };
    OrderBroker::place(&mut seam, "", "", -1, 1.0, take, false);
    twin.submit_take_profit(-1, 1.0, 105.0, Some(105.5));
    let rows = |b: &LiveBroker| b.submissions.iter().map(row).collect::<Vec<_>>();
    assert_eq!(rows(&seam), rows(&twin));

    let mut odd = flat();
    OrderBroker::place(&mut odd, "", "", 1, 1.0, OrderPlacement::Pegged { peg: PEG }, false);
    let [s] = odd.submissions.as_slice() else { panic!("one row") };
    assert_eq!(s.tag, None);
    assert!(s.refused.as_deref().is_some_and(|w| w.contains("give it a tag")), "{:?}", s.refused);
}

/// A TAGGED plain limit through the seam on the mount's own symbol (`""`) buffers
/// `submit_limit_tagged`'s row, field for field; an amend with no trigger `modify`'s, a cancel
/// `cancel_tagged`'s: what an emulated law's child verbs (`vike_execution::emulated::OrderEmulationChildSlot`) send now,
/// so `CoreThread::child_intents` reads the same rows.
#[test]
fn a_tagged_limit_its_amend_and_its_cancel_buffer_the_tagged_verbs_rows() {
    let mut seam = flat();
    let mut twin = flat();
    OrderBroker::place(
        &mut seam,
        "",
        "chase",
        1,
        2.0,
        OrderPlacement::Limit { price: 99.5 },
        false,
    );
    twin.submit_limit_tagged("chase", 1, 2.0, 99.5);
    let rows = |b: &LiveBroker| b.submissions.iter().map(row).collect::<Vec<_>>();
    assert_eq!(rows(&seam), rows(&twin));
    OrderBroker::amend(&mut seam, "chase", None, Some(100.0), None);
    twin.modify("chase", None, Some(100.0));
    let amends = |b: &LiveBroker| {
        let m = b.modifications.iter();
        m.map(|m| (m.tag.clone(), m.new_qty, m.new_price, m.new_trigger_price)).collect::<Vec<_>>()
    };
    assert_eq!(amends(&seam), amends(&twin));
    OrderBroker::cancel(&mut seam, "chase");
    twin.cancel_tagged("chase");
    assert_eq!(seam.cancels, twin.cancels);
    assert_eq!((seam.submissions.len(), seam.modifications.len(), seam.cancels.len()), (1, 1, 1));
}

/// Calls its verbs on its first feed-status call and records every fill and lifecycle event it hears.
struct Seam {
    calls: Calls,
    heard: Heard,
}

impl Strategy<LiveBroker> for Seam {
    fn on_feed_status(&mut self, broker: &mut LiveBroker, _status: FeedStatus) {
        (self.calls)(broker);
    }

    fn on_fill(&mut self, _broker: &mut LiveBroker, fill: &Fill) {
        self.heard.lock().unwrap().0.push(fill.clone());
    }

    fn on_order_event(&mut self, _broker: &mut LiveBroker, event: &OrderEvent) {
        self.heard.lock().unwrap().1.push(event.clone());
    }
}

/// A core whose one engine is `deribit`'s, with one mount on `X` running [`Seam`], after the strategy ran once.
fn run(calls: Calls) -> (Core, Heard) {
    let heard: Heard = Arc::new(Mutex::new((Vec::new(), Vec::new())));
    let mount = Mount {
        order_client_id_code: None,
        account: None,
        symbols: Vec::new(),
        controller_id: Some("seam".to_string()),
        underlying_symbol: None,
        venue: VENUE.into(),
        symbol: SYM.into(),
        interval: "1m".into(),
        strategy: Box::new(Seam { calls, heard: Arc::clone(&heard) }),
    };
    let engine = engine_on(VENUE, SYM, RecordingClient::default());
    let mut c =
        core_of(engine, Vec::new(), CoreConfig { strategy: Some(mount), ..CoreConfig::default() });
    // What `spawn_core` turns on for a mounted primary, which `core_of` does not.
    c.engine.collect_applied_fills = true;
    c.drive_strategy_feed_status(VENUE, SYM, FeedStatus::Disconnected);
    (c, heard)
}

/// The order client id the strategy tag `tag` names, if the registry files one.
fn order_client_id_of(c: &Core, tag: &str) -> Option<String> {
    let suffix = format!("|{tag}");
    c.strategy_tags.iter().find(|(k, _)| k.ends_with(&suffix)).map(|(_, v)| v.clone())
}

fn fill(order_client_id: &str, trade: &str, qty: f64) -> FillEvent {
    FillEvent {
        trade_id: TradeId::prefixed("t-", trade),
        order_client_id: order_client_id.to_string(),
        venue: VENUE.into(),
        symbol: SYM.into(),
        side: 1,
        last_qty: qty,
        last_price: 99.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "maker".to_string().into(),
        timestamp: 0,
        mark_price: Some(99.0),
        position_side: PositionSide::Both,
    }
}

#[test]
fn a_tagged_orders_fills_name_its_tag_partial_and_whole() {
    let (mut c, heard) = run(|b| {
        OrderBroker::place(b, "", "q", 1, 2.0, OrderPlacement::Limit { price: 99.0 }, false)
    });
    let order_client_id = order_client_id_of(&c, "q")
        .expect("the drain filed the tag against the order client id it minted");
    let cid = || order_client_id.clone();
    for event in [
        Event::OrderSubmitted(OrderSubmitted { order_client_id: cid(), timestamp: 0 }),
        Event::OrderAccepted(OrderAccepted {
            order_client_id: cid(),
            venue_order_id: Some(format!("v-{order_client_id}").into()),
            timestamp: 0,
        }),
        Event::Fill(fill(&order_client_id, "a", 1.0)),
        Event::OrderPartiallyFilled(OrderPartiallyFilled {
            order_client_id: cid(),
            fill: fill(&order_client_id, "a", 1.0),
            timestamp: 0,
        }),
    ] {
        c.dispatch(Ingest::Event(event));
    }
    c.dispatch_applied_fills();
    assert_eq!(
        order_client_id_of(&c, "q").as_deref(),
        Some(order_client_id.as_str()),
        "a partial fill keeps the tag"
    );
    for event in [
        Event::Fill(fill(&order_client_id, "b", 1.0)),
        Event::OrderFilled(OrderFilled {
            order_client_id: cid(),
            fill: fill(&order_client_id, "b", 1.0),
            timestamp: 0,
        }),
    ] {
        c.dispatch(Ingest::Event(event));
    }
    c.dispatch_applied_fills();

    let (fills, events) = heard.lock().unwrap().clone();
    let named: Vec<_> = fills.iter().map(|f| (f.size, f.tag.as_deref())).collect();
    assert_eq!(named, vec![(1.0, Some("q")), (1.0, Some("q"))], "each fill names its order");
    let filled: Vec<_> =
        events.iter().filter(|e| e.kind == OrderEventKind::Filled).map(|e| e.tag.clone()).collect();
    assert_eq!(filled, vec![Some("q".to_string())], "the completion names it too");
    assert_eq!(order_client_id_of(&c, "q"), None, "and the tag is retired with the order");
}

#[test]
fn an_untagged_orders_fill_names_nothing() {
    let (mut c, heard) = run(|b| {
        OrderBroker::place(b, "", "", 1, 1.0, OrderPlacement::Limit { price: 99.0 }, false)
    });
    let order_client_id =
        c.order_client_id_mount.keys().next().cloned().expect("the drain minted one order");
    for event in [
        Event::OrderSubmitted(OrderSubmitted {
            order_client_id: order_client_id.clone(),
            timestamp: 0,
        }),
        Event::OrderAccepted(OrderAccepted {
            order_client_id: order_client_id.clone(),
            venue_order_id: Some(format!("v-{order_client_id}").into()),
            timestamp: 0,
        }),
        Event::Fill(fill(&order_client_id, "a", 1.0)),
        Event::OrderFilled(OrderFilled {
            order_client_id: order_client_id.clone(),
            fill: fill(&order_client_id, "a", 1.0),
            timestamp: 0,
        }),
    ] {
        c.dispatch(Ingest::Event(event));
    }
    c.dispatch_applied_fills();
    let fills = heard.lock().unwrap().0.clone();
    assert_eq!(fills.len(), 1, "{fills:?}");
    assert_eq!(fills[0].tag, None);
}

#[test]
fn the_drain_files_every_leg_of_a_tagged_group() {
    let (c, _heard) = run(|b| OrderBroker::place_group(b, "", oco()));
    let (take_profit, stop_loss) = (order_client_id_of(&c, "tp"), order_client_id_of(&c, "sl"));
    assert!(take_profit.is_some() && stop_loss.is_some(), "both legs filed: {:?}", c.strategy_tags);
    assert_ne!(take_profit, stop_loss, "each tag names its own leg");
}

/// An unlinked `gtc` trailing leg selling 1 by `trail`, with no activation.
fn trailing(trail: f64) -> OrderListLeg {
    OrderListLeg { trail: Some(trail), ..leg(OrderType::Trailing, None, None) }
}

/// The venue's acknowledgement of `order_client_id`, folded.
fn accepted(c: &mut Core, order_client_id: &str) {
    for event in [
        Event::OrderSubmitted(OrderSubmitted {
            order_client_id: order_client_id.to_string(),
            timestamp: 0,
        }),
        Event::OrderAccepted(OrderAccepted {
            order_client_id: order_client_id.to_string(),
            venue_order_id: Some(format!("v-{order_client_id}").into()),
            timestamp: 0,
        }),
    ] {
        c.dispatch(Ingest::Event(event));
    }
}

/// The whole fill of the accepted order `order_client_id` (1 at 99 on `side`), folded.
fn filled(c: &mut Core, order_client_id: &str, side: i32) {
    let f = FillEvent { side, ..fill(order_client_id, "f", 1.0) };
    for event in [
        Event::Fill(f.clone()),
        Event::OrderFilled(OrderFilled {
            order_client_id: order_client_id.to_string(),
            fill: f,
            timestamp: 0,
        }),
    ] {
        c.dispatch(Ingest::Event(event));
    }
}

/// The seam's two working kinds take their verbs' path (`LiveBroker::submit_limit_display_tagged`,
/// `submit_pegged_tagged`): a tagged iceberg reaches the venue with its visible size where the row wires
/// one (deribit), and a tagged pegged order the row does not hold is the core's to emulate (decision
/// 0275), never refused for its kind or for a peg it lacks.
#[test]
fn a_tagged_iceberg_reaches_the_venue_with_its_slice_and_a_peg_is_taken_to_emulate() {
    const ICEBERG: OrderPlacement = OrderPlacement::Iceberg { price: 99.0, display: 0.5 };
    let (c, _heard) = run(|b| OrderBroker::place(b, "", "i", 1, 2.0, ICEBERG, false));
    let sent = c.engine.client.submissions.last().expect("deribit wires a visible size");
    assert_eq!(
        (sent.order_type.clone(), sent.price, sent.display_quantity),
        (OrderType::Limit, Some(99.0), Some(0.5))
    );

    let (mut c, heard) =
        run(|b| OrderBroker::place(b, "", "p", 1, 2.0, OrderPlacement::Pegged { peg: PEG }, false));
    c.dispatch_applied_fills();
    let ring: Vec<&str> = c.recent.iter().map(|n| &**n).collect();
    assert!(
        ring.iter().all(|n| !n.contains("ORDER_KIND_UNSUPPORTED") && !n.contains("its peg")),
        "not refused for its kind or its peg: {ring:?}"
    );
    let heard = heard.lock().unwrap();
    let refused = |e: &&OrderEvent| {
        matches!(e.kind, OrderEventKind::Denied { .. } | OrderEventKind::Rejected { .. })
    };
    assert!(!heard.1.iter().any(|e| refused(&e)), "{:?}", heard.1);
}

/// `OrderBroker::deny` (the order language's refusal of terms it cannot send) reaches no venue and
/// ends as a terminal `Denied` the strategy hears, under its tag and in its words, as any drain
/// refusal does.
#[test]
fn a_denied_order_is_heard_under_its_tag_in_its_words_and_sent_nowhere() {
    let (mut c, heard) =
        run(|b| OrderBroker::deny(b, "", "i#1", 2.0, "order \"i\": .iceberg(0) shows no slice"));
    c.dispatch_applied_fills();
    assert!(c.engine.client.submissions.is_empty(), "nothing reaches the venue");
    let heard = heard.lock().unwrap();
    assert!(
        matches!(heard.1.as_slice(), [e] if e.tag.as_deref() == Some("i#1")
            && matches!(&e.kind, OrderEventKind::Denied { reason } if reason.contains(".iceberg(0)"))),
        "{:?}",
        heard.1
    );
}

#[test]
fn a_trailing_leg_in_an_oco_is_sent_with_its_trail_and_its_siblings_fill_cancels_it() {
    let (mut c, _heard) = run(|b| {
        let link = |mut l: OrderListLeg, other: usize| {
            l.contingency = Some(ContingencyType::Oco);
            l.linked = vec![other];
            l
        };
        OrderBroker::place_group(
            b,
            "",
            vec![
                ("tp".to_string(), link(leg(OrderType::Limit, Some(110.0), None), 1)),
                ("tr".to_string(), link(trailing(2.0), 0)),
            ],
        );
    });
    let (take_profit, tr) = (
        order_client_id_of(&c, "tp").expect("filed"),
        order_client_id_of(&c, "tr").expect("filed"),
    );
    let sent = c.engine.client.submissions.iter().find(|o| o.order_client_id == tr);
    let sent = sent.expect("the trailing leg goes to the venue at once");
    assert_eq!((sent.order_type.clone(), sent.trail), (OrderType::Trailing, Some(2.0)));
    assert!(c.contingency.siblings_of(&tr).contains(&take_profit), "the core holds the link");

    accepted(&mut c, &take_profit);
    accepted(&mut c, &tr);
    filled(&mut c, &take_profit, -1);
    assert_eq!(c.engine.client.cancels, vec![tr], "the target's fill cancels the trailing leg");
}

#[test]
fn a_trailing_leg_in_an_oto_is_held_until_its_entry_fills_then_sent_with_its_trail() {
    let (mut c, _heard) = run(|b| {
        let entry = OrderListLeg { side: 1, ..leg(OrderType::Limit, Some(99.0), None) };
        let exit = OrderListLeg { parent: Some(0), reduce_only: true, ..trailing(2.0) };
        OrderBroker::place_group(b, "", vec![("e".to_string(), entry), ("tr".to_string(), exit)]);
    });
    let (e, tr) =
        (order_client_id_of(&c, "e").expect("filed"), order_client_id_of(&c, "tr").expect("filed"));
    let ids = |c: &Core| {
        c.engine.client.submissions.iter().map(|o| o.order_client_id.clone()).collect::<Vec<_>>()
    };
    assert_eq!(ids(&c), vec![e.clone()], "only the entry goes to the venue");
    let held = c.held_orders.get(&tr).map(|o| o.trail);
    assert_eq!(held, Some(Some(2.0)), "the trailing child is held with its trail");

    accepted(&mut c, &e);
    filled(&mut c, &e, 1);
    assert_eq!(ids(&c), vec![e, tr], "the entry's fill releases it");
    let sent = c.engine.client.submissions.last().expect("released");
    assert_eq!(
        (sent.order_type.clone(), sent.trail, sent.reduce_only),
        (OrderType::Trailing, Some(2.0), true)
    );
}
