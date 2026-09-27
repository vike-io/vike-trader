//! **The refusal a venue whose mount addresses only ONE account gets — said out loud** — and, since
//! 2026-09-15, a message NO ROSTER VENUE PRODUCES.
//!
//! # What this file was, and why it did not simply get deleted
//!
//! It was written for dukascopy. An operator who wanted two Dukascopy accounts wrote the obvious
//! thing — `dukascopy = "demo"` in `<project>/settings/policy.toml`'s `[venues]`, an `[accounts]`
//! line naming a second, and both credential trios in the store — and the whole mount emitted,
//! verbatim, three lines: `arming ceiling for bybit is paper`, `effective fee schedule`, and the
//! HALT sentinel line. None of them named dukascopy at all. `vike_mount::unaddressable_accounts_
//! message` is what closed that, and this file proved it fired.
//!
//! **Dukascopy is no longer in that class.** Its arm addresses the account it is mounted for, keyed
//! on the settings database's `account` row (`vike_mount`'s `dukascopy` module), so it joined
//! `arm_addresses_accounts` and every `vike_model::VENUES` id is now in that list. Nothing on the
//! roster can produce an `ArmingBlock::NoAccountSupport` row, so nothing can produce this message
//! through `unaddressable_accounts_message`, so the EMISSION half of this file became a test of a
//! thing that cannot happen — the shape the file's own doc anticipated.
//!
//! ⚠ **Deleting it would have been wrong, and the reason is `vike_ops::new_venue_gate`'s.** A venue
//! scaffolded by `just new-venue` is deliberately NOT added to `arm_addresses_accounts` — the marker
//! comment inside that list instructs the author not to, because a `true` there beside an arm that
//! still reads the venue's UNLABELLED keys is the two-engines-on-one-account hazard the seam exists
//! to prevent. So the FIRST refusal every future venue's second account meets is exactly this one,
//! and a message left with no test between now and then is a message that rots unread. What changed
//! is HOW it is driven:
//!
//! * the CONTENT is proven against a PLANTED `NoAccountSupport` row, through
//!   `vike_mount::unaddressable_accounts_text` — the rows-only half split out of the message for
//!   this purpose, the same move `vike_mount::accounts_to_mount` made out of its own loop;
//! * the SILENCE is proven the way it always was, over the real projection;
//! * and one new assertion pins the retirement itself: no roster venue produces the block today, so
//!   a regression that re-refused one fails here by name.
//!
//! The through-the-mount EMISSION test is GONE with its producer, and that is recorded rather than
//! quietly dropped.
//!
//! ⚠ **So is the SILENCE test, and for a harder reason: it could not fail.**
//! `crates/vike-mount/tests/unaddressable_account_silence.rs` asserted that a single-account box
//! emits no unaddressable-account line — over a projection that cannot produce one for ANY roster
//! venue, for any input, which is what the paragraph above says in the other direction. Its module
//! doc claimed it was "doing MORE work than it was". That file now proves the refusal an operator
//! actually meets on a two-account box (dukascopy's one-sidecar decline), in both directions, and
//! its own doc records the replacement. The retirement this file's last test pins is what covers
//! the property that was lost — and it is a test that CAN fail, which is the whole difference.

use std::collections::HashMap;

use vike_config::{ArmingBlock, VenueArming, VenueMode, VenuePolicy};
use vike_model::VENUES;
use vike_model::account_keys::AccountLabel;

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).expect("a legal label")
}

/// A row exactly as a venue outside `arm_addresses_accounts` would produce one. Planted, because no
/// roster venue produces one any more — see the module doc.
fn refused_row(venue: &'static str, label: AccountLabel) -> VenueArming {
    VenueArming {
        venue,
        label,
        ceiling: VenueMode::Demo,
        effective: VenueMode::Paper,
        block: ArmingBlock::NoAccountSupport,
    }
}

/// **The message still says everything an operator needs**, proven against a planted row so it
/// cannot rot between the venue it was written for and the next venue that needs it.
#[test]
fn the_refusal_message_names_the_venue_the_account_and_the_way_out() {
    let rows = vec![refused_row("bybit", label("SECOND"))];
    let report = vike_mount::unaddressable_accounts_text(&rows).expect("a refused row speaks");

    for needle in [
        "A SECOND ACCOUNT IS NAMED",
        "bybit",
        "`SECOND`",
        "addresses exactly ONE account",
        "DEFAULT one",
        "UNLABELLED credential keys",
        "policy.accounts.bybit.SECOND",
        "`bybit#SECOND` engine is built",
    ] {
        assert!(report.contains(needle), "the report must carry {needle:?}: {report}");
    }
    // …and it says the mount CONTINUES, so nobody reads it as a startup failure.
    assert!(report.contains("report, not a refusal"), "{report}");
    // …and it names the way out, which is the only thing the operator can actually do.
    assert!(report.contains("VIKE_SETTINGS_DIR"), "{report}");
}

/// Two refused accounts are named in ONE message, not two — the property that makes it a
/// process-wide line rather than a per-venue one.
#[test]
fn every_refused_account_is_named_in_one_message() {
    let rows = vec![refused_row("bybit", label("SECOND")), refused_row("okx", label("THIRD"))];
    let report = vike_mount::unaddressable_accounts_text(&rows).expect("refused rows speak");
    assert!(report.contains("bybit account `SECOND`"), "{report}");
    assert!(report.contains("okx account `THIRD`"), "{report}");
    assert_eq!(
        report.matches("A SECOND ACCOUNT IS NAMED").count(),
        1,
        "one headline, however many accounts: {report}"
    );
}

/// A row carrying any other block says nothing — the message is keyed on the BLOCK, so a venue that
/// is merely unconfigured is never reported as unaddressable.
#[test]
fn only_the_no_account_support_block_speaks() {
    for block in ArmingBlock::ALL {
        if block == ArmingBlock::NoAccountSupport {
            continue;
        }
        let rows = vec![VenueArming {
            venue: "bybit",
            label: label("SECOND"),
            ceiling: VenueMode::Demo,
            effective: VenueMode::Paper,
            block,
        }];
        assert_eq!(
            vike_mount::unaddressable_accounts_text(&rows),
            None,
            "{block:?} is not this message's subject"
        );
    }
}

/// **THE RETIREMENT, pinned**: no roster venue's labelled account is refused for want of
/// second-account support any more.
///
/// Driven over the real projection, over the whole roster, with a labelled account declared for each
/// — so a change that put a venue back into the refused class fails here by name rather than by the
/// message quietly reappearing on somebody's daemon.
#[test]
fn no_roster_venue_is_unaddressable_any_more() {
    let alt = label("ALT");
    for venue in VENUES {
        let policy = VenuePolicy::default().declare(venue, VenueMode::Demo).declare_account(
            venue,
            &alt,
            VenueMode::Demo,
        );
        let mount_policy = vike_mount::MountPolicy { venues: policy, ..Default::default() };
        assert_eq!(
            vike_mount::unaddressable_accounts_message(&HashMap::new(), Some(&mount_policy)),
            None,
            "{venue} is refused for want of second-account support — if that is deliberate, this \
             test and `crates/vike-mount/tests/account_fanout.rs`'s REFUSED list are where the \
             argument gets written"
        );
    }
}

/// **A box with one account per venue gets no message at all** — unchanged, and driven over the
/// settings of a box that arms dukascopy and names no second account, which is every deployment
/// today.
#[test]
fn a_single_account_box_has_nothing_to_say() {
    let vars: HashMap<String, String> =
        [("DUKASCOPY_DEMO1_LOGIN", "fake-login-1"), ("DUKASCOPY_DEMO1_PASSWORD", "fake-pass-1")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
    let policy = VenuePolicy::default().declare("dukascopy", VenueMode::Demo);
    let mount_policy = vike_mount::MountPolicy { venues: policy, ..Default::default() };
    assert_eq!(vike_mount::unaddressable_accounts_message(&vars, Some(&mount_policy)), None);
}

/// A caller that threads no policy has no `[accounts]` table and no ceiling, so it can have named
/// nothing — and must not be handed a message about accounts it never declared.
#[test]
fn no_policy_says_nothing() {
    assert_eq!(vike_mount::unaddressable_accounts_message(&HashMap::new(), None), None);
}
