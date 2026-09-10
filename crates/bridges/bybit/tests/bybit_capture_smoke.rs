//! LIVE sanitized-fixture CAPTURE arm (testing-arch plan §5, PR 5b) — bybit V5 linear-perp
//! private-WS twin of `binance_capture_smoke.rs`:
//!
//!     VIKE_CAPTURE_FIXTURES=1 cargo test -p vike-bybit --test bybit_capture_smoke -- \
//!         --ignored --nocapture
//!
//! Triple-gated: `#[ignore]` + demo creds in the workspace `.env` + the EXACT env string
//! `VIKE_CAPTURE_FIXTURES=1` — a plain `--ignored` run without the env var self-skips, so the
//! committed fixture files can never be rewritten by accident.
//!
//! The ladder (all REAL demo orders, position left flat): open the private WS
//! (`execution`/`order`/`wallet` topics) with a raw-frame tap → rest a LIMIT ~10% below mark
//! (order topic `New`) → cancel it (`Cancelled`) → MARKET BUY min qty (execution topic `Trade`) →
//! MARKET SELL reduce-only (flatten). Frames are classified by what the REAL `map_bybit_perp`
//! decodes them to, sanitized via `vike_bridge_core::capture`, and written to
//! `tests/fixtures/captured/<kind>.json` with a provenance stamp — consumed by the CI'd
//! `captured_wire_replay.rs` and the conformance harness's captured-template sourcing (plan 5c).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use vike_bridge_core::capture::FrameCapture;
use vike_bridge_core::credentials::{
    Environment, load_credentials_from, load_workspace_dotenv_from,
};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::BybitV5Signer;
use vike_bridge_core::user_data::run_user_data_forever;
use vike_bybit::event_mapper::map_bybit_perp;
use vike_bybit::perp::{
    BybitPerpRest, DEMO_REST, DEMO_WS, PATH_INSTRUMENTS, parse_bybit_perp_instruments,
};
use vike_bybit::transport::{BybitTransport, UreqBybitTransport};
use vike_bybit::user_data::open_bybit_user_data_ws;
use vike_model::clock::now_ms;
use vike_model::events::Event;

/// Classify one RAW frame by what the real mapper decodes it to — same kind vocabulary as the
/// binance capture smoke (`ws_accepted`/`ws_fill`/`ws_fill_partial`/`ws_canceled`/`ws_account_state`).
fn kind_of(frame: &Value) -> Option<&'static str> {
    let events = map_bybit_perp(frame, "bybit", "BTCUSDT");
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
    None // op acks / order-topic Filled echoes (map to []) — not fixture material
}

#[test]
#[ignore = "network + demo creds + VIKE_CAPTURE_FIXTURES=1 — rewrites committed fixtures (see module doc)"]
fn bybit_capture_lifecycle_fixtures() {
    vike_log::test_init();
    if std::env::var("VIKE_CAPTURE_FIXTURES").as_deref() != Ok("1") {
        tracing::warn!(target: "vike_bybit", "SKIP: VIKE_CAPTURE_FIXTURES != \"1\"");
        return;
    }
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let Some(creds) = load_credentials_from("bybit", Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_bybit", "SKIP: BYBIT_DEMO creds absent");
        return;
    };

    // --- REST client (instruments + mark), the exec-client-smoke setup ---
    let transport =
        UreqBybitTransport::new().with_rate_gate(vike_bybit::ratelimit::rest_rate_gate());
    let info = transport
        .signed(
            DEMO_REST,
            PATH_INSTRUMENTS,
            "GET",
            &[("category", json!("linear")), ("symbol", json!("BTCUSDT"))],
            &BybitV5Signer::new(&creds, now_ms),
        )
        .expect("instruments-info");
    let inst = parse_bybit_perp_instruments(&info)["BTCUSDT"].clone();
    let rest = BybitPerpRest {
        signer: BybitV5Signer::new(&creds, now_ms),
        transport: UreqBybitTransport::new()
            .with_rate_gate(vike_bybit::ratelimit::rest_rate_gate()),
        base_url: DEMO_REST.to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: inst.properties,
        leverage: 2.0,
    };
    let mark = rest.last_price().expect("last_price");

    // --- the raw-frame tap: REAL open + REAL pump, decode routed through a capturing tee ---
    let raw_frames: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let (api_key, api_secret) = (creds.api_key.clone(), creds.api_secret.clone());
    let (frames_t, stop_t) = (Arc::clone(&raw_frames), Arc::clone(&stop));
    let pump = std::thread::Builder::new()
        .name("bybit-capture-tap".into())
        .spawn(move || {
            let ws_gate = vike_bybit::ratelimit::ws_rate_gate();
            let mut ping = |ws: &mut vike_bridge_core::ws::TungsteniteStream| {
                let _ = ws.send_text(&vike_bybit::ws_auth::ping_frame());
            };
            run_user_data_forever(
                || {
                    open_bybit_user_data_ws(
                        DEMO_WS,
                        &api_key,
                        &api_secret,
                        now_ms,
                        &["execution", "order", "wallet"],
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
                Some((Duration::from_secs(20), &mut ping)),
                || {},
            )
        })
        .expect("spawn capture tap");
    std::thread::sleep(Duration::from_secs(4)); // auth+subscribe settle (exec-smoke idiom)

    // Poll the tap until a frame for `coid` decodes to `want`, bounded.
    let saw = |coid: &str, want: &str, secs: u64| -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            for f in raw_frames.lock().unwrap().iter() {
                for ev in map_bybit_perp(f, "bybit", "BTCUSDT") {
                    let (c, k) = match &ev {
                        Event::OrderAccepted(x) => (x.client_order_id.clone(), "Accepted"),
                        Event::OrderCanceled(x) => (x.client_order_id.clone(), "Canceled"),
                        Event::OrderFilled(x) => (x.client_order_id.clone(), "Filled"),
                        Event::OrderRejected(x) => {
                            panic!("{} REJECTED: {}", x.client_order_id, x.reason)
                        }
                        _ => continue,
                    };
                    if c == coid && k == want {
                        return true;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        false
    };

    // --- 1) LIMIT ~10% below mark rests → order-topic New; 2) cancel → Cancelled ---
    let round = |p: f64| (p / inst.properties.tick_size).round() * inst.properties.tick_size;
    let px = round(mark * 0.90);
    let rest_qty = f64::max(inst.properties.min_qty, (inst.properties.min_notional * 1.4) / px);
    let rest_qty = (rest_qty / inst.properties.step_size).ceil() * inst.properties.step_size;
    let coid_rest = format!("vtrcapR{}", now_ms() % 100_000_000);
    let req: vike_model::OrderRequest = serde_json::from_value(json!({
        "client_order_id": coid_rest, "venue": "bybit", "symbol": "BTCUSDT",
        "side": 1, "qty": rest_qty, "order_type": "limit", "price": px, "ts": now_ms()
    }))
    .unwrap();
    rest.submit_order(&req);
    assert!(saw(&coid_rest, "Accepted", 15), "{coid_rest} never New on the WS");
    rest.cancel_order(&coid_rest).expect("cancel");
    assert!(saw(&coid_rest, "Canceled", 15), "{coid_rest} never Cancelled on the WS");

    // --- 3) MARKET BUY min qty → execution Trade (terminal); 4) MARKET SELL reduce-only ---
    let qty = f64::max(inst.properties.min_qty, (inst.properties.min_notional * 1.4) / mark);
    let qty = (qty / inst.properties.step_size).ceil() * inst.properties.step_size;
    let coid_buy = format!("vtrcapB{}", now_ms() % 100_000_000);
    let req: vike_model::OrderRequest = serde_json::from_value(json!({
        "client_order_id": coid_buy, "venue": "bybit", "symbol": "BTCUSDT",
        "side": 1, "qty": qty, "order_type": "market", "ts": now_ms()
    }))
    .unwrap();
    rest.submit_order(&req);
    assert!(saw(&coid_buy, "Filled", 30), "{coid_buy} never Filled on the WS");

    let coid_sell = format!("vtrcapS{}", now_ms() % 100_000_000);
    let req: vike_model::OrderRequest = serde_json::from_value(json!({
        "client_order_id": coid_sell, "venue": "bybit", "symbol": "BTCUSDT",
        "side": -1, "qty": qty, "order_type": "market", "reduce_only": true, "ts": now_ms()
    }))
    .unwrap();
    rest.submit_order(&req);
    assert!(saw(&coid_sell, "Filled", 30), "{coid_sell} never Filled on the WS (flatten leg)");

    // reconcile-confirm flat before writing anything (leave-flat etiquette)
    std::thread::sleep(Duration::from_secs(2));
    let base = rest
        .reconcile_positions()
        .expect("reconcile after")
        .positions
        .first()
        .map_or(0.0, |(_, q)| *q);
    assert!(base.abs() < inst.properties.step_size * 1.5, "round trip must flatten: {base}");

    // --- teardown the tap, then sanitize + write in emission order ---
    stop.store(true, Ordering::Relaxed);
    pump.join().expect("tap thread").expect("tap pump auth-clean");

    let mut cap = FrameCapture::new(
        "bybit",
        format!(
            "bybit V5 linear DEMO ({DEMO_WS}) — bybit_capture_smoke ladder: rest-LIMIT/cancel + \
             market round-trip (reduce-only flatten), real demo orders, position left flat"
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
        tracing::info!(target: "vike_bybit", "captured fixture written: {}", p.display());
    }
}
