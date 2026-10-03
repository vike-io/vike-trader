//! **What a demo-pinned arm says about a LIVE tier it will not use — the three lines beyond the
//! one `live_tier_not_wired_report.rs` holds — proved by RUNNING them and capturing the events.**
//!
//! `report_live_tier_not_wired` covers one cell of a small table: the arm found NO demo set and a
//! COMPLETE live one. Two cells of the same table were silent:
//!
//! * a COMPLETE live set BESIDE a complete demo set — the arm mounts the demo tier, the live set is
//!   never read, and nothing said so (an operator who stored live keys believing they would trade
//!   live read a green mount line that named the demo tier and nothing else);
//! * a HALF-WRITTEN live set (a typo'd secret name) with no demo set — the loader calls that
//!   absent, so the venue stayed paper under the same words as an empty store.
//!
//! `report_unused_live_tier` is the one place the whole table is spoken, so six bridges cannot word
//! it six ways. This file holds it cell by cell: the level, the structured fields (`venue`,
//! `account`, `found_tier` — and NEVER `tier`, which on every live-MOUNT line is the tier that was
//! mounted), the words, that the unused-beside-demo `warn!` is said ONCE per process per
//! `(venue, account)`, and that the missing-keys line carries key NAMES and nothing else.
//!
//! The dedup state is process-global, so every test below names an account label (and one a venue)
//! that no other test in this binary uses.

use vike_bridge_core::credentials::{Environment, TierKeys, tier_keys_for_account};
use vike_bridge_core::venue_mount::{LiveTierSet, Tier, report_unused_live_tier};
use vike_bridge_core::venue_mount_fixture::{CapturedEvent, captured};
use vike_model::account_keys::{AccountLabel, account_key};

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).expect("a legal label")
}

fn say(
    venue: &'static str,
    account: &AccountLabel,
    demo_mounts: bool,
    live: &LiveTierSet,
) -> Vec<CapturedEvent> {
    captured(|| report_unused_live_tier(venue, account, "sandbox", demo_mounts, live)).1
}

// ── a complete live set BESIDE the demo one ──────────────────────────────────────────────────────

#[test]
fn a_live_set_beside_the_demo_one_is_one_warn_naming_venue_account_and_found_tier() {
    let seen = say("alpaca", &label("BESIDE1"), true, &LiveTierSet::Complete);
    assert_eq!(seen.len(), 1, "exactly one event: {seen:?}");
    let e = &seen[0];
    assert_eq!(
        e.level,
        tracing::Level::WARN,
        "the demo tier DID mount, so this is a warning: {e:?}"
    );
    assert_eq!(e.field("venue"), Some("alpaca"), "{e:?}");
    assert_eq!(e.field("account"), Some("BESIDE1"), "{e:?}");
    assert_eq!(e.field("found_tier"), Some(Tier::Live.as_str()), "{e:?}");
    assert!(
        e.field("tier").is_none(),
        "`tier` is reserved for the tier a mount BOUND; a diagnostic must not carry it: {e:?}"
    );
    let text = &e.message;
    assert!(text.contains("alpaca"), "names the venue: {text}");
    assert!(text.contains("LIVE"), "names the tier that was found: {text}");
    assert!(text.contains("sandbox"), "names the tier the arm DID mount: {text}");
    assert!(text.contains("UNUSED"), "says the live set is not used: {text}");
    assert!(
        !text.contains("REAL-MONEY") && !text.contains("LIVE exec client"),
        "must not borrow the live-mount announcement's wording: {text}"
    );
    assert!(
        !text.contains("MAINNET") && !text.contains('='),
        "names no key, no value and no variable assignment: {text}"
    );
}

#[test]
fn the_warn_is_said_once_per_process_for_one_venue_and_account() {
    let alt = label("ONCE1");
    let first = say("ig", &alt, true, &LiveTierSet::Complete);
    let again = say("ig", &alt, true, &LiveTierSet::Complete);
    assert_eq!(first.len(), 1, "the first mount says it: {first:?}");
    assert!(again.is_empty(), "the second mount in the same process is silent: {again:?}");
    // …and the dedup is per (venue, account), not per venue and not global.
    assert_eq!(
        say("ig", &label("ONCE2"), true, &LiveTierSet::Complete).len(),
        1,
        "another ACCOUNT of the same venue is its own line"
    );
    assert_eq!(
        say("deribit", &alt, true, &LiveTierSet::Complete).len(),
        1,
        "the same account label on another VENUE is its own line"
    );
}

// ── the cells that were already loud, and the ones that must stay silent ─────────────────────────

#[test]
fn a_live_set_alone_is_still_the_not_wired_error() {
    let seen = say("alpaca", &label("ALONE1"), false, &LiveTierSet::Complete);
    assert_eq!(seen.len(), 1, "{seen:?}");
    let e = &seen[0];
    assert_eq!(e.level, tracing::Level::ERROR, "{e:?}");
    assert_eq!(e.field("found_tier"), Some("live"), "{e:?}");
    assert!(e.message.contains("stays PAPER"), "{}", e.message);
}

#[test]
fn nothing_is_said_where_nothing_is_wrong() {
    let incomplete = LiveTierSet::Incomplete { missing: vec!["some-key".to_string()] };
    for (venue, demo_mounts, live) in [
        ("alpaca", true, LiveTierSet::Absent),
        ("alpaca", false, LiveTierSet::Absent),
        // A half-written live set BESIDE a complete demo set changes nothing: the demo tier mounts
        // and no live key was going to be read.
        ("alpaca", true, incomplete),
    ] {
        let seen = say(venue, &label("QUIET1"), demo_mounts, &live);
        assert!(seen.is_empty(), "{venue} demo_mounts={demo_mounts} {live:?}: {seen:?}");
    }
}

// ── a half-written live set ──────────────────────────────────────────────────────────────────────

#[test]
fn a_half_written_live_set_is_one_error_naming_the_missing_keys_and_only_names() {
    let missing = vec!["first-missing-key".to_string(), "second-missing-key".to_string()];
    let seen = say(
        "ctrader",
        &label("HALF1"),
        false,
        &LiveTierSet::Incomplete { missing: missing.clone() },
    );
    assert_eq!(seen.len(), 1, "exactly one event: {seen:?}");
    let e = &seen[0];
    assert_eq!(
        e.level,
        tracing::Level::ERROR,
        "a present-and-unusable credential is an error: {e:?}"
    );
    assert_eq!(e.field("venue"), Some("ctrader"), "{e:?}");
    assert_eq!(e.field("account"), Some("HALF1"), "{e:?}");
    assert_eq!(e.field("found_tier"), Some("live"), "{e:?}");
    assert!(e.field("tier").is_none(), "{e:?}");
    for name in &missing {
        assert!(e.message.contains(name.as_str()), "names the missing key {name}: {}", e.message);
    }
    assert!(e.message.contains("PAPER"), "says where the venue stays: {}", e.message);
    assert!(
        e.message.contains("sandbox"),
        "names the tier the arm DOES mount, which a complete live set would not change: {}",
        e.message
    );
}

// ── reading the state out of a store ─────────────────────────────────────────────────────────────

fn spellings(label: &AccountLabel) -> Vec<TierKeys> {
    tier_keys_for_account("deribit", Environment::Live, label)
}

fn store(pairs: &[(String, &str)]) -> std::collections::HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.clone(), (*v).to_string())).collect()
}

#[test]
fn the_state_is_read_from_the_loaders_verdict_and_then_from_the_names() {
    let default = AccountLabel::Default;
    let default_spellings = spellings(&default);
    let names = &default_spellings[0];
    let (key, secret) = (names.required[0].clone(), names.required[1].clone());

    // The loader's verdict wins: a complete set is complete whatever the names say.
    assert_eq!(LiveTierSet::read(true, &spellings(&default), &store(&[])), LiveTierSet::Complete);
    // Nothing stored: absent.
    assert_eq!(LiveTierSet::read(false, &spellings(&default), &store(&[])), LiveTierSet::Absent);
    // One of two stored: the other is named, and ONLY the other.
    assert_eq!(
        LiveTierSet::read(false, &spellings(&default), &store(&[(key.clone(), "v")])),
        LiveTierSet::Incomplete { missing: vec![secret.clone()] }
    );
    // A blank value is an absent one, exactly as the loader reads it.
    assert_eq!(
        LiveTierSet::read(
            false,
            &spellings(&default),
            &store(&[(key, "v"), (secret.clone(), "  ")])
        ),
        LiveTierSet::Incomplete { missing: vec![secret] }
    );
}

#[test]
fn a_labelled_account_is_told_its_own_names_and_never_the_default_accounts() {
    let alt = label("NAMES1");
    let default_spellings = spellings(&AccountLabel::Default);
    let alt_spellings = spellings(&alt);
    let (default_names, alt_names) = (&default_spellings[0], &alt_spellings[0]);
    let alt_key = account_key(&default_names.required[0], &alt);
    assert_eq!(alt_names.required[0], alt_key, "the label is appended to the whole key");
    // Only the DEFAULT account's key is stored: ALT has started nothing.
    let only_default = store(&[(default_names.required[0].clone(), "v")]);
    assert_eq!(LiveTierSet::read(false, &spellings(&alt), &only_default), LiveTierSet::Absent);
    // ALT's own key is stored: ALT's OTHER key is the one named — the labelled spelling.
    let only_alt = store(&[(alt_key, "v")]);
    assert_eq!(
        LiveTierSet::read(false, &spellings(&alt), &only_alt),
        LiveTierSet::Incomplete { missing: vec![alt_names.required[1].clone()] }
    );
}

#[test]
fn the_legacy_tier_spelling_is_a_set_of_its_own() {
    // deribit's live tier is read under `LIVE` and then under the legacy `MAINNET` spelling; a set
    // started under the legacy one is half-written under THAT spelling's names.
    let spellings = spellings(&AccountLabel::Default);
    assert_eq!(spellings.len(), 2, "the current spelling and the legacy one: {spellings:?}");
    let legacy_key = spellings[1].required[0].clone();
    let legacy_secret = spellings[1].required[1].clone();
    assert_ne!(legacy_key, spellings[0].required[0]);
    assert_eq!(
        LiveTierSet::read(false, &spellings, &store(&[(legacy_key, "v")])),
        LiveTierSet::Incomplete { missing: vec![legacy_secret] }
    );
}

#[test]
fn a_tier_less_key_starts_no_live_set() {
    // A key required by a tier but belonging to none (cTrader's app registration pair) must not
    // read as a started live set — that would accuse an operator who never wrote a live key.
    let keys = TierKeys {
        required: vec!["app-id".to_string(), "live-token".to_string()],
        tier_named: vec!["live-token".to_string()],
    };
    assert_eq!(keys.missing_in(&store(&[("app-id".to_string(), "v")])), None);
    assert_eq!(
        keys.missing_in(&store(&[("live-token".to_string(), "v")])),
        Some(vec!["app-id".to_string()])
    );
}
