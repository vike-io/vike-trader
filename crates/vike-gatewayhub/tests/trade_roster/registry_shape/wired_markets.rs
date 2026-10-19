//! The build filter's pins: `ArmingTable::wired_markets` over THIS daemon's registry mounts the
//! vike-model table's engine rows whose registry row mounts in this build, in table order. The
//! table's own rows and orders are pinned in `crates/vike-venues/src/venue_wiring_tests.rs`;
//! what only the daemon can pin — which feature build drops which row, and which bridge takes the
//! recon trigger — is pinned here.

use crate::local::wired_markets;
use vike_gatewayhub::registry::trade::REGISTRY;
use vike_gatewayhub::trade::adapters::registry::VenueRow;
use vike_venues::venue_wiring::{VENUE_WIRING, Wiring, wiring_for};

fn registry_row(venue: &str) -> &'static VenueRow {
    REGISTRY
        .iter()
        .find(|r| r.venue() == venue)
        .unwrap_or_else(|| panic!("{venue} has no registry row"))
}

/// This build mounts exactly the table's engine rows whose registry row is a `Mount`, in table
/// (MOUNT) order — and a feature-gated row is present iff its feature is on: the `#[cfg(feature)]`
/// the old per-venue rows carried, now read off the registry. The CI `tradehub-all` job runs this
/// with all three features on.
#[test]
fn the_build_mounts_the_tables_engine_rows_its_registry_mounts() {
    let mounted: Vec<&str> = wired_markets(REGISTRY).iter().map(|m| m.venue).collect();
    let want: Vec<&str> = VENUE_WIRING
        .iter()
        .filter(|(v, w)| w.engine().is_some() && matches!(registry_row(v), VenueRow::Adapter(_)))
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

/// A `LiveExecRecon` venue mounts in EVERY build: its registry row is never `FeatureAbsent`.
#[test]
fn every_live_exec_recon_venue_mounts_in_every_build() {
    for &(venue, w) in VENUE_WIRING {
        if matches!(w, Wiring::LiveExecRecon { .. }) {
            assert!(
                matches!(registry_row(venue), VenueRow::Adapter(_)),
                "{venue} is LiveExecRecon in vike-model but its registry row is FeatureAbsent \
                 here: it is FeatureGated"
            );
        }
    }
}

/// The converse: a registry row that is `FeatureAbsent` in this build belongs to a
/// `FeatureGated` venue (a `LiveExecRecon` one would silently drop out of this build). The one
/// pair of facts the layers keep apart (the model cannot see the registry), held here.
#[test]
fn a_feature_absent_registry_row_is_a_feature_gated_venue() {
    for row in REGISTRY {
        if let VenueRow::FeatureAbsent { venue, .. } = row {
            assert!(
                matches!(wiring_for(venue), Some(Wiring::FeatureGated { .. })),
                "{venue} is registered FeatureAbsent but vike-model does not call it FeatureGated"
            );
        }
    }
}

/// The venues whose bridge takes the recon trigger (its `VenueDeclaration`'s
/// `takes_recon_trigger`); every other mounted venue reconciles on the interval only. The pin the
/// old per-venue table's trigger column carried, read off the one declaration that always decided it
/// (`crates/vike-gatewayhub/src/trade/adapters/contract.rs` passes the trigger only to a bridge that takes it).
#[test]
fn exactly_four_venues_take_the_recon_trigger() {
    let mut triggered: Vec<&str> = wired_markets(REGISTRY)
        .iter()
        .filter(|m| match registry_row(m.venue) {
            VenueRow::Adapter(mount) => mount.declaration().takes_recon_trigger,
            VenueRow::FeatureAbsent { .. } => false,
        })
        .map(|m| m.venue)
        .collect();
    triggered.sort_unstable();
    assert_eq!(triggered, ["binance", "bybit", "hyperliquid", "okx"]);
}

/// The xEMM v1 pair against the REAL mounted set — the half of the xEMM validation tests that
/// asserted the daemon's wiring rather than the rule (the rule's own tests plant a table). The v1
/// pair validates to the 6.5 bp round-trip fee bit for bit, and a hedge symbol okx is not wired
/// for is refused by name. (Moved from the old wired-market table's own tests.)
#[test]
fn the_xemm_v1_pair_is_wired_in_the_real_table() {
    let v1 = || {
        vike_tradehub_core::XemmConfig::crypto(
            "hyperliquid",
            "BTC",
            "okx",
            "BTC-USDT-SWAP",
            0.01,
            0.0005,
            0.5,
        )
    };
    let mounted = wired_markets(REGISTRY);
    let fee =
        v1().validate(Some(mounted.as_slice())).expect("the v1 pair is wired and fee-expressible");
    assert_eq!(fee.to_bits(), (1.5_f64 / 10_000.0 + 5.0 / 10_000.0).to_bits());

    let mut cfg = v1();
    cfg.hedge_symbol = "ETH-USDT-SWAP".into();
    assert_eq!(
        cfg.validate(Some(mounted.as_slice())),
        Err(vike_tradehub_core::XemmConfigError::HedgeSymbolNotAccepted {
            venue: "okx".into(),
            wired: "BTC-USDT-SWAP".into(),
            requested: "ETH-USDT-SWAP".into(),
        })
    );
}
