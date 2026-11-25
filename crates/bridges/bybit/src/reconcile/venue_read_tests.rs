//! The operator's balance and open-orders reads (`super`'s module doc), offline: what is asked of the venue, how each
//! coin of the unified wallet (and of a UTA 1.0 account's `CONTRACT` wallet) reads and how each book's orders read.
//! Every test here FAILS on the base, where `crate::adapter`'s `VenueAdapterBybit` kept the trait's default and
//! answered every read `not offered by bybit`; the UTA 1.0 tests failed on the read before (it asked the unified
//! wallet alone), and the open-orders tests on the read before that (it offered the balance alone).

use std::cell::RefCell;

use crate::signer::BybitV5Signer;
use serde_json::{Value, json};
use vike_bridge_core::transport::VenueApiError;
use vike_model::{VenueReadAssetBalance, VenueReadKind};
use vike_secrets::credentials::Credentials;

use super::{balance_from, read_balance, read_open_orders, venue_read};
use crate::account::leverage::PATH_ACCOUNT_INFO;
use crate::execution::rest::{PATH_ACCOUNT, PATH_ORDER_REALTIME};
use crate::reconcile::ReconcileClientBybit;
use crate::refusals::BybitAccountRefusal;
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::json::RestTransportJson;

fn creds() -> Credentials {
    Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: None,
    }
}

/// One request as the venue saw it: method, path, params.
type Asked = (String, String, Vec<(String, Value)>);

/// Records each request and answers `account/info`, the unified wallet and the `CONTRACT` wallet, each an envelope.
struct Venue {
    asked: RefCell<Vec<Asked>>,
    info: Value,
    unified: Value,
    contract: Value,
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
        let owned = params.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect();
        self.asked.borrow_mut().push((method.to_string(), path.to_string(), owned));
        let account_type = params.iter().find(|(k, _)| *k == "accountType").map(|(_, v)| v);
        Ok(match (path, account_type) {
            (PATH_ACCOUNT_INFO, _) => self.info.clone(),
            (PATH_ACCOUNT, Some(t)) if t == "CONTRACT" => self.contract.clone(),
            _ => self.unified.clone(),
        })
    }
}

fn ok(result: Value) -> Value {
    json!({"retCode": 0, "retMsg": "OK", "result": result})
}

/// `account/info` stating `unifiedMarginStatus` `status`.
fn info(status: i64) -> Value {
    ok(json!({"marginMode": "REGULAR_MARGIN", "unifiedMarginStatus": status}))
}

/// The unified wallet of the CI box's bybit demo account (rows as the demo host answered them, 2026-11-09).
fn unified() -> Value {
    ok(json!({"list": [{
        "accountType": "UNIFIED", "totalEquity": "124855.1",
        "coin": [
            {"coin": "USDT", "walletBalance": "124095.10775833", "equity": "124095.10775833"},
            {"coin": "USDC", "walletBalance": "0", "equity": "0"},
            {"coin": "BTC", "walletBalance": "0.0099904", "equity": "0.0099904"},
        ]
    }]}))
}

/// What the demo host answered `accountType=CONTRACT` on an account stating `3` (2026-11-09).
fn only_unified() -> Value {
    json!({"retCode": 10001, "retMsg": "accountType only support UNIFIED.", "result": {}})
}

fn venue(info: Value, unified: Value, contract: Value) -> Venue {
    Venue { asked: RefCell::default(), info, unified, contract }
}

fn read(
    wire: &Venue,
) -> Result<vike_model::VenueReadBalance, vike_utility::errors::CarriedRefusal> {
    let demo = crate::execution::rest::seed_endpoints(vike_model::AccountTier::Demo).rest;
    read_balance(wire, &BybitV5Signer::new(&creds(), || 0), &demo)
}

/// The `accountType` of every wallet request, in order.
fn wallets_asked(wire: &Venue) -> Vec<Value> {
    wire.asked
        .borrow()
        .iter()
        .filter(|(_, path, _)| path == PATH_ACCOUNT)
        .map(|(_, _, params)| {
            assert!(params.iter().all(|(k, _)| k != "coin"), "no coin filter: {params:?}");
            params.iter().find(|(k, _)| k == "accountType").map(|(_, v)| v.clone()).unwrap()
        })
        .collect()
}

/// The coin row the demo host answered on 2026-11-08 (`tests/fixtures/classes/`): its `walletBalance` is the figure.
#[test]
fn the_captured_demo_coin_row_reads_as_its_wallet_balance() {
    let captured: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/classes/wallet_balance_btc_coin_scoped.json"
    ))
    .expect("the capture is JSON");
    let got = balance_from(&captured["result"]).unwrap();
    assert_eq!((got.asset.as_str(), got.balance), ("BTC", 0.0000004));
    assert!(got.other_assets.is_empty() && got.allowances.is_empty());
}

/// A UTA 2.0 account: ONE read of the whole unified wallet after `account/info` (no `coin`, so every coin the account
/// holds is listed), and an account that holds dollars and the settle coin of an inverse engine answers both: the coin
/// is not dropped behind the first figure, and an empty coin is left out. No `CONTRACT` read (not supported there).
#[test]
fn a_uta2_account_reads_the_unified_wallet_alone() {
    for status in [5, 6] {
        let wire = venue(info(status), unified(), json!(null));
        let got = read(&wire).unwrap();
        assert_eq!((got.asset.as_str(), got.balance), ("USDT", 124_095.107_758_33));
        assert_eq!(
            got.other_assets,
            [VenueReadAssetBalance { asset: "BTC".into(), balance: 0.0099904 }]
        );
        assert_eq!(wallets_asked(&wire), [json!("UNIFIED")], "status {status}");
        assert!(wire.asked.borrow().iter().all(|(method, _, _)| method == "GET"), "reads only");
    }
}

/// **A UTA 1.0 account lists its `CONTRACT` wallet's coins too, named as that wallet's** (the documented shape: the
/// unified one's), so the BTC of the inverse book is never read as the unified BTC beside it.
///
/// Base: the `CONTRACT` wallet was never asked; the account's 0.5 BTC of inverse money did not appear.
#[test]
fn a_uta1_account_lists_its_contract_wallet_labelled() {
    for status in [1, 3, 4] {
        let contract = ok(json!({"list": [{"accountType": "CONTRACT", "coin": [
            {"coin": "BTC", "walletBalance": "0.5", "equity": "0.51"},
            {"coin": "ETH", "walletBalance": "0", "equity": "0"},
        ]}]}));
        let wire = venue(info(status), unified(), contract);
        let got = read(&wire).unwrap();
        assert_eq!((got.asset.as_str(), got.balance), ("USDT", 124_095.107_758_33));
        assert_eq!(
            got.other_assets,
            [
                VenueReadAssetBalance { asset: "BTC".into(), balance: 0.0099904 },
                VenueReadAssetBalance { asset: "BTC (CONTRACT wallet)".into(), balance: 0.5 },
            ],
            "status {status}"
        );
        assert_eq!(wallets_asked(&wire), [json!("UNIFIED"), json!("CONTRACT")]);
    }
}

/// The demo host's account states `3` and answers `CONTRACT` with "only support UNIFIED" (measured 2026-11-09): the
/// account has the unified wallet alone, which is listed as before.
#[test]
fn a_uta1_account_the_venue_keeps_unified_lists_the_unified_wallet() {
    let wire = venue(info(3), unified(), only_unified());
    let got = read(&wire).unwrap();
    assert_eq!(
        got.other_assets,
        [VenueReadAssetBalance { asset: "BTC".into(), balance: 0.0099904 }]
    );
    assert_eq!(wallets_asked(&wire), [json!("UNIFIED"), json!("CONTRACT")]);
}

/// **No guess**: an account version this bridge does not know, an absent one, and a `CONTRACT` read refused for any
/// other reason fail the read in words, rather than show a list that may lack the inverse wallet.
///
/// Base: the read answered the unified wallet whatever the account's version.
#[test]
fn an_unknown_version_or_a_refused_contract_read_fails_the_read() {
    for (answer, status) in
        [(info(2), "2"), (info(7), "7"), (ok(json!({"marginMode": "REGULAR_MARGIN"})), "absent")]
    {
        let why = read(&venue(answer, unified(), json!(null))).unwrap_err();
        let refusal = BybitAccountRefusal::UnknownAccountVersion { status: status.into() };
        assert_eq!(why, refusal.carried());
    }
    let refused = json!({"retCode": 10002, "retMsg": "request expired", "result": {}});
    let why = read(&venue(info(3), unified(), refused)).unwrap_err();
    assert_eq!(
        why.text(),
        "bybit GET /v5/account/wallet-balance (accountType CONTRACT): venue error 10002, message \"request expired\""
    );
}

/// The venue's refusal is the operator's sentence with its code; a row with no figure is said, never read as zero.
#[test]
fn a_refusal_and_an_unreadable_row_are_said() {
    let refusal = json!({"retCode": 10003, "retMsg": "API key is invalid.", "result": {}});
    let refused = venue(refusal.clone(), refusal.clone(), refusal);
    let why = read_balance(&refused, &BybitV5Signer::new(&creds(), || 0), "x").unwrap_err();
    assert_eq!(
        why.text(),
        "bybit GET /v5/account/info: venue error 10003, message \"API key is invalid.\""
    );

    let why = balance_from(&json!({"list": [{"coin": [{"coin": "BTC"}]}]})).unwrap_err();
    assert_eq!(why, BybitAccountRefusal::WalletNoBalance { asset: "BTC".into() });
    let blank = json!({"list": [{"coin": [{"coin": "ETH", "walletBalance": ""}]}]});
    assert_eq!(
        balance_from(&blank).unwrap_err(),
        BybitAccountRefusal::WalletNoBalance { asset: "ETH".into() },
        "an empty string says nothing too"
    );
    assert_eq!(
        balance_from(&json!({})).unwrap_err(),
        BybitAccountRefusal::WalletNoList,
        "no `list` is no answer"
    );
}

/// The venue lists no coin for an account with nothing in it: that is an answer (zero), not an error. With money
/// nowhere, the first row the venue lists is shown at zero.
#[test]
fn an_account_with_no_money_answers_zero() {
    let none = balance_from(&json!({"list": [{"accountType": "UNIFIED", "coin": []}]})).unwrap();
    assert_eq!((none.asset.as_str(), none.balance), ("", 0.0));
    let zero = balance_from(&json!({"list": [{"coin": [{"coin": "USDT", "walletBalance": "0"}]}]}))
        .unwrap();
    assert_eq!((zero.asset.as_str(), zero.balance, zero.other_assets.len()), ("USDT", 0.0, 0));
}

/// The positions are not offered (the venue lists them per category and coin): said in the shared words, before any
/// client is built. A missing address row is the open-orders read's refusal, with nothing asked.
///
/// Base: the open orders were not offered either.
#[test]
fn positions_are_not_offered_and_open_orders_need_the_address_row() {
    let why = venue_read(&creds(), Err("no address row".into()), VenueReadKind::Positions);
    assert_eq!(
        why.unwrap_err(),
        vike_model::not_offered_words("bybit", VenueReadKind::Positions).into()
    );
    let why = venue_read(&creds(), Err("no address row".into()), VenueReadKind::OpenOrders);
    assert_eq!(why.unwrap_err().text(), "no address row");
}

/// Answers `order/realtime` by book with the demo host's captures (`tests/fixtures/classes/`) and records each
/// request's params; `failing` is a category the venue refuses.
#[derive(Default)]
struct Books {
    asked: RefCell<Vec<Vec<(String, Value)>>>,
    failing: Option<&'static str>,
}

impl RestTransportJson for Books {
    fn signed(
        &self,
        _base_url: &str,
        path: &str,
        _method: &str,
        params: &[(&str, Value)],
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        assert_eq!(path, PATH_ORDER_REALTIME, "the open orders are this list alone");
        self.asked
            .borrow_mut()
            .push(params.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect());
        let word = |key: &str| params.iter().find(|(k, _)| *k == key).and_then(|(_, v)| v.as_str());
        let capture =
            |text: &str| serde_json::from_str::<Value>(text).expect("the capture is JSON");
        if word("category") == self.failing {
            return Ok(json!({"retCode": 10001, "retMsg": "params error", "result": {}}));
        }
        Ok(match (word("category"), word("settleCoin")) {
            (Some("linear"), Some("USDT")) => capture(include_str!(
                "../../tests/fixtures/classes/order_realtime_dated_far_stop.json"
            )),
            (Some("linear"), Some("USDC")) => ok(json!({"category": "linear", "list": []})),
            (Some("inverse"), None) => capture(include_str!(
                "../../tests/fixtures/classes/order_realtime_inverse_far_limit.json"
            )),
            (Some("spot"), None) => capture(include_str!(
                "../../tests/fixtures/classes/order_realtime_spot_filled_buy.json"
            )),
            (Some("option"), None) => {
                capture(include_str!("../../tests/fixtures/classes/order_realtime_option_far.json"))
            }
            other => panic!("no such book is asked: {other:?}"),
        })
    }
}

fn read_books(wire: Books) -> Result<vike_model::VenueRead, String> {
    let demo = crate::execution::rest::seed_endpoints(vike_model::AccountTier::Demo).rest;
    read_open_orders(&ReconcileClientBybit::new(BybitV5Signer::new(&creds(), || 0), wire, demo, ""))
}

/// **Every book of the account, no symbol asked**: one `order/realtime` walk per book (`linear` per settle coin, then
/// `inverse`, `spot` and `option` on the category alone), no `orderFilter`, so a resting conditional is listed beside
/// the plain orders. Each row (the demo host's own) names its KIND, a stop's TRIGGER (no PRICE: it fires as a market
/// order), OUR ID (the `orderLinkId`) and its instrument by its USER name (decision 0290: the venue's
/// `ETHUSDT-06NOV26` is `ETHUSDT_261106`, the inverse `BTCUSD` `BTCUSD.P`, the spot `DOGEUSDT` the pair `DOGEUSDT`; an
/// option, which no book this bridge signs holds, keeps the venue's name). A short page ends its walk although the
/// venue hands a cursor on it.
#[test]
fn the_open_orders_read_lists_every_book_of_the_account() {
    crate::history::kept_list_for_tests();
    let vike_model::VenueRead::OpenOrders { orders, truncated: false } =
        read_books(Books::default()).unwrap()
    else {
        panic!("an open-orders answer, not cut")
    };
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
            ("ETHUSDT_261106", "stop", -1, None, Some(1345.95), Some("bbcls-6-390091825")),
            ("BTCUSD.P", "limit", 1, Some(45471.5), None, Some("vtinv41668269")),
            ("DOGEUSDT", "market", 1, None, None, Some("bbcls-3-390082193")),
            ("BTC-31OCT26-75500-C-USDT", "limit", 1, Some(5.0), None, Some("bbcls-15-390103881")),
        ]
    );
    let stop = &orders[0];
    assert_eq!(stop.venue_order_id, "7e858862-9618-47ca-930b-d71b00fee604");
    assert_eq!(
        (stop.status.as_str(), stop.qty, stop.timestamp_ms),
        ("ACCEPTED", 0.01, 1_793_390_091_915)
    );
    assert_eq!(
        (orders[1].qty, orders[1].filled_qty),
        (8.0, 0.0),
        "an inverse size is the venue's (dollars)"
    );
}

/// The requests the read makes: one page per book (every capture is a short page), each naming its category (a
/// linear one its settle coin), the page size and nothing else: no symbol, no `orderFilter`.
#[test]
fn the_open_orders_read_asks_each_book_once_by_category() {
    let wire = Books::default();
    let demo = crate::execution::rest::seed_endpoints(vike_model::AccountTier::Demo).rest;
    let client = ReconcileClientBybit::new(BybitV5Signer::new(&creds(), || 0), wire, demo, "");
    read_open_orders(&client).unwrap();
    let asked = client.transport.asked.borrow().clone();
    let scopes: Vec<Vec<(String, Value)>> = [
        vec![("category", json!("linear")), ("settleCoin", json!("USDT"))],
        vec![("category", json!("linear")), ("settleCoin", json!("USDC"))],
        vec![("category", json!("inverse"))],
        vec![("category", json!("spot"))],
        vec![("category", json!("option"))],
    ]
    .into_iter()
    .map(|scope| {
        let mut want: Vec<(String, Value)> =
            scope.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        want.push(("limit".to_string(), json!(50)));
        want
    })
    .collect();
    assert_eq!(asked, scopes);
}

/// **A book the venue refuses fails the read**, in its words with the book named: a list that lacked a book must not
/// look like the account's every order.
#[test]
fn a_refused_book_fails_the_open_orders_read_in_words() {
    for category in ["linear", "inverse", "spot", "option"] {
        let why = read_books(Books { failing: Some(category), ..Books::default() }).unwrap_err();
        assert!(
            why.starts_with(&format!("bybit GET /v5/order/realtime (category {category}"))
                && why.ends_with("limit 50): venue error 10001, message \"params error\""),
            "{why}"
        );
    }
}
