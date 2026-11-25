//! The registry's shape: every roster venue exactly once, every feature-off row named after its
//! feature, and every contract row paper and offline over an empty store.

use std::assert_matches;
use vike_bridge_core::venue_adapter::{ArmingResolution, VenueAdapterExecutionOutcome};
use vike_bridge_core::venue_adapter_fixture::AdapterFixture;
use vike_gatewayhub::registry::trade::REGISTRY;
use vike_gatewayhub::trade::adapters::registry::VenueAdapterRow;
use vike_model::AccountTier;

#[test]
fn the_registry_partitions_the_roster() {
    let mut seen: Vec<&str> = REGISTRY.iter().map(VenueAdapterRow::venue).collect();
    let mut roster = vike_model::VENUES.to_vec();
    seen.sort_unstable();
    roster.sort_unstable();
    assert_eq!(seen, roster, "one row per vike_model::VENUES id, in EVERY feature combination");
}

/// The table the roster members take cannot drift from the one the daemon serves: the same venues in the same order,
/// each row an adapter there, and `FeatureAbsent` here for exactly the three optional venues, or the members would be
/// proving another table's answers.
#[test]
fn the_absent_table_is_the_daemons_but_for_the_three_optional_venues() {
    use crate::optional_venues_absent::REGISTRY as ABSENT;
    let venues = |t: &[VenueAdapterRow]| t.iter().map(VenueAdapterRow::venue).collect::<Vec<_>>();
    assert_eq!(venues(ABSENT), venues(REGISTRY), "the same venues, in the same order");
    for (here, served) in ABSENT.iter().zip(REGISTRY) {
        assert!(
            matches!(served, VenueAdapterRow::Adapter(_)),
            "the served table's `{}` row is no adapter: a test build has lost a feature",
            served.venue()
        );
        assert_eq!(
            matches!(here, VenueAdapterRow::FeatureAbsent { .. }),
            ["fxcm", "polymarket", "ibkr"].contains(&here.venue()),
            "`{}`: only the three optional venues are absent here",
            here.venue()
        );
    }
}

/// A feature-off row names a feature this crate declares, spelled after its venue.
#[test]
fn every_feature_absent_row_names_one_of_this_crates_features() {
    for row in REGISTRY {
        if let VenueAdapterRow::FeatureAbsent { venue, feature } = row {
            assert!(["ibkr", "polymarket", "fxcm"].contains(feature), "{venue}: {feature}?");
            assert_eq!(venue, feature, "each optional bridge's feature is named after its venue");
        }
    }
}

/// THE PAPER HALF OF "PROBE AND ARM AGREE", for every contract row: with no credentials, `resolve`
/// says paper and `start` builds nothing — offline, at both ceilings.
#[test]
fn with_no_credentials_every_contract_row_resolves_and_starts_paper() {
    let fx = AdapterFixture::new(&[]);
    let (tx, _rx) = vike_execution::event_channel(8);
    for row in REGISTRY {
        let VenueAdapterRow::Adapter(m) = row else { continue };
        for live in [AccountTier::Demo, AccountTier::Live] {
            let r = m.resolve(&fx.inputs(live));
            assert_matches!(
                r,
                ArmingResolution::NotRunning(_),
                "{}: an empty store resolved {r:?}",
                m.venue()
            );
            let out = m.start(fx.request(live, "BTCUSDT", &tx));
            assert!(
                matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning),
                "{}: mount disagrees",
                m.venue()
            );
            assert!(
                out.reconcile.is_none(),
                "{}: a paper mount with no keys reconciles nothing",
                m.venue()
            );
        }
    }
}
