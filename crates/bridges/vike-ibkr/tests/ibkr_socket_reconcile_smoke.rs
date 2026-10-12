//! LIVE read-only smoke for the SOCKET `ReconClient`
//! (`vike_ibkr::socket_recon::IbkrSocketReconClient`, decision 0140) — proves the real client
//! connects on its own API client id, pins the conId, and fetches AND parses order, fill, position
//! and balance reports against a running IB Gateway logged into the PAPER account, not just the
//! constructed rows `tests/ibkr_socket_reconcile_parse.rs` exercises offline. The owner runs it
//! before `hybrid` is ever considered for ibkr:
//!
//!     cargo test -p vike-ibkr --features ibkr --test ibkr_socket_reconcile_smoke -- --ignored --nocapture
//!
//! READ-ONLY: places NO order and never touches the account. Triple-gated: `#[ignore]`;
//! self-skips unless an `IBKR_DEMO_*` config on the `socket` backend exists and the gateway
//! answers; and a hard assert refuses anything but a paper `DU…` account (every read is filtered to
//! that account, so a live gateway's rows are never reported here).
//!
//! It also PRINTS the executions this connection can see per placing client id
//! (`IbkrSocketReconClient::executions_by_client_id`). Run it after the exec client has a fill
//! today (e.g. a marketable paper order): if the exec client id is absent from the map, IB shows a
//! non-master client id only its own executions, and the reconcile client needs the Gateway's
//! Master API client id (the module doc of `src/socket_recon.rs`, question 1). A flat account with no
//! fill today shows an empty map and settles nothing.
#![cfg(feature = "ibkr")]

use vike_exec::recon::ReconClient;
use vike_ibkr::config::load_ibkr_config_from;
use vike_ibkr::socket_recon::IbkrSocketReconClient;
use vike_model::IbkrBackend;
use vike_secrets::credentials::Environment;
use vike_secrets::credentials::load_workspace_secrets_at;

const SYMBOL: &str = "AAPL.SMART.USD";

#[test]
#[ignore = "network + a running IB Gateway logged into a paper DU… account — run manually (see module doc)"]
fn ibkr_socket_reconcile_fetch_smoke() {
    vike_log::test_init();

    let settings_dir = std::env::var("VIKE_SETTINGS_DIR").ok();
    let Some(cfg) = load_ibkr_config_from(
        Environment::Demo,
        &load_workspace_secrets_at(settings_dir.as_deref()),
        &ibkr_settings(settings_dir.as_deref()),
    ) else {
        eprintln!("skip: no IBKR_DEMO_* config in the settings database");
        return;
    };
    if cfg.backend == IbkrBackend::Cpapi {
        eprintln!("skip: venue.ibkr.demo.backend is cpapi; this smoke reads the IB Gateway");
        return;
    }
    // HARD SAFETY: paper only — this assert stays a hard failure, never a skip.
    assert!(
        cfg.account.starts_with("DU"),
        "smoke must target a paper (DU\u{2026}) account, got {}",
        cfg.account
    );
    eprintln!(
        "gateway {}:{} exec client id {} recon client id {}",
        cfg.host, cfg.port, cfg.client_id, cfg.recon_client_id
    );

    let client = match IbkrSocketReconClient::connect(&cfg, SYMBOL) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skip: IbkrSocketReconClient::connect failed (gateway/conId): {e}");
            return;
        }
    };

    let orders = client.fetch_order_status_reports(0).expect("fetch_order_status_reports");
    eprintln!("open orders on {SYMBOL}: {}", orders.len());
    for o in &orders {
        assert_eq!((o.venue.as_str(), o.symbol.as_str()), ("ibkr", SYMBOL));
        assert!(o.qty.is_finite() && o.qty >= 0.0, "implausible order qty: {}", o.qty);
        assert!(o.side == 1 || o.side == -1, "side must be ±1: {}", o.side);
        eprintln!(
            "  order {} {:?} {:?} qty={} filled={}",
            o.venue_order_id, o.status, o.order_type, o.qty, o.filled_qty
        );
    }

    let fills = client.fetch_fill_reports(0).expect("fetch_fill_reports");
    eprintln!("today's fills on {SYMBOL} by exec client {}: {}", cfg.client_id, fills.len());
    for f in &fills {
        assert_eq!((f.venue.as_str(), f.symbol.as_str()), ("ibkr", SYMBOL));
        assert!(f.last_qty.is_finite() && f.last_qty > 0.0, "fill qty must be > 0: {}", f.last_qty);
        assert!(f.side == 1 || f.side == -1, "side must be ±1: {}", f.side);
    }
    let seen = client.executions_by_client_id().expect("executions_by_client_id");
    eprintln!(
        "executions visible to recon client id {} today, per placing client id: {seen:?} \
         (exec client id {} {})",
        cfg.recon_client_id,
        cfg.client_id,
        if seen.contains_key(&cfg.client_id) { "IS visible" } else { "is NOT in the map" }
    );

    let positions = client.fetch_position_status_reports().expect("fetch_position_status_reports");
    assert!(!positions.is_empty(), "the socket recon must echo {SYMBOL} even when flat");
    for p in &positions {
        assert_eq!((p.venue.as_str(), p.symbol.as_str()), ("ibkr", SYMBOL));
        assert!(p.qty.is_finite() && p.avg_px.is_finite(), "implausible position: {p:?}");
    }
    eprintln!("position: qty={} avg_px={}", positions[0].qty, positions[0].avg_px);

    // The BALANCE seed `quarantine` writes: the `$LEDGER:ALL` `CashBalance` in USD (the BASE block
    // only when it declares USD as the base), else no seed.
    let balance = client.fetch_balance().expect("fetch_balance");
    eprintln!("USD cash balance: {balance:?}");
    if let Some(b) = balance {
        assert!(b.is_finite(), "implausible balance: {b}");
    }

    eprintln!(
        "IBKR socket reconcile fetch smoke green: orders + fills + positions + balance fetched & \
         parsed live (account untouched)"
    );
}

/// The venue's gateway rows (`venue.ibkr.<tier>.*`, decision 0095) from the settings database the
/// credential read above resolves. LOUD on an unreadable database, never a quiet default: the
/// built-in gateway would make this smoke skip having tested nothing.
fn ibkr_settings(
    settings_dir_override: Option<&str>,
) -> vike_secrets::venue_setting::VenueSettings {
    vike_model::paths::state_path::project_settings_dir_from(
        settings_dir_override,
        &std::env::current_dir().expect("a working directory"),
    )
    .map(|dir| {
        vike_secrets::venue_setting::load_venue_settings(&dir)
            .unwrap_or_else(|e| panic!("the settings database could not be read: {e}"))
    })
    .and_then(|mut all| all.remove("ibkr"))
    .unwrap_or_default()
}
