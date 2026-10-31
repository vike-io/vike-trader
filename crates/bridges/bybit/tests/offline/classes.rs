//! The instrument CLASSES the exec plane signs since decision 0231 (offline): a dated linear future, a
//! coin-margined contract and a spot pair, over the rows the Bybit DEMO host really served on 2026-10-30
//! (`tests/fixtures/classes/`, captured by `tests/bybit_classes_probe.rs`).
//!
//! * the order wire per book (category, no position index on spot, `marketUnit` on a spot market order);
//! * the private-stream mapper (a spot row keeps its own engine symbol; a spot buy's base-coin fee is netted);
//! * the reconcile reads and the replay (the book a symbol is read in; a spot holding is a wallet row);
//! * the account settings a coin-margined contract needs (the collateral switch), DEMO only.
//!
//! Where a test says "base", it fails on the code before this decision (the assertion differs), not at compile.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::BybitV5Signer;
use vike_bridge_core::transport::VenueApiError;
use vike_bybit::data::Category;
use vike_bybit::event_mapper::{map_bybit_perp, map_execution_fast};
use vike_bybit::history::map_bybit_history;
use vike_bybit::modes::{Modes, inverse_settle_coin, order_reject_reason};
use vike_bybit::perp::{BybitPerpRest, DEMO_REST, MAINNET_REST};
use vike_bybit::recon_client::spot_holding;
use vike_bybit::transport::BybitTransport;
use vike_model::events::Event;
use vike_model::{MarginMode, OrderRequest, OrderType, SymbolProperties};
use vike_secrets::credentials::Credentials;

type Call = (String, String, Vec<(String, Value)>);

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<Call>>>);

impl Log {
    fn calls(&self) -> Vec<Call> {
        self.0.lock().unwrap().clone()
    }
}

/// Records every signed call; `answer` plays the venue for a path (default: an accepted order).
struct Wire {
    log: Log,
    answer: fn(&str, &[(&str, Value)]) -> Value,
}

impl BybitTransport for Wire {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
        _signer: &BybitV5Signer,
    ) -> Result<Value, VenueApiError> {
        let owned = params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        self.log.0.lock().unwrap().push((method.to_string(), path.to_string(), owned));
        Ok((self.answer)(path, params))
    }
}

fn accepted(_path: &str, _params: &[(&str, Value)]) -> Value {
    json!({"retCode": 0, "retMsg": "OK", "result": {"orderId": "oid-1", "list": []}})
}

fn creds() -> Credentials {
    Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: None,
    }
}

fn client(
    symbol: &str,
    category: Category,
    tick: f64,
    step: f64,
    log: &Log,
) -> BybitPerpRest<Wire> {
    BybitPerpRest {
        category,
        signer: BybitV5Signer::new(&creds(), || 0),
        transport: Wire { log: log.clone(), answer: accepted },
        base_url: "https://unused.invalid".to_string(),
        symbol: symbol.to_string(),
        properties: SymbolProperties { tick_size: tick, step_size: step, ..Default::default() },
        leverage: 2.0,
    }
}

fn request(symbol: &str, side: i32, qty: f64, kind: OrderType, price: Option<f64>) -> OrderRequest {
    let trigger_price = (kind == OrderType::Stop).then_some(price.unwrap_or(1.0));
    OrderRequest {
        client_order_id: "c1".into(),
        symbol: symbol.into(),
        side,
        qty,
        order_type: kind,
        price,
        trigger_price,
        ..Default::default()
    }
}

fn keys(params: &[(&str, Value)]) -> Vec<String> {
    params.iter().map(|(k, _)| (*k).to_string()).collect()
}

fn get<'a>(params: &'a [(&'static str, Value)], key: &str) -> Option<&'a Value> {
    params.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
}

// ---- the order wire, per book ----------------------------------------------------------------------------

/// A spot order names no position index and no reduce-only word, and a MARKET order states the base coin: the
/// venue read `qty 13` of a market BUY as 13 USDT (the echo said `marketUnit quoteCoin`, `orderQty 0`).
#[test]
fn a_spot_order_names_no_position_index_and_a_market_order_states_the_base_unit() {
    let log = Log::default();
    let spot = client("DOGEUSDT", Category::Spot, 0.00001, 0.1, &log);

    let limit =
        spot.build_order_params(&request("DOGEUSDT.S", 1, 236.7, OrderType::Limit, Some(0.05281)));
    assert_eq!(get(&limit, "category"), Some(&json!("spot")));
    assert_eq!(
        get(&limit, "symbol"),
        Some(&json!("DOGEUSDT")),
        "the wire name, never the engine's `.S`"
    );
    assert_eq!(get(&limit, "qty"), Some(&json!("236.7")));
    assert_eq!(get(&limit, "price"), Some(&json!("0.05281")));
    let names = keys(&limit.iter().map(|(k, v)| (*k, v.clone())).collect::<Vec<_>>());
    assert!(!names.contains(&"positionIdx".to_string()), "{names:?}");
    assert!(!names.contains(&"reduceOnly".to_string()), "{names:?}");
    assert_eq!(get(&limit, "marketUnit"), None, "a limit order's qty is base already");

    let market = spot.build_order_params(&request("DOGEUSDT.S", 1, 165.7, OrderType::Market, None));
    assert_eq!(get(&market, "marketUnit"), Some(&json!("baseCoin")));
    assert_eq!(get(&market, "qty"), Some(&json!("165.7")));
    let sell = spot.build_order_params(&request("DOGEUSDT.S", -1, 165.5, OrderType::Market, None));
    assert_eq!(get(&sell, "marketUnit"), Some(&json!("baseCoin")), "both sides state the unit");
}

/// A stop on a spot pair is refused whole with the reason and NOTHING is sent: the derivative stop wire is not
/// a spot word, and a bracket's stop leg refused after its entry filled would leave an unprotected position.
#[test]
fn a_stop_order_on_a_spot_pair_is_refused_before_anything_is_sent() {
    let log = Log::default();
    let spot = client("DOGEUSDT", Category::Spot, 0.00001, 0.1, &log);
    let events = spot.submit_order(&request("DOGEUSDT.S", -1, 100.0, OrderType::Stop, Some(0.05)));
    assert!(log.calls().is_empty(), "nothing is signed: {:?}", log.calls());
    assert_eq!(events.len(), 2, "{events:?}");
    let Event::OrderRejected(rejected) = &events[1] else { panic!("{events:?}") };
    assert!(
        rejected.reason.contains("spot") && rejected.reason.contains("nothing was sent"),
        "{}",
        rejected.reason
    );
    // The batch door shapes the same refusal.
    let events =
        spot.submit_batch(&[request("DOGEUSDT.S", -1, 100.0, OrderType::Stop, Some(0.05))]);
    assert!(log.calls().is_empty());
    assert!(events.iter().any(|e| matches!(e, Event::OrderRejected(_))), "{events:?}");
}

/// A coin-margined contract and a dated linear future keep the derivative wire (one-way index, reduce-only), each
/// in its own book; the quantity is the venue's own step (a whole contract on an inverse one).
#[test]
fn an_inverse_and_a_dated_order_keep_the_derivative_wire_in_their_own_book() {
    let log = Log::default();
    let inverse = client("BTCUSD", Category::Inverse, 0.1, 1.0, &log);
    let p = inverse.build_order_params(&request("BTCUSD", 1, 8.0, OrderType::Market, None));
    assert_eq!(get(&p, "category"), Some(&json!("inverse")));
    // A step of `1` formats as `8.0` (the grid's own scale); the demo ACCEPTED exactly this spelling for 8 contracts.
    assert_eq!(get(&p, "qty"), Some(&json!("8.0")), "8 contracts, the demo's accepted `qty`");
    assert_eq!(get(&p, "positionIdx"), Some(&json!(0)));
    assert_eq!(get(&p, "reduceOnly"), Some(&json!(false)));
    assert_eq!(get(&p, "marketUnit"), None);

    let dated = client("ETHUSDT-06NOV26", Category::Linear, 0.01, 0.01, &log);
    let p = dated.build_order_params(&request(
        "ETHUSDT-06NOV26",
        1,
        0.01,
        OrderType::Limit,
        Some(1000.0),
    ));
    assert_eq!(get(&p, "category"), Some(&json!("linear")));
    assert_eq!(get(&p, "symbol"), Some(&json!("ETHUSDT-06NOV26")));
    assert_eq!(get(&p, "positionIdx"), Some(&json!(0)));
    let stop = dated.build_order_params(&request(
        "ETHUSDT-06NOV26",
        -1,
        0.01,
        OrderType::Stop,
        Some(1345.95),
    ));
    assert_eq!(get(&stop, "triggerPrice"), Some(&json!("1345.95")));
    assert_eq!(
        get(&stop, "triggerDirection"),
        Some(&json!(2)),
        "the demo took this stop (`Untriggered`)"
    );
}

/// Cancel, replay and leverage follow the client's book; a spot pair has no leverage to set.
#[test]
fn cancel_replay_and_leverage_follow_the_clients_book() {
    let log = Log::default();
    let inverse = client("BTCUSD", Category::Inverse, 0.1, 1.0, &log);
    inverse.cancel_order("c1").unwrap();
    inverse.set_leverage().unwrap();
    let calls = log.calls();
    assert_eq!(calls.len(), 2);
    for call in &calls {
        let category = call.2.iter().find(|(k, _)| k == "category").map(|(_, v)| v.clone());
        assert_eq!(category, Some(json!("inverse")), "{call:?}");
    }

    let spot_log = Log::default();
    let spot = client("DOGEUSDT", Category::Spot, 0.00001, 0.1, &spot_log);
    spot.set_leverage().unwrap();
    assert!(spot_log.calls().is_empty(), "a spot pair has no leverage; nothing is sent");
}

/// The mounted client replays ADMITTED symbols (`bybit_history_events_of`): each is asked in the book its name
/// says, under the venue's name for it.
#[test]
fn a_replay_asks_each_symbol_in_its_own_book() {
    let log = Log::default();
    let mounted = client("BTCUSDT", Category::Linear, 0.1, 0.001, &log);
    for symbol in ["BTCUSDT", "DOGEUSDT.S", "BTCUSD", "BTCUSDZ26", "ETHUSDT-06NOV26"] {
        mounted.get_order_history_for(symbol, 50).unwrap();
    }
    let asked: Vec<(String, String)> = log
        .calls()
        .iter()
        .map(|(_, _, p)| {
            let text = |k: &str| {
                p.iter().find(|(n, _)| n == k).and_then(|(_, v)| v.as_str()).unwrap().to_string()
            };
            (text("category"), text("symbol"))
        })
        .collect();
    let want = [
        ("linear", "BTCUSDT"),
        ("spot", "DOGEUSDT"),
        ("inverse", "BTCUSD"),
        ("inverse", "BTCUSDZ26"),
        ("linear", "ETHUSDT-06NOV26"),
    ]
    .map(|(c, s)| (c.to_string(), s.to_string()));
    assert_eq!(asked, want);
}

// ---- the private stream ----------------------------------------------------------------------------------

fn spot_buy_rows() -> Vec<Value> {
    let envelope: Value =
        serde_json::from_str(include_str!("../fixtures/classes/execution_list_spot_buys.json"))
            .unwrap();
    envelope["result"]["list"].as_array().unwrap().clone()
}

/// The private stream carries every book of the account in one `execution` topic. A WS row names its `category`
/// (the REST row of the fixture does not), added here as the venue documents it.
fn ws_execution(row: &Value, category: &str) -> Value {
    let mut row = row.clone();
    row["category"] = json!(category);
    json!({"topic": "execution", "data": [row]})
}

/// **A spot buy is booked as the quantity the account RECEIVED**: the fee is charged in the base coin (`execFee
/// 0.1657 DOGE` of `execQty 165.7`, the wallet gained 165.5343), so the fill is net of it and the commission is the
/// fee at the fill price in the quote coin. And the fill carries the spot engine symbol, never the perpetual's.
#[test]
fn a_spot_buy_is_booked_net_of_its_base_coin_fee_under_the_spot_symbol() {
    let rows = spot_buy_rows();
    let base_unit = rows.iter().find(|r| r["marketUnit"] == "baseCoin").expect("the base-unit buy");
    let events = map_bybit_perp(&ws_execution(base_unit, "spot"), "bybit", "BTCUSDT");
    let Event::Fill(fill) = &events[0] else { panic!("{events:?}") };
    assert_eq!(
        fill.symbol.as_str(),
        "DOGEUSDT.S",
        "base: the row's bare name, which is the PERPETUAL's"
    );
    assert_eq!(fill.side, 1);
    assert!((fill.last_qty - 165.5343).abs() < 1e-9, "{}", fill.last_qty);
    assert!((fill.commission - 0.1657 * 0.07545).abs() < 1e-9, "{}", fill.commission);
    assert_eq!(fill.commission_asset.as_str(), "USDT");
    assert_eq!(fill.last_px, 0.07545);
    let Event::OrderFilled(wrap) = &events[1] else { panic!("{events:?}") };
    assert_eq!(wrap.fill, *fill, "the wrap carries the very fill the position folds");
}

/// A spot SELL's fee is in the quote coin already (measured: `cumFeeDetail {"USDT": ...}`), so the fill stays
/// as the venue stated it; and a linear row keeps the bare name it always had.
#[test]
fn a_sell_and_a_derivative_row_keep_what_the_venue_stated() {
    let sell = json!({
        "execType": "Trade", "execId": "sell-1", "orderLinkId": "c-sell", "symbol": "DOGEUSDT",
        "side": "Sell", "execQty": "337.6", "execPrice": "0.07548", "execFee": "0.025482048",
        "feeCurrency": "USDT", "isMaker": false, "execTime": "1793390090000",
        "orderQty": "337.6", "cumExecQty": "337.6", "leavesQty": "0"
    });
    let events = map_bybit_perp(&ws_execution(&sell, "spot"), "bybit", "BTCUSDT");
    let Event::Fill(fill) = &events[0] else { panic!("{events:?}") };
    assert_eq!(fill.symbol.as_str(), "DOGEUSDT.S");
    assert_eq!((fill.last_qty, fill.commission), (337.6, 0.025482048));
    assert_eq!(fill.commission_asset.as_str(), "USDT");

    let dated: Value =
        serde_json::from_str(include_str!("../fixtures/classes/execution_list_dated_buy.json"))
            .unwrap();
    let row = &dated["result"]["list"][0];
    let events = map_bybit_perp(&ws_execution(row, "linear"), "bybit", "BTCUSDT");
    let Event::Fill(fill) = &events[0] else { panic!("{events:?}") };
    assert_eq!(fill.symbol.as_str(), "ETHUSDT-06NOV26");
    assert_eq!((fill.last_qty, fill.commission_asset.as_str()), (0.01, "USDT"));
}

/// ⚠ The fix that does not need a spot class: the SAME name is a spot pair and a linear perpetual, and a spot fill
/// of the account (an operator's, from the venue's own app) used to be folded into the perpetual's position.
#[test]
fn a_spot_fill_never_reaches_the_perpetual_of_the_same_name() {
    let row = json!({
        "execType": "Trade", "execId": "x1", "orderLinkId": "", "symbol": "BTCUSDT", "side": "Buy",
        "execQty": "0.001", "execPrice": "77000", "execFee": "0.000001", "feeCurrency": "BTC",
        "isMaker": false, "execTime": "1793390090000", "orderQty": "0.001", "leavesQty": "0"
    });
    for ev in map_bybit_perp(&ws_execution(&row, "spot"), "bybit", "BTCUSDT") {
        if let Event::Fill(f)
        | Event::OrderFilled(vike_model::events::OrderFilled { fill: f, .. }) = &ev
        {
            assert_ne!(f.symbol.as_str(), "BTCUSDT", "folded into the perpetual: {ev:?}");
        }
    }
}

/// The fast-fill hint carries no fee, and a spot buy's quantity is `execQty - execFee`: the hint is dropped for a
/// spot row (its slow twin books the fill), and kept for a derivative.
#[test]
fn the_fast_hint_is_dropped_for_a_spot_row_and_kept_for_a_derivative() {
    let row = json!({
        "execId": "f1", "orderLinkId": "c1", "symbol": "DOGEUSDT", "side": "Buy",
        "execQty": "165.7", "execPrice": "0.07545", "execTime": "1793390090000", "category": "spot"
    });
    assert!(map_execution_fast(&row, "bybit", "BTCUSDT").is_empty());
    let mut linear = row.clone();
    linear["category"] = json!("linear");
    linear["symbol"] = json!("ETHUSDT");
    assert_eq!(map_execution_fast(&linear, "bybit", "BTCUSDT").len(), 1);
}

/// The history replay stamps the symbol it was ASKED for (a REST row has no `category`), and nets the same fee.
#[test]
fn the_replay_of_a_spot_pair_keeps_its_symbol_and_nets_its_fee() {
    let rows = Value::Array(spot_buy_rows());
    let events = map_bybit_history(&json!([]), &rows, "bybit", "DOGEUSDT.S");
    let fills: Vec<_> =
        events.iter().filter_map(|e| if let Event::Fill(f) = e { Some(f) } else { None }).collect();
    assert_eq!(fills.len(), 2);
    for f in &fills {
        assert_eq!(f.symbol.as_str(), "DOGEUSDT.S");
        assert_eq!(f.commission_asset.as_str(), "USDT");
    }
    let total: f64 = fills.iter().map(|f| f.last_qty).sum();
    // 165.7 + 172.29953611663353214 gross, less the two fees: what the wallet gained (measured 337.66153658).
    assert!((total - 337.66153658).abs() < 1e-6, "{total}");
}

// ---- reconcile -------------------------------------------------------------------------------------------

/// A spot holding is the wallet row of the pair's base coin, `Cash`, and a quantity under one step of the pair is
/// flat (the demo's DOGE dust of 0.0615 under a step of 0.1); a coin the answer does not list is none held.
#[test]
fn a_spot_holding_is_the_base_coins_wallet_row() {
    let body: Value =
        serde_json::from_str(include_str!("../fixtures/classes/wallet_balance_doge_dust.json"))
            .unwrap();
    let result = body["result"].to_string();
    let dust = spot_holding(&result, "DOGEUSDT.S", "DOGE", 0.1, 5).unwrap();
    assert_eq!(
        (dust.qty, dust.symbol.as_str()),
        (0.0, "DOGEUSDT.S"),
        "dust under one step is flat"
    );
    assert_eq!(dust.margin_mode, MarginMode::Cash);

    let mut held = body["result"].clone();
    held["list"][0]["coin"][0]["walletBalance"] = json!("337.66153658");
    let held = spot_holding(&held.to_string(), "DOGEUSDT.S", "DOGE", 0.1, 5).unwrap();
    assert_eq!(held.qty, 337.66153658);
    assert_eq!(held.avg_px, 0.0, "the wallet states no cost basis");

    let none = spot_holding(&body["result"].to_string(), "ETHUSDT.S", "ETH", 0.0001, 5).unwrap();
    assert_eq!(none.qty, 0.0, "a coin the wallet does not list is none held");
    assert!(
        spot_holding("{}", "DOGEUSDT.S", "DOGE", 0.1, 5).is_err(),
        "no list is malformed, never flat"
    );
}

// ---- the settings a coin-margined contract needs ---------------------------------------------------------

#[test]
fn an_inverse_contracts_settlement_coin_is_its_name_up_to_usd() {
    for (symbol, coin) in
        [("BTCUSD", Some("BTC")), ("BTCUSDZ26", Some("BTC")), ("ETHUSDH27", Some("ETH"))]
    {
        assert_eq!(inverse_settle_coin(symbol), coin, "{symbol}");
    }
    for symbol in ["BTCUSDT", "ETHUSDT-06NOV26", "DOGEUSDT.S", "BTCPERP"] {
        assert_eq!(inverse_settle_coin(symbol), None, "{symbol}");
    }
}

/// The venue's collateral switch scripted: the first read says OFF (the captured answer for BTC), a write turns it ON.
struct CollateralWorld {
    log: Log,
    switch_on: Mutex<bool>,
}

impl BybitTransport for CollateralWorld {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
        _signer: &BybitV5Signer,
    ) -> Result<Value, VenueApiError> {
        let owned = params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        self.log.0.lock().unwrap().push((method.to_string(), path.to_string(), owned));
        let ok = |result: Value| Ok(json!({"retCode": 0, "retMsg": "OK", "result": result}));
        match path {
            "/v5/account/collateral-info" => {
                let on = *self.switch_on.lock().unwrap();
                let mut info: Value = serde_json::from_str(include_str!(
                    "../fixtures/classes/collateral_info_btc_off.json"
                ))
                .unwrap();
                info["result"]["list"][0]["collateralSwitch"] = json!(on);
                Ok(info)
            }
            "/v5/account/set-collateral-switch" => {
                *self.switch_on.lock().unwrap() = true;
                ok(json!({}))
            }
            "/v5/position/list" => {
                let symbol = params
                    .iter()
                    .find(|(k, _)| *k == "symbol")
                    .and_then(|(_, v)| v.as_str())
                    .unwrap_or("");
                ok(
                    json!({"list": [{"symbol": symbol, "positionIdx": 0, "size": "0", "tradeMode": 0}]}),
                )
            }
            "/v5/account/info" => {
                ok(json!({"marginMode": "REGULAR_MARGIN", "unifiedMarginStatus": 3}))
            }
            _ => ok(json!({"list": []})),
        }
    }
}

fn modes(base: &str, on: bool, log: &Log) -> Modes<CollateralWorld> {
    Modes::new(
        BybitV5Signer::new(&creds(), || 0),
        CollateralWorld { log: log.clone(), switch_on: Mutex::new(on) },
        base,
        base == MAINNET_REST,
    )
}

fn paths(log: &Log) -> Vec<String> {
    log.calls().into_iter().map(|c| format!("{} {}", c.0, c.1)).collect()
}

/// On the demo host the first order of a coin-margined contract finds the settlement coin OFF (the venue's own
/// `110101` otherwise) and switches it ON, once; a coin already ON is left alone.
#[test]
fn a_coin_margined_contract_gets_its_settlement_coin_switched_on_on_the_demo_host() {
    let log = Log::default();
    modes(DEMO_REST, false, &log).ensure("BTCUSD").expect("switched, then trades");
    let seen = paths(&log);
    assert!(seen.contains(&"GET /v5/account/collateral-info".to_string()), "{seen:?}");
    assert!(seen.contains(&"POST /v5/account/set-collateral-switch".to_string()), "{seen:?}");
    let write = log.calls().into_iter().find(|c| c.1.ends_with("set-collateral-switch")).unwrap();
    assert!(write.2.contains(&("coin".to_string(), json!("BTC"))), "{write:?}");
    assert!(write.2.contains(&("collateralSwitch".to_string(), json!("ON"))), "{write:?}");

    let log = Log::default();
    modes(DEMO_REST, true, &log).ensure("BTCUSDZ26").expect("already on");
    assert!(
        !paths(&log).iter().any(|p| p.contains("set-collateral-switch")),
        "a coin already on is never written: {:?}",
        paths(&log)
    );
}

/// A spot pair has no mode to read, and a LIVE account is never read or written (owner ruling 2026-10-26).
#[test]
fn a_spot_pair_and_a_live_account_are_never_touched() {
    let log = Log::default();
    modes(DEMO_REST, false, &log).ensure("DOGEUSDT.S").unwrap();
    assert!(log.calls().is_empty(), "{:?}", paths(&log));
    modes(MAINNET_REST, false, &log).ensure("BTCUSD").unwrap();
    assert!(log.calls().is_empty(), "{:?}", paths(&log));
}

/// On a LIVE account the venue's `110101` is worded with the hand fix (the bridge changes no live setting).
#[test]
fn a_live_account_is_told_which_coin_to_switch_on() {
    let why = order_reject_reason(
        110101,
        "Trading contracts under Cross and Portfolio modes requires enabling the settlement asset as collateral.",
        MAINNET_REST,
        "BTCUSD",
    );
    assert!(why.contains("BTC") && why.contains("switch") && why.contains("collateral"), "{why}");
    // The demo host keeps the venue's own text (the bridge switches the coin on before the order there).
    let raw = order_reject_reason(110101, "x", DEMO_REST, "BTCUSD");
    assert_eq!(raw, "x");
}
