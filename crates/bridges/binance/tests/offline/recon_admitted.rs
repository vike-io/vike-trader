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
//!   * no admitted symbol: the request list is EXACTLY the mounted symbol's, byte for byte;
//!   * an admitted symbol: its orders, positions and fills appear, tagged with the engine's spelling
//!     (`.P` on the perp lane), its fills under the symbol-qualified trade id the user-data stream
//!     spells (`family::fill_trade_id`, decision 0174 item 5);
//!   * a symbol nobody admitted (another lane's spelling, the mounted one again, a 17th) is never
//!     queried;
//!   * a failed read of an admitted symbol fails the whole fetch (a partial list would be read by the
//!     restore planner as "the rest are gone");
//!   * a row for a symbol that was not asked for is dropped, never reported.

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
}

impl Wire {
    fn new() -> (Self, Calls) {
        let calls: Calls = Arc::new(Mutex::new(Vec::new()));
        (Wire { calls: Arc::clone(&calls), fail_on: None, fail_code: -1003, stray: false }, calls)
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
        self.calls.lock().unwrap().push((path.to_string(), recorded));
        if self.fail_on == Some(symbol.as_str()) {
            return Err(VenueApiError { code: self.fail_code, msg: format!("refused: {symbol}") });
        }
        let p = BINANCE_RECON.paths;
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

fn perp(wire: Wire) -> BinanceReconClient<NullSigner, Wire> {
    BinanceReconClient::perp(NullSigner, wire, "https://x", "BTCUSDT")
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
            (p.perp_positions.to_string(), vec![sym("BTCUSDT")]),
            (
                p.perp_user_trades.to_string(),
                vec![sym("BTCUSDT"), ("startTime".into(), "5".into())]
            ),
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
            (p.perp_positions, "BTCUSDT"),
            (p.perp_positions, "ETHUSDT"),
            (p.perp_user_trades, "BTCUSDT"),
            (p.perp_user_trades, "ETHUSDT"),
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
        [(p.perp_open_orders, "BTCUSDT"), (p.perp_open_orders, "SOLUSDT")]
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
    assert_eq!(queried.len(), 1 + MAX_EXTRA_SYMBOLS, "the mounted symbol plus sixteen");
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
