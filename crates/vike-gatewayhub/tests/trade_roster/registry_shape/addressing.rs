//! **Which venues can address a SECOND account**, asked of the REAL registry: the two pins that make a
//! venue scaffolded by `just new-venue` fail until somebody writes the argument. They were
//! `crates/vike-tradehub/tests/account_fanout.rs`'s and `unaddressable_account_warning.rs`'s while the
//! registry was the tradehub's; they read each bridge's `VenueDeclaration::addresses_accounts`, which only
//! the crate that links the bridges can, so they moved with it (wave 2, W9b).
//!
//! A scaffolded venue declares `addresses_accounts: false`, because a `true` beside a start that still reads
//! the venue's UNLABELLED keys is the two-engines-on-one-account hazard the seam exists to prevent. So the
//! FIRST refusal every future venue's second account meets is `NoAccountSupport`, and these tests are where
//! that meets a human.

use std::collections::HashMap;

use vike_config::ArmingBlock;
use vike_gatewayhub::registry::trade::REGISTRY;
use vike_model::AccountTier;
use vike_model::VENUES;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::AdapterPolicy;

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).expect("a legal label")
}

/// **A labelled account is REFUSED on every venue whose start arm cannot address one** — and since
/// 2026-09-15 that is NO roster venue.
///
/// A venue qualifies when its arm threads the account label into a loader that reads THAT account's
/// key names. An arm that still read UNLABELLED names would build a SECOND live client on the
/// DEFAULT account's keys — two engines on one venue account, the exact accident `route_key` and
/// `vike_boot::live_lock` exist to prevent.
///
/// Asserted over the WHOLE roster, in both directions, so the supported set cannot silently grow
/// *or shrink*. ⚠ The list below is the REFUSED half rather than the supported half, deliberately:
/// it is the short one, so an added venue that forgets its loader shows up as a test that has to be
/// edited, and the edit is the place the argument gets made.
///
/// ⚠ **It is EMPTY, and an empty list is still the right shape.** `dukascopy` was its last entry
/// and left when its arm started addressing the settings database's `account` rows
/// (`crates/bridges/dukascopy/src/adapter.rs` since the venue-adapter contract). A venue scaffolded by
/// `just new-venue` declares `addresses_accounts: false`, so the next venue to exist lands here and the
/// test below fails until somebody writes the argument — which is exactly what this list is for.
/// Replacing it with `assert!(no venue is refused)` would delete that.
const REFUSED: &[&str] = &[];

#[test]
fn a_labelled_account_is_refused_on_every_venue_whose_arm_cannot_address_one() {
    let alt = label("ALT");
    for venue in VENUES {
        let policy = AdapterPolicy::default()
            .with_account(venue, &AccountLabel::Default, AccountTier::Demo)
            .with_account(venue, &alt, AccountTier::Demo);
        let rows = crate::local::rows(REGISTRY, venue, &HashMap::new(), Some(&policy));
        let row = rows.iter().find(|r| r.label == alt).expect("the table names ALT");
        assert_eq!(row.effective, None, "{venue}: no store, so nothing arms either way");
        if REFUSED.contains(venue) {
            assert_eq!(
                row.block,
                ArmingBlock::NoAccountSupport,
                "{venue}'s arm reads unlabelled keys — a second account there would trade the \
                 FIRST account's credentials"
            );
            assert!(row.why().contains("ALT"), "{venue}: {}", row.why());
        } else {
            assert_ne!(
                row.block,
                ArmingBlock::NoAccountSupport,
                "{venue} threads the account label into its own loader, so a second account is \
                 expressible"
            );
        }
    }
}

/// **THE RETIREMENT, pinned**: no roster venue's labelled account is refused for want of
/// second-account support any more.
///
/// Driven over the real projection, over the whole roster, with a labelled account row planted for each
/// — so a change that put a venue back into the refused class fails here by name rather than by the
/// message quietly reappearing on somebody's daemon. The message's CONTENT is proven against a planted
/// row in `crates/vike-tradehub/tests/unaddressable_account_warning.rs`.
#[test]
fn no_roster_venue_is_unaddressable_any_more() {
    let alt = label("ALT");
    for venue in VENUES {
        let adapter_policy = AdapterPolicy::default()
            .with_account(venue, &AccountLabel::Default, AccountTier::Demo)
            .with_account(venue, &alt, AccountTier::Demo);
        assert_eq!(
            crate::local::unaddressable_accounts_message(
                REGISTRY,
                &HashMap::new(),
                Some(&adapter_policy)
            ),
            None,
            "{venue} is refused for want of second-account support — if that is deliberate, this \
             test and the REFUSED list above are where the argument gets written"
        );
    }
}
