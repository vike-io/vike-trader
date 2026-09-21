//! LIVE idle-cadence MEASUREMENT for the Binance **spot** user-data stream — the other half of the
//! Class-B blocker (`binance_userdata_soak.rs` measured perp).
//!
//!     cargo test -p vike-binance --test binance_spot_userdata_soak -- --ignored --nocapture
//!
//! Double-gated: `#[ignore]` + demo creds. Read-only — places nothing and sends nothing.
//!
//! **Why spot needs its own number.** Spot does NOT share the listenKey family pump that perp and
//! Aster use: it drives the bridge-core loop directly over the WS-API endpoint
//! (`demo-ws-api.binance.com/ws-api/v3`), a different stream on a different host with a different
//! handshake. Perp's measured 180.0s server-ping interval is therefore evidence about perp only —
//! assuming it here is exactly the cross-venue inference this whole rollout has refused to make.
//!
//! Reports rather than asserts a threshold, because spot is not armed: the point is to establish
//! the interval so a value can be chosen from evidence. Run with `VIKE_SOAK_SECS=900`; a 300s
//! window can miss a ~180s period entirely.

use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use vike_binance::user_data::{DEMO_WS, open_binance_user_data_ws};
use vike_bridge_core::credentials::{
    Environment, load_credentials_from, load_workspace_dotenv_from,
};
use vike_bridge_core::user_data::{OpenOutcome, StreamError, UserStream};
use vike_model::clock::now_ms;

fn soak_duration() -> Duration {
    let secs = std::env::var("VIKE_SOAK_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(900);
    Duration::from_secs(secs)
}

#[test]
#[ignore = "network + demo creds — long read-only measurement, run with VIKE_SOAK_SECS=900+"]
fn binance_spot_user_data_server_ping_interval() {
    vike_log::test_init();
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let Some(creds) = load_credentials_from("binance", Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_binance", "SKIP: BINANCE_DEMO creds absent");
        return;
    };

    let stop = AtomicBool::new(false);
    let gate = vike_binance::ratelimit::ws_rate_gate();
    let mut ws = match open_binance_user_data_ws(
        DEMO_WS,
        &creds.api_key,
        &creds.api_secret,
        now_ms,
        &stop,
        &gate,
    ) {
        OpenOutcome::Ready(ws) => ws,
        OpenOutcome::Transport(e) => panic!("binance spot user-data connect failed: {e}"),
        OpenOutcome::Auth(e) => panic!("binance spot user-data auth failed: {e}"),
        OpenOutcome::Stopped => unreachable!("stop flag is never raised here"),
    };

    let soak = soak_duration();
    let started = Instant::now();
    let mut last_rx = Instant::now();
    let mut max_gap = Duration::ZERO;
    let mut gaps: Vec<Duration> = Vec::new();

    // Nothing is sent: the whole point is the SERVER's unprompted cadence.
    while started.elapsed() < soak {
        match ws.recv() {
            Ok(_) => {
                let gap = last_rx.elapsed();
                max_gap = max_gap.max(gap);
                gaps.push(gap);
                last_rx = Instant::now();
                tracing::info!(target: "vike_binance", gap_s = gap.as_secs_f64(), "inbound frame");
            }
            Err(StreamError::Timeout) => max_gap = max_gap.max(last_rx.elapsed()),
            Err(StreamError::Closed(msg)) => {
                panic!("socket closed {}s into the soak: {msg}", started.elapsed().as_secs())
            }
        }
    }

    let observed: Vec<u64> = gaps.iter().map(|g| g.as_secs()).collect();
    println!(
        "binance SPOT idle cadence over {}s: {} inbound frames, gaps(s)={observed:?}, worst {:?}",
        soak.as_secs(),
        gaps.len(),
        max_gap
    );
    println!(
        "=> a spot IDLE_THRESHOLD must be well above {max_gap:?} (suggest >= 4x the typical gap)"
    );

    assert!(
        !gaps.is_empty(),
        "no inbound frame in {}s — cannot establish a server-ping interval; re-run longer before \
         arming any threshold on binance spot",
        soak.as_secs()
    );
}
