//! Offline pins for `crate::class_prep` over a scripted venue (NO network): the first-order leverage write of a
//! symbol the account never traded, the TradFi-Perps agreement a `-4411` reject signs, and the words of `-2027`.
//! Every shape below is the one MEASURED on the demo host on 2026-10-30
//! (`crates/bridges/binance/tests/binance_classes_smoke.rs`).
//!
//! | test | what it pins |
//! |---|---|
//! | [`a_never_traded_symbol_gets_one_leverage_write_capped_at_its_bracket`] | `positionRisk` leverage `0` + flat -> ONE `POST /fapi/v1/leverage` of `min(configured, bracket max)` |
//! | [`a_symbol_that_holds_a_leverage_is_never_written`] | a healthy symbol costs one read and no write |
//! | [`a_symbol_that_is_not_provably_flat_or_unreadable_is_never_written`] | a position, a missing row, a failed read: nothing written |
//! | [`an_unreadable_bracket_writes_the_configured_figure_and_a_refusal_is_named`] | the bracket is optional; the venue's refusal is worded |
//! | [`the_mainnet_host_is_never_read_or_written`] | the owner's ruling: no write on a live host |
//! | [`a_tradfi_reject_signs_the_agreement_once_per_window_on_the_demo_host`] | `-4411` -> ONE `POST /fapi/v1/stock/contract`, a burst is one signature, the window ends |
//! | [`an_unreadable_2xx_is_a_signature_that_landed_and_a_refusal_is_named`] | the plain-text 2xx is "sent"; a venue refusal says so |
//! | [`a_live_host_signs_nothing_and_names_the_hand_step`] | the agreement is the holder's on a live account |
//! | [`the_position_at_leverage_refusal_is_worded_for_both_networks`] | `-2027` explained, demo and mainnet |

use std::cell::RefCell;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use vike_bridge_core::signer::{BinanceHmacSigner, Signer};
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, RestTransport, VenueApiError};
use vike_model::SymbolProperties;
use vike_secrets::credentials::Credentials;

use super::*;
use crate::modes::order_reject_reason;
use crate::perp::{DEMO_FAPI_REST, MAINNET_FAPI_REST};

type Call = (String, String, Vec<(String, String)>);
type Answer = Result<Value, VenueApiError>;

/// A venue that answers from a script keyed by `(method, path)` and records every request. A request the script
/// does not name PANICS: an unexpected call is a finding, not a default answer.
struct Venue {
    script: Vec<(&'static str, &'static str, Answer)>,
    calls: RefCell<Vec<Call>>,
}

impl Venue {
    fn new() -> Self {
        Venue { script: Vec::new(), calls: RefCell::new(Vec::new()) }
    }
    fn on(mut self, method: &'static str, path: &'static str, answer: Answer) -> Self {
        self.script.push((method, path, answer));
        self
    }
    fn sent(&self) -> Vec<String> {
        self.calls.borrow().iter().map(|(m, p, _)| format!("{m} {p}")).collect()
    }
    fn params_of(&self, method: &str, path: &str) -> Vec<(String, String)> {
        let calls = self.calls.borrow();
        let mut hits = calls.iter().filter(|(m, p, _)| m == method && p == path);
        let hit = hits.next().unwrap_or_else(|| panic!("no {method} {path} was sent"));
        assert!(hits.next().is_none(), "{method} {path} was sent twice");
        hit.2.clone()
    }
}

impl RestTransport for Venue {
    fn signed(
        &self,
        _base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Answer {
        self.calls.borrow_mut().push((
            method.to_string(),
            path.to_string(),
            params.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect(),
        ));
        self.script
            .iter()
            .find(|(m, p, _)| *m == method && *p == path)
            .unwrap_or_else(|| panic!("the script names no answer for {method} {path}"))
            .2
            .clone()
    }
    fn public(&self, _base_url: &str, path: &str, _params: &[(&str, String)]) -> Answer {
        panic!("class_prep reads nothing public: {path}")
    }
}

fn refusal(code: i64, msg: &str) -> Answer {
    Err(VenueApiError { code, msg: msg.to_string() })
}

fn rest_on(
    venue: Venue,
    base_url: &str,
    leverage: f64,
) -> BinancePerpRest<BinanceHmacSigner, Venue> {
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: None,
    };
    BinancePerpRest {
        signer: BinanceHmacSigner::new(&creds, || 0),
        transport: venue,
        base_url: base_url.to_string(),
        symbol: "TSLAUSDT".to_string(),
        properties: SymbolProperties::default(),
        leverage,
        link_id: None,
        algo_coids: Default::default(),
    }
}

const GET_RISK: &str = "GET /fapi/v2/positionRisk";
const GET_BRACKET: &str = "GET /fapi/v1/leverageBracket";
const POST_LEVERAGE: &str = "POST /fapi/v1/leverage";
const POST_AGREEMENT: &str = "POST /fapi/v1/stock/contract";

/// `TSLAUSDT` as the demo host answered it before any leverage was set: leverage `"0"`, flat, cross.
fn never_traded() -> Answer {
    Ok(json!([{
        "symbol": "TSLAUSDT", "positionAmt": "0.00", "marginType": "cross", "positionSide": "BOTH",
        "leverage": "0", "isolatedMargin": "0.00000000"
    }]))
}

/// The bracket the demo host stated for `TSLAUSDT` (first bracket 5x).
fn tsla_bracket() -> Answer {
    Ok(json!([{
        "symbol": "TSLAUSDT",
        "brackets": [
            {"bracket": 1, "initialLeverage": 5, "notionalCap": 5000, "notionalFloor": 0},
            {"bracket": 2, "initialLeverage": 4, "notionalCap": 10000, "notionalFloor": 5000}
        ]
    }]))
}

fn leverage_ok() -> Answer {
    Ok(json!({"leverage": 2, "maxNotionalValue": "80000", "symbol": "TSLAUSDT"}))
}

fn venue_for_a_write() -> Venue {
    Venue::new()
        .on("GET", "/fapi/v2/positionRisk", never_traded())
        .on("GET", "/fapi/v1/leverageBracket", tsla_bracket())
        .on("POST", "/fapi/v1/leverage", leverage_ok())
}

#[test]
fn a_never_traded_symbol_gets_one_leverage_write_capped_at_its_bracket() {
    // Configured 2x, bracket maximum 5x -> 2.
    let rest = rest_on(venue_for_a_write(), DEMO_FAPI_REST, 2.0);
    assert_eq!(ensure_symbol_leverage(&rest), LeverageHeal::Written(2));
    assert_eq!(rest.transport.sent(), [GET_RISK, GET_BRACKET, POST_LEVERAGE]);
    assert_eq!(
        rest.transport.params_of("POST", "/fapi/v1/leverage"),
        [("symbol".to_string(), "TSLAUSDT".to_string()), ("leverage".to_string(), "2".to_string())],
    );
    // Configured 20x, bracket maximum 5x -> 5, never above what the symbol allows.
    let rest = rest_on(venue_for_a_write(), DEMO_FAPI_REST, 20.0);
    assert_eq!(ensure_symbol_leverage(&rest), LeverageHeal::Written(5));
    assert_eq!(
        rest.transport.params_of("POST", "/fapi/v1/leverage")[1],
        ("leverage".to_string(), "5".to_string())
    );
    // A fractional configured figure is written as the whole number below it (the venue takes integers).
    let rest = rest_on(venue_for_a_write(), DEMO_FAPI_REST, 2.9);
    assert_eq!(ensure_symbol_leverage(&rest), LeverageHeal::Written(2));
}

#[test]
fn a_symbol_that_holds_a_leverage_is_never_written() {
    let held = Ok(json!([{
        "symbol": "TSLAUSDT", "positionAmt": "0.00", "marginType": "cross", "positionSide": "BOTH",
        "leverage": "3"
    }]));
    let rest = rest_on(Venue::new().on("GET", "/fapi/v2/positionRisk", held), DEMO_FAPI_REST, 2.0);
    assert_eq!(ensure_symbol_leverage(&rest), LeverageHeal::Held(3.0));
    assert_eq!(rest.transport.sent(), [GET_RISK], "one read, no bracket, no write");
}

#[test]
fn a_symbol_that_is_not_provably_flat_or_unreadable_is_never_written() {
    // A leverage of 0 with a position is a surprise: left alone.
    let open = Ok(json!([{
        "symbol": "TSLAUSDT", "positionAmt": "0.10", "positionSide": "BOTH", "leverage": "0"
    }]));
    let rest = rest_on(Venue::new().on("GET", "/fapi/v2/positionRisk", open), DEMO_FAPI_REST, 2.0);
    assert!(matches!(ensure_symbol_leverage(&rest), LeverageHeal::Unread(_)));
    assert_eq!(rest.transport.sent(), [GET_RISK]);
    // No amount at all is not "flat".
    let no_amt = Ok(json!([{"symbol": "TSLAUSDT", "leverage": "0"}]));
    let rest =
        rest_on(Venue::new().on("GET", "/fapi/v2/positionRisk", no_amt), DEMO_FAPI_REST, 2.0);
    assert!(matches!(ensure_symbol_leverage(&rest), LeverageHeal::Unread(_)));
    assert_eq!(rest.transport.sent(), [GET_RISK]);
    // No row for the symbol, and a failed read.
    let other = Ok(json!([{"symbol": "ETHUSDT", "positionAmt": "0", "leverage": "0"}]));
    let rest = rest_on(Venue::new().on("GET", "/fapi/v2/positionRisk", other), DEMO_FAPI_REST, 2.0);
    assert!(matches!(ensure_symbol_leverage(&rest), LeverageHeal::Unread(_)));
    let failed = refusal(-1003, "Too many requests");
    let rest =
        rest_on(Venue::new().on("GET", "/fapi/v2/positionRisk", failed), DEMO_FAPI_REST, 2.0);
    assert!(matches!(ensure_symbol_leverage(&rest), LeverageHeal::Unread(_)));
    assert_eq!(rest.transport.sent(), [GET_RISK]);
}

#[test]
fn an_unreadable_bracket_writes_the_configured_figure_and_a_refusal_is_named() {
    let venue = Venue::new()
        .on("GET", "/fapi/v2/positionRisk", never_traded())
        .on("GET", "/fapi/v1/leverageBracket", refusal(-1000, "unknown"))
        .on("POST", "/fapi/v1/leverage", leverage_ok());
    let rest = rest_on(venue, DEMO_FAPI_REST, 2.0);
    assert_eq!(ensure_symbol_leverage(&rest), LeverageHeal::Written(2));
    // The venue refuses the write: the sentence carries its words and the hand fix.
    let venue = Venue::new()
        .on("GET", "/fapi/v2/positionRisk", never_traded())
        .on("GET", "/fapi/v1/leverageBracket", tsla_bracket())
        .on("POST", "/fapi/v1/leverage", refusal(-4028, "Leverage 2 is not valid"));
    let rest = rest_on(venue, DEMO_FAPI_REST, 2.0);
    let LeverageHeal::Refused(why) = ensure_symbol_leverage(&rest) else {
        panic!("expected a refusal")
    };
    assert!(
        why.contains("TSLAUSDT") && why.contains("-4028") && why.contains("Binance app"),
        "{why}"
    );
    // A configured figure below 1 fits no symbol: nothing is written.
    let rest = rest_on(venue_for_a_write(), DEMO_FAPI_REST, 0.5);
    assert!(matches!(ensure_symbol_leverage(&rest), LeverageHeal::Unread(_)));
    assert_eq!(rest.transport.sent(), [GET_RISK, GET_BRACKET]);
}

#[test]
fn the_mainnet_host_is_never_read_or_written() {
    // The script names nothing: any request would panic.
    let rest = rest_on(Venue::new(), MAINNET_FAPI_REST, 2.0);
    assert_eq!(ensure_symbol_leverage(&rest), LeverageHeal::LeftAlone);
    assert!(rest.transport.sent().is_empty());
    let rest = rest_on(Venue::new(), "https://proxy.example", 2.0);
    assert_eq!(ensure_symbol_leverage(&rest), LeverageHeal::LeftAlone);
}

#[test]
fn a_tradfi_reject_signs_the_agreement_once_per_window_on_the_demo_host() {
    let memory = AgreementMemory::new();
    let venue = Venue::new().on("POST", "/fapi/v1/stock/contract", Ok(json!({"code": 200})));
    let rest = rest_on(venue, DEMO_FAPI_REST, 2.0);
    let t0 = Instant::now();
    let msg = "Please sign TradFi-Perps agreement contract fapi.";
    let said = agreement_sentence_at(&memory, &rest, msg, t0);
    assert!(said.contains("signed it just now") && said.contains("send the order again"), "{said}");
    assert!(said.contains("-4411") && said.contains(msg), "{said}");
    assert_eq!(rest.transport.sent(), [POST_AGREEMENT]);
    assert!(rest.transport.params_of("POST", "/fapi/v1/stock/contract").is_empty());
    // A burst of rejects inside the window is the SAME signature.
    let again = agreement_sentence_at(&memory, &rest, msg, t0 + Duration::from_secs(5));
    assert_eq!(again, said);
    assert_eq!(rest.transport.sent().len(), 1, "no second POST inside the window");
    // The window ends: the venue is asked again.
    let _ = agreement_sentence_at(
        &memory,
        &rest,
        msg,
        t0 + crate::modes::REPROBE_WINDOW + Duration::from_secs(1),
    );
    assert_eq!(rest.transport.sent().len(), 2);
}

#[test]
fn an_unreadable_2xx_is_a_signature_that_landed_and_a_refusal_is_named() {
    // MEASURED: the venue answers the POST with a plain-text 2xx body, which the transport reports as the
    // ambiguous error of a write. The reject still says it was sent, and the NEXT order is the proof.
    let ambiguous = refusal(E_TIMEOUT_AMBIGUOUS, "bad json: expected value at line 1 column 1");
    let rest =
        rest_on(Venue::new().on("POST", "/fapi/v1/stock/contract", ambiguous), DEMO_FAPI_REST, 2.0);
    let said = agreement_sentence_at(&AgreementMemory::new(), &rest, "m", Instant::now());
    assert!(said.contains("signed it just now") && said.contains("plain text"), "{said}");
    assert!(said.contains("sign the agreement in the Binance app"), "{said}");
    // A real refusal is worded and does not claim a signature.
    let denied = refusal(-2015, "Invalid API-key, IP, or permissions for action");
    let rest =
        rest_on(Venue::new().on("POST", "/fapi/v1/stock/contract", denied), DEMO_FAPI_REST, 2.0);
    let said = agreement_sentence_at(&AgreementMemory::new(), &rest, "m", Instant::now());
    assert!(!said.contains("signed it just now"), "{said}");
    assert!(said.contains("-2015") && said.contains("Binance app"), "{said}");
}

#[test]
fn a_live_host_signs_nothing_and_names_the_hand_step() {
    let rest = rest_on(Venue::new(), MAINNET_FAPI_REST, 2.0);
    let said = agreement_sentence_at(&AgreementMemory::new(), &rest, "msg", Instant::now());
    assert!(
        said.contains("signs no agreement for a LIVE account") && said.contains("Binance app"),
        "{said}"
    );
    assert!(rest.transport.sent().is_empty());
    assert_eq!(said, agreement_hand_step("msg"));
    // The pure reject text on a live host is the same hand step.
    assert_eq!(order_reject_reason(E_TRADFI_AGREEMENT, "msg", MAINNET_FAPI_REST), said);
}

#[test]
fn the_position_at_leverage_refusal_is_worded_for_both_networks() {
    let msg = "Exceeded the maximum allowable position at current leverage.";
    let demo = order_reject_reason(E_POSITION_AT_LEVERAGE, msg, DEMO_FAPI_REST);
    assert!(demo.contains("-2027") && demo.contains(msg), "{demo}");
    assert!(demo.contains("given a leverage by the bridge before its first order"), "{demo}");
    let live = order_reject_reason(E_POSITION_AT_LEVERAGE, msg, MAINNET_FAPI_REST);
    assert!(live.contains("Binance app") && live.contains("never traded"), "{live}");
    // Every other code keeps the venue's own text.
    assert_eq!(
        order_reject_reason(-2019, "Margin is insufficient.", DEMO_FAPI_REST),
        "Margin is insufficient."
    );
}
