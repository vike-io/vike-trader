//! LIVE idle-cadence soak for the Aster perp user-data stream — the measurement that would let
//! Aster's silent-stall watchdog be armed.
//!
//!     cargo test -p vike-aster --test aster_userdata_soak -- --ignored --nocapture
//!
//! Runs against **TESTNET** (`Environment::Demo` → `urls_for` testnet hosts), read-only: mints a
//! listenKey, listens, sends nothing, places nothing.
//!
//! ## Status: needs credentials that are not configured yet
//!
//! Aster's non-live tier reads **`ASTER_TESTNET_USER` + `ASTER_TESTNET_PRIVATE_KEY`**
//! ([`vike_aster::signing::load_aster_credentials`] — note `TESTNET`, not `DEMO`, and the bespoke
//! agent-wallet shape rather than the usual `_API_KEY`/`_API_SECRET`). Neither is present in the
//! credential store (`<project>/settings/secrets.env`) today, which holds only `ASTER_LIVE_*`, so
//! this self-skips.
//!
//! The testnet itself is REAL and reachable — `https://fapi.asterdex-testnet.com/fapi/v1/time`
//! answers HTTP 200, and all three testnet hosts resolve. Add the two vars above and this runs.
//!
//! ⚠ It deliberately refuses to fall back to `Environment::Live`. Aster's live keys are a REAL
//! mainnet account; a soak is read-only but still authenticates against it, and the standing rule
//! is that validation runs never touch a live account.
//!
//! ## Why Aster needs its OWN number
//!
//! Aster shares Binance's listenKey pump, and that family has NO app-level ping — the keepalive is
//! a REST listenKey PUT that never touches the socket — so the only inbound traffic on an idle
//! account is the venue's own server ping. Binance perp measured 180s and Binance SPOT measured
//! 20s: a 9x spread WITHIN one exchange. Assuming either for Aster would be exactly the
//! cross-venue inference this rollout has refused everywhere else.

use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use vike_aster::listenkey_auth::AsterListenKeyAuth;
use vike_aster::perp_user_data::LISTENKEY_PATH as PERP_LISTENKEY_PATH;
use vike_aster::signing::load_aster_credentials;
use vike_aster::urls::urls_for;
use vike_aster::user_data::LISTENKEY_PATH as SPOT_LISTENKEY_PATH;
use vike_binance::family::listenkey::open_user_data_ws;
use vike_bridge_core::Environment;
use vike_bridge_core::credentials::load_workspace_dotenv_from;
use vike_bridge_core::user_data::{OpenOutcome, StreamError, UserStream};

/// Opt-in gate for the mainnet arm. Aster has no testnet CREDENTIALS configured (the testnet itself
/// exists — see the module doc), so measuring its server-ping cadence at all currently requires the
/// live account. The soak is read-only, but it still AUTHENTICATES against real money, which is why
/// this is a deliberate per-run flag rather than a silent fallback: the default path still refuses.
const ALLOW_MAINNET_ENV: &str = "ASTER_SOAK_ALLOW_MAINNET";

/// The EXACT string `"1"` — the same idiom as `VIKE_RECONCILE`, not a fuzzy truthy parse.
fn mainnet_opt_in() -> bool {
    std::env::var(ALLOW_MAINNET_ENV).ok().as_deref() == Some("1")
}

fn soak_duration() -> Duration {
    let secs = std::env::var("VIKE_SOAK_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(900);
    Duration::from_secs(secs)
}

/// Resolve the credential tier for a run: testnet by default, mainnet only behind the explicit
/// opt-in. `None` means "skip this test" (already logged why).
fn resolve_tier() -> Option<(Environment, vike_bridge_core::credentials::Credentials)> {
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    match load_aster_credentials(Environment::Demo, &vars) {
        Some(c) => Some((Environment::Demo, c)),
        None if mainnet_opt_in() => {
            let c = load_aster_credentials(Environment::Live, &vars).or_else(|| {
                tracing::warn!(target: "vike_aster", "SKIP: neither ASTER_TESTNET_* nor ASTER_LIVE_* present");
                None
            })?;
            tracing::warn!(
                target: "vike_aster",
                "{}=1 — soaking the REAL MAINNET account. Read-only: mints a listenKey and                  listens; sends nothing, places nothing, moves nothing.",
                ALLOW_MAINNET_ENV
            );
            Some((Environment::Live, c))
        }
        None => {
            tracing::warn!(
                target: "vike_aster",
                "SKIP: ASTER_TESTNET_USER + ASTER_TESTNET_PRIVATE_KEY absent (only ASTER_LIVE_* is                  configured; set {}=1 to soak mainnet read-only instead)",
                ALLOW_MAINNET_ENV
            );
            None
        }
    }
}

/// Measure one stream's unprompted SERVER cadence. `lane` names it in the output ("perp"/"spot").
///
/// Shared by both lanes on purpose: they differ ONLY in host + listenKey path, so a divergence in
/// the measurement loop itself would make the two numbers incomparable — and comparability is the
/// whole point (binance spot 20s vs perp 180s is why aster spot cannot inherit aster perp's 300s).
fn measure_lane(lane: &str, rest_url: &str, listenkey_path: &'static str, ws_base_url: &str) {
    let Some((_env, creds)) = resolve_tier() else { return };

    let stop = AtomicBool::new(false);
    let auth = AsterListenKeyAuth::new(&creds, listenkey_path);
    let mut ws = match open_user_data_ws(rest_url, listenkey_path, ws_base_url, "/ws", &auth, &stop)
    {
        OpenOutcome::Ready(ws) => ws,
        OpenOutcome::Transport(e) => panic!("aster {lane} user-data connect failed: {e}"),
        OpenOutcome::Auth(e) => panic!("aster {lane} user-data auth failed: {e}"),
        OpenOutcome::Stopped => unreachable!("stop flag is never raised here"),
    };

    let soak = soak_duration();
    let started = Instant::now();
    let mut last_rx = Instant::now();
    let mut max_gap = Duration::ZERO;
    let mut gaps: Vec<Duration> = Vec::new();

    // Nothing is sent: the point is the SERVER's unprompted cadence. (The listenKey PUT keepalive
    // is REST-side and irrelevant over this window — the key lives 60 min, the PUT fires at 30.)
    while started.elapsed() < soak {
        match ws.recv() {
            Ok(_) => {
                let gap = last_rx.elapsed();
                max_gap = max_gap.max(gap);
                gaps.push(gap);
                last_rx = Instant::now();
                tracing::info!(target: "vike_aster", %lane, gap_s = gap.as_secs_f64(), "inbound frame");
            }
            Err(StreamError::Timeout) => max_gap = max_gap.max(last_rx.elapsed()),
            Err(StreamError::Closed(msg)) => {
                panic!("socket closed {}s into the {lane} soak: {msg}", started.elapsed().as_secs())
            }
        }
    }

    let observed: Vec<u64> = gaps.iter().map(|g| g.as_secs()).collect();
    println!(
        "aster {lane} idle cadence over {}s: {} inbound frames, gaps(s)={observed:?}, worst {:?}",
        soak.as_secs(),
        gaps.len(),
        max_gap
    );
    println!("=> an aster {lane} IDLE_THRESHOLD must be well above {max_gap:?} (>= 4x typical)");

    assert!(
        !gaps.is_empty(),
        "no inbound frame in {}s on the {lane} lane — cannot establish a server-ping interval; do          NOT arm it until one is measured",
        soak.as_secs()
    );
}

/// PERP (fapi/fstream) — measured 2026-07-29 at 300s, now armed at 1200s in `perp_user_data.rs`.
#[test]
#[ignore = "network + creds — long read-only measurement (see module doc)"]
fn aster_perp_user_data_server_ping_interval() {
    vike_log::test_init();
    let urls = urls_for(resolve_tier().map(|(e, _)| e).unwrap_or(Environment::Demo));
    measure_lane("perp", urls.fapi_rest, PERP_LISTENKEY_PATH, urls.fapi_ws);
}

/// SPOT (sapi/sstream) — a DIFFERENT host and listenKey endpoint from perp, so perp's 300s is not
/// evidence about it. Binance shows the divergence inside one exchange: spot 20s vs perp 180s.
#[test]
#[ignore = "network + creds — long read-only measurement (see module doc)"]
fn aster_spot_user_data_server_ping_interval() {
    vike_log::test_init();
    let urls = urls_for(resolve_tier().map(|(e, _)| e).unwrap_or(Environment::Demo));
    measure_lane("spot", urls.sapi_rest, SPOT_LISTENKEY_PATH, urls.sapi_ws);
}
