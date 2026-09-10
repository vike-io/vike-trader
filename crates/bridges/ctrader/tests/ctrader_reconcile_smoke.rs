//! LIVE demo smoke for the cTrader `ReconClient` (`vike_ctrader::CtraderReconClient`) — proves the
//! real client fetches AND parses order/position/fill reports against the live cTrader demo endpoint
//! (`demo.ctraderapi.com:5035`, full two-stage OAuth handshake + symbol discovery over its OWN
//! dedicated authed socket), not just the scripted prost bodies `tests/offline/recon_client_parse.rs` and
//! `tests/recon_client_revive.rs` exercise offline (no network).
//!
//!     cargo test -p vike-ctrader --test ctrader_reconcile_smoke -- --ignored --nocapture
//!
//! READ-ONLY: places NO order. cTrader's `ProtoOAReconcileRes` reports the CURRENTLY-pending order
//! and open-position set, so a flat demo account is a perfectly valid observation — an empty order
//! report and a single synthesized flat position row (cTrader omits a closed position entirely; the
//! client synthesizes a zero row so a stale local position stays detectable — see
//! `recon_client.rs`'s module doc). No resting order is required to observe reconcile state, so per
//! the task's "place NO order if avoidable" guidance none is placed and the account is never
//! touched — it stays exactly as flat (or not) as it began. The non-empty order/fill parse paths are
//! already exhaustively covered offline against scripted prost rows.
//!
//! Credentials come from the workspace's gitignored `.env` (`CTRADER_CLIENT_ID`/`_SECRET` +
//! `CTRADER_DEMO_ACCESS_TOKEN`/`_REFRESH_TOKEN`/optional `_ACCOUNT_ID` — see
//! `config::CtraderConfig`), loaded via `load_workspace_dotenv_from` + `CtraderConfig::from_vars`. Absent creds -> the test SKIPS
//! (the live gate), never fails CI (it is also `#[ignore]`d, so it never even runs there).

use std::time::Duration;

use vike_bridge_core::credentials::Environment;
use vike_ctrader::config::CtraderConfig;
use vike_ctrader::recon_client::CtraderReconClient;
use vike_exec::recon::ReconClient;
use vike_model::now_ms;

const SYMBOL: &str = "EURUSD";

#[test]
#[ignore = "network + REAL cTrader demo endpoint + CTRADER_DEMO creds — run manually (see module doc)"]
fn ctrader_reconcile_fetch_smoke() {
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

    // The real dedicated recon socket: same handshake the exec/data actor runs, its OWN connection
    // (a reconcile fetch never contends the exec side's socket — see `recon_client.rs`'s module doc).
    let client = CtraderReconClient::connect(&config.to_conn_config(), SYMBOL).expect(
        "connect dedicated recon socket + handshake against the real cTrader demo endpoint",
    );

    // Orders: the `ProtoOAReconcileRes` pending set, filtered to SYMBOL. May legitimately be empty
    // on a flat account — assert only that every row that IS present is well-formed and ours.
    let orders = client.fetch_order_status_reports(0).expect("fetch_order_status_reports");
    tracing::info!(target: "vike_ctrader::smoke", "reconcile open orders: {}", orders.len());
    for o in &orders {
        assert_eq!(o.venue, "ctrader");
        assert_eq!(o.symbol, SYMBOL);
        assert!(o.qty.is_finite() && o.qty >= 0.0, "implausible order qty: {}", o.qty);
        assert!(o.side == 1 || o.side == -1, "side must be ±1: {}", o.side);
        assert!(o.avg_px.is_finite() && o.avg_px >= 0.0, "implausible avg_px: {}", o.avg_px);
    }

    // Positions: cTrader omits a CLOSED position, so a flat account yields the client's synthesized
    // zero row for SYMBOL — the report is therefore NEVER empty (load-bearing: a stale local
    // position stays detectable). Pins the trait dispatch + the synthesize-flat contract live.
    let positions = client.fetch_position_status_reports().expect("fetch_position_status_reports");
    assert!(
        !positions.is_empty(),
        "ctrader recon must echo {SYMBOL} even when flat (synthesized zero row)"
    );
    for p in &positions {
        assert_eq!(p.venue, "ctrader");
        assert_eq!(p.symbol, SYMBOL);
        assert!(p.qty.is_finite(), "implausible position qty: {}", p.qty);
        assert!(p.avg_px.is_finite() && p.avg_px >= 0.0, "implausible avg_px: {}", p.avg_px);
    }
    tracing::info!(
        target: "vike_ctrader::smoke",
        "position: qty={} avg_px={}", positions[0].qty, positions[0].avg_px
    );

    // Fills: historical deals over a bounded lookback. Best-effort — the brief requires only the
    // order/position reports read-only, and cTrader's `ProtoOADealListReq` window semantics vary by
    // account/history depth; an error here is logged (a genuine finding for the operator running the
    // smoke), never a hard failure of the order/position seam this test primarily proves. Every row
    // that DOES come back must be well-formed and ours.
    let since = now_ms() - Duration::from_secs(7 * 24 * 3600).as_millis() as i64;
    match client.fetch_fill_reports(since) {
        Ok(fills) => {
            tracing::info!(target: "vike_ctrader::smoke", "reconcile recent fills: {}", fills.len());
            for f in &fills {
                assert_eq!(f.venue, "ctrader");
                assert_eq!(f.symbol, SYMBOL);
                assert!(
                    f.last_qty.is_finite() && f.last_qty > 0.0,
                    "fill qty must be > 0: {}",
                    f.last_qty
                );
                assert!(f.side == 1 || f.side == -1, "side must be ±1: {}", f.side);
            }
        }
        Err(e) => tracing::warn!(
            target: "vike_ctrader::smoke",
            error = %e, "fill-report fetch failed (DealList window/history caveat) — order/position seam still proven"
        ),
    }

    tracing::info!(
        target: "vike_ctrader::smoke",
        "ctrader reconcile fetch smoke green: orders + positions fetched & parsed live (account left FLAT)"
    );
}
