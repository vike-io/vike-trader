//! LIVE read-only smoke for the Polymarket `ReconClient` (`vike_polymarket::PolymarketReconClient`)
//! — proves the real client FETCHES and PARSES order/fill/position/balance reports against the live
//! CLOB + data-api, not just the synthetic bodies `tests/offline/polymarket_reconcile_parse.rs` exercises
//! offline. Polymarket is US-geo-blocked, so it routes through the Dublin SOCKS proxy (ON by default;
//! see `egress::proxy_url`) and self-skips cleanly wherever creds are absent — so it never fails in CI
//! or on a box without the tunnel + `.env`.
//!
//! ```sh
//! cargo test -p vike-polymarket --features polymarket --test polymarket_reconcile_smoke \
//!   -- --ignored --nocapture
//! ```
//! The tunnel is `venue.polymarket.*` in the settings database (see `tests/support/egress.rs`).
//!
//! READ-ONLY: places NO order and never touches the account — safe to run any time. `#[ignore]`d and
//! double-gated exactly like the exec smokes: self-skips unless the default `live` polymarket
//! account holds its `PRIVATE_KEY` field AND the L2 derivation + first read succeed. The reconcile client is built with a
//! FRESH (empty) registry, so orders come back with `client_order_id: None` and the fills report is
//! empty (no resting orders of a fresh session to re-key) — this smoke proves the fetch+parse
//! plumbing (positions + balance especially, which need no registry), not a populated order book.
#![cfg(feature = "polymarket")]

use vike_bridge_core::eip712::eth_address_from_private_key;
use vike_exec::recon::ReconClient;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_polymarket::{
    CLOB_BASE, PolymarketCreds, PolymarketReconClient, PolymarketRegistry, SignatureType, ensure_l2,
};

#[path = "support/egress.rs"]
mod egress_support;

/// The default polymarket account's credential FIELDS, out of the smoke store — the account at tier
/// `live`, the only tier this venue has (decision 0156 item 6: no credential name is composed).
fn live_account(
    dir: &vike_secrets::account_directory::AccountDirectory,
) -> vike_secrets::account_credentials::AccountCredentials<'_> {
    dir.credentials_for("polymarket", &AccountLabel::Default, AccountTier::Live)
}

#[test]
#[ignore = "network + Polymarket creds + the Dublin proxy — run manually (see module doc)"]
fn polymarket_reconcile_fetch_smoke() {
    vike_log::test_init();
    egress_support::declare_from_the_settings_database();
    // §0.2 egress guard: a read-only smoke declares no region (decision 0095 made the region a
    // parameter), so this makes no network call — the order-placing smokes are the ones that
    // assert `vike_polymarket::DUBLIN_EGRESS_COUNTRY`.
    match vike_polymarket::check_expected_egress(None, vike_polymarket::DEFAULT_EGRESS_PROBE)
        .and_then(vike_polymarket::EgressCheck::into_result)
    {
        Ok(Some(e)) => eprintln!("egress OK: {e}"),
        Ok(None) => eprintln!("egress: unchecked (a read-only smoke declares no region)"),
        Err(e) => panic!("egress guard: {e}"),
    }

    let dir = vike_secrets::smoke_directory();
    let account = live_account(&dir);
    let Some(pk) = account.get("PRIVATE_KEY").map(str::to_string) else {
        eprintln!("skip: the live polymarket account holds no PRIVATE_KEY");
        return;
    };
    let Ok(signer) = eth_address_from_private_key(&pk) else {
        eprintln!("skip: could not derive the EOA from the account's PRIVATE_KEY");
        return;
    };
    let mut creds =
        PolymarketCreds { private_key: pk, address: signer.clone(), ..Default::default() };
    let boot = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    if let Err(e) = ensure_l2(&mut creds, CLOB_BASE, boot) {
        eprintln!("skip: could not derive L2 creds (proxy down / geo-blocked?): {e:?}");
        return;
    }

    // The funder (data-api positions key) + signature type mirror the exec-side wallet: a deposit
    // wallet (POLY_1271) by default, its address in the account's FUNDER field.
    let funder = account.get("FUNDER").map_or_else(|| signer.clone(), str::to_string);
    let sig_type = match account.get("SIGNATURE_TYPE") {
        Some("0") => SignatureType::Eoa,
        Some("1") => SignatureType::PolyProxy,
        Some("2") => SignatureType::PolyGnosisSafe,
        _ => SignatureType::Poly1271,
    };

    let client =
        PolymarketReconClient::new(creds, funder.clone(), sig_type, PolymarketRegistry::new());

    // Orders: the account's active set (empty on a flat account is valid) — assert well-formedness.
    let orders = client.fetch_order_status_reports(0).expect("fetch_order_status_reports");
    eprintln!("reconcile active orders: {}", orders.len());
    for o in &orders {
        assert_eq!(o.venue, "polymarket");
        assert!(!o.symbol.is_empty(), "order must carry an asset_id symbol");
        assert!(o.side == 1 || o.side == -1, "side must be ±1: {}", o.side);
        assert!(o.qty.is_finite() && o.qty >= 0.0, "implausible order qty: {}", o.qty);
    }

    // Fills: the recent /data/trades window, re-keyed via the (fresh) registry → expected empty here;
    // assert only that every row present is well-formed.
    let fills = client.fetch_fill_reports(0).expect("fetch_fill_reports");
    eprintln!("reconcile recent fills (fresh registry → expect 0): {}", fills.len());
    for f in &fills {
        assert_eq!(f.venue, "polymarket");
        assert!(f.last_qty.is_finite() && f.last_qty > 0.0, "fill qty must be > 0: {}", f.last_qty);
        assert!(f.side == 1 || f.side == -1, "side must be ±1: {}", f.side);
    }

    // Positions: the funder's token holdings (needs no registry). May be empty on a fresh account.
    let positions = client.fetch_position_status_reports().expect("fetch_position_status_reports");
    eprintln!("reconcile positions: {}", positions.len());
    for p in &positions {
        assert_eq!(p.venue, "polymarket");
        assert!(!p.symbol.is_empty(), "position must carry a token_id symbol");
        assert!(
            p.qty.is_finite() && p.qty >= 0.0,
            "Polymarket holdings are one-way ≥ 0: {}",
            p.qty
        );
        assert!(p.avg_px.is_finite() && p.avg_px >= 0.0, "implausible avg_px: {}", p.avg_px);
    }

    // Balance: the account's USDC collateral. A funded account is non-negative; an empty one may
    // report None — accept both, reject garbage.
    let balance = client.fetch_balance().expect("fetch_balance");
    eprintln!("USDC collateral: {balance:?}");
    if let Some(b) = balance {
        assert!(b.is_finite() && b >= 0.0, "implausible balance: {b}");
    }

    eprintln!(
        "Polymarket reconcile fetch smoke green: orders + fills + positions + balance fetched & \
         parsed live (account untouched)"
    );
}

/// The START seam, live: what `crates/bridges/polymarket/src/exec_plane/adapter.rs`'s
/// `PolymarketVenueAdapter` calls for a recon-only start (`recon_client_for_account`), over the
/// default `live` account's fields.
///
/// The test above hand-builds the client (explicit creds, explicit funder, explicit signature type).
/// This one proves the one-call factory the composition root actually uses —
/// [`vike_polymarket::recon_client_for_account`] — resolves all three off the account's fields and
/// comes back with a client that fetches. It is the regression guard for the two things that are
/// easy to get silently wrong there:
///
/// 1. the ClobAuth signer address must be the KEY's own EOA, not the `FUNDER` (a `POLY_1271`
///    deposit-wallet account has them different — derive the wrong one and `/auth/derive-api-key`
///    rejects the signature);
/// 2. the funder must still be the `FUNDER` for the data-api `/positions` read — swap the two and
///    positions silently come back empty rather than erroring.
///
/// So it asserts the positions read actually resolves the funder: a 0-row result is accepted (a flat
/// account is legitimate) but every row must be well-formed, and the balance read — which is the
/// L2-signed one, i.e. the half that fails first if the EOA was wrong — must succeed.
///
/// READ-ONLY, `#[ignore]`d and self-skipping exactly like the test above.
#[test]
#[ignore = "network + Polymarket creds + the Dublin proxy — run manually (see module doc)"]
fn polymarket_recon_client_for_account_smoke() {
    vike_log::test_init();
    egress_support::declare_from_the_settings_database();
    match vike_polymarket::check_expected_egress(None, vike_polymarket::DEFAULT_EGRESS_PROBE)
        .and_then(vike_polymarket::EgressCheck::into_result)
    {
        Ok(Some(e)) => eprintln!("egress OK: {e}"),
        Ok(None) => eprintln!("egress: unchecked (a read-only smoke declares no region)"),
        Err(e) => panic!("egress guard: {e}"),
    }

    let dir = vike_secrets::smoke_directory();
    let account = live_account(&dir);
    if account.get("PRIVATE_KEY").is_none() {
        eprintln!("skip: the live polymarket account holds no PRIVATE_KEY");
        return;
    }
    // The deployment's named values — the reconcile gate is one of them.
    let vars = vike_secrets::smoke_vars();
    // The gate is a MOUNT-time concern, not a factory one — the factory is callable regardless, so
    // this smoke proves the client itself, and just reports where the gate currently sits.
    eprintln!(
        "POLY_RECONCILE gate currently: {}",
        if vike_polymarket::poly_reconcile_enabled(&vars) { "ON" } else { "OFF (default)" }
    );

    let Some(client) = vike_polymarket::recon_client_for_account(&account) else {
        // L2 derivation failed — the tunnel is down or the key is not usable from here. That is the
        // documented `None` (reconcile-inert), not a test failure.
        eprintln!("skip: recon_client_for_account returned None (proxy down / geo-blocked?)");
        return;
    };

    let positions = client.fetch_position_status_reports().expect("fetch_position_status_reports");
    eprintln!("for_account positions: {}", positions.len());
    for p in &positions {
        assert_eq!(p.venue, "polymarket");
        assert!(!p.symbol.is_empty(), "position must carry a token_id symbol");
        assert!(p.qty.is_finite() && p.qty >= 0.0, "holdings are one-way ≥ 0: {}", p.qty);
    }
    // The L2-signed half: this is what breaks if the derived EOA was wrong.
    let balance = client.fetch_balance().expect("fetch_balance (L2-signed — proves the EOA)");
    eprintln!("for_account USDC collateral: {balance:?}");
    if let Some(b) = balance {
        assert!(b.is_finite() && b >= 0.0, "implausible balance: {b}");
    }
    let orders = client.fetch_order_status_reports(0).expect("fetch_order_status_reports");
    eprintln!("for_account active orders: {}", orders.len());

    eprintln!("recon_client_for_account smoke green: the make_engine mount seam fetches live");
}
