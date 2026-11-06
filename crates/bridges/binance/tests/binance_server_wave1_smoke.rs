//! Two live measurements behind the server wave-1 Binance fixes. Neither places an order.
//!
//!     MSYS_NO_PATHCONV=1 BOX=the CI box LANE=<pinned> just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-binance --test binance_server_wave1_smoke -- --ignored --nocapture --test-threads=1
//!
//! 1. `binance_book_ticker_quotes_carry_no_venue_time` (public streams, NO credentials): runs the
//!    bridge's own market-data pump on a SPOT symbol into the real single-writer core for 20 s and records
//!    every `QuoteTick` the core hands a strategy. It MEASURES the premise of
//!    `crates/vike-tradehub/src/feeds/price_tap.rs`'s `TapSink::mark_lane_twin` stamping a mark with its
//!    arrival time when the tick has none: Binance's raw `@bookTicker` frame has no event time, so the
//!    bridge's `QuoteTick::ts` is 0 on every frame (`crates/bridges/binance/src/market_data.rs`'s
//!    `decode_book_ticker`) and a mark filed under it reads as a price from 1970. The stamping itself runs
//!    in `vike-tradehub`, which links no bridge and keeps `TapSink` crate-private, so this file cannot
//!    drive it from live frames without a node; `price_tap.rs`'s
//!    `a_twin_mark_from_a_quote_the_venue_did_not_time_is_fresh_to_the_price_only_sweep` runs it offline.
//!
//! 2. `binance_a_refused_rest_call_is_logged_with_its_method_path_and_code` (DEMO creds, one signed READ):
//!    asks the demo futures host for an algo order that does not exist (a made-up `clientAlgoId`), which
//!    the venue refuses with HTTP 400, and captures the transport's own `tracing` warning. The line must
//!    name the method, the path (never the query: it holds the signature), the status and the venue's code
//!    and message (`crates/vike-bridge-core/src/transport.rs`'s `UreqTransport::run`). ONE request, no
//!    order, no position, no state change; the twin's symbol and leverage row are not touched.
//!
//! DEMO ONLY: the REST host is the `DEMO_FAPI_REST` constant, asserted below; credentials come from the
//! smoke store (`vike_secrets::smoke_directory`) at `AccountTier::Demo` and are never printed.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_binance::market_data::spawn_binance_market_data;
use vike_binance::perp::DEMO_FAPI_REST;
use vike_bridge_core::signer::BinanceHmacSigner;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_core::{CoreConfig, LiveBroker, StrategyMount, spawn_core};
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, ExecutionEngine, RiskGate};
use vike_marketdata::QuoteTick;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AccountTier, RiskLimits, Strategy, now_ms};
use vike_secrets::credentials::api_credentials;

/// A SPOT symbol, never the twin's `BTCUSDT.P`.
const SYMBOL: &str = "ETHUSDT";

/// Every quote the strategy was handed.
#[derive(Clone, Default)]
struct Quotes(Arc<Mutex<Vec<QuoteTick>>>);

struct Recorder(Quotes);

impl Strategy<LiveBroker> for Recorder {
    fn on_quote_tick(&mut self, _b: &mut LiveBroker, q: &QuoteTick) {
        self.0.0.lock().unwrap().push(q.clone());
    }
}

#[test]
#[ignore = "network — public Binance streams, run manually (see module doc)"]
fn binance_book_ticker_quotes_carry_no_venue_time() {
    vike_log::test_init();
    let quotes = Quotes::default();
    let engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        SYMBOL,
    );
    let config = CoreConfig {
        strategy: Some(StrategyMount {
            account: None,
            symbols: Vec::new(),
            controller_id: None,
            underlying_symbol: None,
            venue: "binance".into(),
            symbol: SYMBOL.into(),
            interval: "1m".into(),
            strategy: Box::new(Recorder(quotes.clone())),
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    let feed = spawn_binance_market_data(handle.tick_sender(), SYMBOL, 0.01);

    let (from, until) = (now_ms(), Instant::now() + Duration::from_secs(20));
    while Instant::now() < until {
        std::thread::sleep(Duration::from_millis(250));
    }
    feed.shutdown();
    handle.shutdown_and_join();

    let seen = quotes.0.lock().unwrap().clone();
    let untimed = seen.iter().filter(|q| q.ts == 0).count();
    let unstamped = seen.iter().filter(|q| q.local_ts == 0).count();
    println!("MEASURED {SYMBOL} @bookTicker for 20 s from {from}: {} quotes", seen.len());
    println!(
        "MEASURED quotes with ts == 0 (the venue sent no event time): {untimed} of {}",
        seen.len()
    );
    println!(
        "MEASURED quotes with local_ts == 0 (no arrival stamp): {unstamped} of {}",
        seen.len()
    );
    for q in seen.iter().take(3) {
        println!("FRAME ts={} local_ts={} bid={} ask={}", q.ts, q.local_ts, q.bid, q.ask);
    }
    assert!(seen.len() >= 10, "the public stream delivered {} quotes in 20 s", seen.len());
    assert!(seen.iter().all(|q| q.bid > 0.0 && q.ask >= q.bid), "a one-sided or crossed quote");
}

#[test]
#[ignore = "network + demo creds — one signed READ of an unknown algo order on the demo host — run manually (see module doc)"]
fn binance_a_refused_rest_call_is_logged_with_its_method_path_and_code() {
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("binance", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "binance") else {
        tracing::warn!(target: "vike_binance", "SKIP: BINANCE_DEMO creds absent");
        return;
    };
    // The owner's fence, as in `perp_algo_smoke`: these account ids are never driven by a drill.
    const OFF_LIMITS: [i64; 6] = [2, 8, 11, 12, 16, 18];
    let id = directory
        .account_row("binance", &AccountLabel::Default, AccountTier::Demo)
        .map(|row| row.id);
    println!("ACCOUNT id {id:?} (default binance account at the demo tier)");
    if id.is_none_or(|id| OFF_LIMITS.contains(&id)) {
        println!("SKIP: the default binance demo account is off limits or unresolved ({id:?})");
        return;
    }
    assert_eq!(DEMO_FAPI_REST, "https://demo-fapi.binance.com", "demo host constant moved");

    let transport = UreqTransport::new("binance");
    let signer = BinanceHmacSigner::new(&creds, now_ms);
    let unknown = format!("vtnone{}", now_ms() % 100_000_000);
    let (answer, events) = vike_log::capture::captured(|| {
        transport.signed(
            DEMO_FAPI_REST,
            "/fapi/v1/algoOrder",
            "GET",
            &[("clientAlgoId", unknown.clone())],
            &signer,
        )
    });
    let err = answer.expect_err("an algo order that was never placed must be refused");
    println!("MEASURED the venue answered code {} msg {:?}", err.code, err.msg);
    for e in &events {
        println!("CAPTURED {:?} {} {:?}", e.level, e.message, e.fields);
    }

    let line = events.iter().find(|e| e.message == "REST request failed").unwrap_or_else(|| {
        panic!("the refusal logged no `REST request failed` warning: {events:?}")
    });
    assert_eq!(line.field("venue"), Some("binance"), "{line:?}");
    assert_eq!(line.field("method"), Some("GET"), "{line:?}");
    assert_eq!(line.field("path"), Some("/fapi/v1/algoOrder"), "{line:?}");
    assert_eq!(line.field("status"), Some("400"), "{line:?}");
    assert_eq!(line.field("code"), Some(err.code.to_string().as_str()), "{line:?}");
    assert!(
        line.field("msg").is_some_and(|m| !m.is_empty()),
        "the venue's message is named: {line:?}"
    );
    // The query holds the signature: no field of the line may carry it.
    for (name, value) in &line.fields {
        assert!(
            !value.contains("signature") && !value.contains("timestamp="),
            "field {name} leaks the query: {value}"
        );
    }
    assert!(
        !line.field("path").unwrap_or_default().contains('?'),
        "the path carries a query: {line:?}"
    );
}
