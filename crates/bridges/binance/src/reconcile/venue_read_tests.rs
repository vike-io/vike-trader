//! The operator's open-orders read (`super`'s module doc), offline: what is asked of each book and how its rows read.
//! Every test here FAILS on the base, where `crate::adapter`'s `VenueAdapterBinance` kept the trait's default and
//! answered every read `not offered by binance`.

use std::cell::RefCell;
use std::rc::Rc;

use crate::family::signer::BinanceHmacSigner;
use serde_json::{Value, json};
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_model::{VenueRead, VenueReadKind, VenueReadOpenOrder, not_offered_words};
use vike_secrets::credentials::Credentials;

use super::{read_books, venue_read};
use crate::execution::rest::coin_m::RestTransportDapiPaths;
use crate::reconcile::{Book, ReconcileClientBinance};

fn creds() -> Credentials {
    Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: None,
    }
}

/// One request as the venue saw it: host, path, params.
type Asked = (String, String, Vec<(String, String)>);

/// Answers each list by its path and records every request; `failing` is a path the venue refuses.
#[derive(Clone, Default)]
struct Venue {
    asked: Rc<RefCell<Vec<Asked>>>,
    failing: Option<&'static str>,
}

impl RestTransport for Venue {
    fn signed(
        &self,
        base_url: &str,
        path: &str,
        _method: &str,
        params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<Value, VenueApiError> {
        let owned = params.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect();
        self.asked.borrow_mut().push((base_url.to_string(), path.to_string(), owned));
        if self.failing == Some(path) {
            return Err(VenueApiError {
                code: -2015,
                msg: "Invalid API-key, IP, or permissions for action.".into(),
            });
        }
        Ok(match path {
            "/api/v3/openOrders" => spot_rows(),
            "/fapi/v1/openOrders" => perp_rows(),
            "/fapi/v1/openAlgoOrders" => algo_rows(),
            "/dapi/v1/openOrders" => coin_m_rows(),
            other => panic!("nothing else is asked: {other}"),
        })
    }

    fn public(&self, _: &str, path: &str, _: &[(&str, String)]) -> Result<Value, VenueApiError> {
        panic!("the read makes no public call: {path}")
    }
}

/// The spot list: a plain `LIMIT` in the demo host's row shape, and the `STOP_LOSS_LIMIT` the demo host answered on
/// 2026-11-08 (`crate::reconcile`'s `MEASURED`, verbatim but for the broker prefix on its id).
fn spot_rows() -> Value {
    json!([
        {"clientOrderId":"vtspotL1","cummulativeQuoteQty":"0.00000000","executedQty":"0.00100000","icebergQty":"0.00000000","isWorking":true,"orderId":14608754001_i64,"orderListId":-1,"origQty":"0.00450000","origQuoteOrderQty":"0.00000000","price":"2300.00000000","selfTradePreventionMode":"EXPIRE_MAKER","side":"BUY","status":"PARTIALLY_FILLED","stopPrice":"0.00000000","symbol":"ETHUSDT","time":1794122400000_i64,"timeInForce":"GTC","type":"LIMIT","updateTime":1794122401000_i64,"workingTime":1794122400000_i64},
        {"clientOrderId":"x-LINK-vtsslL22402550","cummulativeQuoteQty":"0.00000000","executedQty":"0.00000000","icebergQty":"0.00000000","isWorking":false,"orderId":14608754892_i64,"orderListId":-1,"origQty":"0.00450000","origQuoteOrderQty":"0.00000000","price":"2504.64000000","selfTradePreventionMode":"EXPIRE_MAKER","side":"BUY","status":"NEW","stopPrice":"2459.92000000","symbol":"ETHUSDT","time":1794122402685_i64,"timeInForce":"GTC","type":"STOP_LOSS_LIMIT","updateTime":1794122402685_i64,"workingTime":-1}
    ])
}

/// The USDⓈ-M list: a resting `LIMIT` on another symbol than any engine's (the documented row shape).
fn perp_rows() -> Value {
    json!([{"symbol":"SOLUSDT","orderId":8389765590_i64,"clientOrderId":"x-LINK-vtperpL7","price":"120.50","avgPrice":"0.00","origQty":"3","executedQty":"0","cumQuote":"0","status":"NEW","timeInForce":"GTC","type":"LIMIT","reduceOnly":false,"closePosition":false,"side":"SELL","positionSide":"BOTH","stopPrice":"0","workingType":"CONTRACT_PRICE","priceProtect":false,"origType":"LIMIT","updateTime":1794122403000_i64,"time":1794122403000_i64}])
}

/// The Algo Service's list: a resting `STOP_MARKET` (the page's `Current All Algo Open Orders` row shape), which
/// `openOrders` does not list (measured on the demo host 2026-10-19).
fn algo_rows() -> Value {
    json!([{"algoId":2146760,"clientAlgoId":"x-LINK-vtstopS9","algoType":"CONDITIONAL","orderType":"STOP_MARKET","symbol":"BTCUSDT","side":"SELL","positionSide":"BOTH","timeInForce":"GTC","quantity":"0.002","algoStatus":"NEW","actualOrderId":"","actualPrice":"0.00000","triggerPrice":"90000.00","price":"0.00","reduceOnly":true,"workingType":"MARK_PRICE","createTime":1794122404000_i64,"updateTime":1794122404000_i64,"triggerTime":0}])
}

/// The COIN-M list: one contract resting on `BTCUSD_PERP` (the row shape `/dapi/v1/openOrders` documents).
fn coin_m_rows() -> Value {
    json!([{"symbol":"BTCUSD_PERP","pair":"BTCUSD","orderId":4608218148_i64,"clientOrderId":"","price":"80000.0","avgPrice":"0.0","origQty":"1","executedQty":"0","cumBase":"0","status":"NEW","timeInForce":"GTC","type":"LIMIT","reduceOnly":false,"side":"BUY","positionSide":"BOTH","stopPrice":"0","updateTime":1794122405000_i64,"time":1794122405000_i64}])
}

/// The three books' clients over one recorded venue, as `super::venue_read` builds them (`BOOKS`): no symbol.
fn books(
    venue: &Venue,
) -> [(Book, ReconcileClientBinance<BinanceHmacSigner, RestTransportDapiPaths<Venue>>); 3] {
    let signer = || BinanceHmacSigner::new(&creds(), || 0);
    let on = |margining| RestTransportDapiPaths(venue.clone(), margining);
    use vike_model::Margining::{Inverse, Linear};
    // The account's orders carry the broker link id `LINK` (`x-LINK-<order client id>`), as the start would tell it.
    let link = || Some("LINK".to_string());
    [
        (
            Book::Spot,
            ReconcileClientBinance::spot(signer(), on(Linear), "https://spot", "")
                .with_link_id(link()),
        ),
        (
            Book::UsdM,
            ReconcileClientBinance::perp(signer(), on(Linear), "https://fapi", "")
                .with_link_id(link()),
        ),
        (
            Book::CoinM,
            ReconcileClientBinance::perp(signer(), on(Inverse), "https://dapi", "")
                .with_link_id(link()),
        ),
    ]
}

/// The book a reconcile client of an engine symbol reads (`crate::reconcile`'s `reader`, the venue read's books
/// without a symbol): a `.P` series is a futures book, a COIN-M name the inverse one, anything else spot; the exchange
/// symbol drops the lane marker.
#[test]
fn an_engine_symbol_routes_to_its_book() {
    assert_eq!(Book::of("BTCUSDT"), (Book::Spot, "BTCUSDT"));
    assert_eq!(Book::of("BTCUSDT.P"), (Book::UsdM, "BTCUSDT"));
    assert_eq!(Book::of("BTCUSDT_261225.P"), (Book::UsdM, "BTCUSDT_261225"));
    assert_eq!(Book::of("BTCUSD_PERP.P"), (Book::CoinM, "BTCUSD_PERP"));
    assert_eq!(Book::of("BTCUSD_261225.P"), (Book::CoinM, "BTCUSD_261225"));
}

fn rows(read: VenueRead) -> Vec<VenueReadOpenOrder> {
    match read {
        VenueRead::OpenOrders { orders, truncated: false } => orders,
        other => panic!("an open-orders answer, not cut: {other:?}"),
    }
}

/// **Every book, no symbol asked**: four lists (spot, USDⓈ-M, its Algo Service, COIN-M), each on its book's host and
/// none naming a symbol, so an order resting on a symbol no engine trades is listed too; dapi's algo list is answered
/// empty without a call. Each row names its book, its KIND, a stop's TRIGGER (not under PRICE unless it rests a limit)
/// and OUR ID with the broker prefix stripped.
#[test]
fn the_read_lists_every_book_of_the_account() {
    crate::instrument::names::kept_list_for_tests();
    let venue = Venue::default();
    let got = rows(read_books(&books(&venue)).unwrap());

    let asked: Vec<(String, String)> =
        venue.asked.borrow().iter().map(|(host, path, _)| (host.clone(), path.clone())).collect();
    let want = [
        ("https://spot", "/api/v3/openOrders"),
        ("https://fapi", "/fapi/v1/openOrders"),
        ("https://fapi", "/fapi/v1/openAlgoOrders"),
        ("https://dapi", "/dapi/v1/openOrders"),
    ];
    assert_eq!(asked, want.map(|(h, p)| (h.to_string(), p.to_string())));
    assert!(
        venue.asked.borrow().iter().all(|(_, _, params)| params.is_empty()),
        "no symbol, no filter"
    );

    let view: Vec<_> = got
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
            ("ETHUSDT", "limit", 1, Some(2300.0), None, Some("vtspotL1")),
            ("ETHUSDT", "stop_limit", 1, Some(2504.64), Some(2459.92), Some("vtsslL22402550")),
            ("SOLUSDT.P", "limit", -1, Some(120.5), None, Some("vtperpL7")),
            ("BTCUSDT.P", "stop", -1, None, Some(90000.0), Some("vtstopS9")),
            // the COIN-M row under its USER name (decision 0290): the lane's `BTCUSD_PERP.P` is `BTCUSD.P`
            ("BTCUSD.P", "limit", 1, Some(80000.0), None, None),
        ]
    );
    let first = &got[0];
    assert_eq!(first.venue_order_id, "14608754001");
    assert_eq!((first.qty, first.filled_qty), (0.0045, 0.001));
    assert_eq!(
        (first.status.as_str(), first.timestamp_ms),
        ("PARTIALLY_FILLED", 1_794_122_401_000)
    );
    assert_eq!((got[3].venue_order_id.as_str(), got[3].status.as_str()), ("2146760", "ACCEPTED"));
    assert_eq!(got[4].order_client_id, None, "an order placed elsewhere names no id of ours");
}

/// **Any list that fails fails the read**, in the venue's words with the list named: a read that lacked the stops
/// (or a whole book) must not look like the account's every order.
#[test]
fn a_failed_list_fails_the_read_in_words() {
    for (path, named) in [
        ("/api/v3/openOrders", "binance spot openOrders"),
        ("/fapi/v1/openAlgoOrders", "binance USDⓈ-M openAlgoOrders"),
        ("/dapi/v1/openOrders", "binance COIN-M openOrders"),
    ] {
        let venue = Venue { failing: Some(path), ..Venue::default() };
        let why = read_books(&books(&venue)).unwrap_err();
        assert!(why.contains(named) && why.contains("-2015"), "{path}: {why}");
    }
}

/// The balance and the positions are not offered (said before any client is built), and a missing address row is the
/// open-orders read's refusal, with nothing asked.
#[test]
fn the_other_reads_are_not_offered_and_a_missing_row_is_said() {
    for kind in [VenueReadKind::Balance, VenueReadKind::Positions] {
        let why = venue_read(&creds(), Err("no address row".into()), kind, None).unwrap_err();
        assert_eq!(why, not_offered_words("binance", kind).into());
    }
    let why = venue_read(&creds(), Err("no address row".into()), VenueReadKind::OpenOrders, None);
    assert_eq!(why.unwrap_err().text(), "no address row");
}
