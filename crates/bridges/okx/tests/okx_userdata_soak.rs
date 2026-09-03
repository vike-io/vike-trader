//! LIVE idle-cadence soak for the OKX private WS — the empirical basis for
//! [`vike_okx::user_data::IDLE_THRESHOLD`]. OKX twin of `bybit_userdata_soak.rs`; see that file's
//! module doc for what is measured and why.
//!
//!     cargo test -p vike-okx --test okx_userdata_soak -- --ignored --nocapture
//!
//! Read-only: places NO orders, sends only the raw-text `ping` keepalive the pump sends.
//! Soak length defaults to 300s; override with `VIKE_SOAK_SECS`.

use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use vike_bridge_core::credentials::{
    load_credentials_from, load_workspace_dotenv_from, Environment,
};
use vike_bridge_core::user_data::{OpenOutcome, StreamError, UserStream};
use vike_model::clock::now_ms;
use vike_okx::perp::DEMO_WS;
use vike_okx::user_data::{open_okx_user_data_ws, IDLE_THRESHOLD};
use vike_okx::ws_auth::PING_TEXT;

/// The pump's own cadence — mirrored so the measurement reflects production behavior.
const PING_EVERY: Duration = Duration::from_secs(15);

fn soak_duration() -> Duration {
    let secs = std::env::var("VIKE_SOAK_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(300);
    Duration::from_secs(secs)
}

#[test]
#[ignore = "network + demo creds — multi-minute read-only soak (see module doc)"]
fn okx_user_data_idle_cadence_soak() {
    vike_log::test_init();
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let Some(creds) = load_credentials_from("okx", Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_okx", "SKIP: OKX_DEMO creds absent");
        return;
    };
    let Some(passphrase) = creds.passphrase.clone() else {
        tracing::warn!(target: "vike_okx", "SKIP: OKX_DEMO passphrase absent");
        return;
    };

    let stop = AtomicBool::new(false);
    let gate = vike_okx::ratelimit::ws_rate_gate();
    let mut ws = match open_okx_user_data_ws(
        DEMO_WS,
        &creds.api_key,
        &creds.api_secret,
        &passphrase,
        now_ms,
        "SWAP",
        &stop,
        &gate,
    ) {
        OpenOutcome::Ready(ws) => ws,
        OpenOutcome::Transport(e) => panic!("okx user-data connect failed: {e}"),
        OpenOutcome::Auth(e) => panic!("okx user-data auth failed: {e}"),
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
            let _ = ws.send_text(PING_TEXT);
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
        target: "vike_okx",
        soak_s = soak.as_secs(),
        frames,
        max_gap_ms = max_gap.as_millis() as u64,
        threshold_ms = IDLE_THRESHOLD.as_millis() as u64,
        "okx user-data idle cadence"
    );
    println!(
        "okx idle cadence: {frames} frames in {}s, worst gap {:?}, threshold {:?}",
        soak.as_secs(),
        max_gap,
        IDLE_THRESHOLD
    );

    assert!(
        frames > 0,
        "no inbound frames at all in {}s — the ping is not being answered",
        soak.as_secs()
    );
    assert!(
        max_gap * 2 < IDLE_THRESHOLD,
        "worst inbound gap {max_gap:?} leaves under 2x headroom below IDLE_THRESHOLD \
         {IDLE_THRESHOLD:?} — raise the threshold before shipping"
    );
}
