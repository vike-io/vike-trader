//! LIVE idle-cadence soak for the Aster perp user-data stream — the measurement that would let
//! Aster's silent-stall watchdog be armed.
//!
//!     cargo test -p vike-aster --test aster_userdata_soak -- --ignored --nocapture
//!
//! Runs against **TESTNET** (`AccountTier::Demo` → `urls_for` testnet hosts), read-only: mints a
//! listenKey, listens, sends nothing, places nothing.
//!
//! ## Status: needs credentials that are not configured yet
//!
//! It reads the DEFAULT aster account at tier **`demo`**, and that account's **`USER` +
//! `PRIVATE_KEY`** fields ([`vike_aster::signing::load_aster_credentials`] — the agent-wallet shape
//! rather than the usual `API_KEY`/`API_SECRET`). No such account holds them in the credential
//! store (the settings database) today — only the `live`-tier aster account holds a pair — so this
//! self-skips.
//!
//! The testnet itself is REAL and reachable — `https://fapi.asterdex-testnet.com/fapi/v1/time`
//! answers HTTP 200, and all three testnet hosts resolve. Give a `demo` aster account the two
//! fields above and this runs.
//!
//! ⚠ It deliberately refuses to fall back to the `live` account. Aster's live keys are a REAL
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
use vike_bridge_core::user_data::{OpenOutcome, StreamError, UserStream};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;

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
/// opt-in. `None` means "skip this test" (already logged why). The tier is the ACCOUNT's: the
/// default aster account at `demo`, and — opted in — the default account at `live`.
fn resolve_tier() -> Option<(AccountTier, vike_secrets::credentials::Credentials)> {
    let dir = vike_secrets::smoke_directory();
    let account = |tier| {
        load_aster_credentials(&dir.credentials_for(
            vike_aster::urls::VENUE,
            &AccountLabel::Default,
            tier,
        ))
    };
    match account(AccountTier::Demo) {
        Some(c) => Some((AccountTier::Demo, c)),
        None if mainnet_opt_in() => {
            let c = account(AccountTier::Live).or_else(|| {
                tracing::warn!(target: "vike_aster", "SKIP: neither the demo nor the live aster account holds its pair");
                None
            })?;
            tracing::warn!(
                target: "vike_aster",
                "{}=1 — soaking the REAL MAINNET account. Read-only: mints a listenKey and                  listens; sends nothing, places nothing, moves nothing.",
                ALLOW_MAINNET_ENV
            );
            Some((AccountTier::Live, c))
        }
        None => {
            tracing::warn!(
                target: "vike_aster",
                "SKIP: the demo aster account holds no USER + PRIVATE_KEY (only the live one is                  configured; set {}=1 to soak mainnet read-only instead)",
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
    let urls = urls_for(resolve_tier().map(|(e, _)| e).unwrap_or(AccountTier::Demo));
    measure_lane("perp", urls.fapi_rest, PERP_LISTENKEY_PATH, urls.fapi_ws);
}

/// SPOT (sapi/sstream) — a DIFFERENT host and listenKey endpoint from perp, so perp's 300s is not
/// evidence about it. Binance shows the divergence inside one exchange: spot 20s vs perp 180s.
#[test]
#[ignore = "network + creds — long read-only measurement (see module doc)"]
fn aster_spot_user_data_server_ping_interval() {
    vike_log::test_init();
    let urls = urls_for(resolve_tier().map(|(e, _)| e).unwrap_or(AccountTier::Demo));
    measure_lane("spot", urls.sapi_rest, SPOT_LISTENKEY_PATH, urls.sapi_ws);
}
