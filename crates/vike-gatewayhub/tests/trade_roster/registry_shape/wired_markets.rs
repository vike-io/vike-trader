//! The build filter's pins: `ArmingTable::wired_markets` over THIS daemon's registry mounts the
//! vike-model table's engine rows whose registry row mounts in this build, in table order. The
//! table's own rows and orders are pinned in `crates/vike-venues/src/venue_wiring_tests.rs`;
//! what only the daemon can pin — which feature build drops which row, and which mounted venue takes
//! the recon trigger — is pinned here.

use crate::local::wired_markets;
use vike_gatewayhub::registry::trade::REGISTRY;
use vike_gatewayhub::trade::adapters::registry::VenueAdapterRow;
use vike_venues::venue_wiring::{VENUE_WIRING, VenueWiring, wiring_for};

fn registry_row(venue: &str) -> &'static VenueAdapterRow {
    REGISTRY
        .iter()
        .find(|r| r.venue() == venue)
        .unwrap_or_else(|| panic!("{venue} has no registry row"))
}

/// This build mounts exactly the table's engine rows whose registry row is a `Mount`, in table
/// (MOUNT) order — and a feature-gated row is present iff its feature is on: the `#[cfg(feature)]`
/// the old per-venue rows carried, now read off the registry. Every test build runs this
/// with all three features on.
#[test]
fn the_build_mounts_the_tables_engine_rows_its_registry_mounts() {
    let mounted: Vec<&str> = wired_markets(REGISTRY).iter().map(|m| m.venue).collect();
    let want: Vec<&str> = VENUE_WIRING
        .iter()
        .filter(|(v, w)| {
            w.engine().is_some() && matches!(registry_row(v), VenueAdapterRow::Adapter(_))
        })
        .map(|(v, _)| *v)
        .collect();
    assert_eq!(mounted, want);
    for (venue, on) in [
        ("ibkr", cfg!(feature = "ibkr")),
        ("polymarket", cfg!(feature = "polymarket")),
        ("fxcm", cfg!(feature = "fxcm")),
    ] {
        assert_eq!(mounted.contains(&venue), on, "{venue}: mounted iff its feature is on");
    }
}

/// A `LiveExecutionReconcile` venue mounts in EVERY build: its registry row is never `FeatureAbsent`.
#[test]
fn every_live_execution_reconcile_venue_mounts_in_every_build() {
    for &(venue, w) in VENUE_WIRING {
        if matches!(w, VenueWiring::LiveExecutionReconcile { .. }) {
            assert!(
                matches!(registry_row(venue), VenueAdapterRow::Adapter(_)),
                "{venue} is LiveExecRecon in vike-model but its registry row is FeatureAbsent \
                 here: it is FeatureGated"
            );
        }
    }
}

/// The converse: a registry row that is `FeatureAbsent` in this build belongs to a
/// `FeatureGated` venue (a `LiveExecutionReconcile` one would silently drop out of this build). The one
/// pair of facts the layers keep apart (the model cannot see the registry), held here.
#[test]
fn a_feature_absent_registry_row_is_a_feature_gated_venue() {
    for row in REGISTRY {
        if let VenueAdapterRow::FeatureAbsent { venue, .. } = row {
            assert!(
                matches!(wiring_for(venue), Some(VenueWiring::FeatureGated { .. })),
                "{venue} is registered FeatureAbsent but vike-model does not call it FeatureGated"
            );
        }
    }
}

/// The mounted venues that take the recon trigger (their connection row's `nudges_reconcile`,
/// `vike_venues::venue_connection`, which `crates/vike-gatewayhub/src/trade/adapters/contract.rs`'s `contract_parts`
/// hands the trigger by); every other mounted venue reconciles on the interval only. The pin the old per-venue
/// table's trigger column carried, and then each bridge's declaration flag.
#[test]
fn exactly_four_venues_take_the_reconcile_trigger() {
    use vike_venues::venue_connection::venue_wide_exec_for;
    let mut triggered: Vec<&str> = wired_markets(REGISTRY)
        .iter()
        .filter(|m| matches!(registry_row(m.venue), VenueAdapterRow::Adapter(_)))
        .filter(|m| venue_wide_exec_for(m.venue).nudges_reconcile)
        .map(|m| m.venue)
        .collect();
    triggered.sort_unstable();
    assert_eq!(triggered, ["binance", "bybit", "hyperliquid", "okx"]);
}

/// The xEMM v1 pair against the REAL mounted set — the half of the xEMM validation tests that
/// asserted the daemon's wiring rather than the rule (the rule's own tests plant a table). The taker's
/// symbol is its default account's row (`BTCUSDT.P`, decision 0290's user name, stated on an okx row).
/// The v1 pair validates to the 6.5 bp round-trip fee bit for bit, and a hedge symbol okx's account is
/// not mounted on is refused by name. (Moved from the old wired-market table's own tests.)
#[test]
fn the_xemm_v1_pair_is_wired_in_the_real_table() {
    // The maker is hyperliquid's perpetual by its USER name (decision 0290): `BTC` would read as a spot pair's fee lane.
    let mount =
        |hedge: &str| vike_tradehub_core::xemm_mount("hyperliquid", "BTCUSDC.P", "okx", hedge, 0.5);
    let params = vike_tradehub_core::xemm_params(0.01, 0.0005);
    let mounted = wired_markets(REGISTRY);
    let accounts = vike_secrets::account_directory::AccountDirectory::from_rows(
        vike_secrets::AccountRows::Known(vec![vike_secrets::account_row(
            1,
            "okx",
            vike_model::AccountTier::Demo,
            None,
            true,
        )]),
        None,
    );
    let validate = |hedge: &str| {
        vike_tradehub_core::validate_xemm_mount(
            &mount(hedge),
            &params,
            Some((mounted.as_slice(), &accounts)),
        )
    };
    let fee = validate("BTCUSDT.P").expect("the v1 pair is wired and fee-expressible");
    assert_eq!(fee.to_bits(), (1.5_f64 / 10_000.0 + 5.0 / 10_000.0).to_bits());

    assert_eq!(
        validate("ETHUSDT.P"),
        Err(vike_tradehub_core::XemmConfigError::HedgeSymbolNotAccepted {
            venue: "okx".into(),
            wired: "BTCUSDT.P".into(),
            requested: "ETHUSDT.P".into(),
        })
    );
}
