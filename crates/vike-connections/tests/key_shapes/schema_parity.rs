//! **The form writes exactly the fields the loader reads** — `vike_model::credential_schema`
//! against the GUI's forms (`crates/vike-connections/src/keys.rs`'s `form_fields`) and against the
//! cells the panel lights (`crates/vike-connections/src/status.rs`'s `TierCell`).
//!
//! The schema is the ONE statement of each venue's credential fields that the bridges' loaders,
//! this form and these cells all read (decision 0156 item 6). This file proves the panel's two
//! halves agree with it end to end: every form IS the schema row's form, every REQUIRED field is
//! offered (so a cell can always be completed from the panel), and a Save of the form's fields
//! through the store completes the cell while dropping any ONE required field leaves it naming
//! exactly that field as missing. The other half — the loaders against the schema — is each
//! bridge's, and `crates/vike-model/tests/credential_key_names.rs` holds the schema itself.
//!
//! An integration test for the reason `crates/vike-connections/tests/key_shapes.rs` gives: every
//! case writes a REAL store.

use vike_connections::keys::{FieldTarget, FormPlace, form_fields};
use vike_connections::{Presence, TierCell, VenueCredStatus};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::credential_schema::{FieldOwner, VenueKeySchema, schema_for};
use vike_model::{AccountTier, VENUES};
use vike_secrets::CredentialOwner;

use crate::support::{account, fields, grids, store};

fn schema(venue: &str) -> &'static VenueKeySchema {
    schema_for(venue).unwrap_or_else(|| panic!("{venue} has no schema row"))
}

/// The account fields one tier's form writes — the venue-wide attribution tail (a DEPLOYMENT row,
/// which the schema does not carry) aside.
fn account_form(venue: &str, tier: AccountTier) -> Vec<(&'static str, &'static str)> {
    form_fields(venue, FormPlace::Tier(tier))
        .into_iter()
        .filter_map(|f| match f.target {
            FieldTarget::Account(k) => Some((f.label, k.field)),
            FieldTarget::Venue(k) => panic!("{venue}: a tier form carries the venue's {}", k.field),
            FieldTarget::Deployment(_) => None,
        })
        .collect()
}

/// **The form IS the schema row's account form**, label for label, field for field, in order — at
/// both tiers that take a credential, for every roster venue.
#[test]
fn the_gui_form_is_the_schemas_form() {
    for venue in VENUES {
        let want: Vec<(&str, &str)> =
            schema(venue).form(FieldOwner::Account).iter().map(|f| (f.label, f.field)).collect();
        for tier in [AccountTier::Demo, AccountTier::Live] {
            assert_eq!(account_form(venue, tier), want, "{venue} {tier}");
        }
        assert!(
            form_fields(venue, FormPlace::Tier(AccountTier::Paper)).is_empty(),
            "{venue}: the paper simulator reads no credential, so paper has no form"
        );
        let app: Vec<(&str, &str)> =
            schema(venue).form(FieldOwner::Venue).iter().map(|f| (f.label, f.field)).collect();
        let gui_app: Vec<(&str, &str)> = form_fields(venue, FormPlace::VenueApp)
            .into_iter()
            .map(|f| match f.target {
                FieldTarget::Venue(k) => (f.label, k.field),
                other => panic!("{venue}: the application form carries {other:?}"),
            })
            .collect();
        assert_eq!(gui_app, app, "{venue}: the application group is the schema's venue form");
    }
}

/// **Every REQUIRED field is offered** — so every cell can be completed from the panel. A required
/// field the form did not offer would be a cell that can never light, whatever the operator types.
#[test]
fn every_required_field_is_offered_by_its_form() {
    for venue in VENUES {
        let offered: Vec<&str> =
            account_form(venue, AccountTier::Demo).iter().map(|(_, f)| *f).collect();
        for required in schema(venue).required(FieldOwner::Account) {
            assert!(offered.contains(&required), "{venue}: {required} is required and not offered");
        }
        let app: Vec<String> =
            form_fields(venue, FormPlace::VenueApp).iter().map(|f| f.hint().to_string()).collect();
        for required in schema(venue).required(FieldOwner::Venue) {
            assert!(app.iter().any(|a| a == required), "{venue}: app {required} is not offered");
        }
    }
}

fn demo_cell<'g>(rows: &'g [VenueCredStatus], venue: &str) -> &'g TierCell {
    &rows.iter().find(|s| s.venue == venue).expect("a roster venue").demo
}

fn label_of(text: &str) -> AccountLabel {
    AccountLabel::parse(text).expect("a legal label")
}

/// **A Save of the form's fields completes the cell, and dropping any ONE required field leaves the
/// cell naming exactly that field as missing** — round-tripped through a real store, for every
/// roster venue. Each case is its own labelled DEMO account, created with its tier before a field
/// is attached (a credential write never creates one).
#[test]
fn a_save_of_the_forms_fields_completes_the_cell_and_drops_name_the_missing_field() {
    let dir = store("vike-connections-schema-parity");
    for venue in VENUES {
        let form: Vec<&str> =
            account_form(venue, AccountTier::Demo).iter().map(|(_, f)| *f).collect();
        let required = schema(venue).required(FieldOwner::Account);

        let full = account(&dir, venue, AccountTier::Demo, Some("FULL"));
        let every: Vec<(&str, &str)> = form.iter().map(|f| (*f, "v")).collect();
        fields(&dir, &CredentialOwner::Account(full), &every);

        let mut dropped: Vec<(i64, &str)> = Vec::new();
        for (i, missing) in required.iter().enumerate() {
            let label = format!("DROP{i}");
            let id = account(&dir, venue, AccountTier::Demo, Some(&label));
            let rest: Vec<(&str, &str)> =
                form.iter().filter(|f| *f != missing).map(|f| (*f, "v")).collect();
            if !rest.is_empty() {
                fields(&dir, &CredentialOwner::Account(id), &rest);
            }
            dropped.push((id, *missing));
        }

        let g = grids(&dir);
        let rows = g.grid_for(&label_of("FULL")).expect("FULL rows exist");
        match demo_cell(rows, venue) {
            TierCell::Account(a) => {
                assert_eq!(a.id, full, "{venue}");
                assert_eq!(
                    a.presence(),
                    Presence::Complete,
                    "{venue}: the whole form completes it"
                );
            }
            other => panic!("{venue}: {other:?}"),
        }
        for (i, (id, missing)) in dropped.iter().enumerate() {
            let rows = g.grid_for(&label_of(&format!("DROP{i}"))).expect("the DROP rows exist");
            match demo_cell(rows, venue) {
                TierCell::Account(a) if a.id == *id => {
                    assert_eq!(a.missing_required, vec![*missing], "{venue}: dropping {missing}");
                    assert_ne!(a.presence(), Presence::Complete, "{venue}: dropping {missing}");
                }
                other => panic!("{venue} DROP{i}: {other:?}"),
            }
        }
    }
}

/// The venue's APPLICATION form round-trips the same way: cTrader's client pair, saved against the
/// venue, is held by the venue — and every required application field is among what was saved.
#[test]
fn the_application_form_round_trips_through_the_venue() {
    let dir = store("vike-connections-schema-app");
    for venue in VENUES {
        let app: Vec<String> =
            form_fields(venue, FormPlace::VenueApp).iter().map(|f| f.hint().to_string()).collect();
        if app.is_empty() {
            continue;
        }
        let pairs: Vec<(&str, &str)> = app.iter().map(|f| (f.as_str(), "v")).collect();
        fields(&dir, &CredentialOwner::Venue((*venue).to_string()), &pairs);
        let g = grids(&dir);
        let held = g.venue_fields(venue).expect("the venue's application rows");
        for required in schema(venue).required(FieldOwner::Venue) {
            assert!(held.holds(required), "{venue}: {required}");
        }
    }
}
