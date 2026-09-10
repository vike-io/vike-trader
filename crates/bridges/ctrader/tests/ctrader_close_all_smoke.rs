//! LIVE demo smoke: clear a PRE-EXISTING stacked position book via `CtraderExec::close_all`.
//!     cargo test -p vike-ctrader --test ctrader_close_all_smoke -- --ignored --nocapture
//!
//! The demo account can accumulate positions a fresh connect never tracked (e.g. hedged pairs a
//! prior session's opposite orders opened, or the residue of earlier flatten tests). A REDUCE order
//! cannot clear them — against an empty exec position-map `plan_reduce` sees no opposing exposure and
//! OPENS a hedge — so this proves the first-class `close_all` path instead: a dedicated
//! `CtraderReconClient` fetches the venue's RAW open positions (WITH their `position_id`s, via
//! `open_positions()`), `CtraderExec::close_all` issues one `ProtoOAClosePositionReq` per position,
//! and a follow-up reconcile confirms the book reaches ZERO. It logs the before/after counts and
//! leaves the account FLAT.
//!
//! This DOES place close orders on the demo account (it flattens whatever is open), unlike the
//! read-only `ctrader_reconcile_smoke`. It is safe: it only closes positions that already exist and
//! never opens anything. Run it to clear a stacked demo book.
//!
//! Credentials come from the workspace's gitignored `.env` (`CTRADER_CLIENT_ID`/`_SECRET` +
//! `CTRADER_DEMO_ACCESS_TOKEN`/`_REFRESH_TOKEN`/optional `_ACCOUNT_ID` — see `config::CtraderConfig`),
//! loaded via `load_workspace_dotenv_from` + `CtraderConfig::from_vars`. Absent creds -> the test SKIPS (the live gate), never fails
//! CI (it is also `#[ignore]`d, so it never even runs there).

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_bridge_core::credentials::Environment;
use vike_ctrader::CtraderReconClient;
use vike_ctrader::config::CtraderConfig;
use vike_ctrader::conn::connect_and_auth_exec;
use vike_ctrader::exec::CtraderExec;
use vike_exec::event_channel;

use common::NoopSink;

const SYMBOL: &str = "EURUSD";

#[test]
#[ignore = "network + REAL cTrader demo endpoint + CTRADER_DEMO creds — run manually (see module doc)"]
fn ctrader_close_all_clears_the_book() {
    vike_log::test_init();
    // Tests own the `.env` I/O (`from_env` was deleted with the settings-registry conversion):
    // load the workspace map here, then the same pure `from_vars` gate as production.
    let vars = vike_bridge_core::credentials::load_workspace_dotenv_from(
        std::env::var("VIKE_SETTINGS_DIR").ok().as_deref(),
    );
    let Some(config) = CtraderConfig::from_vars(Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_ctrader::smoke", "SKIP: CTRADER_DEMO creds absent");
        return;
    };

    // A dedicated recon socket to fetch the RAW open positions (with their position_ids).
    let recon = CtraderReconClient::connect(&config.to_conn_config(), SYMBOL)
        .expect("open a dedicated reconcile connection against the real cTrader demo endpoint");

    let before = recon.open_positions().expect("fetch open positions (before)");
    tracing::info!(target: "vike_ctrader::smoke", "BEFORE: {} open {SYMBOL} position(s)", before.len());
    for p in &before {
        tracing::info!(
            target: "vike_ctrader::smoke",
            "  position_id={} side={} volume={}", p.position_id, p.side, p.volume
        );
    }
    if before.is_empty() {
        tracing::info!(target: "vike_ctrader::smoke", "already flat — nothing to clear");
        return;
    }

    // The exec actor owns the order/close command path + the ingest event lane. Keep `_rx` bound so
    // the actor's synthesized fill events have a live receiver (we verify flatness via reconcile, so
    // we never need to read them — but a dropped receiver would just log "core ingest gone").
    let (events, _rx) = event_channel(256);
    let handle = connect_and_auth_exec(config.to_conn_config(), Arc::new(NoopSink), events.clone())
        .expect("connect + auth exec against the real cTrader demo endpoint");
    let exec = CtraderExec::new(handle, events);

    // Close EVERY pre-existing position by id (side-agnostic — longs close SELL, shorts close BUY).
    let legs: Vec<(i64, i64)> = before.iter().map(|p| (p.position_id, p.volume)).collect();
    let coids = exec.close_all(&legs);
    tracing::info!(target: "vike_ctrader::smoke", "issued {} close order(s)", coids.len());
    assert_eq!(coids.len(), before.len(), "one close order per open position");

    // Poll the venue (via reconcile) until the book is flat — the close round-trips take a moment.
    let mut remaining = before.len();
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        remaining = recon.open_positions().expect("fetch open positions (poll)").len();
        if remaining == 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    tracing::info!(
        target: "vike_ctrader::smoke",
        "AFTER: {remaining} open {SYMBOL} position(s) (was {})", before.len()
    );
    assert_eq!(remaining, 0, "close_all must clear every {SYMBOL} position; {remaining} remain");
    tracing::info!(
        target: "vike_ctrader::smoke",
        "close_all cleared {} → 0 {SYMBOL} position(s) — account left FLAT", before.len()
    );

    drop(exec); // deterministic teardown (ActorHandle::drop joins the actor thread)
}
