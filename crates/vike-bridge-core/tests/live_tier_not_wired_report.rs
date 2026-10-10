//! **The loud half of `PaperCause::LiveTierNotWired`, proved by RUNNING it and capturing the event.**
//!
//! A DEMO-pinned arm that finds a LIVE-tier key set in the store and no demo one stays paper — that
//! is the arming probe's half, a cause name. The other half is a sentence in the log, because a
//! configured-and-inert credential must not look like a fresh install: the credential doctrine is
//! that an ABSENT credential is silent and a PRESENT-and-unusable one is an `error!`.
//! `report_live_tier_not_wired` is the one place that sentence is written, so six bridges cannot
//! word it six ways, and this file holds what it carries: the level, the structured fields `venue`,
//! `account` and `found_tier` (and NOT `tier`, which on every live-MOUNT line is the tier that was
//! mounted — a refusal that carried it would match a filter for mounts that went live), the venue and
//! the account in the text, and nothing else — no key name, no value.
//!
//! The capture is `vike_log::capture::captured`, the workspace's one scoped `tracing` capture; its
//! module doc carries why a hand-rolled subscriber misses lines.

use tracing::Level;
use vike_bridge_core::venue_mount::{Tier, report_live_tier_not_wired};
use vike_log::capture::captured;
use vike_model::accounts::account_keys::AccountLabel;

#[test]
fn the_report_is_one_error_carrying_venue_account_and_found_tier() {
    let ((), seen) =
        captured(|| report_live_tier_not_wired("alpaca", &AccountLabel::Default, "sandbox"));
    assert_eq!(seen.len(), 1, "exactly one event: {seen:?}");
    let e = &seen[0];
    assert_eq!(e.level, Level::ERROR, "a present-and-unusable credential is an error: {e:?}");
    assert_eq!(e.fields.get("venue").map(String::as_str), Some("alpaca"), "{e:?}");
    assert_eq!(
        e.fields.get("account").map(String::as_str),
        Some("DEFAULT"),
        "the default account is rendered the way every other mount line renders it: {e:?}"
    );
    assert_eq!(
        e.fields.get("found_tier").map(String::as_str),
        Some(Tier::Live.as_str()),
        "the tier named is the one that was found and refused: {e:?}"
    );
    assert!(
        !e.fields.contains_key("tier"),
        "`tier` is reserved for the tier a mount BOUND; a refusal must not carry it: {e:?}"
    );
}

#[test]
fn the_message_says_which_tier_is_not_wired_and_that_nothing_traded() {
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let ((), seen) = captured(|| report_live_tier_not_wired("deribit", &alt, "testnet"));
    let e = &seen[0];
    assert_eq!(e.fields.get("account").map(String::as_str), Some("ALT"), "{e:?}");
    let text = &e.message;
    assert!(text.contains("deribit"), "names the venue: {text}");
    assert!(text.contains("LIVE"), "names the tier that was found: {text}");
    assert!(text.contains("testnet"), "names the tier the arm DOES mount: {text}");
    assert!(text.contains("PAPER"), "says where the venue stays: {text}");
    assert!(
        text.contains("vike-cli secrets list"),
        "points at the verb that names what the store holds, instead of naming it: {text}"
    );
    assert!(
        !text.contains("_API_") && !text.contains('='),
        "names no key, no value and no variable assignment: {text}"
    );
}

/// The two labels render differently, so two accounts of one venue are two different lines.
#[test]
fn two_accounts_of_one_venue_are_told_apart() {
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let ((), a) = captured(|| report_live_tier_not_wired("ig", &AccountLabel::Default, "demo"));
    let ((), b) = captured(|| report_live_tier_not_wired("ig", &alt, "demo"));
    assert_ne!(a[0].fields.get("account"), b[0].fields.get("account"));
}

#[test]
fn a_tier_names_itself_in_one_vocabulary() {
    assert_eq!(Tier::Demo.as_str(), "demo");
    assert_eq!(Tier::Live.as_str(), "live");
}
