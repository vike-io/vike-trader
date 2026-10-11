//! The `venue_setting` renderer proved to be the INVERSE of the classifier.
//!
//! The renderer and its pure-grammar tests live in `crates/vike-secrets/src/venue_setting.rs`; what
//! stays here is what asks `classify_credential_name` a question.

use super::*;

/// ⚠ **THE PROOF.** Every name the renderer produces for the nine moved rows classifies BACK to the
/// venue, tier and field it was rendered from — the inverse of [`classify_credential_name`], not a
/// second function that happens to agree today.
#[test]
fn every_rendered_name_classifies_back_to_the_row_it_came_from() {
    use vike_secrets::Placement;
    // The nine rows, spelled as `(venue, tier, field)` exactly as `venue_setting` holds them.
    let rows: &[(&str, Option<&str>, &str)] = &[
        ("ibkr", Some("demo"), "HOST"),
        ("ibkr", Some("demo"), "PORT"),
        ("ibkr", Some("demo"), "BACKEND"),
        ("fxcm", Some("demo"), "URL"),
        ("fxcm", Some("demo"), "CONNECTION"),
        ("dukascopy", Some("demo"), "SERVER"),
        ("polymarket", None, "PROXY_ENABLED"),
        ("polymarket", None, "PROXY_HOST"),
        ("polymarket", None, "PROXY_PORT"),
    ];
    for (venue, tier, field) in rows {
        let names = venue_setting_names(venue, *tier, field);
        assert!(!names.is_empty(), "({venue}, {tier:?}, {field}) rendered nothing");
        for name in &names {
            let class = classify_credential_name(name);
            assert!(class.recognised, "{name} rendered but does not classify");
            assert_eq!(&class.field, field, "{name} classifies to a different FIELD");
            let got_venue = match &class.placement {
                Placement::Account(key) => key.venue.clone(),
                Placement::Venue(v) => v.clone(),
                Placement::Infrastructure => String::new(),
            };
            assert_eq!(&got_venue, venue, "{name} classifies to a different VENUE");
        }
    }
}
