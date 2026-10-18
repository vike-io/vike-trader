//! `account_arming`'s tests: every answer the `account` table can give, one account at a time, then
//! the two shapes decisions 0119/0140 added (dukascopy's two books of one tier, ibkr's venue-wide
//! tier). They came here with the functions from `vike-tradehub-core`'s `policy_tests.rs`, which
//! drove them through `AdapterPolicy`; the rows are planted by hand now, with no policy type.

use std::collections::BTreeMap;

use super::*;
use vike_model::AccountTier;

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).expect("a legal label")
}

/// One `account` row, built by hand.
fn row(
    id: i64,
    venue: &str,
    tier: AccountTier,
    label: Option<&str>,
    active: bool,
) -> vike_secrets::Account {
    vike_secrets::Account {
        id,
        venue: venue.to_string(),
        tier,
        label: label.map(str::to_string),
        venue_account_id: None,
        parent_id: None,
        active,
        last_verified_at: None,
        max_exposure: None,
        leverage: None,
    }
}

/// A KNOWN table over `rows` (ids as given, `id`-ordered as `vike_secrets::read_accounts` returns).
fn known(mut rows: Vec<vike_secrets::Account>) -> AccountDirectory {
    rows.sort_by_key(|a| a.id);
    AccountDirectory::from_rows(vike_secrets::Accounts::Known(rows), None)
}

/// `account_tier` for the DEFAULT account of `venue`.
fn tier_of(accounts: Option<&AccountDirectory>, venue: &str) -> Result<AccountTier, ArmingBlock> {
    account_tier(accounts, venue, &AccountLabel::Default)
}

/// **Every answer the account table can give, one account at a time.** Every state but one is a
/// STOP with its own block (decision 0156: the account does not run): exactly one tier among the
/// ACTIVE rows of `(venue, label)` — a `paper` row never conflicts — is the tier it runs at.
#[test]
fn the_account_table_answers_each_accounts_tier() {
    use ArmingBlock as Block;
    let stops = |block: Block| Err::<AccountTier, Block>(block);

    // Nobody asked: no directory, an UNREAD table, a box with no store.
    assert_eq!(tier_of(None, "bybit"), stops(Block::NoAccountRow), "no directory");
    assert_eq!(tier_of(Some(&AccountDirectory::unread()), "bybit"), stops(Block::NoAccountRow));
    let no_store = AccountDirectory::from_rows(
        vike_secrets::Accounts::Unanswerable(vike_secrets::NoAccountTable::NoStore {
            db: std::path::PathBuf::from("planted/vike.db"),
        }),
        None,
    );
    assert_eq!(tier_of(Some(&no_store), "bybit"), stops(Block::NoAccountRow), "no store");

    // A store that EXISTS and would not open: loud at the root, not running here, its own block.
    type Keys = Option<BTreeMap<i64, vike_secrets::AccountKeys>>;
    let unreadable = AccountDirectory::read(
        Err::<vike_secrets::Accounts, &str>("database is locked"),
        Err::<Keys, &str>("database is locked"),
        Err::<vike_secrets::account_credentials::CredentialBook, &str>("database is locked"),
    );
    assert_eq!(tier_of(Some(&unreadable), "bybit"), stops(Block::AccountNotInStore));

    // The table answered.
    let other_venue = known(vec![row(1, "okx", AccountTier::Live, None, true)]);
    assert_eq!(tier_of(Some(&other_venue), "bybit"), stops(Block::NoAccountRow), "no row");
    let inactive = known(vec![row(1, "bybit", AccountTier::Live, None, false)]);
    assert_eq!(tier_of(Some(&inactive), "bybit"), stops(Block::AccountInactive));
    let paper_only = known(vec![row(1, "bybit", AccountTier::Paper, None, true)]);
    assert_eq!(tier_of(Some(&paper_only), "bybit"), Ok(AccountTier::Paper));
    let demo_and_paper = known(vec![
        row(1, "bybit", AccountTier::Paper, None, true),
        row(2, "bybit", AccountTier::Demo, None, true),
    ]);
    assert_eq!(
        tier_of(Some(&demo_and_paper), "bybit"),
        Ok(AccountTier::Demo),
        "a paper row never conflicts"
    );
    let two_demo = known(vec![
        row(1, "bybit", AccountTier::Demo, None, true),
        row(2, "bybit", AccountTier::Demo, None, true),
    ]);
    assert_eq!(
        tier_of(Some(&two_demo), "bybit"),
        Ok(AccountTier::Demo),
        "two rows of ONE tier (dukascopy's two demo books)"
    );
    let demo_and_live = known(vec![
        row(1, "bybit", AccountTier::Demo, None, true),
        row(2, "bybit", AccountTier::Live, None, true),
    ]);
    assert_eq!(tier_of(Some(&demo_and_live), "bybit"), stops(Block::TierConflict), "no pick");
    let live_beside_inactive_demo = known(vec![
        row(1, "bybit", AccountTier::Demo, None, false),
        row(2, "bybit", AccountTier::Live, None, true),
    ]);
    assert_eq!(
        tier_of(Some(&live_beside_inactive_demo), "bybit"),
        Ok(AccountTier::Live),
        "an INACTIVE row never conflicts — deactivating one is the remedy"
    );
}

/// A LABELLED row answers for its label only: it arms nothing for the DEFAULT account, and the
/// DEFAULT account's rows arm nothing for it.
#[test]
fn a_labelled_row_answers_for_its_own_label_only() {
    let alt = label("ALT");
    let labelled = known(vec![row(1, "bybit", AccountTier::Live, Some("ALT"), true)]);
    assert_eq!(account_tier(Some(&labelled), "bybit", &alt), Ok(AccountTier::Live));
    assert_eq!(
        tier_of(Some(&labelled), "bybit"),
        Err(ArmingBlock::NoAccountRow),
        "a labelled row is not the default account's"
    );
    let default_only = known(vec![row(1, "bybit", AccountTier::Live, None, true)]);
    assert_eq!(
        account_tier(Some(&default_only), "bybit", &alt),
        Err(ArmingBlock::NoAccountRow),
        "the default account's row is not a labelled one's"
    );
    assert_eq!(venue_tier(Some(&default_only), "bybit"), Some(AccountTier::Live));
    assert_eq!(venue_tier(Some(&labelled), "bybit"), None, "the default account has no row");
    assert_eq!(venue_tier(None, "bybit"), None, "no directory: nothing to ask");
}

/// `known_accounts` lists every labelled row of the venue, inactive ones included, so an inactive
/// labelled account shows as `AccountInactive` rather than vanishing; the default account first
/// WHEN IT HAS A ROW, and a label the grammar refuses names no account.
#[test]
fn known_accounts_lists_every_labelled_row_of_the_venue_default_first() {
    let table = known(vec![
        row(1, "bybit", AccountTier::Demo, None, true),
        row(2, "bybit", AccountTier::Live, Some("ALT"), true),
        row(7, "bybit", AccountTier::Demo, Some("OLD"), false),
        row(8, "bybit", AccountTier::Demo, Some("ALT"), true),
        row(9, "bybit", AccountTier::Demo, Some("not a label"), true),
        row(10, "okx", AccountTier::Demo, Some("ELSEWHERE"), true),
    ]);
    assert_eq!(
        known_accounts("bybit", Some(&table)),
        vec![AccountLabel::Default, label("ALT"), label("OLD")],
        "default first, then every labelled row of THIS venue, active or not, once"
    );
    assert_eq!(
        account_tier(Some(&table), "bybit", &label("OLD")),
        Err(ArmingBlock::AccountInactive)
    );
    // Nothing else to list: a box with no table still SAYS why its default account does not run.
    assert_eq!(known_accounts("bybit", None), vec![AccountLabel::Default]);
    assert_eq!(
        known_accounts("bybit", Some(&AccountDirectory::unread())),
        vec![AccountLabel::Default]
    );
}

/// **Decision 0172: a venue whose accounts are all LABELLED lists no default account** — the CI box's
/// bybit (`DEMO` and `LIVE`, no unlabelled row) printed a phantom `NoAccountRow` line for an account
/// nobody created. The default account is still NOT RUNNING (`account_tier` is unchanged), it is
/// just not a row.
#[test]
fn a_venue_of_labelled_accounts_lists_no_default_account() {
    let table = known(vec![
        row(4, "bybit", AccountTier::Demo, Some("DEMO"), true),
        row(18, "bybit", AccountTier::Live, Some("LIVE"), true),
        row(30, "okx", AccountTier::Demo, None, true),
    ]);
    assert_eq!(
        known_accounts("bybit", Some(&table)),
        vec![label("DEMO"), label("LIVE")],
        "no unlabelled bybit row: no default account in the list"
    );
    assert_eq!(
        tier_of(Some(&table), "bybit"),
        Err(ArmingBlock::NoAccountRow),
        "…and the default account is still NOT RUNNING, asked directly"
    );
    // An INACTIVE labelled row alone is still an account (`AccountInactive`), not a reason to list
    // a default one.
    let inactive_only = known(vec![row(7, "bybit", AccountTier::Demo, Some("OLD"), false)]);
    assert_eq!(known_accounts("bybit", Some(&inactive_only)), vec![label("OLD")]);
}

/// The default account is listed, first, when the venue has an UNLABELLED row — active or not — and
/// the labelled ones follow.
#[test]
fn a_venue_with_an_unlabelled_row_lists_the_default_account_first() {
    for active in [true, false] {
        let table = known(vec![
            row(2, "bybit", AccountTier::Live, Some("ALT"), true),
            row(1, "bybit", AccountTier::Demo, None, active),
        ]);
        assert_eq!(
            known_accounts("bybit", Some(&table)),
            vec![AccountLabel::Default, label("ALT")],
            "active = {active}"
        );
    }
}

/// Nothing else to list: a venue with NO row, and a venue whose only rows carry a label the grammar
/// refuses, still answer the default account, so the answer is never empty and an empty store SAYS
/// why its default account does not run.
#[test]
fn a_venue_with_nothing_else_to_list_lists_the_default_account() {
    let elsewhere = known(vec![row(10, "okx", AccountTier::Demo, Some("ELSEWHERE"), true)]);
    assert_eq!(known_accounts("bybit", Some(&elsewhere)), vec![AccountLabel::Default]);
    let empty_table = known(Vec::new());
    assert_eq!(known_accounts("bybit", Some(&empty_table)), vec![AccountLabel::Default]);
    let refused_label = known(vec![row(9, "bybit", AccountTier::Demo, Some("not a label"), true)]);
    assert_eq!(known_accounts("bybit", Some(&refused_label)), vec![AccountLabel::Default]);
}

/// **Dukascopy's two books** (decision 0119: two rows of ONE tier are fine): the default account
/// holds a demo row per book, each naming its own `venue_account_id`, and the account arms at
/// `demo`, with both rows behind the answer so a remedy names both `--id`s. A `live` row beside
/// them is the conflict, and the remedy then names the non-paper rows only.
#[test]
fn dukascopy_two_rows_of_one_tier_arm_and_both_ids_are_named() {
    let book = |id: i64, account: &str| vike_secrets::Account {
        venue_account_id: Some(account.to_string()),
        ..row(id, "dukascopy", AccountTier::Demo, None, true)
    };
    let two_books = known(vec![book(3, "DEMO-EU"), book(4, "DEMO-US")]);
    assert_eq!(tier_of(Some(&two_books), "dukascopy"), Ok(AccountTier::Demo));
    assert_eq!(account_ids(Some(&two_books), "dukascopy", &AccountLabel::Default), vec![3, 4]);
    assert_eq!(known_accounts("dukascopy", Some(&two_books)), vec![AccountLabel::Default]);

    // A paper row beside them changes neither the tier nor the rows behind it.
    let with_paper = known(vec![
        book(3, "DEMO-EU"),
        book(4, "DEMO-US"),
        row(5, "dukascopy", AccountTier::Paper, None, true),
    ]);
    assert_eq!(tier_of(Some(&with_paper), "dukascopy"), Ok(AccountTier::Demo));
    assert_eq!(account_ids(Some(&with_paper), "dukascopy", &AccountLabel::Default), vec![3, 4]);

    // A live row is the conflict: no pick, and the remedy names the demo AND live rows, not paper.
    let conflict = known(vec![
        book(3, "DEMO-EU"),
        row(4, "dukascopy", AccountTier::Live, None, true),
        row(5, "dukascopy", AccountTier::Paper, None, true),
    ]);
    assert_eq!(tier_of(Some(&conflict), "dukascopy"), Err(ArmingBlock::TierConflict));
    assert_eq!(account_ids(Some(&conflict), "dukascopy", &AccountLabel::Default), vec![3, 4]);
}

/// **IBKR's venue-wide tier** (decision 0140): the box rides ONE session, so an unlabelled `demo`
/// row beside a labelled `live` one arms NEITHER, where for an account-scoped venue (bybit) the same
/// two rows are two accounts that both arm.
#[test]
fn ibkr_venue_scoped_conflict_holds_every_label_and_names_every_row() {
    let alt = label("ALT");
    let rows = |venue: &str| {
        known(vec![
            row(1, venue, AccountTier::Demo, None, true),
            row(2, venue, AccountTier::Live, Some("ALT"), true),
        ])
    };

    // The control: bybit is account-scoped, so each account arms at its own tier.
    let bybit = rows("bybit");
    assert_eq!(tier_of(Some(&bybit), "bybit"), Ok(AccountTier::Demo));
    assert_eq!(account_tier(Some(&bybit), "bybit", &alt), Ok(AccountTier::Live));

    // ibkr: neither arms, and the remedy names the rows of EVERY label.
    let ibkr = rows("ibkr");
    assert_eq!(tier_of(Some(&ibkr), "ibkr"), Err(ArmingBlock::TierConflict));
    assert_eq!(account_tier(Some(&ibkr), "ibkr", &alt), Err(ArmingBlock::TierConflict));
    assert_eq!(account_ids(Some(&ibkr), "ibkr", &AccountLabel::Default), vec![1, 2]);
    assert_eq!(account_ids(Some(&ibkr), "ibkr", &alt), vec![1, 2]);
    // …while the same two ids on bybit are each account's own.
    assert_eq!(account_ids(Some(&bybit), "bybit", &AccountLabel::Default), vec![1]);
    assert_eq!(account_ids(Some(&bybit), "bybit", &alt), vec![2]);

    // A `paper` row still runs the simulator whatever the others say: it opens no session.
    let beside_paper = known(vec![
        row(1, "ibkr", AccountTier::Demo, None, true),
        row(2, "ibkr", AccountTier::Live, Some("ALT"), true),
        row(3, "ibkr", AccountTier::Paper, Some("SIM"), true),
    ]);
    assert_eq!(account_tier(Some(&beside_paper), "ibkr", &label("SIM")), Ok(AccountTier::Paper));

    // An account whose OWN rows already refuse it keeps that cause (not the venue's).
    let own_inactive = known(vec![
        row(1, "ibkr", AccountTier::Demo, None, true),
        row(2, "ibkr", AccountTier::Live, Some("ALT"), true),
        row(3, "ibkr", AccountTier::Live, Some("OFF"), false),
    ]);
    assert_eq!(
        account_tier(Some(&own_inactive), "ibkr", &label("OFF")),
        Err(ArmingBlock::AccountInactive)
    );
    assert_eq!(
        account_tier(Some(&own_inactive), "ibkr", &label("NONE")),
        Err(ArmingBlock::NoAccountRow)
    );

    // Deactivating one is the remedy: the other arms.
    let remedied = known(vec![
        row(1, "ibkr", AccountTier::Demo, None, true),
        row(2, "ibkr", AccountTier::Live, Some("ALT"), false),
    ]);
    assert_eq!(tier_of(Some(&remedied), "ibkr"), Ok(AccountTier::Demo));
}

/// `account_ids` answers EMPTY for every state that names no row, and names the active rows of a
/// stated tier (a `paper` answer included) — keyed on the table's answer.
#[test]
fn account_ids_name_the_rows_behind_a_stated_tier_and_nothing_otherwise() {
    assert!(account_ids(None, "bybit", &AccountLabel::Default).is_empty(), "no directory");
    assert!(
        account_ids(Some(&AccountDirectory::unread()), "bybit", &AccountLabel::Default).is_empty()
    );
    let inactive = known(vec![row(1, "bybit", AccountTier::Live, None, false)]);
    assert!(account_ids(Some(&inactive), "bybit", &AccountLabel::Default).is_empty());
    let paper = known(vec![row(4, "bybit", AccountTier::Paper, None, true)]);
    assert_eq!(account_ids(Some(&paper), "bybit", &AccountLabel::Default), vec![4]);
    let alt = known(vec![row(5, "bybit", AccountTier::Live, Some("ALT"), true)]);
    assert!(
        account_ids(Some(&alt), "bybit", &AccountLabel::Default).is_empty(),
        "a labelled row is not the default account's"
    );
}

/// `PAPER_RUNS` is what a `paper` tier answers in a projection: the tier AND the block that says the
/// simulator, not a venue, runs it.
#[test]
fn a_paper_tier_answers_the_simulator_block() {
    assert_eq!(PAPER_RUNS, (Some(AccountTier::Paper), ArmingBlock::PaperTier));
}
