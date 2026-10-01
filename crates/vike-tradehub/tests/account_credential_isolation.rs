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
//!   `crates/bridges/fxcm/src/config.rs`, and the mount decision's in
//!   `a_labelled_account_reads_only_its_own_keys` in `crates/bridges/fxcm/src/mount_tests.rs`.
//! * **dukascopy** was the one venue still REFUSED until 2026-09-15 and is not any more — but it
//!   cannot be a row in the 2×2 above, because the fact that separates its two accounts is not in
//!   the vars map at all. Its mount resolves an account from the settings database's `account` table
//!   (`crates/bridges/dukascopy/src/mount.rs`: the row's credential-key OWNER PREFIX picks the
//!   broker), so the isolation property here — *a labelled account never reads the default
//!   account's keys* — is held by that bridge's `src/mount_tests.rs`, which drives the resolution
//!   over planted `Accounts` and `AccountKeys` values and asserts that an account the store cannot
//!   identify is REFUSED rather than coerced onto `DUKASCOPY_DEMO1_*`. The projection half is
//!   `account_fanout.rs`'s `a_labelled_account_of_a_store_that_cannot_name_it_produces_no_engine`,
//!   which pins that a box with no `account` table mounts its default account and nothing else —
//!   byte-identically to the old refusal.
//! * **ibkr** left this file with its feature (the venue mount contract,
//!   `docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md`): the default build
//!   this file runs in compiles no ibkr bridge and registers the venue `FeatureAbsent`, so there is
//!   no ibkr mount here to hold to the 2×2. The same 2×2 runs through the real projection in
//!   `crates/vike-tradehub/tests/ibkr_mount.rs`'s `a_labelled_ibkr_account_reads_only_its_own_keys`
//!   (built by the `ibkr` CI lane) and through the loader in
//!   `crates/bridges/vike-ibkr/src/mount_tests.rs`'s `a_labelled_account_reads_only_its_own_keys`.
//! * **polymarket** left this file with its feature (the venue mount contract,
//!   `docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md`): the default build
//!   this file runs in compiles no polymarket bridge and registers the venue `FeatureAbsent`. The
//!   same 2×2 runs through the real projection in
//!   `crates/vike-tradehub/tests/polymarket_mount.rs`'s
//!   `a_labelled_polymarket_account_reads_only_its_own_key` (built by the `polymarket-stack` CI
//!   lane) and through the loader in
//!   `crates/bridges/polymarket/src/exec_plane/mount_contract_tests.rs`'s
//!   `a_labelled_account_reads_only_its_own_keys`.
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

use vike_config::{ArmingBlock, VenueMode, VenuePolicy};
use vike_model::account_keys::{AccountLabel, account_key};
use vike_tradehub::registry::REGISTRY;

/// The label every case uses.
const LABEL: &str = "ALT";

fn alt() -> AccountLabel {
    AccountLabel::parse(LABEL).expect("a legal label")
}

/// One venue's fixture: the credential keys that arm it, and the ceiling its arm needs.
struct Case {
    venue: &'static str,
    /// The ceiling to declare for BOTH the venue and the labelled account.
    ceiling: VenueMode,
    /// The DEFAULT account's minimal arming key set — exactly what that venue's loader requires and
    /// nothing more, so a loader that started demanding a new key fails loudly rather than silently
    /// reading a fixture that happens to carry it.
    keys: &'static [(&'static str, &'static str)],
    /// What the row says when that account IS armed.
    armed: VenueMode,
}

/// Every venue whose arm this projection can actually exercise. See the module doc for the three
/// that are covered elsewhere and why.
fn cases() -> Vec<Case> {
    vec![
        // Aster: the agent-wallet pair. DEMO reads the `TESTNET` tier token, which is outside
        // `vike_model::credential_keys::CREDENTIAL_TIERS` — a shape the grammar handles by never
        // re-parsing the base.
        Case {
            venue: "aster",
            ceiling: VenueMode::Demo,
            keys: &[("ASTER_TESTNET_USER", "0xmaster"), ("ASTER_TESTNET_PRIVATE_KEY", "0xkey")],
            armed: VenueMode::Demo,
        },
        // Hyperliquid: the private key alone is the live gate (`_ACCOUNT_ADDRESS` is optional).
        // Under a `demo` ceiling `Env::for_ceiling` cannot select mainnet (decision 0095: no
        // variable is read anywhere on this path), so this case is independent of the environment.
        Case {
            venue: "hyperliquid",
            ceiling: VenueMode::Demo,
            keys: &[("HYPERLIQUID_DEMO_PRIVATE_KEY", "0xkey")],
            armed: VenueMode::Demo,
        },
        // Alpaca: the OAuth pair plus the PINNED account id — the field that makes borrowing the
        // default account's credentials place orders in the first account's book.
        Case {
            venue: "alpaca",
            ceiling: VenueMode::Demo,
            keys: &[
                ("ALPACA_SANDBOX_CLIENT_ID", "cid"),
                ("ALPACA_SANDBOX_CLIENT_SECRET", "csecret"),
                ("ALPACA_SANDBOX_ACCOUNT_ID", "acct-1"),
            ],
            armed: VenueMode::Demo,
        },
        // IG: all three secrets required; `_IDENTIFIER` is one of the multi-word suffixes that
        // forced the label to the END of the grammar.
        Case {
            venue: "ig",
            ceiling: VenueMode::Demo,
            keys: &[
                ("IG_DEMO_API_KEY", "k"),
                ("IG_DEMO_IDENTIFIER", "user"),
                ("IG_DEMO_PASSWORD", "pw"),
            ],
            armed: VenueMode::Demo,
        },
        // OANDA: `_ACCOUNT_ID` is THE key the label grammar was designed around, and the one whose
        // borrowing would place a second account's orders in the first account's book.
        Case {
            venue: "oanda",
            ceiling: VenueMode::Demo,
            keys: &[("OANDA_DEMO_API_KEY", "tok"), ("OANDA_DEMO_ACCOUNT_ID", "101-004-1-001")],
            armed: VenueMode::Demo,
        },
        // cTrader: the per-tier OAuth grant AND the app registration, both labelled — this loader
        // takes no fallback for the app pair, deliberately (its own doc argues why).
        Case {
            venue: "ctrader",
            ceiling: VenueMode::Demo,
            keys: &[
                ("CTRADER_CLIENT_ID", "cid"),
                ("CTRADER_CLIENT_SECRET", "csecret"),
                ("CTRADER_DEMO_ACCESS_TOKEN", "at"),
                ("CTRADER_DEMO_REFRESH_TOKEN", "rt"),
            ],
            armed: VenueMode::Demo,
        },
    ]
}

/// The store holding this case's key set for ONE account — the DEFAULT account's names verbatim, or
/// each of them with the label appended.
fn store(case: &Case, label: &AccountLabel) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for (k, v) in case.keys {
        m.insert(account_key(k, label), (*v).to_string());
    }
    m
}

/// ⚠ A [`vike_mount::MountPolicy`], because that is what the projection takes: its `accounts` field
/// carries the settings database's `account` table, which dukascopy's row resolves an account out
/// of. UNREAD here, which is a box with no settings database — the state every case in this table
/// describes.
fn policy(case: &Case) -> vike_mount::MountPolicy {
    vike_mount::MountPolicy {
        venues: VenuePolicy::default().declare(case.venue, case.ceiling).declare_account(
            case.venue,
            &alt(),
            case.ceiling,
        ),
        ..Default::default()
    }
}

/// The row `venue_account_arming` produces for one account.
fn row(
    case: &Case,
    label: &AccountLabel,
    store: &HashMap<String, String>,
) -> vike_config::VenueArming {
    let rows = vike_mount::venue_account_arming(REGISTRY, case.venue, store, Some(&policy(case)));
    rows.into_iter()
        .find(|r| r.label == *label)
        .unwrap_or_else(|| panic!("{}: no row for {label}", case.venue))
}

/// The fixture is only as good as its own premise: a key set that does NOT arm its venue would make
/// every assertion below vacuously true (`NoCredentials` everywhere, all four cells agreeing).
#[test]
fn every_fixture_actually_arms_its_venue() {
    let cases = cases();
    assert!(cases.len() >= 6, "only {} cases — the table is broken", cases.len());
    for case in &cases {
        let r = row(case, &AccountLabel::Default, &store(case, &AccountLabel::Default));
        assert_eq!(
            r.effective, case.armed,
            "{}: the fixture key set must ARM the default account, or this file proves nothing \
             ({:?})",
            case.venue, r.block
        );
    }
}

/// **THE mutation detector.** A store holding ONLY the DEFAULT account's credentials must leave the
/// LABELLED account unarmed — `NoCredentials`, exactly as it reads on a venue with no keys at all.
///
/// This is the cell that goes wrong if a bridge loader stops appending the label: the arm would
/// resolve the default account's key set for a labelled mount and build a SECOND live client on it.
/// ⚠ It ACCUMULATES rather than panicking at the first venue, deliberately. The failure mode this
/// guards is one shared mistake replicated across nine independent loaders (a `*_for_account`
/// reverted, an `account_var` swapped back for a `vars.get`), and a test that stops at `alpaca`
/// reports one venue for a defect that is in all nine — so the reader fixes one and re-runs, nine
/// times. Naming every offender in one message is the difference between a table and a treadmill.
#[test]
fn a_labelled_account_reads_none_of_the_default_accounts_credentials() {
    let mut leaked: Vec<String> = Vec::new();
    for case in &cases() {
        let r = row(case, &alt(), &store(case, &AccountLabel::Default));
        if r.effective != VenueMode::Paper || r.block != ArmingBlock::NoCredentials {
            leaked.push(format!("{} ({:?}/{:?})", case.venue, r.effective, r.block));
        }
    }
    assert!(
        leaked.is_empty(),
        "these venues armed a LABELLED account off the DEFAULT account's keys — each is a second \
         live client signing for the first account: {}",
        leaked.join(", ")
    );
}

/// The mirror: a store holding ONLY the LABELLED credentials arms the LABELLED account and leaves
/// the DEFAULT one unarmed.
///
/// The second half catches a loader that appended the label unconditionally (losing the default
/// account); the first half catches one that ignores the label entirely, since an ignored label
/// could never find these keys.
///
/// Accumulating, for the reason its twin above is.
#[test]
fn a_labelled_key_set_arms_the_labelled_account_and_only_it() {
    let mut wrong: Vec<String> = Vec::new();
    for case in &cases() {
        let s = store(case, &alt());
        let labelled = row(case, &alt(), &s);
        if labelled.effective != case.armed {
            wrong.push(format!(
                "{}: labelled account NOT armed by its own keys ({:?})",
                case.venue, labelled.block
            ));
        }
        let default = row(case, &AccountLabel::Default, &s);
        if default.effective != VenueMode::Paper || default.block != ArmingBlock::NoCredentials {
            wrong.push(format!(
                "{}: the DEFAULT account armed off a LABELLED key set ({:?}/{:?})",
                case.venue, default.effective, default.block
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// **BYTE-IDENTITY, per venue.** The DEFAULT account's row is EQUAL with and without a labelled key
/// set sitting beside it in the same store.
///
/// An EQUALITY against the account-free store rather than a pin of today's answer: that is what
/// makes it survive a legitimate change to a venue's arming rules while still failing the instant a
/// second account's presence perturbs the first.
///
/// ⚠ Only the DEFAULT account's row is compared, and that is what this test is FOR: the claim is
/// "a second account's keys perturb nothing about the first", which is the byte-identity a
/// single-account box depends on. The labelled account's own row is a different claim, made by
/// `account_fanout.rs`.
#[test]
fn a_labelled_key_set_changes_nothing_about_the_default_accounts_row() {
    let mut perturbed: Vec<String> = Vec::new();
    for case in &cases() {
        let alone = store(case, &AccountLabel::Default);
        let mut beside = alone.clone();
        for (k, v) in case.keys {
            beside.insert(account_key(k, &alt()), format!("{v}-second-account"));
        }

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
///
/// Incompleteness is the interesting half: a loader that took a fallback for *some* of its keys
/// (cTrader's app pair is the standing temptation) would let a half-written labelled account
/// resolve, and the two sides would disagree here.
#[test]
fn an_absent_or_half_written_labelled_account_reads_like_a_half_written_default_one() {
    for case in &cases() {
        // ABSENT: an empty store.
        let empty = HashMap::new();
        assert_eq!(
            row(case, &alt(), &empty).block,
            row(case, &AccountLabel::Default, &empty).block,
            "{}: an unconfigured labelled account must read like an unconfigured default one",
            case.venue
        );

        // HALF-WRITTEN: every key but the last. A single-key venue has no half, so skip it.
        if case.keys.len() < 2 {
            continue;
        }
        let partial: Vec<(&str, &str)> = case.keys[..case.keys.len() - 1].to_vec();
        let mut labelled = HashMap::new();
        for (k, v) in &partial {
            labelled.insert(account_key(k, &alt()), (*v).to_string());
        }
        let mut default = HashMap::new();
        for (k, v) in &partial {
            default.insert((*k).to_string(), (*v).to_string());
        }
        assert_eq!(
            row(case, &alt(), &labelled).block,
            row(case, &AccountLabel::Default, &default).block,
            "{}: a half-written labelled account must read like a half-written default one",
            case.venue
        );
    }
}
