//! Offline pins for `crate::account_modes`: every branch of the start probe over a scripted OKX that
//! RECORDS each request. The recording is the point: "no write when the account is not flat" and "one
//! write, then a read back" are claims about the request list, and only the list can check them.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{Value, json};
use vike_bridge_core::signer::OkxV5Signer;
use vike_bridge_core::transport::VenueApiError;
use vike_secrets::credentials::Credentials;

use super::*;

const CONFIG: &str = "GET /api/v5/account/config";
const POSITIONS: &str = "GET /api/v5/account/positions";
const ORDERS: &str = "GET /api/v5/trade/orders-pending";
const STOPS: &str = "GET /api/v5/trade/orders-algo-pending";
const WRITE: &str = "POST /api/v5/account/set-position-mode";

type Answer = Result<Value, VenueApiError>;

/// Answers per `"METHOD path"`, in order; the last answer repeats. A request nobody scripted panics:
/// the probe must not make a call the test did not expect.
struct Venue {
    script: HashMap<&'static str, Vec<Answer>>,
    seen: Mutex<HashMap<&'static str, usize>>,
    calls: Mutex<Vec<(String, Value)>>,
    /// Whether this venue plays the SIMULATED network (the `x-simulated-trading` header): `true` for every
    /// test but the ones that play the real one.
    simulated: bool,
}

fn envelope(rows: Value) -> Answer {
    Ok(json!({"code": "0", "msg": "", "data": rows}))
}

fn refusal(code: &str, msg: &str) -> Answer {
    Ok(json!({"code": code, "msg": msg, "data": []}))
}

fn config(pos_mode: &str, level: &str) -> Answer {
    envelope(json!([{"posMode": pos_mode, "acctLv": level, "uid": "1", "mainUid": "1"}]))
}

impl Venue {
    fn new(script: Vec<(&'static str, Vec<Answer>)>) -> Self {
        Venue {
            script: script.into_iter().collect(),
            seen: Mutex::new(HashMap::new()),
            calls: Mutex::new(Vec::new()),
            simulated: true,
        }
    }

    /// This venue as the REAL network (no simulated-trading header).
    fn real(mut self) -> Self {
        self.simulated = false;
        self
    }

    /// A flat net account: the answers every read gives when nothing is open.
    fn flat(config_answers: Vec<Answer>) -> Vec<(&'static str, Vec<Answer>)> {
        vec![
            (CONFIG, config_answers),
            (POSITIONS, vec![envelope(json!([]))]),
            (ORDERS, vec![envelope(json!([]))]),
            (STOPS, vec![envelope(json!([]))]),
        ]
    }

    fn answer(&self, method: &str, path: &str, params: &[(&str, Value)]) -> Answer {
        let key = format!("{method} {path}");
        let params: serde_json::Map<String, Value> =
            params.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect();
        self.calls.lock().unwrap().push((key.clone(), Value::Object(params)));
        let (name, answers) = self
            .script
            .get_key_value(key.as_str())
            .unwrap_or_else(|| panic!("the probe made a request nobody scripted: {key}"));
        let mut seen = self.seen.lock().unwrap();
        let n = seen.entry(*name).or_insert(0);
        let answer = answers[(*n).min(answers.len() - 1)].clone();
        *n += 1;
        answer
    }

    fn requests(&self) -> Vec<String> {
        self.calls.lock().unwrap().iter().map(|(k, _)| k.clone()).collect()
    }

    fn params_of(&self, key: &str) -> Vec<Value> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, p)| p.clone())
            .collect()
    }

    fn count(&self, key: &str) -> usize {
        self.requests().iter().filter(|k| *k == key).count()
    }
}

impl OkxTransport for Venue {
    fn simulated(&self) -> bool {
        self.simulated
    }

    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
        _signer: &OkxV5Signer,
    ) -> Answer {
        self.answer(method, path, params)
    }

    fn public(&self, _base: &str, path: &str, _params: &[(&str, String)]) -> Answer {
        panic!("the modes probe is SIGNED only: a public call to {path} is a wrong endpoint");
    }
}

fn signer() -> OkxV5Signer {
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: Some("p".to_string().into()),
    };
    OkxV5Signer::new(&creds, || 0)
}

fn settle(venue: &Venue) -> ModesReport {
    settle_modes(venue, &signer(), "https://example.invalid")
}

fn blocked_text(report: &ModesReport) -> &str {
    match &report.position {
        PositionMode::Blocked(why) => why,
        other => panic!("expected Blocked, got {other:?}"),
    }
}

/// `net_mode` already: ONE read, no flat check, no write.
#[test]
fn a_net_account_is_left_alone_and_costs_one_read() {
    let venue = Venue::new(Venue::flat(vec![config("net_mode", "3")]));
    assert_eq!(settle(&venue).position, PositionMode::Net);
    assert_eq!(venue.requests(), vec![CONFIG]);
}

/// The whole write: a long/short account, flat on all three reads, gets exactly ONE set-position-mode
/// carrying `net_mode`, and is read back before the word is `Healed`.
#[test]
fn a_flat_long_short_account_is_switched_to_net_and_read_back() {
    let venue = Venue::new({
        let mut script = Venue::flat(vec![config("long_short_mode", "3"), config("net_mode", "3")]);
        script.push((WRITE, vec![envelope(json!([{"posMode": "net_mode"}]))]));
        script
    });
    assert_eq!(settle(&venue).position, PositionMode::Healed);
    assert_eq!(venue.requests(), vec![CONFIG, POSITIONS, ORDERS, STOPS, WRITE, CONFIG]);
    assert_eq!(venue.params_of(WRITE), vec![json!({"posMode": "net_mode"})]);
    // the stop read carries the one parameter OKX requires of it
    assert_eq!(venue.params_of(STOPS), vec![json!({"ordType": "conditional"})]);
}

/// A second start after a heal finds net mode and writes nothing: the switch is idempotent.
#[test]
fn a_second_start_after_the_switch_writes_nothing() {
    let venue = Venue::new({
        let mut script = Venue::flat(vec![
            config("long_short_mode", "3"),
            config("net_mode", "3"),
            config("net_mode", "3"),
        ]);
        script.push((WRITE, vec![envelope(json!([{"posMode": "net_mode"}]))]));
        script
    });
    assert_eq!(settle(&venue).position, PositionMode::Healed);
    assert_eq!(settle(&venue).position, PositionMode::Net);
    assert_eq!(venue.count(WRITE), 1);
}

/// An open position blocks the switch, is NAMED, and nothing is written.
#[test]
fn an_open_position_blocks_the_switch_and_is_named() {
    let mut script = Venue::flat(vec![config("long_short_mode", "3")]);
    script[1] = (POSITIONS, vec![envelope(json!([{"instId": "BTC-USDT-SWAP", "pos": "0.5"}]))]);
    let venue = Venue::new(script);
    let report = settle(&venue);
    let why = blocked_text(&report);
    assert!(why.contains("1 open position(s) (BTC-USDT-SWAP)"), "{why}");
    assert!(why.contains("account.active"), "the way out is named: {why}");
    assert_eq!(venue.count(WRITE), 0);
}

/// A row with `pos` 0 is a flat row, not a position: it does not block.
#[test]
fn a_zero_size_position_row_is_flat() {
    let mut script = Venue::flat(vec![config("long_short_mode", "3"), config("net_mode", "3")]);
    script[1] = (POSITIONS, vec![envelope(json!([{"instId": "BTC-USDT-SWAP", "pos": "0"}]))]);
    script.push((WRITE, vec![envelope(json!([{"posMode": "net_mode"}]))]));
    assert_eq!(settle(&Venue::new(script)).position, PositionMode::Healed);
}

/// An unreadable size must not read as flat: the row blocks.
#[test]
fn an_unreadable_position_size_is_not_flat() {
    let mut script = Venue::flat(vec![config("long_short_mode", "3")]);
    script[1] = (POSITIONS, vec![envelope(json!([{"instId": "ETH-USDT-SWAP", "pos": ""}]))]);
    let venue = Venue::new(script);
    assert!(blocked_text(&settle(&venue)).contains("ETH-USDT-SWAP"));
    assert_eq!(venue.count(WRITE), 0);
}

/// A resting order blocks, is counted, and nothing is written.
#[test]
fn a_resting_order_blocks_the_switch() {
    let mut script = Venue::flat(vec![config("long_short_mode", "3")]);
    script[2] = (ORDERS, vec![envelope(json!([{"ordId": "1"}, {"ordId": "2"}]))]);
    let venue = Venue::new(script);
    assert!(blocked_text(&settle(&venue)).contains("2 resting order(s)"));
    assert_eq!(venue.count(WRITE), 0);
}

/// A resting conditional stop blocks too: the one algo kind this adapter places.
#[test]
fn a_resting_stop_blocks_the_switch() {
    let mut script = Venue::flat(vec![config("long_short_mode", "3")]);
    script[3] = (STOPS, vec![envelope(json!([{"algoId": "9"}]))]);
    let venue = Venue::new(script);
    assert!(blocked_text(&settle(&venue)).contains("1 resting stop(s)"));
    assert_eq!(venue.count(WRITE), 0);
}

/// Every blocker is listed in ONE sentence, not only the first.
#[test]
fn every_blocker_is_named_at_once() {
    let mut script = Venue::flat(vec![config("long_short_mode", "3")]);
    script[1] = (POSITIONS, vec![envelope(json!([{"instId": "BTC-USDT-SWAP", "pos": "1"}]))]);
    script[2] = (ORDERS, vec![envelope(json!([{"ordId": "1"}]))]);
    let report = settle(&Venue::new(script));
    let why = blocked_text(&report);
    assert!(why.contains("open position(s)") && why.contains("resting order(s)"), "{why}");
}

/// A read that FAILS reads as not flat: no write, and the sentence says which read.
#[test]
fn a_failed_flat_read_blocks_the_switch() {
    for broken in [POSITIONS, ORDERS, STOPS] {
        let mut script = Venue::flat(vec![config("long_short_mode", "3")]);
        script.retain(|(key, _)| *key != broken);
        script.push((broken, vec![refusal("50011", "Too many requests")]));
        let venue = Venue::new(script);
        let report = settle(&venue);
        let why = blocked_text(&report);
        assert!(why.contains("could not be read") && why.contains("Too many requests"), "{why}");
        assert_eq!(venue.count(WRITE), 0, "{broken}: nothing is written on an unproven flat");
    }
}

/// The venue's own refusal of the write (59000 is its "close positions and orders first") is worded with
/// its code and its message, and no read-back follows a refusal.
#[test]
fn a_refused_write_is_named_with_the_venues_words() {
    let mut script = Venue::flat(vec![config("long_short_mode", "3")]);
    script.push((
        WRITE,
        vec![refusal(
            "59000",
            "Settings failed. Close any open positions or orders before modifying settings.",
        )],
    ));
    let venue = Venue::new(script);
    let report = settle(&venue);
    let why = blocked_text(&report);
    assert!(why.contains("59000") && why.contains("Close any open positions"), "{why}");
    assert_eq!(venue.count(CONFIG), 2, "the account is read back after a refusal too");
}

/// A write whose answer was lost (a timeout is ambiguous: the switch may have landed) is judged by the
/// read-back alone: net is a heal, and a mode still long/short is the venue's refusal.
#[test]
fn a_write_whose_answer_was_lost_is_judged_by_the_read_back() {
    let lost = || Err(VenueApiError { code: 0, msg: "timed out".to_string() });
    for (second_read, healed) in
        [(config("net_mode", "3"), true), (config("long_short_mode", "3"), false)]
    {
        let mut script = Venue::flat(vec![config("long_short_mode", "3"), second_read]);
        script.push((WRITE, vec![lost()]));
        let report = settle(&Venue::new(script));
        if healed {
            assert_eq!(report.position, PositionMode::Healed);
        } else {
            assert!(blocked_text(&report).contains("timed out"), "{report:?}");
        }
    }
}

/// A write answered with success whose read-back still says long/short is NOT a heal.
#[test]
fn a_write_that_the_read_back_does_not_confirm_is_not_a_heal() {
    let mut script =
        Venue::flat(vec![config("long_short_mode", "3"), config("long_short_mode", "3")]);
    script.push((WRITE, vec![envelope(json!([{"posMode": "net_mode"}]))]));
    let report = settle(&Venue::new(script));
    assert!(blocked_text(&report).contains("still reads posMode \"long_short_mode\""));
}

/// An unreadable config writes nothing and asks nothing more.
#[test]
fn an_unreadable_config_is_unknown_and_touches_nothing() {
    let venue = Venue::new(vec![(CONFIG, vec![refusal("50113", "Invalid Sign")])]);
    let report = settle(&venue);
    assert!(matches!(&report.position, PositionMode::Unknown(why) if why.contains("Invalid Sign")));
    assert_eq!(venue.requests(), vec![CONFIG]);
}

/// A posMode word that is neither of the two documented ones is not guessed at; nor is an absent one.
#[test]
fn an_unknown_or_absent_position_mode_is_not_written() {
    for answer in [config("hedge", "3"), envelope(json!([{"acctLv": "3"}]))] {
        let venue = Venue::new(vec![(CONFIG, vec![answer])]);
        assert!(matches!(settle(&venue).position, PositionMode::Unknown(_)));
        assert_eq!(venue.requests(), vec![CONFIG]);
    }
    let venue = Venue::new(vec![(CONFIG, vec![envelope(json!([]))])]);
    assert!(matches!(settle(&venue).position, PositionMode::Unknown(_)));
}

/// The account level is NAMED when it is Simple mode, and never written: the report carries a note
/// beside whatever the position mode said, and no `set-account-level` is ever requested.
#[test]
fn a_simple_mode_account_is_named_and_never_written() {
    let venue = Venue::new(Venue::flat(vec![config("net_mode", "1")]));
    let report = settle(&venue);
    assert_eq!(report.position, PositionMode::Net);
    assert!(report.level.as_deref().is_some_and(|n| n.contains("acctLv 1")));
    assert!(venue.requests().iter().all(|r| !r.contains("set-account-level")));
}

/// OKX answers a net order on a long/short account `Parameter posSide error` (measured on the demo
/// host): the refusal then names the account's mode, and every other message is byte for byte the
/// venue's own.
#[test]
fn a_pos_side_refusal_names_the_account_mode_and_other_refusals_pass_through() {
    for other in ["Parameter instId error", "Order price is not within the price limit", ""] {
        assert_eq!(explain_refusal(other, true), other);
        assert_eq!(explain_refusal(other, false), other);
    }
    let said = explain_refusal("Parameter posSide error ", true);
    assert!(
        said.starts_with("Parameter posSide error (usually the account is in long/short"),
        "{said}"
    );
    assert!(said.contains("switch this account off and on"), "{said}");
    // The real network: the bridge changes no account mode there, so the words name the hand fix.
    let live = explain_refusal("Parameter posSide error ", false);
    assert!(
        live.starts_with("Parameter posSide error (usually the account is in long/short"),
        "{live}"
    );
    assert!(live.contains("changes no LIVE account's mode"), "{live}");
    assert!(live.contains("switch the account to Net mode in OKX's trading settings"), "{live}");
    assert!(!live.contains("switches the account to net mode when its engine starts"), "{live}");
}

/// OWNER RULING 2026-10-26: no exchange write on a live account. A transport that does not send the
/// simulated-trading header (the real network, which is also what a transport that does not say is) is never
/// read and never written: a long/short account whose every write the venue would accept gets NO request.
#[test]
fn the_real_network_is_never_read_or_written() {
    let script = || {
        let mut script = Venue::flat(vec![config("long_short_mode", "1"), config("net_mode", "1")]);
        script.push((WRITE, vec![envelope(json!([{"posMode": "net_mode"}]))]));
        script
    };
    let live = Venue::new(script()).real();
    let report = settle(&live);
    assert_eq!(report, ModesReport { position: PositionMode::LeftAlone, level: None });
    assert!(live.requests().is_empty(), "{:?}", live.requests());
    // The same account on the simulated network IS healed: the gate is the only difference.
    let demo = Venue::new(script());
    assert_eq!(settle(&demo).position, PositionMode::Healed);
    assert_eq!(demo.count(WRITE), 1);
    // A transport that does not say defaults to the real network.
    struct Silent;
    impl OkxTransport for Silent {
        fn signed(
            &self,
            _: &str,
            p: &str,
            _: &str,
            _: &[(&str, Value)],
            _: &OkxV5Signer,
        ) -> Answer {
            panic!("a transport that does not say is the real network: {p}")
        }
        fn public(&self, _: &str, p: &str, _: &[(&str, String)]) -> Answer {
            panic!("{p}")
        }
    }
    assert!(!Silent.simulated());
    assert_eq!(settle_modes(&Silent, &signer(), "x").position, PositionMode::LeftAlone);
}

/// The same words through the real submit path: a scripted venue refuses the net order the way the
/// demo host did (top-level code 1, the per-order sCode 51000), and the terminal `OrderRejected` the
/// operator reads carries the explanation.
#[test]
fn a_net_order_the_venue_refuses_for_its_pos_side_is_rejected_with_the_explanation() {
    use vike_bridge_core::rest::VenueRest;
    use vike_model::events::Event;
    use vike_model::{OrderRequest, SymbolProperties};

    let refusal = Ok(json!({
        "code": "1", "msg": "All operations failed",
        "data": [{"sCode": "51000", "sMsg": "Parameter posSide error ", "clOrdId": "c1"}]
    }));
    let venue = Venue::new(vec![("POST /api/v5/trade/order", vec![refusal])]);
    let rest = crate::perp::OkxPerpRest {
        signer: signer(),
        transport: venue,
        base_url: "https://example.invalid".to_string(),
        symbol: "BTC-USDT-SWAP".to_string(),
        properties: SymbolProperties { step_size: 1.0, tick_size: 0.1, ..Default::default() },
        ct_val: 0.01,
        leverage: 2.0,
        broker_code: None,
    };
    let request: OrderRequest = serde_json::from_value(json!({
        "client_order_id": "c1", "venue": "okx", "symbol": "BTC-USDT-SWAP", "side": 1,
        "qty": 0.01, "order_type": "limit", "price": 100.0, "ts": 1
    }))
    .unwrap();
    let events = VenueRest::submit_order(&rest, &request);
    let reason = events
        .iter()
        .find_map(|e| match e {
            Event::OrderRejected(r) => Some(r.reason.to_string()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("a terminal OrderRejected is expected: {events:?}"));
    assert!(reason.contains("Parameter posSide error"), "{reason}");
    assert!(reason.contains("long/short position mode"), "{reason}");
    assert!(reason.contains("switches the account to net mode when its engine starts"), "{reason}");
}

/// The same refusal on the REAL network, through the single and the stop paths (the batch path calls the
/// same `explain_refusal`, but rides `signed_json`, which this scripted venue does not play): the words
/// name the mismatch and the hand fix, and promise no switch by the bridge.
#[test]
fn a_live_pos_side_refusal_names_the_hand_fix_on_every_submit_path() {
    use vike_bridge_core::rest::VenueRest;
    use vike_model::events::Event;
    use vike_model::{OrderRequest, SymbolProperties};

    let refusal = || {
        Ok(json!({
            "code": "1", "msg": "All operations failed",
            "data": [{"sCode": "51000", "sMsg": "Parameter posSide error ", "clOrdId": "c1"}]
        }))
    };
    let request = |kind: &str| -> OrderRequest {
        serde_json::from_value(json!({
            "client_order_id": "c1", "venue": "okx", "symbol": "BTC-USDT-SWAP", "side": 1,
            "qty": 0.01, "order_type": kind, "price": 100.0, "trigger_price": 90.0, "ts": 1
        }))
        .unwrap()
    };
    let client = |simulated: bool| {
        let mut venue = Venue::new(vec![
            ("POST /api/v5/trade/order", vec![refusal()]),
            ("POST /api/v5/trade/order-algo", vec![refusal()]),
        ]);
        venue.simulated = simulated;
        crate::perp::OkxPerpRest {
            signer: signer(),
            transport: venue,
            base_url: "https://example.invalid".to_string(),
            symbol: "BTC-USDT-SWAP".to_string(),
            properties: SymbolProperties { step_size: 1.0, tick_size: 0.1, ..Default::default() },
            ct_val: 0.01,
            leverage: 2.0,
            broker_code: None,
        }
    };
    let reason_of = |events: Vec<Event>| {
        events
            .iter()
            .find_map(|e| match e {
                Event::OrderRejected(r) => Some(r.reason.to_string()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("a terminal OrderRejected is expected: {events:?}"))
    };
    for (path, events) in [
        ("single", VenueRest::submit_order(&client(false), &request("limit"))),
        ("stop", client(false).submit_stop_algo(&request("stop"))),
    ] {
        let why = reason_of(events);
        assert!(why.contains("changes no LIVE account's mode"), "{path}: {why}");
        assert!(why.contains("Net mode"), "{path}: {why}");
    }
}

#[test]
fn only_level_one_is_noted() {
    for level in ["2", "3", "4"] {
        assert_eq!(level_note(&json!({"acctLv": level})), None, "acctLv {level}");
    }
    assert_eq!(level_note(&json!({})), None, "an absent level makes no claim");
    assert!(level_note(&json!({"acctLv": "1"})).is_some());
}
