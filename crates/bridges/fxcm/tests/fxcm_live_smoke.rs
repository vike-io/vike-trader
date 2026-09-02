//! LIVE FXCM ForexConnect smoke (Windows or Linux x86_64 + the vendored ForexConnect SDK + demo
//! creds — run manually). This is the wire-verification the repo convention owes before FXCM can
//! mount: it
//! drives the real `ExecutionClient` seam against the FXCM demo account and proves the market-order
//! path end to end, INCLUDING the async fill lane whose instrument is now resolved from the trade's
//! offer id (fcshim.cpp `instrumentForOfferId` — the fix that lifted the `IO2GTradeRow::getInstrument`
//! real-SDK build break).
//!
//! ```text
//! # Requires the SDK so the native shim is built in (FCSDK_DIR defaults to the platform vendor
//! # dir: vendor/fcsdk on Windows, vendor/fcsdk/linux on Linux):
//! set FCSDK_DIR=C:\path\to\vendor\fcsdk          # Windows
//! export FCSDK_DIR=<repo>/vendor/fcsdk/linux      # Linux (or rely on the default)
//! cargo test -p vike-fxcm --features fxcm --test fxcm_live_smoke -- --ignored --nocapture
//! ```
//!
//! Double-gated, exactly like the other venues' `*_smoke.rs`:
//! 1. `#[ignore]` — never runs in the normal suite.
//! 2. Self-skips (logs + returns) when `FXCM_DEMO_USER`/`FXCM_DEMO_PASSWORD` are absent from the
//!    workspace `.env` — the standard live gate. It ALSO self-skips a STUB build (no ForexConnect
//!    SDK): with no session the exec thread exits and no `OrderSubmitted` ever comes back, so the
//!    first-event wait times out and the test returns rather than failing.
//!
//! Weekend-tolerant: FX is closed Fri 22:00 → Sun 22:00 GMT and a market order then comes back
//! venue-rejected. That still proves the submit → session → venue → event round trip, so the market
//! test logs the rejection and passes; the FILL assertions (which exercise the instrument fix) only
//! run against an open market.

use std::time::Duration;

use vike_bridge_core::credentials::{load_workspace_dotenv_from, Environment};
use vike_exec::{event_channel, ExecutionClient, Ingest};
use vike_fxcm::{load_fxcm_config_from, FxcmConfig, FxcmExecutionClient};
use vike_model::events::Event;
use vike_model::OrderRequest;

const VENUE: &str = "vike_fxcm";
const SYMBOL: &str = "EURUSD";

/// EUR/USD's base unit size on the FXCM demo account — i.e. ONE lot, expressed in the base units
/// `OrderRequest::qty` carries.
///
/// A CONSTANT rather than a live read on purpose: the adapter is the thing under test here, and it
/// reads the real figure from the session (`fc_base_unit_size`). A smoke that asked the same
/// question the same way could not disagree with it, so it would prove nothing about the
/// conversion. Hard-coding the expected answer means a venue that changed it turns this red with a
/// reject naming the truth, which is the outcome worth having.
const FXCM_BASE_UNIT: f64 = 1000.0;

/// Demo creds from the workspace `.env`; `None` (skip) when absent — the live gate.
fn live_config() -> Option<FxcmConfig> {
    load_fxcm_config_from(
        Environment::Demo,
        &load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref()),
    )
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

fn order(coid: &str, side: i32, order_type: &str, price: Option<f64>) -> OrderRequest {
    OrderRequest {
        combo_legs: Vec::new(),
        client_order_id: coid.into(),
        venue: "fxcm".into(),
        symbol: SYMBOL.into(),
        side,
        // ⚠ BASE UNITS, not lots — and this line used to read `qty: 1.0` with the comment
        // "→ lots=1". That was the venue-wide defect: `qty` is base units on every `OrderRequest`
        // in this workspace and in every fill this venue reports, while the shim places
        // `Amount = base_unit_size * lots`. `vike_fxcm::event_mapper::lots_for` divides now, so ONE
        // lot of EUR/USD on the FXCM demo is spelled by its base unit size.
        //
        // If the account's base unit differs from this, the submit comes back as an
        // `OrderRejected` naming the two placeable sizes either side — which is the new behaviour
        // working, not the smoke being wrong. Read the reason and use the number it gives you; the
        // old code would have silently placed 1000 LOTS for this line instead.
        qty: FXCM_BASE_UNIT,
        order_type: order_type.into(),
        price,
        trigger_price: None,
        trigger_by: None,
        reduce_only: false,
        time_in_force: Default::default(),
        gtd_expiry: None,
        ts: now_ms(),
        parent_order_id: None,
        linked_order_ids: vec![],
        order_list_id: None,
        contingency_type: None,
        weight: 0.0,
        stop: None,
        trail: None,
        extreme: None,
        on_close: false,
        margin_mode: None,
    }
}

/// Next venue event off the ingest lane within `secs`, or `None` on timeout (venue round trips are
/// seconds, not ms). `None` is how the caller self-skips a stub build / failed login.
fn try_event(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, secs: u64) -> Option<Event> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let ingest =
        rt.block_on(async { tokio::time::timeout(Duration::from_secs(secs), rx.recv()).await });
    match ingest {
        Ok(Some(Ingest::Event(ev))) => Some(ev),
        Ok(Some(other)) => panic!("expected Ingest::Event, got {other:?}"),
        Ok(None) => panic!("ingest channel closed"),
        Err(_) => None, // timeout
    }
}

/// Next venue event, panicking on timeout — used once a live session is already proven.
fn recv_event(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, secs: u64) -> Event {
    try_event(rx, secs).expect("timed out waiting for venue event")
}

/// LIVE: real ForexConnect login on the FXCM demo, proven by a resting LIMIT entry (50 pips away, so
/// it cannot fill) that comes back Submitted → Accepted, then a clean cancel. Non-filling: leaves no
/// position. Also self-skips a stub build (no `OrderSubmitted` ever arrives → first wait times out).
#[test]
#[ignore = "live: Windows/Linux + vendored ForexConnect SDK (--features fxcm) + FXCM_DEMO_* creds"]
fn fxcm_login_and_limit_cancel() {
    vike_log::test_init();
    let Some(cfg) = live_config() else {
        tracing::warn!(target: VENUE, "fxcm live smoke: no FXCM_DEMO_* creds in workspace .env — skipping");
        return;
    };
    let (events, mut rx) = event_channel(64);
    let mut client = FxcmExecutionClient::spawn(cfg, events);

    let coid = format!("fxlim{}", now_ms());
    client.submit(&order(&coid, 1, "limit", None));

    // First event doubles as the live-session probe: no OrderSubmitted ⇒ stub build / login failed.
    match try_event(&mut rx, 45) {
        Some(Event::OrderSubmitted(e)) => assert_eq!(e.client_order_id, coid),
        Some(other) => panic!("expected OrderSubmitted, got {other:?}"),
        None => {
            client.detach();
            // Credentials were present, so the two outcomes here are NOT the same thing: a STUB
            // build has no SDK to log in with (a legitimate skip — CI never vendors one), while a
            // LINKED build reaching this arm means the login FAILED, which must redden.
            assert!(
                !vike_fxcm::sdk_linked(),
                "FXCM credentials are configured and the ForexConnect SDK IS linked, yet no \
                 session came back within the window — the login FAILED. This arm used to log a \
                 warning and return, which reported an unreachable venue as a GREEN test. See \
                 `vike_fxcm::sdk_linked()`."
            );
            tracing::warn!(target: VENUE, "fxcm live smoke: STUB build (no vendored SDK) — skipping");
            return;
        }
    }
    match recv_event(&mut rx, 45) {
        Event::OrderAccepted(e) => {
            assert_eq!(e.client_order_id, coid);
            tracing::info!(target: VENUE, "LOGIN OK — limit RESTING venue_order_id={:?}; canceling", e.venue_order_id);
        }
        Event::OrderRejected(e) => {
            // Venue answered (e.g. market closed): round trip proven, nothing left resting.
            tracing::warn!(target: VENUE, "limit REJECTED (acceptable, e.g. market closed): {}", e.reason);
            client.detach();
            return;
        }
        other => panic!("expected OrderAccepted or OrderRejected, got {other:?}"),
    }

    client.cancel(&coid);
    match recv_event(&mut rx, 45) {
        Event::OrderCanceled(e) => assert_eq!(e.client_order_id, coid),
        other => panic!("expected OrderCanceled, got {other:?}"),
    }
    tracing::info!(target: VENUE, "LOGIN+LIMIT+CANCEL GREEN");
    client.detach();
}

/// LIVE market round trip: market BUY → async FILL → net-flat market SELL. The fill assertions are
/// the point — they exercise the fcshim `instrumentForOfferId` fix live (the fill's symbol is
/// resolved from the trade's offer id, not from a `getInstrument` that does not exist on the base
/// `IO2GTradeRow`). Weekend-tolerant: a closed-market rejection ends the test early (placement round
/// trip still proven; fills need an open market). Self-skips a stub build like the login test.
#[test]
#[ignore = "live: Windows/Linux + vendored ForexConnect SDK (--features fxcm) + FXCM_DEMO_* creds; fills need an open market"]
fn fxcm_market_round_trip() {
    vike_log::test_init();
    let Some(cfg) = live_config() else {
        tracing::warn!(target: VENUE, "fxcm live smoke: no FXCM_DEMO_* creds in workspace .env — skipping");
        return;
    };
    let (events, mut rx) = event_channel(64);
    let mut client = FxcmExecutionClient::spawn(cfg, events);

    // Leg 1: market BUY 1 unit-of-base EURUSD.
    let coid1 = format!("fxbuy{}", now_ms());
    client.submit(&order(&coid1, 1, "market", None));

    match try_event(&mut rx, 45) {
        Some(Event::OrderSubmitted(e)) => assert_eq!(e.client_order_id, coid1),
        Some(other) => panic!("expected OrderSubmitted, got {other:?}"),
        None => {
            client.detach();
            // Same split as the sibling test: STUB ⇒ legitimate skip, LINKED ⇒ the login failed.
            assert!(
                !vike_fxcm::sdk_linked(),
                "FXCM credentials are configured and the ForexConnect SDK IS linked, yet no \
                 session came back within the window — the login FAILED. This arm used to log a \
                 warning and return, which reported an unreachable venue as a GREEN test. See \
                 `vike_fxcm::sdk_linked()`."
            );
            tracing::warn!(target: VENUE, "fxcm live smoke: STUB build (no vendored SDK) — skipping");
            return;
        }
    }
    match recv_event(&mut rx, 45) {
        Event::OrderAccepted(e) => {
            assert_eq!(e.client_order_id, coid1);
            tracing::info!(target: VENUE, "MARKET ACCEPTED venue_order_id={:?}", e.venue_order_id);
        }
        Event::OrderRejected(e) => {
            tracing::warn!(target: VENUE, "MARKET REJECTED (acceptable when market closed): {}", e.reason);
            client.detach();
            return;
        }
        other => panic!("expected OrderAccepted or OrderRejected, got {other:?}"),
    }

    // The async fill lane: Fill (Account fold) then the OrderFilled wrap (FSM). The instrument on the
    // fill was resolved by the shim's instrumentForOfferId — assert it decoded back to our symbol.
    let mut filled = false;
    for _ in 0..2 {
        match recv_event(&mut rx, 60) {
            Event::Fill(f) => {
                assert_eq!(f.client_order_id, coid1);
                assert_eq!(f.venue, "fxcm");
                assert_eq!(f.symbol, SYMBOL, "fill instrument must resolve via the offer id");
                assert!(f.last_qty > 0.0, "a market fill carries a positive quantity");
                tracing::info!(target: VENUE, "FILL {} {} @ {} (instrument resolved from offer id)", f.side, f.last_qty, f.last_px);
            }
            Event::OrderFilled(w) => {
                assert_eq!(w.client_order_id, coid1);
                assert_eq!(w.fill.symbol, SYMBOL);
                filled = true;
                tracing::info!(target: VENUE, "ORDER FILLED {} @ {}", w.fill.last_qty, w.fill.last_px);
                break;
            }
            other => panic!("expected Fill/OrderFilled, got {other:?}"),
        }
    }
    assert!(filled, "market order must produce a terminal OrderFilled");

    // Leg 2: opposite market SELL to net the position flat (FXCM demo is FIFO/netting). Best-effort
    // cleanup — logged, not hard-asserted, so a hedging account or a partial does not red the run.
    let coid2 = format!("fxsell{}", now_ms());
    client.submit(&order(&coid2, -1, "market", None));
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        let Some(ev) = try_event(&mut rx, 15) else { break };
        match ev {
            Event::OrderFilled(w) => {
                tracing::info!(target: VENUE, "NET-FLAT close fill {} @ {}", w.fill.last_qty, w.fill.last_px);
                break;
            }
            Event::OrderRejected(e) => {
                tracing::warn!(target: VENUE, "net-flat SELL rejected (position may remain on demo): {}", e.reason);
                break;
            }
            other => tracing::info!(target: VENUE, "net-flat lifecycle: {other:?}"),
        }
    }
    tracing::info!(target: VENUE, "MARKET ROUND TRIP GREEN: buy -> fill (instrument resolved) -> net-flat");
    client.detach();
}
