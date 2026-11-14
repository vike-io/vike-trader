//! Offline pins for `crate::execution::rest::coin_m` (NO network), over the demo host's committed `dapi` list
//! (`tests/fixtures/exchange_info/dapi_demo_classes_2026-10-30.json`) and a scripted venue that panics on a
//! request its script does not name.
//!
//! | test | what it pins |
//! |---|---|
//! | [`a_coin_m_mount_is_its_rows_inverse_face_and_settle_coin`] | `mount_of`: `Inverse(contractSize)`, the coin, the whole-contract grid, no `contract_size` |
//! | [`a_coin_m_name_whose_row_cannot_value_it_is_refused`] | absent, not TRADING, no face, no coin: refused, never guessed |
//! | [`a_usd_m_host_is_the_inner_transport_request_for_request`] | linear byte-identical: nothing is rewritten for a USDⓈ-M client |
//! | [`the_book_not_the_host_decides_the_rewrite`] | PLAN trap 1: a COIN-M client on any host is rewritten, a USDⓈ-M one on the dapi host is not |
//! | [`a_coin_m_order_goes_to_dapi_in_whole_contracts`] | the order, cancel and status words on `/dapi/v1/order` |
//! | [`the_reads_of_the_usd_m_code_reach_their_dapi_twins`] | positionRisk by pair, one symbol's rows; balance, commission, leverage bracket |
//! | [`dapi_has_no_algo_service_and_says_so_without_a_call`] | algo lists empty, lookups "no such order", a stop refused, no request |
//! | [`what_a_coin_m_client_does_not_build_is_refused_by_name`] | stops and GTD, on the dapi host only |
//! | [`the_coin_wallet_is_the_settle_coins_row`] | `parse_coin_balance` |
//! | [`coin_m_funding_pages_past_a_thousand_rows_and_moves_its_window`] | `crate::execution::funding::income_poller` on a dapi client: `/dapi/v1/income`, every row past one page, then the next window, each once |

use crate::family::signer::BinanceHmacSigner;
use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_model::events::Event;
use vike_secrets::credentials::Credentials;

use super::*;
use crate::account::modes::modes_tests::{Answer, Venue};
use crate::execution::BinanceUrls;
use crate::execution::rest::perp::VenueRestBinancePerp;
use vike_model::AccountTier;
use vike_venues::venue_urls::UrlType;

fn dapi_list() -> Value {
    serde_json::from_str(include_str!(
        "../../../tests/fixtures/exchange_info/dapi_demo_classes_2026-10-30.json"
    ))
    .expect("the committed dapi list parses")
}

/// The addresses the release's seed gives an account at `tier`.
fn seeded(tier: AccountTier) -> BinanceUrls {
    BinanceUrls::resolve(&vike_venues::venue_urls::VenueUrls::seed("binance"), tier)
        .expect("the seed")
}

/// A DEMO futures client of `symbol` over [`RestTransportDapiPaths`] of the scripted `venue` (fake keys, a frozen clock), on
/// the BTCUSD_PERP grid: of the COIN-M book on the demo `dapi` host when `Inverse`, else of USDⓈ-M on `fapi`.
fn client(
    venue: Venue,
    margining: Margining,
    symbol: &str,
) -> VenueRestBinancePerp<BinanceHmacSigner, RestTransportDapiPaths<Venue>> {
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: None,
    };
    let demo = seeded(AccountTier::Demo);
    VenueRestBinancePerp {
        signer: BinanceHmacSigner::new(&creds, || 0),
        transport: RestTransportDapiPaths(venue, margining),
        base_url: if margining == Margining::Inverse { demo.dapi_rest } else { demo.fapi_rest }
            .to_string(),
        ty: demo.ty,
        margining,
        symbol: symbol.to_string(),
        properties: mount_of("BTCUSD_PERP.P", &dapi_list()).expect("the BTC row").properties,
        leverage: 2.0,
        link_id: None,
        algo_order_client_ids: Default::default(),
    }
}

fn order(kind: &str, tif: &str) -> OrderRequest {
    serde_json::from_value(json!({
        "order_client_id": "c1", "venue": "binance", "symbol": "BTCUSD_PERP.P", "side": 1, "qty": 2.0,
        "order_type": kind, "price": 60000.0, "trigger_price": 59000.0,
        "time_in_force": tif.to_ascii_uppercase(),
        "gtd_expiry": vike_model::now_ms() + 3_600_000,
    }))
    .expect("an order")
}

fn ok(body: Value) -> Answer {
    Ok(body)
}

/// `[(k, v), ..]` as the owned pairs `Venue::params_of` answers.
fn pairs(kv: &[(&str, &str)]) -> Vec<(String, String)> {
    kv.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

#[test]
fn a_coin_m_mount_is_its_rows_inverse_face_and_settle_coin() {
    let list = dapi_list();
    let btc = mount_of("BTCUSD_PERP.P", &list).expect("BTCUSD_PERP is TRADING on the demo list");
    assert_eq!(btc.value, ContractValue { kind: NotionalKind::Inverse, per_unit: 100.0 });
    assert_eq!(btc.coin, "BTC");
    let g = btc.properties;
    assert_eq!((g.tick_size, g.step_size, g.min_qty, g.max_qty), (0.1, 1.0, 1.0, 30_000.0));
    assert_eq!(
        g.contract_size, 0.0,
        "the face is the contract value's, never a multiplier on the grid"
    );
    // The lane suffix and the case are the engine's; the row is the venue's.
    assert_eq!(mount_of("btcusd_perp.p", &list).map(|m| m.value), Ok(btc.value));
    let eth = mount_of("ETHUSD_PERP.P", &list).expect("ETHUSD_PERP");
    assert_eq!((eth.value.per_unit, eth.coin.as_str()), (10.0, "ETH"), "10 US dollars a contract");
    let quarter = mount_of("BTCUSD_261225.P", &list).expect("the quarterly");
    assert_eq!((quarter.value.kind, quarter.value.per_unit), (NotionalKind::Inverse, 100.0));
    // Every committed row's coin is the catalog's name rule, the one the node's door reads.
    for symbol in ["BTCUSD_PERP.P", "ETHUSD_PERP.P", "BTCUSD_261225.P", "BTCUSD_270326.P"] {
        let mount = mount_of(symbol, &list).expect(symbol);
        assert_eq!(
            vike_catalog::inverse_settle_coin("binance", symbol),
            Some(mount.coin),
            "{symbol}"
        );
        assert!(names_coin_m(symbol) && is_coin_m(symbol), "{symbol}");
    }
    for linear in ["BTCUSDT.P", "BTCUSDT_261225.P", "BTCUSD", "BTCUSD_PERP"] {
        assert!(!names_coin_m(linear), "{linear}: not a COIN-M mount");
    }
}

#[test]
fn a_coin_m_name_whose_row_cannot_value_it_is_refused() {
    let list = dapi_list();
    // EGLDUSD_PERP is PENDING_TRADING and BTCUSD_260626 DELIVERING on the demo list; FOOUSD_PERP is not listed.
    for symbol in ["EGLDUSD_PERP.P", "BTCUSD_260626.P", "FOOUSD_PERP.P"] {
        let why = mount_of(symbol, &list).expect_err(symbol);
        assert!(why.contains("no TRADING row"), "{symbol}: {why}");
    }
    let mut no_face = list.clone();
    no_face["symbols"][0].as_object_mut().expect("row").remove("contractSize");
    assert!(mount_of("BTCUSD_PERP.P", &no_face).expect_err("no face").contains("contractSize"));
    let mut linear = list.clone();
    linear["symbols"][0]["marginAsset"] = "USD".into();
    linear["symbols"][0]["quoteAsset"] = "USD".into();
    let why = mount_of("BTCUSD_PERP.P", &linear).expect_err("not coin-margined");
    assert!(why.contains("not a coin-margined contract"), "{why}");
    assert!(mount_of("BTCUSD_PERP.P", &json!({})).is_err(), "an unreadable body values nothing");
}

/// A USDⓈ-M (or spot) client is the inner transport, request for request: the linear wire is byte-identical.
#[test]
fn a_usd_m_host_is_the_inner_transport_request_for_request() {
    let (demo, live) = (seeded(AccountTier::Demo), seeded(AccountTier::Live));
    for host in [demo.fapi_rest, live.fapi_rest, demo.spot_rest] {
        let venue = Venue::new()
            .on("POST", "/fapi/v1/order", ok(json!({"orderId": 7})))
            .on("GET", "/fapi/v2/positionRisk", ok(json!([{"symbol": "BTCUSDT"}, {"symbol": "X"}])))
            .on("GET", "/fapi/v1/openAlgoOrders", ok(json!([{"algoId": 1}])));
        let rest = client(venue, Margining::Linear, "BTCUSDT");
        let t = &rest.transport;
        let s = &rest.signer;
        let params = [("symbol", "BTCUSDT".to_string())];
        t.signed(host, "/fapi/v1/order", "POST", &params, s).expect("order");
        let rows = t.signed(host, "/fapi/v2/positionRisk", "GET", &params, s).expect("risk");
        assert_eq!(rows.as_array().map(Vec::len), Some(2), "{host}: no row is dropped");
        let algo = t.signed(host, "/fapi/v1/openAlgoOrders", "GET", &params, s).expect("algo");
        assert_eq!(algo, json!([{"algoId": 1}]), "{host}: the algo read is the venue's");
        assert_eq!(
            t.0.sent(),
            ["POST /fapi/v1/order", "GET /fapi/v2/positionRisk", "GET /fapi/v1/openAlgoOrders"]
        );
        assert_eq!(t.0.params_of("GET", "/fapi/v2/positionRisk"), pairs(&[("symbol", "BTCUSDT")]));
    }
}

/// **The client's BOOK decides the rewrite, never its host string** (the venue_url work's PLAN trap 1): a
/// COIN-M client whose row an operator moved to a host this crate never spelled is still carried to the `dapi`
/// paths, and a USDⓈ-M client handed the `dapi` host's address is still the inner transport.
#[test]
fn the_book_not_the_host_decides_the_rewrite() {
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: None,
    };
    let signer = BinanceHmacSigner::new(&creds, || 0);
    let params = [("symbol", "BTCUSD_PERP".to_string())];

    let coin_m = RestTransportDapiPaths(
        Venue::new().on("POST", "/dapi/v1/order", ok(json!({"orderId": 1}))),
        Margining::Inverse,
    );
    coin_m
        .signed("https://moved.example", "/fapi/v1/order", "POST", &params, &signer)
        .expect("sent");
    assert_eq!(coin_m.0.sent(), ["POST /dapi/v1/order"], "a COIN-M client on a moved host");

    let usd_m = RestTransportDapiPaths(
        Venue::new().on("POST", "/fapi/v1/order", ok(json!({"orderId": 2}))),
        Margining::Linear,
    );
    let dapi_host = seeded(AccountTier::Demo).dapi_rest;
    usd_m.signed(dapi_host, "/fapi/v1/order", "POST", &params, &signer).expect("sent");
    assert_eq!(usd_m.0.sent(), ["POST /fapi/v1/order"], "a USDⓈ-M client on the dapi address");
    assert_eq!(
        (time_path_for("BTCUSD_PERP"), time_path_for("BTCUSDT")),
        ("/dapi/v1/time", "/fapi/v1/time")
    );
}

#[test]
fn a_coin_m_order_goes_to_dapi_in_whole_contracts() {
    let venue = Venue::new()
        .on(
            "POST",
            "/dapi/v1/order",
            ok(json!({"orderId": 4_211, "status": "NEW", "clientOrderId": "c1"})),
        )
        .on("DELETE", "/dapi/v1/order", ok(json!({"orderId": 4_211, "status": "CANCELED"})));
    let rest = client(venue, Margining::Inverse, "BTCUSD_PERP");
    let events = rest.submit_order(&order("limit", "gtc"));
    assert!(
        events.iter().any(
            |e| matches!(e, Event::OrderAccepted(a) if a.venue_order_id.as_deref() == Some("4211"))
        ),
        "{events:?}"
    );
    let sent = rest.transport.0.params_of("POST", "/dapi/v1/order");
    let get = |k: &str| sent.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
    assert_eq!(get("symbol"), Some("BTCUSD_PERP"), "the exchange symbol");
    // Whole contracts on the row's step of 1, spelled as the shared step format spells an integral step
    // (`vike_bridge_core::format::format_to_step_f`, golden-pinned; fapi's step-1 symbols take the same spelling).
    assert_eq!(get("quantity"), Some("2.0"), "two whole contracts");
    assert_eq!((get("type"), get("timeInForce")), (Some("LIMIT"), Some("GTC")));
    assert_eq!(get("positionSide"), Some("BOTH"));
    assert!(rest.cancel_order("c1").is_ok());
    assert_eq!(rest.transport.0.sent(), ["POST /dapi/v1/order", "DELETE /dapi/v1/order"]);
}

#[test]
fn the_reads_of_the_usd_m_code_reach_their_dapi_twins() {
    let risk = json!([
        {"symbol": "BTCUSD_PERP", "positionAmt": "3", "entryPrice": "60000.0", "marginType": "cross", "positionSide": "BOTH"},
        {"symbol": "BTCUSD_261225", "positionAmt": "0", "entryPrice": "0.0", "marginType": "cross", "positionSide": "BOTH"},
    ]);
    let venue = Venue::new()
        .on("GET", "/dapi/v1/positionRisk", ok(risk))
        .on("GET", "/dapi/v1/balance", ok(json!([])))
        .on("GET", "/dapi/v1/commissionRate", ok(json!({})))
        .on("GET", "/dapi/v2/leverageBracket", ok(json!([])))
        .on("GET", "/dapi/v1/userTrades", ok(json!([])))
        .on("POST", "/dapi/v1/leverage", ok(json!({"leverage": 2})));
    let rest = client(venue, Margining::Inverse, "BTCUSD_PERP");
    let (t, s, h) = (&rest.transport, &rest.signer, rest.base_url.as_str());
    let symbol = [("symbol", "BTCUSD_PERP".to_string())];
    let rows = t
        .signed_requery(h, crate::execution::rest::perp::PATH_POSITIONS, "GET", &symbol, s)
        .expect("risk");
    assert_eq!(
        rows,
        json!([{"symbol": "BTCUSD_PERP", "positionAmt": "3", "entryPrice": "60000.0", "marginType": "cross", "positionSide": "BOTH"}])
    );
    assert_eq!(
        t.0.params_of("GET", "/dapi/v1/positionRisk"),
        pairs(&[("pair", "BTCUSD")]),
        "dapi's positionRisk filters by pair"
    );
    t.signed(h, crate::execution::rest::perp::PATH_BALANCE, "GET", &[], s).expect("balance");
    t.signed(h, "/fapi/v1/commissionRate", "GET", &symbol, s).expect("fee");
    t.signed(h, crate::account::leverage::PATH_LEVERAGE_BRACKET, "GET", &symbol, s)
        .expect("bracket");
    t.signed(h, "/fapi/v1/userTrades", "GET", &symbol, s).expect("fills");
    rest.set_leverage().expect("the start's one leverage write");
    assert_eq!(
        t.0.params_of("GET", "/dapi/v2/leverageBracket"),
        pairs(&[("symbol", "BTCUSD_PERP")]),
        "v2 is the per-symbol bracket read"
    );
}

#[test]
fn dapi_has_no_algo_service_and_says_so_without_a_call() {
    let rest = client(Venue::new(), Margining::Inverse, "BTCUSD_PERP");
    let (t, s, h) = (&rest.transport, &rest.signer, seeded(AccountTier::Live).dapi_rest);
    let symbol = [("symbol", "BTCUSD_PERP".to_string())];
    for path in [
        crate::execution::rest::algo::PATH_OPEN_ALGO_ORDERS,
        crate::execution::rest::algo::PATH_ALL_ALGO_ORDERS,
    ] {
        assert_eq!(
            t.signed(h, path, "GET", &symbol, s),
            Ok(json!([])),
            "{path}: nothing rests there"
        );
    }
    let lookup = t
        .signed(h, crate::execution::rest::algo::PATH_ALGO_ORDER, "GET", &symbol, s)
        .expect_err("no algo order");
    assert!(crate::execution::rest::algo::is_unknown_order(&lookup), "{lookup:?}");
    let cancel = t
        .signed(h, crate::execution::rest::algo::PATH_ALGO_ORDER, "DELETE", &symbol, s)
        .expect_err("none");
    assert!(crate::transport::error_codes::cancel_says_order_unknown(&cancel), "{cancel:?}");
    let submit = t
        .signed(h, crate::execution::rest::algo::PATH_ALGO_ORDER, "POST", &symbol, s)
        .expect_err("refused");
    assert!(
        !crate::transport::error_codes::submit_outcome_unknown(submit.code),
        "a definite refusal, not an unknown outcome"
    );
    // A cancel of an order the venue no longer has is the idempotent "gone", as on fapi: the regular endpoint's
    // -2011, then the (local) algo answer.
    let gone = Venue::new().on(
        "DELETE",
        "/dapi/v1/order",
        Err(VenueApiError { code: -2011, msg: "Unknown order sent.".into() }),
    );
    let rest = client(gone, Margining::Inverse, "BTCUSD_PERP");
    assert!(rest.cancel_order("c9").is_ok(), "already gone is not a failure");
    assert_eq!(rest.transport.0.sent(), ["DELETE /dapi/v1/order"], "the algo retry asked nobody");
    // The resync's history read: orders and fills from dapi, no algo request.
    let history = Venue::new().on("GET", "/dapi/v1/allOrders", ok(json!([]))).on(
        "GET",
        "/dapi/v1/userTrades",
        ok(json!([])),
    );
    let rest = client(history, Margining::Inverse, "BTCUSD_PERP");
    assert!(crate::execution::perp_history_events(&rest, "BTCUSD_PERP.P").is_empty());
    assert_eq!(rest.transport.0.sent(), ["GET /dapi/v1/allOrders", "GET /dapi/v1/userTrades"]);
}

#[test]
fn what_a_coin_m_client_does_not_build_is_refused_by_name() {
    let reason = |e: Option<Event>| match e {
        Some(Event::OrderRejected(r)) => r.reason.to_string(),
        other => panic!("not refused: {other:?}"),
    };
    for kind in ["stop", "stop_limit", "take_profit"] {
        let why = reason(deny_unbuilt(Margining::Inverse, &order(kind, "gtc")));
        assert!(why.contains("COIN-M") && why.contains("Algo Service"), "{kind}: {why}");
    }
    let why = reason(deny_unbuilt(Margining::Inverse, &order("limit", "gtd")));
    assert!(why.contains("good-till-date") && why.contains("GTX"), "{why}");
    assert!(deny_unbuilt(Margining::Inverse, &order("limit", "gtc")).is_none());
    assert!(deny_unbuilt(Margining::Inverse, &order("market", "ioc")).is_none());
    // Not this gate's on a USDⓈ-M client: fapi builds its stops on the Algo Service and has GTD.
    assert!(deny_unbuilt(Margining::Linear, &order("stop", "gtc")).is_none());
    assert!(deny_unbuilt(Margining::Linear, &order("limit", "gtd")).is_none());
    // Through the real submit path: refused after `OrderSubmitted`, nothing sent.
    let rest = client(Venue::new(), Margining::Inverse, "BTCUSD_PERP");
    let events = rest.submit_order(&order("stop", "gtc"));
    assert!(
        matches!(events.as_slice(), [Event::OrderSubmitted(_), Event::OrderRejected(_)]),
        "{events:?}"
    );
    let events = rest.submit_batch(&[order("limit", "gtd")]);
    assert!(
        matches!(events.as_slice(), [Event::OrderSubmitted(_), Event::OrderRejected(_)]),
        "{events:?}"
    );
    assert!(rest.transport.0.sent().is_empty());
}

/// **No binance name is quanto, and a client stating it is refused in words, never read as either book**: every
/// order (`deny_unbuilt`) and every request (`RestTransportDapiPaths`, nothing sent: the script names nothing).
#[test]
fn a_quanto_client_is_refused_in_words_and_sends_nothing() {
    let Some(Event::OrderRejected(r)) = deny_unbuilt(Margining::Quanto, &order("limit", "gtc"))
    else {
        panic!("a quanto order is refused");
    };
    assert_eq!(r.reason.to_string(), NO_QUANTO_BOOK);
    let quanto = RestTransportDapiPaths(Venue::new(), Margining::Quanto);
    let err = quanto.public("https://fapi.example", "/fapi/v1/time", &[]).expect_err("refused");
    assert_eq!((err.code, err.msg.as_str()), (E_NO_QUANTO_BOOK, NO_QUANTO_BOOK));
    assert!(quanto.0.sent().is_empty());
}

#[test]
fn the_coin_wallet_is_the_settle_coins_row() {
    // The documented `/dapi/v1/balance` shape: one row per asset.
    let body = r#"[
        {"accountAlias":"SgsR","asset":"BTC","balance":"0.00250000","withdrawAvailable":"0.00250000","crossWalletBalance":"0.00241969","crossUnPnl":"0.00000000","availableBalance":"0.00241969","updateTime":1592468353979},
        {"accountAlias":"SgsR","asset":"ETH","balance":"1.5","withdrawAvailable":"1.5","crossWalletBalance":"1.5","crossUnPnl":"0","availableBalance":"1.5","updateTime":1592468353979}
    ]"#;
    assert_eq!(parse_coin_balance(body, "BTC"), Ok(Some(0.0025)));
    assert_eq!(parse_coin_balance(body, "eth"), Ok(Some(1.5)));
    assert_eq!(parse_coin_balance(body, "SOL"), Ok(None), "no row is not zero");
    assert!(parse_coin_balance("{}", "BTC").is_err());
    assert!(parse_coin_balance("not json", "BTC").is_err());
}

/// `/dapi/v1/income` as the venue answers it: the rows of `[startTime, endTime]`, oldest first, at most `limit`.
/// Any other request panics; each request's window is recorded.
struct IncomeVenue {
    rows: Vec<Value>,
    asked: std::cell::RefCell<Vec<(i64, i64)>>,
}

impl RestTransport for IncomeVenue {
    fn signed(
        &self,
        _base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<Value, VenueApiError> {
        assert_eq!((method, path), ("GET", "/dapi/v1/income"), "a COIN-M funding read is dapi's");
        let number =
            |k: &str| params.iter().find(|(n, _)| *n == k).and_then(|(_, v)| v.parse::<i64>().ok());
        let (start, end) =
            (number("startTime").expect("both ends"), number("endTime").expect("both ends"));
        self.asked.borrow_mut().push((start, end));
        let limit = usize::try_from(number("limit").unwrap_or(100)).expect("a positive limit");
        let inside = |r: &&Value| r["time"].as_i64().is_some_and(|t| (start..=end).contains(&t));
        Ok(Value::Array(self.rows.iter().filter(inside).take(limit).cloned().collect()))
    }
    fn public(&self, _base_url: &str, path: &str, _p: &[(&str, String)]) -> Answer {
        panic!("the funding read reads nothing public: {path}")
    }
}

/// **A COIN-M mount's funding is the one income poller on its dapi client**: every row of a window that holds more
/// than one page (1,200 settlements, the venue's page is 1,000), then the NEXT window's rows, each booked once, in
/// the settle coin. The poller it replaced asked one page from the spawn instant forever, so it stopped booking at
/// the 1,001st row and never left its first window.
#[test]
fn coin_m_funding_pages_past_a_thousand_rows_and_moves_its_window() {
    let floor = 1_700_000_000_000_i64;
    let row = |id: i64, time: i64| {
        json!({"symbol": "BTCUSD_PERP", "incomeType": "FUNDING_FEE", "income": "-0.00000047",
               "asset": "BTC", "info": "FUNDING_FEE", "time": time, "tranId": id, "tradeId": ""})
    };
    let first_end = floor + 3_600_000;
    let mut rows: Vec<Value> = (0..1_200).map(|i| row(i, floor + 1_000 * (i + 1))).collect();
    rows.extend((0..3).map(|i| row(10_000 + i, first_end + 600_000 * (i + 1))));
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: None,
    };
    let demo = seeded(AccountTier::Demo);
    let rest = VenueRestBinancePerp {
        signer: BinanceHmacSigner::new(&creds, || 0),
        transport: RestTransportDapiPaths(
            IncomeVenue { rows, asked: Default::default() },
            Margining::Inverse,
        ),
        base_url: demo.futures_rest("BTCUSD_PERP").to_string(),
        ty: demo.ty,
        margining: margining_of("BTCUSD_PERP"),
        symbol: "BTCUSD_PERP".to_string(),
        properties: SymbolProperties::default(),
        leverage: 2.0,
        link_id: None,
        algo_order_client_ids: Default::default(),
    };
    assert_eq!(rest.base_url, demo.dapi_rest, "the mount's own book");
    assert_eq!(rest.margining, Margining::Inverse);
    let mut poller = crate::execution::funding::income_poller(&rest, floor);

    let first = poller.poll(first_end).expect("the read answered");
    assert_eq!(first.len(), 1_200, "every row of the window, past the first page");
    let Event::Funding(f) = &first[1_199] else { panic!("a funding event: {:?}", first[1_199]) };
    assert_eq!(
        (f.symbol.as_str(), f.amount, f.timestamp),
        ("BTCUSD_PERP.P", -0.000_000_47, floor + 1_200_000)
    );
    assert_eq!(f.currency.as_deref(), Some("BTC"), "the settle coin, as the row states it");

    let second = poller.poll(first_end + 3_600_000).expect("the read answered");
    let stamps: Vec<i64> = second
        .iter()
        .map(|e| match e {
            Event::Funding(f) => f.timestamp,
            other => panic!("a funding event: {other:?}"),
        })
        .collect();
    assert_eq!(
        stamps,
        (1..=3).map(|i| first_end + 600_000 * i).collect::<Vec<_>>(),
        "the next window's rows, none of the first's again"
    );
    let asked = rest.transport.0.asked.borrow();
    let last = *asked.last().expect("asked");
    assert_eq!(
        last,
        (first_end - crate::execution::funding::LOOKBACK_MS, first_end + 3_600_000),
        "the window moved past the spawn instant, both ends sent"
    );
    assert!(asked.len() >= 3, "the first window took two pages: {asked:?}");
}

/// The demo host's own answers to the mount's reads, CAPTURED on account row 3 on 2026-11-08
/// (`tests/binance_smokes/binance_coin_m_smoke.rs`'s read-only test; the account alias is blanked, rows of other
/// coins and delivered contracts trimmed): `positionRisk?pair=BTCUSD` answers EVERY contract of the pair, and
/// the reconcile read keeps the mounted one; the commission row carries a key fapi's does not
/// (`rpiCommissionRate`); the BTC wallet row is the coin cash (empty on that account).
const CAPTURED_RISK: &str = r#"[{"breakEvenPrice":"0.00000000","entryPrice":"0.00000000","isAutoAddMargin":"false","isolatedMargin":"0.00000000","isolatedWallet":"0","leverage":"0","liquidationPrice":"0","marginType":"cross","markPrice":"0.00000000","maxQty":"9223372036854775807","notionalValue":"0","positionAmt":"0","positionSide":"BOTH","symbol":"BTCUSD_260626","unRealizedProfit":"0.00000000","updateTime":0},{"breakEvenPrice":"0.00000000","entryPrice":"0.00000000","isAutoAddMargin":"false","isolatedMargin":"0.00000000","isolatedWallet":"0","leverage":"2","liquidationPrice":"0","marginType":"cross","markPrice":"0.00000000","maxQty":"1500.0","notionalValue":"0","positionAmt":"0","positionSide":"BOTH","symbol":"BTCUSD_PERP","unRealizedProfit":"0.00000000","updateTime":0},{"breakEvenPrice":"0.00000000","entryPrice":"0.00000000","isAutoAddMargin":"false","isolatedMargin":"0.00000000","isolatedWallet":"0","leverage":"0","liquidationPrice":"0","marginType":"cross","markPrice":"0.00000000","maxQty":"9223372036854775807","notionalValue":"0","positionAmt":"0","positionSide":"BOTH","symbol":"BTCUSD_261225","unRealizedProfit":"0.00000000","updateTime":0}]"#;
const CAPTURED_FEE: &str = r#"{"makerCommissionRate":"0.000200","rpiCommissionRate":"0","symbol":"BTCUSD_PERP","takerCommissionRate":"0.000400"}"#;
const CAPTURED_BALANCE: &str = r#"[{"accountAlias":"X","asset":"NEAR","availableBalance":"0.00000000","balance":"0.00000000","crossUnPnl":"0.00000000","crossWalletBalance":"0.00000000","updateTime":0,"withdrawAvailable":"0.00000000"},{"accountAlias":"X","asset":"BTC","availableBalance":"0.00000000","balance":"0.00000000","crossUnPnl":"0.00000000","crossWalletBalance":"0.00000000","updateTime":0,"withdrawAvailable":"0.00000000"}]"#;

#[test]
fn the_demo_hosts_captured_answers_read_as_the_mounts() {
    let risk: Value = serde_json::from_str(CAPTURED_RISK).expect("captured");
    let venue = Venue::new().on("GET", "/dapi/v1/positionRisk", ok(risk));
    let rest = client(venue, Margining::Inverse, "BTCUSD_PERP");
    let symbol = [("symbol", "BTCUSD_PERP".to_string())];
    let rows = rest
        .transport
        .signed(
            &rest.base_url,
            crate::execution::rest::perp::PATH_POSITIONS,
            "GET",
            &symbol,
            &rest.signer,
        )
        .expect("the pair read");
    let reports = crate::reconcile::parse_perp_position_risk(&rows.to_string()).expect("parses");
    let got: Vec<_> = reports.iter().map(|r| (r.symbol.as_str(), r.qty)).collect();
    assert_eq!(got, [("BTCUSD_PERP.P", 0.0)], "the mounted contract alone, on its series label");
    assert_eq!(
        crate::reconcile::parse_perp_fee_rates(CAPTURED_FEE),
        Ok(Some(vike_model::FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 4.0 }))
    );
    assert_eq!(
        parse_coin_balance(CAPTURED_BALANCE, "BTC"),
        Ok(Some(0.0)),
        "an empty wallet is 0, not absent"
    );
}

/// The venue's own row of the first COIN-M order that RESTED (account row 3, 2026-11-08, the order smoke's
/// `CAPTURED GET /dapi/v1/order`): one contract sent as the shared format's `1.0`, stored as `origQty` `1`.
const CAPTURED_ORDER: &str = r#"{"avgPrice":"0.00","clientOrderId":"vtcm60421613","closePosition":false,"cumBase":"0.00000000","executedQty":"0","goodTillDate":0,"orderId":4608218148,"origQty":"1","origType":"LIMIT","pair":"BTCUSD","positionSide":"BOTH","price":"69588.7","priceMatch":"NONE","priceProtect":false,"reduceOnly":false,"selfTradePreventionMode":"EXPIRE_MAKER","side":"BUY","status":"NEW","stopPrice":"0.0","symbol":"BTCUSD_PERP","time":1794160426755,"timeInForce":"GTC","type":"LIMIT","updateTime":1794160426755,"workingType":"CONTRACT_PRICE"}"#;

/// The by-client-id confirm reads that row on dapi as the live order it was: `OrderAccepted` with its venue id.
#[test]
fn the_confirm_reads_the_rested_coin_m_orders_own_row() {
    use vike_bridge_core::confirm::ConfirmByOrderClientId;
    let row: Value = serde_json::from_str(CAPTURED_ORDER).expect("captured");
    let rest = client(
        Venue::new().on("GET", "/dapi/v1/order", ok(row)),
        Margining::Inverse,
        "BTCUSD_PERP",
    );
    let events = rest.confirm_when("vtcm60421613", 7, true);
    assert!(
        matches!(events.as_slice(), [Event::OrderAccepted(a)]
            if a.order_client_id == "vtcm60421613" && a.venue_order_id.as_deref() == Some("4608218148")),
        "{events:?}"
    );
    let sent = rest.transport.0.params_of("GET", "/dapi/v1/order");
    assert!(
        sent.contains(&("origClientOrderId".to_string(), "vtcm60421613".to_string())),
        "{sent:?}"
    );
}

#[test]
fn the_coin_m_book_follows_the_tier() {
    for (tier, ty) in [
        (AccountTier::Demo, UrlType::Demo),
        (AccountTier::Paper, UrlType::Demo),
        (AccountTier::Live, UrlType::Live),
    ] {
        let hosts = seeded(tier);
        assert_eq!((hosts.ty, margining_of("BTCUSD_PERP")), (ty, Margining::Inverse), "{tier:?}");
        assert_eq!(hosts.futures_rest("BTCUSD_PERP.P"), hosts.dapi_rest, "{tier:?}");
        assert_eq!(margining_of("BTCUSDT"), Margining::Linear, "{tier:?}");
    }
    assert_ne!(seeded(AccountTier::Demo).dapi_rest, seeded(AccountTier::Live).dapi_rest);
}
