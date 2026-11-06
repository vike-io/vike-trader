//! Unit tests of the per-venue forms, the attribution tail and the field classifier.
//!
//! ⚠ No venue-prefixed NAME is spelled here: a `src/` test region is swept by
//! `crates/vike-ops/tests/settings_secrets/settings_registry/directions.rs`'s
//! `every_read_variable_is_declared`, which would demand a `SETTINGS` row for it. Field words
//! (`API_KEY`, `LOGIN`) carry no venue prefix, so they are not that shape. The deployment names the
//! attribution tail writes are COMPOSED (`attribution_var_for`), never spelled.

use super::*;
use vike_venues::credential_schema::schema_for;

fn hints(venue: &str, place: FormPlace) -> Vec<String> {
    form_fields(venue, place).iter().map(|f| f.hint().to_string()).collect()
}

/// **The paper tier has no form for any venue**: the paper simulator reads no credential, so a
/// form there would be a control that writes nothing anything reads.
#[test]
fn the_paper_tier_offers_no_form_anywhere() {
    for &venue in vike_model::VENUES {
        assert!(form_fields(venue, FormPlace::Tier(AccountTier::Paper)).is_empty(), "{venue}");
    }
}

/// **The DEMO form is the schema row's account form, field for field, label for label.** Folded
/// over the roster rather than spot-checked: the schema is the one statement of what each loader
/// reads, so the form cannot drift from it without this reddening.
#[test]
fn a_tier_form_is_the_schema_rows_account_form() {
    for &venue in vike_model::VENUES {
        let schema = schema_for(venue).expect("every roster venue has a schema row");
        let want: Vec<(&str, &str)> =
            schema.form(FieldOwner::Account).iter().map(|f| (f.label, f.field)).collect();
        let got: Vec<(&str, &str)> = form_fields(venue, FormPlace::Tier(AccountTier::Demo))
            .iter()
            .map(|f| match &f.target {
                FieldTarget::Account(k) => (f.label, k.field),
                other => panic!("{venue}: a DEMO form field that is not the account's: {other:?}"),
            })
            .collect();
        assert_eq!(got, want, "{venue}");
    }
}

/// Dukascopy's form is ONE account's login pair — its two demo accounts are two labelled rows
/// (decision 0156 item 6), so the form no longer carries two groups — and no `SERVER` field: the
/// JForex server is the `venue.dukascopy.demo.server` setting.
#[test]
fn the_dukascopy_form_is_one_accounts_login_pair() {
    assert_eq!(hints("dukascopy", FormPlace::Tier(AccountTier::Demo)), vec!["LOGIN", "PASSWORD"]);
    assert!(
        !hints("dukascopy", FormPlace::Tier(AccountTier::Demo))
            .iter()
            .any(|h| h.ends_with("SERVER")),
        "the JForex server is a venue setting, not a credential this form writes"
    );
}

/// **The DEMO form says where the JForex server is set, and the condition on its value**, for
/// dukascopy only.
#[test]
fn the_demo_form_names_the_server_setting_and_its_condition() {
    let note = form_note("dukascopy", FormPlace::Tier(AccountTier::Demo))
        .expect("the DEMO form names the server setting");
    assert!(
        note.contains("vike-cli config set venue.dukascopy.demo.server"),
        "the note must name the verb and the setting: {note}"
    );
    assert!(note.contains(".jnlp"), "the note must name the condition itself: {note}");
    assert!(
        note.contains("No live smoke exercises a stored JNLP URL"),
        "the note keeps saying what is unproven about the value: {note}"
    );
    assert!(form_note("dukascopy", FormPlace::Tier(AccountTier::Live)).is_none());
    assert!(form_note("binance", FormPlace::Tier(AccountTier::Live)).is_none(), "no other venue");
}

/// **The VENUE-WIDE attribution tail lands on LIVE, on the mechanised venues only, and it is a
/// DEPLOYMENT row** — written by name, never against the account row.
#[test]
fn the_live_form_carries_the_venue_wide_attribution_tail() {
    use vike_venues::attribution::attribution_for;
    for &venue in vike_model::VENUES {
        let live = form_fields(venue, FormPlace::Tier(AccountTier::Live));
        let tail: Vec<String> = live
            .iter()
            .filter_map(|f| match &f.target {
                FieldTarget::Deployment(name) => Some(name.clone()),
                _ => None,
            })
            .collect();
        if attribution_for(venue).is_none() {
            assert!(tail.is_empty(), "{venue} has no mechanic and must grow no tail: {tail:?}");
            continue;
        }
        let want = credential_keys::attribution_var_for(venue).expect("a mechanised venue");
        assert_eq!(tail.first(), Some(&want), "{venue}");
        assert!(tail.iter().all(|n| is_venue_wide_key(n)), "{venue}: {tail:?}");
        // …and on LIVE ONLY: one name, every account, so a DEMO field would be an operator
        // changing what a REAL order is tagged with from a demo account's form.
        assert!(
            form_fields(venue, FormPlace::Tier(AccountTier::Demo))
                .iter()
                .all(|f| !matches!(f.target, FieldTarget::Deployment(_))),
            "{venue}/demo must carry no venue-wide field"
        );
    }
    let aster = hints("aster", FormPlace::Tier(AccountTier::Live));
    assert!(aster.contains(&"ASTER_BUILDER_FEE_RATE".to_string()), "{aster:?}");
    let hl = hints("hyperliquid", FormPlace::Tier(AccountTier::Live));
    assert!(hl.contains(&"HYPERLIQUID_BUILDER_FEE_TENTHS_BP".to_string()), "{hl:?}");
    assert!(
        hints("binance", FormPlace::Tier(AccountTier::Live))
            .iter()
            .all(|k| !k.contains("_BUILDER_FEE")),
        "a CEX venue takes no builder fee"
    );
}

/// **The venue application group is the schema's `FieldOwner::Venue` form**: cTrader's OAuth client
/// pair, and nothing for every venue that has none.
#[test]
fn the_venue_application_form_is_the_schemas_venue_form() {
    assert_eq!(hints("ctrader", FormPlace::VenueApp), vec!["CLIENT_ID", "CLIENT_SECRET"]);
    assert!(
        !hints("ctrader", FormPlace::Tier(AccountTier::Demo))
            .iter()
            .any(|h| h.starts_with("CLIENT_")),
        "the application pair is the venue's, shown once — never a field of a tier's form"
    );
    for &venue in vike_model::VENUES {
        if venue != "ctrader" {
            assert!(form_fields(venue, FormPlace::VenueApp).is_empty(), "{venue}");
        }
    }
}

/// **Sensitivity is the schema's `secret`, and it FAILS CLOSED** — a field no schema row declares,
/// and any field of a venue the schema does not carry, is a secret.
#[test]
fn the_field_classifier_reads_the_schema_and_fails_closed() {
    for (venue, field, want) in [
        ("binance", "API_KEY", Sensitivity::Secret),
        ("binance", "API_SECRET", Sensitivity::Secret),
        ("dukascopy", "PASSWORD", Sensitivity::Secret),
        ("polymarket", "PRIVATE_KEY", Sensitivity::Secret),
        ("polymarket", "RELAYER_API_KEY", Sensitivity::Secret),
        ("ctrader", "ACCESS_TOKEN", Sensitivity::Secret),
        ("ctrader", "CLIENT_SECRET", Sensitivity::Secret),
        // …and the readable half: a login, an account, an address, the app's client id.
        ("dukascopy", "LOGIN", Sensitivity::Public),
        ("ibkr", "ACCOUNT", Sensitivity::Public),
        ("alpaca", "CLIENT_ID", Sensitivity::Public),
        ("ctrader", "CLIENT_ID", Sensitivity::Public),
        ("hyperliquid", "ACCOUNT_ADDRESS", Sensitivity::Public),
        ("polymarket", "RELAYER_API_KEY_ADDRESS", Sensitivity::Public),
    ] {
        assert_eq!(field_sensitivity(venue, field), want, "{venue} {field}");
    }
    for (venue, field) in [("binance", "SOMETHING_NEW"), ("not-a-venue", "LOGIN"), ("ibkr", "")] {
        assert_eq!(
            field_sensitivity(venue, field),
            Sensitivity::Secret,
            "{venue} {field:?} must fail CLOSED — an unclassified field may not be rendered"
        );
    }
}

/// A form field's own sensitivity agrees with the classifier, and the attribution tail is public
/// (an affiliate tag stamped on outgoing orders in the clear).
#[test]
fn a_form_fields_sensitivity_is_the_classifiers() {
    for &venue in vike_model::VENUES {
        for place in [FormPlace::Tier(AccountTier::Live), FormPlace::VenueApp] {
            for f in form_fields(venue, place) {
                let want = match &f.target {
                    FieldTarget::Account(k) | FieldTarget::Venue(k) => {
                        field_sensitivity(venue, k.field)
                    }
                    FieldTarget::Deployment(_) => Sensitivity::Public,
                };
                assert_eq!(f.sensitivity(), want, "{venue} {}", f.hint());
            }
        }
    }
}
