//! **The reconcile client reads every ADMITTED symbol of the account, not only the mounted one**
//! (offline: a recording `RestTransport`, canned bodies, no network, nothing `#[ignore]`d).
//!
//! The core's reconcile driver asks a `ReconcileClient` three questions with no symbol in them
//! (`fetch_order_status_reports`, `fetch_position_status_reports`, `fetch_fill_reports`) and the
//! restore planner reads an order the venue's report does not carry as GONE. So a client that read the
//! mounted symbol alone made every resting order on a symbol admitted later invisible after a restart.
//! The client is told which symbols the engine admitted (`ReconcileClient::admit_symbol`) and reads each
//! one with the SAME request shape it uses for the mounted symbol.
//!
//! The pins, each guarding a different way this can go wrong:
//!   * no admitted symbol: the request list is EXACTLY the mounted symbol's, byte for byte (on the
//!     perp lane its order report is followed by the Algo Service's open-list read for the same
//!     symbol: `openOrders` is not documented to list a conditional order);
//!   * an admitted symbol: its orders, positions and fills appear, tagged with the engine's spelling
//!     (`.P` on the perp lane), its fills under the symbol-qualified trade id the user-data stream
//!     spells (`family::fill_trade_id`, decision 0174 item 5);
//!   * a symbol nobody admitted (another lane's spelling, the mounted one again, a 17th) is never
//!     queried;
//!   * a failed read of an admitted symbol fails the whole fetch (a partial list would be read by the
//!     restore planner as "the rest are gone");
//!   * a row for a symbol that was not asked for is dropped, never reported.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use vike_binance::ReconcileClientBinance;
use vike_binance::reconcile::BINANCE_RECONCILE;
use vike_bridge_core::signer::{PreparedRequest, Signer};
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_execution::MAX_EXTRA_SYMBOLS;
use vike_execution::recon::ReconcileClient;

type Call = (String, Vec<(String, String)>);
type Calls = Arc<Mutex<Vec<Call>>>;

/// The Algo Service's open-order list: on the PERP lane every order report is followed by this read
/// for the same symbols, because `openOrders` is not documented to list a conditional order.
const ALGO_LIST: &str = vike_binance::execution::rest::algo::PATH_OPEN_ALGO_ORDERS;

/// The Algo Service's order history: on the PERP lane a fill read is followed by this read for every
/// symbol that has an order-client-id-less fill, to name the fills of triggered stops (`algo::name_algo_children`).
const ALGO_HISTORY: &str = vike_binance::execution::rest::algo::PATH_ALL_ALGO_ORDERS;

/// One order by id (`GET /fapi/v1/order`) and one algo order by `clientAlgoId` (`GET
/// /fapi/v1/algoOrder`): the two reads that name an order-client-id-less fill whose stop the history did not list
/// (a stop older than the history's window), asked once per fill order and remembered.
const ORDER_ONE: &str = vike_binance::execution::rest::perp::PATH_ORDER;
const ALGO_ONE: &str = vike_binance::execution::rest::algo::PATH_ALGO_ORDER;
/// The spot lane's order read (`GET /api/v3/order`): its `myTrades` fills name the order by id too.
const SPOT_ORDER_ONE: &str = vike_binance::execution::rest::spot::PATH_ORDER;
/// A spot pair's public `exchangeInfo` row: its base coin and size step, which the spot lane's holding needs.
const SPOT_EXCHANGE_INFO: &str = vike_binance::execution::rest::spot::PATH_EXCHANGE_INFO;
/// The account's liquidation and ADL orders: on the PERP lane a fill read that answered rows is
/// followed by this read for the same symbol, and those orders' trades are never reported.
const FORCE_ORDERS: &str = BINANCE_RECONCILE.paths.perp_force_orders;

struct NullSigner;
impl Signer for NullSigner {
    fn prepare(&self, _p: &[(&str, String)], _m: &str, _path: &str) -> PreparedRequest {
        PreparedRequest::default()
    }
}

/// Records every request and answers each endpoint with one row for the `symbol` it was asked about,
/// plus (when `stray`) a row for SOLUSDT that nobody admitted. `fail_on` makes one symbol's reads fail.
struct Wire {
    calls: Calls,
    fail_on: Option<&'static str>,
    /// The venue code `fail_on` answers with.
    fail_code: i64,
    stray: bool,
    /// Triggered stops the Algo Service history reports: `(symbol, child order id, bare order client id)`.
    children: Vec<(&'static str, &'static str, &'static str)>,
    /// Make the Algo Service history read fail (any symbol); a test flips it between passes.
    fail_history: Arc<AtomicBool>,
    /// What `GET /fapi/v1/order` knows by id: `(symbol, order id, clientOrderId as the venue stores
    /// it)`. An order not listed answers `-2013` (unknown order).
    orders: Vec<(&'static str, &'static str, &'static str)>,
    /// What `GET /fapi/v1/algoOrder` knows by `clientAlgoId`: `(clientAlgoId, actualOrderId, symbol)`;
    /// an empty `actualOrderId` is a stop the venue has not (yet) said which order it placed. One not
    /// listed answers `-2013`.
    algo_one: Vec<(&'static str, &'static str, &'static str)>,
    /// Make `GET /fapi/v1/order` fail with a code that is not "unknown order".
    fail_order_lookup: Arc<AtomicBool>,
    /// Make `GET /fapi/v1/order` fail for this one symbol only (a persistent error on one symbol),
    /// from its second read on: the first is the liquidation check's, which is remembered.
    fail_order_symbol: Option<&'static str>,
    /// How many `GET /fapi/v1/order` reads `fail_order_symbol` has seen.
    failing_symbol_reads: std::sync::atomic::AtomicUsize,
    /// Make `GET /fapi/v1/algoOrder` fail with a code that is not "unknown order".
    fail_algo_lookup: Arc<AtomicBool>,
    /// Answer the fills read with no rows at all (a pass in which nothing traded).
    no_fills: Arc<AtomicBool>,
    /// More order-client-id-less fills, each of its own order: the ids (the `fill_row` of `symbol` is still first).
    more_fill_orders: Vec<u64>,
    /// The order ids `forceOrders` lists (liquidation and ADL orders), for every symbol asked.
    forced: Vec<u64>,
    /// Make `forceOrders` fail.
    fail_forced: bool,
}

impl Wire {
    fn new() -> (Self, Calls) {
        let calls: Calls = Arc::new(Mutex::new(Vec::new()));
        let wire = Wire {
            calls: Arc::clone(&calls),
            fail_on: None,
            fail_code: -1003,
            stray: false,
            children: Vec::new(),
            fail_history: Arc::default(),
            orders: Vec::new(),
            algo_one: Vec::new(),
            fail_order_lookup: Arc::default(),
            fail_order_symbol: None,
            failing_symbol_reads: Default::default(),
            fail_algo_lookup: Arc::default(),
            no_fills: Arc::default(),
            more_fill_orders: Vec::new(),
            forced: Vec::new(),
            fail_forced: false,
        };
        (wire, calls)
    }
}

fn order_row(symbol: &str, spot: bool) -> Value {
    let mut row = json!({
        "symbol": symbol, "orderId": 1, "clientOrderId": format!("c-{symbol}"), "price": "1",
        "avgPrice": "0", "origQty": "1", "executedQty": "0", "cummulativeQuoteQty": "0",
        "status": "NEW", "type": "LIMIT", "side": "BUY", "updateTime": 1,
    });
    if spot {
        row["avgPrice"] = Value::Null;
    }
    row
}

fn position_row(symbol: &str) -> Value {
    json!({
        "symbol": symbol, "positionAmt": "0.5", "entryPrice": "10", "positionSide": "BOTH",
        "updateTime": 1, "marginType": "cross",
    })
}

fn fill_row(symbol: &str) -> Value {
    json!({
        "symbol": symbol, "id": 77, "orderId": 9, "side": "BUY", "price": "10", "qty": "1",
        "commission": "0", "commissionAsset": "USDT", "maker": true, "isBuyer": true,
        "isMaker": true, "time": 1,
    })
}

impl RestTransport for Wire {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<Value, VenueApiError> {
        let recorded: Vec<(String, String)> =
            params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        let symbol = recorded
            .iter()
            .find(|(k, _)| k == "symbol")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        self.calls.lock().unwrap().push((path.to_string(), recorded.clone()));
        if self.fail_on == Some(symbol.as_str()) {
            return Err(VenueApiError { code: self.fail_code, msg: format!("refused: {symbol}") });
        }
        if path == ALGO_HISTORY {
            if self.fail_history.load(Ordering::Relaxed) {
                return Err(VenueApiError { code: -1000, msg: "history refused".to_string() });
            }
            let rows: Vec<Value> = self
                .children
                .iter()
                .filter(|(s, _, _)| *s == symbol)
                .map(|(s, child, order_client_id)| {
                    json!({ "algoId": 55, "clientAlgoId": order_client_id, "symbol": s,
                            "actualOrderId": child, "algoStatus": "FINISHED" })
                })
                .collect();
            return Ok(Value::Array(rows));
        }
        if path == ALGO_LIST {
            // The Algo Service's open list (conditional orders): none rest in these scenarios; the
            // algo read's own cases are `algo_orders.rs`'s.
            return Ok(Value::Array(Vec::new()));
        }
        if path == FORCE_ORDERS {
            if self.fail_forced {
                return Err(VenueApiError { code: -1000, msg: "forceOrders refused".to_string() });
            }
            // the documented row shape (`GET /fapi/v1/forceOrders`), one per listed order
            let rows = self.forced.iter().map(|id| {
                json!({ "orderId": id, "symbol": symbol, "status": "FILLED",
                        "clientOrderId": format!("autoclose-{id}"), "type": "LIMIT",
                        "timeInForce": "IOC", "side": "SELL", "time": 1, "updateTime": 1 })
            });
            return Ok(Value::Array(rows.collect()));
        }
        let unknown = || VenueApiError { code: -2013, msg: "Order does not exist.".to_string() };
        if path == ORDER_ONE {
            let failing_symbol = self.fail_order_symbol == Some(symbol.as_str())
                && self.failing_symbol_reads.fetch_add(1, Ordering::Relaxed) > 0;
            if self.fail_order_lookup.load(Ordering::Relaxed) || failing_symbol {
                return Err(VenueApiError { code: -1000, msg: "order lookup refused".to_string() });
            }
            let id = recorded.iter().find(|(k, _)| k == "orderId").map(|(_, v)| v.as_str());
            return self
                .orders
                .iter()
                .find(|(s, o, _)| *s == symbol && Some(*o) == id)
                .map(|(s, o, c)| json!({ "symbol": s, "orderId": o, "clientOrderId": c }))
                .ok_or_else(unknown);
        }
        if path == ALGO_ONE {
            if self.fail_algo_lookup.load(Ordering::Relaxed) {
                return Err(VenueApiError { code: -1000, msg: "algo lookup refused".to_string() });
            }
            let cid = recorded.iter().find(|(k, _)| k == "clientAlgoId").map(|(_, v)| v.as_str());
            return self
                .algo_one
                .iter()
                .find(|(c, _, _)| Some(*c) == cid)
                .map(|(c, actual, s)| {
                    json!({ "algoId": 55, "clientAlgoId": c, "actualOrderId": actual,
                            "symbol": s, "algoStatus": "FINISHED" })
                })
                .ok_or_else(unknown);
        }
        let p = BINANCE_RECONCILE.paths;
        if path == p.perp_user_trades && self.no_fills.load(Ordering::Relaxed) {
            return Ok(Value::Array(Vec::new()));
        }
        if path == p.spot_account {
            // the spot wallet: the user's BTC beyond the strategy's, some ETH locked in an order
            return Ok(json!({ "uid": 7, "balances": [
                { "asset": "USDT", "free": "1000", "locked": "0" },
                { "asset": "BTC", "free": "0.4", "locked": "0.1" },
                { "asset": "ETH", "free": "1.5", "locked": "0.5" },
            ]}));
        }
        let spot = path == p.spot_open_orders || path == p.spot_my_trades;
        let row = |s: &str| {
            if path == p.spot_open_orders || path == p.perp_open_orders {
                order_row(s, spot)
            } else if path == p.perp_positions {
                position_row(s)
            } else {
                fill_row(s)
            }
        };
        let mut rows = vec![row(&symbol)];
        if self.stray {
            rows.push(row("SOLUSDT"));
        }
        if path == p.perp_user_trades {
            for (n, order) in self.more_fill_orders.iter().enumerate() {
                let mut more = fill_row(&symbol);
                more["orderId"] = json!(order);
                more["id"] = json!(1_000 + n as u64);
                rows.push(more);
            }
        }
        Ok(Value::Array(rows))
    }

    /// The one public read: a spot pair's `exchangeInfo` row (its base coin and `LOT_SIZE` step), recorded.
    fn public(
        &self,
        _base: &str,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        assert_eq!(path, SPOT_EXCHANGE_INFO, "a reconcile read is signed, never public");
        let recorded: Vec<(String, String)> =
            params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        let symbol = recorded
            .iter()
            .find(|(k, _)| k == "symbol")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        self.calls.lock().unwrap().push((path.to_string(), recorded));
        if self.fail_on == Some(symbol.as_str()) {
            return Err(VenueApiError { code: self.fail_code, msg: format!("refused: {symbol}") });
        }
        let base = symbol.strip_suffix("USDT").unwrap_or(&symbol);
        Ok(json!({ "symbols": [{
            "symbol": symbol, "baseAsset": base, "quoteAsset": "USDT", "status": "TRADING",
            "filters": [{ "filterType": "LOT_SIZE", "minQty": "0.0001", "maxQty": "9000",
                          "stepSize": "0.0001" }]
        }]}))
    }
}

/// The perp lane of an account whose orders are placed under the broker link id `LINK`
/// (`x-LINK-<order client id>`): the id its stops carry at the venue.
fn perp(wire: Wire) -> ReconcileClientBinance<NullSigner, Wire> {
    // the engine admits USER names, read off the kept list (decision 0290): every test holds the same one
    vike_binance::instrument::names::kept_list_for_tests();
    ReconcileClientBinance::perp(NullSigner, wire, "https://x", "BTCUSDT")
        .with_link_id(Some("LINK".to_string()))
}

fn spot(wire: Wire) -> ReconcileClientBinance<NullSigner, Wire> {
    vike_binance::instrument::names::kept_list_for_tests();
    ReconcileClientBinance::spot(NullSigner, wire, "https://x", "BTCUSDT")
}

fn asked(calls: &Calls) -> Vec<(String, String)> {
    calls
        .lock()
        .unwrap()
        .iter()
        .map(|(path, params)| {
            let symbol = params
                .iter()
                .find(|(k, _)| k == "symbol")
                .map(|(_, v)| v.clone())
                .unwrap_or_default();
            (path.clone(), symbol)
        })
        .collect()
}

#[test]
fn with_no_admitted_symbol_the_requests_are_exactly_the_mounted_symbols() {
    let (wire, calls) = Wire::new();
    let client = perp(wire);
    client.fetch_order_status_reports(0).unwrap();
    client.fetch_position_status_reports().unwrap();
    client.fetch_fill_reports(5).unwrap();
    let p = BINANCE_RECONCILE.paths;
    let symbol = |s: &str| ("symbol".to_string(), s.to_string());
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            (p.perp_open_orders.to_string(), vec![symbol("BTCUSDT")]),
            (ALGO_LIST.to_string(), vec![symbol("BTCUSDT")]),
            (p.perp_positions.to_string(), vec![symbol("BTCUSDT")]),
            // the fill read asks its page size (owner ruling F-2: it walks every page)
            (
                p.perp_user_trades.to_string(),
                vec![
                    symbol("BTCUSDT"),
                    ("startTime".into(), "5".into()),
                    ("limit".into(), "1000".into())
                ]
            ),
            // the fills are rid of any liquidation or ADL order's (no `startTime` here: `since` is
            // within the minute the liquidation window opens early)
            (FORCE_ORDERS.to_string(), vec![symbol("BTCUSDT"), ("limit".into(), "100".into())]),
            // …and of any other venue liquidation order (a settlement one is not in forceOrders): the
            // fill's order by id, for its stored client id ("unknown order" here: not one)
            (ORDER_ONE.to_string(), vec![symbol("BTCUSDT"), ("orderId".into(), "9".into())]),
            // the fill row carries no order client id, so the Algo Service is asked which triggered stop (if any)
            // it was the child of: the latest rows, NO startTime (a stop created before the window
            // that then triggers is the one to name)
            (ALGO_HISTORY.to_string(), vec![symbol("BTCUSDT"), ("limit".into(), "1000".into())]),
            // …and a fill the history did not name is asked about by its order id: was it the child
            // of a stop the history's window did not reach? (the venue answers "unknown order" here,
            // so there is nothing to ask the algo endpoint)
            (ORDER_ONE.to_string(), vec![symbol("BTCUSDT"), ("orderId".into(), "9".into())]),
        ]
    );
}

#[test]
fn an_admitted_symbols_order_position_and_fills_are_reported_under_the_engines_spelling() {
    let (wire, calls) = Wire::new();
    let client = perp(wire);
    client.admit_symbol("ethusdt.p");

    let orders = client.fetch_order_status_reports(0).unwrap();
    let symbols: Vec<_> = orders.iter().map(|o| o.symbol.as_str()).collect();
    assert_eq!(symbols, ["BTCUSDT.P", "ETHUSDT.P"]);
    assert_eq!(orders[1].order_client_id.as_deref(), Some("c-ETHUSDT"));

    let positions = client.fetch_position_status_reports().unwrap();
    let symbols: Vec<_> = positions.iter().map(|p| p.symbol.as_str()).collect();
    assert_eq!(symbols, ["BTCUSDT.P", "ETHUSDT.P"]);

    // The mounted symbol keeps its bare trade id; an admitted one is qualified exactly as the
    // user-data stream spells it, so a fill the stream already folded is KNOWN to the diff, and two
    // symbols' per-symbol sequences never collide.
    let fills = client.fetch_fill_reports(0).unwrap();
    let ids: Vec<_> = fills.iter().map(|f| f.trade_id.as_str().to_string()).collect();
    assert_eq!(ids, ["77", "ETHUSDT.P:77"]);

    let p = BINANCE_RECONCILE.paths;
    assert_eq!(
        asked(&calls),
        [
            (p.perp_open_orders, "BTCUSDT"),
            (p.perp_open_orders, "ETHUSDT"),
            (ALGO_LIST, "BTCUSDT"),
            (ALGO_LIST, "ETHUSDT"),
            (p.perp_positions, "BTCUSDT"),
            (p.perp_positions, "ETHUSDT"),
            (p.perp_user_trades, "BTCUSDT"),
            (FORCE_ORDERS, "BTCUSDT"),
            (ORDER_ONE, "BTCUSDT"),
            (p.perp_user_trades, "ETHUSDT"),
            (FORCE_ORDERS, "ETHUSDT"),
            (ORDER_ONE, "ETHUSDT"),
            (ALGO_HISTORY, "BTCUSDT"),
            (ORDER_ONE, "BTCUSDT"),
            (ALGO_HISTORY, "ETHUSDT"),
            (ORDER_ONE, "ETHUSDT"),
        ]
        .map(|(a, b)| (a.to_string(), b.to_string()))
    );
}

/// **The client NAMES the symbols it was told** (`ReconcileClient::admitted_symbols`, which the gatewayhub puts on
/// page 0 of a list answer): a list, empty before anything is told, in the engine's spelling (`.P` on the perp
/// lane, bare on spot) and arrival order, with nothing the client ignores; and the list is the one the exec
/// thread's resync reads when the adapter shares it (`with_admitted`).
#[test]
fn the_client_names_the_symbols_it_was_told_and_shares_the_list() {
    let shared: std::sync::Arc<vike_bridge_core::reconcile_admitted::ReconcileAdmittedSymbols> =
        Default::default();
    let (wire, _calls) = Wire::new();
    let client = perp(wire).with_admitted(std::sync::Arc::clone(&shared));
    assert_eq!(client.admitted_symbols(), Some(Vec::new()), "a claim of nothing, not no claim");
    client.admit_symbol("ethusdt.p");
    client.admit_symbol("ETHUSDT"); // the SPOT spelling: not this lane's
    client.admit_symbol("BTCUSDT.P"); // the mounted symbol
    client.admit_symbol("SOLUSDT.P");
    assert_eq!(
        client.admitted_symbols(),
        Some(vec!["ETHUSDT.P".to_string(), "SOLUSDT.P".to_string()])
    );
    assert_eq!(
        shared.symbols(),
        ["ETHUSDT.P", "SOLUSDT.P"],
        "the exec thread's list is the same list"
    );

    let (wire, _calls) = Wire::new();
    let spot = spot(wire);
    spot.admit_symbol("ETHUSDT");
    spot.admit_symbol("SOLUSDT.P"); // a perp spelling: not this lane's
    assert_eq!(spot.admitted_symbols(), Some(vec!["ETHUSDT".to_string()]));
}

#[test]
fn a_symbol_nobody_admitted_is_never_queried() {
    let (wire, calls) = Wire::new();
    let client = perp(wire);
    client.admit_symbol("ETHUSDT"); // the SPOT spelling: the perp engine cannot hold it
    client.admit_symbol("btcusdt.p"); // the mounted symbol again
    client.admit_symbol("");
    client.admit_symbol("SOLUSDT.P");
    client.admit_symbol("solusdt.p"); // a repeat
    client.fetch_order_status_reports(0).unwrap();
    let p = BINANCE_RECONCILE.paths;
    assert_eq!(
        asked(&calls),
        [
            (p.perp_open_orders, "BTCUSDT"),
            (p.perp_open_orders, "SOLUSDT"),
            (ALGO_LIST, "BTCUSDT"),
            (ALGO_LIST, "SOLUSDT"),
        ]
        .map(|(a, b)| (a.to_string(), b.to_string()))
    );
}

#[test]
fn at_most_the_engines_extra_symbol_cap_is_read() {
    let (wire, calls) = Wire::new();
    let client = perp(wire);
    for i in 0..(MAX_EXTRA_SYMBOLS + 4) {
        client.admit_symbol(&format!("S{i}USDT.P"));
    }
    client.fetch_order_status_reports(0).unwrap();
    let queried = asked(&calls);
    // the mounted symbol plus sixteen, each read once for its orders and once for its algo orders
    assert_eq!(queried.len(), 2 * (1 + MAX_EXTRA_SYMBOLS));
    assert!(!queried.iter().any(|(_, s)| s == &format!("S{MAX_EXTRA_SYMBOLS}USDT")));
}

#[test]
fn a_failed_read_of_an_admitted_symbol_fails_the_whole_fetch() {
    let (mut wire, _calls) = Wire::new();
    wire.fail_on = Some("ETHUSDT");
    let client = perp(wire);
    client.admit_symbol("ETHUSDT.P");
    // Never a partial list: the restore planner reads an absent order as GONE.
    assert!(client.fetch_order_status_reports(0).is_err());
    assert!(client.fetch_position_status_reports().is_err());
    assert!(client.fetch_fill_reports(0).is_err());
}

#[test]
fn a_row_for_a_symbol_that_was_not_asked_for_is_dropped() {
    let (mut wire, _calls) = Wire::new();
    wire.stray = true;
    let client = perp(wire);
    client.admit_symbol("ETHUSDT.P");
    let orders = client.fetch_order_status_reports(0).unwrap();
    let positions = client.fetch_position_status_reports().unwrap();
    let fills = client.fetch_fill_reports(0).unwrap();
    // The MOUNTED symbol's answer is passed through untouched (byte-identical to before); the stray
    // SOLUSDT row in the ETHUSDT answer is the one that goes.
    let sol = |symbols: Vec<&str>| symbols.iter().filter(|s| **s == "SOLUSDT.P").count();
    assert_eq!(sol(orders.iter().map(|o| o.symbol.as_str()).collect()), 1, "mounted answer as is");
    assert_eq!(sol(positions.iter().map(|o| o.symbol.as_str()).collect()), 1);
    assert_eq!(sol(fills.iter().map(|o| o.symbol.as_str()).collect()), 1);
    assert_eq!(orders.iter().filter(|o| o.symbol == "ETHUSDT.P").count(), 1);
    assert_eq!(orders.len(), 3, "BTC + its stray SOL + ETH; ETH's stray SOL is gone");
}

/// The spot lane reads each admitted pair too, and reports each pair's holding as its BASE coin's wallet figure
/// (`free + locked`), a `Cash` FLOOR row with no price (the spot rule, owner 2026-11-11): one `exchangeInfo` read
/// per pair names its base coin and size step, one account read serves every pair.
#[test]
fn the_spot_lane_reads_each_admitted_symbol_and_reports_each_pairs_wallet() {
    let (wire, calls) = Wire::new();
    let client = spot(wire);
    client.admit_symbol("ETHUSDT");
    client.admit_symbol("SOLUSDT.P"); // a perp spelling: not this lane's
    let orders = client.fetch_order_status_reports(0).unwrap();
    let symbols: Vec<_> = orders.iter().map(|o| o.symbol.as_str()).collect();
    assert_eq!(symbols, ["BTCUSDT", "ETHUSDT"], "the spot lane keeps the bare label");
    calls.lock().unwrap().clear();
    let positions = client.fetch_position_status_reports().unwrap();
    let rows: Vec<_> = positions
        .iter()
        .map(|r| (r.symbol.as_str(), r.qty, r.average_price, r.margin_mode, r.position_side))
        .collect();
    let (cash, both) = (vike_model::MarginMode::Cash, vike_model::events::PositionSide::Both);
    assert_eq!(rows, [("BTCUSDT", 0.5, 0.0, cash, both), ("ETHUSDT", 2.0, 0.0, cash, both)]);
    let p = BINANCE_RECONCILE.paths;
    assert_eq!(
        asked(&calls),
        [(SPOT_EXCHANGE_INFO, "BTCUSDT"), (SPOT_EXCHANGE_INFO, "ETHUSDT"), (p.spot_account, "")]
            .map(|(a, b)| (a.to_string(), b.to_string()))
    );
    calls.lock().unwrap().clear();
    client.fetch_order_status_reports(0).unwrap();
    let fills = client.fetch_fill_reports(0).unwrap();
    let ids: Vec<_> = fills.iter().map(|f| f.trade_id.as_str().to_string()).collect();
    assert_eq!(ids, ["77", "ETHUSDT:77"]);
    assert_eq!(
        asked(&calls),
        [
            (p.spot_open_orders, "BTCUSDT"),
            (p.spot_open_orders, "ETHUSDT"),
            (p.spot_my_trades, "BTCUSDT"),
            (p.spot_my_trades, "ETHUSDT"),
            // `myTrades` names the order by id and no order client id: each order-client-id-less fill's order is read
            // (spot has no Algo Service, so that is the whole of it)
            (SPOT_ORDER_ONE, "BTCUSDT"),
            (SPOT_ORDER_ONE, "ETHUSDT"),
        ]
        .map(|(a, b)| (a.to_string(), b.to_string()))
    );
}

/// A symbol the venue no longer lists (delisted: `-1121 Invalid symbol`) must not blind the account:
/// for an ADMITTED symbol it reads as "no rows" (the truth: nothing rests on a symbol that does not
/// exist), and the mounted symbol's rows still arrive. Any other error still fails the whole fetch,
/// and the same code on the MOUNTED symbol is still an error.
#[test]
fn a_delisted_admitted_symbol_reads_as_no_rows_and_does_not_blind_the_account() {
    let (mut wire, _calls) = Wire::new();
    wire.fail_on = Some("ETHUSDT");
    wire.fail_code = -1121;
    let client = perp(wire);
    client.admit_symbol("ETHUSDT.P");
    let orders = client.fetch_order_status_reports(0).unwrap();
    assert_eq!(orders.iter().map(|o| o.symbol.as_str()).collect::<Vec<_>>(), ["BTCUSDT.P"]);
    assert_eq!(client.fetch_position_status_reports().unwrap().len(), 1);
    assert_eq!(client.fetch_fill_reports(0).unwrap().len(), 1);

    let (mut wire, _calls) = Wire::new();
    wire.fail_on = Some("BTCUSDT");
    wire.fail_code = -1121;
    assert!(perp(wire).fetch_order_status_reports(0).is_err(), "the mounted symbol is not excused");
}

/// A rate-limit answer (`-1003`, or the HTTP status the transport keeps as the code: 429, 418) on an
/// admitted read backs the whole pass off: the next fetches make NO request until the pause lapses, so
/// the extra reads cannot walk the account into a ban. With nothing admitted there is no backoff, and the
/// mounted symbol's request goes out every time, as it always did.
#[test]
fn a_rate_limit_answer_backs_the_pass_off_and_only_when_symbols_are_admitted() {
    for code in [-1003, 429, 418] {
        let (mut wire, calls) = Wire::new();
        wire.fail_on = Some("ETHUSDT");
        wire.fail_code = code;
        let client = perp(wire);
        client.admit_symbol("ETHUSDT.P");
        assert!(client.fetch_order_status_reports(0).is_err(), "{code}");
        let before = calls.lock().unwrap().len();
        let err = client.fetch_position_status_reports().unwrap_err();
        assert!(err.contains("backing off"), "{code}: {err}");
        assert_eq!(calls.lock().unwrap().len(), before, "{code}: a backed-off pass sends nothing");
    }
    let (mut wire, calls) = Wire::new();
    wire.fail_on = Some("BTCUSDT");
    wire.fail_code = 429;
    let client = perp(wire);
    assert!(client.fetch_order_status_reports(0).is_err());
    assert!(client.fetch_order_status_reports(0).is_err());
    assert_eq!(calls.lock().unwrap().len(), 2, "nothing admitted: no backoff, the request repeats");
}

/// **The fill of a triggered stop is named from the Algo Service's history** (the restart-safe-orders
/// audit's G2). `userTrades` reports the CHILD order a trigger placed, with no order client id; `allAlgoOrders`
/// says which stop it was the child of, and the fill comes out carrying the stop's bare order client id. A fill
/// that is no stop's child stays order-client-id-less (the restore joins it by its venue order id), and the read
/// is made for the symbols that have an order-client-id-less fill, for the latest rows and with NO `startTime`: a
/// stop created before the pass's window that then triggers must still be named.
#[test]
fn a_triggered_stops_fill_is_named_from_the_algo_history() {
    let (mut wire, calls) = Wire::new();
    // `fill_row`'s order id is 9: the stop whose trigger placed order 9 was `stop-1`
    wire.children = vec![("BTCUSDT", "9", "stop-1"), ("BTCUSDT", "123", "another-stop")];
    let client = perp(wire);
    let fills = client.fetch_fill_reports(5).unwrap();
    assert_eq!(fills.len(), 1);
    assert_eq!(fills[0].order_client_id.as_deref(), Some("stop-1"), "named from the history");
    assert_eq!(&*fills[0].venue_order_id, "9", "its venue id is untouched");
    let history: Vec<_> = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(path, _)| path == ALGO_HISTORY)
        .map(|(_, params)| params.clone())
        .collect();
    assert_eq!(
        history,
        vec![vec![("symbol".to_string(), "BTCUSDT".to_string()), ("limit".into(), "1000".into())]]
    );

    // not a stop's child: nothing to name
    let (mut wire, _calls) = Wire::new();
    wire.children = vec![("BTCUSDT", "123", "another-stop")];
    let fills = perp(wire).fetch_fill_reports(0).unwrap();
    assert_eq!(fills[0].order_client_id, None);
}

/// The history read costs a request only when a fill needs it, and it is the PERP lane's alone: an
/// empty fill list asks nothing, and the spot lane (no Algo Service) never asks.
#[test]
fn the_algo_history_is_read_only_for_a_perp_fill_that_carries_no_order_client_id() {
    let (wire, calls) = Wire::new();
    let client = perp(wire);
    client.fetch_position_status_reports().unwrap();
    assert!(
        !calls.lock().unwrap().iter().any(|(path, _)| path == ALGO_HISTORY),
        "no fill read, no history read"
    );

    let (wire, calls) = Wire::new();
    spot(wire).fetch_fill_reports(0).unwrap();
    assert!(!calls.lock().unwrap().iter().any(|(path, _)| path == ALGO_HISTORY));
}

/// A history that cannot be read fails the whole fill fetch, as the open-list read fails the order
/// report: a fill list that silently lacked the names would let the restore planner read a filled stop
/// as one the venue stopped listing. While the by-id order read is down TOO, the perpetual's fills
/// cannot be cleared of venue liquidation orders either, and that check is never forgiven, so every
/// pass fails (the naming's own "goes ahead unnamed after a few passes" is reached only when the
/// order read answers: `a_failed_history_with_fill_orders_left_over_is_still_an_error` and the spot
/// lane's `plain_fill_naming.rs`). One good read makes it strict again.
#[test]
fn an_unreadable_algo_history_and_order_read_fail_the_fill_fetch_on_every_pass() {
    let (mut wire, _calls) = Wire::new();
    wire.children = vec![("BTCUSDT", "9", "stop-1")];
    // both ways of naming are down: the history and the by-id reads
    let down = [Arc::clone(&wire.fail_history), Arc::clone(&wire.fail_order_lookup)];
    let set = |up: bool| down.iter().for_each(|d| d.store(!up, Ordering::Relaxed));
    set(false);
    let client = perp(wire);
    for pass in 1..=6 {
        assert!(client.fetch_fill_reports(0).is_err(), "pass {pass}: strict, never forgiven");
    }

    set(true);
    let fills = client.fetch_fill_reports(0).unwrap();
    assert_eq!(fills[0].order_client_id.as_deref(), Some("stop-1"), "named again");
    set(false);
    assert!(client.fetch_fill_reports(0).is_err(), "a good read made it strict again");
}

/// **"Three failing passes in a row" means since the LAST GOOD READ**, and a pass in which nothing
/// traded is a good read (there was nothing to name): three single failures on separate days, with
/// quiet passes between, must not make the fourth go ahead unnamed.
#[test]
fn failures_are_counted_since_the_last_good_read_and_a_pass_with_no_fills_is_one() {
    let (wire, _calls) = Wire::new();
    let down = [Arc::clone(&wire.fail_history), Arc::clone(&wire.fail_order_lookup)];
    let no_fills = Arc::clone(&wire.no_fills);
    let client = perp(wire);
    for day in 1..=4 {
        down.iter().for_each(|d| d.store(true, Ordering::Relaxed));
        no_fills.store(false, Ordering::Relaxed);
        assert!(client.fetch_fill_reports(0).is_err(), "day {day}: a single failure is strict");
        // the same day, later: nothing traded, so there is nothing to name and nothing to fail
        no_fills.store(true, Ordering::Relaxed);
        assert_eq!(client.fetch_fill_reports(0).unwrap().len(), 0, "day {day}: a quiet pass");
    }
}

/// **A stop older than the history's window is named by its child order.** `allAlgoOrders` does not
/// return a stop last updated before its default window (`algo_old_stop.rs` replays the measured
/// rules), so the fill of the stop's child order stays order-client-id-less after the history; the child is then
/// asked about by id: the
/// order's stored `clientOrderId` is the stop's `clientAlgoId`, and the algo endpoint says it placed
/// that very order. The answer is remembered, so the second pass asks nothing.
#[test]
fn a_stop_older_than_the_histories_window_is_named_by_its_child_order() {
    let (mut wire, calls) = Wire::new();
    wire.orders = vec![("BTCUSDT", "9", "x-LINK-stop1")];
    wire.algo_one = vec![("x-LINK-stop1", "9", "BTCUSDT")]; // the history (`children`) knows nothing
    let client = perp(wire);

    let fills = client.fetch_fill_reports(5).unwrap();
    assert_eq!(
        fills[0].order_client_id.as_deref(),
        Some("stop1"),
        "the bare order client id of the stop"
    );
    assert_eq!(&*fills[0].venue_order_id, "9", "its venue id is untouched");
    let symbol = |s: &str| ("symbol".to_string(), s.to_string());
    let by_id: Vec<_> = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(path, _)| path == ORDER_ONE || path == ALGO_ONE)
        .cloned()
        .collect();
    assert_eq!(
        by_id,
        vec![
            // the liquidation check's read of the fill order (the family's, remembered on its own)
            (ORDER_ONE.to_string(), vec![symbol("BTCUSDT"), ("orderId".into(), "9".into())]),
            (ORDER_ONE.to_string(), vec![symbol("BTCUSDT"), ("orderId".into(), "9".into())]),
            (ALGO_ONE.to_string(), vec![("clientAlgoId".into(), "x-LINK-stop1".into())]),
        ],
        "one read of the child, one of the stop it names (as the venue stored it)"
    );

    let again = client.fetch_fill_reports(5).unwrap();
    assert_eq!(again[0].order_client_id.as_deref(), Some("stop1"), "named from memory");
    let reads = |calls: &Calls| {
        calls.lock().unwrap().iter().filter(|(p, _)| p == ORDER_ONE || p == ALGO_ONE).count()
    };
    assert_eq!(reads(&calls), 3, "nothing was asked twice");
}

/// A plain order's fill is asked about ONCE and is named with the order's own order client id: the venue knows
/// no algo order of that `clientOrderId`, so the order is no stop's child and its stored id (ours)
/// is the answer (`plain_fill_naming.rs` has the cases of that rule). And an algo order that placed
/// ANOTHER order does not name this fill.
#[test]
fn a_plain_orders_fill_is_named_with_its_own_order_client_id_and_asked_about_once() {
    let (mut wire, calls) = Wire::new();
    wire.orders = vec![("BTCUSDT", "9", "x-LINK-plain1")]; // no algo order of that id: -2013
    let client = perp(wire);
    assert_eq!(client.fetch_fill_reports(0).unwrap()[0].order_client_id.as_deref(), Some("plain1"));
    assert_eq!(client.fetch_fill_reports(0).unwrap()[0].order_client_id.as_deref(), Some("plain1"));
    let asked_algo = calls.lock().unwrap().iter().filter(|(p, _)| p == ALGO_ONE).count();
    let asked_order = calls.lock().unwrap().iter().filter(|(p, _)| p == ORDER_ONE).count();
    // the order twice in all: the liquidation check's read and the naming's, each remembered
    assert_eq!((asked_order, asked_algo), (2, 1), "remembered: the second pass asks nothing");

    let (mut wire, _calls) = Wire::new();
    wire.orders = vec![("BTCUSDT", "9", "x-LINK-stop1")];
    wire.algo_one = vec![("x-LINK-stop1", "77", "BTCUSDT")]; // placed order 77, not this fill's 9
    assert_eq!(perp(wire).fetch_fill_reports(0).unwrap()[0].order_client_id, None);
}

/// The by-id reads are bounded per pass (`ALGO_CHILD_LOOKUPS_PER_PASS` fill orders): a pass that meets
/// more waits for the next for the rest, without failing, and nothing is asked twice.
#[test]
fn fill_orders_beyond_the_per_pass_budget_wait_for_the_next_pass() {
    use vike_binance::execution::rest::algo::ALGO_CHILD_LOOKUPS_PER_PASS;
    let (mut wire, calls) = Wire::new();
    // the mounted row's order 9, plus enough more to pass the budget
    wire.more_fill_orders = (100..100 + ALGO_CHILD_LOOKUPS_PER_PASS as u64 + 5).collect();
    let client = perp(wire);
    let order_reads =
        |calls: &Calls| calls.lock().unwrap().iter().filter(|(p, _)| p == ORDER_ONE).count();

    // the liquidation check reads every one of the 31 fill orders once (it has no budget: it is
    // strict), on top of the naming's reads
    let orders = ALGO_CHILD_LOOKUPS_PER_PASS + 6;
    client.fetch_fill_reports(0).expect("a capped pass is not a failed one");
    assert_eq!(order_reads(&calls), orders + ALGO_CHILD_LOOKUPS_PER_PASS, "the budget, no more");
    client.fetch_fill_reports(0).unwrap();
    assert_eq!(
        order_reads(&calls),
        orders + ALGO_CHILD_LOOKUPS_PER_PASS + 6,
        "the next pass asks the remaining six (the 25 before are remembered)"
    );
}

/// A by-id read that FAILS (not "unknown order") is an error, never "not a stop's child": the fill
/// fetch fails like the history's does, so a stop's fill cannot be read as a plain order's by a venue
/// that merely did not answer.
#[test]
fn a_failed_lookup_by_id_is_an_error_not_a_no() {
    let (wire, _calls) = Wire::new();
    wire.fail_order_lookup.store(true, Ordering::Relaxed);
    assert!(perp(wire).fetch_fill_reports(0).is_err(), "unanswered is not 'no'");
}

/// A failed HISTORY is forgiven when every fill order was resolved by id (nothing the history would
/// have named is left), and the by-id answer names the fill by itself.
#[test]
fn a_failed_history_is_forgiven_when_every_fill_order_was_resolved_by_id() {
    let (mut wire, _calls) = Wire::new();
    wire.orders = vec![("BTCUSDT", "9", "x-LINK-stop1")];
    wire.algo_one = vec![("x-LINK-stop1", "9", "BTCUSDT")];
    wire.fail_history.store(true, Ordering::Relaxed);
    let client = perp(wire);
    for pass in 1..=6 {
        let fills = client
            .fetch_fill_reports(0)
            .unwrap_or_else(|e| panic!("pass {pass}: the history is down but by id named it: {e}"));
        assert_eq!(fills[0].order_client_id.as_deref(), Some("stop1"), "pass {pass}");
    }
}

/// Forgiving a failed history says so: ONE warning an hour, on its own clock, naming the REST
/// spelling it rests on. Three forgiven passes inside the hour make one warning.
#[test]
fn forgiving_a_failed_history_warns_once_an_hour() {
    let (mut wire, _calls) = Wire::new();
    wire.orders = vec![("BTCUSDT", "9", "x-LINK-stop1")];
    wire.algo_one = vec![("x-LINK-stop1", "9", "BTCUSDT")];
    wire.fail_history.store(true, Ordering::Relaxed);
    let client = perp(wire);
    let ((), events) = vike_log::capture::captured(|| {
        for _ in 0..3 {
            client.fetch_fill_reports(0).unwrap();
        }
    });
    let warned = events
        .iter()
        .filter(|e| {
            e.level == tracing::Level::WARN
                && e.message.contains("relies on the child order's clientOrderId")
        })
        .count();
    assert_eq!(warned, 1, "forgiven three times, warned once: {events:?}");
}

/// A history that is down while fill orders are LEFT OVER (past the per-pass budget) is not forgiven:
/// the history might have named the ones the budget did not reach, so the pass is an error.
#[test]
fn a_failed_history_with_fill_orders_left_over_is_still_an_error() {
    use vike_binance::execution::rest::algo::ALGO_CHILD_LOOKUPS_PER_PASS;
    let (mut wire, _calls) = Wire::new();
    wire.more_fill_orders = (100..100 + ALGO_CHILD_LOOKUPS_PER_PASS as u64 + 5).collect();
    wire.fail_history.store(true, Ordering::Relaxed);
    let client = perp(wire);
    assert!(client.fetch_fill_reports(0).is_err(), "left over and the history down: an error");
}

/// A `GET algoOrder` that fails with anything but "unknown order" is an error, never "no": a venue
/// that did not answer must not turn a stop's child into a plain order's fill.
#[test]
fn a_failed_algo_lookup_is_an_error_not_a_no() {
    let (mut wire, _calls) = Wire::new();
    wire.orders = vec![("BTCUSDT", "9", "x-LINK-stop1")];
    wire.fail_algo_lookup.store(true, Ordering::Relaxed);
    assert!(perp(wire).fetch_fill_reports(0).is_err(), "the algo endpoint did not answer");
}

/// **A stop placed by hand is not named** (nor is a stop of another symbol's or another account's
/// id): only an id of OURS names a fill, because an order-client-id-linked fill of an order the node never minted
/// is worse than an unnamed one. A foreign id costs one read of the child and none of the algo
/// endpoint; a stop of OUR id on ANOTHER symbol is read and refused.
#[test]
fn a_stop_that_is_not_ours_does_not_name_a_fill() {
    for foreign in ["web_ab12cd34", "x-OTHER-stop1", "android_9Zx"] {
        let (mut wire, calls) = Wire::new();
        wire.orders = vec![("BTCUSDT", "9", foreign)];
        wire.algo_one = vec![(foreign, "9", "BTCUSDT")]; // a real stop, which triggered this child
        let fills = perp(wire).fetch_fill_reports(0).unwrap();
        assert_eq!(fills[0].order_client_id, None, "{foreign}: not our id, so not named");
        let algo_reads = calls.lock().unwrap().iter().filter(|(p, _)| p == ALGO_ONE).count();
        assert_eq!(algo_reads, 0, "{foreign}: the algo endpoint is not even asked");
    }

    // without the account's link id no `x-` id is ours
    let (mut wire, _calls) = Wire::new();
    wire.orders = vec![("BTCUSDT", "9", "x-LINK-stop1")];
    wire.algo_one = vec![("x-LINK-stop1", "9", "BTCUSDT")];
    let unlinked = ReconcileClientBinance::perp(NullSigner, wire, "https://x", "BTCUSDT");
    assert_eq!(unlinked.fetch_fill_reports(0).unwrap()[0].order_client_id, None);

    // our own id, but the stop is on another symbol
    let (mut wire, _calls) = Wire::new();
    wire.orders = vec![("BTCUSDT", "9", "x-LINK-stop1")];
    wire.algo_one = vec![("x-LINK-stop1", "9", "ETHUSDT")];
    assert_eq!(perp(wire).fetch_fill_reports(0).unwrap()[0].order_client_id, None);

    // …and the same stop on the fill's own symbol, with a bare order client id of ours, IS named
    let (mut wire, _calls) = Wire::new();
    wire.orders = vec![("BTCUSDT", "9", "abcd1234stop1")];
    wire.algo_one = vec![("abcd1234stop1", "9", "BTCUSDT")];
    let named = perp(wire).fetch_fill_reports(0).unwrap();
    assert_eq!(named[0].order_client_id.as_deref(), Some("abcd1234stop1"));
}

/// A stop that exists but does not yet say which order it placed is not an answer: it is not
/// remembered as "no", so the pass that sees it again names the fill.
#[test]
fn a_stop_that_has_not_said_what_it_placed_is_asked_again() {
    let (mut wire, calls) = Wire::new();
    wire.orders = vec![("BTCUSDT", "9", "x-LINK-stop1")];
    wire.algo_one = vec![("x-LINK-stop1", "", "BTCUSDT")]; // the algo record lags the child
    let client = perp(wire);
    assert_eq!(client.fetch_fill_reports(0).unwrap()[0].order_client_id, None, "not yet");
    assert_eq!(client.fetch_fill_reports(0).unwrap()[0].order_client_id, None);
    let order_reads = calls.lock().unwrap().iter().filter(|(p, _)| p == ORDER_ONE).count();
    // the liquidation check's one read, then the naming's on each pass
    assert_eq!(order_reads, 3, "asked again, not remembered as a no");
}

/// **A persistent error on one symbol does not blind the others.** The first symbol's by-id read
/// fails every pass after the liquidation check's first one (remembered, so asked once); the second
/// symbol's stop is still named (on the pass that goes ahead after the strict ones), because the
/// failure is recorded and the pass moves on.
#[test]
fn an_error_on_one_symbol_does_not_stop_the_other_symbols_being_named() {
    let (mut wire, _calls) = Wire::new();
    wire.fail_order_symbol = Some("BTCUSDT");
    wire.orders = vec![("ETHUSDT", "9", "x-LINK-stop1")];
    wire.algo_one = vec![("x-LINK-stop1", "9", "ETHUSDT")];
    let client = perp(wire);
    client.admit_symbol("ETHUSDT.P");
    for pass in 1..=3 {
        assert!(client.fetch_fill_reports(0).is_err(), "pass {pass}: strict");
    }
    let fills = client.fetch_fill_reports(0).expect("the fourth failure goes ahead");
    let by_symbol: Vec<_> =
        fills.iter().map(|f| (f.symbol.as_str(), f.order_client_id.as_deref())).collect();
    assert_eq!(
        by_symbol,
        [("BTCUSDT.P", None), ("ETHUSDT.P", Some("stop1"))],
        "the failing symbol stays unnamed, the other is named"
    );
}

/// The id an admitted symbol's fill carries is the one the user-data stream's mapper produces for the
/// same fill (not a literal restated here), so a fill the stream already folded is known to the diff.
#[test]
fn an_admitted_symbols_fill_id_is_the_one_the_stream_mapper_spells() {
    use vike_binance::execution::event_mapper::perp::map_binance_perp;
    use vike_binance::execution::event_mapper::spot::map_execution_report;
    use vike_model::events::Event;
    let fill_id = |evs: Vec<Event>| {
        evs.into_iter()
            .find_map(|e| match e {
                Event::Fill(f) => Some(f.trade_id.as_str().to_string()),
                _ => None,
            })
            .expect("the frame folds a fill")
    };
    let perp_frame = json!({ "e": "ORDER_TRADE_UPDATE", "T": 5, "o": {
        "s": "ETHUSDT", "c": "c1", "x": "TRADE", "X": "FILLED", "S": "BUY", "l": "1.0",
        "L": "50000", "n": "0.1", "N": "USDT", "m": false, "ps": "BOTH", "t": 77 } });
    let streamed = fill_id(map_binance_perp(&perp_frame, "binance", "BTCUSDT.P", None));
    let (wire, _calls) = Wire::new();
    let client = perp(wire);
    client.admit_symbol("ETHUSDT.P");
    let reported = client.fetch_fill_reports(0).unwrap();
    assert_eq!(reported[1].trade_id.as_str(), streamed);

    let spot_frame = json!({ "e": "executionReport", "s": "ETHUSDT", "c": "c1", "x": "TRADE",
        "X": "FILLED", "S": "BUY", "l": "1.0", "L": "50000", "n": "0.1", "N": "USDT", "m": false,
        "T": 5, "t": 77 });
    let streamed = fill_id(map_execution_report(&spot_frame, "binance", "BTCUSDT", None));
    let (wire, _calls) = Wire::new();
    let client = spot(wire);
    client.admit_symbol("ETHUSDT");
    let reported = client.fetch_fill_reports(0).unwrap();
    assert_eq!(reported[1].trade_id.as_str(), streamed);
}

/// **A trade of a liquidation or ADL order is never reported as a fill** (owner, 2026-11-09:
/// liquidation trades are listed in userTrades). The stream books it as a `PositionLiquidated`, which
/// the fill diff never sees, so a fill report of it would be a `MissingFill` that `hybrid` books: the
/// position closed twice. `forceOrders` names those orders (the ids are the venue's documented
/// example rows), for the mounted symbol and every admitted one alike, and its window opens a minute
/// before the fills'.
#[test]
fn a_liquidation_or_adl_orders_trade_is_never_reported_as_a_fill() {
    let (mut wire, calls) = Wire::new();
    wire.more_fill_orders = vec![6_071_832_819, 6_072_734_303];
    wire.forced = vec![6_071_832_819, 6_072_734_303];
    let client = perp(wire);
    client.admit_symbol("ETHUSDT.P");
    let fills = client.fetch_fill_reports(1_596_107_620_000).unwrap();
    let kept: Vec<_> = fills.iter().map(|f| (f.symbol.as_str(), &*f.venue_order_id)).collect();
    assert_eq!(kept, [("BTCUSDT.P", "9"), ("ETHUSDT.P", "9")], "only the plain order's fills");
    let asked: Vec<_> =
        calls.lock().unwrap().iter().filter(|(p, _)| p == FORCE_ORDERS).cloned().collect();
    let params = |s: &str| {
        vec![
            ("symbol".to_string(), s.to_string()),
            ("limit".into(), "100".into()),
            ("startTime".into(), "1596107560000".into()),
        ]
    };
    assert_eq!(
        asked,
        [
            (FORCE_ORDERS.to_string(), params("BTCUSDT")),
            (FORCE_ORDERS.to_string(), params("ETHUSDT"))
        ]
    );
}

/// **A trade of a SETTLEMENT order (delisting, delivery) is never reported as a fill either**, nor one
/// of any order whose stored client id carries a venue liquidation prefix: `forceOrders` lists only
/// liquidation and ADL orders, so every other fill order is read by id (once: the answer is
/// remembered) and judged by the stream mapper's own prefix list. A foreign PLAIN order's trade (an
/// order placed by hand) is kept, and so is one whose order the venue says it does not have
/// (`-2013`: by the venue's docs an order with a trade is found unless it is 90 days old, and a
/// settlement order is made at its trade).
#[test]
fn a_settlement_orders_trade_is_never_reported_and_a_foreign_plain_orders_is() {
    for stored in ["settlement_autoclose-1", "autoclose-2", "adl_autoclose"] {
        let (mut wire, calls) = Wire::new();
        wire.orders = vec![("BTCUSDT", "9", stored)];
        let client = perp(wire);
        assert_eq!(client.fetch_fill_reports(0).unwrap(), Vec::new(), "{stored}: dropped");
        assert_eq!(client.fetch_fill_reports(0).unwrap(), Vec::new(), "{stored}: again");
        let reads = calls.lock().unwrap().iter().filter(|(p, _)| p == ORDER_ONE).count();
        assert_eq!(reads, 1, "{stored}: the order is read once, and the naming never asks");
    }

    // a foreign plain order (the web UI's id), and an order the venue says it does not have
    let (mut wire, _calls) = Wire::new();
    wire.orders = vec![("BTCUSDT", "9", "web_ab12cd34")];
    wire.more_fill_orders = vec![10];
    let kept = perp(wire).fetch_fill_reports(0).unwrap();
    let orders: Vec<_> =
        kept.iter().map(|f| (&*f.venue_order_id, f.order_client_id.clone())).collect();
    assert_eq!(orders, [("9", None), ("10", None)], "kept, and not named: neither is ours");
}

/// **The by-id reads are capped at 100 a pass** (the IP weight is shared with every other read), and a
/// pending check never fails the read: 250 fill orders nobody has asked about take three passes, each
/// reporting ONLY the fills whose orders are checked (100, 200, then all 250) and holding the rest
/// back with one INFO line, never an unchecked row. The naming runs on the checked fills every pass
/// (the triggered stop's fill of order 9 is named on the first).
#[test]
fn two_hundred_and_fifty_unknown_orders_take_three_passes_reporting_the_checked_fills() {
    use vike_binance::execution::rest::algo::ALGO_CHILD_LOOKUPS_PER_PASS as NAMING;
    let (mut wire, calls) = Wire::new();
    wire.more_fill_orders = (10_000..10_249).collect(); // with `fill_row`'s order 9: 250 orders
    wire.children = vec![("BTCUSDT", "9", "stop-1")];
    let client = perp(wire);
    let order_reads =
        |calls: &Calls| calls.lock().unwrap().iter().filter(|(p, _)| p == ORDER_ONE).count();
    let held = |events: &[vike_log::capture::CapturedEvent]| -> Vec<String> {
        events
            .iter()
            .filter(|e| {
                e.level == tracing::Level::INFO && e.message.contains("still being checked")
            })
            .map(|e| e.message.clone())
            .collect()
    };

    let (first, events) = vike_log::capture::captured(|| client.fetch_fill_reports(0).unwrap());
    assert_eq!(first.len(), 100, "the fills of the 100 orders the cap checked");
    assert_eq!(first[0].order_client_id.as_deref(), Some("stop-1"), "named on the checked subset");
    let orders: Vec<&str> = first.iter().map(|f| &*f.venue_order_id).collect();
    assert_eq!(orders[1..], (10_000..10_099).map(|o| o.to_string()).collect::<Vec<_>>());
    let lines = held(&events);
    assert!(
        lines.len() == 1
            && lines[0].contains("150 orders still being checked; their fills are held back"),
        "one INFO line a pass: {lines:?}"
    );
    assert_eq!(order_reads(&calls), 100 + NAMING, "the cap, then the naming's own reads");

    let (second, events) = vike_log::capture::captured(|| client.fetch_fill_reports(0).unwrap());
    assert_eq!(second.len(), 200, "the next 100 checked, none asked twice");
    assert!(held(&events).len() == 1 && held(&events)[0].contains("50 orders still being checked"));
    assert_eq!(order_reads(&calls), 200 + 2 * NAMING);

    let (third, events) = vike_log::capture::captured(|| client.fetch_fill_reports(0).unwrap());
    assert_eq!(third.len(), 250, "every fill, once all 250 orders are checked");
    assert!(held(&events).is_empty(), "nothing held back");
    assert_eq!(order_reads(&calls), 250 + 3 * NAMING, "the last 50");
}

/// A by-id read that FAILS (not "no such order") fails the fill read: the fill cannot be cleared of
/// a settlement order, so it is never reported unfiltered.
#[test]
fn a_failed_order_lookup_fails_the_fill_read() {
    let (wire, calls) = Wire::new();
    wire.fail_order_lookup.store(true, Ordering::Relaxed);
    let err = perp(wire).fetch_fill_reports(0).unwrap_err();
    assert!(err.contains("order lookup refused"), "{err}");
    assert!(
        !calls.lock().unwrap().iter().any(|(p, _)| p == ALGO_HISTORY),
        "it fails before the naming reads anything"
    );
}

/// The fills are never reported UNFILTERED: a `forceOrders` read that fails or answers a full page (the
/// list may be cut) fails the fill read; a pass with no fills asks nothing.
#[test]
fn a_force_orders_read_that_cannot_be_trusted_fails_the_fill_read() {
    let (mut wire, _calls) = Wire::new();
    wire.fail_forced = true;
    assert!(perp(wire).fetch_fill_reports(0).is_err(), "a failed read");

    let (mut wire, _calls) = Wire::new();
    wire.forced = (1..=100).collect();
    let err = perp(wire).fetch_fill_reports(0).unwrap_err();
    assert!(err.contains("may be cut"), "a full page: {err}");

    let (mut wire, _calls) = Wire::new();
    wire.forced = (1_000..1_099).collect(); // not `fill_row`'s order 9
    assert_eq!(perp(wire).fetch_fill_reports(0).unwrap().len(), 1, "99 rows are the whole list");

    let (mut wire, calls) = Wire::new();
    wire.no_fills.store(true, Ordering::Relaxed);
    wire.forced = vec![9];
    let client = perp(wire);
    assert!(client.fetch_fill_reports(0).unwrap().is_empty());
    assert!(!calls.lock().unwrap().iter().any(|(p, _)| p == FORCE_ORDERS), "no fills, no read");
}
