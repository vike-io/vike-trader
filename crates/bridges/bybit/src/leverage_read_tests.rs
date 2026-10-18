//! bybit's leverage READ over a scripted signed transport (decision 0182): no venue network.
//!
//! ⚠ The fixtures are the venue's DOCUMENTED shapes, not captures from the demo host (the module doc
//! of `leverage_read.rs` says why that matters); a capture replaces them in the live demo drill.
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`the_account_mode_must_be_plain_cross`] | an isolated or portfolio-margin account asked to hold a figure the engine cannot model |
//! | [`one_position_row_is_read_and_every_other_shape_refused`] | a hedge account, a flat answer or a portfolio account's empty leverage read as a number |
//! | [`the_maximum_is_the_instrument_rows_and_a_non_linear_row_is_refused`] | a figure above the symbol's maximum asked of the venue |
//! | [`a_clean_account_passes_the_probe_with_reads_only`] | the probe writing, or reading more than it says |
//! | [`each_refusal_reason_is_said_and_nothing_is_written`] | a refused account half-applied |
//! | [`held_equal_to_the_figure_is_a_no_op_with_no_sleep`] | an extra write or wait when stated equals current |
//! | [`the_poll_converges_when_the_write_lands_and_never_assumes_when_it_does_not`] | "assume it worked" |
//! | [`an_unreadable_venue_confirms_nothing_and_leaks_no_key`] | a transport failure read as a confirmation, or a credential in the words |

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::Duration;

use serde_json::{Value, json};
use vike_bridge_core::leverage_read::{confirm, probe};
use vike_bridge_core::transport::VenueApiError;
use vike_secrets::credentials::Credentials;

use super::*;

/// A scripted signed transport: answers per path from a queue (the last answer repeats) and logs
/// `(method, path)` of every call. The log is shared, so a test reads it after the reader owns the wire.
struct Wire {
    log: Log,
    answers: RefCell<Answers>,
}

type Log = Rc<RefCell<Vec<(String, String)>>>;
type Answers = HashMap<&'static str, VecDeque<Result<Value, VenueApiError>>>;

impl Wire {
    fn new() -> (Self, Log) {
        let log = Rc::new(RefCell::new(Vec::new()));
        (Wire { log: Rc::clone(&log), answers: RefCell::new(HashMap::new()) }, log)
    }

    fn then(self, path: &'static str, answer: Result<Value, VenueApiError>) -> Self {
        self.answers.borrow_mut().entry(path).or_default().push_back(answer);
        self
    }

    fn says(self, path: &'static str, body: Value) -> Self {
        self.then(path, Ok(body))
    }
}

impl BybitTransport for Wire {
    fn signed(
        &self,
        _base_url: &str,
        path: &str,
        method: &str,
        _params: &[(&str, Value)],
        _signer: &BybitV5Signer,
    ) -> Result<Value, VenueApiError> {
        self.log.borrow_mut().push((method.to_string(), path.to_string()));
        let mut answers = self.answers.borrow_mut();
        let queue = answers.get_mut(path).unwrap_or_else(|| panic!("an unscripted read of {path}"));
        if queue.len() > 1 { queue.pop_front().expect("one") } else { queue[0].clone() }
    }
}

fn reader(wire: Wire) -> BybitLeverageReader<Wire> {
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: None,
    };
    BybitLeverageReader::new(BybitV5Signer::new(&creds, || 0), wire, "https://demo", "BTCUSDT")
}

fn envelope(result: Value) -> Value {
    json!({"retCode": 0, "retMsg": "OK", "result": result, "retExtInfo": {}, "time": 1})
}

fn account_info(mode: &str) -> Value {
    envelope(json!({"marginMode": mode, "unifiedMarginStatus": 5, "isMasterTrader": false}))
}

fn row(symbol: &str, leverage: &str, size: &str, idx: i64) -> Value {
    json!({
        "symbol": symbol, "side": if size == "0" { "" } else { "Buy" }, "size": size,
        "leverage": leverage, "positionIdx": idx, "tradeMode": 0, "avgPrice": "0"
    })
}

fn positions(rows: Vec<Value>) -> Value {
    envelope(json!({"list": rows, "category": "linear"}))
}

fn instruments(kind: &str, max: &str) -> Value {
    envelope(json!({"category": "linear", "list": [{
        "symbol": "BTCUSDT", "contractType": kind, "status": "Trading", "baseCoin": "BTC",
        "quoteCoin": "USDT", "settleCoin": "USDT",
        "leverageFilter": {"minLeverage": "1", "maxLeverage": max, "leverageStep": "0.01"},
        "priceFilter": {"tickSize": "0.10"},
        "lotSizeFilter": {"qtyStep": "0.001", "minOrderQty": "0.001", "minNotionalValue": "5"}
    }]}))
}

const INFO: &str = "/v5/account/info";
const LIST: &str = "/v5/position/list";
const INSTR: &str = "/v5/market/instruments-info";

/// A clean account: plain cross, flat at `held`, a 100x symbol.
fn clean(held: &str) -> Wire {
    Wire::new()
        .0
        .says(INFO, account_info("REGULAR_MARGIN"))
        .says(LIST, positions(vec![row("BTCUSDT", held, "0", 0)]))
        .says(INSTR, instruments("LinearPerpetual", "100.00"))
}

#[test]
fn the_account_mode_must_be_plain_cross() {
    assert_eq!(check_margin_mode(&json!({"marginMode": "REGULAR_MARGIN"})), Ok(()));
    let isolated = check_margin_mode(&json!({"marginMode": "ISOLATED_MARGIN"})).unwrap_err();
    assert!(isolated.contains("isolated margin"), "{isolated}");
    let portfolio = check_margin_mode(&json!({"marginMode": "PORTFOLIO_MARGIN"})).unwrap_err();
    assert!(portfolio.contains("portfolio margin"), "{portfolio}");
    assert!(check_margin_mode(&json!({"marginMode": "SOMETHING_NEW"})).is_err());
    assert!(check_margin_mode(&json!({"unifiedMarginStatus": 5})).is_err(), "no field, no guess");
}

#[test]
fn one_position_row_is_read_and_every_other_shape_refused() {
    let read = |rows: Vec<Value>| parse_position(&json!({"list": rows}), "BTCUSDT");
    assert_eq!(
        read(vec![row("BTCUSDT", "10", "0", 0)]),
        Ok(HeldLeverage { held: 10.0, position_open: false })
    );
    assert_eq!(
        read(vec![row("BTCUSDT", "2.5", "0.012", 0)]),
        Ok(HeldLeverage { held: 2.5, position_open: true }),
        "a non-zero size is an open position"
    );
    // Numbers on the wire as strings or numbers alike.
    assert_eq!(
        read(vec![json!({"symbol": "BTCUSDT", "leverage": 5, "size": 0, "positionIdx": "0"})]),
        Ok(HeldLeverage { held: 5.0, position_open: false })
    );
    // None: a flat symbol answered no row, or only another symbol's.
    assert!(read(vec![]).unwrap_err().contains("no position row"));
    assert!(read(vec![row("ETHUSDT", "10", "0", 0)]).is_err());
    // Hedge mode: two rows, or one that is positionIdx 1.
    let hedge = read(vec![row("BTCUSDT", "10", "0", 1), row("BTCUSDT", "10", "0", 2)]).unwrap_err();
    assert!(hedge.contains("hedge mode"), "{hedge}");
    assert!(read(vec![row("BTCUSDT", "10", "0", 1)]).unwrap_err().contains("hedge mode"));
    // Portfolio margin answers an empty leverage; a non-figure and a sub-1 figure are no leverage.
    for bad in ["", "abc", "0", "0.5"] {
        let why = read(vec![row("BTCUSDT", bad, "0", 0)]).unwrap_err();
        assert!(why.contains("no usable leverage"), "{bad:?}: {why}");
    }
    assert!(parse_position(&json!({}), "BTCUSDT").unwrap_err().contains("no `list`"));
    // A size that is missing or no figure is NOT read as flat: the open-position guard fails closed.
    for no_size in [
        json!({"symbol": "BTCUSDT", "leverage": "10", "positionIdx": 0}),
        json!({"symbol": "BTCUSDT", "leverage": "10", "positionIdx": 0, "size": "n/a"}),
    ] {
        let why = parse_position(&json!({"list": [no_size]}), "BTCUSDT").unwrap_err();
        assert!(why.contains("no usable size"), "{why}");
    }
}

#[test]
fn the_maximum_is_the_instrument_rows_and_a_non_linear_row_is_refused() {
    assert_eq!(
        parse_max_leverage(&instruments("LinearPerpetual", "100.00"), "BTCUSDT"),
        Ok(Some(100.0))
    );
    let mut no_filter = instruments("LinearPerpetual", "100.00");
    no_filter["result"]["list"][0].as_object_mut().expect("a row").remove("leverageFilter");
    assert_eq!(parse_max_leverage(&no_filter, "BTCUSDT"), Ok(None), "unknown, not zero");
    let inverse = parse_max_leverage(&instruments("InversePerpetual", "50.00"), "BTCUSDT");
    assert!(inverse.unwrap_err().contains("linear perpetuals only"));
    assert!(parse_max_leverage(&instruments("LinearPerpetual", "100"), "XRPUSDT").is_err());
}

#[test]
fn a_clean_account_passes_the_probe_with_reads_only() {
    let (wire, log) = Wire::new();
    let wire = wire
        .says(INFO, account_info("REGULAR_MARGIN"))
        .says(LIST, positions(vec![row("BTCUSDT", "2", "0", 0)]))
        .says(INSTR, instruments("LinearPerpetual", "100.00"));
    assert_eq!(probe(&reader(wire), 10.0), Ok(()));
    let log = log.borrow();
    assert!(log.iter().all(|(method, _)| method == "GET"), "the probe wrote: {log:?}");
    assert!(log.iter().all(|(_, path)| !path.contains("set-leverage")), "{log:?}");
    let paths: Vec<&str> = log.iter().map(|(_, p)| p.as_str()).collect();
    assert_eq!(paths, [INFO, LIST, INSTR], "exactly the three reads, in order");
}

#[test]
fn each_refusal_reason_is_said_and_nothing_is_written() {
    let cases: Vec<(&str, Wire, f64, &str)> = vec![
        (
            "isolated account",
            Wire::new().0.says(INFO, account_info("ISOLATED_MARGIN")),
            10.0,
            "isolated margin",
        ),
        (
            "portfolio margin",
            Wire::new().0.says(INFO, account_info("PORTFOLIO_MARGIN")),
            10.0,
            "portfolio margin",
        ),
        (
            "hedge mode",
            Wire::new().0.says(INFO, account_info("REGULAR_MARGIN")).says(
                LIST,
                positions(vec![row("BTCUSDT", "2", "0", 1), row("BTCUSDT", "2", "0", 2)]),
            ),
            10.0,
            "hedge mode",
        ),
        (
            "an empty position list",
            Wire::new().0.says(INFO, account_info("REGULAR_MARGIN")).says(LIST, positions(vec![])),
            10.0,
            "no position row",
        ),
        (
            "a figure above the instrument's maximum",
            clean("2"),
            125.0,
            "above the venue's maximum of 100x",
        ),
        (
            "an open position at another leverage",
            Wire::new()
                .0
                .says(INFO, account_info("REGULAR_MARGIN"))
                .says(LIST, positions(vec![row("BTCUSDT", "3", "0.5", 0)]))
                .says(INSTR, instruments("LinearPerpetual", "100.00")),
            10.0,
            "open position is held at 3x",
        ),
    ];
    for (what, wire, figure, expect) in cases {
        let log = Rc::clone(&wire.log);
        let why = probe(&reader(wire), figure).unwrap_err();
        assert!(why.contains(expect), "{what}: {why}");
        assert!(log.borrow().iter().all(|(m, _)| m == "GET"), "{what}: wrote");
    }
    // The same open position, but already at the figure: a no-op, not a refusal.
    let wire = Wire::new()
        .0
        .says(INFO, account_info("REGULAR_MARGIN"))
        .says(LIST, positions(vec![row("BTCUSDT", "10", "0.5", 0)]))
        .says(INSTR, instruments("LinearPerpetual", "100.00"));
    assert_eq!(probe(&reader(wire), 10.0), Ok(()));
}

#[test]
fn held_equal_to_the_figure_is_a_no_op_with_no_sleep() {
    let (wire, log) = Wire::new();
    let wire = wire
        .says(INFO, account_info("REGULAR_MARGIN"))
        .says(LIST, positions(vec![row("BTCUSDT", "10", "0", 0)]))
        .says(INSTR, instruments("LinearPerpetual", "100.00"));
    let mut slept = Vec::new();
    assert_eq!(confirm(&reader(wire), 10.0, |d| slept.push(d)), Ok(10.0));
    assert!(slept.is_empty(), "stated equals current: nothing to wait for");
    assert!(log.borrow().iter().all(|(m, _)| m == "GET"), "no write from the lane");
}

#[test]
fn the_poll_converges_when_the_write_lands_and_never_assumes_when_it_does_not() {
    // The probe's read and the first poll see the old 2x; the second poll sees the write.
    let (wire, _) = Wire::new();
    let wire = wire
        .says(INFO, account_info("REGULAR_MARGIN"))
        .says(INSTR, instruments("LinearPerpetual", "100.00"))
        .says(LIST, positions(vec![row("BTCUSDT", "2", "0", 0)]))
        .says(LIST, positions(vec![row("BTCUSDT", "2", "0", 0)]))
        .says(LIST, positions(vec![row("BTCUSDT", "10", "0", 0)]));
    let mut slept: Vec<Duration> = Vec::new();
    assert_eq!(confirm(&reader(wire), 10.0, |d| slept.push(d)), Ok(10.0));
    assert_eq!(slept, [Duration::from_millis(250)]);

    // A read-only key: the bridge's write was refused, the venue keeps 2x, the poll ends in an error.
    let mut slept: Vec<Duration> = Vec::new();
    let why = confirm(&reader(clean("2")), 10.0, |d| slept.push(d)).unwrap_err();
    assert!(why.contains("still holds 2x"), "{why}");
    assert_eq!(slept.iter().sum::<Duration>(), Duration::from_millis(5000));
}

#[test]
fn an_unreadable_venue_confirms_nothing_and_leaks_no_key() {
    let denied = VenueApiError { code: 10003, msg: "API key is invalid.".to_string() };
    let wire = Wire::new().0.then(INFO, Err(denied));
    let why = probe(&reader(wire), 10.0).unwrap_err();
    assert!(why.contains("account/info could not be read") && why.contains("10003"), "{why}");
    // A `retCode` envelope error is read too.
    let wire = Wire::new().0.says(
        INFO,
        json!({"retCode": 10002, "retMsg": "timestamp out of range", "result": {}, "time": 1}),
    );
    let why = probe(&reader(wire), 10.0).unwrap_err();
    assert!(why.contains("10002"), "{why}");
    // The credentials are only ever in the signer; their characters are not in any sentence above.
    assert!(!why.contains("\"k\"") && !why.contains("\"s\""), "{why}");
}
