//! **The credential-map fold is RETIRED (decision 0095, the settings-in-SQLite plan's Task 7):
//! these tests pin that a `venue_setting` row reaches no credential map.**
//!
//! Ruling 10 moved ten config-shaped names out of the `credential` table into `venue_setting`, and
//! until Task 7 the store rendered every row back into the credential map under its LEGACY name
//! (`IBKR_DEMO_PORT`, `POLY_RATE_GATE`, …), because every reader still looked that name up. The
//! readers take the rows as `vike_secrets::venue_setting::VenueSettings` now, so the fold went —
//! and a credential row still carrying a field's legacy name is refused at boot
//! (`vike_config::refuse_stranded_venue_settings`) rather than read by nothing.
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`a_venue_setting_row_never_reaches_the_credential_map`] | the whole-map front door still folding — a credential-map reader and a `VenueSettings` reader answering one setting two ways |
//! | [`a_venue_setting_row_never_reaches_a_scoped_map`] | the SCOPED front door, which did not delegate to the whole-map one and had its own fold, still folding |
//! | [`the_node_key_table_never_gains_a_venue_name`] | `docs/decisions/0051`'s namespace split, undone |
//! | [`a_box_with_no_database_is_byte_identical`] | a file-store box resolving anything but its file |
//!
//! ⚠ Values are obviously fake. No real credential exists anywhere near this file.

use vike_secrets::venue_setting::SettingTier;
use vike_secrets::{KeyScope, Lookup, Table};

use crate::support::{self, Fixture, Rule};

/// A `venue_setting` row is read through `VenueSettings` alone; the credential map the store answers
/// with carries none of the legacy names the row was once rendered under.
///
/// ⚠ The rows are planted through `set_venue_setting_in` — the writer `vike-cli config set venue.*`
/// uses — and NOT through `plant_settings_rows`, whose whole-table writer deliberately leaves
/// `venue_setting` untouched: a fixture planted that way holds no row, and this test would then pass
/// (or fail on the read-back below) whatever the store's front door did.
#[test]
fn a_venue_setting_row_never_reaches_the_credential_map() {
    let fx = Fixture::empty();
    vike_secrets::create_empty_store_for_test(&fx.db()).expect("a fresh settings store");
    for (venue, tier, field, value) in
        [("polymarket", None, "RATE_GATE", "1"), ("ibkr", Some("demo"), "PORT", "4002")]
    {
        plant(&fx, venue, tier, field, value);
    }
    let resolved = vike_secrets::resolve_store_in(fx.dir(), vike_secrets::Table::Credential)
        .expect("the store opens");
    for name in [concat!("POLY", "_RATE_GATE"), concat!("IBKR", "_DEMO_PORT")] {
        assert!(
            !resolved.secrets.keys().any(|k| k == name),
            "{name} was folded into the credential map"
        );
    }
    let all = vike_secrets::venue_setting::load_venue_settings(fx.dir()).expect("the rows read");
    assert_eq!(all["polymarket"].get(SettingTier::Any, "rate_gate"), Some("1"));
    assert_eq!(all["ibkr"].get(SettingTier::Demo, "port"), Some("4002"));
}

/// The SCOPED front door, which never delegated to the whole-map one and carried a fold of its own:
/// a declared name that only a `venue_setting` row answers for is ABSENT from the store's answer,
/// while a declared credential beside it is present.
#[test]
fn a_venue_setting_row_never_reaches_a_scoped_map() {
    let fx = migrated(&[("POLY_PRIVATE_KEY", "not-a-real-key")]);
    plant(&fx, "polymarket", None, "PROXY_HOST", "an-example-proxy-host");
    plant(&fx, "ibkr", Some("demo"), "PORT", "4102");

    let scoped = vike_secrets::resolve_project_scoped(
        fx.arg(),
        &KeyScope::of(["POLY_PROXY_HOST", "IBKR_DEMO_PORT", "POLY_PRIVATE_KEY"]),
    )
    .expect("the store must open");
    for name in ["POLY_PROXY_HOST", "IBKR_DEMO_PORT"] {
        assert_eq!(
            scoped.get(name),
            Lookup::AbsentFromStore,
            "{name} was folded into a scoped credential map"
        );
    }
    // The control: the scoped read still answers the credential it was asked for, so the absences
    // above are the fold being gone rather than the read having failed.
    assert_eq!(scoped.get("POLY_PRIVATE_KEY"), Lookup::Present("not-a-real-key"));
    // …and the rows are there, through the one reader that reads them.
    let all = vike_secrets::venue_setting::load_venue_settings(fx.dir()).expect("the rows read");
    assert_eq!(
        all["polymarket"].get(SettingTier::Any, "proxy_host"),
        Some("an-example-proxy-host")
    );
    assert_eq!(all["ibkr"].get(SettingTier::Demo, "port"), Some("4102"));
}

/// ⚠ **`docs/decisions/0051`: a node key and a venue credential are different NAMESPACES.** No
/// venue setting reaches the node table either.
#[test]
fn the_node_key_table_never_gains_a_venue_name() {
    let fx = migrated(&[("IBKR_DEMO_ACCOUNT", "DU0000000")]);
    plant(&fx, "ibkr", Some("demo"), "PORT", "4102");

    let node =
        vike_secrets::resolve_store_in(fx.dir(), Table::NodeKey).expect("the store must open");
    let names: Vec<&str> = node.secrets.keys().collect();
    assert_eq!(names, vec![NODE_KEY], "the node namespace holds its own key and no other");
}

/// A box that has not migrated has no `venue_setting` table, and resolves exactly what its file
/// holds.
#[test]
fn a_box_with_no_database_is_byte_identical() {
    let fx = files_only(&[("IBKR_DEMO_ACCOUNT", "DU0000000")]);
    let resolved =
        vike_secrets::resolve_store_in(fx.dir(), Table::Credential).expect("the store must open");
    assert_eq!(
        resolved.secrets.keys().collect::<Vec<_>>(),
        vec!["IBKR_DEMO_ACCOUNT"],
        "an unmigrated box resolves exactly what its file holds"
    );
}

// ---------------------------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------------------------

/// The one node key this fixture plants, so `docs/decisions/0051`'s namespace has a row in it.
const NODE_KEY: &str = "VIKE_TRADEHUB_OBSERVE_KEY";

/// A settings directory with a credential file holding exactly `credentials` and NO migration — a
/// box still on `Backend::Files`.
fn files_only(credentials: &[(&str, &str)]) -> Fixture {
    Fixture::with_store_text(&support::hand_edited_store_text(credentials.iter().copied()))
}

/// The same, migrated into a database, plus a node file so the node table is not empty (the
/// namespace test needs something on the other side of the split).
///
/// The classifier is a deliberately SIMPLER one than the production one, for the reason
/// `crates/vike-secrets/tests/migration/database/mod.rs`'s own `classify` gives: this crate cannot link
/// the crate that owns the real tables, and every assertion here is about what the store answers
/// once a row is planted rather than about how a name was classified on the way in.
fn migrated(credentials: &[(&str, &str)]) -> Fixture {
    let fx = files_only(credentials);
    fx.write_node_text(&support::hand_edited_store_text([(NODE_KEY, "node-value")]));
    fx.migrate_with(
        support::node_keys_in(&[NODE_KEY]),
        &support::classifier([
            Rule::prefix("IBKR_DEMO_").account("ibkr", "demo"),
            Rule::prefix("POLY_").account("polymarket", "live"),
        ]),
    );
    fx
}

/// Plant one `venue_setting` row — the state a completed `secrets move-venue-config` leaves.
fn plant(fx: &Fixture, venue: &str, tier: Option<&str>, field: &str, value: &str) {
    vike_secrets::set_venue_setting_in(fx.dir(), venue, tier, field, value)
        .unwrap_or_else(|e| panic!("planting ({venue}, {tier:?}, {field}) failed: {e}"));
}
