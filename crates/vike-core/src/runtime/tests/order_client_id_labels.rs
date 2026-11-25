//! The core's mint (decision 0270, order client id row R2): every id after routing, through the ROUTED engine's
//! plan, labelled from the route (`S` and the mount's code on a mount's route, `E` otherwise) and the
//! folded book (the intent); one stem per bracket and list; a cancel-then-new amend's `R<n>`; and the
//! late-retry memory at every door that takes a caller's own id. A v1 session mints today's bytes.

use super::*;
use crate::runtime::JournalConfig;
use crate::runtime::recent_order_client_ids::RecentOrderClientIds;
use crate::runtime::test_support::{core_of, test_core};
use vike_execution::testing::{RecordingClient, engine_on};
use vike_journal::{Journal, JournalRecord};
use vike_model::events::{OrderAccepted, OrderCanceled, OrderSubmitted};
use vike_model::orders::client_id::{
    OrderClientIdAccountCode, OrderClientIdGenerator, OrderClientIdPlan, OrderClientIdReading,
    OrderClientIdShape, parse_order_client_id,
};
use vike_model::{LadderSpec, LadderTarget, OrderBroker, OrderPlacement, OrderType};
use vike_venues::venue_order_client_id_budget::order_client_id_shape_for;

type Core = CoreThread<RecordingClient>;

/// An untagged v2 session whose unique part starts at `k3x9q2m7d1p4` (sequence 0).
const V2_SESSION: &str = "Wk3x9q2m7d1p4";

/// The refusal every door says for an id the late-retry memory holds.
const BEFORE_RESTART: &str = "was used by an order before this node's last restart (within 24 h): pick a new order client id";

fn session(c: &mut Core, s: &str) {
    c.order_client_id_gen = OrderClientIdGenerator::resume(s.to_string(), 0);
}

fn order(venue: &str, symbol: &str, side: i32, kind: OrderType) -> OrderRequest {
    OrderRequest {
        venue: venue.into(),
        symbol: symbol.into(),
        side,
        qty: 1.0,
        price: (kind == OrderType::Limit).then_some(100.0),
        trigger_price: (kind == OrderType::Stop).then_some(90.0),
        order_type: kind,
        ..Default::default()
    }
}

fn with_id(order_client_id: &str, req: OrderRequest) -> OrderRequest {
    OrderRequest { order_client_id: order_client_id.into(), ..req }
}

fn submit(c: &mut Core, req: OrderRequest) -> Vec<String> {
    c.apply_intent(OrderIntent::Submit(Box::new(req)), 0)
}

fn feed(c: &mut Core, events: Vec<Event>) {
    for event in events {
        c.dispatch(Ingest::Event(event));
    }
}

fn accept(c: &mut Core, order_client_id: &str) {
    feed(
        c,
        vec![
            Event::OrderSubmitted(OrderSubmitted {
                order_client_id: order_client_id.into(),
                timestamp: 0,
            }),
            Event::OrderAccepted(OrderAccepted {
                order_client_id: order_client_id.into(),
                venue_order_id: Some(format!("v-{order_client_id}").into()),
                timestamp: 0,
            }),
        ],
    );
}

/// The ring lines that mention `needle`.
fn said(c: &Core, needle: &str) -> Vec<String> {
    c.recent.iter().filter(|l| l.contains(needle)).map(|l| l.to_string()).collect()
}

fn bracket(stem: Option<&str>) -> vike_model::OrderBracket {
    vike_model::OrderBracket {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        entry_price: Some(100.0),
        stop_loss: 90.0,
        take_profit: 110.0,
        account: None,
        order_client_id: stem.map(str::to_string),
    }
}

/// A long ladder of 2 on sim: entry, stop, two targets.
fn ladder() -> vike_model::OrderList {
    vike_model::build_ladder(&LadderSpec {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 2.0,
        entry_price: Some(100.0),
        stop_loss: 90.0,
        targets: [110.0, 120.0].map(|price| LadderTarget { price, qty: 1.0 }).to_vec(),
        account: None,
    })
    .expect("a valid ladder")
}

fn combo(order_client_id: Option<&str>) -> vike_model::OrderCombo {
    vike_model::OrderCombo {
        venue: "sim".into(),
        side: 1,
        qty: 1.0,
        legs: vec![
            vike_model::OrderComboLeg {
                symbol: "BTC-27MAR26-100000-C".into(),
                ratio: 1,
                limit: None,
            },
            vike_model::OrderComboLeg {
                symbol: "BTC-27MAR26-120000-C".into(),
                ratio: -1,
                limit: None,
            },
        ],
        net_limit: Some(-0.0125),
        time_in_force: vike_model::TimeInForce::Gtc,
        account: None,
        order_client_id: order_client_id.map(str::to_string),
    }
}

/// PRESERVE: a v1 session mints `<session><seq>` at all five sites, one sequence number per order and
/// per leg in the order they always took, whatever labels the core reads.
#[test]
fn a_v1_session_mints_todays_exact_ids_at_every_site() {
    let mut c = test_core();
    session(&mut c, "cafef00d");
    let ids =
        |from: u64, n: u64| (from..from + n).map(|i| format!("cafef00d{i}")).collect::<Vec<_>>();

    assert_eq!(submit(&mut c, order("sim", "BTCUSDT", 1, OrderType::Market)), ids(0, 1));
    assert_eq!(c.apply_intent(OrderIntent::Bracket(Box::new(bracket(None))), 0), ids(1, 3));
    assert_eq!(c.apply_intent(OrderIntent::OrderList(Box::new(ladder())), 0), ids(4, 4));
    let batch = vec![
        order("sim", "BTCUSDT", 1, OrderType::Limit),
        order("sim", "BTCUSDT", -1, OrderType::Limit),
    ];
    assert_eq!(c.apply_intent(OrderIntent::SubmitBatch(batch), 0), ids(8, 2));
    assert_eq!(c.apply_intent(OrderIntent::Combo(Box::new(combo(None))), 0), ids(10, 1));
    assert_eq!(c.order_client_id_gen.state(), ("cafef00d".to_string(), 11));
}

/// One generator, two plans: a Binance engine mints the compact shape (no account) and a Bybit engine the
/// full one (its account code), from ONE sequence, every id distinct and read back by the one parser.
#[test]
fn a_binance_and_a_bybit_engine_mint_compact_and_full_ids_from_one_sequence() {
    let mut c = core_of(
        engine_on("binance", "BTCUSDT.P", RecordingClient::default()),
        vec![(1.0, engine_on("bybit", "BTCUSDT", RecordingClient::default()))],
        CoreConfig::default(),
    );
    session(&mut c, V2_SESSION);
    c.order_client_id_plans[0] = OrderClientIdPlan::new(
        order_client_id_shape_for("binance", 8, 0),
        OrderClientIdAccountCode::of_id(7),
    );
    c.order_client_id_plans[1] =
        OrderClientIdPlan::new(OrderClientIdShape::FULL, OrderClientIdAccountCode::of_id(4));

    let a = submit(&mut c, order("binance", "BTCUSDT.P", 1, OrderType::Limit)).remove(0);
    let b = submit(&mut c, order("bybit", "BTCUSDT", 1, OrderType::Limit)).remove(0);
    let d = submit(&mut c, order("binance", "BTCUSDT.P", -1, OrderType::Limit)).remove(0);

    assert_eq!(
        [a.as_str(), b.as_str(), d.as_str()],
        ["E0000k3x9q2m7d1p4OL", "E000004k3x9q2m7d1p5OL", "E0000k3x9q2m7d1p6OS"]
    );
    assert_eq!(
        c.order_client_id_gen.state().1,
        3,
        "one sequence number per order, across both engines"
    );
    let Some(OrderClientIdReading::V2(pa)) = parse_order_client_id(&a) else { panic!("{a} is v2") };
    let Some(OrderClientIdReading::V2(pb)) = parse_order_client_id(&b) else { panic!("{b} is v2") };
    assert_eq!((pa.account, pb.account), (None, Some(OrderClientIdAccountCode::of_id(4))));
    assert!(c.engine.registry.contains_key(&a) && c.extra_engines[0].1.registry.contains_key(&b));
}

/// A strategy's bracket is ONE stem (one sequence number) with three legs: `S`, the mount's code, the
/// engine's account, then the entry's intent `OLEN` and the exits' `CLSL` / `CLTP1`.
#[test]
fn a_strategy_bracket_is_one_stem_of_three_legs() {
    let mut mount = crate::testkit::mount_of(
        "sim",
        "BTCUSDT",
        "1m",
        Box::new(crate::testkit::AlwaysSubmitStrategy { calls: Arc::default() }),
    );
    mount.order_client_id_code =
        vike_model::orders::client_id::OrderClientIdStrategyCode::parse("00A3");
    let mut c = crate::runtime::test_support::core_with(CoreConfig {
        strategy: Some(mount),
        ..CoreConfig::default()
    });
    session(&mut c, V2_SESSION);
    c.order_client_id_plans[0] =
        OrderClientIdPlan::new(OrderClientIdShape::FULL, OrderClientIdAccountCode::of_id(7));

    let legs = c.apply_strategy_intent(OrderIntent::Bracket(Box::new(bracket(None))), 0, 0);

    assert_eq!(
        legs,
        ["S00A307k3x9q2m7d1p4OLEN", "S00A307k3x9q2m7d1p4CLSL", "S00A307k3x9q2m7d1p4CLTP1"]
    );
    assert_eq!(c.order_client_id_gen.state().1, 1, "a group costs one sequence number");
}

/// What the core does on its own is `E`: a fired core-held stop lowers as a reduce-only sell on its arm's
/// engine (`EngineRoute::Engine`), so it reads `E…CL`.
#[test]
fn an_operators_fired_stop_is_engine_made_and_closes_long() {
    let mut c = test_core();
    session(&mut c, V2_SESSION);
    let fire = OrderRequest { reduce_only: true, ..order("sim", "BTCUSDT", -1, OrderType::Market) };

    let order_client_id = c.apply_intent_routed(
        OrderIntent::Submit(Box::new(fire)),
        0,
        OrderCancelIntent::Unspecified,
        EngineRoute::Engine(0),
    );

    assert_eq!(order_client_id, ["E000000k3x9q2m7d1p4CL"]);
}

/// The intent is read off the folded book: any spot sell is `CL`; a PLAIN stop sell while long closes the
/// long and may open a short, `FL`; a plain buy while long adds, `OL`.
#[test]
fn a_spot_sell_is_cl_and_a_plain_stop_while_long_is_fl() {
    let mut spot = test_core();
    session(&mut spot, V2_SESSION);
    spot.engine.account.admit_margin_mode("BTCUSDT", Some(vike_model::MarginMode::Cash));
    let sold = submit(&mut spot, order("sim", "BTCUSDT", -1, OrderType::Market));
    assert_eq!(sold, ["E000000k3x9q2m7d1p4CL"]);

    let mut c = test_core();
    let open = submit(&mut c, with_id("open1", order("sim", "BTCUSDT", 1, OrderType::Market)));
    assert_eq!(open, ["open1"]);
    let fill = vike_model::events::FillEvent {
        trade_id: vike_model::events::TradeId::prefixed("t-", "open1"),
        order_client_id: "open1".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: 1.0,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        timestamp: 0,
        mark_price: Some(100.0),
        position_side: vike_model::events::PositionSide::Both,
    };
    accept(&mut c, "open1");
    feed(&mut c, vec![Event::Fill(fill)]);
    assert!(
        c.engine.position_size_of("BTCUSDT", vike_model::events::PositionSide::Both) > 0.0,
        "premise: long"
    );
    session(&mut c, V2_SESSION);

    let stop = submit(&mut c, order("sim", "BTCUSDT", -1, OrderType::Stop));
    let add = submit(&mut c, order("sim", "BTCUSDT", 1, OrderType::Market));

    assert_eq!(stop, ["E000000k3x9q2m7d1p4FL"]);
    assert_eq!(add, ["E000000k3x9q2m7d1p5OL"]);
}

/// A binance perpetual stop is amended by cancel-then-new: the replacement is `…R1`, the next `…R2`, and one
/// refused attempt's id is skipped (`…R3`). Each is journaled as a minted submit, no sequence number is
/// spent on any, and every member names the chain's root in `replaces`.
#[test]
fn a_binance_perp_amend_gives_r1_then_r2_journaled_with_replaces_kept() {
    let dir = crate::scratch::Scratch::reserved("order-client-id-r2-amend-generations");
    let mut c = core_of(
        engine_on("binance", "BTCUSDT.P", RecordingClient::default()),
        vec![],
        CoreConfig { journal: Some(JournalConfig::at(dir.to_path_buf())), ..CoreConfig::default() },
    );
    session(&mut c, V2_SESSION);
    c.order_client_id_plans[0] = OrderClientIdPlan::new(
        order_client_id_shape_for("binance", 8, 0),
        OrderClientIdAccountCode::NONE,
    );
    let modify = |c: &mut Core, trigger: f64| {
        c.apply_intent(
            OrderIntent::Modify {
                order_client_id: "E0000k3x9q2m7d1p4CL".into(),
                new_qty: None,
                new_price: Some(trigger),
                new_trigger_price: None,
            },
            10,
        );
        c.engine.client.submissions.last().expect("a replacement").order_client_id.clone()
    };
    let stop =
        OrderRequest { reduce_only: true, ..order("binance", "BTCUSDT.P", -1, OrderType::Stop) };
    let s1 = submit(&mut c, stop).remove(0);
    assert_eq!(s1, "E0000k3x9q2m7d1p4CL");
    accept(&mut c, &s1);

    let r1 = modify(&mut c, 91.0);
    assert_eq!(r1, format!("{s1}R1"));
    accept(&mut c, &r1);
    c.drive_amends(11);
    feed(
        &mut c,
        vec![Event::OrderCanceled(OrderCanceled {
            order_client_id: s1.clone(),
            reason: "venue".to_string().into(),
            timestamp: 0,
            post_only_would_cross: false,
            refusal_id: None,
        })],
    );
    c.drive_amends(12);
    assert!(c.amends.is_empty(), "the first amend ended");

    let r2 = modify(&mut c, 92.0);
    assert_eq!(r2, format!("{s1}R2"), "the live member's next generation");
    feed(
        &mut c,
        vec![
            Event::OrderSubmitted(OrderSubmitted { order_client_id: r2.clone(), timestamp: 0 }),
            Event::OrderRejected(OrderRejected {
                cause: None,
                order_client_id: r2.clone(),
                reason: "venue".to_string().into(),
                timestamp: 0,
                refusal_id: None,
            }),
        ],
    );
    c.drive_amends(13);
    assert!(c.amends.is_empty(), "the refused amend ended, R1 stands");

    let r3 = modify(&mut c, 93.0);
    assert_eq!(r3, format!("{s1}R3"), "R2 is spent by the refused attempt");
    assert_eq!(c.order_client_id_gen.state().1, 1, "a successor spends no sequence number");
    for member in [&r1, &r2, &r3] {
        assert_eq!(c.engine.registry[member].replaces.as_deref(), Some(s1.as_str()), "{member}");
    }

    c.journal.as_mut().expect("journal").flush().expect("flush");
    let minted: Vec<String> = Journal::read_all(&dir)
        .expect("readable")
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::MintedSubmit { request: req, .. } => Some(req.order_client_id),
            _ => None,
        })
        .collect();
    assert_eq!(minted, [s1.clone(), r1, r2, r3]);
}

/// A batch leg that brings an id the engine holds ANSWERED is refused in the single submit's words and
/// leaves the batch; the other legs go on.
#[test]
fn a_batch_leg_reusing_an_answered_id_is_refused_and_the_rest_go() {
    let mut c = test_core();
    submit(&mut c, with_id("dup", order("sim", "BTCUSDT", 1, OrderType::Limit)));
    accept(&mut c, "dup");
    let held = c.engine.registry["dup"].clone();

    let placed = c.apply_intent(
        OrderIntent::SubmitBatch(vec![
            with_id("dup", order("sim", "BTCUSDT", 1, OrderType::Limit)),
            with_id("fresh", order("sim", "BTCUSDT", 1, OrderType::Limit)),
        ]),
        0,
    );

    assert_eq!(placed, ["fresh"]);
    assert_eq!(c.engine.registry["dup"], held, "the held order is untouched");
    let sent: Vec<&str> =
        c.engine.client.submissions.iter().map(|r| r.order_client_id.as_str()).collect();
    assert_eq!(sent, ["dup", "fresh"], "the repeat never reached the client");
    assert_eq!(said(&c, "order dup REFUSED, nothing sent").len(), 1, "{:?}", c.recent);
}

/// The late-retry memory (ids seen before the restart) refuses a caller's own id at every door: a submit,
/// a batch leg, a list leg, a combo and a bracket's stem (under any intent), each in the one sentence,
/// nothing sent and nothing registered.
#[test]
fn an_id_from_before_the_restart_is_refused_by_submit_batch_list_combo_and_bracket() {
    let stem = "C000007k3x9q2m7d1p4";
    let mut c = test_core();
    c.recent_order_client_ids = RecentOrderClientIds::new(
        ["old1", "old2", "old3", "old4", &format!("{stem}FSEN")]
            .map(|id| (id.to_string(), 0))
            .to_vec(),
    );

    assert!(
        submit(&mut c, with_id("old1", order("sim", "BTCUSDT", 1, OrderType::Limit))).is_empty()
    );
    let batch = c.apply_intent(
        OrderIntent::SubmitBatch(vec![
            with_id("old2", order("sim", "BTCUSDT", 1, OrderType::Limit)),
            with_id("new2", order("sim", "BTCUSDT", 1, OrderType::Limit)),
        ]),
        0,
    );
    assert_eq!(batch, ["new2"]);
    let mut list = ladder();
    for (leg, id) in list.legs.iter_mut().zip(["l1", "l2", "old3", "l4"]) {
        leg.order_client_id = Some(id.into());
    }
    assert!(c.apply_intent(OrderIntent::OrderList(Box::new(list)), 0).is_empty());
    assert!(c.apply_intent(OrderIntent::Combo(Box::new(combo(Some("old4")))), 0).is_empty());
    assert!(
        c.apply_intent(OrderIntent::Bracket(Box::new(bracket(Some(stem)))), 0).is_empty(),
        "the stem's entry was used under another intent: the group is the stem's"
    );

    for id in ["old1", "old2", "old3", "old4", &format!("{stem}FSEN")] {
        let words =
            format!("order {id} REFUSED, nothing sent: order client id {id} {BEFORE_RESTART}");
        assert_eq!(said(&c, &words).len(), 1, "{id}: {:?}", c.recent);
    }
    assert_eq!(said(&c, "order list of 4 legs on sim BTCUSDT REFUSED WHOLE").len(), 1);
    assert_eq!(said(&c, "combo on sim REFUSED WHOLE").len(), 1);
    assert_eq!(said(&c, "bracket on sim BTCUSDT REFUSED WHOLE").len(), 1);
    let sent: Vec<&str> =
        c.engine.client.submissions.iter().map(|r| r.order_client_id.as_str()).collect();
    assert_eq!(sent, ["new2"], "only the fresh batch leg was sent");
    assert_eq!(c.engine.registry.len(), 1);
}

/// A SENDER's stem names the bracket's three legs (no sequence number spent, on any session), and a
/// resend of the same bracket is refused whole; a stem that is no v2 stem is refused before anything.
#[test]
fn a_senders_stem_names_the_legs_and_a_resend_is_refused_whole() {
    let stem = "C000007k3x9q2m7d1p4";
    let mut c = test_core();
    session(&mut c, "cafef00d");

    let legs = c.apply_intent(OrderIntent::Bracket(Box::new(bracket(Some(stem)))), 0);
    assert_eq!(legs, [format!("{stem}OLEN"), format!("{stem}CLSL"), format!("{stem}CLTP1")]);
    assert_eq!(c.order_client_id_gen.state().1, 0, "nothing minted");
    let sent = c.engine.client.submissions.len();

    assert!(c.apply_intent(OrderIntent::Bracket(Box::new(bracket(Some(stem)))), 0).is_empty());
    assert_eq!(c.engine.client.submissions.len(), sent, "the resend sent nothing");
    assert_eq!(said(&c, &format!("order {stem}OLEN REFUSED, nothing sent")).len(), 1);

    assert!(
        c.apply_intent(OrderIntent::Bracket(Box::new(bracket(Some("notastem")))), 0).is_empty()
    );
    assert_eq!(said(&c, "its order client id notastem is not a group stem").len(), 1);
    assert_eq!(c.engine.client.submissions.len(), sent);
}

/// A client mints a Binance bracket's stem in the target block's COMPACT shape (no account part): the
/// core derives the three legs from it as given, and refuses the bracket whole when one of a stem's legs
/// is in the late-retry memory.
#[test]
fn a_binance_compact_stem_names_the_legs_and_one_remembered_leg_refuses_the_bracket() {
    let on_binance = |stem: &str| vike_model::OrderBracket {
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        ..bracket(Some(stem))
    };
    let (fresh, remembered) = ("C0000k3x9q2m7d1p4", "C0000k3x9q2m7d1p5");
    let mut c = core_of(
        engine_on("binance", "BTCUSDT.P", RecordingClient::default()),
        vec![],
        CoreConfig::default(),
    );
    session(&mut c, V2_SESSION);
    c.order_client_id_plans[0] = OrderClientIdPlan::new(
        order_client_id_shape_for("binance", 8, 0),
        OrderClientIdAccountCode::NONE,
    );
    c.recent_order_client_ids = RecentOrderClientIds::new(vec![(format!("{remembered}CLTP1"), 0)]);

    let legs = c.apply_intent(OrderIntent::Bracket(Box::new(on_binance(fresh))), 0);
    assert_eq!(legs, [format!("{fresh}OLEN"), format!("{fresh}CLSL"), format!("{fresh}CLTP1")]);
    assert_eq!(c.order_client_id_gen.state().1, 0, "a sender's stem spends no sequence number");
    let sent = c.engine.client.submissions.len();

    assert!(c.apply_intent(OrderIntent::Bracket(Box::new(on_binance(remembered))), 0).is_empty());
    let words =
        format!("order {remembered}CLTP1 REFUSED, nothing sent: order client id {remembered}CLTP1");
    assert_eq!(said(&c, &format!("{words} {BEFORE_RESTART}")).len(), 1, "{:?}", c.recent);
    assert_eq!(said(&c, "bracket on binance BTCUSDT.P REFUSED WHOLE").len(), 1);
    assert_eq!(c.engine.client.submissions.len(), sent, "nothing of the refused bracket was sent");
    assert!(!c.engine.registry.keys().any(|k| k.starts_with(remembered)));
}

/// Rests ONE reduce-only SELL stop of 2 at 95 on its first feed-status call.
struct ReduceStop;

impl Strategy<LiveBroker> for ReduceStop {
    fn on_feed_status(&mut self, broker: &mut LiveBroker, _status: FeedStatus) {
        broker.submit_stop_reduce(-1, 2.0, 95.0);
    }
}

/// A mount on `venue`/`symbol` running `strategy`, its strategy code `00A3`.
fn coded_mount(venue: &str, symbol: &str, strategy: Box<dyn Strategy<LiveBroker> + Send>) -> Mount {
    let mut mount = crate::testkit::mount_of(venue, symbol, "1m", strategy);
    mount.order_client_id_code =
        vike_model::orders::client_id::OrderClientIdStrategyCode::parse("00A3");
    mount
}

/// Long 2 of polymarket's `X` on engine 0.
fn long_two(c: &mut Core) {
    let key = ("polymarket".into(), "X".into(), vike_model::events::PositionSide::Both);
    let entry =
        vike_execution::PositionEntry { size: 2.0, average_price: 100.0, ..Default::default() };
    c.engine.account.positions.insert(key, entry);
}

/// A STRATEGY's core-held stop (polymarket's row rests none) fires on its arm's engine
/// (`EngineRoute::Engine`), and its close is still the strategy's in its id: `S`, the mount's code.
/// An OPERATOR's core-held stop on the same venue stays the engine's own: `E`, `0000`.
#[test]
fn a_strategys_fired_core_held_stop_is_s_and_an_operators_is_e() {
    let mut c = core_of(
        engine_on("polymarket", "X", RecordingClient::default()),
        Vec::new(),
        CoreConfig {
            strategy: Some(coded_mount("polymarket", "X", Box::new(ReduceStop))),
            ..CoreConfig::default()
        },
    );
    long_two(&mut c);
    session(&mut c, V2_SESSION);
    c.order_client_id_plans[0] =
        OrderClientIdPlan::new(OrderClientIdShape::FULL, OrderClientIdAccountCode::of_id(7));
    c.drive_strategy_feed_status("polymarket", "X", FeedStatus::Disconnected);
    assert_eq!(
        c.emulated.cond_engine.values().next().map(|a| a.mount),
        Some(Some(0)),
        "the mount's arm"
    );

    c.fire_conditionals_at_price("polymarket", "X", 94.0, 5);

    let close = &c.engine.client.submissions[0];
    assert!(close.reduce_only, "premise: the core-held stop's close");
    assert_eq!(close.order_client_id, "S00A307k3x9q2m7d1p4CL");
    assert_eq!(c.order_client_id_owner, None, "the owner is cleared after its one submit");

    let mut op = core_of(
        engine_on("polymarket", "X", RecordingClient::default()),
        Vec::new(),
        CoreConfig::default(),
    );
    long_two(&mut op);
    session(&mut op, V2_SESSION);
    op.order_client_id_plans[0] =
        OrderClientIdPlan::new(OrderClientIdShape::FULL, OrderClientIdAccountCode::of_id(7));
    let stop = OrderRequest { reduce_only: true, ..order("polymarket", "X", -1, OrderType::Stop) };
    assert!(submit(&mut op, stop).is_empty(), "premise: held by the core, nothing minted");

    op.fire_conditionals_at_price("polymarket", "X", 89.0, 5);

    assert_eq!(op.engine.client.submissions[0].order_client_id, "E000007k3x9q2m7d1p4CL");
}

/// A strategy's BUDGET-LATCH flatten is the strategy's in its id (`S`, its code), its close a `CL`.
#[test]
fn a_strategys_budget_latch_flatten_is_s() {
    let mut budgets = std::collections::HashMap::new();
    budgets.insert(
        crate::strategy_state::mount_id_with(None, "sim", "BTCUSDT", "1m"),
        MountBudget {
            max_loss: Some(50.0),
            max_notional: None,
            loss_action: vike_model::MountLossAction::Flatten,
            max_leverage: None,
        },
    );
    let strategy = Box::new(crate::testkit::AlwaysSubmitStrategy { calls: Arc::default() });
    let mut c = crate::runtime::test_support::core_with(CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(coded_mount("sim", "BTCUSDT", strategy)),
        mount_budgets: budgets,
        ..CoreConfig::default()
    });
    c.engine.collect_applied_fills = true;
    session(&mut c, V2_SESSION);
    c.order_client_id_plans[0] =
        OrderClientIdPlan::new(OrderClientIdShape::FULL, OrderClientIdAccountCode::of_id(7));
    c.drive_strategy_tick("sim", "BTCUSDT", 100.0, 100.0, 0, |_, b| {
        b.place("BTCUSDT", "", 1, 1.0, OrderPlacement::Market, false)
    });
    let open = crate::runtime::test_support::order_client_id_of(&c, 0);
    assert_eq!(open, "S00A307k3x9q2m7d1p4OL");
    let fill = vike_model::events::FillEvent {
        trade_id: vike_model::events::TradeId::prefixed("t-", &open),
        order_client_id: open.clone(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: 1.0,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        timestamp: 0,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Both,
    };
    c.bus.publish(Event::Fill(fill), &mut c.engine);
    c.dispatch_applied_fills();
    c.engine.price_board.set_mark("sim", "BTCUSDT", 40.0, 2);

    c.sweep_mount_budgets(3);

    assert!(c.mount_latched[0], "premise: the loss of 60 passes the budget of 50");
    let flat = c
        .engine
        .client
        .submissions
        .iter()
        .find(|r| r.reduce_only && r.order_type == OrderType::Market)
        .expect("the latch's flatten");
    assert_eq!(flat.order_client_id, "S00A307k3x9q2m7d1p5CL");
}
