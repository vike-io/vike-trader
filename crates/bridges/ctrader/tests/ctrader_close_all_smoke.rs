//! LIVE demo smoke: clear a PRE-EXISTING stacked position book via `ExecutionClientCtrader::close_all`.
//!     cargo test -p vike-ctrader --test integration -- --ignored --nocapture ctrader_close_all_smoke::
//!
//! The demo account can accumulate positions a fresh connect never tracked (e.g. hedged pairs a
//! prior session's opposite orders opened, or the residue of earlier flatten tests). A REDUCE order
//! cannot clear them — against an empty exec position-map `plan_reduce` sees no opposing exposure and
//! OPENS a hedge — so this proves the first-class `close_all` path instead: a dedicated
//! `ReconcileClientCtrader` fetches the venue's RAW open positions (WITH their `position_id`s, via
//! `open_positions()`), `ExecutionClientCtrader::close_all` issues one `ProtoOAClosePositionReq` per position,
//! and a follow-up reconcile confirms the book reaches ZERO. It logs the before/after counts and
//! leaves the account FLAT.
//!
//! This DOES place close orders on the demo account (it flattens whatever is open), unlike the
//! read-only `ctrader_reconcile_smoke`. It is safe: it only closes positions that already exist and
//! never opens anything. Run it to clear a stacked demo book.
//!
//! Credentials are the DEFAULT demo account's — its `ACCESS_TOKEN`/`REFRESH_TOKEN`/optional
//! `ACCOUNT_ID` and the venue's `CLIENT_ID`/`CLIENT_SECRET` (see `config::CtraderConfig`) — resolved
//! through `vike_secrets::smoke_directory` + `CtraderConfig::from_credentials`. Absent creds -> the
//! test SKIPS (the live gate), never fails CI (it is also `#[ignore]`d, so it never even runs there).

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_ctrader::ReconcileClientCtrader;
use vike_ctrader::config::CtraderConfig;
use vike_ctrader::execution::ExecutionClientCtrader;
use vike_ctrader::transport::connection::connect_and_auth_execution;
use vike_execution::event_channel;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;

use vike_ctrader::test_support::NoopSink;

const SYMBOL: &str = "EURUSD";

#[test]
#[ignore = "network + REAL cTrader demo endpoint + a demo account's creds — run manually (see module doc)"]
fn ctrader_close_all_clears_the_book() {
    vike_log::test_init();
    // Tests own the store I/O (`from_env` was deleted with the settings-registry conversion):
    // resolve the account here, then the same pure `from_credentials` gate as production.
    let directory = vike_secrets::smoke_directory();
    let credentials =
        directory.credentials_for("ctrader", &AccountLabel::Default, AccountTier::Demo);
    let endpoints = vike_ctrader::test_support::demo_endpoints();
    let Some(config) = CtraderConfig::from_credentials(&endpoints, &credentials) else {
        tracing::warn!(target: "vike_ctrader::smoke", "SKIP: demo account creds absent");
        return;
    };

    // A dedicated recon socket to fetch the RAW open positions (with their position_ids).
    let recon = ReconcileClientCtrader::connect(&config.to_connection_config(), SYMBOL)
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
    let handle = connect_and_auth_execution(
        config.to_connection_config(),
        Arc::new(NoopSink),
        events.clone(),
    )
    .expect("connect + auth exec against the real cTrader demo endpoint");
    let exec = ExecutionClientCtrader::new(handle, events);

    // Close EVERY pre-existing position by id (side-agnostic — longs close SELL, shorts close BUY).
    let legs: Vec<(i64, i64)> = before.iter().map(|p| (p.position_id, p.volume)).collect();
    let order_client_ids = exec.close_all(&legs);
    tracing::info!(target: "vike_ctrader::smoke", "issued {} close order(s)", order_client_ids.len());
    assert_eq!(order_client_ids.len(), before.len(), "one close order per open position");

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
