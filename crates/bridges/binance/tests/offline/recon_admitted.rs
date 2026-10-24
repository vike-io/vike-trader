//! **The reconcile client reads every ADMITTED symbol of the account, not only the mounted one**
//! (offline: a recording `RestTransport`, canned bodies, no network, nothing `#[ignore]`d).
//!
//! The core's reconcile driver asks a `ReconClient` three questions with no symbol in them
//! (`fetch_order_status_reports`, `fetch_position_status_reports`, `fetch_fill_reports`) and the
//! restore planner reads an order the venue's report does not carry as GONE. So a client that read the
//! mounted symbol alone made every resting order on a symbol admitted later invisible after a restart.
//! The client is told which symbols the engine admitted (`ReconClient::admit_symbol`) and reads each
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
use vike_binance::BinanceReconClient;
use vike_binance::recon_client::BINANCE_RECON;
use vike_bridge_core::signer::{PreparedRequest, Signer};
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_exec::MAX_EXTRA_SYMBOLS;
use vike_exec::recon::ReconClient;

type Call = (String, Vec<(String, String)>);
type Calls = Arc<Mutex<Vec<Call>>>;

/// The Algo Service's open-order list: on the PERP lane every order report is followed by this read
/// for the same symbols, because `openOrders` is not documented to list a conditional order.
const ALGO_LIST: &str = vike_binance::algo::PATH_OPEN_ALGO_ORDERS;

/// The Algo Service's order history: on the PERP lane a fill read is followed by this read for every
/// symbol that has a coid-less fill, to name the fills of triggered stops (`algo::name_algo_children`).
const ALGO_HISTORY: &str = vike_binance::algo::PATH_ALL_ALGO_ORDERS;

/// One order by id (`GET /fapi/v1/order`) and one algo order by `clientAlgoId` (`GET
/// /fapi/v1/algoOrder`): the two reads that name a coid-less fill whose stop the history did not list
/// (a stop older than the history's window), asked once per fill order and remembered.
const ORDER_ONE: &str = vike_binance::perp::PATH_ORDER;
const ALGO_ONE: &str = vike_binance::algo::PATH_ALGO_ORDER;

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
    /// Triggered stops the Algo Service history reports: `(symbol, child order id, bare coid)`.
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
    /// Make `GET /fapi/v1/order` fail for this one symbol only (a persistent error on one symbol).
    fail_order_symbol: Option<&'static str>,
    /// Make `GET /fapi/v1/algoOrder` fail with a code that is not "unknown order".
    fail_algo_lookup: Arc<AtomicBool>,
    /// Answer the fills read with no rows at all (a pass in which nothing traded).
    no_fills: Arc<AtomicBool>,
    /// More coid-less fills, each of its own order: the ids (the `fill_row` of `symbol` is still first).
    more_fill_orders: Vec<u64>,
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
            fail_algo_lookup: Arc::default(),
            no_fills: Arc::default(),
            more_fill_orders: Vec::new(),
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
                .map(|(s, child, coid)| {
                    json!({ "algoId": 55, "clientAlgoId": coid, "symbol": s,
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
        let unknown = || VenueApiError { code: -2013, msg: "Order does not exist.".to_string() };
        if path == ORDER_ONE {
            if self.fail_order_lookup.load(Ordering::Relaxed)
                || self.fail_order_symbol == Some(symbol.as_str())
            {
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
        let p = BINANCE_RECON.paths;
        if path == p.perp_user_trades && self.no_fills.load(Ordering::Relaxed) {
            return Ok(Value::Array(Vec::new()));
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

    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        panic!("a reconcile read is signed, never public ({path})");
    }
}

/// The perp lane of an account whose orders are placed under the broker link id `LINK`
/// (`x-LINK-<coid>`): the id its stops carry at the venue.
fn perp(wire: Wire) -> BinanceReconClient<NullSigner, Wire> {
    BinanceReconClient::perp(NullSigner, wire, "https://x", "BTCUSDT")
        .with_link_id(Some("LINK".to_string()))
}

fn spot(wire: Wire) -> BinanceReconClient<NullSigner, Wire> {
    BinanceReconClient::spot(NullSigner, wire, "https://x", "BTCUSDT")
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
    let p = BINANCE_RECON.paths;
    let sym = |s: &str| ("symbol".to_string(), s.to_string());
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            (p.perp_open_orders.to_string(), vec![sym("BTCUSDT")]),
            (ALGO_LIST.to_string(), vec![sym("BTCUSDT")]),
            (p.perp_positions.to_string(), vec![sym("BTCUSDT")]),
            (
                p.perp_user_trades.to_string(),
                vec![sym("BTCUSDT"), ("startTime".into(), "5".into())]
            ),
            // the fill row carries no coid, so the Algo Service is asked which triggered stop (if any)
            // it was the child of: the latest rows, NO startTime (a stop created before the window
            // that then triggers is the one to name)
            (ALGO_HISTORY.to_string(), vec![sym("BTCUSDT"), ("limit".into(), "1000".into())]),
            // …and a fill the history did not name is asked about by its order id: was it the child
            // of a stop the history's window did not reach? (the venue answers "unknown order" here,
            // so there is nothing to ask the algo endpoint)
            (ORDER_ONE.to_string(), vec![sym("BTCUSDT"), ("orderId".into(), "9".into())]),
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
    assert_eq!(orders[1].client_order_id.as_deref(), Some("c-ETHUSDT"));

    let positions = client.fetch_position_status_reports().unwrap();
    let symbols: Vec<_> = positions.iter().map(|p| p.symbol.as_str()).collect();
    assert_eq!(symbols, ["BTCUSDT.P", "ETHUSDT.P"]);

    // The mounted symbol keeps its bare trade id; an admitted one is qualified exactly as the
    // user-data stream spells it, so a fill the stream already folded is KNOWN to the diff, and two
    // symbols' per-symbol sequences never collide.
    let fills = client.fetch_fill_reports(0).unwrap();
    let ids: Vec<_> = fills.iter().map(|f| f.trade_id.as_str().to_string()).collect();
    assert_eq!(ids, ["77", "ETHUSDT.P:77"]);

    let p = BINANCE_RECON.paths;
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
            (p.perp_user_trades, "ETHUSDT"),
            (ALGO_HISTORY, "BTCUSDT"),
            (ORDER_ONE, "BTCUSDT"),
            (ALGO_HISTORY, "ETHUSDT"),
            (ORDER_ONE, "ETHUSDT"),
        ]
        .map(|(a, b)| (a.to_string(), b.to_string()))
    );
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
    let p = BINANCE_RECON.paths;
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
    let sol = |syms: Vec<&str>| syms.iter().filter(|s| **s == "SOLUSDT.P").count();
    assert_eq!(sol(orders.iter().map(|o| o.symbol.as_str()).collect()), 1, "mounted answer as is");
    assert_eq!(sol(positions.iter().map(|o| o.symbol.as_str()).collect()), 1);
    assert_eq!(sol(fills.iter().map(|o| o.symbol.as_str()).collect()), 1);
    assert_eq!(orders.iter().filter(|o| o.symbol == "ETHUSDT.P").count(), 1);
    assert_eq!(orders.len(), 3, "BTC + its stray SOL + ETH; ETH's stray SOL is gone");
}

#[test]
fn the_spot_lane_reads_each_admitted_symbol_and_still_reports_no_positions() {
    let (wire, calls) = Wire::new();
    let client = spot(wire);
    client.admit_symbol("ETHUSDT");
    client.admit_symbol("SOLUSDT.P"); // a perp spelling: not this lane's
    let orders = client.fetch_order_status_reports(0).unwrap();
    let symbols: Vec<_> = orders.iter().map(|o| o.symbol.as_str()).collect();
    assert_eq!(symbols, ["BTCUSDT", "ETHUSDT"], "the spot lane keeps the bare label");
    assert_eq!(client.fetch_position_status_reports().unwrap(), Vec::new());
    let fills = client.fetch_fill_reports(0).unwrap();
    let ids: Vec<_> = fills.iter().map(|f| f.trade_id.as_str().to_string()).collect();
    assert_eq!(ids, ["77", "ETHUSDT:77"]);
    let p = BINANCE_RECON.paths;
    assert_eq!(
        asked(&calls),
        [
            (p.spot_open_orders, "BTCUSDT"),
            (p.spot_open_orders, "ETHUSDT"),
            (p.spot_my_trades, "BTCUSDT"),
            (p.spot_my_trades, "ETHUSDT"),
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
/// audit's G2). `userTrades` reports the CHILD order a trigger placed, with no coid; `allAlgoOrders`
/// says which stop it was the child of, and the fill comes out carrying the stop's bare coid. A fill
/// that is no stop's child stays coid-less (the restore joins it by its venue order id), and the read
/// is made for the symbols that have a coid-less fill, for the latest rows and with NO `startTime`: a
/// stop created before the pass's window that then triggers must still be named.
#[test]
fn a_triggered_stops_fill_is_named_from_the_algo_history() {
    let (mut wire, calls) = Wire::new();
    // `fill_row`'s order id is 9: the stop whose trigger placed order 9 was `stop-1`
    wire.children = vec![("BTCUSDT", "9", "stop-1"), ("BTCUSDT", "123", "another-stop")];
    let client = perp(wire);
    let fills = client.fetch_fill_reports(5).unwrap();
    assert_eq!(fills.len(), 1);
    assert_eq!(fills[0].client_order_id.as_deref(), Some("stop-1"), "named from the history");
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
    assert_eq!(fills[0].client_order_id, None);
}

/// The history read costs a request only when a fill needs it, and it is the PERP lane's alone: an
/// empty fill list asks nothing, and the spot lane (no Algo Service) never asks.
#[test]
fn the_algo_history_is_read_only_for_a_perp_fill_that_carries_no_coid() {
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

/// A history that cannot be read fails the whole fill fetch for the first few passes in a row, as the
/// open-list read fails the order report: a fill list that silently lacked the names would let the
/// restore planner read a filled stop as one the venue stopped listing. A history that STAYS broken
/// must not blind the account for good, so from the next failure on the fetch goes ahead with the
/// fills unnamed; one good read makes it strict again.
#[test]
fn an_unreadable_algo_history_fails_the_fill_fetch_a_few_times_then_goes_ahead_unnamed() {
    let (mut wire, _calls) = Wire::new();
    wire.children = vec![("BTCUSDT", "9", "stop-1")];
    // both ways of naming are down: the history and the by-id reads
    let down = [Arc::clone(&wire.fail_history), Arc::clone(&wire.fail_order_lookup)];
    let set = |up: bool| down.iter().for_each(|d| d.store(!up, Ordering::Relaxed));
    set(false);
    let client = perp(wire);
    for pass in 1..=3 {
        assert!(client.fetch_fill_reports(0).is_err(), "pass {pass}: strict");
    }
    let fills = client.fetch_fill_reports(0).expect("the fourth failure goes ahead");
    assert_eq!(fills.len(), 1);
    assert_eq!(fills[0].client_order_id, None, "unnamed: the history was unreadable");

    set(true);
    let fills = client.fetch_fill_reports(0).unwrap();
    assert_eq!(fills[0].client_order_id.as_deref(), Some("stop-1"), "named again");
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

/// **A stop older than the history's window is named by its child order.** `allAlgoOrders` may not
/// return a stop created before its default window (the filter is unmeasured), so the fill of the
/// stop's child order stays coid-less after the history; the child is then asked about by id: the
/// order's stored `clientOrderId` is the stop's `clientAlgoId`, and the algo endpoint says it placed
/// that very order. The answer is remembered, so the second pass asks nothing.
#[test]
fn a_stop_older_than_the_histories_window_is_named_by_its_child_order() {
    let (mut wire, calls) = Wire::new();
    wire.orders = vec![("BTCUSDT", "9", "x-LINK-stop1")];
    wire.algo_one = vec![("x-LINK-stop1", "9", "BTCUSDT")]; // the history (`children`) knows nothing
    let client = perp(wire);

    let fills = client.fetch_fill_reports(5).unwrap();
    assert_eq!(fills[0].client_order_id.as_deref(), Some("stop1"), "the bare coid of the stop");
    assert_eq!(&*fills[0].venue_order_id, "9", "its venue id is untouched");
    let sym = |s: &str| ("symbol".to_string(), s.to_string());
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
            (ORDER_ONE.to_string(), vec![sym("BTCUSDT"), ("orderId".into(), "9".into())]),
            (ALGO_ONE.to_string(), vec![("clientAlgoId".into(), "x-LINK-stop1".into())]),
        ],
        "one read of the child, one of the stop it names (as the venue stored it)"
    );

    let again = client.fetch_fill_reports(5).unwrap();
    assert_eq!(again[0].client_order_id.as_deref(), Some("stop1"), "named from memory");
    let reads = |calls: &Calls| {
        calls.lock().unwrap().iter().filter(|(p, _)| p == ORDER_ONE || p == ALGO_ONE).count()
    };
    assert_eq!(reads(&calls), 2, "nothing was asked twice");
}

/// A plain order's fill is asked about ONCE and stays coid-less: the venue knows no algo order of
/// that `clientOrderId`. And an algo order that placed ANOTHER order does not name this fill.
#[test]
fn a_plain_orders_fill_stays_unnamed_and_is_asked_about_once() {
    let (mut wire, calls) = Wire::new();
    wire.orders = vec![("BTCUSDT", "9", "x-LINK-plain1")]; // no algo order of that id: -2013
    let client = perp(wire);
    assert_eq!(client.fetch_fill_reports(0).unwrap()[0].client_order_id, None);
    assert_eq!(client.fetch_fill_reports(0).unwrap()[0].client_order_id, None);
    let asked_algo = calls.lock().unwrap().iter().filter(|(p, _)| p == ALGO_ONE).count();
    let asked_order = calls.lock().unwrap().iter().filter(|(p, _)| p == ORDER_ONE).count();
    assert_eq!((asked_order, asked_algo), (1, 1), "remembered: the second pass asks nothing");

    let (mut wire, _calls) = Wire::new();
    wire.orders = vec![("BTCUSDT", "9", "x-LINK-stop1")];
    wire.algo_one = vec![("x-LINK-stop1", "77", "BTCUSDT")]; // placed order 77, not this fill's 9
    assert_eq!(perp(wire).fetch_fill_reports(0).unwrap()[0].client_order_id, None);
}

/// The by-id reads are bounded per pass (`ALGO_CHILD_LOOKUPS_PER_PASS` fill orders): a pass that meets
/// more waits for the next for the rest, without failing, and nothing is asked twice.
#[test]
fn fill_orders_beyond_the_per_pass_budget_wait_for_the_next_pass() {
    use vike_binance::algo::ALGO_CHILD_LOOKUPS_PER_PASS;
    let (mut wire, calls) = Wire::new();
    // the mounted row's order 9, plus enough more to pass the budget
    wire.more_fill_orders = (100..100 + ALGO_CHILD_LOOKUPS_PER_PASS as u64 + 5).collect();
    let client = perp(wire);
    let order_reads =
        |calls: &Calls| calls.lock().unwrap().iter().filter(|(p, _)| p == ORDER_ONE).count();

    client.fetch_fill_reports(0).expect("a capped pass is not a failed one");
    assert_eq!(order_reads(&calls), ALGO_CHILD_LOOKUPS_PER_PASS, "the budget, no more");
    client.fetch_fill_reports(0).unwrap();
    assert_eq!(
        order_reads(&calls),
        ALGO_CHILD_LOOKUPS_PER_PASS + 6,
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
        assert_eq!(fills[0].client_order_id.as_deref(), Some("stop1"), "pass {pass}");
    }
}

/// Forgiving a failed history says so: ONE warning an hour, on its own clock, naming the unmeasured
/// REST spelling it rests on. Three forgiven passes inside the hour make one warning.
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
                && e.message.contains("relies on an unmeasured REST spelling")
        })
        .count();
    assert_eq!(warned, 1, "forgiven three times, warned once: {events:?}");
}

/// A history that is down while fill orders are LEFT OVER (past the per-pass budget) is not forgiven:
/// the history might have named the ones the budget did not reach, so the pass is an error.
#[test]
fn a_failed_history_with_fill_orders_left_over_is_still_an_error() {
    use vike_binance::algo::ALGO_CHILD_LOOKUPS_PER_PASS;
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
/// id): only an id of OURS names a fill, because a coid-linked fill of an order the node never minted
/// is worse than an unnamed one. A foreign id costs one read of the child and none of the algo
/// endpoint; a stop of OUR id on ANOTHER symbol is read and refused.
#[test]
fn a_stop_that_is_not_ours_does_not_name_a_fill() {
    for foreign in ["web_ab12cd34", "x-OTHER-stop1", "android_9Zx"] {
        let (mut wire, calls) = Wire::new();
        wire.orders = vec![("BTCUSDT", "9", foreign)];
        wire.algo_one = vec![(foreign, "9", "BTCUSDT")]; // a real stop, which triggered this child
        let fills = perp(wire).fetch_fill_reports(0).unwrap();
        assert_eq!(fills[0].client_order_id, None, "{foreign}: not our id, so not named");
        let algo_reads = calls.lock().unwrap().iter().filter(|(p, _)| p == ALGO_ONE).count();
        assert_eq!(algo_reads, 0, "{foreign}: the algo endpoint is not even asked");
    }

    // without the account's link id no `x-` id is ours
    let (mut wire, _calls) = Wire::new();
    wire.orders = vec![("BTCUSDT", "9", "x-LINK-stop1")];
    wire.algo_one = vec![("x-LINK-stop1", "9", "BTCUSDT")];
    let unlinked = BinanceReconClient::perp(NullSigner, wire, "https://x", "BTCUSDT");
    assert_eq!(unlinked.fetch_fill_reports(0).unwrap()[0].client_order_id, None);

    // our own id, but the stop is on another symbol
    let (mut wire, _calls) = Wire::new();
    wire.orders = vec![("BTCUSDT", "9", "x-LINK-stop1")];
    wire.algo_one = vec![("x-LINK-stop1", "9", "ETHUSDT")];
    assert_eq!(perp(wire).fetch_fill_reports(0).unwrap()[0].client_order_id, None);

    // …and the same stop on the fill's own symbol, with a bare coid of ours, IS named
    let (mut wire, _calls) = Wire::new();
    wire.orders = vec![("BTCUSDT", "9", "abcd1234stop1")];
    wire.algo_one = vec![("abcd1234stop1", "9", "BTCUSDT")];
    let named = perp(wire).fetch_fill_reports(0).unwrap();
    assert_eq!(named[0].client_order_id.as_deref(), Some("abcd1234stop1"));
}

/// A stop that exists but does not yet say which order it placed is not an answer: it is not
/// remembered as "no", so the pass that sees it again names the fill.
#[test]
fn a_stop_that_has_not_said_what_it_placed_is_asked_again() {
    let (mut wire, calls) = Wire::new();
    wire.orders = vec![("BTCUSDT", "9", "x-LINK-stop1")];
    wire.algo_one = vec![("x-LINK-stop1", "", "BTCUSDT")]; // the algo record lags the child
    let client = perp(wire);
    assert_eq!(client.fetch_fill_reports(0).unwrap()[0].client_order_id, None, "not yet");
    assert_eq!(client.fetch_fill_reports(0).unwrap()[0].client_order_id, None);
    let order_reads = calls.lock().unwrap().iter().filter(|(p, _)| p == ORDER_ONE).count();
    assert_eq!(order_reads, 2, "asked again, not remembered as a no");
}

/// **A persistent error on one symbol does not blind the others.** The first symbol's by-id read
/// fails every pass; the second symbol's stop is still named (on the pass that goes ahead after the
/// strict ones), because the failure is recorded and the pass moves on.
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
        fills.iter().map(|f| (f.symbol.as_str(), f.client_order_id.as_deref())).collect();
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
    use vike_binance::event_mapper::map_execution_report;
    use vike_binance::perp_mapper::map_binance_perp;
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
    let streamed = fill_id(map_binance_perp(&perp_frame, "binance", "BTCUSDT.P"));
    let (wire, _calls) = Wire::new();
    let client = perp(wire);
    client.admit_symbol("ETHUSDT.P");
    let reported = client.fetch_fill_reports(0).unwrap();
    assert_eq!(reported[1].trade_id.as_str(), streamed);

    let spot_frame = json!({ "e": "executionReport", "s": "ETHUSDT", "c": "c1", "x": "TRADE",
        "X": "FILLED", "S": "BUY", "l": "1.0", "L": "50000", "n": "0.1", "N": "USDT", "m": false,
        "T": 5, "t": 77 });
    let streamed = fill_id(map_execution_report(&spot_frame, "binance", "BTCUSDT"));
    let (wire, _calls) = Wire::new();
    let client = spot(wire);
    client.admit_symbol("ETHUSDT");
    let reported = client.fetch_fill_reports(0).unwrap();
    assert_eq!(reported[1].trade_id.as_str(), streamed);
}
