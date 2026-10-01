//! LIVE idle-cadence soak for the Bybit private WS — the empirical basis for
//! [`vike_bybit::user_data::IDLE_THRESHOLD`].
//!
//!     cargo test -p vike-bybit --test bybit_userdata_soak -- --ignored --nocapture
//!
//! Double-gated like every other live smoke: `#[ignore]` + demo creds in the workspace `.env`
//! (absent ⇒ self-skip). Places NO orders and sends nothing but the venue's own keepalive, so it
//! is read-only and safe to run against a funded demo account.
//!
//! **What it measures and why.** The user-data silent-stall watchdog trips when NO inbound frame
//! of any kind arrives within `IDLE_THRESHOLD`. On this lane data silence is normal (an account
//! with no orders emits no events for hours), so the threshold is justified purely by the
//! venue's answer to our app-level ping. This soak drives the REAL socket the way the pump does —
//! same handshake, same ping cadence — and records the true worst-case gap between inbound
//! frames, then asserts that gap leaves comfortable headroom under the shipped constant.
//!
//! A failure here means the threshold is too tight and would reconnect a HEALTHY socket in a
//! loop — the exact hazard that kept binance/deribit out of the rollout.
//!
//! Soak length defaults to 300s; override with `VIKE_SOAK_SECS` (e.g. `VIKE_SOAK_SECS=900`).

use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use vike_bridge_core::credentials::{
    Environment, load_credentials_from, load_workspace_dotenv_from,
};
use vike_bridge_core::user_data::{OpenOutcome, StreamError, UserStream};
use vike_bybit::perp::DEMO_WS;
use vike_bybit::user_data::{IDLE_THRESHOLD, open_bybit_user_data_ws};
use vike_model::clock::now_ms;

/// The pump's own cadence — mirrored here so the measurement reflects production behavior.
const PING_EVERY: Duration = Duration::from_secs(20);

fn soak_duration() -> Duration {
    let secs = std::env::var("VIKE_SOAK_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(300);
    Duration::from_secs(secs)
}

#[test]
#[ignore = "network + demo creds — multi-minute read-only soak (see module doc)"]
fn bybit_user_data_idle_cadence_soak() {
    vike_log::test_init();
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let Some(creds) = load_credentials_from("bybit", Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_bybit", "SKIP: BYBIT_DEMO creds absent");
        return;
    };

    let stop = AtomicBool::new(false);
    let gate = vike_bybit::ratelimit::ws_rate_gate();
    let mut ws = match open_bybit_user_data_ws(
        DEMO_WS,
        &creds.api_key,
        &creds.api_secret,
        now_ms,
        &["execution", "order"],
        &stop,
        &gate,
    ) {
        OpenOutcome::Ready(ws) => ws,
        OpenOutcome::Transport(e) => panic!("bybit user-data connect failed: {e}"),
        OpenOutcome::Auth(e) => panic!("bybit user-data auth failed: {e}"),
        OpenOutcome::Stopped => unreachable!("stop flag is never raised here"),
    };

    let soak = soak_duration();
    let started = Instant::now();
    let mut last_rx = Instant::now();
    let mut last_ping = Instant::now();
    let mut max_gap = Duration::ZERO;
    let mut frames = 0usize;

    while started.elapsed() < soak {
        if last_ping.elapsed() >= PING_EVERY {
            let _ = ws.send_text(&vike_bybit::ws_auth::ping_frame());
            last_ping = Instant::now();
        }
        match ws.recv() {
            Ok(_) => {
                max_gap = max_gap.max(last_rx.elapsed());
                last_rx = Instant::now();
                frames += 1;
            }
            // A read timeout is not a frame — the silence keeps growing, which is exactly what
            // the watchdog would be measuring at this instant.
            Err(StreamError::Timeout) => max_gap = max_gap.max(last_rx.elapsed()),
            Err(StreamError::Closed(msg)) => {
                panic!("socket closed {}s into the soak: {msg}", started.elapsed().as_secs())
            }
        }
    }

    tracing::info!(
        target: "vike_bybit",
        soak_s = soak.as_secs(),
        frames,
        max_gap_ms = max_gap.as_millis() as u64,
        threshold_ms = IDLE_THRESHOLD.as_millis() as u64,
        "bybit user-data idle cadence"
    );
    println!(
        "bybit idle cadence: {frames} frames in {}s, worst gap {:?}, threshold {:?}",
        soak.as_secs(),
        max_gap,
        IDLE_THRESHOLD
    );

    assert!(
        frames > 0,
        "no inbound frames at all in {}s — the ping is not being answered",
        soak.as_secs()
    );
    // Headroom, not a bare inequality: a threshold that only just clears the observed worst case
    // would flap on the first slow response.
    assert!(
        max_gap * 2 < IDLE_THRESHOLD,
        "worst inbound gap {max_gap:?} leaves under 2x headroom below IDLE_THRESHOLD \
         {IDLE_THRESHOLD:?} — raise the threshold before shipping"
    );
}
