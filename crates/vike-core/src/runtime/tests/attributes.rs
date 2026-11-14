//! The attributes wave in the core (decision 0267, the owner's question 3): the core-held reduce-only
//! exits of its three forms (a trailing take-profit, a percent trail, an adjustable stop), their law in
//! the core's conditional book, their cover on the POSITION WITHOUT STOP line, and the strategy verbs
//! that send them through the capability row. White-box, on the real `lower_submit`, the emulator, the
//! drain and the reconcile pass. The held-set matrix by name is `tests/apply/conditionals_preflight.rs`'s;
//! the replay fence of an adjusted stop is `tests/journal/conditional_journal/adjusted_stop_fence.rs`.

use std::sync::{Arc, Mutex};

use super::*;
use vike_execution::testing::{RecordingClient, engine_on};
use vike_model::events::PositionSide;
use vike_model::{
    OrderAttributes, OrderEvent, OrderEventKind, OrderKind, OrderResting, OrderStatus,
    OrderStopAdjust, OrderStopAdjustTo, OrderType,
};

use crate::emulator::{ConditionalArms, OrderEmulationArm, OrderEmulationFire};
use crate::runtime::test_support::core_of;

type Core = CoreThread<RecordingClient>;
type Heard = Arc<Mutex<Vec<OrderEvent>>>;
/// One strategy verb call, run once by [`Calls`].
type Verb = Box<dyn FnOnce(&mut LiveBroker) + Send>;
/// A held form's case: the request, what the note names, the safe ticks, the crossing one, the limit sent.
type HeldCase = (OrderRequest, &'static str, Vec<f64>, f64, Option<f64>);

const SYM: &str = "X";

// ── the book: the three forms' law, as the core holds them ─────────────────────────────────────────

/// A SELL arm of 2 of `kind`, its terms set by `terms`.
fn arm(kind: OrderKind, terms: impl FnOnce(&mut OrderResting)) -> OrderEmulationArm {
    let mut order = OrderResting::new(kind, -1, 2.0);
    terms(&mut order);
    OrderEmulationArm { order, trigger_by: None }
}

/// `armed` alone in a book, then each of `prices` as a tick in turn: the fires, one slot per tick.
fn ticks(armed: OrderEmulationArm, prices: &[f64]) -> Vec<Option<OrderEmulationFire>> {
    let mut book = ConditionalArms::new();
    assert!(book.add("a0", armed), "the book holds it");
    prices.iter().enumerate().map(|(i, &px)| book.check_price(px, i as i64).pop()).collect()
}

/// **The book holds each form and reads its level**: a trailing take-profit's trigger mirrors a trailing
/// stop's (a sell's sits above its low), a percent trail is its percent of the extreme, an unactivated one
/// waits at its activation, and a trailing take-profit with nothing to trail from is not held at all.
#[test]
fn the_book_holds_each_form_and_reads_its_level() {
    let ttp =
        arm(OrderKind::TrailingTakeProfit, |o| (o.trail, o.extreme) = (Some(5.0), Some(100.0)));
    assert_eq!(ttp.trigger_level(), 105.0, "a sell: its low plus the trail");
    let mut buy = ttp.clone();
    buy.order.side = 1;
    assert_eq!(buy.trigger_level(), 95.0, "a buy: its high less the trail");
    let pct =
        arm(OrderKind::Trailing, |o| (o.trail_percent, o.extreme) = (Some(10.0), Some(100.0)));
    assert_eq!(pct.trigger_level(), 90.0, "10% under the extreme");
    let waits =
        arm(OrderKind::Trailing, |o| (o.trail_percent, o.activation) = (Some(10.0), Some(120.0)));
    assert_eq!(waits.trigger_level(), 120.0, "it waits for its activation");
    assert!(ConditionalArms::new().add("w", waits), "held unactivated");
    let bare = arm(OrderKind::TrailingTakeProfit, |o| o.trail = Some(5.0));
    assert!(!ConditionalArms::new().add("b", bare), "no extreme and no activation: not held");
    let adj = arm(OrderKind::Stop, |o| {
        o.price = Some(95.0);
        o.adjust = Some(Box::new(OrderStopAdjust {
            at: 105.0,
            to: OrderStopAdjustTo::Stop { stop: 101.0 },
        }));
    });
    assert_eq!(adj.trigger_level(), 95.0, "before its adjustment, its own stop");
}

/// **A trailing take-profit fires on the turn off its low**, a market, or with an offset a limit that far
/// below the trigger; a percent trailing stop fires at its percent off the RATCHETED extreme.
#[test]
fn a_trailing_take_profit_and_a_percent_trail_fire_where_their_law_says() {
    let ttp = |offset| {
        arm(OrderKind::TrailingTakeProfit, |o| {
            (o.trail, o.extreme, o.limit_offset) = (Some(5.0), Some(100.0), offset);
        })
    };
    // 98 lowers the low (trigger 103), 102.9 is short of it, 103.5 reaches it
    let fired = ticks(ttp(None), &[98.0, 102.9, 103.5]);
    assert!(fired[0].is_none() && fired[1].is_none(), "{fired:?}");
    let f = fired[2].as_ref().expect("fired at the turn");
    assert_eq!((f.trigger_price, f.limit, f.qty), (103.5, None, 2.0), "a market");
    let f = ticks(ttp(Some(1.0)), &[98.0, 103.5]).pop().flatten().expect("fired");
    assert_eq!(f.limit, Some(102.0), "a limit 1 below the trigger 103");

    let pct =
        arm(OrderKind::Trailing, |o| (o.trail_percent, o.extreme) = (Some(10.0), Some(100.0)));
    let fired = ticks(pct, &[110.0, 99.5, 98.9]);
    assert!(fired[0].is_none() && fired[1].is_none(), "110 ratchets the stop to 99: {fired:?}");
    assert_eq!(fired[2].as_ref().map(|f| (f.limit, f.qty)), Some((None, 2.0)));
}

/// **An adjustable stop becomes its target once the price reaches its level, once, and is judged as the
/// target from the next tick**: a stop moves, a stop-limit fires its limit, a trailing stop trails from the
/// target's stop. Before the level it is the stop it was (104 neither fires nor adjusts it).
#[test]
fn an_adjustable_stop_becomes_its_target_at_its_level_and_fires_as_the_target() {
    let adjustable = |to| {
        arm(OrderKind::Stop, |o| {
            o.price = Some(95.0);
            o.adjust = Some(Box::new(OrderStopAdjust { at: 105.0, to }));
        })
    };
    let fired =
        ticks(adjustable(OrderStopAdjustTo::Stop { stop: 101.0 }), &[104.0, 105.0, 101.5, 100.5]);
    assert!(fired[..3].iter().all(Option::is_none), "{fired:?}");
    assert_eq!(fired[3].as_ref().map(|f| f.limit), Some(None), "the moved stop at 101, a market");

    let to = OrderStopAdjustTo::StopLimit { stop: 101.0, limit: 100.5 };
    let fired = ticks(adjustable(to), &[105.0, 100.8]);
    assert_eq!(fired[1].as_ref().map(|f| f.limit), Some(Some(100.5)), "{fired:?}");

    let to = OrderStopAdjustTo::Trailing { stop: 101.0, trail: Some(4.0), trail_percent: None };
    let fired = ticks(adjustable(to), &[105.0, 108.0, 104.5, 103.9]);
    assert!(fired[..3].iter().all(Option::is_none), "108 ratchets the stop to 104: {fired:?}");
    assert!(fired[3].is_some(), "{fired:?}");
}

// ── the core-held exits (`arm_core_held_stop`) ─────────────────────────────────────────────────────

/// A polymarket core (its row has no trigger kind and takes a reduce-only limit) holding +2 of `X`, its
/// mark at 100 for a trailing arm to seed from.
fn polymarket_long() -> Core {
    let mut c = core_of(
        engine_on("polymarket", SYM, RecordingClient::default()),
        Vec::new(),
        CoreConfig::default(),
    );
    c.engine.account.positions.insert(
        ("polymarket".into(), SYM.into(), PositionSide::Both),
        vike_execution::PositionEntry { size: 2.0, average_price: 100.0, ..Default::default() },
    );
    c.engine.account.set_mark_from("polymarket", SYM, 100.0, MarkSource::VenueMark, 0);
    c
}

/// A reduce-only SELL of 5 of `kind` on polymarket, its terms set by `terms`.
fn exit(kind: OrderType, terms: impl FnOnce(&mut OrderRequest)) -> OrderRequest {
    let mut req = OrderRequest {
        venue: "polymarket".into(),
        symbol: SYM.into(),
        side: -1,
        qty: 5.0,
        order_type: kind,
        reduce_only: true,
        ..Default::default()
    };
    terms(&mut req);
    req
}

fn armed(c: &Core) -> usize {
    c.emulated.order_emulation_arms.values().map(ConditionalArms::len).sum()
}

/// **Each form is HELD where the row refuses it, and fires a capped reduce-only exit** (decision 0267,
/// question 3): nothing minted or sent at the arm, the note names what the row lacks, a tick on the safe
/// side fires nothing, and the crossing one sends the position (2), not the armed 5, reduce-only: a market,
/// or a limit for a trailing take-profit with an offset. The adjustable stop adjusts at 105 first.
#[test]
fn each_form_is_held_where_the_row_refuses_it_and_fires_a_capped_reduce_only_exit() {
    let adjust = Some(OrderStopAdjust { at: 105.0, to: OrderStopAdjustTo::Stop { stop: 101.0 } });
    // (the request, what the note names, the ticks before the cross, the cross, the limit sent)
    let cases: [HeldCase; 4] = [
        (
            exit(OrderType::TrailingTakeProfit, |r| r.trail = Some(5.0)),
            "reduce-only trailing_take_profit",
            vec![104.0],
            105.5,
            None,
        ),
        (
            exit(OrderType::TrailingTakeProfit, |r| {
                (r.trail, r.limit_offset) = (Some(5.0), Some(1.0))
            }),
            "reduce-only trailing_take_profit",
            vec![104.0],
            105.5,
            Some(104.0),
        ),
        (
            exit(OrderType::Trailing, |r| r.trail_percent = Some(5.0)),
            "reduce-only percent trailing",
            vec![96.0],
            94.5,
            None,
        ),
        (
            exit(OrderType::Stop, |r| (r.trigger_price, r.adjust) = (Some(95.0), adjust)),
            "reduce-only adjustable stop",
            vec![104.0, 105.0, 101.5],
            100.5,
            None,
        ),
    ];
    for (req, lacks, safe, cross, limit) in cases {
        let what = format!("{} {:?}", req.order_type, req.limit_offset);
        let mut c = polymarket_long();
        let sequence_before = c.order_client_id_gen.state().1;
        assert!(c.apply_intent(OrderIntent::Submit(Box::new(req)), 7).is_empty(), "{what}");
        assert_eq!(armed(&c), 1, "{what}: held: {:?}", c.recent);
        assert!(c.engine.registry.is_empty() && c.engine.client.submissions.is_empty(), "{what}");
        assert_eq!(
            c.order_client_id_gen.state().1,
            sequence_before,
            "{what}: no order client id was burned"
        );
        let says = format!("polymarket has NO native {lacks}");
        assert!(c.recent.iter().any(|n| n.contains(&says)), "{what}: {:?}", c.recent);
        for (i, px) in safe.into_iter().enumerate() {
            c.fire_conditionals_at_price("polymarket", SYM, px, 8 + i as i64);
        }
        assert!(c.engine.client.submissions.is_empty(), "{what}: the safe side fires nothing");
        c.fire_conditionals_at_price("polymarket", SYM, cross, 20);
        let sent = &c.engine.client.submissions;
        assert_eq!(sent.len(), 1, "{what}: one close: {sent:?}");
        let kind = if limit.is_some() { OrderType::Limit } else { OrderType::Market };
        assert_eq!(
            (sent[0].side, sent[0].qty, sent[0].reduce_only, &sent[0].order_type, sent[0].price),
            (-1, 2.0, true, &kind, limit),
            "{what}: the position, reduce-only"
        );
    }
}

/// **The terms rules judge a held form, and an ENTRY keeps its refusal**: an adjustment on the wrong side
/// of the stop (`stop_adjust_refusal`) and a percent of 100 (`order_terms_refusal`) are not held, so the row
/// refuses them in its words; a trailing take-profit that is not reduce-only is refused by name too.
#[test]
fn a_form_its_rules_refuse_or_an_entry_is_refused_by_the_row_and_never_held() {
    let wrong_side = Some(OrderStopAdjust { at: 90.0, to: OrderStopAdjustTo::Stop { stop: 93.0 } });
    let cases = [
        exit(OrderType::Stop, |r| (r.trigger_price, r.adjust) = (Some(95.0), wrong_side)),
        exit(OrderType::Trailing, |r| r.trail_percent = Some(100.0)),
        exit(OrderType::TrailingTakeProfit, |r| (r.trail, r.reduce_only) = (Some(5.0), false)),
    ];
    for req in cases {
        let what = format!("{:?}", (&req.order_type, req.adjust, req.trail_percent));
        let mut c = polymarket_long();
        let order_client_id = c.apply_intent(OrderIntent::Submit(Box::new(req)), 7);
        assert_eq!(armed(&c), 0, "{what}: not held");
        let mo = c.engine.registry.get(&order_client_id[0]).expect("refused in words, registered");
        assert_eq!(mo.status, OrderStatus::Rejected, "{what}");
        assert!(
            c.recent.iter().any(|n| n.contains("TRIGGER_UNSUPPORTED")),
            "{what}: {:?}",
            c.recent
        );
    }
}

// ── the POSITION WITHOUT STOP line ─────────────────────────────────────────────────────────────────

/// Two quarantine passes of binance's one account reporting a long of 2 `BTCUSDT`, nothing resting: the
/// second NAMES a bare position.
fn two_passes(c: &mut Core) {
    for _ in 0..2 {
        c.engine.now_ms += 60_000;
        c.reconcile_reports(ReconcileReports {
            venue: "binance".into(),
            since: 0,
            orders: Vec::new(),
            fills: Vec::new(),
            positions: vec![vike_model::PositionStatusReport {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                position_side: PositionSide::Both,
                qty: 2.0,
                average_price: 100.0,
                timestamp: 1,
                margin_mode: Default::default(),
                isolated_margin: None,
                delta: None,
            }],
            policy: vike_execution::recon::ReconcilePolicy {
                default: vike_execution::recon::ReconcileMode::Quarantine,
                ..Default::default()
            },
            balance: None,
            generate_missing_orders: false,
            reconcile_balance: false,
            balance_tol: vike_execution::recon::ReconcileBalanceTolerance::default(),
            route_key: None,
            admitted_symbols: None,
            lists_trigger_orders: false,
        });
    }
}

/// **A percent trailing arm and an adjustable stop are COVER, a trailing take-profit is not** (it closes a
/// winner): each held SELL of 2 against the long of 2, and the line names the position only beside the
/// trailing take-profit.
#[test]
fn a_percent_trail_and_an_adjustable_stop_are_cover_and_a_trailing_take_profit_is_not() {
    let adjustable = arm(OrderKind::Stop, |o| {
        o.price = Some(95.0);
        o.adjust = Some(Box::new(OrderStopAdjust {
            at: 105.0,
            to: OrderStopAdjustTo::Stop { stop: 101.0 },
        }));
    });
    let cases = [
        (arm(OrderKind::Trailing, |o| (o.trail_percent, o.extreme) = (Some(5.0), Some(100.0))), 0),
        (adjustable, 0),
        (
            arm(OrderKind::TrailingTakeProfit, |o| (o.trail, o.extreme) = (Some(5.0), Some(100.0))),
            1,
        ),
    ];
    for (armed, named) in cases {
        let what = format!("{:?}", armed.order.kind);
        let mut c = core_of(
            engine_on("binance", "BTCUSDT", RecordingClient::default()),
            Vec::new(),
            CoreConfig::default(),
        );
        let book = c
            .emulated
            .order_emulation_arms
            .entry(("binance".into(), "BTCUSDT".into()))
            .or_default();
        assert!(book.add("a0", armed), "{what}");
        two_passes(&mut c);
        let raised = c.recent.iter().filter(|l| l.contains("POSITION WITHOUT STOP:")).count();
        assert_eq!(raised, named, "{what}: {:?}", c.recent);
    }
}

// ── the strategy verbs (decision 0237's shape) ─────────────────────────────────────────────────────

/// Calls its one closure on its first feed-status call, and records every lifecycle event it hears.
struct Calls {
    verb: Option<Verb>,
    heard: Heard,
}

impl Strategy<LiveBroker> for Calls {
    fn on_feed_status(&mut self, broker: &mut LiveBroker, _status: FeedStatus) {
        if let Some(verb) = self.verb.take() {
            verb(broker);
        }
    }
    fn on_order_event(&mut self, _broker: &mut LiveBroker, event: &OrderEvent) {
        self.heard.lock().unwrap().push(event.clone());
    }
}

/// A core whose one engine is `venue`'s on `X`, one mount running `verb` once; every event delivered.
fn run(
    venue: &str,
    verb: impl FnOnce(&mut LiveBroker) + Send + 'static,
) -> (Core, Vec<OrderEvent>) {
    let heard: Heard = Arc::new(Mutex::new(Vec::new()));
    let mount = Mount {
        order_client_id_code: None,
        account: None,
        symbols: Vec::new(),
        controller_id: Some("at".to_string()),
        underlying_symbol: None,
        venue: venue.into(),
        symbol: SYM.into(),
        interval: "1m".into(),
        strategy: Box::new(Calls { verb: Some(Box::new(verb)), heard: Arc::clone(&heard) }),
    };
    let engine = engine_on(venue, SYM, RecordingClient::default());
    let mut c =
        core_of(engine, Vec::new(), CoreConfig { strategy: Some(mount), ..CoreConfig::default() });
    c.engine.collect_applied_fills = true;
    c.engine.account.set_mark_from(venue, SYM, 100.0, MarkSource::VenueMark, 0);
    c.drive_strategy_feed_status(venue, SYM, FeedStatus::Disconnected);
    c.dispatch_applied_fills();
    let heard = heard.lock().unwrap().clone();
    (c, heard)
}

/// The one `Rejected` the strategy heard, in its words.
fn rejected(heard: &[OrderEvent]) -> String {
    let words: Vec<_> = heard
        .iter()
        .filter_map(|e| match &e.kind {
            OrderEventKind::Rejected { reason } | OrderEventKind::Denied { reason } => {
                Some(reason.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(words.len(), 1, "one refusal: {heard:?}");
    words[0].clone()
}

/// **Each plain verb is an ORDER through the row, refused BY NAME where the row lacks its term**: on
/// deribit (a native stop and trailing stop, no attribute and no percent trail) an attributed limit is
/// `ATTRIBUTE_UNSUPPORTED attribute=all_or_none`, an adjustable stop `attribute=adjust`, a trailing
/// take-profit and a percent trail (a stop's or a take-profit's) `TRIGGER_UNSUPPORTED`; the strategy hears
/// each, nothing is sent.
#[test]
fn each_plain_verb_is_refused_by_name_where_the_row_lacks_its_term() {
    let aon = OrderAttributes { all_or_none: true, ..Default::default() };
    let adjust = OrderStopAdjust { at: 90.0, to: OrderStopAdjustTo::Stop { stop: 93.0 } };
    let cases: [(Verb, &str); 5] = [
        (
            Box::new(move |b| b.submit_limit_attributed(1, 1.0, 99.0, aon)),
            "ATTRIBUTE_UNSUPPORTED: venue=deribit attribute=all_or_none",
        ),
        (
            Box::new(move |b| b.submit_stop_adjustable(1, 1.0, 105.0, None, adjust, false)),
            "ATTRIBUTE_UNSUPPORTED: venue=deribit attribute=adjust",
        ),
        (
            Box::new(|b| b.submit_trailing_take_profit(-1, 1.0, 5.0, None, false)),
            "TRIGGER_UNSUPPORTED: kind=trailing_take_profit venue=deribit",
        ),
        (
            Box::new(|b| {
                b.submit_trailing_percent(OrderType::Trailing, -1, 1.0, 5.0, None, None, false)
            }),
            "TRIGGER_UNSUPPORTED: kind=trailing venue=deribit",
        ),
        (
            Box::new(|b| {
                b.submit_trailing_percent(
                    OrderType::TrailingTakeProfit,
                    -1,
                    1.0,
                    5.0,
                    None,
                    None,
                    false,
                );
            }),
            "TRIGGER_UNSUPPORTED: kind=trailing_take_profit venue=deribit",
        ),
    ];
    for (verb, words) in cases {
        let (c, heard) = run("deribit", verb);
        assert!(rejected(&heard).contains(words), "{words}: {heard:?}");
        assert!(c.engine.client.submissions.is_empty() && armed(&c) == 0, "{words}");
    }
}

/// **A reduce-only verb on a row that refuses it is the CORE's hold, the mount's own**: a trailing
/// take-profit, a percent trail (each of the three trailing kinds, as the simulator's verb takes them) and an
/// adjustable stop on polymarket are armed for the mount, nothing sent.
#[test]
fn a_reduce_only_verb_the_row_refuses_is_held_by_the_core_for_the_mount() {
    let adjust = OrderStopAdjust { at: 105.0, to: OrderStopAdjustTo::Stop { stop: 101.0 } };
    let percent = |kind: OrderType, offset: Option<f64>| -> Verb {
        Box::new(move |b| b.submit_trailing_percent(kind, -1, 1.0, 5.0, None, offset, true))
    };
    let verbs: [Verb; 6] = [
        Box::new(|b| b.submit_trailing_take_profit(-1, 1.0, 5.0, Some(1.0), true)),
        percent(OrderType::Trailing, None),
        percent(OrderType::TrailingLimit, Some(1.0)),
        percent(OrderType::TrailingTakeProfit, None),
        percent(OrderType::TrailingTakeProfit, Some(1.0)),
        Box::new(move |b| b.submit_stop_adjustable(-1, 1.0, 95.0, None, adjust, true)),
    ];
    for verb in verbs {
        let (c, heard) = run("polymarket", verb);
        assert_eq!(armed(&c), 1, "{:?} {heard:?}", c.recent);
        assert_eq!(
            c.emulated.cond_engine.values().next().and_then(|a| a.mount),
            Some(0),
            "the mount's own"
        );
        assert!(c.engine.client.submissions.is_empty() && heard.is_empty(), "{heard:?}");
    }
}

/// **The drain applies the wave's rules, and refuses a price condition naming a venue**: a discretion
/// that is no number above zero (`attributes_refusal`), an adjustment on the wrong side
/// (`stop_adjust_refusal`), and a release read on another venue (the operator's doors alone send one) are each
/// a `Denied` in those words, before anything is minted.
#[test]
fn the_drain_denies_what_the_rules_refuse_and_a_condition_on_another_venue() {
    let bad_discretion = OrderAttributes { discretionary_amount: Some(-1.0), ..Default::default() };
    let wrong_side = OrderStopAdjust { at: 110.0, to: OrderStopAdjustTo::Stop { stop: 108.0 } };
    let elsewhere = vike_model::OrderRelease {
        at: 4_102_444_800_000,
        price: Some(vike_model::OrderReleasePrice {
            symbol: "BTCUSDT".into(),
            level: 100_000.0,
            side: vike_model::OrderReleasePriceSide::Above,
            venue: Some("binance".into()),
        }),
    };
    let cases: [(Verb, &str); 3] = [
        (
            Box::new(move |b| b.submit_limit_attributed(1, 1.0, 99.0, bad_discretion)),
            "the discretionary amount -1 is not a finite number above zero",
        ),
        (
            Box::new(move |b| b.submit_stop_adjustable(1, 1.0, 105.0, None, wrong_side, false)),
            "is not on the protected side of the stop",
        ),
        (
            Box::new(move |b| b.submit_market_at("", 1, 1.0, elsewhere)),
            "a strategy's price condition reads its own venue only: this one names binance",
        ),
    ];
    for (verb, words) in cases {
        let (c, heard) = run("deribit", verb);
        assert!(rejected(&heard).contains(words), "{words}: {heard:?}");
        assert!(
            heard.iter().all(|e| matches!(e.kind, OrderEventKind::Denied { .. })),
            "{words}: a Denied"
        );
        assert!(
            c.engine.registry.is_empty() && c.held_releases.is_empty(),
            "{words}: nothing minted"
        );
    }
}
