//! LIVE idle-cadence soak for the Hyperliquid user streams — converts
//! the exec lane's `idle_threshold` ([`vike_venues::venue_timings::HYPERLIQUID`]) from
//! INFERRED to measured.
//!
//!     cargo test -p vike-hyperliquid --test hyperliquid_userdata_soak -- --ignored --nocapture
//!
//! Runs against **testnet**, and needs no key at all: HL user streams are address-scoped and
//! keyless, so this only needs the default hyperliquid account's `ACCOUNT_ADDRESS` field in the
//! credential store (the `demo` account's, else the `live` one's) to know whose stream to
//! subscribe. Read-only — subscribes and listens, places nothing.
//!
//! Measures the worst gap between INBOUND frames of any kind, which is exactly what the pump's
//! silent-stall watchdog clocks. HL answers `{"method":"ping"}` with a pong, so a healthy socket
//! stays live on control frames alone even with zero account activity.

use std::time::{Duration, Instant};

use vike_bridge_core::user_data::{OpenOutcome, StreamError, UserStream};
use vike_hyperliquid::base::consts::{TESTNET_WS, VENUE};
use vike_hyperliquid::exec::user_data::{open_ws, subscribe_frame};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_venues::venue_timings::HYPERLIQUID;

/// The pump's own ping cadence and silent-stall threshold: the same exec-lane row the pump reads,
/// so the measurement reflects production.
const PING_EVERY: Duration = HYPERLIQUID.exec().keepalive_every();
const IDLE_THRESHOLD: Duration = HYPERLIQUID.exec().idle_threshold();
const PING_FRAME: &str = r#"{"method":"ping"}"#;

fn soak_duration() -> Duration {
    let secs = std::env::var("VIKE_SOAK_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(300);
    Duration::from_secs(secs)
}

#[test]
#[ignore = "network — multi-minute read-only testnet soak (see module doc)"]
fn hyperliquid_user_data_idle_cadence_soak() {
    vike_log::test_init();
    let dir = vike_secrets::smoke_directory();
    // Any address works — the stream is public per-address. Prefer the demo account's if configured.
    let Some(address) = [AccountTier::Demo, AccountTier::Live].into_iter().find_map(|tier| {
        let account = dir.credentials_for(VENUE, &AccountLabel::Default, tier);
        account.get("ACCOUNT_ADDRESS").map(str::to_string)
    }) else {
        tracing::warn!(target: "vike_hyperliquid", "SKIP: no hyperliquid account holds an ACCOUNT_ADDRESS");
        return;
    };

    let order_sub = subscribe_frame("orderUpdates", &address);
    let fills_sub = subscribe_frame("userFills", &address);
    let mut ws = match open_ws(TESTNET_WS, &order_sub, &fills_sub) {
        OpenOutcome::Ready(ws) => ws,
        OpenOutcome::Transport(e) => panic!("hyperliquid testnet connect failed: {e}"),
        OpenOutcome::Auth(e) => panic!("unexpected auth outcome on a keyless stream: {e}"),
        OpenOutcome::Stopped => unreachable!("no stop flag is involved here"),
    };

    let soak = soak_duration();
    let started = Instant::now();
    let mut last_rx = Instant::now();
    let mut last_ping = Instant::now();
    let mut max_gap = Duration::ZERO;
    let mut frames = 0usize;

    while started.elapsed() < soak {
        if last_ping.elapsed() >= PING_EVERY {
            let _ = ws.send_text(PING_FRAME);
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
        target: "vike_hyperliquid",
        soak_s = soak.as_secs(),
        frames,
        max_gap_ms = max_gap.as_millis() as u64,
        threshold_ms = IDLE_THRESHOLD.as_millis() as u64,
        "hyperliquid user-data idle cadence"
    );
    println!(
        "hyperliquid idle cadence: {frames} frames in {}s, worst gap {:?}, threshold {:?}",
        soak.as_secs(),
        max_gap,
        IDLE_THRESHOLD
    );

    assert!(
        frames > 0,
        "no inbound frames in {}s — the ping is not being answered",
        soak.as_secs()
    );
    assert!(
        max_gap * 2 < IDLE_THRESHOLD,
        "worst inbound gap {max_gap:?} leaves under 2x headroom below IDLE_THRESHOLD \
         {IDLE_THRESHOLD:?} — raise the threshold before shipping"
    );
}
