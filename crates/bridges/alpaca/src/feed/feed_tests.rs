//! Unit pins for the market-data module's pure pieces: the symbol-shape guess answers in the
//! shared `AssetClass` taxonomy, the stream route is taken from the class (an unlisted class
//! routes nowhere), the WS URLs and the auth / subscribe frames, the frames the engine's hold
//! writes per key and when it stops a connection, the caps-driven `subscribe_book` refusal, and
//! the venue `error` control frames the connection thread logs.

use super::decode::{ControlError, decode_control_errors};
use super::*;
use crate::feed::connection::{RouteConnection, RouteConnectionCommand};
use std::assert_matches;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use vike_bridge_core::data_client::bus::{
    DataClientBus, DataClientBusSpec, DataClientBusSpecHold, DataClientBusSpecKey,
};
use vike_data::{DataClient, DataClientError};

use crate::config::AlpacaConfig;

/// The shape guess answers in the SHARED vocabulary — the property that stopped this file
/// carrying a second, venue-local `AssetClass`.
#[test]
fn class_guess_answers_in_the_shared_taxonomy() {
    assert_eq!(class_of("BTC/USD"), AssetClass::CryptoSpot);
    assert_eq!(class_of("ETH/USDT"), AssetClass::CryptoSpot);
    assert_eq!(class_of("AAPL"), AssetClass::Equity);
    assert_eq!(class_of("SPY"), AssetClass::Equity);
}

/// The route is a function of the CLASS, and the classes Alpaca does not stream route NOWHERE
/// rather than to whichever endpoint a string test would have picked (`"BTC-PERP"` carries no
/// slash, so the old `contains('/')` test would have opened the EQUITY socket for a perp).
#[test]
fn route_is_taken_from_the_class() {
    assert_eq!(StreamRoute::for_class(AssetClass::CryptoSpot), Some(StreamRoute::Crypto));
    assert_eq!(StreamRoute::for_class(AssetClass::Equity), Some(StreamRoute::Equity));
    // an ETF is `us_equity` on this venue's own `/v1/assets`, and IEX carries its tape
    assert_eq!(StreamRoute::for_class(AssetClass::Etf), Some(StreamRoute::Equity));
    for unlisted in [
        AssetClass::CryptoPerp,
        AssetClass::CryptoFuture,
        AssetClass::Option,
        AssetClass::Fx,
        AssetClass::Future,
        AssetClass::Index,
        AssetClass::PredictionMarket,
        AssetClass::Cfd,
        AssetClass::Bond,
        AssetClass::Fund,
        AssetClass::Warrant,
        AssetClass::Commodity,
        AssetClass::Combo,
    ] {
        assert_eq!(StreamRoute::for_class(unlisted), None, "{unlisted:?} is not listed here");
    }
}

/// ...and the two that DO route are exactly the two this venue's catalog declares, so the
/// routing table and `catalog.rs`'s `asset_classes` cannot drift apart silently.
#[test]
fn every_class_the_catalog_declares_has_a_stream() {
    use vike_catalog::CatalogProvider;

    // Credential-less: `new` does no I/O, and `asset_classes` is the venue's static claim.
    let catalog = crate::catalog::CatalogProviderAlpaca::new(
        &vike_secrets::account_credentials::AccountCredentials::none(),
        &vike_venues::venue_urls::VenueUrls::seed("alpaca"),
    );
    for class in catalog.asset_classes().iter().copied() {
        assert!(
            StreamRoute::for_class(class).is_some(),
            "{class:?} is listed by the catalog but routes to no stream"
        );
    }
}

#[test]
fn websocket_urls_by_route() {
    let h = "wss://stream.data.sandbox.alpaca.markets";
    assert_eq!(websocket_url(h, StreamRoute::Crypto), format!("{h}/v1beta3/crypto/us"));
    // equity must be the free IEX feed, NOT sip (sip → 409 insufficient subscription)
    assert_eq!(websocket_url(h, StreamRoute::Equity), format!("{h}/v2/iex"));
}

#[test]
fn sub_action_frames() {
    assert_eq!(
        sub_action("subscribe", Verb::Quotes, "BTC/USD"),
        serde_json::json!({"action": "subscribe", "quotes": ["BTC/USD"]})
    );
    assert_eq!(
        sub_action("unsubscribe", Verb::Trades, "AAPL"),
        serde_json::json!({"action": "unsubscribe", "trades": ["AAPL"]})
    );
    assert_eq!(
        sub_action("subscribe", Verb::Bars, "ETH/USD"),
        serde_json::json!({"action": "subscribe", "bars": ["ETH/USD"]})
    );
}

#[test]
fn auth_frame_is_oauth_key_bearer_secret() {
    assert_eq!(
        auth_frame("tok-123"),
        serde_json::json!({"action": "auth", "key": "oauth", "secret": "tok-123"})
    );
}

/// A sandbox config with no credentials: nothing here dials.
fn stub_config() -> AlpacaConfig {
    use crate::transport::urls::urls_for;
    use vike_model::AccountTier;

    let env = AccountTier::Paper;
    AlpacaConfig {
        client_id: vike_model::secret::Secret::new(String::new()),
        client_secret: vike_model::secret::Secret::new(String::new()),
        account_id: String::new(),
        env,
        hosts: urls_for(env, &vike_venues::venue_urls::VenueUrls::seed("alpaca"))
            .expect("the seed's sandbox rows"),
    }
}

/// A connection that dials nothing: its thread waits for its stop flag, and its command channel is read back.
fn planted(
    keys: &[(Verb, &str)],
) -> (RouteConnection, mpsc::Receiver<RouteConnectionCommand>, Arc<AtomicBool>) {
    let (command_tx, command_rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let join = std::thread::spawn(move || {
        while !flag.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    let keys = keys.iter().map(|(verb, symbol)| (*verb, symbol.to_string())).collect();
    (RouteConnection { command_tx, stop: Arc::clone(&stop), join, keys }, command_rx, stop)
}

fn frames(rx: &mpsc::Receiver<RouteConnectionCommand>) -> Vec<String> {
    rx.try_iter()
        .map(|cmd| match cmd {
            RouteConnectionCommand::Sub(verb, symbol) => format!("+{verb:?} {symbol}"),
            RouteConnectionCommand::Unsubscribe(verb, symbol) => format!("-{verb:?} {symbol}"),
        })
        .collect()
}

/// The hold writes a frame only for a key its connection does not carry yet (the engine counts the holders, so a
/// key reaches `unwant` once, from its last), and stops its connection with the route's last key: the pins the old
/// per-pair refcount carried.
#[test]
fn the_hold_writes_one_frame_per_key_and_stops_the_connection_with_its_last() {
    let source = Arc::new(DataClientBusSpecAlpaca::new(stub_config(), || {}));
    let hold = source.consume(Box::new(|_| {}));
    let (conn, rx, stop) = planted(&[(Verb::Quotes, "BTC/USD")]);
    hold.plant(StreamRoute::Crypto, conn);
    let quotes = DataClientBusSpecKey::Quotes("BTC/USD".into());
    let trades = DataClientBusSpecKey::Trades("BTC/USD".into());
    let eth = DataClientBusSpecKey::Quotes("ETH/USD".into());

    hold.ask(&quotes, true).expect("carried already");
    hold.ask(&trades, false).expect("a verb of its own");
    hold.ask(&eth, false).expect("a symbol of its own");
    assert_eq!(frames(&rx), ["+Trades BTC/USD", "+Quotes ETH/USD"]);

    hold.unwant(&quotes);
    hold.unwant(&eth);
    assert_eq!(frames(&rx), ["-Quotes BTC/USD", "-Quotes ETH/USD"]);
    assert!(!stop.load(Ordering::Relaxed), "trades still ride the connection");
    hold.unwant(&eth); // not carried any more: nothing
    hold.unwant(&trades);
    assert!(frames(&rx).is_empty(), "the last key stops the connection, it writes no frame");
    assert!(stop.load(Ordering::Relaxed) && !hold.has_connection(StreamRoute::Crypto));
}

/// A connection whose thread is gone is dropped and the ask fails, so the next key respawns it cleanly.
#[test]
fn a_dead_connection_fails_the_ask_and_is_dropped() {
    let source = Arc::new(DataClientBusSpecAlpaca::new(stub_config(), || {}));
    let hold = source.consume(Box::new(|_| {}));
    let (conn, rx, stop) = planted(&[(Verb::Quotes, "AAPL")]);
    drop(rx);
    hold.plant(StreamRoute::Equity, conn);
    let why = hold.ask(&DataClientBusSpecKey::Trades("AAPL".into()), false).expect_err("dead");
    assert_matches!(why, DataClientError::Subscribe(_));
    assert!(!hold.has_connection(StreamRoute::Equity));
    stop.store(true, Ordering::Relaxed);
}

/// `subscribe_book` refuses THROUGH `require_live_verb` (driven off `VenueCapabilities.live_data.book` = false) rather than
/// a hand-rolled message, so its refusal stays pinned to the declared matrix, and a bar of any interval but `1m` is
/// refused in this venue's words; both as `Unsupported`, on a client built with a stub config (no network: the
/// refusal comes before any connection).
#[test]
fn subscribe_book_and_a_non_minute_bar_are_refused() {
    let source = Arc::new(DataClientBusSpecAlpaca::new(stub_config(), || {}));
    let mut client = DataClientBus::on(&source, Arc::new(vike_data::DataClientSinkTee(Vec::new())));
    assert_matches!(client.subscribe_book("BTC/USD"), Err(DataClientError::Unsupported(_)));
    assert_matches!(
        client.subscribe_bars("BTC/USD", "5m"),
        Err(DataClientError::Unsupported("alpaca market-data WS serves 1m bars only"))
    );
}

/// The three error frames this crate's notes record (verified live 2026-07-14) read as their code
/// and text, alone or amid acks, and the same frames still decode to NO data message — the
/// control-frame contract `tests/mapper_data.rs` pins is untouched.
#[test]
fn venue_error_frames_are_read_for_the_log() {
    let limit = r#"[{"T":"error","code":406,"msg":"connection limit exceeded"}]"#;
    assert_eq!(
        decode_control_errors(limit),
        vec![ControlError { code: Some(406), msg: "connection limit exceeded".into() }]
    );
    assert!(decode_websocket_message('c', limit).is_empty());

    let mixed = r#"[{"T":"success","msg":"connected"},{"T":"error","code":402,"msg":"auth failed"},
        {"T":"error","code":409,"msg":"insufficient subscription"}]"#;
    assert_eq!(
        decode_control_errors(mixed),
        vec![
            ControlError { code: Some(402), msg: "auth failed".into() },
            ControlError { code: Some(409), msg: "insufficient subscription".into() },
        ]
    );
}

/// Data frames and acks carry no error, and an error element missing its code or text still reads:
/// the log line names what the venue sent, not what the decoder expected.
#[test]
fn only_error_elements_are_error_frames() {
    let data = r#"[{"T":"q","S":"BTC/USD","bp":1.0,"ap":2.0},{"T":"t","S":"AAPL","p":3.0}]"#;
    assert!(decode_control_errors(data).is_empty());
    assert!(decode_control_errors(r#"[{"T":"success","msg":"authenticated"}]"#).is_empty());
    assert!(decode_control_errors(r#"[{"T":"subscription","trades":["error"]}]"#).is_empty());
    assert_eq!(
        decode_control_errors(r#"[{"T":"error"}]"#),
        vec![ControlError { code: None, msg: String::new() }]
    );
    assert_eq!(
        decode_control_errors(r#"[{"T":"error","code":"406","msg":7}]"#),
        vec![ControlError { code: None, msg: String::new() }],
        "a code that is not a non-negative integer and a non-text msg read as absent"
    );
}

/// Total over hostile text: a non-array, truncated or non-JSON frame that merely CONTAINS the word
/// is empty (never a panic), and the venue's text is cut so one frame cannot flood a log line.
#[test]
fn error_frame_reader_is_total_and_bounded() {
    for hostile in [
        "",
        "error",
        r#""error""#,
        r#"{"T":"error","code":406}"#,
        r#"[{"T":"error","code":406"#,
        r#"["error", null, 3, {"T":["error"]}]"#,
    ] {
        assert!(decode_control_errors(hostile).is_empty(), "{hostile:?}");
    }
    let long = "é".repeat(5_000);
    let frame = format!(r#"[{{"T":"error","code":500,"msg":"{long}"}}]"#);
    let got = decode_control_errors(&frame);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].msg.chars().count(), 200);
}

/// A market-data connection thread pins itself to the `md` role FIRST, as the WebSocket venues' feeds
/// do (owner ruling G2.5 row 15). A no-op unless `config.pin_cores` names `md`; before the ruling the
/// thread never asked, so it floated on any core, the trading core included.
#[test]
fn the_connection_thread_pins_to_the_market_data_role() {
    let src = include_str!("client.rs");
    let spawn = src.find(".name(format!(\"alpaca-data-").expect("the connection thread's spawn");
    let body = &src[spawn..];
    let body = &body[..body.find("connection_main(ctx, initial)").expect("the thread's body")];
    assert!(body.contains("pin_current_thread(") && body.contains("Role::MarketData"), "{body}");
}
