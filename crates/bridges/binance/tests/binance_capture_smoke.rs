//! LIVE sanitized-fixture CAPTURE arm (testing-arch plan §5, PR 5b) — the mechanized successor to
//! the deribit probe→fixture flow, for the binance spot WS-API user-data stream:
//!
//!     VIKE_CAPTURE_FIXTURES=1 cargo test -p vike-binance --test integration -- binance_capture_smoke:: \
//!         --ignored --nocapture
//!
//! Triple-gated: `#[ignore]` + demo creds in the workspace `.env` + the EXACT env string
//! `VIKE_CAPTURE_FIXTURES=1` (same idiom as `VIKE_RECONCILE`) — a plain `--ignored` run without
//! the env var self-skips, so the fixture files can never be rewritten by accident.
//!
//! The ladder (all REAL demo orders, account left flat): open the user-data WS with a raw-frame
//! tap → rest a far-from-market LIMIT (venue emits `x=NEW`) → cancel it (`x=CANCELED`) → MARKET
//! BUY min-notional (`x=TRADE, X=FILLED`) → MARKET SELL the filled qty (flatten). Every raw frame
//! is classified by what the REAL `map_binance_private` decodes it to, sanitized via
//! `vike_bridge_core::capture` (structure verbatim, sensitive leaves → stable placeholders), and
//! written to `tests/fixtures/captured/<kind>.json` with a provenance stamp. Committed fixtures
//! are then replayed by the normal CI'd `offline/captured_wire_replay.rs` and sourced as frame templates
//! by the cross-bridge conformance harness (plan 5c).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use vike_binance::execution::BinanceUrls;
use vike_binance::execution::event_mapper::spot::map_binance_private;
use vike_binance::execution::rest::spot::{
    PATH_EXCHANGE_INFO, VenueRestBinanceSpot, parse_symbol_properties,
};
use vike_binance::execution::user_data::spot::open_binance_user_data_websocket;
use vike_binance::family::signer::BinanceHmacSigner;
use vike_bridge_core::capture::FrameCapture;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_bridge_core::websocket_session::user::run_user;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::now_ms;
use vike_secrets::credentials::api_credentials;
use vike_venues::reconnect::ReconnectBackoff;
use vike_venues::venue_urls::VenueUrls;

/// Classify one RAW frame by what the real mapper decodes it to — the fixture `kind` vocabulary
/// shared with bybit's capture smoke and the conformance harness's captured-template sourcing.
fn kind_of(frame: &Value) -> Option<&'static str> {
    let events = map_binance_private(frame, "binance", "BTCUSDT", None);
    for event in &events {
        match event {
            Event::OrderAccepted(_) => return Some("ws_accepted"),
            Event::OrderCanceled(_) => return Some("ws_canceled"),
            Event::OrderFilled(_) => return Some("ws_fill"),
            Event::OrderPartiallyFilled(_) => return Some("ws_fill_partial"),
            Event::AccountState(_) => return Some("ws_account_state"),
            _ => {}
        }
    }
    None // subscribe acks / unknown frames — not fixture material
}

#[test]
#[ignore = "network + demo creds + VIKE_CAPTURE_FIXTURES=1 — rewrites committed fixtures (see module doc)"]
fn binance_capture_lifecycle_fixtures() {
    vike_log::test_init();
    if std::env::var("VIKE_CAPTURE_FIXTURES").as_deref() != Ok("1") {
        tracing::warn!(target: "vike_binance", "SKIP: VIKE_CAPTURE_FIXTURES != \"1\"");
        return;
    }
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("binance", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "binance") else {
        tracing::warn!(target: "vike_binance", "SKIP: BINANCE_DEMO creds absent");
        return;
    };

    // The demo hosts: the release's `venue_url` rows (`crates/vike-venues/src/venue_urls.rs`'s `VENUE_URL_SEED`).
    let hosts = BinanceUrls::resolve(&VenueUrls::seed("binance"), AccountTier::Demo)
        .expect("the seed holds every binance row");
    let (demo_rest, demo_websocket) = (hosts.spot_rest, hosts.spot_websocket);

    // --- REST client (properties + clock offset), exactly the demo-smoke ladder's setup ---
    let transport = UreqTransport::new("binance")
        .with_rate_gate(vike_binance::transport::ratelimit::spot_rest_gate());
    let info = transport
        .public(demo_rest, PATH_EXCHANGE_INFO, &[("symbol", "BTCUSDT".into())])
        .expect("exchangeInfo");
    let properties = parse_symbol_properties(&info)["BTCUSDT"];
    let client = VenueRestBinanceSpot {
        link_id: None,
        signer: BinanceHmacSigner::new(&creds, now_ms),
        transport,
        base_url: demo_rest.to_string(),
        symbol: "BTCUSDT".to_string(),
        properties,
        base_asset: "BTC".to_string(),
    };
    let offset = client.server_time_offset(now_ms()).expect("server time");
    client.signer.set_offset_ms(offset);
    let mark = vike_binance::family::execution::spot_fee::ticker_price(
        &client.transport,
        demo_rest,
        "BTCUSDT",
    )
    .expect("ticker mark must be live");

    // --- the raw-frame tap: the REAL pump + REAL open, decode routed through a capturing tee ---
    let raw_frames: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let (api_key, api_secret) = (creds.api_key.expose().clone(), creds.api_secret.expose().clone());
    let (frames_t, stop_t) = (Arc::clone(&raw_frames), Arc::clone(&stop));
    let pump = std::thread::Builder::new()
        .name("binance-capture-tap".into())
        .spawn(move || {
            let websocket_gate = vike_binance::transport::ratelimit::websocket_rate_gate();
            run_user(
                || {
                    open_binance_user_data_websocket(
                        demo_websocket,
                        &api_key,
                        &api_secret,
                        now_ms,
                        &stop_t,
                        &websocket_gate,
                    )
                },
                |frame| {
                    frames_t.lock().unwrap().push(frame.clone());
                    Vec::new() // capture-only tap: nothing to emit
                },
                |_| true,
                &stop_t,
                Duration::from_secs(1),
                ReconnectBackoff::new(Duration::from_secs(1), Duration::from_secs(30)),
                None,
                || {},
                None, // no silent-stall watchdog
                None,
            )
        })
        .expect("spawn capture tap");
    std::thread::sleep(Duration::from_secs(3)); // subscribe handshake settle (fullloop-smoke idiom)

    // Poll the RAW tapped frames until one carrying our `order_client_id` shows exec-status `x_marker`,
    // bounded. RAW (not mapper-decoded) on purpose: the venue puts the original order client id in `C`
    // (origClientOrderId) on a CANCELED report while `c` holds the auto-generated cancel-request
    // id — so a mapper that reads `c` never matches the resting order client id. Detecting the frame's ARRIVAL
    // from the wire (order client id in `c` OR `C`) is mapper-independent; the committed frame is still
    // verbatim, and the replay test asserts whatever the mapper actually does with it.
    let saw = |order_client_id: &str, x_marker: &str, secs: u64| -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            for f in raw_frames.lock().unwrap().iter() {
                let inner = f.get("event").unwrap_or(f);
                if inner.get("x").and_then(|x| x.as_str()) == Some("REJECTED")
                    && (inner.get("c").and_then(|c| c.as_str()) == Some(order_client_id)
                        || inner.get("C").and_then(|c| c.as_str()) == Some(order_client_id))
                {
                    panic!("{order_client_id} REJECTED on the WS: {inner}");
                }
                if inner.get("x").and_then(|x| x.as_str()) == Some(x_marker)
                    && (inner.get("c").and_then(|c| c.as_str()) == Some(order_client_id)
                        || inner.get("C").and_then(|c| c.as_str()) == Some(order_client_id))
                {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        false
    };

    // --- 1) resting LIMIT ~10% below mark → venue x=NEW; 2) cancel → x=CANCELED ---
    // 10% below (not 50%) stays inside Binance's PERCENT_PRICE_BY_SIDE band (a 0.5×mark bid trips
    // -1013), yet is far enough that a BTC bid never fills in the test window.
    let round = |p: f64| (p / properties.tick_size).round() * properties.tick_size;
    let limit_px = round(mark * 0.9);
    let rest_qty = (properties.min_notional.max(5.0) * 1.6) / limit_px;
    let order_client_id_rest = format!("vtrcapR{}", now_ms() % 100_000_000);
    let req: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "order_client_id": order_client_id_rest, "venue": "binance", "symbol": "BTCUSDT",
        "side": 1, "qty": rest_qty, "order_type": "limit", "price": limit_px, "ts": now_ms()
    }))
    .unwrap();
    client.submit_order(&req);
    assert!(saw(&order_client_id_rest, "NEW", 15), "{order_client_id_rest} never NEW on the WS");
    client.cancel_order(&order_client_id_rest).expect("cancel");
    assert!(
        saw(&order_client_id_rest, "CANCELED", 15),
        "{order_client_id_rest} never CANCELED on the WS"
    );

    // --- 3) MARKET BUY (~min notional) → x=TRADE/X=FILLED; 4) MARKET SELL flatten ---
    let buy_qty = (properties.min_notional.max(5.0) * 1.4) / mark;
    let order_client_id_buy = format!("vtrcapB{}", now_ms() % 100_000_000);
    let req: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "order_client_id": order_client_id_buy, "venue": "binance", "symbol": "BTCUSDT",
        "side": 1, "qty": buy_qty, "order_type": "market", "ts": now_ms()
    }))
    .unwrap();
    client.submit_order(&req);
    assert!(saw(&order_client_id_buy, "TRADE", 20), "{order_client_id_buy} never FILLED on the WS");

    // flatten with the venue-confirmed filled qty (the fullloop-smoke idiom — leave flat)
    let filled: f64 = raw_frames
        .lock()
        .unwrap()
        .iter()
        .flat_map(|f| map_binance_private(f, "binance", "BTCUSDT", None))
        .filter_map(|event| match event {
            Event::Fill(f) if f.order_client_id == order_client_id_buy => Some(f.last_qty),
            _ => None,
        })
        .sum();
    assert!(filled > 0.0);
    let order_client_id_sell = format!("vtrcapS{}", now_ms() % 100_000_000);
    let req: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "order_client_id": order_client_id_sell, "venue": "binance", "symbol": "BTCUSDT",
        "side": -1, "qty": filled, "order_type": "market", "ts": now_ms()
    }))
    .unwrap();
    client.submit_order(&req);
    assert!(
        saw(&order_client_id_sell, "TRADE", 20),
        "{order_client_id_sell} never FILLED on the WS (flatten leg)"
    );

    // --- teardown the tap, then sanitize + write in emission order ---
    stop.store(true, Ordering::Relaxed);
    pump.join().expect("tap thread").expect("tap pump auth-clean");

    let mut cap = FrameCapture::new(
        "binance",
        format!(
            "binance spot DEMO ({demo_websocket}) — binance_capture_smoke ladder: rest-LIMIT/cancel + \
             market round-trip, real demo orders, account left flat"
        ),
    );
    for f in raw_frames.lock().unwrap().iter() {
        if let Some(kind) = kind_of(f) {
            cap.capture(kind, f);
        }
    }
    for kind in ["ws_accepted", "ws_fill", "ws_canceled"] {
        assert!(cap.count(kind) > 0, "capture ladder must yield at least one {kind} frame");
    }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/captured");
    let written = cap.write_all(&dir, now_ms()).expect("write fixtures");
    for p in &written {
        tracing::info!(target: "vike_binance", "captured fixture written: {}", p.display());
    }
}
