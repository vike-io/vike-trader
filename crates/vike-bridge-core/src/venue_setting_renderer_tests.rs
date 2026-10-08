use super::*;
// Only the TESTS here render an operator-facing key, so this import is test-local: a
// crate-level `use` of it would be unused in a normal build and `-D warnings` refuses that.
use vike_secrets::venue_setting::venue_setting_key;

/// ⚠ **THE PROOF.** Every name the renderer produces for the nine rows ruling 10 actually moves
/// must classify BACK to the venue, tier and field it was rendered from. That is what makes
/// this the inverse of [`classify_credential_name`] rather than a second function that happens
/// to agree today — and it is the property the whole move rests on, because a loader looks the
/// legacy name up and the classifier is what put the row where it is.
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

/// ⚠ **THE CHECK §6.2 SAYS THE MIGRATION OWES.** A settings row rendering a name a LIVE
/// credential row already holds puts one key in the map with two candidate values, resolved by
/// insertion order. SQLite cannot refuse it — `credential_one_live_name` holds inside
/// `credential` alone — so this does.
///
/// ⚠ The live set is built FROM the renderer rather than spelled, and the reason is this file:
/// a whole env-prefixed literal in `src/` is read by
/// `crates/vike-ops/tests/settings_secrets/settings_registry.rs`' sweep as evidence that THIS CRATE reads that
/// variable, and a row under the owning bridge does not declare a read here — rows are keyed
/// `(name, krate)`. The negative control below is what keeps this from being circular.
#[test]
fn a_rendered_name_that_a_live_credential_already_holds_is_a_collision() {
    let key = venue_setting_key("ibkr", Some("demo"), "BACKEND");
    let rendered = venue_setting_names("ibkr", Some("demo"), "BACKEND");
    let live: std::collections::BTreeSet<String> = rendered.iter().cloned().collect();
    let found = rendered_name_collisions(std::slice::from_ref(&key), &live);
    assert_eq!(found.len(), rendered.len(), "every rendered name collides: {found:?}");
    for name in &rendered {
        assert_eq!(found.get(name), Some(&key), "…and each names the settings key");
    }
}

/// ⚠ **THE NEGATIVE CONTROL.** Without it the test above would pass for a function that
/// reported every key it was handed. The live set here holds a name no grammar renders.
#[test]
fn no_overlap_is_no_collision() {
    let live: std::collections::BTreeSet<String> =
        ["nothing-a-renderer-would-ever-produce".to_string()].into_iter().collect();
    let keys = [
        venue_setting_key("ibkr", Some("demo"), "BACKEND"),
        venue_setting_key("polymarket", None, "PROXY_HOST"),
    ];
    assert!(rendered_name_collisions(&keys, &live).is_empty());
}

/// ⚠ **THE DUKASCOPY ROW COLLIDES ON EITHER OF ITS TWO NAMES.** One row renders both, so a
/// half-finished move that left only the second behind must still be caught — a check that
/// looked at one rendered name would pass while the map carried two candidate values for it.
#[test]
fn a_one_to_many_row_collides_on_either_name() {
    let key = venue_setting_key("dukascopy", Some("demo"), "SERVER");
    let rendered = venue_setting_names("dukascopy", Some("demo"), "SERVER");
    assert_eq!(rendered.len(), 2, "this row is the one-to-many one: {rendered:?}");
    for surviving in &rendered {
        // ONE of the pair survives in `credential` — the half-done move, either way round.
        let live: std::collections::BTreeSet<String> = [surviving.clone()].into_iter().collect();
        let found = rendered_name_collisions(std::slice::from_ref(&key), &live);
        assert_eq!(found.get(surviving), Some(&key), "the row renders it: {found:?}");
        assert_eq!(found.len(), 1, "and only the surviving one collides: {found:?}");
    }
}

/// A key this grammar does not own is SKIPPED, not guessed at — the `setting` table carries
/// every `config.*` key on the box and only the `venue.` ones render a credential name.
#[test]
fn an_unrelated_settings_key_renders_nothing_and_collides_with_nothing() {
    let live: std::collections::BTreeSet<String> =
        ["nothing-a-renderer-would-ever-produce".to_string()].into_iter().collect();
    let keys = ["log_dir".to_string(), "max_notional_per_order".to_string()];
    assert!(rendered_name_collisions(&keys, &live).is_empty());
}

/// ⚠ …and every one of those names is one the classifier ALREADY marks as owed to
/// `venue_setting`. Without this the inverse proof above would pass for a name nobody ever
/// intends to move, and the renderer's roster would be free to drift away from
/// `pending_move`'s.
///
/// ⚠ This paragraph sat above the WRONG test until 2026-09-22 — glued to the head of a doc
/// block describing the settings-key round trip, which was a different claim entirely and has
/// now moved down to `vike_secrets::venue_setting`. It is restored to the test it describes.
#[test]
fn every_rendered_name_is_one_the_classifier_marked_as_moving() {
    use vike_secrets::PendingMove;
    for (venue, tier, field) in [
        ("ibkr", Some("demo"), "BACKEND"),
        ("fxcm", Some("demo"), "CONNECTION"),
        ("dukascopy", Some("demo"), "SERVER"),
        ("polymarket", None, "PROXY_HOST"),
    ] {
        for name in venue_setting_names(venue, tier, field) {
            assert_eq!(
                classify_credential_name(&name).pending_move,
                Some(PendingMove::VenueSetting),
                "{name} is rendered by the venue_setting renderer but is not marked as moving \
                     there — the two rosters have drifted"
            );
        }
    }
}
