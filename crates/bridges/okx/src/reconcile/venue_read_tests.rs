//! The operator's open-orders read (`super`'s module doc), offline: what is asked of the venue and how its rows read.
//! Every test here FAILS on the base, where `crate::adapter`'s `VenueAdapterOkx` kept the trait's default and answered
//! every read `not offered by okx`.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Value, json};
use vike_bridge_core::transport::VenueApiError;
use vike_model::{VenueRead, VenueReadKind, not_offered_words};
use vike_secrets::credentials::Credentials;

use super::{read_open_orders, venue_read};
use crate::execution::rest::PATH_ORDERS_ALGO_PENDING;
use crate::reconcile::{PATH_ORDERS_PENDING, ReconcileClientOkx};
use crate::refusals::OkxAccountRefusal;
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::json::RestTransportJson;

/// The `data` rows of a recorded body under `tests/fixtures/algo/`.
fn rows_of(text: &str) -> Vec<Value> {
    let body: Value = serde_json::from_str(text).expect("the fixture is JSON");
    body["data"].as_array().expect("a `data` list").clone()
}

/// The plain list: a resting LIMIT on a swap (the documented row shape) and the fired stop's child order the demo
/// account listed (`ws_orders_child_live.json`'s row, captured 2026-10-24: OKX's `clOrdId`, ours in `algoClOrdId`).
fn pending_rows() -> Vec<Value> {
    let mut rows = vec![json!({
        "instType": "SWAP", "instId": "ETH-USDT-SWAP", "ordId": "4038740198699210001", "clOrdId": "b7okxlim",
        "algoClOrdId": "", "px": "2100.5", "sz": "3", "ordType": "post_only", "side": "buy", "posSide": "net",
        "accFillSz": "1", "avgPx": "2100.5", "state": "partially_filled", "uTime": "1792866100000"
    })];
    rows.extend(rows_of(include_str!("../../tests/fixtures/algo/ws_orders_child_live.json")));
    rows
}

/// The conditional list: the resting stop the demo account's algo channel pushed live (`ws_orders_algo_live.json`,
/// captured 2026-10-24) and the venue page's take-profit example (`doc_orders_algo_pending_take_profit.json`).
fn conditional_rows() -> Vec<Value> {
    let mut rows = rows_of(include_str!("../../tests/fixtures/algo/ws_orders_algo_live.json"));
    rows.extend(rows_of(include_str!(
        "../../tests/fixtures/algo/doc_orders_algo_pending_take_profit.json"
    )));
    rows
}

/// One request as the venue saw it: path, params.
type Asked = (String, Vec<(String, Value)>);

/// Answers each list from the rows above, records every request; `failing` is an `ordType` (or `pending`) the venue
/// refuses.
#[derive(Clone, Default)]
struct Venue {
    asked: Rc<RefCell<Vec<Asked>>>,
    failing: Option<&'static str>,
}

impl RestTransportJson for Venue {
    fn signed(
        &self,
        _base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        assert_eq!(method, "GET", "a read");
        let owned = params.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect();
        self.asked.borrow_mut().push((path.to_string(), owned));
        let ord_type = params.iter().find(|(k, _)| *k == "ordType").and_then(|(_, v)| v.as_str());
        let list = if path == PATH_ORDERS_PENDING { Some("pending") } else { ord_type };
        if list == self.failing {
            return Ok(json!({"code": "50011", "msg": "Too Many Requests", "data": []}));
        }
        let data = match (path, ord_type) {
            (PATH_ORDERS_PENDING, None) => pending_rows(),
            (PATH_ORDERS_ALGO_PENDING, Some("conditional")) => conditional_rows(),
            (PATH_ORDERS_ALGO_PENDING, Some("move_order_stop")) => Vec::new(),
            other => panic!("nothing else is asked: {other:?}"),
        };
        Ok(json!({"code": "0", "msg": "", "data": data}))
    }

    fn public(&self, _: &str, path: &str, _: &[(&str, String)]) -> Result<Value, VenueApiError> {
        panic!("no instrument is read for an operator's read: {path}")
    }
}

fn client(venue: Venue) -> ReconcileClientOkx<Venue> {
    ReconcileClientOkx::new(crate::test_support::signer(), venue, "https://okx", "", 1.0)
}

/// **Every instrument, its stops and take-profits included, no `instId` asked**: the plain list (one page of 100),
/// then the paged algo lists of `conditional` and `move_order_stop`. Each row names its KIND, a stop's or a
/// take-profit's TRIGGER (no PRICE when it fires at market), OUR ID (the `algoClOrdId`, a fired stop's child's too)
/// and a size in the venue's own unit.
#[test]
fn the_read_lists_every_resting_order_and_stop_of_the_account() {
    crate::instrument::kept_list_for_tests();
    let venue = Venue::default();
    let VenueRead::OpenOrders { orders, truncated: false } =
        read_open_orders(&client(venue.clone())).unwrap()
    else {
        panic!("an open-orders answer, not cut")
    };
    assert_eq!(
        *venue.asked.borrow(),
        [
            (PATH_ORDERS_PENDING.to_string(), vec![("limit".to_string(), json!("100"))]),
            (
                PATH_ORDERS_ALGO_PENDING.to_string(),
                vec![
                    ("ordType".to_string(), json!("conditional")),
                    ("limit".to_string(), json!("100"))
                ]
            ),
            (
                PATH_ORDERS_ALGO_PENDING.to_string(),
                vec![
                    ("ordType".to_string(), json!("move_order_stop")),
                    ("limit".to_string(), json!("100"))
                ]
            ),
        ],
        "no instId, no instType"
    );
    let view: Vec<_> = orders
        .iter()
        .map(|o| {
            (
                o.symbol.as_str(),
                o.order_type.as_str(),
                o.side,
                o.price,
                o.trigger_price,
                o.order_client_id.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        view,
        [
            // each row under its USER name (decision 0290): `ETH-USDT-SWAP` is `ETHUSDT.P`, `BTC-USDT` `BTCUSDT`
            ("ETHUSDT.P", "limit", 1, Some(2100.5), None, Some("b7okxlim")),
            ("BTCUSDT.P", "market", -1, None, None, Some("b7okxtsb")),
            ("BTCUSDT.P", "stop", -1, None, Some(80211.0), Some("b7okxtsb")),
            ("BTCUSDT", "take_profit", 1, None, Some(10000.0), None),
        ]
    );
    let limit = &orders[0];
    assert_eq!((limit.qty, limit.filled_qty), (3.0, 1.0), "contracts, as the venue lists them");
    assert_eq!(
        (limit.status.as_str(), limit.timestamp_ms),
        ("PARTIALLY_FILLED", 1_792_866_100_000)
    );
    assert_eq!(
        (orders[2].venue_order_id.as_str(), orders[2].status.as_str()),
        ("4038739027414659073", "ACCEPTED"),
        "a stop is listed by its algoId"
    );
}

/// A full plain page is said as cut (the read asks one page), and any list the venue refuses fails the read in its
/// words with the list named.
#[test]
fn a_full_page_is_cut_and_a_refused_list_fails_the_read() {
    for (list, named) in [
        ("pending", "orders-pending"),
        ("conditional", "orders-algo-pending (conditional)"),
        ("move_order_stop", "orders-algo-pending (move_order_stop)"),
    ] {
        let why = read_open_orders(&client(Venue { failing: Some(list), ..Venue::default() }))
            .unwrap_err();
        let venue = VenueApiError { code: 50011, msg: "Too Many Requests".to_string() };
        let refusal =
            OkxAccountRefusal::OpenOrdersUnread { list: named.to_string(), why: venue.to_string() };
        assert_eq!(why, refusal.to_string(), "{list}");
    }

    struct Full;
    impl RestTransportJson for Full {
        fn signed(
            &self,
            _: &str,
            path: &str,
            _: &str,
            _: &[(&str, Value)],
            _: &dyn SignerJson,
        ) -> Result<Value, VenueApiError> {
            let row = pending_rows().remove(0);
            let data = if path == PATH_ORDERS_PENDING { vec![row; 100] } else { Vec::new() };
            Ok(json!({"code": "0", "msg": "", "data": data}))
        }
        fn public(
            &self,
            _: &str,
            path: &str,
            _: &[(&str, String)],
        ) -> Result<Value, VenueApiError> {
            panic!("{path}")
        }
    }

    let full = ReconcileClientOkx::new(crate::test_support::signer(), Full, "https://okx", "", 1.0);
    let VenueRead::OpenOrders { orders, truncated } = read_open_orders(&full).unwrap() else {
        panic!("an open-orders answer")
    };
    assert_eq!((orders.len(), truncated), (100, true), "the venue may hold more than one page");
}

/// The balance and the positions are not offered (said before any client is built), and a missing address row is the
/// open-orders read's refusal, with nothing asked.
#[test]
fn the_other_reads_are_not_offered_and_a_missing_row_is_said() {
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: Some("p".to_string().into()),
    };
    for kind in [VenueReadKind::Balance, VenueReadKind::Positions] {
        let why = venue_read(&creds, Err("no address row".into()), kind).unwrap_err();
        assert_eq!(why, not_offered_words("okx", kind).into());
    }
    let why = venue_read(&creds, Err("no address row".into()), VenueReadKind::OpenOrders);
    assert_eq!(why.unwrap_err().text(), "no address row");
}
