//! binance's leverage READ over a scripted signed transport (decision 0182): no venue network.
//!
//! ⚠ The fixtures are the venue's DOCUMENTED shapes; a capture replaces them in the live demo drill.
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`one_both_cross_row_is_read_and_every_other_shape_refused`] | a hedge or isolated account, or a missing row, read as a number |
//! | [`the_bracket_maximum_is_optional_and_never_refuses`] | an unreadable bracket stopping a start it was never required for |
//! | [`a_spot_symbol_has_no_leverage_to_read`] | a spot engine asked to hold a futures leverage |
//! | [`a_clean_account_passes_the_probe_with_reads_only`] | the probe writing, or reading more than it says |
//! | [`each_refusal_reason_is_said_and_nothing_is_written`] | a refused account half-applied |
//! | [`held_equal_to_the_figure_is_a_no_op_with_no_sleep`] | an extra wait when stated equals current |
//! | [`the_poll_converges_when_the_write_lands_and_never_assumes_when_it_does_not`] | "assume it worked", and a rejected over-maximum figure going unseen |
//! | [`an_unreadable_venue_confirms_nothing`] | a transport failure (a portfolio-margin account answers /fapi with an error) read as a confirmation |

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

/// One call the reader made.
#[derive(Debug, Clone, PartialEq)]
struct Call {
    method: String,
    path: String,
    params: Vec<(String, String)>,
}

/// A scripted binance transport: answers per path from a queue (the last answer repeats) and logs
/// every call. A path with nothing scripted answers a venue error (`-2014`), as the real bracket does
/// for a key that cannot read it.
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
}

impl RestTransport for Wire {
    fn signed(
        &self,
        _base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<Value, VenueApiError> {
        self.log.borrow_mut().push(Call {
            method: method.to_string(),
            path: path.to_string(),
            params: params.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect(),
        });
        let mut answers = self.answers.borrow_mut();
        let Some(queue) = answers.get_mut(path) else {
            return Err(VenueApiError { code: -2014, msg: "API-key format invalid.".to_string() });
        };
        if queue.len() > 1 { queue.pop_front().expect("one") } else { queue[0].clone() }
    }

    fn public(
        &self,
        _base_url: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        panic!("the reader made an unsigned read of {path}");
    }
}

fn reader(wire: Wire) -> BinanceLeverageReader<BinanceHmacSigner, Wire> {
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: None,
    };
    BinanceLeverageReader::new(
        BinanceHmacSigner::new(&creds, || 0),
        wire,
        "https://fapi.test",
        "BTCUSDT",
    )
}

fn row(symbol: &str, leverage: &str, amt: &str, side: &str, margin: &str) -> Value {
    json!({
        "symbol": symbol, "positionAmt": amt, "entryPrice": "0.0", "leverage": leverage,
        "marginType": margin, "positionSide": side, "isolatedMargin": "0.00000000",
        "updateTime": 0
    })
}

fn flat(leverage: &str) -> Value {
    json!([row("BTCUSDT", leverage, "0.000", "BOTH", "cross")])
}

fn bracket(max: u32) -> Value {
    json!([{"symbol": "BTCUSDT", "notionalCoef": 1.0, "brackets": [
        {"bracket": 1, "initialLeverage": max, "notionalCap": 50000, "notionalFloor": 0},
        {"bracket": 2, "initialLeverage": max / 2, "notionalCap": 250000, "notionalFloor": 50000}
    ]}])
}

const RISK: &str = "/fapi/v2/positionRisk";
const BRACKET: &str = "/fapi/v1/leverageBracket";

/// A clean account: cross, one-way, flat at `held`, a 125x symbol.
fn clean(held: &str) -> Wire {
    Wire::new().0.says(RISK, flat(held)).says(BRACKET, bracket(125))
}

#[test]
fn one_both_cross_row_is_read_and_every_other_shape_refused() {
    let read = |body: Value| parse_position_risk(&body, "BTCUSDT");
    assert_eq!(read(flat("10")), Ok(HeldLeverage { held: 10.0, position_open: false }));
    assert_eq!(
        read(json!([row("BTCUSDT", "5", "-0.010", "BOTH", "cross")])),
        Ok(HeldLeverage { held: 5.0, position_open: true }),
        "a short is signed"
    );
    // Case-insensitive margin word, numbers as numbers.
    assert_eq!(
        read(
            json!([{"symbol": "BTCUSDT", "positionAmt": 0, "leverage": 3, "marginType": "CROSS"}])
        ),
        Ok(HeldLeverage { held: 3.0, position_open: false })
    );
    assert!(read(json!([])).unwrap_err().contains("no positionRisk row"));
    assert!(read(json!([row("ETHUSDT", "10", "0", "BOTH", "cross")])).is_err());
    // Hedge mode: LONG and SHORT rows, or one row that is a side.
    let hedge = read(json!([
        row("BTCUSDT", "10", "0", "LONG", "cross"),
        row("BTCUSDT", "10", "0", "SHORT", "cross")
    ]))
    .unwrap_err();
    assert!(hedge.contains("hedge mode"), "{hedge}");
    assert!(
        read(json!([row("BTCUSDT", "10", "0", "LONG", "cross")]))
            .unwrap_err()
            .contains("hedge-mode side")
    );
    // Isolated, or no margin word at all.
    let isolated = read(json!([row("BTCUSDT", "10", "0", "BOTH", "isolated")])).unwrap_err();
    assert!(isolated.contains("isolated margin"), "{isolated}");
    assert!(read(json!([{"symbol": "BTCUSDT", "leverage": "10", "positionAmt": "0"}])).is_err());
    // A leverage that is no figure.
    for bad in ["", "abc", "0", "0.5"] {
        assert!(read(json!([row("BTCUSDT", bad, "0", "BOTH", "cross")])).is_err(), "{bad:?}");
    }
    assert!(
        read(json!({"code": -2015, "msg": "Invalid API-key"})).is_err(),
        "an object is no array"
    );
    // An amount that is missing or no figure is NOT read as flat: the open-position guard fails closed.
    let no_amt = json!([{"symbol": "BTCUSDT", "leverage": "10", "marginType": "cross"}]);
    assert!(read(no_amt).unwrap_err().contains("no usable positionAmt"));
}

#[test]
fn the_bracket_maximum_is_optional_and_never_refuses() {
    assert_eq!(parse_bracket_max(&bracket(125), "BTCUSDT"), Some(125.0), "tier 0 is the highest");
    // The single-object shape older docs show.
    let single = bracket(50)[0].clone();
    assert_eq!(parse_bracket_max(&single, "BTCUSDT"), Some(50.0));
    // Anything else is "unknown".
    assert_eq!(parse_bracket_max(&bracket(125), "ETHUSDT"), None);
    assert_eq!(parse_bracket_max(&json!([]), "BTCUSDT"), None);
    assert_eq!(parse_bracket_max(&json!({"code": -2014}), "BTCUSDT"), None);
    assert_eq!(parse_bracket_max(&json!("x"), "BTCUSDT"), None);
    // The reader: an unreadable bracket is `Ok(None)`, not an error.
    let unreadable = Wire::new().0.says(RISK, flat("2"));
    assert_eq!(reader(unreadable).venue_max(), Ok(None));
    assert_eq!(probe(&reader(Wire::new().0.says(RISK, flat("2"))), 125.0), Ok(()));
}

#[test]
fn a_spot_symbol_has_no_leverage_to_read() {
    use vike_secrets::account_credentials::{AccountCredentials, CredentialFields};
    let fields = CredentialFields::from_pairs([("API_KEY", "k"), ("API_SECRET", "s")]);
    let creds = AccountCredentials::new(Some(1), Some(&fields), None);
    let spot = leverage_reader(&creds, AccountTier::Demo, "BTCUSDT").err().expect("refused");
    assert!(spot.contains("SPOT symbol"), "{spot}");
    assert!(leverage_reader(&creds, AccountTier::Demo, "BTCUSDT.P").is_ok());
    let none = AccountCredentials::none();
    let why = leverage_reader(&none, AccountTier::Demo, "BTCUSDT.P").err().expect("refused");
    assert!(why.contains("no binance api key pair"), "{why}");
}

#[test]
fn a_clean_account_passes_the_probe_with_reads_only() {
    let wire = clean("2");
    let log = Rc::clone(&wire.log);
    assert_eq!(probe(&reader(wire), 10.0), Ok(()));
    let log = log.borrow();
    assert!(log.iter().all(|c| c.method == "GET"), "the probe wrote: {log:?}");
    assert!(
        log.iter().all(|c| !c.path.contains("/leverage") || c.path.contains("Bracket")),
        "{log:?}"
    );
    let paths: Vec<&str> = log.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(paths, [RISK, RISK, BRACKET], "mode, held, then the optional maximum");
    assert!(
        log.iter().all(|c| c.params == [("symbol".to_string(), "BTCUSDT".to_string())]),
        "every read names the exchange symbol: {log:?}"
    );
}

#[test]
fn each_refusal_reason_is_said_and_nothing_is_written() {
    let cases: Vec<(&str, Wire, f64, &str)> = vec![
        (
            "isolated margin",
            Wire::new().0.says(RISK, json!([row("BTCUSDT", "10", "0", "BOTH", "isolated")])),
            10.0,
            "isolated margin",
        ),
        (
            "hedge mode",
            Wire::new().0.says(
                RISK,
                json!([
                    row("BTCUSDT", "10", "0", "LONG", "cross"),
                    row("BTCUSDT", "10", "0", "SHORT", "cross")
                ]),
            ),
            10.0,
            "hedge mode",
        ),
        ("an empty answer", Wire::new().0.says(RISK, json!([])), 10.0, "no positionRisk row"),
        (
            "a figure above the bracket's maximum",
            clean("2"),
            126.0,
            "above the venue's maximum of 125x",
        ),
        (
            "an open position at another leverage",
            Wire::new()
                .0
                .says(RISK, json!([row("BTCUSDT", "3", "0.5", "BOTH", "cross")]))
                .says(BRACKET, bracket(125)),
            10.0,
            "open position is held at 3x",
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
        .says(RISK, json!([row("BTCUSDT", "10", "0.5", "BOTH", "cross")]))
        .says(BRACKET, bracket(125));
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
    // mode read, held read and the first poll see 2x; the next poll sees the write.
    let wire = Wire::new()
        .0
        .says(RISK, flat("2"))
        .says(RISK, flat("2"))
        .says(RISK, flat("2"))
        .says(RISK, flat("10"))
        .says(BRACKET, bracket(125));
    let mut slept: Vec<Duration> = Vec::new();
    assert_eq!(confirm(&reader(wire), 10.0, |d| slept.push(d)), Ok(10.0));
    assert_eq!(slept, [Duration::from_millis(250)]);

    // No bracket is readable and the venue rejected an over-maximum figure at the start (the exec
    // thread only warns): the account keeps 2x and the poll says so after its whole schedule.
    let mut slept: Vec<Duration> = Vec::new();
    let unbracketed = Wire::new().0.says(RISK, flat("2"));
    let why = confirm(&reader(unbracketed), 126.0, |d| slept.push(d)).unwrap_err();
    assert!(why.contains("still holds 2x") && why.contains("126x"), "{why}");
    assert_eq!(slept.iter().sum::<Duration>(), Duration::from_millis(5000));
}

#[test]
fn an_unreadable_venue_confirms_nothing() {
    // A portfolio-margin account answers /fapi with an error.
    let pm = VenueApiError {
        code: -2015,
        msg: "Invalid API-key, IP, or permissions for action".to_string(),
    };
    let why = probe(&reader(Wire::new().0.then(RISK, Err(pm))), 10.0).unwrap_err();
    assert!(why.contains("positionRisk could not be read") && why.contains("-2015"), "{why}");
    // An unscripted path (the bracket) is `-2014` and does not stop a clean probe.
    assert_eq!(probe(&reader(Wire::new().0.says(RISK, flat("2"))), 10.0), Ok(()));
}
