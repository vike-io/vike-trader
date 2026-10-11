//! **Which Dukascopy account this process arms — over the REAL projection, on a real two-account
//! store.**
//!
//! # The defect this file exists to hold shut
//!
//! `vike-mount`'s dukascopy arm learned to address the account it is mounted for (#1845), keyed on
//! the settings database's `account` row, and the feature **could not be reached**. The fan-out
//! mounts the DEFAULT account first (`known_accounts` puts it first and `accounts_to_mount`
//! preserves row order), the one-sidecar claim was first-come, and arming a labelled account
//! necessarily armed the default one too — so the default took the sidecar on every box, every
//! time, and every labelled account hit the claim refusal. On the owner's own box, holding both
//! `DUKASCOPY_DEMO1_*` and `DUKASCOPY_DEMO2_*`, there was **no setting that mounted the second
//! account**.
//!
//! `vike-mount`'s generic exclusive rule (`holder` in `crates/vike-mount/src/exclusive.rs`) decides
//! the holder from the `account` TABLE, over `DukascopyVenueMount` in
//! `crates/bridges/dukascopy/src/mount.rs`, before anything is mounted: a LABELLED account names
//! itself by an ACTIVE row whose `label` is its book (decision 0119 — the row's tier and `active`
//! are the arming), and the DEFAULT account yields to a labelled account that arms. Everything
//! below drives that through `venue_account_arming` — the projection the mount selects with and the
//! one `crates/vike-mount/src/node/accounts.rs`'s `refuse_unarmed_mount_accounts` judges a strategy
//! mount against.
//!
//! # Why these tests can exist at all
//!
//! The `account` table is a PARAMETER (`vike_bridge_core::account_directory::AccountDirectory`,
//! carried on `MountPolicy::accounts` and read by the composition root), where the arm used to open
//! the store itself at a path taken from a process global. So a two-account box is a value a test can
//! write down, no database on disk and no process state — which is also why the store read moved.
//!
//! Network-free and JVM-free by construction: the projection opens no socket and spawns nothing.
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

use std::collections::{BTreeMap, HashMap};

use vike_bridge_core::account_directory::AccountDirectory;
use vike_bridge_core::credentials::{Account, AccountKeys, Accounts};
use vike_config::{ArmingBlock, VenueMode};
use vike_dukascopy::DukascopyAccount;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub::registry::REGISTRY;

const SWISS_BOOK: &str = "3709890";
const EU_BOOK: &str = "3716974";

/// The row ids of the two measured rows, and of a row no credential family is keyed to.
const SWISS_ROW: i64 = 7;
const EU_ROW: i64 = 8;
const STRAY_ROW: i64 = 9;

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).expect("a legal label")
}

/// One `(dukascopy, demo)` row carrying the book the venue gave it.
///
/// ⚠ `label` is NULL as the migration writes it (the owner refused the provisional `DEMO1`/`DEMO2`
/// spellings, so the key family is the only thing that separates two such rows — which is the whole
/// reason the arm reads it) UNLESS the operator NAMED the account: then the label is the BOOK
/// (`crates/bridges/dukascopy/src/account.rs`'s module doc), and that active labelled row is what
/// arms the labelled account.
fn row(id: i64, book: Option<&str>, named: bool, active: bool) -> Account {
    Account {
        id,
        venue: "dukascopy".to_string(),
        tier: "demo".to_string(),
        label: if named { book.map(str::to_string) } else { None },
        venue_account_id: book.map(str::to_string),
        parent_id: None,
        active,
        last_verified_at: None,
        max_exposure: None,
    }
}

/// Each measured row's credential-key OWNER PREFIX — the broker mapping. A row absent from this map
/// owns no key family.
fn key_names() -> BTreeMap<i64, AccountKeys> {
    let keys = |prefix: &str| AccountKeys { prefixes: vec![prefix.to_string()], names: Vec::new() };
    [
        (SWISS_ROW, keys(DukascopyAccount::Demo1.key_prefix())),
        (EU_ROW, keys(DukascopyAccount::Demo2.key_prefix())),
    ]
    .into_iter()
    .collect()
}

/// The measured live shape — two `(dukascopy, demo)` rows, each owning one credential family, each
/// carrying its book — with the books in `named` given as their rows' labels.
fn policy(named: &[&str]) -> vike_mount::MountPolicy {
    let is_named = |book: &str| named.contains(&book);
    let mut rows = vec![
        row(SWISS_ROW, Some(SWISS_BOOK), is_named(SWISS_BOOK), true),
        row(EU_ROW, Some(EU_BOOK), is_named(EU_BOOK), true),
    ];
    // A named account the measured rows do not carry: a row of its own, keyed to no family.
    for &stray in named {
        if ![SWISS_BOOK, EU_BOOK].contains(&stray) {
            rows.push(row(STRAY_ROW, Some(stray), true, true));
        }
    }
    with_rows(rows)
}

fn with_rows(rows: Vec<Account>) -> vike_mount::MountPolicy {
    vike_mount::MountPolicy {
        accounts: AccountDirectory::from_rows(Accounts::Known(rows), Some(key_names())),
        ..Default::default()
    }
}

/// Both credential families present — the owner's box, and the only configuration in which the
/// choice of holder decides anything.
fn both_key_families() -> HashMap<String, String> {
    [
        ("DUKASCOPY_DEMO1_LOGIN", "fake-login-1"),
        ("DUKASCOPY_DEMO1_PASSWORD", "fake-pass-1"),
        ("DUKASCOPY_DEMO2_LOGIN", "fake-login-2"),
        ("DUKASCOPY_DEMO2_PASSWORD", "fake-pass-2"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

fn arming_rows(
    vars: &HashMap<String, String>,
    policy: &vike_mount::MountPolicy,
) -> Vec<vike_config::VenueArming> {
    vike_mount::venue_account_arming(REGISTRY, "dukascopy", vars, Some(policy))
}

fn find<'a>(
    rows: &'a [vike_config::VenueArming],
    label: &AccountLabel,
) -> &'a vike_config::VenueArming {
    rows.iter()
        .find(|r| r.label == *label)
        .unwrap_or_else(|| panic!("no row for {label}: {rows:?}"))
}

/// **THE HEADLINE: there is a setting that mounts the SECOND account**, and it is one row's label.
#[test]
fn naming_the_second_account_arms_it_and_the_default_yields() {
    let vars = both_key_families();
    let rows = arming_rows(&vars, &policy(&[EU_BOOK]));

    let eu = find(&rows, &label(EU_BOOK));
    assert_ne!(eu.effective, VenueMode::Paper, "the named account arms: {}", eu.why());
    assert_eq!(eu.route_key(), format!("dukascopy#{EU_BOOK}"));
    assert_eq!(eu.account_ids, [EU_ROW], "the row that named it");

    let default = find(&rows, &AccountLabel::Default);
    assert_eq!(
        default.effective,
        VenueMode::Paper,
        "the DEFAULT account must yield — one sidecar per process, and the operator named the \
         other one: {}",
        default.why()
    );
    assert_eq!(
        default.block,
        ArmingBlock::SidecarHeldElsewhere,
        "…and the row must say WHY it is paper, or it reads as a missing credential"
    );
    assert!(
        default.why().contains("ONE") && default.why().contains("dukascopy"),
        "{}",
        default.why()
    );
}

/// **…and the mount SET follows the rows**, so `crates/vike-mount/src/node/accounts.rs`'s
/// `refuse_unarmed_mount_accounts` — which reads exactly this projection — cannot pass a strategy
/// that names an account the mount papers.
///
/// The default account is still in the set (it is the engine every caller binds at `[0]`), and its
/// engine is the paper one its row describes.
#[test]
fn the_mount_set_carries_both_and_the_projection_says_which_one_arms() {
    let vars = both_key_families();
    let rows = arming_rows(&vars, &policy(&[EU_BOOK]));
    assert_eq!(
        vike_mount::accounts_to_mount(&rows),
        vec![AccountLabel::Default, label(EU_BOOK)],
        "the default account is always mounted; the named one is mounted because it ARMED"
    );
}

/// **A box that names no account is byte-identical**: its two unlabelled `demo` rows state ONE tier
/// for the DEFAULT account (several rows of the same tier are not a conflict), the default account
/// arms exactly as it has since the arm was written, and nothing is declined.
#[test]
fn a_box_that_names_no_account_still_mounts_the_swiss_bank() {
    let vars = both_key_families();
    let rows = arming_rows(&vars, &policy(&[]));
    assert_eq!(rows.len(), 1, "no second account is named anywhere: {rows:?}");
    let default = find(&rows, &AccountLabel::Default);
    assert_ne!(default.effective, VenueMode::Paper, "{}", default.why());
    assert_ne!(default.block, ArmingBlock::SidecarHeldElsewhere);
    assert_eq!(default.route_key(), "dukascopy", "the sentinel filename does not move");
}

/// **AT MOST ONE dukascopy account ever arms** — the closure of the two-engines-on-one-account
/// hazard a bridge's `addresses_accounts` declaration guards (the since-deleted
/// `arm_addresses_accounts`' marker comment forbade joining that list with it).
///
/// The arm reads a ROW's credential family, and a labelled account can legitimately resolve to the
/// SAME family as the default account (address `3709890` and the row owning `DUKASCOPY_DEMO1_*` is
/// the same account, reached by its book). Two ARMED engines over one credential set would be two
/// engines on one venue account; one holder per process makes that unreachable rather than
/// unlikely — including in that exact case, asserted below.
#[test]
fn only_one_dukascopy_account_can_ever_arm() {
    let vars = both_key_families();
    for named in [&[][..], &[EU_BOOK][..], &[SWISS_BOOK][..], &[SWISS_BOOK, EU_BOOK][..]] {
        let rows = arming_rows(&vars, &policy(named));
        let armed: Vec<&vike_config::VenueArming> =
            rows.iter().filter(|r| r.effective != VenueMode::Paper).collect();
        assert!(
            armed.len() <= 1,
            "{named:?}: {} dukascopy accounts armed at once — two engines over one JForex sidecar, \
             and on a shared key family two engines over one ACCOUNT: {armed:?}",
            armed.len()
        );
    }
}

/// **The DEFAULT account's own credential family, addressed by its BOOK, is one account and not
/// two.** Naming `3709890` moves the engine's route key, not the broker — and the default account
/// yields, so nothing signs twice.
#[test]
fn addressing_the_default_accounts_own_row_does_not_produce_two_engines_on_it() {
    let vars = both_key_families();
    let rows = arming_rows(&vars, &policy(&[SWISS_BOOK]));
    let swiss = find(&rows, &label(SWISS_BOOK));
    assert_ne!(swiss.effective, VenueMode::Paper, "the named account arms: {}", swiss.why());
    assert_eq!(
        find(&rows, &AccountLabel::Default).effective,
        VenueMode::Paper,
        "…and the default account, which reads the SAME credential family, does not"
    );
}

/// **An account named but unarmable takes nothing from the default account.**
///
/// The holder is the account that WOULD arm on its own merits, not merely the one a row names. A
/// row labelled before `vike-cli secrets set-book` ran, a typo, a row no key family is keyed to —
/// the box keeps mounting the account it has always mounted instead of losing it to an account that
/// will not arm either.
#[test]
fn an_account_that_cannot_arm_does_not_take_the_sidecar() {
    let vars = both_key_families();

    // Named by an active row of its own, but no credential family is keyed to that row.
    let rows = arming_rows(&vars, &policy(&["9999999"]));
    assert_eq!(find(&rows, &label("9999999")).block, ArmingBlock::AccountNotInStore);
    assert_ne!(
        find(&rows, &AccountLabel::Default).effective,
        VenueMode::Paper,
        "an unresolvable account must not disarm the account that works"
    );

    // …and a real row whose credential family is absent from the store.
    let only_swiss: HashMap<String, String> = both_key_families()
        .into_iter()
        .filter(|(k, _)| !k.starts_with(DukascopyAccount::Demo2.key_prefix()))
        .collect();
    let rows = arming_rows(&only_swiss, &policy(&[EU_BOOK]));
    assert_eq!(find(&rows, &label(EU_BOOK)).block, ArmingBlock::NoCredentials);
    assert_ne!(find(&rows, &AccountLabel::Default).effective, VenueMode::Paper);
}

/// **An INACTIVE labelled row takes nothing either** — `active = 0` is the off switch for that
/// account: it reads `AccountInactive`, arms nothing, and the DEFAULT account keeps the sidecar
/// (it is NOT declined as `SidecarHeldElsewhere`), with both credential families present.
#[test]
fn an_inactive_named_account_leaves_the_sidecar_with_the_default() {
    let vars = both_key_families();
    let policy = with_rows(vec![
        row(SWISS_ROW, Some(SWISS_BOOK), false, true),
        row(EU_ROW, Some(EU_BOOK), true, false),
    ]);
    let rows = arming_rows(&vars, &policy);

    let eu = find(&rows, &label(EU_BOOK));
    assert_eq!((eu.effective, eu.block), (VenueMode::Paper, ArmingBlock::AccountInactive));
    let default = find(&rows, &AccountLabel::Default);
    assert_ne!(default.effective, VenueMode::Paper, "{}", default.why());
    assert_ne!(
        default.block,
        ArmingBlock::SidecarHeldElsewhere,
        "an inactive account must not take the sidecar from the default"
    );
    // …and the control: the SAME row active takes it.
    let active = with_rows(vec![
        row(SWISS_ROW, Some(SWISS_BOOK), false, true),
        row(EU_ROW, Some(EU_BOOK), true, true),
    ]);
    let rows = arming_rows(&vars, &active);
    assert_eq!(find(&rows, &AccountLabel::Default).block, ArmingBlock::SidecarHeldElsewhere);
}

/// **A store that will not open is reported as a STORE failure, and never as a bad row** — the
/// projection's half of the refusal `vike_dukascopy`'s `DukascopyRefusal::StoreUnreadable` renders.
///
/// Both halves of the read are carried separately, because the KEY-NAME half used to be swallowed:
/// `.ok().flatten()` turned "the store would not open" into "this row's key names name no broker",
/// sending an operator to fix a row that was never at fault.
///
/// ⚠ **The two halves now differ in what they take down** (decision 0119): the ROWS are where every
/// account's tier lives, so a row read that fails holds EVERY account at paper, the default
/// included — the safe direction, and an outage the block names. The KEY NAMES only say which broker
/// a labelled row is, so their failure refuses the labelled account and leaves the default armed.
///
/// ⚠ **The granularity this test CANNOT see is stated rather than implied.** An arming ROW carries
/// one block, and `AccountNotInStore` is the answer for every dukascopy refusal — so re-swallowing
/// the key-read error would leave this test GREEN (MEASURED, by mutating exactly that). What it
/// proves here is that a broken store does not ARM anything it cannot identify. The distinction
/// between a store failure and a bad row lives in the REFUSAL an operator reads, and
/// `crates/bridges/dukascopy/src/mount_tests.rs`'s
/// `a_store_that_will_not_open_is_not_reported_as_a_bad_row` is what fails when it is lost.
#[test]
fn a_store_that_will_not_open_never_reads_as_an_unmappable_row() {
    let vars = both_key_families();
    let rows_of = |accounts: AccountDirectory| {
        let p = vike_mount::MountPolicy { accounts, ..Default::default() };
        vike_mount::venue_account_arming(REGISTRY, "dukascopy", &vars, Some(&p))
    };

    // The rows would not read: no account's tier can be known, so the DEFAULT account is paper
    // too, and says the store is why.
    let rows = rows_of(AccountDirectory::read(
        Err::<Accounts, _>("disk I/O error"),
        Err::<Option<BTreeMap<i64, AccountKeys>>, _>("disk I/O error"),
    ));
    let default = find(&rows, &AccountLabel::Default);
    assert_eq!(
        (default.effective, default.block),
        (VenueMode::Paper, ArmingBlock::AccountNotInStore)
    );

    // …and the KEY NAMES would not read, which is the half that used to look like a bad row: the
    // labelled account cannot be identified, and the DEFAULT account — whose tier the rows state
    // and whose broker needs no key name — still arms.
    let rows = rows_of(AccountDirectory::read(
        Ok::<_, &str>(Accounts::Known(vec![
            row(SWISS_ROW, Some(SWISS_BOOK), false, true),
            row(EU_ROW, Some(EU_BOOK), true, true),
        ])),
        Err::<Option<BTreeMap<i64, AccountKeys>>, _>("database is locked"),
    ));
    assert_eq!(find(&rows, &label(EU_BOOK)).block, ArmingBlock::AccountNotInStore);
    assert_ne!(find(&rows, &AccountLabel::Default).effective, VenueMode::Paper);
}

/// **A process that read no store arms NOTHING** (decision 0119): the tier lives in the `account`
/// table, so an UNREAD directory — every caller that threads no store — holds the DEFAULT account at
/// paper with `NoAccountRow`, credentials or not. Paper is the safe default whenever the tier cannot
/// be read.
#[test]
fn an_unread_directory_arms_nothing() {
    let vars = both_key_families();
    let p = vike_mount::MountPolicy::default();
    let rows = vike_mount::venue_account_arming(REGISTRY, "dukascopy", &vars, Some(&p));
    assert_eq!(rows.len(), 1, "no labelled key and no row names a second account: {rows:?}");
    let default = find(&rows, &AccountLabel::Default);
    assert_eq!((default.effective, default.block), (VenueMode::Paper, ArmingBlock::NoAccountRow));
}
