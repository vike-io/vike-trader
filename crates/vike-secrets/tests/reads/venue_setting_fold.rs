//! **The credential-map fold is RETIRED (decision 0095, the settings-in-SQLite plan's Task 7):
//! these tests pin that a `venue_setting` row reaches no credential map.**
//!
//! Ruling 10 moved ten config-shaped names out of the `credential` table into `venue_setting`, and
//! until Task 7 the store rendered every row back into the credential map under its LEGACY name
//! (`IBKR_DEMO_PORT`, `POLY_RATE_GATE`, …), because every reader still looked that name up. The
//! readers take the rows as `vike_secrets::venue_setting::VenueSettings` now, so the fold went —
//! and a credential row still carrying a field's legacy name is read by nothing.
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`a_venue_setting_row_never_reaches_the_credential_map`] | the whole-map front door still folding — a credential-map reader and a `VenueSettings` reader answering one setting two ways |
//! | [`a_venue_setting_row_never_reaches_a_scoped_map`] | the SCOPED front door, which did not delegate to the whole-map one and had its own fold, still folding |
//! | [`the_hub_key_table_never_gains_a_venue_name`] | `docs/decisions/0051`'s namespace split, undone |
//!
//! ⚠ Values are obviously fake. No real credential exists anywhere near this file.

use vike_secrets::venue_setting::SettingTier;
use vike_secrets::{KeyScope, Lookup};

use crate::support::{self, Fixture};

/// A `venue_setting` row is read through `VenueSettings` alone; the credential map the store answers
/// with carries none of the legacy names the row was once rendered under.
///
/// ⚠ The rows are planted through `set_venue_setting_in_unjournalled` — the writer `vike-cli config set venue.*`
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
    let resolved = crate::support::store_in(fx.dir()).read_all().expect("the store opens");
    for name in [concat!("POLY", "_RATE_GATE"), concat!("IBKR", "_DEMO_PORT")] {
        assert!(
            !resolved.secrets.keys().any(|k| k == name),
            "{name} was folded into the credential map"
        );
    }
    let all = vike_secrets::venue_setting::load_venue_settings(fx.dir()).expect("the rows read");
    assert_eq!(all["polymarket"].get(SettingTier::Any, "rate_gate"), Some("1"));
    assert_eq!(
        all["ibkr"].get(SettingTier::Tier(vike_model::AccountTier::Demo), "port"),
        Some("4002")
    );
}

/// The SCOPED front door, which never delegated to the whole-map one and carried a fold of its own:
/// a declared name that only a `venue_setting` row answers for is ABSENT from the store's answer,
/// while a declared credential beside it is present.
#[test]
fn a_venue_setting_row_never_reaches_a_scoped_map() {
    let fx = seeded(&[("POLY_PRIVATE_KEY", "not-a-real-key")]);
    plant(&fx, "polymarket", None, "PROXY_HOST", "an-example-proxy-host");
    plant(&fx, "ibkr", Some("demo"), "PORT", "4102");

    let scoped = crate::support::store_from(fx.arg())
        .read(&KeyScope::of(["POLY_PROXY_HOST", "IBKR_DEMO_PORT", "POLY_PRIVATE_KEY"]))
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
    assert_eq!(
        all["ibkr"].get(SettingTier::Tier(vike_model::AccountTier::Demo), "port"),
        Some("4102")
    );
}

/// ⚠ **`docs/decisions/0051`: a hub key and a venue credential are different NAMESPACES.** No
/// venue setting reaches the node table either.
#[test]
fn the_hub_key_table_never_gains_a_venue_name() {
    let fx = seeded(&[("IBKR_DEMO_ACCOUNT", "DU0000000")]);
    plant(&fx, "ibkr", Some("demo"), "PORT", "4102");

    let node = crate::support::store_in(fx.dir()).hub_keys(|_| true).expect("the store must open");
    let names: Vec<&str> = node.secrets.keys().collect();
    assert_eq!(names, vec![NODE_KEY], "the node namespace holds its own key and no other");
}

// ---------------------------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------------------------

/// The one hub key this fixture plants, so `docs/decisions/0051`'s namespace has a row in it.
const NODE_KEY: &str = "VIKE_TRADEHUB_OBSERVE_KEY";

/// A store holding exactly `credentials`, BY NAME (deployment rows), plus one hub key so the node
/// table is not empty (the namespace test needs something on the other side of the split). Every
/// assertion here is about what the store answers once a row is planted, never about which account
/// owns it.
fn seeded(credentials: &[(&str, &str)]) -> Fixture {
    Fixture::seeded_with(
        credentials.iter().copied().chain([(NODE_KEY, "node-value")]),
        support::hub_keys_in(&[NODE_KEY]),
    )
}

/// Plant one `venue_setting` row through the public venue-setting writer.
fn plant(fx: &Fixture, venue: &str, tier: Option<&str>, field: &str, value: &str) {
    let tier = tier.map(|t| t.parse::<vike_model::AccountTier>().expect("a tier"));
    vike_secrets::set_venue_setting_in_unjournalled(fx.dir(), venue, tier, field, value)
        .unwrap_or_else(|e| panic!("planting ({venue}, {tier:?}, {field}) failed: {e}"));
}
