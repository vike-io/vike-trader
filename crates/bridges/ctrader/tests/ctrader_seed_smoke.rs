//! LIVE demo smoke for the connect-time POSITION SEED, and the measurement its bound is sized on:
//!     cargo test -p vike-ctrader --test ctrader_seed_smoke -- --ignored --nocapture
//!
//! `crates/bridges/ctrader/src/conn.rs`'s `seed_positions_at_connect` adds a blocking
//! `ProtoOAReconcileReq` round trip INSIDE the exec handshake, on the mount path — cTrader is the
//! one venue whose exec client connects SYNCHRONOUSLY at mount (`vike_mount::make_engine`'s
//! `ctrader` arm), so that round trip is startup latency an operator waits on. Its ceiling
//! (`conn::SEED_TIMEOUT`) is therefore a number that has to be MEASURED against the real venue, not
//! guessed: too low and a healthy seed becomes a coin flip, too high and a mute venue holds up
//! every mount behind it.
//!
//! This is that measurement, and the live proof of the seed itself. It mounts the REAL exec
//! handshake against `demo.ctraderapi.com:5035` [`REPS`] times and, per mount, asserts the returned
//! `ActorHandle`'s book is FETCHED — the state a bare `HashMap` could not represent, and the whole
//! point of the seed. The seed's OWN cost prints from `seed_positions_at_connect`'s `position book
//! seeded at connect`
//! line (`elapsed_ms`), which is why `--nocapture` is in the command above; the mount wall time this
//! file prints is the seed PLUS the two-stage OAuth handshake and the full symbol-list download, so
//! it is the outer bound, never the seed's cost.
//!
//! READ-ONLY, and it must stay that way: it places NO order and closes nothing, so the account is
//! left exactly as it was found (`open_positions` is reported, not modified).
//!
//! Credentials come from `<project>/settings/secrets.env` (`CTRADER_CLIENT_ID`/`_SECRET` +
//! `CTRADER_DEMO_ACCESS_TOKEN`/`_REFRESH_TOKEN`/optional `_ACCOUNT_ID` — see
//! `config::CtraderConfig`), loaded via `load_workspace_dotenv_from` + `CtraderConfig::from_vars`. Absent
//! creds -> the test SKIPS (the live gate), never fails CI (it is also `#[ignore]`d, so it never
//! even runs there).

mod common;

use std::sync::Arc;
use std::time::Instant;

use vike_bridge_core::credentials::Environment;
use vike_ctrader::config::CtraderConfig;
use vike_ctrader::conn::{SEED_TIMEOUT, connect_and_auth_exec};
use vike_exec::event_channel;

use common::NoopSink;

/// How many real mounts to time. Enough for a median rather than a single sample (one connect can
/// catch a TLS session miss or a venue hiccup), few enough that the smoke stays a ~1 minute run.
const REPS: usize = 5;

#[test]
#[ignore = "network + REAL cTrader demo endpoint + CTRADER_DEMO creds — run manually (see module doc)"]
fn ctrader_seed_at_connect_smoke() {
    vike_log::test_init();
    // Tests own the store I/O (the same shape as every other cTrader smoke): load the map here,
    // then the same pure `from_vars` gate production uses.
    let vars = vike_bridge_core::credentials::load_workspace_dotenv_from(
        std::env::var("VIKE_SETTINGS_DIR").ok().as_deref(),
    );
    let Some(config) = CtraderConfig::from_vars(Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_ctrader::smoke", "SKIP: CTRADER_DEMO creds absent");
        return;
    };

    let mut mounts_ms: Vec<u128> = Vec::with_capacity(REPS);
    for rep in 1..=REPS {
        // Keep `_rx` bound for the mount's lifetime: the actor's event lane needs a live receiver.
        let (events, _rx) = event_channel(256);
        let started = Instant::now();
        let handle =
            connect_and_auth_exec(config.to_conn_config(), Arc::new(NoopSink), events.clone())
                .expect("exec handshake against the real cTrader demo endpoint");
        let mount_ms = started.elapsed().as_millis();
        mounts_ms.push(mount_ms);

        // THE assertion. Not "the book is empty" — an empty book is the value a mount that never
        // asked also has. `is_fetched()` is true only because the venue answered this connect's
        // `ProtoOAReconcileReq`, which is what did not happen before the seed existed.
        let (fetched, open) = {
            let book = handle.positions.lock().expect("position book lock");
            (book.is_fetched(), book.len())
        };
        assert!(
            fetched,
            "rep {rep}: the mount returned with an UNFETCHED book — the connect-time seed did not \
             land (write failure, venue ERROR_RES, or it exceeded SEED_TIMEOUT = {}ms; the \
             `initial position seed` warn line says which)",
            SEED_TIMEOUT.as_millis()
        );
        tracing::info!(
            target: "vike_ctrader::smoke",
            rep, mount_ms = mount_ms as u64, open_positions = open,
            "exec mount returned with a FETCHED position book"
        );
        handle.shutdown();
    }

    mounts_ms.sort_unstable();
    tracing::info!(
        target: "vike_ctrader::smoke",
        min_ms = mounts_ms[0] as u64,
        median_ms = mounts_ms[REPS / 2] as u64,
        max_ms = mounts_ms[REPS - 1] as u64,
        seed_timeout_ms = SEED_TIMEOUT.as_millis() as u64,
        "ctrader connect-seed smoke green — WHOLE-MOUNT wall times (OAuth + symbols + seed); the \
         seed's own cost is the `position book seeded at connect` line's elapsed_ms"
    );
}
