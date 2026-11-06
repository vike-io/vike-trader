//! **Planting a keyed account row** for the `vike-backend venues` golden (`src/venues_cli_parity_tests.rs`)
//! and the remote backend's conversions (`src/remote_backend/wire_tests.rs`). The credential FIELDS that
//! arm each roster venue are the arming parity golden's fixtures, ONE copy in the gatewayhub's testkit
//! (`crates/vike-gatewayhub/src/testkit/arming_fixtures.rs`'s `armed_fields`) that both sides of the wire
//! read; this file kept a twin of them until 2026-10-31.
#![allow(dead_code)]

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::AdapterPolicy;
use vike_venues::credential_schema::{FieldOwner, schema_for};

/// Plant one ACTIVE row `(venue, label)` at `tier` holding `fields` — each FIELD filed against the
/// owner the venue's schema row names (cTrader's OAuth application pair is the VENUE's, every other
/// field the ACCOUNT's), the filing `crates/vike-tradehub-core/src/test_support/account_book.rs`'s `with_keyed_account` does.
pub(crate) fn keyed_account(
    policy: AdapterPolicy,
    venue: &str,
    label: &AccountLabel,
    tier: AccountTier,
    fields: &[(&str, &str)],
) -> AdapterPolicy {
    let is_venue_field = |field: &str| {
        schema_for(venue).and_then(|s| s.field(field)).is_some_and(|f| f.owner == FieldOwner::Venue)
    };
    let account_fields: Vec<(&str, &str)> =
        fields.iter().copied().filter(|(f, _)| !is_venue_field(f)).collect();
    let venue_fields: Vec<(&str, &str)> =
        fields.iter().copied().filter(|(f, _)| is_venue_field(f)).collect();
    policy
        .with_credentials(venue, label, tier, &account_fields)
        .with_venue_credentials(venue, &venue_fields)
}
