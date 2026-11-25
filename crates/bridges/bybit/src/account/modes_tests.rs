//! The account-mode rule over a scripted signed transport: no venue network.
//!
//! The fixtures are the answers MEASURED on the demo host on 2026-10-26 (`tests/bybit_smokes/bybit_modes_smoke.rs`'s
//! `bybit_wire_drill`): a flat ONE-WAY symbol is ONE `position/list` row (`positionIdx 0`, `size "0"`), a
//! HEDGE one is two flat rows (`positionIdx 1`, `2`), a repeated switch is `110025`, and a
//! `set-margin-mode` that changes nothing is `0 "Request accepted"` with `reasons: []`. The refusal texts
//! of a switch under a position (`110024`) and of an `ISOLATED_MARGIN` -> `REGULAR_MARGIN` write that the
//! venue declines are the documented ones, not captures (the module doc says so).
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`a_one_way_cross_account_is_only_read`] | a write on every start |
//! | [`a_flat_empty_hedge_symbol_is_switched_to_one_way_before_the_first_order`] | an order the venue answers `10001` |
//! | [`a_hedge_symbol_that_cannot_be_switched_is_refused_in_words_and_nothing_is_written`] | a switch under a position, or a silent reject |
//! | [`the_venues_own_refusal_of_the_switch_is_quoted`] | a refusal worded as ours |
//! | [`a_not_modified_answer_is_success`] | a healed symbol refused |
//! | [`a_read_that_fails_writes_nothing_and_refuses_nothing`] | a venue hiccup taking a working start off the venue |
//! | [`a_write_whose_answer_says_nothing_final_refuses_nothing_and_is_asked_again`] | a write that may have landed read as done for good, or as refused |
//! | [`a_settled_symbol_is_asked_once_and_a_refusal_for_a_window`] | a probe per order |
//! | [`an_isolated_account_with_a_flat_empty_book_is_switched_to_cross`] | the engine booking cross on an isolated account |
//! | [`an_isolated_account_is_not_switched_under_a_position_an_order_or_an_unreadable_book`] | an account-wide write on a guess |
//! | [`margin_states_that_are_only_reported_are_never_written`] | portfolio margin, a classic account or one isolated symbol rewritten |
//! | [`a_refused_margin_write_never_refuses_the_order_and_quotes_the_reasons`] | an order refused for a margin state the venue trades in |
//! | [`the_parsers_read_the_measured_shapes`] | a shape the venue sends read as another |
//! | [`a_mainnet_account_is_never_read_or_written_whatever_its_host`] | the owner's ruling: a live account is never read or written for a mode |
//! | [`a_hedge_reject_is_worded_for_a_mainnet_account_only`] | a live order refused by the venue, in the venue's bare words |

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use vike_bridge_core::transport::VenueApiError;
use vike_secrets::credentials::Credentials;

use super::*;
use crate::execution::rest::seed_endpoints;
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_model::AccountTier;

type Call = (String, String, Vec<(String, Value)>);

/// Every signed request, in order.
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<Call>>>);

impl Log {
    /// `METHOD path` plus the symbol or settle coin the request names.
    fn lines(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|(method, path, params)| {
                let named =
                    |key: &str| params.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.as_str());
                let target = match (named("symbol"), named("settleCoin")) {
                    (Some(symbol), _) => format!(" {symbol}"),
                    (_, Some(coin)) => format!(" settleCoin={coin}"),
                    _ => String::new(),
                };
                format!("{method} {path}{target}")
            })
            .collect()
    }

    fn writes(&self) -> Vec<Call> {
        self.0.lock().unwrap().iter().filter(|c| c.0 == "POST").cloned().collect()
    }
}

/// How the symbol's `position/list` rows look.
#[derive(Clone, Copy)]
enum Shape {
    OneWay,
    Hedge,
    /// No row at all.
    NoRow,
    /// A row naming a `positionIdx` no mode has.
    Garbled,
}

/// What the venue holds, and so what it answers.
#[derive(Clone)]
struct World {
    shape: Shape,
    /// `size` of the first leg row.
    size: &'static str,
    trade_mode: i64,
    resting: usize,
    resting_unreadable: bool,
    positions_unreadable: bool,
    /// `(marginMode, unifiedMarginStatus)`; `None` = `account/info` cannot be read.
    info: Option<(&'static str, i64)>,
    /// The settle coin that lists an open linear position / a resting order account-wide.
    book_position_in: Option<&'static str>,
    book_order_in: Option<&'static str>,
    book_unreadable: bool,
    switch: Value,
    margin: Value,
}

fn envelope(result: Value) -> Value {
    json!({"retCode": 0, "retMsg": "OK", "result": result, "retExtInfo": {}, "time": 1})
}

fn refusal(code: i64, msg: &str) -> Value {
    json!({"retCode": code, "retMsg": msg, "result": {}, "retExtInfo": {}, "time": 1})
}

impl Default for World {
    fn default() -> Self {
        World {
            shape: Shape::OneWay,
            size: "0",
            trade_mode: 0,
            resting: 0,
            resting_unreadable: false,
            positions_unreadable: false,
            info: Some(("REGULAR_MARGIN", 3)),
            book_position_in: None,
            book_order_in: None,
            book_unreadable: false,
            switch: envelope(json!({})),
            margin: json!({"retCode": 0, "retMsg": "Request accepted", "result": {"reasons": []}}),
        }
    }
}

fn leg(symbol: &str, index: i64, size: &str, trade_mode: i64) -> Value {
    json!({
        "symbol": symbol, "positionIdx": index, "size": size, "side": if size == "0" { "" } else { "Buy" },
        "leverage": "10", "tradeMode": trade_mode, "positionStatus": "Normal"
    })
}

fn unreadable() -> Result<Value, VenueApiError> {
    Err(VenueApiError { code: 10006, msg: "Too many visits".to_string() })
}

impl World {
    fn answer(
        &self,
        method: &str,
        path: &str,
        params: &[(&str, Value)],
    ) -> Result<Value, VenueApiError> {
        let named = |key: &str| params.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone());
        let by_coin = named("settleCoin").and_then(|v| v.as_str().map(str::to_string));
        let symbol =
            named("symbol").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
        match (method, path) {
            ("GET", "/v5/account/info") => match self.info {
                Some((mode, status)) => Ok(envelope(json!({
                    "marginMode": mode, "unifiedMarginStatus": status, "isMasterTrader": false
                }))),
                None => unreadable(),
            },
            ("GET", "/v5/position/list") => match by_coin {
                Some(_) if self.book_unreadable => unreadable(),
                Some(coin) => {
                    let open = self.book_position_in == Some(coin.as_str());
                    let rows = if open { vec![leg("XRPUSDT", 0, "5", 0)] } else { vec![] };
                    Ok(envelope(json!({"category": "linear", "list": rows, "nextPageCursor": ""})))
                }
                None if self.positions_unreadable => unreadable(),
                None => {
                    let rows = match self.shape {
                        Shape::OneWay => vec![leg(&symbol, 0, self.size, self.trade_mode)],
                        Shape::Hedge => vec![
                            leg(&symbol, 1, self.size, self.trade_mode),
                            leg(&symbol, 2, "0", self.trade_mode),
                        ],
                        Shape::NoRow => vec![],
                        Shape::Garbled => vec![leg(&symbol, 7, "0", 0)],
                    };
                    Ok(envelope(json!({"category": "linear", "list": rows, "nextPageCursor": ""})))
                }
            },
            ("GET", "/v5/order/realtime") => match by_coin {
                Some(_) if self.book_unreadable => unreadable(),
                Some(coin) => {
                    let n = usize::from(self.book_order_in == Some(coin.as_str()));
                    Ok(envelope(json!({"list": vec![json!({"orderId": "o"}); n]})))
                }
                None if self.resting_unreadable => unreadable(),
                None => Ok(envelope(json!({"list": vec![json!({"orderId": "o"}); self.resting]}))),
            },
            ("POST", PATH_SWITCH_MODE) => Ok(self.switch.clone()),
            ("POST", PATH_SET_MARGIN_MODE) => Ok(self.margin.clone()),
            other => panic!("an unscripted call {other:?}"),
        }
    }
}

struct Wire {
    log: Log,
    world: World,
}

impl RestTransportJson for Wire {
    fn signed(
        &self,
        _base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        let owned = params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        self.log.0.lock().unwrap().push((method.to_string(), path.to_string(), owned));
        self.world.answer(method, path, params)
    }
}

/// The rule over `world`; the symbols it is asked about are USER names (decision 0290: `DOGEUSDT.P` is the kept
/// row's linear `DOGEUSDT`).
fn modes_on(world: World) -> (Modes<Wire>, Log) {
    crate::history::kept_list_for_tests();
    let log = Log::default();
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: None,
    };
    let wire = Wire { log: log.clone(), world };
    (
        Modes::new(
            BybitV5Signer::new(&creds, || 0),
            wire,
            seed_endpoints(AccountTier::Demo).rest,
            false,
        ),
        log,
    )
}

/// [`modes_on`] for a chosen host and mainnet flag.
fn modes_at(world: World, base_url: &str, mainnet: bool) -> (Modes<Wire>, Log) {
    let (demo, log) = modes_on(world);
    (Modes { base_url: base_url.to_string(), mainnet, ..demo }, log)
}

/// OWNER RULING 2026-10-26: no exchange write on a live account, decided by the account's TYPE (PLAN trap 1 of
/// the `venue_url` work: an address is a row an operator can move, so no host string decides it). A rule flagged
/// mainnet reads NOTHING and writes NOTHING on a hedge symbol and on an isolated account whose every write the
/// venue would accept, whatever host it names, the demo one included; a demo account still writes, on its seed
/// host and on a host its row was moved to.
///
/// Base (the host compare): the moved demo host wrote nothing, the account's type said demo.
#[test]
fn a_mainnet_account_is_never_read_or_written_whatever_its_host() {
    let both = || World { info: Some(("ISOLATED_MARGIN", 3)), ..hedge() };
    let (demo, live) =
        (seed_endpoints(AccountTier::Demo).rest, seed_endpoints(AccountTier::Live).rest);
    for host in [live.as_str(), demo.as_str(), "https://unused.invalid"] {
        let (modes, log) = modes_at(both(), host, true);
        assert!(!modes.writes_modes(), "{host}");
        assert_eq!(modes.ensure("DOGEUSDT.P"), Ok(()), "{host}: no refusal either");
        assert!(log.lines().is_empty(), "{host}: {:?}", log.lines());
    }
    for host in [demo.as_str(), "https://api-demo-moved.bybit.example"] {
        let (modes, log) = modes_at(both(), host, false);
        assert!(modes.writes_modes(), "{host}");
        assert_eq!(modes.ensure("DOGEUSDT.P"), Ok(()));
        assert!(
            !log.writes().is_empty(),
            "a demo account still switches on {host}: {:?}",
            log.lines()
        );
    }
}

/// A `BTCUSDT` create the venue answered `code` and `msg`.
fn create_refused(code: i64, msg: &str) -> FailedRequest {
    let params =
        [("category", json!("linear")), ("symbol", json!("BTCUSDT")), ("orderLinkId", json!("c1"))];
    FailedRequest::new("POST", "/v5/order/create", &params, VenueApiError { code, msg: msg.into() })
}

/// A hedge symbol's `10001` is worded as the mismatch with the hand fix for a MAINNET account, whatever its host;
/// on a demo account the request and the venue's own text ride through; every other code and text is said the same
/// way everywhere, and the demo host's EMPTY message is said empty, never as an empty reason.
#[test]
fn a_hedge_reject_is_worded_for_a_mainnet_account_only() {
    let text = "position idx not match position mode";
    let live = order_reject_reason(&create_refused(10001, text), true, "BTCUSDT");
    for word in ["BTCUSDT", "HEDGE", "ONE-WAY", "10001", text, "changes no LIVE account's mode"] {
        assert!(live.contains(word), "{word}: {live}");
    }
    assert!(live.contains("One-Way position mode"), "{live}");
    let asked = "bybit POST /v5/order/create (category linear, symbol BTCUSDT, orderLinkId c1)";
    let said = |code: i64, msg: &str| format!("{asked}: venue error {code}, message {msg:?}");
    let reason =
        |code, msg, mainnet| order_reject_reason(&create_refused(code, msg), mainnet, "BTCUSDT");
    assert_eq!(reason(10001, text, false), said(10001, text));
    // Another 10001 (a generic parameter error), another code, another text: the request and the venue's words.
    assert_eq!(reason(10001, "params error", true), said(10001, "params error"));
    assert_eq!(reason(110007, text, true), said(110007, text));
    assert_eq!(reason(0, "plain", true), said(0, "plain"));
    assert_eq!(reason(10001, "", false), format!("{asked}: venue error 10001 (empty message)"));
}

fn param(call: &Call, key: &str) -> Option<Value> {
    call.2.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
}

fn hedge() -> World {
    World { shape: Shape::Hedge, ..World::default() }
}

#[test]
fn a_one_way_cross_account_is_only_read() {
    let (modes, log) = modes_on(World::default());
    assert_eq!(modes.ensure("DOGEUSDT.P"), Ok(()));
    assert_eq!(log.lines(), ["GET /v5/position/list DOGEUSDT", "GET /v5/account/info"]);
    assert!(log.writes().is_empty());
}

#[test]
fn a_flat_empty_hedge_symbol_is_switched_to_one_way_before_the_first_order() {
    let (modes, log) = modes_on(hedge());
    assert_eq!(modes.ensure("dogeusdt.p"), Ok(()), "the symbol is asked upper-cased");
    assert_eq!(
        log.lines(),
        [
            "GET /v5/position/list DOGEUSDT",
            "GET /v5/order/realtime DOGEUSDT",
            "POST /v5/position/switch-mode DOGEUSDT",
            "GET /v5/account/info",
        ]
    );
    let write = &log.writes()[0];
    assert_eq!(param(write, "category"), Some(json!("linear")));
    assert_eq!(param(write, "symbol"), Some(json!("DOGEUSDT")));
    assert_eq!(
        param(write, "mode"),
        Some(json!(0)),
        "mode 0 is ONE-WAY (3 is hedge), a JSON number"
    );
}

#[test]
fn a_hedge_symbol_that_cannot_be_switched_is_refused_in_words_and_nothing_is_written() {
    // A position on one leg.
    let (modes, log) = modes_on(World { size: "0.5", ..hedge() });
    let why = modes.ensure("DOGEUSDT.P").unwrap_err();
    for word in
        ["DOGEUSDT", "HEDGE", "ONE-WAY", "10001", "holds an open position", "Nothing was changed"]
    {
        assert!(why.contains(word), "{word}: {why}");
    }
    assert!(log.writes().is_empty(), "{:?}", log.lines());
    assert_eq!(
        log.lines(),
        ["GET /v5/position/list DOGEUSDT"],
        "a held position blocks before any other read"
    );

    // Resting orders.
    let (modes, log) = modes_on(World { resting: 2, ..hedge() });
    let why = modes.ensure("DOGEUSDT.P").unwrap_err();
    assert!(why.contains("has 2 resting order(s)"), "{why}");
    assert!(log.writes().is_empty());

    // Resting orders that cannot be counted are not taken for none.
    let (modes, log) = modes_on(World { resting_unreadable: true, ..hedge() });
    let why = modes.ensure("DOGEUSDT.P").unwrap_err();
    assert!(why.contains("could not be shown to have no resting order"), "{why}");
    assert!(why.contains("Too many visits"), "{why}");
    assert!(log.writes().is_empty());
}

#[test]
fn the_venues_own_refusal_of_the_switch_is_quoted() {
    let world = World {
        switch: refusal(
            110024,
            "You have an existing position, so position mode cannot be switched",
        ),
        ..hedge()
    };
    let (modes, log) = modes_on(world);
    let why = modes.ensure("DOGEUSDT.P").unwrap_err();
    for word in ["DOGEUSDT", "HEDGE", "refused the switch", "110024", "existing position"] {
        assert!(why.contains(word), "{word}: {why}");
    }
    assert_eq!(log.writes().len(), 1);
}

/// The demo host's answer with an EMPTY message (`10001`, `retMsg ""`, as it answers the fee-rate read): the refusal
/// still names the request and the code. Base: it ended "(POST /v5/position/switch-mode): 10001 ", nothing after.
#[test]
fn an_empty_refusal_of_the_switch_names_the_request_and_the_code() {
    let (modes, _log) = modes_on(World { switch: refusal(10001, ""), ..hedge() });
    let why = modes.ensure("DOGEUSDT.P").unwrap_err();
    assert!(
        why.contains(
            "The venue refused the switch to one-way: bybit POST /v5/position/switch-mode ("
        ),
        "{why}"
    );
    assert!(why.contains("symbol DOGEUSDT, mode 0): venue error 10001 (empty message)"), "{why}");
}

#[test]
fn a_not_modified_answer_is_success() {
    let world = World { switch: refusal(110025, "Position mode is not modified"), ..hedge() };
    let (modes, _log) = modes_on(world);
    assert_eq!(modes.ensure("DOGEUSDT.P"), Ok(()));
}

#[test]
fn a_read_that_fails_writes_nothing_and_refuses_nothing() {
    // The position list cannot be read.
    let (modes, log) = modes_on(World { positions_unreadable: true, ..hedge() });
    assert_eq!(modes.ensure("DOGEUSDT.P"), Ok(()));
    assert!(log.writes().is_empty());
    // No row, or a row naming no mode: the same.
    for shape in [Shape::NoRow, Shape::Garbled] {
        let (modes, log) = modes_on(World { shape, ..World::default() });
        assert_eq!(modes.ensure("DOGEUSDT.P"), Ok(()));
        assert!(log.writes().is_empty());
    }
    // `account/info` unreadable leaves the position heal in place and the margin step skipped.
    let (modes, log) = modes_on(World { info: None, ..hedge() });
    assert_eq!(modes.ensure("DOGEUSDT.P"), Ok(()));
    assert_eq!(log.writes().len(), 1, "the hedge symbol was still switched: {:?}", log.lines());
}

/// A switch-mode or set-margin-mode answer that says nothing final (no `retCode`, server trouble, the transport's
/// sentinel) may have landed: no order is refused and the symbol is probed again after the window, whose read says
/// what the venue holds. Base: a reply with no `retCode` read as `0` (switched, never asked again), and the
/// sentinel on switch-mode refused the order as "The venue refused the switch".
#[test]
fn a_write_whose_answer_says_nothing_final_refuses_nothing_and_is_asked_again() {
    let isolated = World { info: Some(("ISOLATED_MARGIN", 3)), ..World::default() };
    for answer in [
        json!({"retMsg": "OK", "result": {}}),
        refusal(10016, "Server error."),
        refusal(E_TIMEOUT_AMBIGUOUS, "timed out after send"),
    ] {
        for world in [
            World { switch: answer.clone(), ..hedge() },
            World { margin: answer.clone(), ..isolated.clone() },
        ] {
            let (modes, log) = modes_on(world);
            let modes = modes.with_retry_after(Duration::ZERO);
            assert_eq!(modes.ensure("DOGEUSDT.P"), Ok(()), "{answer}");
            assert_eq!(log.writes().len(), 1, "{answer}");
            modes.ensure("DOGEUSDT.P").unwrap();
            assert_eq!(
                log.writes().len(),
                2,
                "{answer}: read again, and written again on that read"
            );
        }
    }
}

#[test]
fn a_settled_symbol_is_asked_once_and_a_refusal_for_a_window() {
    let (modes, log) = modes_on(World::default());
    modes.ensure("DOGEUSDT.P").unwrap();
    let after_first = log.lines().len();
    modes.ensure("dogeusdt.p").unwrap();
    assert_eq!(log.lines().len(), after_first, "a settled symbol costs no read");
    modes.ensure("ETHUSDT.P").unwrap();
    assert!(log.lines().len() > after_first, "another symbol is its own probe");

    // A refusal is remembered for the window, then probed again.
    let blocked = World { size: "0.5", ..hedge() };
    let (modes, log) = modes_on(blocked.clone());
    let first = modes.ensure("DOGEUSDT.P").unwrap_err();
    assert_eq!(modes.ensure("DOGEUSDT.P").unwrap_err(), first);
    assert_eq!(log.lines().len(), 1, "the second order did not ask the venue");
    let (modes, log) = modes_on(blocked);
    let modes = modes.with_retry_after(Duration::ZERO);
    modes.ensure("DOGEUSDT.P").unwrap_err();
    modes.ensure("DOGEUSDT.P").unwrap_err();
    assert_eq!(log.lines().len(), 2, "an expired window asks again");

    // A probe that could not read is asked again after the window, and not before.
    let (modes, log) = modes_on(World { positions_unreadable: true, ..World::default() });
    modes.ensure("DOGEUSDT.P").unwrap();
    let n = log.lines().len();
    modes.ensure("DOGEUSDT.P").unwrap();
    assert_eq!(log.lines().len(), n);
    let (modes, log) = modes_on(World { positions_unreadable: true, ..World::default() });
    let modes = modes.with_retry_after(Duration::ZERO);
    modes.ensure("DOGEUSDT.P").unwrap();
    let n = log.lines().len();
    modes.ensure("DOGEUSDT.P").unwrap();
    assert!(log.lines().len() > n);
}

#[test]
fn an_isolated_account_with_a_flat_empty_book_is_switched_to_cross() {
    let world = World { info: Some(("ISOLATED_MARGIN", 3)), ..World::default() };
    let (modes, log) = modes_on(world);
    assert_eq!(modes.ensure("DOGEUSDT.P"), Ok(()));
    assert_eq!(
        log.lines(),
        [
            "GET /v5/position/list DOGEUSDT",
            "GET /v5/account/info",
            "GET /v5/position/list settleCoin=USDT",
            "GET /v5/order/realtime settleCoin=USDT",
            "GET /v5/position/list settleCoin=USDC",
            "GET /v5/order/realtime settleCoin=USDC",
            "POST /v5/account/set-margin-mode",
        ]
    );
    let write = &log.writes()[0];
    assert_eq!(write.2, [("setMarginMode".to_string(), json!("REGULAR_MARGIN"))]);
}

#[test]
fn an_isolated_account_is_not_switched_under_a_position_an_order_or_an_unreadable_book() {
    let isolated = World { info: Some(("ISOLATED_MARGIN", 3)), ..World::default() };
    for world in [
        World { book_position_in: Some("USDT"), ..isolated.clone() },
        World { book_position_in: Some("USDC"), ..isolated.clone() },
        World { book_order_in: Some("USDT"), ..isolated.clone() },
        World { book_order_in: Some("USDC"), ..isolated.clone() },
        World { book_unreadable: true, ..isolated.clone() },
    ] {
        let (modes, log) = modes_on(world);
        assert_eq!(
            modes.ensure("DOGEUSDT.P"),
            Ok(()),
            "an order is never refused for a margin state"
        );
        assert!(log.writes().is_empty(), "{:?}", log.lines());
    }
}

#[test]
fn margin_states_that_are_only_reported_are_never_written() {
    for world in [
        // Portfolio margin.
        World { info: Some(("PORTFOLIO_MARGIN", 5)), ..World::default() },
        // A classic account (status 1) answering a mode that is not cross.
        World { info: Some(("ISOLATED_MARGIN", 1)), ..World::default() },
        // A margin mode this table does not know.
        World { info: Some(("SOMETHING_NEW", 3)), ..World::default() },
        // One symbol held isolated on a regular account (classic accounts do that per symbol).
        World { trade_mode: 1, ..World::default() },
    ] {
        let (modes, log) = modes_on(world);
        assert_eq!(modes.ensure("DOGEUSDT.P"), Ok(()));
        assert!(log.writes().is_empty(), "{:?}", log.lines());
    }
}

#[test]
fn a_refused_margin_write_never_refuses_the_order_and_quotes_the_reasons() {
    let reasons = json!({
        "retCode": 3400045, "retMsg": "Set margin mode failed",
        "result": {"reasons": [{"reasonCode": "3400037", "reasonMsg": "Position or order exists"}]}
    });
    let world = World { info: Some(("ISOLATED_MARGIN", 3)), margin: reasons, ..World::default() };
    let (modes, log) = modes_on(world);
    assert_eq!(modes.ensure("DOGEUSDT.P"), Ok(()));
    assert_eq!(log.writes().len(), 1);
    // The words the report carries, through the write itself.
    let why = modes.write(PATH_SET_MARGIN_MODE, &[("setMarginMode", json!("REGULAR_MARGIN"))]);
    match why {
        Err(text) => {
            for word in ["3400045", "Set margin mode failed", "Position or order exists"] {
                assert!(text.contains(word), "{word}: {text}");
            }
        }
        Ok(_) => panic!("a refused write read as written"),
    }
    // A 200 whose `reasons` is non-empty is a refusal too.
    let soft = World {
        margin: json!({"retCode": 0, "retMsg": "OK", "result": {"reasons": [{"reasonMsg": "no"}]}}),
        ..World::default()
    };
    let (modes, _) = modes_on(soft);
    assert!(modes.write(PATH_SET_MARGIN_MODE, &[]).is_err());
}

#[test]
fn the_parsers_read_the_measured_shapes() {
    let list = |rows: Vec<Value>| json!({"category": "linear", "list": rows});
    // One flat row: one-way (measured). Two flat rows: hedge (measured). One leg alone: still hedge.
    assert_eq!(
        parse_position_mode(&list(vec![leg("DOGEUSDT", 0, "0", 0)]), "DOGEUSDT"),
        PositionMode::OneWay
    );
    assert_eq!(
        parse_position_mode(
            &list(vec![leg("DOGEUSDT", 1, "0", 0), leg("DOGEUSDT", 2, "0", 0)]),
            "dogeusdt"
        ),
        PositionMode::Hedge
    );
    assert_eq!(
        parse_position_mode(&list(vec![leg("DOGEUSDT", 2, "1", 0)]), "DOGEUSDT"),
        PositionMode::Hedge
    );
    // Another symbol's row, no row, mixed idx, a row with no idx: unknown, never a guess.
    for rows in [
        vec![leg("ETHUSDT", 0, "0", 0)],
        vec![],
        vec![leg("DOGEUSDT", 0, "0", 0), leg("DOGEUSDT", 1, "0", 0)],
        vec![json!({"symbol": "DOGEUSDT", "size": "0"})],
    ] {
        assert_eq!(parse_position_mode(&list(rows.clone()), "DOGEUSDT"), PositionMode::Unknown);
        assert!(unnamed_mode(&list(rows), "DOGEUSDT").contains("name no single position mode"));
    }
    assert_eq!(parse_position_mode(&json!({}), "DOGEUSDT"), PositionMode::Unknown);

    // An unreadable size is an OPEN position: a write is never made on a guess.
    assert!(!holds_position(&list(vec![leg("DOGEUSDT", 0, "0", 0)]), "DOGEUSDT"));
    assert!(holds_position(&list(vec![leg("DOGEUSDT", 0, "0.5", 0)]), "DOGEUSDT"));
    assert!(holds_position(
        &list(vec![json!({"symbol": "DOGEUSDT", "positionIdx": 0})]),
        "DOGEUSDT"
    ));
    assert!(!holds_position(&list(vec![leg("ETHUSDT", 0, "9", 0)]), "DOGEUSDT"));

    // The settle-coin book: an empty list (measured) is flat; a next page is not provably flat.
    assert!(book_is_flat(&json!({"list": [], "nextPageCursor": ""})));
    assert!(!book_is_flat(&json!({"list": [], "nextPageCursor": "abc"})));
    assert!(!book_is_flat(&json!({"list": [leg("XRPUSDT", 0, "5", 0)], "nextPageCursor": ""})));
    assert!(!book_is_flat(&json!({})));

    // `account/info`, as measured: REGULAR_MARGIN, status 3.
    let measured =
        json!({"marginMode": "REGULAR_MARGIN", "unifiedMarginStatus": 3, "isMasterTrader": false});
    assert_eq!(
        parse_account_mode(&measured),
        Ok(AccountMode { margin_mode: "REGULAR_MARGIN".to_string(), classic: false })
    );
    assert!(
        parse_account_mode(&json!({"marginMode": "ISOLATED_MARGIN", "unifiedMarginStatus": 1}))
            .unwrap()
            .classic
    );
    assert!(parse_account_mode(&json!({"unifiedMarginStatus": 3})).is_err());
}
