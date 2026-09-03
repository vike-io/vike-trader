//! LIVE sanitized-fixture CAPTURE arm (testing-arch plan §5, PR 5b) — the mechanized successor to
//! the deribit probe→fixture flow, for the binance spot WS-API user-data stream:
//!
//!     VIKE_CAPTURE_FIXTURES=1 cargo test -p vike-binance --test binance_capture_smoke -- \
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
//! are then replayed by the normal CI'd `captured_wire_replay.rs` and sourced as frame templates
//! by the cross-bridge conformance harness (plan 5c).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use vike_binance::event_mapper::map_binance_private;
use vike_binance::spot::{parse_symbol_properties, BinanceSpotRest, DEMO_REST, PATH_EXCHANGE_INFO};
use vike_binance::user_data::{open_binance_user_data_ws, DEMO_WS};
use vike_bridge_core::capture::FrameCapture;
use vike_bridge_core::credentials::{
    load_credentials_from, load_workspace_dotenv_from, Environment,
};
use vike_bridge_core::signer::BinanceHmacSigner;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_bridge_core::user_data::run_user_data_forever;
use vike_model::clock::now_ms;
use vike_model::events::Event;

/// Classify one RAW frame by what the real mapper decodes it to — the fixture `kind` vocabulary
/// shared with bybit's capture smoke and the conformance harness's captured-template sourcing.
fn kind_of(frame: &Value) -> Option<&'static str> {
    let events = map_binance_private(frame, "binance", "BTCUSDT");
    for ev in &events {
        match ev {
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
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let Some(creds) = load_credentials_from("binance", Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_binance", "SKIP: BINANCE_DEMO creds absent");
        return;
    };

    // --- REST client (properties + clock offset), exactly the demo-smoke ladder's setup ---
    let transport =
        UreqTransport::new("binance").with_rate_gate(vike_binance::ratelimit::spot_rest_gate());
    let info = transport
        .public(DEMO_REST, PATH_EXCHANGE_INFO, &[("symbol", "BTCUSDT".into())])
        .expect("exchangeInfo");
    let properties = parse_symbol_properties(&info)["BTCUSDT"];
    let client = BinanceSpotRest {
        link_id: None,
        signer: BinanceHmacSigner::new(&creds, now_ms),
        transport,
        base_url: DEMO_REST.to_string(),
        symbol: "BTCUSDT".to_string(),
        properties,
        base_asset: "BTC".to_string(),
    };
    let offset = client.server_time_offset(now_ms()).expect("server time");
    client.signer.set_offset_ms(offset);
    let snap = client.connect().expect("connect/reconcile");
    let mark = snap.position_avg_px[0].1;
    assert!(mark > 0.0, "ticker mark must be live");

    // --- the raw-frame tap: the REAL pump + REAL open, decode routed through a capturing tee ---
    let raw_frames: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let (api_key, api_secret) = (creds.api_key.clone(), creds.api_secret.clone());
    let (frames_t, stop_t) = (Arc::clone(&raw_frames), Arc::clone(&stop));
    let pump = std::thread::Builder::new()
        .name("binance-capture-tap".into())
        .spawn(move || {
            let ws_gate = vike_binance::ratelimit::ws_rate_gate();
            run_user_data_forever(
                || {
                    open_binance_user_data_ws(
                        DEMO_WS,
                        &api_key,
                        &api_secret,
                        now_ms,
                        &stop_t,
                        &ws_gate,
                    )
                },
                |frame| {
                    frames_t.lock().unwrap().push(frame.clone());
                    Vec::new() // capture-only tap: nothing to emit
                },
                |_| true,
                &stop_t,
                Duration::from_secs(1),
                Duration::from_secs(30),
                None,
                || {},
            )
        })
        .expect("spawn capture tap");
    std::thread::sleep(Duration::from_secs(3)); // subscribe handshake settle (fullloop-smoke idiom)

    // Poll the RAW tapped frames until one carrying our `coid` shows exec-status `x_marker`,
    // bounded. RAW (not mapper-decoded) on purpose: the venue puts the original coid in `C`
    // (origClientOrderId) on a CANCELED report while `c` holds the auto-generated cancel-request
    // id — so a mapper that reads `c` never matches the resting coid. Detecting the frame's ARRIVAL
    // from the wire (coid in `c` OR `C`) is mapper-independent; the committed frame is still
    // verbatim, and the replay test asserts whatever the mapper actually does with it.
    let saw = |coid: &str, x_marker: &str, secs: u64| -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            for f in raw_frames.lock().unwrap().iter() {
                let inner = f.get("event").unwrap_or(f);
                if inner.get("x").and_then(|x| x.as_str()) == Some("REJECTED")
                    && (inner.get("c").and_then(|c| c.as_str()) == Some(coid)
                        || inner.get("C").and_then(|c| c.as_str()) == Some(coid))
                {
                    panic!("{coid} REJECTED on the WS: {inner}");
                }
                if inner.get("x").and_then(|x| x.as_str()) == Some(x_marker)
                    && (inner.get("c").and_then(|c| c.as_str()) == Some(coid)
                        || inner.get("C").and_then(|c| c.as_str()) == Some(coid))
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
    let coid_rest = format!("vtrcapR{}", now_ms() % 100_000_000);
    let req: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": coid_rest, "venue": "binance", "symbol": "BTCUSDT",
        "side": 1, "qty": rest_qty, "order_type": "limit", "price": limit_px, "ts": now_ms()
    }))
    .unwrap();
    client.submit_order(&req);
    assert!(saw(&coid_rest, "NEW", 15), "{coid_rest} never NEW on the WS");
    client.cancel_order(&coid_rest).expect("cancel");
    assert!(saw(&coid_rest, "CANCELED", 15), "{coid_rest} never CANCELED on the WS");

    // --- 3) MARKET BUY (~min notional) → x=TRADE/X=FILLED; 4) MARKET SELL flatten ---
    let buy_qty = (properties.min_notional.max(5.0) * 1.4) / mark;
    let coid_buy = format!("vtrcapB{}", now_ms() % 100_000_000);
    let req: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": coid_buy, "venue": "binance", "symbol": "BTCUSDT",
        "side": 1, "qty": buy_qty, "order_type": "market", "ts": now_ms()
    }))
    .unwrap();
    client.submit_order(&req);
    assert!(saw(&coid_buy, "TRADE", 20), "{coid_buy} never FILLED on the WS");

    // flatten with the venue-confirmed filled qty (the fullloop-smoke idiom — leave flat)
    let filled: f64 = raw_frames
        .lock()
        .unwrap()
        .iter()
        .flat_map(|f| map_binance_private(f, "binance", "BTCUSDT"))
        .filter_map(|ev| match ev {
            Event::Fill(f) if f.client_order_id == coid_buy => Some(f.last_qty),
            _ => None,
        })
        .sum();
    assert!(filled > 0.0);
    let coid_sell = format!("vtrcapS{}", now_ms() % 100_000_000);
    let req: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": coid_sell, "venue": "binance", "symbol": "BTCUSDT",
        "side": -1, "qty": filled, "order_type": "market", "ts": now_ms()
    }))
    .unwrap();
    client.submit_order(&req);
    assert!(saw(&coid_sell, "TRADE", 20), "{coid_sell} never FILLED on the WS (flatten leg)");

    // --- teardown the tap, then sanitize + write in emission order ---
    stop.store(true, Ordering::Relaxed);
    pump.join().expect("tap thread").expect("tap pump auth-clean");

    let mut cap = FrameCapture::new(
        "binance",
        format!(
            "binance spot DEMO ({DEMO_WS}) — binance_capture_smoke ladder: rest-LIMIT/cancel + \
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
