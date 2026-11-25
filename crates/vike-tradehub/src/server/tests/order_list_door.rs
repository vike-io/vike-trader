//! The ORDER LIST at the node's door (wave 4 of the order kinds): `order-list` is advertised beside the arm that
//! lowers it, the lowering is the model's `OrderList` field for field (a ladder through `build_ladder` arrives as
//! the same legs, each with the id its client minted), and `order_list_refusal` refuses each value shape naming its
//! leg and each engine shape naming what the engine trades, while the account gates treat the list as a submit
//! (decision 0254: every leg on one account). Every opening leg is planned and sized, and a retry is refused whole.

use super::control::{ControlLimits, ControlLimitsConfig, command_kind, lower_command};
use super::handshake::served_features;
use super::order_list::{order_list_refusal, planned_order};
use super::refusal::{account_refusal, one_account_addressed, order_client_id_reuse_refusal};
use vike_execution::{Command, OrderIntent};
use vike_model::ContingencyType;
use vike_tradehub_client::wire::{CommandWire, OrderListLegWire, OrderListWire};

/// The wire form of a model list, as a client builds it, each leg with the id `c<leg>` its client minted
/// (`vike_client::verbs`' builders and its mint are the client's own).
fn wire_of(spec: &vike_model::OrderList, account: Option<&str>) -> OrderListWire {
    OrderListWire {
        venue: spec.venue.clone(),
        symbol: spec.symbol.clone(),
        account: account.map(str::to_string),
        legs: spec
            .legs
            .iter()
            .enumerate()
            .map(|(i, leg)| OrderListLegWire {
                side: leg.side,
                qty: leg.qty,
                order_type: leg.order_type.as_str().to_owned(),
                price: leg.price,
                trigger_price: leg.trigger_price,
                trail: leg.trail,
                reduce_only: leg.reduce_only,
                time_in_force: None,
                parent: leg.parent,
                contingency: leg.contingency,
                linked: leg.linked.clone(),
                order_client_id: Some(format!("c{i}")),
            })
            .collect(),
    }
}

/// A long binance-perp ladder: a market entry of 3, a stop at 90, targets of 1 at 110 and 2 at 120.
fn ladder() -> vike_model::OrderList {
    vike_model::build_ladder(&vike_model::LadderSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        side: 1,
        qty: 3.0,
        entry_price: None,
        stop_loss: 90.0,
        targets: vec![
            vike_model::LadderTarget { price: 110.0, qty: 1.0 },
            vike_model::LadderTarget { price: 120.0, qty: 2.0 },
        ],
        account: None,
    })
    .expect("a valid ladder")
}

fn engine(route_key: &str, symbol: &str) -> vike_execution::VenueBlock {
    vike_execution::VenueBlock {
        venue: vike_model::accounts::account_keys::venue_of_route_key(route_key).into(),
        route_key: route_key.into(),
        symbol: symbol.into(),
        ..Default::default()
    }
}

/// The word ships TOGETHER with the arm that lowers it: a ladder lowers onto `OrderIntent::OrderList` as the very
/// spec the model built (account carried, legs, links and the client's ids unchanged), and the `Ack` echoes every
/// leg's id.
#[test]
fn the_node_advertises_the_order_list_beside_the_arm_that_lowers_it() {
    let word = vike_tradehub_client::proto::FEATURE_ORDER_LIST;
    assert_eq!(word, "order-list");
    assert!(served_features(None, false).iter().any(|f| f == word), "the word is advertised");
    let cmd = CommandWire::OrderList(wire_of(&ladder(), Some("PERP")));
    assert_eq!(command_kind(&cmd), "order_list");
    let (lowered, order_client_id) = lower_command(cmd).expect("the arm lowers a ladder");
    let Command::Order(OrderIntent::OrderList(spec)) = lowered else {
        panic!("not lowered to an order list: {lowered:?}");
    };
    let mut want = ladder();
    want.account = vike_model::accounts::account_keys::parse_wire_account("PERP").ok();
    for (i, leg) in want.legs.iter_mut().enumerate() {
        leg.order_client_id = Some(format!("c{i}"));
    }
    assert_eq!(*spec, want);
    assert_eq!(order_client_id, "c0,c1,c2,c3", "the receipt names every leg");
    assert_eq!(
        vike_tradehub_client::proto::NODE_PROTO_VERSION,
        5,
        "no protocol bump (5 is the phase-4 renames', not this feature's)"
    );
}

/// **A remote list names every leg's id** (a remote submit's rule): none, some, an empty one or one named twice is
/// refused before the `Ack`, by the lowering and the door's verdict alike.
#[test]
fn a_list_without_every_legs_own_id_is_refused() {
    let roster = [engine("binance", "BTCUSDT.P")];
    let ok = wire_of(&ladder(), None);
    let with = |edit: fn(&mut OrderListWire)| {
        let mut l = ok.clone();
        edit(&mut l);
        l
    };
    for (bad, said) in [
        (
            with(|l| l.legs.iter_mut().for_each(|leg| leg.order_client_id = None)),
            "requires a pre-minted order_client_id on every leg",
        ),
        (with(|l| l.legs[2].order_client_id = None), "every leg's order id or none"),
        (with(|l| l.legs[2].order_client_id = Some(String::new())), "leg 2's order id is empty"),
        (with(|l| l.legs[3].order_client_id = Some("c0".into())), "named twice"),
    ] {
        let lowered = lower_command(CommandWire::OrderList(bad.clone())).expect_err(said);
        assert!(lowered.text.contains(said), "{said:?} not in {lowered}");
        assert_eq!(order_list_refusal(&CommandWire::OrderList(bad), &roster), Some(lowered));
    }
}

/// **A retried list is refused WHOLE**: one leg's id an order of this node already carries (the entry the first
/// send placed) refuses the list before the `Ack`, naming the id, so no leg of it is sent twice.
#[test]
fn a_retried_list_with_the_same_ids_is_refused_whole() {
    let retry = CommandWire::OrderList(wire_of(&ladder(), Some("PERP")));
    let placed = vike_execution::OrderView {
        data_delay: None,
        order_client_id: "c0".into(),
        venue: "binance".into(),
        account: vike_model::accounts::account_keys::parse_wire_account("PERP").ok(),
        symbol: "BTCUSDT.P".into(),
        side: 1,
        qty: 3.0,
        order_type: vike_model::OrderType::Market,
        price: None,
        trigger_price: None,
        status: vike_model::OrderStatus::Filled,
        venue_order_id: None,
        filled_qty: 3.0,
        average_fill_price: 100.0,
        refusal_id: None,
        reason: None,
        pending_price: None,
        trail: None,
        limit_offset: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        release: None,
        display_quantity: None,
    };
    let said =
        order_client_id_reuse_refusal(&retry, std::slice::from_ref(&placed), &Default::default())
            .expect("refused")
            .text;
    assert!(said.contains("order client id c0") && said.contains("REFUSED whole"), "{said}");
    let fresh = CommandWire::OrderList(OrderListWire {
        legs: wire_of(&ladder(), Some("PERP"))
            .legs
            .into_iter()
            .map(|leg| OrderListLegWire {
                order_client_id: leg.order_client_id.map(|c| format!("n{c}")),
                ..leg
            })
            .collect(),
        ..wire_of(&ladder(), Some("PERP"))
    });
    assert_eq!(
        order_client_id_reuse_refusal(&fresh, &[placed], &Default::default()),
        None,
        "new ids are a new list"
    );
}

/// **EVERY opening leg is sized, not only the first**: a straddle OCO of two MARKET entries, the second over the
/// ceiling at the price the node holds, is refused before the `Ack` naming that leg; under the ceiling it passes.
/// And the symbol plan is made for a leg that needs the price wait when one does.
#[test]
fn every_opening_leg_is_planned_and_sized() {
    let leg = |qty: f64, linked| OrderListLegWire {
        side: 1,
        qty,
        order_type: "market".into(),
        price: None,
        trigger_price: None,
        trail: None,
        reduce_only: false,
        time_in_force: None,
        parent: None,
        contingency: Some(ContingencyType::Oco),
        linked: vec![linked],
        order_client_id: Some(format!("s{linked}")),
    };
    let straddle = |second: f64| {
        CommandWire::OrderList(OrderListWire {
            venue: "binance".into(),
            symbol: "BTCUSDT.P".into(),
            legs: vec![leg(1.0, 1), leg(second, 0)],
            account: Some("PERP".into()),
        })
    };
    let blocks = [engine("binance#PERP", "BTCUSDT.P")];
    let limits =
        ControlLimits::new(ControlLimitsConfig { max_notional: Some(5_000.0), rate_per_sec: 1e9 });
    let held = Some(100.0);
    let said = limits
        .market_reason(&straddle(100.0), &blocks, &[], None, None, held)
        .expect("the second entry is over the ceiling")
        .text;
    assert!(said.contains("leg 1"), "{said}");
    assert_eq!(limits.market_reason(&straddle(10.0), &blocks, &[], None, None, held), None);

    let CommandWire::OrderList(mut list) = straddle(1.0) else { unreachable!() };
    list.legs[0].order_type = "limit".into();
    list.legs[0].price = Some(90.0);
    let planned = planned_order(&list).expect("a list that opens is planned");
    assert_eq!(
        planned.order_type, "market",
        "the leg that needs the price wait is the one planned"
    );
}

/// Each VALUE shape is refused by `lower_command` and by the door's verdict in the same words, naming the leg; a
/// valid list on the engine that trades its symbol passes.
#[test]
fn the_order_list_values_are_refused_naming_the_leg() {
    let roster = [engine("binance", "BTCUSDT.P")];
    let ok = wire_of(&ladder(), None);
    assert_eq!(order_list_refusal(&CommandWire::OrderList(ok.clone()), &roster), None);
    let with = |edit: fn(&mut OrderListWire)| {
        let mut l = ok.clone();
        edit(&mut l);
        l
    };
    for (bad, said) in [
        (with(|l| l.legs[1].side = 0), "leg 1: `side`"),
        (with(|l| l.legs[2].qty = 0.0), "leg 2: `qty`"),
        (with(|l| l.legs[2].price = Some(f64::NAN)), "leg 2: `price`"),
        (with(|l| l.legs[0].order_type = "iceberg".into()), "leg 0: `order_type`"),
        (with(|l| l.legs[1].trigger_price = None), "leg 1: order terms"),
        (with(|l| l.legs[1].order_type = "trailing".into()), "leg 1: order terms"),
        (
            with(|l| l.legs[2].trail = Some(5.0)),
            "leg 2: order terms — a trail belongs to a trailing",
        ),
        (with(|l| l.legs[2].time_in_force = Some("forever".into())), "leg 2: `time_in_force`"),
        (with(|l| l.legs[2].linked = vec![3]), "does not link leg 1 back"),
        (with(|l| l.legs.truncate(1)), "two legs or more"),
        (with(|l| l.symbol = " ".into()), "empty symbol"),
        (with(|l| l.account = Some("bad label!".into())), "`account`"),
    ] {
        let lowered = lower_command(CommandWire::OrderList(bad.clone())).expect_err("refused");
        assert!(lowered.text.contains(said), "lower_command: {said:?} not in {lowered}");
        let door = order_list_refusal(&CommandWire::OrderList(bad), &roster).expect("refused");
        assert_eq!(door, lowered, "the door and the lowering say one sentence");
    }
}

/// A TRAILING leg's trail crosses the door onto the model leg: the list passes the verdict and lowers with it, where a
/// trail-less trailing leg is refused above.
#[test]
fn a_trailing_legs_trail_lowers_onto_the_spec() {
    let mut list = wire_of(&ladder(), None);
    list.legs[1].order_type = "trailing".into();
    list.legs[1].trail = Some(5.0);
    let cmd = CommandWire::OrderList(list);
    assert_eq!(order_list_refusal(&cmd, &[engine("binance", "BTCUSDT.P")]), None);
    let (lowered, _) = lower_command(cmd).expect("a trailing leg lowers");
    let Command::Order(OrderIntent::OrderList(spec)) = lowered else {
        panic!("not lowered to an order list: {lowered:?}");
    };
    assert_eq!(spec.legs[1].order_type, vike_model::OrderType::Trailing);
    assert_eq!((spec.legs[1].trail, spec.legs[1].trigger_price), (Some(5.0), Some(90.0)));
}

/// The ENGINE shapes, which `lower_command` cannot see: no published engine for the list's book, a symbol the engine
/// does not trade, and a stop leg on binance's spot lane. Each names what the engine trades.
#[test]
fn the_order_list_engine_verdict_names_what_the_engine_trades() {
    let list = CommandWire::OrderList(wire_of(&ladder(), None));
    let none = order_list_refusal(&list, &[]).expect("no engine to judge");
    assert!(none.text.contains("publishes no engine for `binance`"), "{none}");
    assert_eq!(none.id, "tradehub.order_list.engine_unknown");
    let other = order_list_refusal(&list, &[engine("binance", "ETHUSDT.P")]).expect("refused");
    assert!(other.text.contains("trades `ETHUSDT.P`, not `BTCUSDT.P`"), "{other}");
    let mut spot = ladder();
    spot.symbol = "BTCUSDT".into();
    let spot = CommandWire::OrderList(wire_of(&spot, None));
    let said = order_list_refusal(&spot, &[engine("binance", "BTCUSDT")]).expect("refused");
    assert!(said.text.contains("spot lane"), "{said}");
    // The named account's engine is the one judged.
    let named = CommandWire::OrderList(wire_of(&ladder(), Some("PERP")));
    let roster = [engine("binance", "BTCUSDT"), engine("binance#PERP", "BTCUSDT.P")];
    assert_eq!(order_list_refusal(&named, &roster), None);
    assert_eq!(order_list_refusal(&CommandWire::Cancel("c".into()), &[]), None, "other verbs");
}

/// The ACCOUNT gates treat a list as a submit (decision 0254): a named account this node does not run is refused
/// naming the ones it does, a bare venue of two accounts is refused, and a bare venue of ONE labelled account has the
/// label written onto the list.
#[test]
fn the_order_list_names_one_account() {
    let roster = ["binance".to_string(), "binance#PERP".to_string()];
    let unknown = CommandWire::OrderList(wire_of(&ladder(), Some("NOPE")));
    let said = account_refusal(&unknown, &roster).expect("refused before the Ack").text;
    assert!(
        said.contains("NOPE") && said.contains("PERP") && said.contains("order list"),
        "{said}"
    );
    assert_eq!(
        account_refusal(&CommandWire::OrderList(wire_of(&ladder(), Some("PERP"))), &roster),
        None
    );

    let bare = CommandWire::OrderList(wire_of(&ladder(), None));
    let blocks = [engine("binance", "BTCUSDT"), engine("binance#ALT", "BTCUSDT.P")];
    let two = ["binance".to_string(), "binance#ALT".to_string()];
    let refused = one_account_addressed(bare.clone(), &two, &[], &blocks, None).expect_err("two");
    assert!(refused.text.contains("the order list"), "{refused}");
    let one = ["binance#ALT".to_string()];
    let Ok(CommandWire::OrderList(addressed)) =
        one_account_addressed(bare, &one, &[], &blocks[1..], None)
    else {
        panic!("a venue of one account takes a bare list");
    };
    assert_eq!(addressed.account.as_deref(), Some("ALT"), "the one label is written out");
}
