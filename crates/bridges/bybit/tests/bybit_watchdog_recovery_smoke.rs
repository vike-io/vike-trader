//! LIVE end-to-end proof that the user-data silent-stall watchdog actually RECOVERS a terminal —
//! the piece neither the unit tests nor the idle-cadence soaks establish.
//!
//!     cargo test -p vike-bybit --test bybit_watchdog_recovery_smoke -- --ignored --nocapture
//!
//! Double-gated: `#[ignore]` + demo creds. **Places REAL demo orders** (market in, reduce-only
//! market out) and leaves the position flat.
//!
//! ## What was still unproven
//!
//! `user_data.rs`'s unit tests prove the watchdog TRIPS on a synthetic stalled stream, and
//! `bybit_userdata_soak.rs` proves a healthy socket never trips it. Neither shows the thing the
//! feature exists for: that a trip on a stalled session leads, through the REAL machinery, to a
//! fill that would otherwise have been lost actually reaching the core.
//!
//! ## The shape of the proof
//!
//! The pump is driven with a stream that NEVER yields a frame and NEVER errors — the half-dead
//! socket, the one condition the watchdog was added for. Everything downstream of that is real:
//! the reconnect, `on_reconnect`'s generation bump, the [`run_resync_supervisor`] thread, the
//! genuine Bybit REST history fetch, `map_bybit_history`, and the ingest lane.
//!
//! A real demo order is then placed while the session is dark. Because the stream yields nothing —
//! on the first session OR any session after it — there is exactly ONE path by which that order's
//! fill can reach the ingest channel: watchdog trip -> reconnect -> gen bump -> A3 resync -> REST
//! replay. Observing the fill therefore proves the whole chain end-to-end.
//!
//! Contrast the control: with `idle_threshold: None` the same stalled stream parks forever, no
//! reconnect ever happens, and the fill never arrives. That is precisely the production bug this
//! feature closes, and `watchdog_off_never_recovers` asserts it stays broken without the watchdog —
//! so this smoke fails if the recovery were ever accidentally coming from somewhere else.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use vike_bridge_core::credentials::{
    Environment, load_credentials_from, load_workspace_dotenv_from,
};
use vike_bridge_core::signer::BybitV5Signer;
use vike_bridge_core::user_data::{
    OpenOutcome, ReconnectHook, ResyncPumpSpec, StreamError, StreamMsg, UserStream,
    run_user_data_forever_with_idle, spawn_pump_with_resync,
};
use vike_bybit::history::map_bybit_history;
use vike_bybit::perp::{BybitPerpRest, DEMO_REST, PATH_INSTRUMENTS, parse_bybit_perp_instruments};
use vike_bybit::transport::{BybitTransport, UreqBybitTransport};
use vike_exec::event_channel;
use vike_exec::lanes::Ingest;
use vike_model::clock::now_ms;
use vike_model::events::Event;

/// The half-dead socket: open, never a frame, never an error, never a close. The open COUNT is
/// tracked by the open closure (which is what proves a re-open happened), so the stream itself
/// carries no state.
struct StalledStream;

impl UserStream for StalledStream {
    fn recv(&mut self) -> Result<StreamMsg, StreamError> {
        std::thread::sleep(Duration::from_millis(50));
        Err(StreamError::Timeout)
    }
    fn pong(&mut self, _payload: Vec<u8>) -> Result<(), StreamError> {
        Ok(())
    }
}

/// The client-order-id an event is about, for the variants this proof can observe.
fn coid_of(ev: &Event) -> Option<&str> {
    match ev {
        Event::Fill(f) => Some(f.client_order_id.as_str()),
        Event::OrderFilled(e) => Some(e.client_order_id.as_str()),
        Event::OrderPartiallyFilled(e) => Some(e.client_order_id.as_str()),
        Event::OrderAccepted(e) => Some(e.client_order_id.as_str()),
        Event::OrderCanceled(e) => Some(e.client_order_id.as_str()),
        _ => None,
    }
}

/// Does this event represent execution (as opposed to a mere acceptance echo)? Only an execution
/// proves the resync recovered something that would otherwise have been LOST.
fn is_fill_shaped(ev: &Event) -> bool {
    matches!(ev, Event::Fill(_) | Event::OrderFilled(_) | Event::OrderPartiallyFilled(_))
}

/// Build the REST client the smoke trades and resyncs through (same setup as the capture smoke).
/// The trade side and the resync side each get their OWN instance, mirroring production where the
/// resync fetch never shares the order-submit client's transport.
fn rest_client() -> BybitPerpRest<UreqBybitTransport> {
    let transport =
        UreqBybitTransport::new().with_rate_gate(vike_bybit::ratelimit::rest_rate_gate());
    let creds = load_credentials_from(
        "bybit",
        Environment::Demo,
        &load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref()),
    )
    .expect("creds checked by the caller");
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
    BybitPerpRest {
        signer: BybitV5Signer::new(&creds, now_ms),
        transport: UreqBybitTransport::new()
            .with_rate_gate(vike_bybit::ratelimit::rest_rate_gate()),
        base_url: DEMO_REST.to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: inst.properties,
        leverage: 2.0,
    }
}

/// Drive one dark-session scenario and report whether the order's activity reached the ingest lane.
///
/// `idle_threshold` is the ONLY difference between the proof and its control.
fn run_dark_session(idle_threshold: Option<Duration>, wait: Duration) -> (bool, usize) {
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let _ = load_credentials_from("bybit", Environment::Demo, &vars).expect("creds");

    let rest_trade = rest_client();
    let rest_resync = rest_client();
    let qty = rest_trade.properties.min_qty.max(0.001);

    let (events_tx, mut rx) = event_channel(4096);
    let events_pump = events_tx.clone();
    let opens = Arc::new(AtomicUsize::new(0));
    let opens_o = Arc::clone(&opens);

    // The resync fetch is REAL: genuine Bybit REST history through the real mapper.
    let seen_history = Arc::new(Mutex::new(Vec::<Event>::new()));
    let seen_h = Arc::clone(&seen_history);
    let feed = spawn_pump_with_resync(ResyncPumpSpec {
        pump_thread_name: "bybit-watchdog-proof-pump".to_string(),
        resync_thread_name: "bybit-watchdog-proof-resync".to_string(),
        poll: Duration::from_millis(200),
        settle: Duration::from_millis(500),
        events: events_tx,
        resync_fetch: move || {
            // Both getters already unwrap the v5 envelope down to `result.list`, so these ARE the
            // arrays `map_bybit_history` wants — do not reach for `.get("list")` again.
            let oh = rest_resync.get_order_history(50).unwrap_or_else(|_| json!([]));
            let eh = rest_resync.get_execution_history(50).unwrap_or_else(|_| json!([]));
            let evs = map_bybit_history(&oh, &eh, "bybit", "BTCUSDT");
            seen_h.lock().unwrap().extend(evs.iter().cloned());
            evs
        },
        on_reconcile: None,
        pump_body: move |stop: Arc<AtomicBool>, on_reconnect: ReconnectHook| {
            run_user_data_forever_with_idle(
                || {
                    opens_o.fetch_add(1, Ordering::Relaxed);
                    OpenOutcome::Ready(StalledStream)
                },
                |_| Vec::new(), // the stream never yields a frame, so this never runs
                |event| events_pump.blocking_send(event).is_ok(),
                &stop,
                Duration::from_millis(200),
                Duration::from_secs(1), // cap backoff so the proof runs in seconds
                None,
                on_reconnect,
                idle_threshold,
            )
        },
    });

    // --- place a REAL order while the session is dark ---
    let coid = format!("wdog{}", now_ms() % 100_000_000);
    let buy: vike_model::OrderRequest = serde_json::from_value(json!({
        "client_order_id": coid, "venue": "bybit", "symbol": "BTCUSDT",
        "side": 1, "qty": qty, "order_type": "market", "ts": now_ms()
    }))
    .expect("order request");
    let submit_events = rest_trade.submit_batch(std::slice::from_ref(&buy));
    tracing::info!(target: "vike_bybit", coid = %coid, events = submit_events.len(), "dark-window order placed");

    // --- did the activity reach the ingest lane? Only the A3 resync can deliver it here. ---
    // Scoped to THIS order's coid, deliberately. The resync replays the last 50 history rows, which
    // on a reused demo account contain fills from earlier runs — accepting any fill would let the
    // proof pass on stale history without the dark-window order ever being recovered.
    let deadline = Instant::now() + wait;
    let mut recovered = false;
    while Instant::now() < deadline && !recovered {
        match rx.try_recv() {
            Ok(Ingest::Event(ev)) => {
                if coid_of(&ev).is_some_and(|c| c == coid) && is_fill_shaped(&ev) {
                    tracing::info!(target: "vike_bybit", ?ev, "dark-window order recovered via A3 resync");
                    recovered = true;
                }
            }
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }

    // --- flatten, always, before tearing the pump down ---
    let sell: vike_model::OrderRequest = serde_json::from_value(json!({
        "client_order_id": format!("wdogX{}", now_ms() % 100_000_000),
        "venue": "bybit", "symbol": "BTCUSDT",
        "side": -1, "qty": qty, "order_type": "market", "reduce_only": true, "ts": now_ms()
    }))
    .expect("flatten request");
    let _ = rest_trade.submit_batch(std::slice::from_ref(&sell));

    let _ = feed.shutdown();
    (recovered, opens.load(Ordering::Relaxed))
}

/// THE PROOF: watchdog armed ⇒ the stalled session is detected, re-opened, and the A3 resync
/// replays the fill that landed in the dark window.
#[test]
#[ignore = "network + demo creds — places REAL demo orders (see module doc)"]
fn watchdog_trip_recovers_a_fill_lost_in_a_dark_session() {
    vike_log::test_init();
    if load_credentials_from(
        "bybit",
        Environment::Demo,
        &load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref()),
    )
    .is_none()
    {
        tracing::warn!(target: "vike_bybit", "SKIP: BYBIT_DEMO creds absent");
        return;
    }

    let (recovered, opens) =
        run_dark_session(Some(Duration::from_secs(3)), Duration::from_secs(45));

    assert!(
        opens >= 2,
        "the watchdog never forced a re-open (opens={opens}) — the trip half of the chain failed"
    );
    assert!(
        recovered,
        "the dark-window fill NEVER reached the ingest lane. The stream yields no frames, so the \
         A3 resync is the only possible path — trip -> reconnect -> gen bump -> REST replay is broken"
    );
}

/// THE CONTROL: same stalled session, watchdog OFF. This is production BEFORE the fix — the pump
/// parks forever, nothing reconnects, and the fill is lost. Asserting the failure keeps the proof
/// above honest: if this ever starts "recovering", the recovery is coming from somewhere else and
/// the sibling test proves nothing.
#[test]
#[ignore = "network + demo creds — places REAL demo orders (see module doc)"]
fn watchdog_off_never_recovers() {
    vike_log::test_init();
    if load_credentials_from(
        "bybit",
        Environment::Demo,
        &load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref()),
    )
    .is_none()
    {
        tracing::warn!(target: "vike_bybit", "SKIP: BYBIT_DEMO creds absent");
        return;
    }

    let (recovered, opens) = run_dark_session(None, Duration::from_secs(20));

    assert_eq!(opens, 1, "with no threshold the session must never re-open (opens={opens})");
    assert!(
        !recovered,
        "a fill reached the lane with the watchdog OFF — then the sibling proof does not isolate \
         the watchdog, and both tests need rethinking"
    );
}
