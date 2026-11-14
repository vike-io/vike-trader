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

use vike_binance::execution::BinanceUrls;
use vike_binance::execution::user_data::spot::open_binance_user_data_websocket;
use vike_bridge_core::websocket_session::user::{
    WebsocketSessionUserError, WebsocketSessionUserOpenOutcome, WebsocketSessionUserStream,
};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::now_ms;
use vike_secrets::credentials::api_credentials;
use vike_venues::venue_urls::VenueUrls;

fn soak_duration() -> Duration {
    let secs = std::env::var("VIKE_SOAK_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(900);
    Duration::from_secs(secs)
}

#[test]
#[ignore = "network + demo creds — long read-only measurement, run with VIKE_SOAK_SECS=900+"]
fn binance_spot_user_data_server_ping_interval() {
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("binance", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "binance") else {
        tracing::warn!(target: "vike_binance", "SKIP: BINANCE_DEMO creds absent");
        return;
    };

    // The demo WS-API session: the release's `venue_url` rows (`crates/vike-venues/src/venue_urls.rs`'s
    // `VENUE_URL_SEED`).
    let demo_websocket = BinanceUrls::resolve(&VenueUrls::seed("binance"), AccountTier::Demo)
        .expect("the seed holds every binance row")
        .spot_websocket;
    let stop = AtomicBool::new(false);
    let gate = vike_binance::transport::ratelimit::websocket_rate_gate();
    let mut ws = match open_binance_user_data_websocket(
        demo_websocket,
        creds.api_key.expose(),
        creds.api_secret.expose(),
        now_ms,
        &stop,
        &gate,
    ) {
        WebsocketSessionUserOpenOutcome::Ready(ws) => ws,
        WebsocketSessionUserOpenOutcome::Transport(e) => {
            panic!("binance spot user-data connect failed: {e}")
        }
        WebsocketSessionUserOpenOutcome::Auth(e) => {
            panic!("binance spot user-data auth failed: {e}")
        }
        WebsocketSessionUserOpenOutcome::Stopped => unreachable!("stop flag is never raised here"),
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
            Err(WebsocketSessionUserError::Timeout) => max_gap = max_gap.max(last_rx.elapsed()),
            Err(WebsocketSessionUserError::Closed(msg)) => {
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
