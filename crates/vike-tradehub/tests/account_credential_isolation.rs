//! **Every venue that says YES to a labelled account reads ITS OWN account's credentials** — the
//! answer is each venue's declaration's `addresses_accounts`, every venue being ported to the mount
//! contract (docs/decisions/0096; `vike_mount`'s hand-written `arm_addresses_accounts` answered for
//! a legacy arm until the last one went). This is the gate behind that answer, and the one that
//! fails if a bespoke loader stops being account-aware.
//!
//! ⚠ Every `vike_model::VENUES` id says yes now, so "every venue that says yes" is the whole
//! roster; the three that are covered somewhere else rather than here are named at the end of this
//! doc, each with the reason it cannot be driven through this projection.
//!
//! # Why this file exists rather than a claim in a doc comment
//!
//! `addresses_accounts` is written by hand in each bridge — as the `arm_addresses_accounts` list
//! was before it — and it CANNOT be derived: the fact it states is "does this mount thread the
//! account label into its loader", which no reflection can see. A venue declaring it whose mount
//! still reads the UNLABELLED key names would mount a SECOND live client on the FIRST account's
//! credentials — two engines, one venue account, real orders signed by the wrong key. Nothing about
//! that is visible at compile time: the mount still compiles, the screen still shows a row, and the
//! divergence appears only on a box that actually holds two key sets.
//!
//! So the declaration is held honest behaviourally, through the REAL projection
//! (`vike_mount::venue_account_arming`, the same function `make_engine_accounts` selects with), over
//! a 2×2 of stores per venue:
//!
//! |                          | default account's row | labelled account's row |
//! |---|---|---|
//! | **only the DEFAULT key set present**  | ARMED                  | `NoCredentials` — THE mutation detector |
//! | **only the LABELLED key set present** | `NoCredentials`        | ARMED |
//!
//! The top-right cell is the one that catches a reverted loader: an arm reading unlabelled names
//! would arm the labelled account off the default account's keys, and that cell would go green in
//! the wrong direction. The bottom-left cell catches the mirror mistake — a loader that appended the
//! label unconditionally and so lost the default account.
//!
//! # …and the BYTE-IDENTITY half
//!
//! [`a_labelled_key_set_changes_nothing_about_the_default_accounts_row`] asserts the DEFAULT
//! account's whole row is EQUAL with and without a labelled key set beside it. That is an equality
//! against the account-free store rather than a pin of what the row currently says, so it stays
//! true when a venue's arming rules legitimately change and fails the moment a second account's
//! presence perturbs the first.
//!
//! # What is NOT here, and where it is instead
//!
//! * **fxcm** cannot be reached through this projection at all: the default build this file runs in
//!   registers fxcm `FeatureAbsent`. Its 2×2 is a unit test on the loader itself, in
//!   `a_labelled_account_reads_its_own_credentials_and_only_its_own` (removed with the tiered credential names; formerly in `config_tests.rs`), and the mount decision's in
//!   `a_labelled_account_reads_only_its_own_keys` in `crates/bridges/fxcm/src/mount_tests.rs`.
//! * **dukascopy** cannot be a row in the 2×2 above, because it admits ONE sidecar per process. Its
//!   mount resolves an account from the settings database's `account` table
//!   (`crates/bridges/dukascopy/src/mount.rs`: the ONE active row of its label and tier), so the
//!   isolation property here — *a labelled account never reads the default account's credentials* —
//!   is held by that bridge's `src/mount_tests.rs`, which drives the resolution over planted
//!   `Accounts` values and asserts that an account the store cannot identify is REFUSED rather
//!   than coerced onto another account's credentials. The projection half is
//!   `account_fanout.rs`'s `a_labelled_account_of_a_store_that_cannot_name_it_produces_no_engine`,
//!   which pins that a box with no `account` table mounts its default account and nothing else —
//!   byte-identically to the old refusal.
//! * **ibkr** left this file with its feature (the venue mount contract,
//!   `docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md`): the default build
//!   this file runs in compiles no ibkr bridge and registers the venue `FeatureAbsent`, so there is
//!   no ibkr mount here to hold to the 2×2. The same 2×2 runs through the real projection in
//!   `crates/vike-tradehub/tests/ibkr_mount.rs`'s `a_labelled_ibkr_account_reads_only_its_own_keys`
//!   (built by the `ibkr` CI lane) and through the loader in
//!   `crates/bridges/vike-ibkr/src/mount/tests/demo.rs`'s `a_labelled_account_reads_only_its_own_keys`.
//! * **polymarket** left this file with its feature (the venue mount contract,
//!   `docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md`): the default build
//!   this file runs in compiles no polymarket bridge and registers the venue `FeatureAbsent`. The
//!   same 2×2 runs through the real projection in
//!   `crates/vike-tradehub/tests/polymarket_mount.rs`'s
//!   `a_labelled_polymarket_account_reads_only_its_own_key` (built by `ci.yml`'s `tradehub-all`
//!   job) and through the loader in
//!   `a_labelled_account_reads_only_its_own_keys` (removed with the tiered credential names; formerly in `mount_contract_tests.rs`).
//!
//! Every case is NETWORK-FREE by construction: `venue_account_arming` is pure — it reads the
//! caller's map through each arm's config loader and opens no socket.
//!
//! Moved from `crates/vike-mount/tests/` when the venue mount contract finished
//! (docs/decisions/0096): it drives `vike-mount`'s public fold with real venue ids, which only a
//! crate holding the registry can — `vike-tradehub` since the 2026-09-29 amendment. Default build
//! only: `vike-mount`'s registry carried ibkr, fxcm and polymarket `FeatureAbsent` in every build
//! and these assertions were written against that; each venue's feature-on half is its own
//! `crates/vike-tradehub/tests/ibkr_mount.rs`, `crates/vike-tradehub/tests/fxcm_mount.rs` or
//! `crates/vike-tradehub/tests/polymarket_mount.rs`. That crate-level `#![cfg]` also makes this its
//! own test binary rather than a `daemon` member.
#![cfg(not(any(feature = "ibkr", feature = "polymarket", feature = "fxcm")))]

use std::collections::HashMap;

use vike_config::ArmingBlock;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub::registry::REGISTRY;

/// The label every case uses.
const LABEL: &str = "ALT";

fn alt() -> AccountLabel {
    AccountLabel::parse(LABEL).expect("a legal label")
}

/// One venue's fixture: the credential FIELDS that arm it, and the account tier its arm needs.
struct Case {
    venue: &'static str,
    /// The tier of the active `account` row planted for BOTH the default and the labelled account.
    tier: AccountTier,
    /// One account's minimal arming field set — exactly what that venue's loader requires and
    /// nothing more, so a loader that started demanding a new field fails loudly rather than
    /// silently reading a fixture that happens to carry it.
    keys: &'static [(&'static str, &'static str)],
    /// The venue's APPLICATION fields (cTrader's OAuth client), filed once against the venue and
    /// shared by every one of its accounts — present in every store below.
    venue_keys: &'static [(&'static str, &'static str)],
    /// What the row says when that account IS armed.
    armed: AccountTier,
}

/// Every venue whose arm this projection can actually exercise. See the module doc for the three
/// that are covered elsewhere and why.
fn cases() -> Vec<Case> {
    vec![
        // Aster: the agent-wallet pair.
        Case {
            venue: "aster",
            tier: AccountTier::Demo,
            keys: &[("USER", "0xmaster"), ("PRIVATE_KEY", "0xkey")],
            venue_keys: &[],
            armed: AccountTier::Demo,
        },
        // Hyperliquid: the private key alone is the live gate (`ACCOUNT_ADDRESS` is optional).
        Case {
            venue: "hyperliquid",
            tier: AccountTier::Demo,
            keys: &[("PRIVATE_KEY", "0xkey")],
            venue_keys: &[],
            armed: AccountTier::Demo,
        },
        // Alpaca: the OAuth pair plus the PINNED account id — the field that makes borrowing the
        // default account's credentials place orders in the first account's book.
        Case {
            venue: "alpaca",
            tier: AccountTier::Demo,
            keys: &[("CLIENT_ID", "cid"), ("CLIENT_SECRET", "csecret"), ("ACCOUNT_ID", "acct-1")],
            venue_keys: &[],
            armed: AccountTier::Demo,
        },
        // IG: all three secrets required.
        Case {
            venue: "ig",
            tier: AccountTier::Demo,
            keys: &[("API_KEY", "k"), ("IDENTIFIER", "user"), ("PASSWORD", "pw")],
            venue_keys: &[],
            armed: AccountTier::Demo,
        },
        // OANDA: `ACCOUNT_ID` is the field whose borrowing would place a second account's orders in
        // the first account's book.
        Case {
            venue: "oanda",
            tier: AccountTier::Demo,
            keys: &[("API_KEY", "tok"), ("ACCOUNT_ID", "101-004-1-001")],
            venue_keys: &[],
            armed: AccountTier::Demo,
        },
        // cTrader: the OAuth grant is the ACCOUNT's, the app registration is the VENUE's.
        Case {
            venue: "ctrader",
            tier: AccountTier::Demo,
            keys: &[("ACCESS_TOKEN", "at"), ("REFRESH_TOKEN", "rt")],
            venue_keys: &[("CLIENT_ID", "cid"), ("CLIENT_SECRET", "csecret")],
            armed: AccountTier::Demo,
        },
    ]
}

/// What each account of the store holds: the DEFAULT account's fields and ALT's, either of which
/// may be empty (the account row exists and holds nothing).
#[derive(Default, Clone)]
struct Held {
    default: Vec<(&'static str, String)>,
    alt: Vec<(&'static str, String)>,
}

/// A store in which the DEFAULT account holds the case's fields.
fn default_only(case: &Case) -> Held {
    Held {
        default: case.keys.iter().map(|(k, v)| (*k, (*v).to_string())).collect(),
        alt: Vec::new(),
    }
}

/// A store in which ALT holds the case's fields.
fn alt_only(case: &Case) -> Held {
    Held {
        default: Vec::new(),
        alt: case.keys.iter().map(|(k, v)| (*k, (*v).to_string())).collect(),
    }
}

/// ⚠ A [`vike_mount::MountPolicy`], because that is what the projection takes: its `accounts` field
/// carries the settings database's `account` table, which states each account's tier, and
/// the credentials each ROW holds. Here it holds one active row per account — the DEFAULT one and
/// ALT — both at the case's tier, so the only thing that can arm or not arm an account is which
/// FIELDS its row holds.
fn policy(case: &Case, held: &Held) -> vike_mount::MountPolicy {
    let pairs =
        |fields: &[(&'static str, String)]| -> Vec<(&'static str, String)> { fields.to_vec() };
    let mut p = vike_mount::MountPolicy::default()
        .with_account(case.venue, &AccountLabel::Default, case.tier)
        .with_account(case.venue, &alt(), case.tier)
        .with_venue_credentials(case.venue, case.venue_keys);
    for (label, fields) in
        [(AccountLabel::Default, pairs(&held.default)), (alt(), pairs(&held.alt))]
    {
        if fields.is_empty() {
            continue;
        }
        let borrowed: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (*k, v.as_str())).collect();
        p = p.with_credentials(case.venue, &label, case.tier, &borrowed);
    }
    p
}

/// The row `venue_account_arming` produces for one account.
fn row(case: &Case, label: &AccountLabel, held: &Held) -> vike_config::VenueArming {
    let rows = vike_mount::venue_account_arming(
        REGISTRY,
        case.venue,
        &HashMap::new(),
        Some(&policy(case, held)),
    );
    rows.into_iter()
        .find(|r| r.label == *label)
        .unwrap_or_else(|| panic!("{}: no row for {label}", case.venue))
}

/// The fixture is only as good as its own premise: a field set that does NOT arm its venue would
/// make every assertion below vacuously true (`NoCredentials` everywhere, all four cells
/// agreeing).
#[test]
fn every_fixture_actually_arms_its_venue() {
    let cases = cases();
    assert!(cases.len() >= 6, "only {} cases — the table is broken", cases.len());
    for case in &cases {
        let r = row(case, &AccountLabel::Default, &default_only(case));
        assert_eq!(
            r.effective,
            Some(case.armed),
            "{}: the fixture field set must ARM the default account, or this file proves nothing \
             ({:?})",
            case.venue,
            r.block
        );
    }
}

/// **THE mutation detector.** A store holding ONLY the DEFAULT account's credentials must leave the
/// LABELLED account unarmed — `NoCredentials`, exactly as it reads on a venue with no keys at all.
///
/// This is the cell that goes wrong if the mount hands a labelled account the DEFAULT account's
/// fields (a credential lookup that ignores the account row): a SECOND live client on them.
/// ⚠ It ACCUMULATES rather than panicking at the first venue, deliberately: one shared mistake
/// replicated across independent loaders should be named whole in one message, not one venue per
/// re-run.
#[test]
fn a_labelled_account_reads_none_of_the_default_accounts_credentials() {
    let mut leaked: Vec<String> = Vec::new();
    for case in &cases() {
        let r = row(case, &alt(), &default_only(case));
        if r.effective.is_some_and(|t| t != AccountTier::Paper)
            || r.block != ArmingBlock::NoCredentials
        {
            leaked.push(format!("{} ({:?}/{:?})", case.venue, r.effective, r.block));
        }
    }
    assert!(
        leaked.is_empty(),
        "these venues armed a LABELLED account off the DEFAULT account's credentials — each is a \
         second live client signing for the first account: {}",
        leaked.join(", ")
    );
}

/// The mirror: a store holding ONLY the LABELLED credentials arms the LABELLED account and leaves
/// the DEFAULT one unarmed.
///
/// Accumulating, for the reason its twin above is.
#[test]
fn a_labelled_key_set_arms_the_labelled_account_and_only_it() {
    let mut wrong: Vec<String> = Vec::new();
    for case in &cases() {
        let s = alt_only(case);
        let labelled = row(case, &alt(), &s);
        if labelled.effective != Some(case.armed) {
            wrong.push(format!(
                "{}: labelled account NOT armed by its own keys ({:?})",
                case.venue, labelled.block
            ));
        }
        let default = row(case, &AccountLabel::Default, &s);
        if default.effective.is_some() || default.block != ArmingBlock::NoCredentials {
            wrong.push(format!(
                "{}: the DEFAULT account armed off a LABELLED key set ({:?}/{:?})",
                case.venue, default.effective, default.block
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// **BYTE-IDENTITY, per venue.** The DEFAULT account's row is EQUAL with and without a labelled
/// account's credentials sitting beside it in the same store.
///
/// An EQUALITY against the account-free store rather than a pin of today's answer: that is what
/// makes it survive a legitimate change to a venue's arming rules while still failing the instant a
/// second account's presence perturbs the first.
#[test]
fn a_labelled_key_set_changes_nothing_about_the_default_accounts_row() {
    let mut perturbed: Vec<String> = Vec::new();
    for case in &cases() {
        let alone = default_only(case);
        let mut beside = alone.clone();
        beside.alt = case.keys.iter().map(|(k, v)| (*k, format!("{v}-second-account"))).collect();

        let a = row(case, &AccountLabel::Default, &alone);
        let b = row(case, &AccountLabel::Default, &beside);
        if a != b {
            perturbed.push(format!("{}: {a:?} alone vs {b:?} beside a second account", case.venue));
        }
    }
    assert!(
        perturbed.is_empty(),
        "a second account's credentials changed the FIRST account's row:\n{}",
        perturbed.join("\n")
    );
}

/// **Absent and INCOMPLETE credentials behave for a labelled account exactly as they do for the
/// default one** — an equality, so this test pins no current answer and cannot rot when a venue's
/// arming rules change.
#[test]
fn an_absent_or_half_written_labelled_account_reads_like_a_half_written_default_one() {
    for case in &cases() {
        // ABSENT: both accounts hold nothing.
        let empty = Held::default();
        assert_eq!(
            row(case, &alt(), &empty).block,
            row(case, &AccountLabel::Default, &empty).block,
            "{}: an unconfigured labelled account must read like an unconfigured default one",
            case.venue
        );

        // HALF-WRITTEN: every field but the last. A single-field venue has no half, so skip it.
        if case.keys.len() < 2 {
            continue;
        }
        let partial: Vec<(&'static str, String)> =
            case.keys[..case.keys.len() - 1].iter().map(|(k, v)| (*k, (*v).to_string())).collect();
        let labelled = Held { default: Vec::new(), alt: partial.clone() };
        let default = Held { default: partial, alt: Vec::new() };
        assert_eq!(
            row(case, &alt(), &labelled).block,
            row(case, &AccountLabel::Default, &default).block,
            "{}: a half-written labelled account must read like a half-written default one",
            case.venue
        );
    }
}
