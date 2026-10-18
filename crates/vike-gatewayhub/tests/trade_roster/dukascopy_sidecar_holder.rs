//! **Which Dukascopy account this process arms — over the REAL projection, on a real two-account
//! store.**
//!
//! # The defect this file exists to hold shut
//!
//! `vike-tradehub-core`'s dukascopy arm learned to address the account it is mounted for (#1845), keyed on
//! the settings database's `account` row, and the feature **could not be reached**. The fan-out
//! mounts the DEFAULT account first (`known_accounts` puts it first and `accounts_to_run`
//! preserves row order), the one-sidecar claim was first-come, and arming a labelled account
//! necessarily armed the default one too — so the default took the sidecar on every box, every
//! time, and every labelled account hit the claim refusal. On the owner's own box, holding both
//! `DUKASCOPY_DEMO1_*` and `DUKASCOPY_DEMO2_*`, there was **no setting that mounted the second
//! account**.
//!
//! `vike-tradehub-core`'s generic exclusive rule (`holder` in `crates/vike-gatewayhub/src/trade/adapters/exclusive.rs`) decides
//! the holder from the `account` TABLE, over `DukascopyVenueAdapter` in
//! `crates/bridges/dukascopy/src/adapter.rs`, before anything is mounted: a LABELLED account names
//! itself by an ACTIVE row whose `label` is its book (decision 0119 — the row's tier and `active`
//! are the arming), and the DEFAULT account yields to a labelled account that arms. Everything
//! below drives that through `venue_account_arming` — the projection the mount selects with and the
//! one `crates/vike-tradehub-core/src/node/accounts.rs`'s `refuse_unarmed_mount_accounts` judges a strategy
//! mount against.
//!
//! # Why these tests can exist at all
//!
//! The `account` table is a PARAMETER (`vike_secrets::account_directory::AccountDirectory`,
//! carried on `AdapterPolicy::accounts` and read by the composition root), where the arm used to open
//! the store itself at a path taken from a process global. So a two-account box is a value a test can
//! write down, no database on disk and no process state — which is also why the store read moved.
//!
//! Network-free and JVM-free by construction: the projection opens no socket and spawns nothing.
//!
//! Moved from `crates/vike-tradehub-core/tests/` when the venue-adapter contract finished
//! (docs/decisions/0096): it drives `vike-tradehub-core`'s public fold with real venue ids, which only a
//! crate holding the registry can — `vike-tradehub` since the 2026-09-29 amendment. Default build
//! only: `vike-tradehub-core`'s registry carried ibkr, fxcm and polymarket `FeatureAbsent` in every build
//! and these assertions were written against that; each venue's feature-on half is its own
//! `crates/vike-gatewayhub/tests/trade_roster/ibkr_adapter.rs`, `crates/vike-gatewayhub/tests/trade_roster/fxcm_adapter.rs` or
//! `crates/vike-gatewayhub/tests/trade_roster/polymarket_adapter.rs`. That crate-level `#![cfg]` also makes this its
//! own test binary rather than a `daemon` member.
#![cfg(not(any(feature = "ibkr", feature = "polymarket", feature = "fxcm")))]

// The in-process backend the roster tests drive the fold through (wave 2, W9a).
#[path = "support/local_backend.rs"]
mod local;

use std::collections::HashMap;

use vike_config::ArmingBlock;
use vike_gatewayhub::registry::trade::REGISTRY;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_credentials::CredentialBook;
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::{Account, AccountKeys, Accounts};

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
        tier: vike_model::AccountTier::Demo,
        label: if named { book.map(str::to_string) } else { None },
        venue_account_id: book.map(str::to_string),
        parent_id: None,
        active,
        last_verified_at: None,
        max_exposure: None,
    }
}

/// The credentials a row holds, by FIELD — decision 0156 item 6: a credential belongs to its
/// account, so the broker mapping that used to be a key-name prefix is the row id itself.
fn login_pair(n: u8) -> [(&'static str, String); 2] {
    [("LOGIN", format!("fake-login-{n}")), ("PASSWORD", format!("fake-pass-{n}"))]
}

/// The label a row's account is addressed by.
fn label_of(row: &Account) -> AccountLabel {
    row.label.as_deref().map_or(AccountLabel::Default, label)
}

/// The measured live shape — the DEFAULT account is the Swiss row and the second account is the
/// EU row, a row of its own. The two are two rows at tier `demo` with two LABELS (an unlabelled
/// pair would be ONE address with two answers, which the store refuses to pick between). The books
/// in `named` are given as their rows' labels: naming the Swiss book labels the row that WAS the
/// default account, so the default account then has no row.
fn policy(named: &[&str]) -> vike_tradehub_core::AdapterPolicy {
    let is_named = |book: &str| named.contains(&book);
    let mut rows = vec![row(SWISS_ROW, Some(SWISS_BOOK), is_named(SWISS_BOOK), true)];
    if is_named(EU_BOOK) {
        rows.push(row(EU_ROW, Some(EU_BOOK), true, true));
    }
    // A named account the measured rows do not carry: a row of its own, holding no credentials.
    for &stray in named {
        if ![SWISS_BOOK, EU_BOOK].contains(&stray) {
            rows.push(row(STRAY_ROW, Some(stray), true, true));
        }
    }
    with_rows(rows, &[SWISS_ROW, EU_ROW])
}

/// A policy over `rows`, in which the rows whose ids are in `holding` hold a LOGIN / PASSWORD pair.
fn with_rows(rows: Vec<Account>, holding: &[i64]) -> vike_tradehub_core::AdapterPolicy {
    let mut policy = vike_tradehub_core::AdapterPolicy::default();
    for r in &rows {
        policy = policy.with_account_row(r.clone());
    }
    for r in rows.iter().filter(|r| r.active && holding.contains(&r.id)) {
        let n = if r.id == EU_ROW { 2 } else { 1 };
        let pair = login_pair(n);
        let fields: Vec<(&str, &str)> = pair.iter().map(|(f, v)| (*f, v.as_str())).collect();
        policy = policy.with_credentials("dukascopy", &label_of(r), AccountTier::Demo, &fields);
    }
    policy
}

/// The process variables: dukascopy reads none (a credential belongs to its account), so none.
fn both_key_families() -> HashMap<String, String> {
    HashMap::new()
}

fn arming_rows(
    vars: &HashMap<String, String>,
    policy: &vike_tradehub_core::AdapterPolicy,
) -> Vec<vike_config::VenueArming> {
    crate::local::rows(REGISTRY, "dukascopy", vars, Some(policy))
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
    assert!(
        eu.effective.is_some_and(|t| t != AccountTier::Paper),
        "the named account arms: {}",
        eu.why()
    );
    assert_eq!(eu.route_key(), format!("dukascopy#{EU_BOOK}"));
    assert_eq!(eu.account_ids, [EU_ROW], "the row that named it");

    let default = find(&rows, &AccountLabel::Default);
    assert_eq!(
        default.effective,
        None,
        "the DEFAULT account must yield — one sidecar per process, and the operator named the \
         other one: {}",
        default.why()
    );
    assert_eq!(
        default.block,
        ArmingBlock::SidecarHeldElsewhere,
        "…and the row must say WHY it does not run, or it reads as a missing credential"
    );
    assert!(
        default.why().contains("ONE") && default.why().contains("dukascopy"),
        "{}",
        default.why()
    );
}

/// **…and the mount SET follows the rows**, so `crates/vike-tradehub-core/src/node/accounts.rs`'s
/// `refuse_unarmed_mount_accounts` — which reads exactly this projection — cannot pass a strategy
/// that names an account that does not run.
///
/// The DEFAULT account, which yields the sidecar, is NOT in the set: since decision 0156 an account
/// that does not run gets no engine, a paper one included.
#[test]
fn the_start_set_carries_only_the_account_that_arms() {
    let vars = both_key_families();
    let rows = arming_rows(&vars, &policy(&[EU_BOOK]));
    assert_eq!(
        vike_tradehub_core::accounts_to_run(&rows),
        vec![label(EU_BOOK)],
        "the default account yields and does not run; the named one is mounted because it ARMED"
    );
}

/// **A box that names no account is byte-identical**: its two unlabelled `demo` rows state ONE tier
/// for the DEFAULT account (several rows of the same tier are not a conflict), the default account
/// arms exactly as it has since the arm was written, and nothing is declined.
#[test]
fn a_box_that_names_no_account_still_starts_the_swiss_bank() {
    let vars = both_key_families();
    let rows = arming_rows(&vars, &policy(&[]));
    assert_eq!(rows.len(), 1, "no second account is named anywhere: {rows:?}");
    let default = find(&rows, &AccountLabel::Default);
    assert!(default.effective.is_some_and(|t| t != AccountTier::Paper), "{}", default.why());
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
            rows.iter().filter(|r| r.effective.is_some_and(|t| t != AccountTier::Paper)).collect();
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
    assert!(
        swiss.effective.is_some_and(|t| t != AccountTier::Paper),
        "the named account arms: {}",
        swiss.why()
    );
    // …and the default account has no row now that the Swiss row carries a label, so it is not
    // listed at all (decision 0172) — it cannot arm a second engine on the SAME credential family.
    assert!(
        rows.iter().all(|r| !r.is_default_account()),
        "a venue whose accounts are all labelled lists no default account: {rows:?}"
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

    // Named by an active row of its own, but that row holds no credentials: a credential belongs
    // to its account (decision 0156 item 6), so the row is the whole address and an account
    // holding nothing reads as one that holds nothing.
    let rows = arming_rows(&vars, &policy(&["9999999"]));
    assert_eq!(find(&rows, &label("9999999")).block, ArmingBlock::NoCredentials);
    assert!(
        find(&rows, &AccountLabel::Default).effective.is_some_and(|t| t != AccountTier::Paper),
        "an unresolvable account must not disarm the account that works"
    );

    // …and a real row whose credentials are absent from the store.
    let only_swiss = with_rows(
        vec![row(SWISS_ROW, Some(SWISS_BOOK), false, true), row(EU_ROW, Some(EU_BOOK), true, true)],
        &[SWISS_ROW],
    );
    let rows = arming_rows(&vars, &only_swiss);
    assert_eq!(find(&rows, &label(EU_BOOK)).block, ArmingBlock::NoCredentials);
    assert!(find(&rows, &AccountLabel::Default).effective.is_some_and(|t| t != AccountTier::Paper));
}

/// **An INACTIVE labelled row takes nothing either** — `active = 0` is the off switch for that
/// account: it reads `AccountInactive`, arms nothing, and the DEFAULT account keeps the sidecar
/// (it is NOT declined as `SidecarHeldElsewhere`), with both credential families present.
#[test]
fn an_inactive_named_account_leaves_the_sidecar_with_the_default() {
    let vars = both_key_families();
    let policy = with_rows(
        vec![
            row(SWISS_ROW, Some(SWISS_BOOK), false, true),
            row(EU_ROW, Some(EU_BOOK), true, false),
        ],
        &[SWISS_ROW, EU_ROW],
    );
    let rows = arming_rows(&vars, &policy);

    let eu = find(&rows, &label(EU_BOOK));
    assert_eq!((eu.effective, eu.block), (None, ArmingBlock::AccountInactive));
    let default = find(&rows, &AccountLabel::Default);
    assert!(default.effective.is_some_and(|t| t != AccountTier::Paper), "{}", default.why());
    assert_ne!(
        default.block,
        ArmingBlock::SidecarHeldElsewhere,
        "an inactive account must not take the sidecar from the default"
    );
    // …and the control: the SAME row active takes it.
    let active = with_rows(
        vec![row(SWISS_ROW, Some(SWISS_BOOK), false, true), row(EU_ROW, Some(EU_BOOK), true, true)],
        &[SWISS_ROW, EU_ROW],
    );
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
/// account's tier lives, so a row read that fails stops EVERY account (it does not run), the default
/// included — the safe direction, and an outage the block names. The KEY NAMES only say which broker
/// a labelled row is, so their failure refuses the labelled account and leaves the default armed.
///
/// ⚠ **The granularity this test CANNOT see is stated rather than implied.** An arming ROW carries
/// one block, and `AccountNotInStore` is the answer for every dukascopy refusal — so re-swallowing
/// the key-read error would leave this test GREEN (MEASURED, by mutating exactly that). What it
/// proves here is that a broken store does not ARM anything it cannot identify. The distinction
/// between a store failure and a bad row lives in the REFUSAL an operator reads, and
/// `crates/bridges/dukascopy/src/adapter_tests.rs`'s
/// `a_store_that_will_not_open_is_not_reported_as_a_bad_row` is what fails when it is lost.
#[test]
fn a_store_that_will_not_open_never_reads_as_an_unmappable_row() {
    let vars = both_key_families();
    let rows_of = |accounts: AccountDirectory| {
        let p = vike_tradehub_core::AdapterPolicy { accounts, ..Default::default() };
        crate::local::rows(REGISTRY, "dukascopy", &vars, Some(&p))
    };

    // The rows would not read: no account's tier can be known, so the DEFAULT account does not run
    // too, and says the store is why.
    let rows = rows_of(AccountDirectory::read(
        Err::<Accounts, _>("disk I/O error"),
        Err::<Option<std::collections::BTreeMap<i64, AccountKeys>>, _>("disk I/O error"),
        Err::<CredentialBook, _>("disk I/O error"),
    ));
    let default = find(&rows, &AccountLabel::Default);
    assert_eq!((default.effective, default.block), (None, ArmingBlock::AccountNotInStore));

    // …and the CREDENTIALS would not read (the rows did): no account can be shown to hold a
    // login, so NO account arms — the labelled one and the DEFAULT one both do not run, the safe
    // direction. (There is no key-name half any more: no credential name carries an account.)
    let rows = rows_of(AccountDirectory::read(
        Ok::<_, &str>(Accounts::Known(vec![
            row(SWISS_ROW, Some(SWISS_BOOK), false, true),
            row(EU_ROW, Some(EU_BOOK), true, true),
        ])),
        Ok::<Option<std::collections::BTreeMap<i64, AccountKeys>>, &str>(None),
        Err::<CredentialBook, _>("database is locked"),
    ));
    assert_eq!(find(&rows, &label(EU_BOOK)).effective, None);
    assert_eq!(find(&rows, &AccountLabel::Default).effective, None);
}

/// **A process that read no store arms NOTHING** (decision 0119): the tier lives in the `account`
/// table, so an UNREAD directory — every caller that threads no store — leaves the DEFAULT account
/// NOT RUNNING with `NoAccountRow`, credentials or not (decision 0156). Not running is the safe
/// answer whenever the tier cannot be read.
#[test]
fn an_unread_directory_arms_nothing() {
    let vars = both_key_families();
    let p = vike_tradehub_core::AdapterPolicy::default();
    let rows = crate::local::rows(REGISTRY, "dukascopy", &vars, Some(&p));
    assert_eq!(rows.len(), 1, "no labelled key and no row names a second account: {rows:?}");
    let default = find(&rows, &AccountLabel::Default);
    assert_eq!((default.effective, default.block), (None, ArmingBlock::NoAccountRow));
}

/// **the CI box's real shape: TWO ACTIVE `demo` rows, BOTH unlabelled (ids 7 and 8), each holding its
/// own `LOGIN`/`PASSWORD` filed against its own account id.** Nothing but the id tells them apart.
/// The DEFAULT account is the lowest id: it ARMS (not refused as ambiguous), the credentials the
/// mount hands the bridge are ITS login — never the other book's — and the projection lists one
/// account, `dukascopy`, with the account row that stated its tier. The process runs one JForex
/// sidecar either way, so the second book (id 8) runs only when it is given a label of its own,
/// and then with its own login (asserted last).
#[test]
fn two_unlabelled_demo_rows_run_the_lowest_id_with_its_own_login() {
    let mut book = CredentialBook::new();
    for (id, n) in [(SWISS_ROW, 1), (EU_ROW, 2)] {
        book.insert_account(id, "LOGIN", format!("login-of-row-{n}"));
        book.insert_account(id, "PASSWORD", format!("password-of-row-{n}"));
    }
    let policy_of = |rows: Vec<Account>| vike_tradehub_core::AdapterPolicy {
        accounts: AccountDirectory::from_rows(Accounts::Known(rows), None)
            .with_credentials(book.clone()),
        ..Default::default()
    };
    // Either insertion order: the choice is the id, not the row order.
    for rows in [
        vec![
            row(SWISS_ROW, Some(SWISS_BOOK), false, true),
            row(EU_ROW, Some(EU_BOOK), false, true),
        ],
        vec![
            row(EU_ROW, Some(EU_BOOK), false, true),
            row(SWISS_ROW, Some(SWISS_BOOK), false, true),
        ],
    ] {
        let policy = policy_of(rows);
        let rows = arming_rows(&HashMap::new(), &policy);
        assert_eq!(rows.len(), 1, "the default account is the only address: {rows:?}");
        let default = find(&rows, &AccountLabel::Default);
        assert_eq!(
            (default.effective, default.block),
            (Some(AccountTier::Demo), ArmingBlock::None),
            "{}",
            default.why()
        );
        let creds =
            policy.accounts.credentials_for("dukascopy", &AccountLabel::Default, AccountTier::Demo);
        assert_eq!(creds.account_id(), Some(SWISS_ROW));
        assert_eq!(creds.get("LOGIN"), Some("login-of-row-1"));
        assert_eq!(creds.get("PASSWORD"), Some("password-of-row-1"));
    }

    // Labelling the second book gives it its own address and its own login.
    let policy = policy_of(vec![
        row(SWISS_ROW, Some(SWISS_BOOK), false, true),
        row(EU_ROW, Some(EU_BOOK), true, true),
    ]);
    let eu = policy.accounts.credentials_for("dukascopy", &label(EU_BOOK), AccountTier::Demo);
    assert_eq!(eu.get("LOGIN"), Some("login-of-row-2"));
}
