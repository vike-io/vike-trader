//! Offline pins for `crate::modes` over a scripted venue (NO network): every branch of the two probes, the
//! start's call ORDER, and the words an order carries when the position mode blocks it.
//!
//! | test | what it pins |
//! |---|---|
//! | [`the_two_mode_parsers_read_the_documented_shapes_and_refuse_the_rest`] | the pure parsers |
//! | [`a_one_way_account_costs_one_read_and_no_write`] / [`a_cross_symbol_costs_one_read_and_no_write`] | a healthy account is never written |
//! | [`a_hedge_account_is_switched_to_one_way_by_one_write`] / [`an_isolated_symbol_is_switched_to_cross_by_one_write`] | the heal and its wire |
//! | [`a_refused_switch_is_named_and_followed_by_no_other_write`] | -4067/-4068/-4047/-4048/-2015/other: named, ONE write, nothing else |
//! | [`no_need_to_change_is_success`] | -4059/-4046 are success, not an error |
//! | [`an_unreadable_mode_writes_nothing`] | a failed or odd read never writes |
//! | [`a_perp_start_probes_both_modes_before_it_writes_the_leverage`] | the start's call order, on every branch |
//! | [`a_hedge_reject_carries_the_hedge_words_on_every_submit_path`] | single, batch (element and chunk) and stop |
//! | [`a_mainnet_or_unnamed_host_is_never_read_or_written`] | the owner's ruling: the mainnet host (or an unnamed one) is left alone, the start posts only its leverage |
//! | [`a_hedge_reject_on_mainnet_names_the_hand_fix_and_not_a_bridge_switch`] | the live order refusal is in words with the hand fix |
//! | [`a_hedge_reject_retries_the_switch_once_per_window`] | a refused start never retried: the first `-4061` asks again, a burst is one probe, the window ends |
//! | [`the_retry_names_what_still_blocks_and_never_probes_where_it_must_not`] | the retry's refusal, "one-way now", unreadable, another code and the mainnet host |
//! | [`the_pre_probe_heal_is_the_starts_switch_and_reports_only_a_refusal`] | the lane's heal before its leverage probe: the start's switch, a refusal in words, mainnet untouched |

use std::cell::RefCell;

use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::{BinanceHmacSigner, Signer};
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_model::events::Event;
use vike_model::{OrderRequest, SymbolProperties};
use vike_secrets::credentials::Credentials;

use super::*;
use crate::exec::perp_start_writes;
use crate::perp::{DEMO_FAPI_REST, MAINNET_FAPI_REST, PATH_SET_LEVERAGE};

/// `(method, path, params)` of one request the bridge sent.
type Call = (String, String, Vec<(String, String)>);
type Answer = Result<Value, VenueApiError>;

/// A venue that answers from a script keyed by `(method, path)` and records every request. A request the
/// script does not name PANICS: an unexpected call is a finding, not a default answer.
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
    fn answer(&self, method: &str, path: &str, params: &[(&str, String)]) -> Answer {
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
    /// `METHOD path` of every request, in order.
    fn sent(&self) -> Vec<String> {
        self.calls.borrow().iter().map(|(m, p, _)| format!("{m} {p}")).collect()
    }
    /// The params of the one request to `method path`.
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
        self.answer(method, path, params)
    }
    fn public(&self, _base_url: &str, path: &str, _params: &[(&str, String)]) -> Answer {
        panic!("a mode probe reads nothing public: {path}")
    }
}

fn refusal(code: i64, msg: &str) -> Answer {
    Err(VenueApiError { code, msg: msg.to_string() })
}

fn success() -> Answer {
    Ok(json!({"code": 200, "msg": "success"}))
}

fn dual(on: bool) -> Answer {
    Ok(json!({"dualSidePosition": on}))
}

/// One `positionRisk` row for BTCUSDT (a flat symbol) on `margin`.
fn risk(margin: &str) -> Answer {
    Ok(json!([{
        "symbol": "BTCUSDT", "positionAmt": "0.000", "marginType": margin,
        "positionSide": "BOTH", "leverage": "2", "isolatedMargin": "0.00000000"
    }]))
}

fn perp(venue: Venue) -> BinancePerpRest<BinanceHmacSigner, Venue> {
    perp_on(venue, DEMO_FAPI_REST)
}

fn perp_on(venue: Venue, base_url: &str) -> BinancePerpRest<BinanceHmacSigner, Venue> {
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: None,
    };
    BinancePerpRest {
        signer: BinanceHmacSigner::new(&creds, || 0),
        transport: venue,
        base_url: base_url.to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties::default(),
        leverage: 2.0,
        link_id: None,
        algo_coids: Default::default(),
    }
}

const GET_DUAL: &str = "GET /fapi/v1/positionSide/dual";
const POST_DUAL: &str = "POST /fapi/v1/positionSide/dual";
const GET_RISK: &str = "GET /fapi/v2/positionRisk";
const POST_MARGIN: &str = "POST /fapi/v1/marginType";
const POST_LEVERAGE: &str = "POST /fapi/v1/leverage";

fn dual_venue(read: Answer, write: Answer) -> Venue {
    Venue::new().on("GET", PATH_POSITION_MODE, read).on("POST", PATH_POSITION_MODE, write)
}

fn margin_venue(read: Answer, write: Answer) -> Venue {
    Venue::new().on("GET", PATH_POSITIONS, read).on("POST", PATH_MARGIN_TYPE, write)
}

#[test]
fn the_two_mode_parsers_read_the_documented_shapes_and_refuse_the_rest() {
    assert_eq!(dual_side_position(&json!({"dualSidePosition": true})), Some(true));
    assert_eq!(dual_side_position(&json!({"dualSidePosition": false})), Some(false));
    assert_eq!(dual_side_position(&json!({"dualSidePosition": "true"})), Some(true));
    assert_eq!(dual_side_position(&json!({"dualSidePosition": "False"})), Some(false));
    for odd in
        [json!({}), json!({"dualSidePosition": 1}), json!({"dualSidePosition": null}), json!([])]
    {
        assert_eq!(dual_side_position(&odd), None, "{odd}");
    }

    let rows = |m: &str| json!([{"symbol": "BTCUSDT", "marginType": m}]);
    assert_eq!(margin_is_isolated(&rows("isolated"), "BTCUSDT"), Ok(true));
    assert_eq!(margin_is_isolated(&rows("ISOLATED"), "btcusdt"), Ok(true));
    assert_eq!(margin_is_isolated(&rows("cross"), "BTCUSDT"), Ok(false));
    // Hedge mode answers two rows for the symbol; both carry the margin type.
    let hedge = json!([
        {"symbol": "BTCUSDT", "positionSide": "LONG", "marginType": "isolated"},
        {"symbol": "BTCUSDT", "positionSide": "SHORT", "marginType": "isolated"},
    ]);
    assert_eq!(margin_is_isolated(&hedge, "BTCUSDT"), Ok(true));
    // Another symbol's row, an empty list, a missing or odd marginType are never read as a margin type.
    assert!(margin_is_isolated(&rows("cross"), "ETHUSDT").is_err());
    assert!(margin_is_isolated(&json!([]), "BTCUSDT").is_err());
    assert!(margin_is_isolated(&json!([{"symbol": "BTCUSDT"}]), "BTCUSDT").is_err());
    assert!(margin_is_isolated(&rows("portfolio"), "BTCUSDT").is_err());
    assert!(margin_is_isolated(&json!({"symbol": "BTCUSDT"}), "BTCUSDT").is_err());
}

#[test]
fn a_one_way_account_costs_one_read_and_no_write() {
    let rest = perp(dual_venue(dual(false), refusal(-9999, "a write here is a finding")));
    assert_eq!(ensure_one_way(&rest), ModeOutcome::Right);
    assert_eq!(rest.transport.sent(), [GET_DUAL]);
}

#[test]
fn a_cross_symbol_costs_one_read_and_no_write() {
    let rest = perp(margin_venue(risk("cross"), refusal(-9999, "a write here is a finding")));
    assert_eq!(ensure_cross_margin(&rest), ModeOutcome::Right);
    assert_eq!(rest.transport.sent(), [GET_RISK]);
    assert_eq!(
        rest.transport.params_of("GET", PATH_POSITIONS),
        [("symbol".to_string(), "BTCUSDT".to_string())],
        "the read is for the client's own symbol"
    );
}

#[test]
fn a_hedge_account_is_switched_to_one_way_by_one_write() {
    let rest = perp(dual_venue(dual(true), success()));
    assert_eq!(ensure_one_way(&rest), ModeOutcome::Switched);
    assert_eq!(rest.transport.sent(), [GET_DUAL, POST_DUAL]);
    assert_eq!(
        rest.transport.params_of("POST", PATH_POSITION_MODE),
        [("dualSidePosition".to_string(), "false".to_string())]
    );
}

#[test]
fn an_isolated_symbol_is_switched_to_cross_by_one_write() {
    let rest = perp(margin_venue(risk("isolated"), success()));
    assert_eq!(ensure_cross_margin(&rest), ModeOutcome::Switched);
    assert_eq!(rest.transport.sent(), [GET_RISK, POST_MARGIN]);
    let params = rest.transport.params_of("POST", PATH_MARGIN_TYPE);
    assert!(params.contains(&("symbol".to_string(), "BTCUSDT".to_string())), "{params:?}");
    assert!(params.contains(&("marginType".to_string(), "CROSSED".to_string())), "{params:?}");
}

#[test]
fn no_need_to_change_is_success() {
    // Somebody switched the mode between our read and our write: the venue says "no need", which is the
    // state we wanted.
    let rest = perp(dual_venue(dual(true), refusal(-4059, "No need to change position side.")));
    assert_eq!(ensure_one_way(&rest), ModeOutcome::Right);
    let rest =
        perp(margin_venue(risk("isolated"), refusal(-4046, "No need to change margin type.")));
    assert_eq!(ensure_cross_margin(&rest), ModeOutcome::Right);
}

#[test]
fn a_refused_switch_is_named_and_followed_by_no_other_write() {
    // The position-mode switch: each venue refusal is worded with what it blocks, keeps the venue's own
    // code and text, says what to do, and is the ONLY write (the venue judges flatness; nothing retries).
    for (code, msg, blocker) in [
        (-4067, "Position side cannot be changed if there exists open orders.", "RESTING ORDERS"),
        (-4068, "Position side cannot be changed if there exists position.", "OPEN POSITION"),
        (
            -2015,
            "Invalid API-key, IP, or permissions for action.",
            "may not change account settings",
        ),
        (-4999, "something new", "refused the switch"),
    ] {
        let rest = perp(dual_venue(dual(true), refusal(code, msg)));
        let ModeOutcome::Refused(why) = ensure_one_way(&rest) else {
            panic!("{code} was not refused")
        };
        assert!(
            why.contains("HEDGE") && why.contains("ONE-WAY") && why.contains(blocker),
            "{code}: {why}"
        );
        assert!(why.contains(&code.to_string()) && why.contains(msg), "{code}: {why}");
        assert!(why.contains("`active` off and on"), "{code}: {why}");
        assert_eq!(rest.transport.sent(), [GET_DUAL, POST_DUAL], "{code}: one write, then nothing");
    }
    // The margin switch likewise, and the sentence says orders are NOT refused.
    for (code, msg, blocker) in [
        (-4047, "Margin type cannot be changed if there exists open orders.", "RESTING ORDERS"),
        (-4048, "Margin type cannot be changed if there exists position.", "OPEN POSITION"),
        (
            -4168,
            "Unable to adjust to isolated-margin mode under the Multi-Assets margin mode.",
            "refused the switch",
        ),
    ] {
        let rest = perp(margin_venue(risk("isolated"), refusal(code, msg)));
        let ModeOutcome::Refused(why) = ensure_cross_margin(&rest) else { panic!("{code}") };
        assert!(
            why.contains("BTCUSDT") && why.contains("ISOLATED") && why.contains(blocker),
            "{code}: {why}"
        );
        assert!(why.contains("Orders are NOT refused") && why.contains(msg), "{code}: {why}");
        assert_eq!(
            rest.transport.sent(),
            [GET_RISK, POST_MARGIN],
            "{code}: one write, then nothing"
        );
    }
}

#[test]
fn an_unreadable_mode_writes_nothing() {
    for read in [refusal(-1021, "Timestamp outside recvWindow"), Ok(json!({"nothing": 1}))] {
        let rest = perp(dual_venue(read, success()));
        assert!(matches!(ensure_one_way(&rest), ModeOutcome::Unread(_)));
        assert_eq!(rest.transport.sent(), [GET_DUAL], "a failed probe never writes");
    }
    for read in [refusal(-2015, "permissions"), Ok(json!([])), risk("portfolio")] {
        let rest = perp(margin_venue(read, success()));
        assert!(matches!(ensure_cross_margin(&rest), ModeOutcome::Unread(_)));
        assert_eq!(rest.transport.sent(), [GET_RISK], "a failed probe never writes");
    }
}

/// The start's venue writes, in order, on every branch: the position mode, the mounted symbol's margin type,
/// THEN the leverage; and no refusal or unreadable mode ever skips the leverage write.
#[test]
fn a_perp_start_probes_both_modes_before_it_writes_the_leverage() {
    let start = |venue: Venue| {
        let rest = perp(venue);
        perp_start_writes(&rest, "BTCUSDT.P", 2.0);
        rest.transport.sent()
    };
    let leverage = || ("POST", PATH_SET_LEVERAGE, Ok(json!({"leverage": 2, "symbol": "BTCUSDT"})));
    let build = |dual_read: Answer, dual_write: Answer, risk_read: Answer, margin_write: Answer| {
        let (m, p, a) = leverage();
        dual_venue(dual_read, dual_write)
            .on("GET", PATH_POSITIONS, risk_read)
            .on("POST", PATH_MARGIN_TYPE, margin_write)
            .on(m, p, a)
    };

    // A healthy account: two reads and the leverage, no mode write (a start with no stated leverage too).
    assert_eq!(
        start(build(dual(false), success(), risk("cross"), success())),
        [GET_DUAL, GET_RISK, POST_LEVERAGE]
    );
    // A hedge, isolated account: both healed, leverage last.
    assert_eq!(
        start(build(dual(true), success(), risk("isolated"), success())),
        [GET_DUAL, POST_DUAL, GET_RISK, POST_MARGIN, POST_LEVERAGE]
    );
    // Both switches refused: still one write each, still the leverage.
    assert_eq!(
        start(build(
            dual(true),
            refusal(-4068, "position"),
            risk("isolated"),
            refusal(-4048, "position")
        )),
        [GET_DUAL, POST_DUAL, GET_RISK, POST_MARGIN, POST_LEVERAGE]
    );
    // Nothing readable: no mode write, the leverage as ever.
    assert_eq!(
        start(build(refusal(-1021, "t"), success(), refusal(-1021, "t"), success())),
        [GET_DUAL, GET_RISK, POST_LEVERAGE]
    );

    // The outcomes themselves, from the entry the start calls.
    let rest = perp(build(dual(true), success(), risk("isolated"), refusal(-4048, "position")));
    let (position, margin) = heal_at_start(&rest);
    assert_eq!(position, ModeOutcome::Switched);
    assert!(matches!(margin, ModeOutcome::Refused(_)), "{margin:?}");
}

fn order(kind: &str) -> OrderRequest {
    serde_json::from_value(json!({
        "client_order_id": "c1", "venue": "binance", "symbol": "BTCUSDT.P", "side": 1, "qty": 0.005,
        "order_type": kind, "price": 60000.0, "trigger_price": 59000.0,
    }))
    .expect("an order")
}

fn reason_of(events: &[Event]) -> String {
    events
        .iter()
        .find_map(|e| match e {
            Event::OrderRejected(r) => Some(r.reason.to_string()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no OrderRejected in {events:?}"))
}

/// A one-way order on a hedge-mode account is rejected `-4061` by the venue; the operator must read WHY.
#[test]
fn a_hedge_reject_carries_the_hedge_words_on_every_submit_path() {
    let venue_text = "Order's position side does not match user's setting.";
    let words = |why: &str| {
        assert!(why.contains("HEDGE") && why.contains("ONE-WAY") && why.contains("-4061"), "{why}");
        assert!(why.contains(venue_text), "the venue's own text rides along: {why}");
        assert!(why.contains("`active` off and on"), "{why}");
    };
    // The first -4061 asks the venue about the mode again (`reject_reason`): here it cannot be read, so every
    // path carries the standard sentence (the re-probe's own sentences are pinned below).
    let rejecting = |path: &'static str| {
        Venue::new().on("POST", path, refusal(-4061, venue_text)).on(
            "GET",
            PATH_POSITION_MODE,
            refusal(-1021, "unreadable"),
        )
    };

    // A plain order.
    let rest = perp(rejecting("/fapi/v1/order"));
    words(&reason_of(&rest.submit_order(&order("limit"))));
    // A stop (the Algo Service).
    let rest = perp(rejecting("/fapi/v1/algoOrder"));
    words(&reason_of(&rest.submit_order(&order("stop"))));
    // A batch refused whole, and a batch whose element is refused.
    let rest = perp(rejecting("/fapi/v1/batchOrders"));
    words(&reason_of(&rest.submit_batch(&[order("limit")])));
    let element = Venue::new()
        .on("POST", "/fapi/v1/batchOrders", Ok(json!([{"code": -4061, "msg": venue_text}])))
        .on("GET", PATH_POSITION_MODE, refusal(-1021, "unreadable"));
    words(&reason_of(&perp(element).submit_batch(&[order("limit")])));

    // Every OTHER venue text is untouched: the words are for -4061 alone.
    let rest =
        perp(Venue::new().on("POST", "/fapi/v1/order", refusal(-2019, "Margin is insufficient.")));
    assert_eq!(reason_of(&rest.submit_order(&order("limit"))), "Margin is insufficient.");
    assert_eq!(order_reject_reason(0, "plain", DEMO_FAPI_REST), "plain");
    assert_eq!(order_reject_reason(0, "plain", MAINNET_FAPI_REST), "plain");
}

/// OWNER RULING 2026-10-26: no exchange write on a LIVE account. The gate is the host, so the mainnet host
/// (whatever tier label the account row states) and any host this crate does not name as demo are left
/// alone: no read and no mode write, and the start still posts its (pre-existing) leverage.
#[test]
fn a_mainnet_or_unnamed_host_is_never_read_or_written() {
    assert!(writes_modes(DEMO_FAPI_REST));
    assert!(!writes_modes(MAINNET_FAPI_REST));
    assert!(!writes_modes("https://demo-fapi.example"));
    assert!(!writes_modes(""));

    for host in [MAINNET_FAPI_REST, "https://demo-fapi.example"] {
        // A hedge, isolated account whose every write would be accepted: a call here is the defect.
        let all = || {
            dual_venue(dual(true), success()).on("GET", PATH_POSITIONS, risk("isolated")).on(
                "POST",
                PATH_MARGIN_TYPE,
                success(),
            )
        };
        let rest = perp_on(all(), host);
        assert_eq!(ensure_one_way(&rest), ModeOutcome::LeftAlone, "{host}");
        assert_eq!(ensure_cross_margin(&rest), ModeOutcome::LeftAlone, "{host}");
        assert_eq!(
            heal_at_start(&rest),
            (ModeOutcome::LeftAlone, ModeOutcome::LeftAlone),
            "{host}"
        );
        assert_eq!(heal_symbol(&rest), ModeOutcome::LeftAlone, "{host}");
        assert!(rest.transport.sent().is_empty(), "{host}: {:?}", rest.transport.sent());

        // The start: the leverage write is the one pre-existing write and stays; no mode is read or written.
        let (m, p, a) =
            ("POST", PATH_SET_LEVERAGE, Ok(json!({"leverage": 2, "symbol": "BTCUSDT"})));
        let rest = perp_on(all().on(m, p, a), host);
        perp_start_writes(&rest, "BTCUSDT.P", 2.0);
        assert_eq!(rest.transport.sent(), [POST_LEVERAGE], "{host}");
    }
}

/// A hedge refusal on a MAINNET host tells the operator the bridge does not change a live account's mode and
/// names the hand fix; on demo it keeps the sentence that says the bridge switches at start.
#[test]
fn a_hedge_reject_on_mainnet_names_the_hand_fix_and_not_a_bridge_switch() {
    let text = "Order's position side does not match user's setting.";
    let live = order_reject_reason(-4061, text, MAINNET_FAPI_REST);
    assert!(live.contains("HEDGE") && live.contains("ONE-WAY") && live.contains("-4061"), "{live}");
    assert!(live.contains(text), "the venue's own text rides along: {live}");
    assert!(live.contains("changes no LIVE account's mode"), "{live}");
    assert!(live.contains("One-way Mode in the Binance Futures preferences"), "{live}");
    assert!(!live.contains("switches the account to one-way by itself"), "{live}");
    let demo = order_reject_reason(-4061, text, DEMO_FAPI_REST);
    assert!(demo.contains("switches the account to one-way by itself"), "{demo}");

    // And through a real submit path on a mainnet-host client.
    let rest =
        perp_on(Venue::new().on("POST", "/fapi/v1/order", refusal(-4061, text)), MAINNET_FAPI_REST);
    let why = reason_of(&rest.submit_order(&order("limit")));
    assert!(why.contains("changes no LIVE account's mode"), "{why}");
}

// ---- the first -4061 asks the venue about the position mode again --------------------------------------

const HEDGE_TEXT: &str = "Order's position side does not match user's setting.";

/// **A hedge reject on the demo host retries the switch**: the account was refused at start (it held something),
/// is flat now, and the order that met `-4061` is told so. A rejected batch is ONE probe (the answer serves the
/// rejects that follow it inside the window), and the window ends: the next reject probes again.
#[test]
fn a_hedge_reject_retries_the_switch_once_per_window() {
    let reprobe = Reprobe::new();
    let t0 = Instant::now();
    let rest = perp(dual_venue(dual(true), success()));

    let why = reject_reason_at(&reprobe, &rest, -4061, HEDGE_TEXT, t0);
    assert!(why.contains("switched the account to one-way just now"), "{why}");
    assert!(why.contains("HEDGE") && why.contains("ONE-WAY") && why.contains("-4061"), "{why}");
    assert!(why.contains(HEDGE_TEXT) && why.contains("send the order again"), "{why}");
    assert_eq!(rest.transport.sent(), [GET_DUAL, POST_DUAL], "one read and ONE write");
    assert_eq!(
        rest.transport.params_of("POST", PATH_POSITION_MODE),
        [("dualSidePosition".to_string(), "false".to_string())]
    );

    // The next element of the same rejected batch: the same sentence, no new request.
    let soon = t0 + Duration::from_secs(2);
    assert_eq!(reject_reason_at(&reprobe, &rest, -4061, HEDGE_TEXT, soon), why);
    assert_eq!(rest.transport.sent().len(), 2, "a burst is one probe");

    // Past the window the next reject asks the venue again.
    let later = t0 + REPROBE_WINDOW + Duration::from_secs(1);
    reject_reason_at(&reprobe, &rest, -4061, HEDGE_TEXT, later);
    assert_eq!(rest.transport.sent().len(), 4, "{:?}", rest.transport.sent());
}

/// A switch the venue refuses again is named with the venue's blocker and the order's own `-4061`; an account
/// that reads one-way by the time of the probe (switched elsewhere) says so; an unreadable mode keeps the
/// standard sentence; a code other than `-4061` and a MAINNET host never probe.
#[test]
fn the_retry_names_what_still_blocks_and_never_probes_where_it_must_not() {
    let t0 = Instant::now();

    // Still not flat: the standard hedge words, the blocker, the order's own text, and no further write.
    let rest = perp(dual_venue(dual(true), refusal(-4068, "position")));
    let why = reject_reason_at(&Reprobe::new(), &rest, -4061, HEDGE_TEXT, t0);
    assert!(
        why.contains("HEDGE") && why.contains("ONE-WAY") && why.contains("OPEN POSITION"),
        "{why}"
    );
    assert!(why.contains(&format!("-4061: {HEDGE_TEXT}")), "{why}");
    assert_eq!(rest.transport.sent(), [GET_DUAL, POST_DUAL]);

    // One-way by the time it looks: no write, and the order is told to go again.
    let rest = perp(dual_venue(dual(false), refusal(-9999, "a write here is a finding")));
    let why = reject_reason_at(&Reprobe::new(), &rest, -4061, HEDGE_TEXT, t0);
    assert!(why.contains("reads ONE-WAY now") && why.contains("send the order again"), "{why}");
    assert_eq!(rest.transport.sent(), [GET_DUAL]);

    // Unreadable: the standard sentence, remembered for the window (the second ask sends nothing).
    let reprobe = Reprobe::new();
    let rest = perp(dual_venue(refusal(-1021, "t"), refusal(-9999, "a write here is a finding")));
    let standard = order_reject_reason(-4061, HEDGE_TEXT, DEMO_FAPI_REST);
    assert_eq!(reject_reason_at(&reprobe, &rest, -4061, HEDGE_TEXT, t0), standard);
    assert_eq!(reject_reason_at(&reprobe, &rest, -4061, HEDGE_TEXT, t0), standard);
    assert_eq!(rest.transport.sent(), [GET_DUAL]);

    // Another code is the venue's own text and asks nothing; a mainnet host asks nothing either.
    let rest = perp(Venue::new());
    assert_eq!(
        reject_reason_at(&Reprobe::new(), &rest, -2019, "Margin is insufficient.", t0),
        "Margin is insufficient."
    );
    let live = perp_on(Venue::new(), MAINNET_FAPI_REST);
    let why = reject_reason_at(&Reprobe::new(), &live, -4061, HEDGE_TEXT, t0);
    assert_eq!(why, order_reject_reason(-4061, HEDGE_TEXT, MAINNET_FAPI_REST));
    assert!(live.transport.sent().is_empty());
}

/// The lane's pre-probe heal (`VenueAdapter::heal_modes`) is the start's own switch: a hedge, isolated account
/// is put right (position mode, then margin type), a refusal is returned in the venue's words, a mainnet host
/// touches nothing.
#[test]
fn the_pre_probe_heal_is_the_starts_switch_and_reports_only_a_refusal() {
    use crate::exec::modes_refusal;
    let build = |dual_write: Answer, margin_write: Answer| {
        dual_venue(dual(true), dual_write).on("GET", PATH_POSITIONS, risk("isolated")).on(
            "POST",
            PATH_MARGIN_TYPE,
            margin_write,
        )
    };

    let rest = perp(build(success(), success()));
    assert_eq!(modes_refusal(heal_at_start(&rest)), Ok(()));
    assert_eq!(rest.transport.sent(), [GET_DUAL, POST_DUAL, GET_RISK, POST_MARGIN]);

    let rest = perp(build(refusal(-4068, "position"), refusal(-4048, "position")));
    let why = modes_refusal(heal_at_start(&rest)).unwrap_err();
    assert!(why.contains("HEDGE") && why.contains("ISOLATED"), "both refusals are said: {why}");

    // A mainnet host: left alone, so nothing is refused and nothing was sent.
    let rest = perp_on(build(success(), success()), MAINNET_FAPI_REST);
    assert_eq!(modes_refusal(heal_at_start(&rest)), Ok(()));
    assert!(rest.transport.sent().is_empty());
}
