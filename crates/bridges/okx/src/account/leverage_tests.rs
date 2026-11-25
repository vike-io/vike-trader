//! okx's leverage READ over a scripted signed transport (decision 0182): no venue network.
//!
//! ⚠ The fixtures are the venue's DOCUMENTED shapes except where `account/leverage.rs`'s module doc says a
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

use std::rc::Rc;
use std::time::Duration;

use serde_json::{Value, json};
use vike_bridge_core::leverage_read::{confirm, probe};
use vike_bridge_core::scripted_rest::RestTransportScript;
use vike_bridge_core::transport::VenueApiError;
use vike_secrets::credentials::Credentials;

use super::*;
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::json::RestTransportJson;

/// The scripted okx transport: the shared wire over okx's signed and public doors. A newtype, because the seam
/// and the wire are both `vike_bridge_core`'s.
struct Scripted(RestTransportScript);

impl RestTransportJson for Scripted {
    fn signed(
        &self,
        _base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        let sent = params
            .iter()
            .map(|(k, v)| {
                ((*k).to_string(), v.as_str().map_or_else(|| v.to_string(), str::to_string))
            })
            .collect();
        self.0.answer(method, path, sent)
    }

    fn public(
        &self,
        _base_url: &str,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        let sent = params.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect();
        self.0.answer("GET", path, sent)
    }
}

fn reader(wire: RestTransportScript) -> LeverageReaderOkx<Scripted> {
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: Some("p".to_string().into()),
    };
    LeverageReaderOkx::new(
        OkxV5Signer::new(&creds, || 0),
        Scripted(wire),
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

fn lever(instrument: &str, lever: &str, pos_side: &str) -> Value {
    json!({"instId": instrument, "mgnMode": "cross", "posSide": pos_side, "lever": lever})
}

fn position(instrument: &str, pos: &str, side: &str, mode: &str) -> Value {
    json!({"instId": instrument, "pos": pos, "posSide": side, "mgnMode": mode, "lever": "5"})
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
fn clean(held: &str) -> RestTransportScript {
    RestTransportScript::new()
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
    assert_eq!(
        check_account_config(&row("1", "net_mode")),
        Err(OkxLeverageRefusal::AccountSpotOnly)
    );
    assert_eq!(
        check_account_config(&row("4", "net_mode")),
        Err(OkxLeverageRefusal::AccountPortfolioMargin)
    );
    assert_eq!(
        check_account_config(&row("9", "net_mode")),
        Err(OkxLeverageRefusal::AccountLevelUnknown { level: "9".to_string() })
    );
    assert_eq!(
        check_account_config(&row("2", "long_short_mode")),
        Err(OkxLeverageRefusal::PositionModeNotNet { mode: "long_short_mode".to_string() })
    );
    assert_eq!(
        check_account_config(&json!({"posMode": "net_mode"})),
        Err(OkxLeverageRefusal::AccountLevelAbsent),
        "no acctLv, no guess"
    );
    assert_eq!(
        check_account_config(&json!({"acctLv": "2"})),
        Err(OkxLeverageRefusal::PositionModeAbsent),
        "no posMode, no guess"
    );
    // The level may arrive as a number.
    assert_eq!(check_account_config(&json!({"acctLv": 2, "posMode": "net_mode"})), Ok(()));
}

#[test]
fn one_net_cross_leverage_row_is_read_and_every_other_shape_refused() {
    let id = "BTC-USDT-SWAP";
    assert_eq!(parse_leverage_info(&json!([lever(id, "10", "net")]), id), Ok(10.0));
    assert_eq!(parse_leverage_info(&json!([lever(id, "2.5", "net")]), id), Ok(2.5));
    let no_row = Err(OkxLeverageRefusal::NoLeverageRow { symbol: id.to_string() });
    assert_eq!(parse_leverage_info(&json!([]), id), no_row);
    assert_eq!(parse_leverage_info(&json!([lever("ETH-USDT-SWAP", "10", "net")]), id), no_row);
    let two = json!([lever(id, "10", "long"), lever(id, "10", "short")]);
    assert_eq!(
        parse_leverage_info(&two, id),
        Err(OkxLeverageRefusal::SeveralLeverageRows { count: 2, symbol: id.to_string() })
    );
    assert_eq!(
        parse_leverage_info(&json!([lever(id, "10", "long")]), id),
        Err(OkxLeverageRefusal::LeverageRowNotNet)
    );
    let isolated = json!([{"instId": id, "mgnMode": "isolated", "posSide": "net", "lever": "10"}]);
    assert_eq!(parse_leverage_info(&isolated, id), Err(OkxLeverageRefusal::LeverageRowNotCross));
    for bad in ["", "abc", "0", "0.5"] {
        assert_eq!(
            parse_leverage_info(&json!([lever(id, bad, "net")]), id),
            Err(OkxLeverageRefusal::LeverageRowNoLever),
            "{bad:?}"
        );
    }
    assert_eq!(
        parse_leverage_info(&json!({}), id),
        Err(OkxLeverageRefusal::NoDataArray { what: "leverage-info" })
    );
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
    assert_eq!(isolated, Err(OkxLeverageRefusal::PositionIsolated { symbol: id.to_string() }));
    let hedge = parse_open_position(&json!([position(id, "3", "long", "cross")]), id);
    assert_eq!(hedge, Err(OkxLeverageRefusal::PositionNotNet { symbol: id.to_string() }));
    assert_eq!(
        parse_open_position(&json!({}), id),
        Err(OkxLeverageRefusal::NoDataArray { what: "positions" })
    );
    // A size that is missing or no figure is NOT read as flat: the open-position guard fails closed.
    let no_pos = json!([{"instId": id, "posSide": "net", "mgnMode": "cross"}]);
    assert_eq!(
        parse_open_position(&no_pos, id),
        Err(OkxLeverageRefusal::PositionNoSize { symbol: id.to_string() })
    );
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
    assert_eq!(
        inverse,
        Err(OkxLeverageRefusal::NotLinear {
            symbol: "BTC-USDT-SWAP".to_string(),
            kind: "inverse".to_string()
        })
    );
    assert_eq!(
        parse_max_leverage(&data(instrument("linear", "100")), "ETH-USDT-SWAP"),
        Err(OkxLeverageRefusal::NoSwapRow { symbol: "ETH-USDT-SWAP".to_string() })
    );
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
    // The one write is `mgnMode: cross` (`VenueRestOkx::set_leverage`); the held leverage is read for cross.
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
    // `Ok`: this bridge's refusal, as the reader words it; `Err`: the shared lane's own words
    // (`vike_bridge_core::leverage_read`).
    let symbol = || "BTC-USDT-SWAP".to_string();
    let cases: Vec<(&str, RestTransportScript, f64, Result<OkxLeverageRefusal, &str>)> = vec![
        (
            "spot-only account",
            RestTransportScript::new().0.says(CONFIG, config("1", "net_mode")),
            10.0,
            Ok(OkxLeverageRefusal::AccountSpotOnly),
        ),
        (
            "portfolio margin",
            RestTransportScript::new().0.says(CONFIG, config("4", "net_mode")),
            10.0,
            Ok(OkxLeverageRefusal::AccountPortfolioMargin),
        ),
        (
            "long/short mode",
            RestTransportScript::new().0.says(CONFIG, config("2", "long_short_mode")),
            10.0,
            Ok(OkxLeverageRefusal::PositionModeNotNet { mode: "long_short_mode".to_string() }),
        ),
        (
            "no leverage row",
            RestTransportScript::new()
                .0
                .says(CONFIG, config("2", "net_mode"))
                .says(LEVER, ok(json!([]))),
            10.0,
            Ok(OkxLeverageRefusal::NoLeverageRow { symbol: symbol() }),
        ),
        (
            "a figure above the instrument's maximum",
            clean("2"),
            125.0,
            Err("above the venue's maximum of 100x"),
        ),
        (
            "an open position at another leverage",
            RestTransportScript::new()
                .0
                .says(CONFIG, config("2", "net_mode"))
                .says(LEVER, ok(json!([lever("BTC-USDT-SWAP", "3", "net")])))
                .says(POSITIONS, ok(json!([position("BTC-USDT-SWAP", "2", "net", "cross")])))
                .says(INSTR, instrument("linear", "100")),
            10.0,
            Err("open position is held at 3x"),
        ),
        (
            "an open isolated position",
            RestTransportScript::new()
                .0
                .says(CONFIG, config("2", "net_mode"))
                .says(LEVER, ok(json!([lever("BTC-USDT-SWAP", "3", "net")])))
                .says(POSITIONS, ok(json!([position("BTC-USDT-SWAP", "2", "net", "isolated")]))),
            10.0,
            Ok(OkxLeverageRefusal::PositionIsolated { symbol: symbol() }),
        ),
    ];
    for (what, wire, figure, expect) in cases {
        let log = Rc::clone(&wire.log);
        let why = probe(&reader(wire), figure).unwrap_err();
        match expect {
            Ok(refusal) => assert_eq!(why, refusal.to_string(), "{what}"),
            Err(words) => assert!(why.contains(words), "{what}: {why}"),
        }
        assert!(log.borrow().iter().all(|c| c.method == "GET"), "{what}: wrote");
    }
    // The same open position, already at the figure: a no-op, not a refusal.
    let wire = RestTransportScript::new()
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
    let wire = RestTransportScript::new()
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
    let unread = |code: i64, msg: &str| {
        let why = VenueApiError { code, msg: msg.to_string() }.to_string();
        OkxLeverageRefusal::Unread { what: "account/config", why }.to_string()
    };
    let denied = VenueApiError { code: 50111, msg: "Invalid OK-ACCESS-KEY".to_string() };
    let why =
        probe(&reader(RestTransportScript::new().0.then(CONFIG, Err(denied))), 10.0).unwrap_err();
    assert_eq!(why, unread(50111, "Invalid OK-ACCESS-KEY"));
    // An envelope error is read too.
    let body = json!({"code": "50102", "msg": "Timestamp request expired", "data": []});
    let why = probe(&reader(RestTransportScript::new().0.says(CONFIG, body)), 10.0).unwrap_err();
    assert_eq!(why, unread(50102, "Timestamp request expired"));
    // An empty config answer is no answer.
    let why =
        probe(&reader(RestTransportScript::new().0.says(CONFIG, ok(json!([])))), 10.0).unwrap_err();
    assert_eq!(why, OkxLeverageRefusal::ConfigNoRow.to_string());
}

/// **The reader reads the network of its tier, at the account's own rows**: the seed's REST base for
/// both types (one host), the `x-simulated-trading` header by the TYPE alone (`paper` reads as demo),
/// and an account whose rows hold no address is refused in the lookup's words, before any request.
#[test]
fn the_reader_reads_the_network_of_its_tier_at_the_accounts_rows() {
    use vike_secrets::account_credentials::{AccountCredentials, CredentialFields};
    let fields = CredentialFields::from_pairs([
        ("API_KEY", "k"),
        ("API_SECRET", "s"),
        ("API_PASSPHRASE", "p"),
    ]);
    let creds = AccountCredentials::new(Some(1), Some(&fields), None);
    let seed = VenueUrls::seed("okx");
    crate::instrument::kept_list_for_tests();
    for (tier, simulated) in
        [(AccountTier::Live, false), (AccountTier::Demo, true), (AccountTier::Paper, true)]
    {
        let reader = leverage_reader(&creds, &seed, tier, "BTCUSDT.P").expect("built");
        assert_eq!(reader.base_url, "https://www.okx.com", "{tier}");
        assert_eq!(reader.symbol, "BTC-USDT-SWAP", "the user name, read as its kept row's instId");
        assert_eq!(
            reader.transport.network() == vike_model::VenueNetwork::Testnet,
            simulated,
            "{tier}"
        );
    }
    let why = leverage_reader(&creds, &VenueUrls::default(), AccountTier::Demo, "BTCUSDT.P")
        .err()
        .expect("no rows, no reader");
    assert!(why.contains("no address row"), "{why}");
    let why = leverage_reader(&creds, &seed, AccountTier::Demo, "NOSUCHUSDT.P")
        .err()
        .expect("no kept row, no reader");
    let refusal = crate::instrument::inst_id_of("NOSUCHUSDT.P").expect_err("no kept row");
    assert_eq!(refusal.id(), crate::refusals::OkxInstrumentRefusalId::NoKeptRow);
    assert_eq!(why, refusal.to_string());
}
