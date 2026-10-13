//! **The credential FIELD table holds what every reader of it relies on** —
//! `vike_model::credential_schema`'s `VENUE_KEY_SCHEMAS`, held to its own claims.
//!
//! Since decision 0156 item 6 a credential belongs to its ACCOUNT: the store addresses it by
//! `credential.account_id` + `field` (or `venue_id` + `field` for a venue's application row), and
//! no credential name carries a tier. So this table carries FIELDS and nothing else, and what is
//! held here is that it stays that way:
//!
//! * one row per roster venue, each once ([`the_table_is_the_roster`]);
//! * every field word is a store `credential.field` word and spells NO tier token as a
//!   `_`-separated word ([`every_field_word_is_a_store_word_and_carries_no_tier_token`]) — the
//!   tier is the account row's, and a field like `DEMO_API_KEY` would put one back into the store;
//! * every row has at least one REQUIRED account field, so absent credentials stay the live gate
//!   for every venue ([`every_row_requires_an_account_field`]), and every required field is one the
//!   GUI form writes ([`every_required_field_is_on_the_form`]);
//! * VENUE-owned fields exist exactly where a bridge row says the venue owns them: cTrader's OAuth
//!   application pair ([`venue_owned_fields_are_ctraders_application_pair_alone`]);
//! * the generic HMAC rows are `credential_keys`' own field words, the passphrase required where the
//!   signer sends it ([`the_grid_rows_are_the_credential_keys_words`]);
//! * the accessors partition the row the way their docs say ([`the_accessors_partition_each_row`]).
//!
//! # Why `tests/`
//!
//! `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s literal sweep reads an
//! env-shaped literal in `src/` (a `#[cfg(test)]` module included) as a READ by this crate; a
//! `tests/` file is a region it does not sweep.
//!
//! ⚠ It names only a few venue ids as quoted literals, on purpose: a file naming the whole roster is
//! a per-venue ROW SITE to `crates/vike-ops/tests/venues/new_venue_gate.rs`.

use std::collections::BTreeSet;

use vike_model::VENUES;
use vike_model::credential_keys::{API_KEY, API_PASSPHRASE, API_SECRET, ATTRIBUTION_SUFFIXES};
use vike_model::credential_schema::{FieldOwner, VENUE_KEY_SCHEMAS, VenueKeySchema, schema_for};

/// Every word that spells a TIER — the account tiers (`paper`/`demo`/`live`), the retired `SIM`,
/// and the venues' own words for theirs (`SANDBOX`, `TESTNET`, `PRACTICE`, `MAINNET`, dukascopy's
/// `DEMO1`/`DEMO2`). None may be a `_`-separated word of a field: the tier is `account.tier`.
const TIER_TOKENS: &[&str] = &[
    "SIM", "PAPER", "DEMO", "LIVE", "SANDBOX", "TESTNET", "PRACTICE", "MAINNET", "DEMO1", "DEMO2",
];

/// The store's `credential.field` grammar (`crates/vike-secrets/src/db/upsert.rs`'s
/// `field_is_well_formed`, which this crate sits below and cannot call): `[A-Z0-9_]`, non-empty,
/// opening and closing with a letter or digit. A field outside it is refused by the writer, so a
/// table row naming one would be a form that can never be saved.
fn is_store_field_word(field: &str) -> bool {
    let alnum = |c: char| c.is_ascii_uppercase() || c.is_ascii_digit();
    !field.is_empty()
        && field.chars().all(|c| alnum(c) || c == '_')
        && field.starts_with(alnum)
        && field.ends_with(alnum)
}

/// The table is the roster: one row per `VENUES` id, each once, and `schema_for` finds that row.
#[test]
fn the_table_is_the_roster() {
    assert_eq!(VENUE_KEY_SCHEMAS.len(), VENUES.len(), "one row per roster venue");
    let venues: BTreeSet<&str> = VENUE_KEY_SCHEMAS.iter().map(|s| s.venue).collect();
    assert_eq!(venues, VENUES.iter().copied().collect::<BTreeSet<_>>());
    for schema in VENUE_KEY_SCHEMAS {
        assert!(
            schema_for(schema.venue).is_some_and(|s| std::ptr::eq(s, schema)),
            "{}: `schema_for` answers another row",
            schema.venue
        );
    }
    assert!(schema_for("nope").is_none());
}

/// **Every field word is a store word, and none spells a tier.** A field is what a bridge asks an
/// account for (`AccountCredentials::get`) and what the writer files a row under; a tier token in
/// it would be a second place the tier is written, which decision 0156 item 6 forbids.
#[test]
fn every_field_word_is_a_store_word_and_carries_no_tier_token() {
    let mut fields = 0usize;
    for schema in VENUE_KEY_SCHEMAS {
        let mut seen = BTreeSet::new();
        for f in schema.fields {
            fields += 1;
            assert!(
                is_store_field_word(f.field),
                "{}: `{}` is no field word",
                schema.venue,
                f.field
            );
            for word in f.field.split('_') {
                assert!(
                    !TIER_TOKENS.contains(&word),
                    "{}: the field `{}` spells the tier token `{word}` — the tier is the account \
                     row's, never a field's",
                    schema.venue,
                    f.field
                );
            }
            // Attribution codes are DEPLOYMENT names, read by name; no account holds one.
            for suffix in ATTRIBUTION_SUFFIXES {
                assert!(
                    !f.field.ends_with(suffix.trim_start_matches('_')),
                    "{}: `{}` is an attribution code, not an account field",
                    schema.venue,
                    f.field
                );
            }
            assert!(seen.insert(f.field), "{}: `{}` is listed twice", schema.venue, f.field);
            assert!(!f.label.trim().is_empty(), "{}: `{}` has no label", schema.venue, f.field);
        }
    }
    // ⚠ An absence proves nothing unless the loop saw fields.
    assert!(fields > 40, "only {fields} fields in the whole table — it has lost its rows");
}

/// **Every row REQUIRES at least one account field**, so a venue whose account holds nothing loads
/// nothing — absent credentials are the live gate, for every venue.
#[test]
fn every_row_requires_an_account_field() {
    for schema in VENUE_KEY_SCHEMAS {
        assert!(
            !schema.required(FieldOwner::Account).is_empty(),
            "{}: no required account field, so an EMPTY account would arm",
            schema.venue
        );
    }
}

/// **Every REQUIRED field is one the GUI form writes** — a required field no form offers would leave
/// the venue unarmable from the GUI, with a status dot that can never turn green.
#[test]
fn every_required_field_is_on_the_form() {
    for schema in VENUE_KEY_SCHEMAS {
        for f in schema.fields.iter().filter(|f| f.required) {
            assert!(f.form, "{}: `{}` is required and no form writes it", schema.venue, f.field);
        }
    }
}

/// **VENUE-owned fields are cTrader's OAuth APPLICATION pair, and nothing else.** One application
/// registration serves every account of that venue (its row's comment); every other venue's fields
/// belong to each account. A venue field anywhere else would be a credential shared by accounts the
/// operator created to keep apart.
#[test]
fn venue_owned_fields_are_ctraders_application_pair_alone() {
    let venue_owned: BTreeSet<(&str, &str)> = VENUE_KEY_SCHEMAS
        .iter()
        .flat_map(|s| s.venue_fields().map(move |f| (s.venue, f.field)))
        .collect();
    assert_eq!(
        venue_owned,
        BTreeSet::from([("ctrader", "CLIENT_ID"), ("ctrader", "CLIENT_SECRET")]),
        "the venue-owned fields moved"
    );
    let ctrader = schema_for("ctrader").expect("a roster venue");
    assert_eq!(ctrader.required(FieldOwner::Venue), ["CLIENT_ID", "CLIENT_SECRET"]);
    assert_eq!(ctrader.required(FieldOwner::Account), ["ACCESS_TOKEN", "REFRESH_TOKEN"]);
}

/// **The generic HMAC rows are `credential_keys`' own field words** — `API_KEY`, `API_SECRET`,
/// `API_PASSPHRASE`, the set `vike_secrets::credentials::api_credentials` reads — with the passphrase
/// REQUIRED exactly where the signer sends it on every request (OKX), optional elsewhere.
#[test]
fn the_grid_rows_are_the_credential_keys_words() {
    let words = |s: &VenueKeySchema| s.fields.iter().map(|f| f.field).collect::<Vec<_>>();
    for required in [false, true] {
        let grid = VenueKeySchema::grid("example", required);
        assert_eq!(words(&grid), [API_KEY, API_SECRET, API_PASSPHRASE]);
        let want: &[&str] =
            if required { &[API_KEY, API_SECRET, API_PASSPHRASE] } else { &[API_KEY, API_SECRET] };
        assert_eq!(grid.required(FieldOwner::Account), want, "passphrase required: {required}");
        assert!(grid.venue_fields().next().is_none(), "the grid has no venue-owned field");
    }
    assert_eq!(
        schema_for("okx").expect("a roster venue").required(FieldOwner::Account),
        [API_KEY, API_SECRET, API_PASSPHRASE],
        "OKX's signer sends the passphrase on every request"
    );
    assert_eq!(
        schema_for("binance").expect("a roster venue").required(FieldOwner::Account),
        [API_KEY, API_SECRET],
        "an HMAC venue that does not need the passphrase does not require it"
    );
}

/// **The accessors partition each row the way their docs say**: `account_fields` and `venue_fields`
/// split the fields by owner, `required(owner)` is that owner's required subset, `form(owner)` the
/// subset a form writes (no read-only field), and `field` finds each word and nothing else.
#[test]
fn the_accessors_partition_each_row() {
    for schema in VENUE_KEY_SCHEMAS {
        let account: Vec<&str> = schema.account_fields().map(|f| f.field).collect();
        let venue: Vec<&str> = schema.venue_fields().map(|f| f.field).collect();
        assert_eq!(account.len() + venue.len(), schema.fields.len(), "{}", schema.venue);
        for owner in [FieldOwner::Account, FieldOwner::Venue] {
            let of_owner: Vec<&str> =
                schema.fields.iter().filter(|f| f.owner == owner).map(|f| f.field).collect();
            let required: Vec<&str> = schema
                .fields
                .iter()
                .filter(|f| f.owner == owner && f.required)
                .map(|f| f.field)
                .collect();
            assert_eq!(schema.required(owner), required, "{} {owner:?}", schema.venue);
            for f in schema.form(owner) {
                assert!(
                    of_owner.contains(&f.field),
                    "{}: {} is another owner's",
                    schema.venue,
                    f.field
                );
                assert!(f.form, "{}: the form offers the read-only `{}`", schema.venue, f.field);
            }
        }
        for f in schema.fields {
            assert_eq!(schema.field(f.field).map(|g| g.field), Some(f.field), "{}", schema.venue);
        }
        assert!(schema.field("NO_SUCH_FIELD").is_none(), "{}", schema.venue);
    }
}
