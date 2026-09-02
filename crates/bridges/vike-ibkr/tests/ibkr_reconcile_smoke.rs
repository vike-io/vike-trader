//! LIVE read-only smoke for the IBKR `ReconClient` (`vike_ibkr::IbkrReconClient`) — proves the real
//! client fetches AND parses order/fill/position/balance reports against a running, browser-
//! authenticated Client Portal Gateway (the cpapi backend), not just the synthetic bodies
//! `tests/ibkr_reconcile_parse.rs` exercises offline (no network).
//!
//!     cargo test -p vike-ibkr --features ibkr --test ibkr_reconcile_smoke -- --ignored --nocapture
//!
//! READ-ONLY: places NO order and never touches the account — safe to run any time the gateway is
//! up. Triple-gated exactly like `tests/ibkr_cpapi_smoke.rs`: `#[ignore]` (never in the default
//! run), self-skips unless an `IBKR_DEMO_*` config exists AND the gateway is reachable, AND a
//! read-only `GET /iserver/accounts` session-account guard refuses to proceed unless the gateway is
//! logged into a paper `DU…` account (cpapi routes by the authenticated SESSION, not `cfg.account`).
//! It self-skips cleanly in CI and on any box without a running Gateway, so it never fails there.
#![cfg(feature = "ibkr-cpapi")]

use vike_bridge_core::credentials::Environment;
use vike_exec::recon::ReconClient;
use vike_ibkr::config::load_ibkr_config_from;
use vike_ibkr::load_workspace_dotenv_from;
use vike_ibkr::transport::CpapiRest;
use vike_ibkr::IbkrReconClient;

const SYMBOL: &str = "AAPL.SMART.USD";

#[test]
#[ignore = "network + a running, authenticated CP Gateway for a paper DU… account — run manually (see module doc)"]
fn ibkr_reconcile_fetch_smoke() {
    vike_log::test_init();

    let Some(cfg) = load_ibkr_config_from(
        Environment::Demo,
        &load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref()),
    ) else {
        eprintln!("skip: no IBKR_DEMO_* config in .env");
        return;
    };

    // HARD SAFETY: refuse anything but a paper account by the .env string first…
    assert!(
        cfg.account.starts_with("DU"),
        "smoke must target a paper (DU\u{2026}) account, got {}",
        cfg.account
    );
    // …then verify the gateway's authenticated SESSION account is a paper DU… (cpapi routes by the
    // session, not cfg.account). Unreachable/unauth → skip; a live U… session → refuse (hard fail).
    let rest = CpapiRest::new(&cfg.cpapi_url, &cfg.account);
    match rest.accounts() {
        Ok(v) => {
            let session_acct = v
                .get("accounts")
                .and_then(|a| a.as_array())
                .and_then(|a| a.first())
                .and_then(|x| x.as_str())
                .unwrap_or("");
            assert!(
                session_acct.starts_with("DU"),
                "cpapi gateway SESSION is on '{session_acct}', not a paper DU\u{2026} account — refusing to fetch"
            );
            eprintln!("session-account guard OK: gateway session is {session_acct}");
        }
        Err(e) => {
            eprintln!(
                "skip: could not read gateway /iserver/accounts (unreachable/unauth?): {e:?}"
            );
            return;
        }
    }

    let client = match IbkrReconClient::connect(&cfg, SYMBOL) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skip: IbkrReconClient::connect failed (gateway/conId): {e}");
            return;
        }
    };

    // Orders: may legitimately be empty on a flat account — assert only that every row present is
    // well-formed and ours.
    let orders = client.fetch_order_status_reports(0).expect("fetch_order_status_reports");
    eprintln!("reconcile open orders: {}", orders.len());
    for o in &orders {
        assert_eq!(o.venue, "ibkr");
        assert_eq!(o.symbol, SYMBOL);
        assert!(o.qty.is_finite() && o.qty >= 0.0, "implausible order qty: {}", o.qty);
        assert!(o.side == 1 || o.side == -1, "side must be ±1: {}", o.side);
        assert!(o.avg_px.is_finite() && o.avg_px >= 0.0, "implausible avg_px: {}", o.avg_px);
    }

    // Fills: the cpapi /trades recent window, filtered to SYMBOL. Best-effort — an empty flat-account
    // window is valid; every row that IS present must be well-formed and ours.
    let fills = client.fetch_fill_reports(0).expect("fetch_fill_reports");
    eprintln!("reconcile recent fills: {}", fills.len());
    for f in &fills {
        assert_eq!(f.venue, "ibkr");
        assert_eq!(f.symbol, SYMBOL);
        assert!(f.last_qty.is_finite() && f.last_qty > 0.0, "fill qty must be > 0: {}", f.last_qty);
        assert!(f.side == 1 || f.side == -1, "side must be ±1: {}", f.side);
    }

    // Positions: cpapi OMITS a closed position, so a flat account yields the client's synthesized
    // zero row for SYMBOL — the report is therefore NEVER empty (load-bearing for stale-position
    // detection). Pins the trait dispatch + synthesize-flat contract live.
    let positions = client.fetch_position_status_reports().expect("fetch_position_status_reports");
    assert!(
        !positions.is_empty(),
        "IBKR recon must echo {SYMBOL} even when flat (synthesized zero row)"
    );
    for p in &positions {
        assert_eq!(p.venue, "ibkr");
        assert_eq!(p.symbol, SYMBOL);
        assert!(p.qty.is_finite(), "implausible position qty: {}", p.qty);
        assert!(p.avg_px.is_finite() && p.avg_px >= 0.0, "implausible avg_px: {}", p.avg_px);
    }
    eprintln!("position: qty={} avg_px={}", positions[0].qty, positions[0].avg_px);

    // Balance: the account ledger's USD cashbalance. A funded paper account is non-negative; an
    // unfunded/empty ledger may report None — accept both, reject garbage.
    let balance = client.fetch_balance().expect("fetch_balance");
    eprintln!("USD cash balance: {balance:?}");
    if let Some(b) = balance {
        assert!(b.is_finite(), "implausible balance: {b}");
    }

    eprintln!("IBKR reconcile fetch smoke green: orders + fills + positions + balance fetched & parsed live (account untouched)");
}
