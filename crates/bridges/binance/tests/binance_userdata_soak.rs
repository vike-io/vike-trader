//! LIVE idle-cadence MEASUREMENT for the Binance perp private WS — the Class-B blocker.
//!
//!     cargo test -p vike-binance --test binance_userdata_soak -- --ignored --nocapture
//!
//! Double-gated: `#[ignore]` + demo creds in the workspace `.env`. Read-only — places NO orders
//! and sends NOTHING on the socket at all (see below).
//!
//! **Why this one only reports.** Binance is the venue where the user-data silent-stall watchdog
//! could NOT be armed with the others: its keepalive is a REST `listenKey` PUT on a 1800s cadence
//! that never touches the socket, so — unlike bybit/okx/polymarket/hyperliquid — there is no
//! app-level ping whose answer proves liveness. The only inbound traffic on an idle account is
//! Binance's OWN server WS ping. That interval is the number this test exists to establish; until
//! it is measured, any threshold would be a guess, and a guess that is too tight reconnect-loops
//! a perfectly healthy socket forever.
//!
//! So this asserts only that the socket stays open and that SOME inbound frame arrives; it prints
//! the observed gaps for a human to turn into a threshold. Run it for at least 900s
//! (`VIKE_SOAK_SECS=900`) — a 300s window can miss a ~180s server-ping period entirely.

use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use vike_binance::family::listenkey::open_user_data_ws;
use vike_binance::perp::{DEMO_FAPI_REST, DEMO_FAPI_WS};
use vike_binance::perp_user_data::{HeaderAuth, LISTENKEY_PATH};
use vike_bridge_core::credentials::{
    Environment, load_credentials_from, load_workspace_dotenv_from,
};
use vike_bridge_core::user_data::{OpenOutcome, StreamError, UserStream};

fn soak_duration() -> Duration {
    let secs = std::env::var("VIKE_SOAK_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(900);
    Duration::from_secs(secs)
}

#[test]
#[ignore = "network + demo creds — long read-only measurement, run with VIKE_SOAK_SECS=900+"]
fn binance_perp_user_data_server_ping_interval() {
    vike_log::test_init();
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let Some(creds) = load_credentials_from("binance", Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_binance", "SKIP: BINANCE_DEMO creds absent");
        return;
    };

    let stop = AtomicBool::new(false);
    let auth = HeaderAuth::new(creds.api_key.clone());
    let mut ws = match open_user_data_ws(
        DEMO_FAPI_REST,
        LISTENKEY_PATH,
        DEMO_FAPI_WS,
        "", // perp connects `<ws_base>/<key>` — no `/ws` segment
        &auth,
        &stop,
    ) {
        OpenOutcome::Ready(ws) => ws,
        OpenOutcome::Transport(e) => panic!("binance perp user-data connect failed: {e}"),
        OpenOutcome::Auth(e) => panic!("binance perp user-data auth failed: {e}"),
        OpenOutcome::Stopped => unreachable!("stop flag is never raised here"),
    };

    let soak = soak_duration();
    let started = Instant::now();
    let mut last_rx = Instant::now();
    let mut max_gap = Duration::ZERO;
    let mut gaps: Vec<Duration> = Vec::new();

    // NOTHING is sent on this socket — the whole point is to observe the SERVER's unprompted
    // cadence. (The pump's listenKey PUT keepalive is REST-side and irrelevant over this window:
    // the key lives 60 min and the PUT fires at 30.)
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
        "binance perp idle cadence over {}s: {} inbound frames, gaps(s)={observed:?}, worst {:?}",
        soak.as_secs(),
        gaps.len(),
        max_gap
    );
    println!(
        "=> a Class-B IDLE_THRESHOLD must be well above {:?} (suggest >= 4x the typical gap)",
        max_gap
    );

    assert!(
        !gaps.is_empty(),
        "no inbound frame in {}s — cannot establish a server-ping interval; re-run longer before \
         arming any threshold on this venue",
        soak.as_secs()
    );
}
