//! The three order-list verbs through the plane's own parser: each builds the `vike_client::verbs` list its MCP twin
//! builds, the REPL line and the one-shot argv are one grammar, every leg goes out with an id, and a malformed line
//! is refused naming what is wrong.

use super::repl_verb;
use vike_client::verbs::{
    OrderListHead, Verb, fill_order_client_id, ladder_list, oco_list, order_client_id_minter,
    oto_list,
};
use vike_model::orders::client_id::OrderClientIdSource;
use vike_tradehub_client::wire::CommandWire;

fn head(side: i32) -> OrderListHead {
    OrderListHead {
        venue: "binance".into(),
        account: Some("PERP".into()),
        symbol: "BTCUSDT.P".into(),
        side,
        qty: 3.0,
    }
}

fn list(line: &str) -> Result<vike_tradehub_client::wire::OrderListWire, String> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    match repl_verb(tokens[0], &tokens[1..])? {
        Verb::OrderList(l) => Ok(l),
        other => panic!("{line}: not an order list: {other:?}"),
    }
}

#[test]
fn each_verb_builds_the_shared_list() {
    assert_eq!(
        list(
            "oco binance/PERP BTCUSDT.P sell 3 --limit 110 --stop 90 --stop-limit 89 --reduce-only"
        ),
        oco_list(head(-1), 110.0, Some(90.0), Some(89.0), None, true)
    );
    assert_eq!(
        list("oto binance/PERP BTCUSDT.P buy 3 @100 --stop 90"),
        oto_list(head(1), Some(100.0), Some(90.0), None, None)
    );
    assert_eq!(
        list("oto binance/PERP BTCUSDT.P buy 3 --limit 120"),
        oto_list(head(1), None, None, Some(120.0), None),
        "no @price is a market entry"
    );
    assert_eq!(
        list(
            "ladder binance/PERP BTCUSDT.P buy 3 @market --stop-loss 90 --take-profit 110:1 \
             --take-profit 120:2"
        ),
        ladder_list(head(1), None, Some(90.0), None, &[(110.0, 1.0), (120.0, 2.0)])
    );
    // `--trail` makes each verb's protective leg a trailing stop, the shared builder's list.
    assert_eq!(
        list("oco binance/PERP BTCUSDT.P sell 3 --limit 110 --trail 5 --reduce-only"),
        oco_list(head(-1), 110.0, None, None, Some(5.0), true)
    );
    assert_eq!(
        list("oto binance/PERP BTCUSDT.P buy 3 @100 --trail 5"),
        oto_list(head(1), Some(100.0), None, None, Some(5.0))
    );
    assert_eq!(
        list("ladder binance/PERP BTCUSDT.P buy 3 --trail 5 --take-profit 110:1"),
        ladder_list(head(1), None, None, Some(5.0), &[(110.0, 1.0)])
    );
    // The ladder's shape is the model's: an entry, the stop for the whole size, the targets, OUO-linked.
    let ladder =
        list("ladder binance/PERP BTCUSDT.P buy 3 --stop-loss 90 --take-profit 110:3").unwrap();
    assert_eq!(ladder.legs.len(), 3);
    assert_eq!((ladder.legs[1].qty, ladder.legs[1].linked.clone()), (3.0, vec![2]));
}

/// **The one-shot sends an id on every leg**: what `execute_write` sends is the verb's command through the one mint
/// (`fill_order_client_id`), which names every leg; `--order-client-id` once per leg pins them instead, and is held to the
/// charset `submit --order-client-id` is held to.
#[test]
fn every_leg_goes_out_with_an_id() {
    let send = |line: &str| {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let verb = repl_verb(tokens[0], &tokens[1..]).expect(line);
        let mut minter = order_client_id_minter(OrderClientIdSource::Cli);
        match fill_order_client_id(
            verb.to_wire_command().expect("a write"),
            &mut minter,
            None,
            |_| true,
        ) {
            CommandWire::OrderList(l) => l,
            other => panic!("not an order list: {other:?}"),
        }
    };
    for line in [
        "oco binance/PERP BTCUSDT.P sell 3 --limit 110 --stop 90",
        "oto binance/PERP BTCUSDT.P buy 3 @100 --stop 90",
        "ladder binance/PERP BTCUSDT.P buy 3 --stop-loss 90 --take-profit 110:1 --take-profit 120:2",
    ] {
        let sent = send(line);
        assert!(
            sent.legs.iter().all(|leg| leg.order_client_id.as_deref().is_some_and(|id| {
                vike_model::is_valid_crypto_order_client_id(id) && id.starts_with('C')
            })),
            "{line}: {sent:?}"
        );
    }
    let pinned = send(
        "oto binance/PERP BTCUSDT.P buy 3 --stop 90 --order-client-id e1 --order-client-id s1",
    );
    let ids: Vec<_> = pinned.legs.iter().map(|leg| leg.order_client_id.as_deref()).collect();
    assert_eq!(ids, [Some("e1"), Some("s1")], "the caller's ids, in leg order, are kept");
}

#[test]
fn a_malformed_line_is_refused_naming_what_is_wrong() {
    for (line, said) in [
        ("oco binance/PERP BTCUSDT.P sell 3 --stop 90", "--limit <price> is required"),
        ("oco binance/PERP BTCUSDT.P sell 3 @100 --limit 110 --stop 90", "takes no @<price>"),
        ("oto binance/PERP BTCUSDT.P buy 3", "[order.list.oto_one_exit]"),
        ("oto binance/PERP BTCUSDT.P buy 3 --stop 90 --limit 120", "[order.list.oto_one_exit]"),
        ("oto binance/PERP BTCUSDT.P buy 3 --stop 90 --trail 5", "[order.list.oto_one_exit]"),
        ("oco binance/PERP BTCUSDT.P sell 3 --limit 110", "[order.list.oco_stop_leg]"),
        (
            "oco binance/PERP BTCUSDT.P sell 3 --limit 110 --stop 90 --trail 5",
            "[order.list.oco_stop_leg]",
        ),
        (
            "oco binance/PERP BTCUSDT.P sell 3 --limit 110 --trail 5 --stop-limit 89",
            "[order.list.oco_stop_leg]",
        ),
        ("oco binance/PERP BTCUSDT.P sell 3 --limit 110 --trail 0", "--trail must be a finite"),
        ("ladder binance/PERP BTCUSDT.P buy 3 --stop-loss 90", "--take-profit"),
        ("ladder binance/PERP BTCUSDT.P buy 3 --take-profit 110:1", "[order.list.ladder_stop_leg]"),
        (
            "ladder binance/PERP BTCUSDT.P buy 3 --stop-loss 90 --trail 5 --take-profit 110:1",
            "[order.list.ladder_stop_leg]",
        ),
        ("ladder binance/PERP BTCUSDT.P buy 3 --stop-loss 90 --take-profit 110", "<price>:<qty>"),
        (
            "ladder binance/PERP BTCUSDT.P buy 3 --stop-loss 90 --take-profit 110:9",
            "past the entry's size",
        ),
        (
            "ladder binance/PERP BTCUSDT.P buy nan --stop-loss 90 --take-profit 110:1",
            "qty must be a finite",
        ),
        ("oco binance/PERP BTCUSDT.P sell --limit 110 --stop 90", "needs <account>"),
        (
            "oto binance/PERP BTCUSDT.P buy 3 --stop 90 --order-client-id e1",
            "1 order client ids for 2 legs",
        ),
        (
            "oto binance/PERP BTCUSDT.P buy 3 --stop 90 --order-client-id e-1 --order-client-id s1",
            "not a usable",
        ),
        (
            "oco binance/PERP BTCUSDT.P sell 3 --limit 110 --stop 90 --node h:1",
            "one-shot verb's flags",
        ),
    ] {
        let err = list(line).expect_err(line);
        assert!(err.contains(said), "{line}: {said:?} not in {err}");
    }
}
