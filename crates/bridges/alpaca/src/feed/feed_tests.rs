//! Unit pins for the market-data module's pieces: a user name's class and wire name are its kept
//! row's, the stream route is taken from the class (an unlisted class routes nowhere), the WS URLs
//! and the auth / subscribe frames, the lane's plan and refusals, what a lane hands the sink from a frame
//! of its route (its own key only, under the user name, by the one price check), the status a session says,
//! and the venue `error` control frames the route's reader logs.

use super::client::{deliver, lane_plan};
use super::connection::RouteLink;
use super::decode::{ControlError, decode_control_errors};
use super::*;
use std::assert_matches;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use vike_bridge_core::data_client::context::DataClientContext;
use vike_bridge_core::data_client::ws::{
    DataClientLane, DataClientLanePlan, DataClientSpec, DataClientWebsocket,
};
use vike_bridge_core::websocket_session::market::WebsocketSessionFrameOutcome;
use vike_bridge_core::websocket_session::spec;
use vike_data::{DataClient, DataClientError, DataClientSink};
use vike_marketdata::{Bar, L2Book, TickQuote, TickTrade};
use vike_model::feed_status::{DataClientConnectionState, DataClientStatus};

use crate::config::AlpacaConfig;
use crate::refusals::AlpacaFeedRefusal;

/// The class and the wire name are the KEPT row's, never the shape of the name (decision 0290): the coin `BTCUSD`
/// is crypto spot spelled `BTC/USD`, the share `AAPL` an equity, and a name with no row (a ticker ending in `BTC`,
/// the venue's own pair) is refused naming the fix.
#[test]
fn the_class_and_the_wire_name_are_the_kept_rows() {
    crate::names::hold_test_rows();
    assert_eq!(class_of("BTCUSD").unwrap(), (AssetClass::CryptoSpot, "BTC/USD".to_string()));
    assert_eq!(class_of("AAPL").unwrap(), (AssetClass::Equity, "AAPL".to_string()));
    for name in ["GBTC", "BTC/USD"] {
        let why = class_of(name).expect_err("a refusal in words");
        assert_eq!(why.id(), crate::refusals::AlpacaNameRefusalId::NotKept, "{name}: {why}");
    }
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

/// **A lane is a `SessionOn` plan on its kept row's stream, and a lane this venue cannot serve is refused in words**:
/// a name with no row (naming the fix), a book, and a bar of any interval but `1m` (on the caller's thread).
#[test]
fn a_lane_plans_on_its_rows_stream_and_an_unservable_one_is_refused() {
    crate::names::hold_test_rows();
    let spec = DataClientSpecAlpaca::new(stub_config());
    for (lane, symbol, interval) in [
        (DataClientLane::Quotes, "BTCUSD", "quotes"),
        (DataClientLane::Trades, "AAPL", "trades"),
        (DataClientLane::Bars, "AAPL", "1m"),
    ] {
        let plan = spec.plan(lane, symbol, interval).expect("a served lane");
        assert!(matches!(plan, DataClientLanePlan::SessionOn { backoff: None, .. }), "{symbol}");
    }
    let Err(why) = spec.plan(DataClientLane::Quotes, "GBTC", "quotes") else {
        panic!("a name with no kept row is refused")
    };
    assert_eq!(why, class_of("GBTC").expect_err("no kept row").to_string());
    let Err(why) = spec.plan(DataClientLane::Book, "BTCUSD", "book") else {
        panic!("alpaca streams no book")
    };
    let lane = DataClientLane::Book.word("book").to_string();
    assert_eq!(why, AlpacaFeedRefusal::NoLane { lane }.to_string());
    assert_eq!(spec.refuse_bars("1m"), Ok(()));
    assert_eq!(spec.refuse_bars("5m"), Err(AlpacaFeedRefusal::BarsOnlyOneMinute.to_string()));
}

/// `subscribe_book` refuses THROUGH `require_live_verb` (driven off `VenueCapabilities.live_data.book` = false) rather
/// than a hand-rolled message, so its refusal stays pinned to the declared matrix, and a bar of any interval but `1m`
/// is refused in this venue's words; both on the caller's thread, before any lane (no network).
#[test]
fn subscribe_book_and_a_non_minute_bar_are_refused() {
    let mut client = DataClientWebsocket::new(
        DataClientSpecAlpaca::new(stub_config()),
        Arc::new(vike_data::DataClientSinkTee(Vec::new())),
        || {},
    );
    assert_matches!(client.subscribe_book("BTCUSD"), Err(DataClientError::Unsupported(_)));
    assert_matches!(
        client.subscribe_bars("BTCUSD", "5m"),
        Err(DataClientError::Subscribe(why)) if why == AlpacaFeedRefusal::BarsOnlyOneMinute.to_string()
    );
    assert_eq!(client.status_writer_lanes(), 0);
    client.shutdown();
}

/// What the lane hands the sink, in order.
#[derive(Default)]
struct Recorded(Mutex<Vec<String>>);

impl Recorded {
    fn say(&self, line: String) {
        self.0.lock().unwrap().push(line);
    }
}

impl DataClientSink for Recorded {
    fn seed_bars(&self, _: &str, _: &str, _: &str, _: Vec<Bar>) {}
    fn close_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.say(format!("close {venue} {symbol} {interval} {}", bar.close));
    }
    fn forming_bar(&self, _: &str, _: &str, _: &str, _: Bar) {}
    fn mark_tick(&self, _: &str, _: &str, _: f64, _: i64) {}
    fn bar_close_tick(&self, venue: &str, symbol: &str, px: f64, _: i64) {
        self.say(format!("last {venue} {symbol} {px}"));
    }
    fn quote(&self, venue: &str, symbol: &str, quote: TickQuote) {
        let stamped = quote.local_timestamp > 0;
        self.say(format!("quote {venue} {symbol} {} {} {stamped}", quote.symbol, quote.bid));
    }
    fn trade(&self, venue: &str, symbol: &str, trade: TickTrade) {
        self.say(format!("trade {venue} {symbol} {} {}", trade.symbol, trade.price));
    }
    fn book(&self, _: &str, _: &str, _: Arc<L2Book>) {}
}

/// A lane's context over `sink`, counting its wakes.
fn context(sink: Arc<Recorded>) -> (DataClientContext, Arc<AtomicUsize>) {
    let wakes = Arc::new(AtomicUsize::new(0));
    let woke = Arc::clone(&wakes);
    let ctx = DataClientContext {
        venue: VENUE,
        sink,
        status: Arc::new(Mutex::new(DataClientStatus::new(
            DataClientConnectionState::Connecting,
            "connecting to Alpaca…",
        ))),
        wake: Arc::new(move || {
            woke.fetch_add(1, Ordering::SeqCst);
        }),
        stop: Arc::default(),
        own: (),
    };
    (ctx, wakes)
}

/// **A lane keeps its own key of every frame of its route, under the USER name, by the one price check**: of a frame
/// naming two coins and three verbs, the `BTCUSD` quote lane takes the `BTC/USD` quote alone, as `BTCUSD`, stamped with
/// its receive time; a crossed quote is dropped; a bar lane ticks the bar-close cache ahead of the bar; one wake per
/// frame that delivered, none for one that did not.
#[test]
fn a_lane_takes_its_own_key_of_a_frame_under_the_user_name() {
    let sink = Arc::new(Recorded::default());
    let (ctx, wakes) = context(Arc::clone(&sink));
    let frame = r#"[{"T":"q","S":"BTC/USD","bp":1.0,"ap":2.0,"bs":1.0,"as":1.0,"t":"2026-07-14T05:39:01Z"},
        {"T":"q","S":"ETH/USD","bp":3.0,"ap":4.0,"t":"2026-07-14T05:39:01Z"},
        {"T":"t","S":"BTC/USD","p":1.5,"s":0.1,"t":"2026-07-14T05:39:01Z"},
        {"T":"b","S":"BTC/USD","o":1,"h":2,"l":1,"c":1.5,"v":3,"t":"2026-07-14T05:39:00Z"}]"#;
    deliver(&ctx, 'c', Verb::Quotes, "BTCUSD", "BTC/USD", frame);
    deliver(&ctx, 'c', Verb::Trades, "BTCUSD", "BTC/USD", frame);
    deliver(&ctx, 'c', Verb::Bars, "BTCUSD", "BTC/USD", frame);
    let crossed = r#"[{"T":"q","S":"BTC/USD","bp":3.0,"ap":2.0,"t":"2026-07-14T05:39:01Z"}]"#;
    deliver(&ctx, 'c', Verb::Quotes, "BTCUSD", "BTC/USD", crossed);
    assert_eq!(
        *sink.0.lock().unwrap(),
        [
            "quote alpaca BTCUSD BTCUSD 1 true",
            "trade alpaca BTCUSD BTCUSD 1.5",
            "last alpaca BTCUSD 1.5",
            "close alpaca BTCUSD 1m 1.5",
        ]
    );
    assert_eq!(wakes.load(Ordering::SeqCst), 3, "one per frame that delivered");
}

/// **A lane's session says `Connected` on its first frame and its error in the engine's words**: the engine forwards
/// its `Live` to this hook and writes nothing itself, so this is the pin that
/// `crates/vike-ops/tests/venues/feed_success_disclosure_gate.rs`, a text scan of the engine's call site, cannot be.
/// The lane keeps the pump row's redial.
#[test]
fn a_lanes_session_says_live_on_its_first_frame_and_its_error_in_the_engines_words() {
    let link = Arc::new(RouteLink::new(
        StreamRoute::Crypto,
        "ws://127.0.0.1:9".into(),
        Arc::new(crate::transport::sign::TokenSource::new(
            "id".into(),
            "secret".into(),
            "http://127.0.0.1:9".into(),
        )),
        spec::row(VENUE),
    ));
    let DataClientLanePlan::SessionOn { mut on_text, mut on_live, mut on_error, backoff, .. } =
        lane_plan(
            link,
            StreamRoute::Crypto,
            Verb::Quotes,
            "BTCUSD".into(),
            "BTC/USD".into(),
            DataClientLane::Quotes.key("BTCUSD", ""),
        )
    else {
        panic!("an alpaca lane is a SessionOn plan")
    };
    assert!(backoff.is_none(), "the pump row's redial");
    let (ctx, _) = context(Arc::default());
    let ack = r#"[{"T":"subscription","quotes":["BTC/USD"]}]"#;
    assert_eq!(
        on_text(&ctx, ack),
        WebsocketSessionFrameOutcome::Confirm,
        "any frame proves the socket"
    );
    on_live(&ctx);
    let status = ctx.status.lock().unwrap().clone();
    assert_eq!(
        (status.state, status.text.as_str()),
        (DataClientConnectionState::Connected, "LIVE · Alpaca")
    );
    let stall = AlpacaFeedRefusal::SilentStall { secs: 180 }.to_string();
    on_error(&ctx, stall.as_str());
    let status = ctx.status.lock().unwrap().clone();
    assert_eq!(status.state, DataClientConnectionState::Error);
    assert_eq!(status.text, format!("BTCUSD quotes ws error (reconnecting): {stall}"));
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

/// A route's socket reader pins itself to the `md` role FIRST, as the WebSocket venues' feeds do (owner ruling G2.5
/// row 15; the lanes are pinned by the engine's registry). A no-op unless `config.pin_cores` names `md`.
#[test]
fn the_socket_reader_pins_to_the_market_data_role() {
    let src = include_str!("connection.rs");
    let spawn = src.find(".name(\"gw-alpaca-link\"").expect("the reader thread's spawn");
    let body = &src[spawn..];
    let body = &body[..body.find("reader.run(socket)").expect("the thread's body")];
    assert!(body.contains("pin_current_thread(") && body.contains("Role::MarketData"), "{body}");
}
