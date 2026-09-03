//! LIVE idle-cadence soak for the Deribit private WS — the empirical basis for
//! [`vike_deribit::user_data::IDLE_THRESHOLD`]. Twin of `bybit_userdata_soak.rs`.
//!
//!     cargo test -p vike-deribit --test deribit_userdata_soak -- --ignored --nocapture
//!
//! Read-only: places NO orders, sends only the `public/test` keepalive the pump sends.
//! Soak length defaults to 300s; override with `VIKE_SOAK_SECS`.
//!
//! **This one also validates a behavior change, not just a constant.** Deribit had no app-level
//! ping until the `public/test` keepalive was added — its sole inbound traffic on an idle account
//! was the 600s token-refresh reply. If `public/test` were ever silently dropped or stopped being
//! answered, this soak is what catches it, and the venue would have to be disarmed again.

use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use vike_bridge_core::credentials::{
    load_credentials_from, load_workspace_dotenv_from, Environment,
};
use vike_bridge_core::user_data::{OpenOutcome, StreamError, UserStream};
use vike_deribit::rpc::JsonRpcBuilder;
use vike_deribit::transport::TESTNET_WS;
use vike_deribit::user_data::{open_deribit_user_data_ws, IDLE_THRESHOLD};
use vike_deribit::ws_auth::build_public_test;

/// The pump's own cadence — mirrored so the measurement reflects production behavior.
const PING_EVERY: Duration = Duration::from_secs(20);

fn soak_duration() -> Duration {
    let secs = std::env::var("VIKE_SOAK_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(300);
    Duration::from_secs(secs)
}

#[test]
#[ignore = "network + demo creds — multi-minute read-only soak (see module doc)"]
fn deribit_user_data_idle_cadence_soak() {
    vike_log::test_init();
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let Some(creds) = load_credentials_from("deribit", Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_deribit", "SKIP: DERIBIT_DEMO creds absent");
        return;
    };

    let stop = AtomicBool::new(false);
    let gate = vike_deribit::ratelimit::ws_rate_gate();
    let builder = JsonRpcBuilder::new();
    let token_cell = Mutex::new(String::new());
    let channels = vec!["user.trades.any.any.raw".to_string()];

    let mut ws = match open_deribit_user_data_ws(
        TESTNET_WS,
        &creds.api_key,
        &creds.api_secret,
        &channels,
        &builder,
        &token_cell,
        &stop,
        &gate,
    ) {
        OpenOutcome::Ready(ws) => ws,
        OpenOutcome::Transport(e) => panic!("deribit user-data connect failed: {e}"),
        OpenOutcome::Auth(e) => panic!("deribit user-data auth failed: {e}"),
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
            let _ = ws.send_text(&build_public_test(builder.next_id()).to_string());
            last_ping = Instant::now();
        }
        match ws.recv() {
            Ok(_) => {
                max_gap = max_gap.max(last_rx.elapsed());
                last_rx = Instant::now();
                frames += 1;
            }
            Err(StreamError::Timeout) => max_gap = max_gap.max(last_rx.elapsed()),
            Err(StreamError::Closed(msg)) => {
                panic!("socket closed {}s into the soak: {msg}", started.elapsed().as_secs())
            }
        }
    }

    tracing::info!(
        target: "vike_deribit",
        soak_s = soak.as_secs(),
        frames,
        max_gap_ms = max_gap.as_millis() as u64,
        threshold_ms = IDLE_THRESHOLD.as_millis() as u64,
        "deribit user-data idle cadence"
    );
    println!(
        "deribit idle cadence: {frames} frames in {}s, worst gap {:?}, threshold {:?}",
        soak.as_secs(),
        max_gap,
        IDLE_THRESHOLD
    );

    assert!(
        frames > 0,
        "no inbound frames in {}s — public/test is not being answered, so the venue must be \
         DISARMED (idle_threshold back to None) until a working ping exists",
        soak.as_secs()
    );
    assert!(
        max_gap * 2 < IDLE_THRESHOLD,
        "worst inbound gap {max_gap:?} leaves under 2x headroom below IDLE_THRESHOLD \
         {IDLE_THRESHOLD:?} — raise the threshold before shipping"
    );
}
