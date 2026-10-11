//! LIVE demo smoke for cTrader's own live-mount seam (decision 0088, step B5), the LEAVE-FLAT twin
//! of `ctrader_live_mount_smoke.rs`: proves `vike_ctrader::mount::live_mount_for_account` end to end
//! through a round trip that can never fill — a far-from-market GTC limit order, confirmed resting
//! via the mount's own `ReconClient`, then cancelled and confirmed gone — rather than a real fill.
//!
//! Written so this one, unlike its sibling, satisfies the `weekly-order-lifecycle` lane's own rule
//! (`docs/ops/live-smoke-lane.md`): every smoke that lane admits rests an order NO PRICE MOVE could
//! ever fill, then cancels it. `ctrader_live_mount_smoke.rs` places a real min-volume market BUY
//! (which fills) and then flattens the resulting position, so it stays hand-run and named in that
//! doc's exclusion table; this file is the leave-flat alternative for the SAME mount seam, mirroring
//! the shape of `binance_reconcile_order_lifecycle_smoke`,
//! `oanda_demo_place_and_cancel_far_from_market_limit` and `bybit_exec_client_resting_modify_cancel`.
//!
//! ⚠ **Moved here from vike-mount's own tests directory (this same filename) before it ever landed
//! on `main`** — it was opened as PR #2225 the same session B5 (this file's own sibling,
//! `ctrader_live_mount_smoke.rs`) split cTrader's mount seam into this crate, so rewriting it against
//! `vike_ctrader::mount::live_mount_for_account` here rather than merging it against
//! `vike_mount::make_engine` first and moving it again a moment later was the cheaper path. See the
//! sibling file's own module doc for the fuller reasoning (the twin move already argued once) and
//! for what this move DROPS (composition-root-only coverage: the account row's tier and
//! `vike_mount::require_live_risk_budget`, both still covered by `vike-mount`'s own unit tests).
//!
//!     cargo test -p vike-ctrader --test ctrader_leave_flat_smoke -- --ignored --nocapture
//!
//! Credentials, the mount call and the resolution shape are copied from `ctrader_live_mount_smoke.rs`
//! verbatim — see that file's module doc for why each piece is shaped the way it is (the OAuth
//! cred-load gate, `live_mount_for_account`'s signature, the `HaltAdmit` value). The only difference
//! is the order this test places and how it proves the account is untouched.
//!
//! Weekend caveat, same as the sibling file: the FX demo is closed Fri 22:00 – Sun 22:00 GMT.
//! Whether a RESTING limit order (as opposed to a market order needing to fill) is even acceptable
//! to place while the underlying market is closed is not independently known for cTrader's demo
//! simulator, so — like the sibling file's market BUY — a non-ACCEPTED terminal for the placement
//! is treated as a SOFT SKIP (re-run during FX hours), never a hard failure. The cancel, by
//! contrast, is a HARD requirement once the order is confirmed resting: an order left resting on a
//! demo account is not "flat", and the account must not be left holding it.

use std::time::Duration;

use vike_bridge_core::credentials::{Environment, load_workspace_secrets_at};
use vike_ctrader::config::CtraderConfig;
use vike_ctrader::mount::live_mount_for_account;
use vike_exec::Ingest;
use vike_exec::event_channel;
use vike_exec::recon::ReconClient;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{HaltAdmit, OrderRequest, OrderStatusReport, now_ms};

const SYMBOL: &str = "EURUSD";
/// Same venue-minimum EURUSD order volume in UNITS as the sibling file — see
/// `vike_ctrader::symbols::VolumeGrid`.
const MIN_EURUSD_UNITS: f64 = 1000.0;
/// Nowhere near any real EUR/USD rate (oanda's own leave-flat smoke uses the same value for the
/// same reason) — the order must rest, never fill, whatever the real market does.
const FAR_FROM_MARKET_PRICE: f64 = 0.10;
/// Bounded wait for each terminal (accept/reject, then cancel-confirmed).
const TERMINAL_TIMEOUT: Duration = Duration::from_secs(20);

fn far_from_market_limit(coid: &str, side: i32) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "ctrader".into(),
        symbol: SYMBOL.into(),
        side,
        qty: MIN_EURUSD_UNITS,
        order_type: "limit".into(),
        price: Some(FAR_FROM_MARKET_PRICE),
        ts: now_ms(),
        ..Default::default()
    }
}

/// Terminal outcome of the order PLACEMENT, drained off the ingest lane.
enum PlaceOutcome {
    Accepted,
    Rejected(String),
    TimedOut,
}

fn await_placed(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, coid: &str) -> PlaceOutcome {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    rt.block_on(async {
        let deadline = tokio::time::Instant::now() + TERMINAL_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(remaining, rx.recv()).await {
                Err(_) => return PlaceOutcome::TimedOut,
                Ok(None) => return PlaceOutcome::TimedOut,
                Ok(Some(Ingest::Event(Event::OrderAccepted(a)))) if a.client_order_id == coid => {
                    return PlaceOutcome::Accepted;
                }
                Ok(Some(Ingest::Event(Event::OrderRejected(r)))) if r.client_order_id == coid => {
                    return PlaceOutcome::Rejected(r.reason.to_string());
                }
                Ok(Some(_)) => {} // keep draining until our coid's terminal
            }
        }
    })
}

/// Block until an `OrderCanceled` for `coid` arrives, or the timeout elapses. Unlike placement,
/// there is no "market closed" excuse for a cancel of an order this mount itself just rested — a
/// timeout here is a hard failure, asserted by the caller.
fn await_canceled(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, coid: &str) -> bool {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    rt.block_on(async {
        let deadline = tokio::time::Instant::now() + TERMINAL_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(remaining, rx.recv()).await {
                Err(_) => return false,
                Ok(None) => return false,
                Ok(Some(Ingest::Event(Event::OrderCanceled(c)))) if c.client_order_id == coid => {
                    return true;
                }
                Ok(Some(_)) => {}
            }
        }
    })
}

/// Re-fetch the mount's reconcile order-status reports until `coid`'s presence in the resting set
/// matches `want_resting`, or a bounded number of attempts elapse. Returns the LAST fetched list;
/// the caller makes the hard assertion on it, so a genuinely-stuck state still fails loudly. Mirrors
/// `ctrader_live_mount_smoke.rs`'s own settle-window shape.
fn fetch_until(recon: &dyn ReconClient, coid: &str, want_resting: bool) -> Vec<OrderStatusReport> {
    let mut orders = Vec::new();
    for attempt in 0..4 {
        orders = recon.fetch_order_status_reports(0).expect("fetch_order_status_reports");
        let resting = orders.iter().any(|o| o.client_order_id.as_deref() == Some(coid));
        if resting == want_resting {
            break;
        }
        if attempt < 3 {
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    orders
}

#[test]
#[ignore = "network + REAL cTrader demo endpoint + CTRADER_DEMO creds — run manually (see module doc)"]
fn ctrader_mount_leave_flat_round_trip() {
    vike_log::test_init();

    let vars = load_workspace_secrets_at(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    if CtraderConfig::from_vars(Environment::Demo, &vars).is_none() {
        tracing::warn!(target: "vike_ctrader::smoke", "SKIP: CTRADER_DEMO creds absent (live gate)");
        return;
    }

    let (tx, mut rx) = event_channel(256);

    // THE unit under test: `vike_ctrader::mount::live_mount_for_account`, exactly as the sibling
    // live-fill smoke calls it — no `MountPolicy`, no risk-budget refusal, this function answers
    // only "can cTrader mount live".
    let mut mount = live_mount_for_account(
        &AccountLabel::Default,
        &vars,
        // No rotation home: this test process declares no project, which is exactly what the
        // bridge's own read of the boot's state directory answered here before the venue mount
        // contract.
        None,
        SYMBOL,
        true,
        &tx,
        HaltAdmit::default(),
        // A sentinel this smoke owns and never creates: it places real demo orders and must not
        // depend on a kill switch lying around on the box running it.
        std::path::Path::new("ctrader-mount-smoke-never-engaged/HALT"),
    )
    .expect("creds present ⇒ exec connect/auth handshake must succeed");

    let recon =
        mount.recon.expect("creds present ⇒ dedicated ReconClient built on its own authed socket");

    // --- PLACE: a far-from-market GTC limit that can never fill -----------------------------------
    let coid = format!("vtrctraderflat{}", now_ms() % 100_000_000);
    mount.client.submit(&far_from_market_limit(&coid, 1));
    match await_placed(&mut rx, &coid) {
        PlaceOutcome::Accepted => {
            tracing::info!(target: "vike_ctrader::smoke", "order {coid} ACCEPTED")
        }
        PlaceOutcome::Rejected(reason) => {
            tracing::warn!(
                target: "vike_ctrader::smoke",
                %reason,
                "SOFT SKIP: order REJECTED (FX demo may be closed, or may refuse a new resting \
                 order out of session — re-run during FX market hours to get a hard verdict). \
                 Nothing rests ⇒ account untouched."
            );
            return;
        }
        PlaceOutcome::TimedOut => {
            tracing::warn!(
                target: "vike_ctrader::smoke",
                "SOFT SKIP: order never reached a terminal accept/reject within {TERMINAL_TIMEOUT:?} \
                 (FX demo likely closed — re-run during FX market hours). Nothing confirmed resting \
                 ⇒ treated as untouched, but see the log above for a stray order to check by hand."
            );
            return;
        }
    }

    // --- CONFIRM RESTING via the mount's OWN reconcile handle -------------------------------------
    let resting_after_accept = fetch_until(recon.as_ref(), &coid, true);
    assert!(
        resting_after_accept.iter().any(|o| o.client_order_id.as_deref() == Some(coid.as_str())),
        "order {coid} must be resting in the mount's own order-status reports after acceptance"
    );
    tracing::info!(target: "vike_ctrader::smoke", "order {coid} confirmed resting");

    // --- CANCEL: a HARD requirement from here on — this mount rested the order, so it must remove it
    mount.client.cancel(&coid);
    let canceled = await_canceled(&mut rx, &coid);
    assert!(
        canceled,
        "CANCEL FAILED (timed out) for a confirmed-resting order — {coid} may still be OPEN on the \
         cTrader demo account; cancel it manually"
    );
    tracing::info!(target: "vike_ctrader::smoke", "cancel {coid} confirmed via ingest lane");

    // --- CONFIRM GONE via the mount's OWN reconcile handle -----------------------------------------
    let resting_after_cancel = fetch_until(recon.as_ref(), &coid, false);
    assert!(
        !resting_after_cancel.iter().any(|o| o.client_order_id.as_deref() == Some(coid.as_str())),
        "expected {coid} to be gone from the mount's order-status reports after cancel, but it is \
         still resting — the cTrader demo account may hold a leftover order; cancel it manually"
    );

    tracing::info!(
        target: "vike_ctrader::smoke",
        "ctrader mount leave-flat smoke GREEN: live_mount_for_account round-trip rested a \
         far-from-market limit and cancelled it, confirmed via the mount's own recon handle — \
         account untouched"
    );
}
