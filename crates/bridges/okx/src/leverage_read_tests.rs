//! okx's leverage READ over a scripted signed transport (decision 0182): no venue network.
//!
//! ⚠ The fixtures are the venue's DOCUMENTED shapes except where `leverage_read.rs`'s module doc says a
//! field was measured; a capture replaces them in the live demo drill.
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`the_account_must_be_futures_or_multi_currency_and_net`] | a spot-only, portfolio-margin or long/short account asked to hold a figure |
//! | [`one_net_cross_leverage_row_is_read_and_every_other_shape_refused`] | a hedge account or a missing row read as a number |
//! | [`an_open_position_is_seen_and_an_isolated_or_hedge_one_refused`] | a figure moved under a position on another margin model |
//! | [`the_maximum_is_the_instruments_lever_and_an_inverse_is_refused`] | a figure above the instrument's maximum asked of the venue |
//! | [`a_clean_account_passes_the_probe_with_reads_only`] | the probe writing, or reading more than it says |
//! | [`each_refusal_reason_is_said_and_nothing_is_written`] | a refused account half-applied |
//! | [`held_equal_to_the_figure_is_a_no_op_with_no_sleep`] | an extra wait when stated equals current |
//! | [`the_poll_converges_when_the_write_lands_and_never_assumes_when_it_does_not`] | "assume it worked" |
//! | [`an_unreadable_venue_confirms_nothing`] | a transport failure read as a confirmation |
//! | [`the_write_is_cross_and_the_reads_ask_for_cross`] | the read and the one write disagreeing about the margin mode |

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::Duration;

use serde_json::{Value, json};
use vike_bridge_core::leverage_read::{confirm, probe};
use vike_bridge_core::transport::VenueApiError;
use vike_secrets::credentials::Credentials;

use super::*;

type Log = Rc<RefCell<Vec<Call>>>;
type Answers = HashMap<&'static str, VecDeque<Result<Value, VenueApiError>>>;

/// One call the reader made: its method, path and the `(key, value)` pairs it sent.
#[derive(Debug, Clone, PartialEq)]
struct Call {
    method: String,
    path: String,
    params: Vec<(String, String)>,
}

/// A scripted okx transport: answers per path from a queue (the last answer repeats) and logs every call.
struct Wire {
    log: Log,
    answers: RefCell<Answers>,
}

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

    fn record(
        &self,
        method: &str,
        path: &str,
        params: Vec<(String, String)>,
    ) -> Result<Value, VenueApiError> {
        self.log.borrow_mut().push(Call {
            method: method.to_string(),
            path: path.to_string(),
            params,
        });
        let mut answers = self.answers.borrow_mut();
        let queue = answers.get_mut(path).unwrap_or_else(|| panic!("an unscripted read of {path}"));
        if queue.len() > 1 { queue.pop_front().expect("one") } else { queue[0].clone() }
    }
}

impl OkxTransport for Wire {
    fn signed(
        &self,
        _base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
        _signer: &OkxV5Signer,
    ) -> Result<Value, VenueApiError> {
        let sent = params
            .iter()
            .map(|(k, v)| {
                ((*k).to_string(), v.as_str().map_or_else(|| v.to_string(), str::to_string))
            })
            .collect();
        self.record(method, path, sent)
    }

    fn public(
        &self,
        _base_url: &str,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        let sent = params.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect();
        self.record("GET", path, sent)
    }
}

fn reader(wire: Wire) -> OkxLeverageReader<Wire> {
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: Some("p".to_string().into()),
    };
    OkxLeverageReader::new(
        OkxV5Signer::new(&creds, || 0),
        wire,
        "https://okx.test",
        "BTC-USDT-SWAP",
    )
}

fn ok(data: Value) -> Value {
    json!({"code": "0", "msg": "", "data": data})
}

fn config(level: &str, pos_mode: &str) -> Value {
    ok(
        json!([{"uid": "1", "mainUid": "1", "acctLv": level, "posMode": pos_mode, "perm": "read_only,trade"}]),
    )
}

fn lever(inst: &str, lever: &str, pos_side: &str) -> Value {
    json!({"instId": inst, "mgnMode": "cross", "posSide": pos_side, "lever": lever})
}

fn position(inst: &str, pos: &str, side: &str, mode: &str) -> Value {
    json!({"instId": inst, "pos": pos, "posSide": side, "mgnMode": mode, "lever": "5"})
}

fn instrument(kind: &str, max: &str) -> Value {
    ok(json!([{
        "instId": "BTC-USDT-SWAP", "instType": "SWAP", "ctType": kind, "ctVal": "0.01",
        "ctValCcy": "BTC", "lever": max, "tickSz": "0.1", "lotSz": "0.01", "minSz": "0.01"
    }]))
}

const CONFIG: &str = "/api/v5/account/config";
const LEVER: &str = "/api/v5/account/leverage-info";
const POSITIONS: &str = "/api/v5/account/positions";
const INSTR: &str = "/api/v5/public/instruments";

/// A clean account: futures mode, net, cross, flat at `held`, a 100x instrument.
fn clean(held: &str) -> Wire {
    Wire::new()
        .0
        .says(CONFIG, config("2", "net_mode"))
        .says(LEVER, ok(json!([lever("BTC-USDT-SWAP", held, "net")])))
        .says(POSITIONS, ok(json!([])))
        .says(INSTR, instrument("linear", "100"))
}

#[test]
fn the_account_must_be_futures_or_multi_currency_and_net() {
    let row = |level: &str, mode: &str| json!({"acctLv": level, "posMode": mode});
    assert_eq!(check_account_config(&row("2", "net_mode")), Ok(()));
    assert_eq!(
        check_account_config(&row("3", "net_mode")),
        Ok(()),
        "multi-currency margin is allowed"
    );
    assert!(check_account_config(&row("1", "net_mode")).unwrap_err().contains("spot-only"));
    assert!(check_account_config(&row("4", "net_mode")).unwrap_err().contains("portfolio margin"));
    assert!(check_account_config(&row("9", "net_mode")).is_err());
    let hedge = check_account_config(&row("2", "long_short_mode")).unwrap_err();
    assert!(hedge.contains("long_short_mode"), "{hedge}");
    assert!(check_account_config(&json!({"posMode": "net_mode"})).is_err(), "no acctLv, no guess");
    assert!(check_account_config(&json!({"acctLv": "2"})).is_err(), "no posMode, no guess");
    // The level may arrive as a number.
    assert_eq!(check_account_config(&json!({"acctLv": 2, "posMode": "net_mode"})), Ok(()));
}

#[test]
fn one_net_cross_leverage_row_is_read_and_every_other_shape_refused() {
    let id = "BTC-USDT-SWAP";
    assert_eq!(parse_leverage_info(&json!([lever(id, "10", "net")]), id), Ok(10.0));
    assert_eq!(parse_leverage_info(&json!([lever(id, "2.5", "net")]), id), Ok(2.5));
    assert!(parse_leverage_info(&json!([]), id).unwrap_err().contains("no leverage row"));
    assert!(parse_leverage_info(&json!([lever("ETH-USDT-SWAP", "10", "net")]), id).is_err());
    let two = json!([lever(id, "10", "long"), lever(id, "10", "short")]);
    assert!(parse_leverage_info(&two, id).unwrap_err().contains("long/short"));
    assert!(parse_leverage_info(&json!([lever(id, "10", "long")]), id).is_err());
    let isolated = json!([{"instId": id, "mgnMode": "isolated", "posSide": "net", "lever": "10"}]);
    assert!(parse_leverage_info(&isolated, id).unwrap_err().contains("not for cross"));
    for bad in ["", "abc", "0", "0.5"] {
        assert!(parse_leverage_info(&json!([lever(id, bad, "net")]), id).is_err(), "{bad:?}");
    }
    assert!(parse_leverage_info(&json!({}), id).is_err());
}

#[test]
fn an_open_position_is_seen_and_an_isolated_or_hedge_one_refused() {
    let id = "BTC-USDT-SWAP";
    assert_eq!(parse_open_position(&json!([]), id), Ok(false), "a flat instrument answers no row");
    assert_eq!(parse_open_position(&json!([position(id, "0", "net", "cross")]), id), Ok(false));
    assert_eq!(parse_open_position(&json!([position(id, "3", "net", "cross")]), id), Ok(true));
    assert_eq!(
        parse_open_position(&json!([position(id, "-3", "net", "cross")]), id),
        Ok(true),
        "short"
    );
    // Another instrument's position is not this one's.
    assert_eq!(
        parse_open_position(&json!([position("ETH-USDT-SWAP", "3", "net", "cross")]), id),
        Ok(false)
    );
    let isolated = parse_open_position(&json!([position(id, "3", "net", "isolated")]), id);
    assert!(isolated.unwrap_err().contains("isolated"));
    let hedge = parse_open_position(&json!([position(id, "3", "long", "cross")]), id);
    assert!(hedge.unwrap_err().contains("not net"));
    assert!(parse_open_position(&json!({}), id).is_err());
    // A size that is missing or no figure is NOT read as flat: the open-position guard fails closed.
    let no_pos = json!([{"instId": id, "posSide": "net", "mgnMode": "cross"}]);
    assert!(parse_open_position(&no_pos, id).unwrap_err().contains("no usable pos"));
}

#[test]
fn the_maximum_is_the_instruments_lever_and_an_inverse_is_refused() {
    let data = |v: Value| v["data"].clone();
    assert_eq!(
        parse_max_leverage(&data(instrument("linear", "100")), "BTC-USDT-SWAP"),
        Ok(Some(100.0))
    );
    let mut none = data(instrument("linear", "100"));
    none[0].as_object_mut().expect("a row").remove("lever");
    assert_eq!(parse_max_leverage(&none, "BTC-USDT-SWAP"), Ok(None), "unknown, not zero");
    let inverse = parse_max_leverage(&data(instrument("inverse", "100")), "BTC-USDT-SWAP");
    assert!(inverse.unwrap_err().contains("linear swaps only"));
    assert!(parse_max_leverage(&data(instrument("linear", "100")), "ETH-USDT-SWAP").is_err());
}

#[test]
fn a_clean_account_passes_the_probe_with_reads_only() {
    let wire = clean("2");
    let log = Rc::clone(&wire.log);
    assert_eq!(probe(&reader(wire), 10.0), Ok(()));
    let log = log.borrow();
    assert!(log.iter().all(|c| c.method == "GET"), "the probe wrote: {log:?}");
    assert!(log.iter().all(|c| !c.path.contains("set-leverage")), "{log:?}");
    let paths: Vec<&str> = log.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(paths, [CONFIG, LEVER, POSITIONS, INSTR], "exactly the four reads, in order");
}

#[test]
fn the_write_is_cross_and_the_reads_ask_for_cross() {
    // The one write is `mgnMode: cross` (`OkxPerpRest::set_leverage`); the held leverage is read for cross.
    let wire = clean("2");
    let log = Rc::clone(&wire.log);
    probe(&reader(wire), 10.0).expect("clean");
    let lever_call = log.borrow().iter().find(|c| c.path == LEVER).cloned().expect("read");
    assert!(
        lever_call.params.contains(&("mgnMode".to_string(), "cross".to_string())),
        "{lever_call:?}"
    );
    assert!(lever_call.params.contains(&("instId".to_string(), "BTC-USDT-SWAP".to_string())));
    let instr_call = log.borrow().iter().find(|c| c.path == INSTR).cloned().expect("read");
    assert!(instr_call.params.contains(&("instType".to_string(), "SWAP".to_string())));
}

#[test]
fn each_refusal_reason_is_said_and_nothing_is_written() {
    let cases: Vec<(&str, Wire, f64, &str)> = vec![
        (
            "spot-only account",
            Wire::new().0.says(CONFIG, config("1", "net_mode")),
            10.0,
            "spot-only",
        ),
        (
            "portfolio margin",
            Wire::new().0.says(CONFIG, config("4", "net_mode")),
            10.0,
            "portfolio margin",
        ),
        (
            "long/short mode",
            Wire::new().0.says(CONFIG, config("2", "long_short_mode")),
            10.0,
            "long_short_mode",
        ),
        (
            "no leverage row",
            Wire::new().0.says(CONFIG, config("2", "net_mode")).says(LEVER, ok(json!([]))),
            10.0,
            "no leverage row",
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
                .says(CONFIG, config("2", "net_mode"))
                .says(LEVER, ok(json!([lever("BTC-USDT-SWAP", "3", "net")])))
                .says(POSITIONS, ok(json!([position("BTC-USDT-SWAP", "2", "net", "cross")])))
                .says(INSTR, instrument("linear", "100")),
            10.0,
            "open position is held at 3x",
        ),
        (
            "an open isolated position",
            Wire::new()
                .0
                .says(CONFIG, config("2", "net_mode"))
                .says(LEVER, ok(json!([lever("BTC-USDT-SWAP", "3", "net")])))
                .says(POSITIONS, ok(json!([position("BTC-USDT-SWAP", "2", "net", "isolated")]))),
            10.0,
            "isolated",
        ),
    ];
    for (what, wire, figure, expect) in cases {
        let log = Rc::clone(&wire.log);
        let why = probe(&reader(wire), figure).unwrap_err();
        assert!(why.contains(expect), "{what}: {why}");
        assert!(log.borrow().iter().all(|c| c.method == "GET"), "{what}: wrote");
    }
    // The same open position, already at the figure: a no-op, not a refusal.
    let wire = Wire::new()
        .0
        .says(CONFIG, config("2", "net_mode"))
        .says(LEVER, ok(json!([lever("BTC-USDT-SWAP", "10", "net")])))
        .says(POSITIONS, ok(json!([position("BTC-USDT-SWAP", "2", "net", "cross")])))
        .says(INSTR, instrument("linear", "100"));
    assert_eq!(probe(&reader(wire), 10.0), Ok(()));
}

#[test]
fn held_equal_to_the_figure_is_a_no_op_with_no_sleep() {
    let wire = clean("10");
    let log = Rc::clone(&wire.log);
    let mut slept = Vec::new();
    assert_eq!(confirm(&reader(wire), 10.0, |d| slept.push(d)), Ok(10.0));
    assert!(slept.is_empty(), "stated equals current: nothing to wait for");
    assert!(log.borrow().iter().all(|c| c.method == "GET"), "no write from the lane");
}

#[test]
fn the_poll_converges_when_the_write_lands_and_never_assumes_when_it_does_not() {
    // The probe's read and the first poll see the old 2x; the second poll sees the write.
    let wire = Wire::new()
        .0
        .says(CONFIG, config("2", "net_mode"))
        .says(POSITIONS, ok(json!([])))
        .says(INSTR, instrument("linear", "100"))
        .says(LEVER, ok(json!([lever("BTC-USDT-SWAP", "2", "net")])))
        .says(LEVER, ok(json!([lever("BTC-USDT-SWAP", "2", "net")])))
        .says(LEVER, ok(json!([lever("BTC-USDT-SWAP", "10", "net")])));
    let mut slept: Vec<Duration> = Vec::new();
    assert_eq!(confirm(&reader(wire), 10.0, |d| slept.push(d)), Ok(10.0));
    assert_eq!(slept, [Duration::from_millis(250)]);

    let mut slept: Vec<Duration> = Vec::new();
    let why = confirm(&reader(clean("2")), 10.0, |d| slept.push(d)).unwrap_err();
    assert!(why.contains("still holds 2x"), "{why}");
    assert_eq!(slept.iter().sum::<Duration>(), Duration::from_millis(5000));
}

#[test]
fn an_unreadable_venue_confirms_nothing() {
    let denied = VenueApiError { code: 50111, msg: "Invalid OK-ACCESS-KEY".to_string() };
    let why = probe(&reader(Wire::new().0.then(CONFIG, Err(denied))), 10.0).unwrap_err();
    assert!(why.contains("account/config could not be read") && why.contains("50111"), "{why}");
    // An envelope error is read too.
    let body = json!({"code": "50102", "msg": "Timestamp request expired", "data": []});
    let why = probe(&reader(Wire::new().0.says(CONFIG, body)), 10.0).unwrap_err();
    assert!(why.contains("50102"), "{why}");
    // An empty config answer is no answer.
    let why = probe(&reader(Wire::new().0.says(CONFIG, ok(json!([])))), 10.0).unwrap_err();
    assert!(why.contains("no row"), "{why}");
}
