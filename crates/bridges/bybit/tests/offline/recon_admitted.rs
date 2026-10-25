//! **The reconcile client reads every ADMITTED symbol of the account, not only the mounted one**
//! (offline: a recording `BybitTransport`, canned envelopes, no network, nothing `#[ignore]`d).
//!
//! The restore planner reads an order the venue's report does not carry as GONE, so a client that read
//! the mounted symbol alone made every resting order on a symbol admitted later invisible after a
//! restart. The client is told which symbols the engine admitted (`ReconClient::admit_symbol`) and
//! reads each one with the SAME request shape it uses for the mounted symbol (`order/realtime`,
//! `position/list` and `execution/list` all with `symbol=`): the settleCoin-wide forms list open orders
//! and non-flat positions only, and the flat row is what lets the diff see a position the venue has
//! since closed.
//!
//! Pins: no admitted symbol = the mounted symbol's requests byte for byte (the open-order read's
//! `limit=50` aside); an admitted symbol's rows
//! appear; a symbol nobody admitted (the mounted one, a repeat, a 17th) is never queried; one failed
//! read fails the whole fetch; a row for a symbol that was not asked for is dropped.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use vike_bridge_core::signer::BybitV5Signer;
use vike_bridge_core::transport::VenueApiError;
use vike_bybit::BybitReconClient;
use vike_bybit::perp::{PATH_EXECUTION_LIST, PATH_ORDER_REALTIME, PATH_POSITION_LIST};
use vike_bybit::transport::BybitTransport;
use vike_exec::MAX_EXTRA_SYMBOLS;
use vike_exec::recon::ReconClient;
use vike_secrets::credentials::Credentials;

type Call = (String, Vec<(String, Value)>);
type Calls = Arc<Mutex<Vec<Call>>>;

/// Records every request; each endpoint answers one row for the `symbol` asked about, plus (when
/// `stray`) a row for SOLUSDT that nobody admitted. `fail_on` makes one symbol's reads fail.
struct Wire {
    calls: Calls,
    fail_on: Option<&'static str>,
    /// The retCode and message `fail_on` answers with.
    fail: (i64, &'static str),
    stray: bool,
}

impl Wire {
    fn new() -> (Self, Calls) {
        let calls: Calls = Arc::new(Mutex::new(Vec::new()));
        (
            Wire {
                calls: Arc::clone(&calls),
                fail_on: None,
                fail: (10006, "rate limit"),
                stray: false,
            },
            calls,
        )
    }
}

fn row(path: &str, symbol: &str) -> Value {
    match path {
        PATH_ORDER_REALTIME => json!({
            "symbol": symbol, "orderId": "o1", "orderLinkId": format!("c-{symbol}"),
            "side": "Buy", "orderType": "Limit", "qty": "1", "cumExecQty": "0", "avgPrice": "0",
            "orderStatus": "New", "updatedTime": "1",
        }),
        PATH_POSITION_LIST => json!({
            "symbol": symbol, "size": "0.5", "side": "Buy", "positionIdx": 0, "avgPrice": "10",
            "updatedTime": "1",
        }),
        PATH_EXECUTION_LIST => json!({
            "symbol": symbol, "execId": format!("x-{symbol}"), "orderId": "o1", "side": "Buy",
            "execQty": "1", "execPrice": "10", "execFee": "0", "feeCurrency": "USDT",
            "isMaker": true, "execTime": "1",
        }),
        other => panic!("not a reconcile read: {other}"),
    }
}

impl BybitTransport for Wire {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, Value)],
        _signer: &BybitV5Signer,
    ) -> Result<Value, VenueApiError> {
        let recorded: Vec<(String, Value)> =
            params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        let symbol = recorded
            .iter()
            .find(|(k, _)| k == "symbol")
            .and_then(|(_, v)| v.as_str().map(str::to_string))
            .unwrap_or_default();
        self.calls.lock().unwrap().push((path.to_string(), recorded));
        if self.fail_on == Some(symbol.as_str()) {
            return Err(VenueApiError {
                code: self.fail.0,
                msg: format!("{}: {symbol}", self.fail.1),
            });
        }
        let mut list = vec![row(path, &symbol)];
        if self.stray {
            list.push(row(path, "SOLUSDT"));
        }
        Ok(json!({ "retCode": 0, "retMsg": "OK", "result": { "list": list } }))
    }
}

fn client(wire: Wire) -> BybitReconClient<Wire> {
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: None,
    };
    BybitReconClient::new(BybitV5Signer::new(&creds, || 0), wire, "https://x", "BTCUSDT")
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
                .and_then(|(_, v)| v.as_str().map(str::to_string))
                .unwrap_or_default();
            (path.clone(), symbol)
        })
        .collect()
}

fn pairs(rows: &[(&str, &str)]) -> Vec<(String, String)> {
    rows.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

#[test]
fn with_no_admitted_symbol_the_requests_are_exactly_the_mounted_symbols() {
    let (wire, calls) = Wire::new();
    let c = client(wire);
    c.fetch_order_status_reports(0).unwrap();
    c.fetch_position_status_reports().unwrap();
    c.fetch_fill_reports(5).unwrap();
    let linear = ("category".to_string(), json!("linear"));
    let sym = ("symbol".to_string(), json!("BTCUSDT"));
    // The open-order read asks the venue's maximum page (`limit=50`); `recon_paging.rs` pins the rest.
    let limit = ("limit".to_string(), json!(50));
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            (PATH_ORDER_REALTIME.to_string(), vec![linear.clone(), sym.clone(), limit]),
            (PATH_POSITION_LIST.to_string(), vec![linear.clone(), sym.clone()]),
            (
                PATH_EXECUTION_LIST.to_string(),
                vec![linear, sym, ("startTime".to_string(), json!(5))]
            ),
        ]
    );
}

#[test]
fn an_admitted_symbols_order_position_and_fills_are_reported() {
    let (wire, calls) = Wire::new();
    let c = client(wire);
    c.admit_symbol("ethusdt");
    c.admit_symbol("SOLUSDT.P"); // tolerated: bybit's engine spelling is the bare symbol

    let orders = c.fetch_order_status_reports(0).unwrap();
    let symbols: Vec<_> = orders.iter().map(|o| o.symbol.as_str()).collect();
    assert_eq!(symbols, ["BTCUSDT", "ETHUSDT", "SOLUSDT"]);
    assert_eq!(orders[1].client_order_id.as_deref(), Some("c-ETHUSDT"));

    let positions = c.fetch_position_status_reports().unwrap();
    assert_eq!(positions.len(), 3);

    // Bybit's `execId` is unique account-wide, so the admitted symbol's fill keeps it as is: it is
    // the very id the user-data stream folds.
    let fills = c.fetch_fill_reports(0).unwrap();
    let ids: Vec<_> = fills.iter().map(|f| f.trade_id.as_str().to_string()).collect();
    assert_eq!(ids, ["x-BTCUSDT", "x-ETHUSDT", "x-SOLUSDT"]);

    assert_eq!(
        asked(&calls),
        pairs(&[
            (PATH_ORDER_REALTIME, "BTCUSDT"),
            (PATH_ORDER_REALTIME, "ETHUSDT"),
            (PATH_ORDER_REALTIME, "SOLUSDT"),
            (PATH_POSITION_LIST, "BTCUSDT"),
            (PATH_POSITION_LIST, "ETHUSDT"),
            (PATH_POSITION_LIST, "SOLUSDT"),
            (PATH_EXECUTION_LIST, "BTCUSDT"),
            (PATH_EXECUTION_LIST, "ETHUSDT"),
            (PATH_EXECUTION_LIST, "SOLUSDT"),
        ])
    );
}

#[test]
fn a_symbol_nobody_admitted_is_never_queried() {
    let (wire, calls) = Wire::new();
    let c = client(wire);
    c.admit_symbol("btcusdt"); // the mounted symbol again
    c.admit_symbol("");
    c.admit_symbol("ETHUSDT");
    c.admit_symbol("ethusdt"); // a repeat
    c.fetch_order_status_reports(0).unwrap();
    assert_eq!(
        asked(&calls),
        pairs(&[(PATH_ORDER_REALTIME, "BTCUSDT"), (PATH_ORDER_REALTIME, "ETHUSDT")])
    );
}

#[test]
fn at_most_the_engines_extra_symbol_cap_is_read() {
    let (wire, calls) = Wire::new();
    let c = client(wire);
    for i in 0..(MAX_EXTRA_SYMBOLS + 4) {
        c.admit_symbol(&format!("S{i}USDT"));
    }
    c.fetch_order_status_reports(0).unwrap();
    let queried = asked(&calls);
    assert_eq!(queried.len(), 1 + MAX_EXTRA_SYMBOLS, "the mounted symbol plus sixteen");
    assert!(!queried.iter().any(|(_, s)| s == &format!("S{MAX_EXTRA_SYMBOLS}USDT")));
}

#[test]
fn a_failed_read_of_an_admitted_symbol_fails_the_whole_fetch() {
    let (mut wire, _calls) = Wire::new();
    wire.fail_on = Some("ETHUSDT");
    let c = client(wire);
    c.admit_symbol("ETHUSDT");
    // Never a partial list: the restore planner reads an absent order as GONE.
    assert!(c.fetch_order_status_reports(0).is_err());
    assert!(c.fetch_position_status_reports().is_err());
    assert!(c.fetch_fill_reports(0).is_err());
}

#[test]
fn a_row_for_a_symbol_that_was_not_asked_for_is_dropped() {
    let (mut wire, _calls) = Wire::new();
    wire.stray = true;
    let c = client(wire);
    c.admit_symbol("ETHUSDT");
    let orders = c.fetch_order_status_reports(0).unwrap();
    // The MOUNTED symbol's answer is passed through untouched (byte-identical to before); the stray
    // SOLUSDT row in the ETHUSDT answer is the one that goes.
    let symbols: Vec<_> = orders.iter().map(|o| o.symbol.as_str()).collect();
    assert_eq!(symbols, ["BTCUSDT", "SOLUSDT", "ETHUSDT"]);
}

/// A symbol bybit no longer lists answers a parameter error naming the symbol; for an ADMITTED symbol
/// that reads as "no rows" and the mounted symbol's rows still arrive. Any other error still fails the
/// whole fetch, and the same answer for the MOUNTED symbol is still an error.
#[test]
fn a_delisted_admitted_symbol_reads_as_no_rows_and_does_not_blind_the_account() {
    let (mut wire, _calls) = Wire::new();
    wire.fail_on = Some("ETHUSDT");
    wire.fail = (10001, "params error: symbol invalid");
    let c = client(wire);
    c.admit_symbol("ETHUSDT");
    let orders = c.fetch_order_status_reports(0).unwrap();
    assert_eq!(orders.iter().map(|o| o.symbol.as_str()).collect::<Vec<_>>(), ["BTCUSDT"]);
    assert_eq!(c.fetch_position_status_reports().unwrap().len(), 1);
    assert_eq!(c.fetch_fill_reports(0).unwrap().len(), 1);

    // A parameter error that is NOT about the symbol is not excused.
    let (mut wire, _calls) = Wire::new();
    wire.fail_on = Some("ETHUSDT");
    wire.fail = (10001, "params error: category invalid");
    let c = client(wire);
    c.admit_symbol("ETHUSDT");
    assert!(c.fetch_order_status_reports(0).is_err());

    let (mut wire, _calls) = Wire::new();
    wire.fail_on = Some("BTCUSDT");
    wire.fail = (10001, "params error: symbol invalid");
    assert!(
        client(wire).fetch_order_status_reports(0).is_err(),
        "the mounted symbol is not excused"
    );
}

/// A rate-limit answer (`10006`, or an HTTP 403/429 the transport keeps as the code) on an admitted
/// read backs the whole pass off: the next fetches make NO request until the pause lapses. With
/// nothing admitted there is no backoff and the mounted symbol's request repeats as it always did.
#[test]
fn a_rate_limit_answer_backs_the_pass_off_and_only_when_symbols_are_admitted() {
    for code in [10006, 403, 429] {
        let (mut wire, calls) = Wire::new();
        wire.fail_on = Some("ETHUSDT");
        wire.fail = (code, "too many visits");
        let c = client(wire);
        c.admit_symbol("ETHUSDT");
        assert!(c.fetch_order_status_reports(0).is_err(), "{code}");
        let before = calls.lock().unwrap().len();
        let err = c.fetch_position_status_reports().unwrap_err();
        assert!(err.contains("backing off"), "{code}: {err}");
        assert_eq!(calls.lock().unwrap().len(), before, "{code}: a backed-off pass sends nothing");
    }
    let (mut wire, calls) = Wire::new();
    wire.fail_on = Some("BTCUSDT");
    wire.fail = (10006, "too many visits");
    let c = client(wire);
    assert!(c.fetch_order_status_reports(0).is_err());
    assert!(c.fetch_order_status_reports(0).is_err());
    assert_eq!(calls.lock().unwrap().len(), 2, "nothing admitted: no backoff, the request repeats");
}
