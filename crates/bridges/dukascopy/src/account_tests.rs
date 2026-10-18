use super::*;
use std::assert_matches;

fn row(id: i64, book: Option<&str>, label: Option<&str>, active: bool) -> Account {
    row_at(id, AccountTier::Demo, book, label, active)
}

fn row_at(
    id: i64,
    tier: AccountTier,
    book: Option<&str>,
    label: Option<&str>,
    active: bool,
) -> Account {
    Account {
        id,
        venue: VENUE.to_string(),
        tier,
        label: label.map(str::to_string),
        venue_account_id: book.map(str::to_string),
        parent_id: None,
        active,
        last_verified_at: None,
        max_exposure: None,
    }
}

/// The owner's shape: two demo rows with two LABELS, and the books an operator wrote against them.
fn live_store() -> Accounts {
    Accounts::Known(vec![
        row(7, Some("3709890"), Some("SWISS"), true),
        row(8, Some("3716974"), Some("EU"), true),
    ])
}

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).expect("a legal label")
}

/// **The whole point of the change**: the label and tier address the row, and each label reaches
/// its own.
#[test]
fn each_label_resolves_its_own_row() {
    let accounts = live_store();
    let eu = resolve_account(&label("EU"), AccountTier::Demo, &accounts).expect("row 8");
    assert_eq!(eu.row, 8);
    assert_eq!(eu.label.as_deref(), Some("EU"));
    assert_eq!(eu.tier, AccountTier::Demo);
    assert_eq!(eu.book.as_deref(), Some("3716974"));

    let swiss = resolve_account(&label("SWISS"), AccountTier::Demo, &accounts).expect("row 7");
    assert_eq!(swiss.row, 7);
    assert_eq!(swiss.book.as_deref(), Some("3709890"));
}

/// The TIER is part of the address: a demo label does not reach a live row of the same label.
#[test]
fn the_tier_is_part_of_the_address() {
    let accounts = Accounts::Known(vec![row_at(9, AccountTier::Live, None, Some("EU"), true)]);
    let err = resolve_account(&label("EU"), AccountTier::Demo, &accounts).expect_err("no demo EU");
    assert_matches!(err, DukascopyRefusal::NoSuchAccount { tier: AccountTier::Demo, .. }, "{err}");
    let live = resolve_account(&label("EU"), AccountTier::Live, &accounts).expect("the live row");
    assert_eq!((live.row, live.tier), (9, AccountTier::Live));
}

/// **The DEFAULT account is its unlabelled row** — resolved like any other account, never picked
/// from the labelled ones.
#[test]
fn the_default_account_resolves_its_unlabelled_row() {
    let accounts = Accounts::Known(vec![
        row(6, Some("1111111"), None, true),
        row(7, Some("3709890"), Some("SWISS"), true),
    ]);
    let default = resolve_account(&AccountLabel::Default, AccountTier::Demo, &accounts)
        .expect("the unlabelled row");
    assert_eq!((default.row, default.label.as_deref()), (6, None));

    // …and a store with only labelled rows has no default account: refused, never coerced.
    let err = resolve_account(&AccountLabel::Default, AccountTier::Demo, &live_store())
        .expect_err("no unlabelled row");
    assert_matches!(err, DukascopyRefusal::NoSuchAccount { .. }, "{err}");
}

/// A BOOK is not an address any more: an account is addressed by its label, and a
/// `venue_account_id` that happens to equal the asked label reaches nothing.
#[test]
fn a_book_is_not_an_address() {
    let accounts = Accounts::Known(vec![row(8, Some("3716974"), None, true)]);
    let err =
        resolve_account(&label("3716974"), AccountTier::Demo, &accounts).expect_err("not a label");
    assert_matches!(err, DukascopyRefusal::NoSuchAccount { .. }, "{err}");
}

/// A store with no `account` table is REFUSED for every account — the default one included.
#[test]
fn a_store_without_a_table_is_refused() {
    let files = Accounts::Unanswerable(NoAccountTable::NoStore {
        db: std::path::PathBuf::from("/p/settings/db/vike.db"),
    });
    for asked in [label("EU"), AccountLabel::Default] {
        let err = resolve_account(&asked, AccountTier::Demo, &files).expect_err("refused");
        assert_matches!(err, DukascopyRefusal::NoAccountTable { .. });
        let text = err.to_string();
        assert!(text.contains(&asked.to_string()), "{text}");
        assert!(text.contains("PAPER"), "{text}");
        assert!(text.contains("/p/settings/db/vike.db"), "{text}");
    }
}

/// An address that names no row is refused, and the refusal NAMES the rows that do exist so an
/// operator can see what they could have written.
#[test]
fn an_unknown_address_is_refused_and_lists_the_rows() {
    let err =
        resolve_account(&label("NOPE"), AccountTier::Demo, &live_store()).expect_err("refused");
    let text = err.to_string();
    assert_matches!(err, DukascopyRefusal::NoSuchAccount { .. });
    for needle in ["NOPE", "id 7", "id 8", "3709890", "3716974", "SWISS", "EU"] {
        assert!(text.contains(needle), "the refusal must carry {needle:?}: {text}");
    }
    assert!(text.contains("NOT routed to any other account"), "{text}");
    assert!(text.contains("vike-cli secrets account add --venue dukascopy"), "{text}");
}

/// An INACTIVE row is not a candidate — a retired account must not be startable by the address
/// that used to reach it.
#[test]
fn an_inactive_row_is_not_addressable() {
    let accounts = Accounts::Known(vec![
        row(7, Some("3709890"), Some("SWISS"), true),
        row(8, Some("3716974"), Some("EU"), false),
    ]);
    let err = resolve_account(&label("EU"), AccountTier::Demo, &accounts).expect_err("refused");
    assert_matches!(err, DukascopyRefusal::NoSuchAccount { .. }, "{err}");
}

/// The owner's real shape — two ACTIVE demo rows, both UNLABELLED — resolves the DEFAULT account to
/// the LOWEST account id, whatever order the rows come in. Neither is refused.
#[test]
fn two_unlabelled_demo_rows_resolve_to_the_lowest_id() {
    for rows in [
        vec![row(7, Some("3709890"), None, true), row(8, Some("3716974"), None, true)],
        vec![row(8, Some("3716974"), None, true), row(7, Some("3709890"), None, true)],
    ] {
        let account_row =
            resolve_account(&AccountLabel::Default, AccountTier::Demo, &Accounts::Known(rows))
                .expect("the default account resolves");
        assert_eq!(account_row.row, 7);
        assert_eq!(account_row.book.as_deref(), Some("3709890"));
    }
}

/// Every refusal renders a sentence that says PAPER, so no arm can be read as a startup failure
/// or as a silent fallback.
#[test]
fn every_refusal_says_it_stays_paper() {
    for refusal in [
        DukascopyRefusal::NoAccountTable { why: "w".into(), label: "L".into() },
        DukascopyRefusal::NoSuchAccount {
            label: "L".into(),
            tier: AccountTier::Demo,
            known: Vec::new(),
        },
        DukascopyRefusal::StoreUnreadable {
            label: "L".into(),
            what: "the `account` table",
            error: "e".into(),
        },
    ] {
        let text = refusal.to_string();
        assert!(text.contains("PAPER"), "{refusal:?}: {text}");
        assert!(text.contains('L'), "{refusal:?} must name the account: {text}");
        assert!(text.len() > 60, "{refusal:?}: {text}");
    }
}

fn account_row_of(row: i64, label: Option<&str>, book: Option<&str>) -> DukascopyAccount {
    DukascopyAccount {
        row,
        label: label.map(str::to_string),
        tier: AccountTier::Demo,
        book: book.map(str::to_string),
    }
}

/// ⚠ **THE ADDRESS IS THE ROW'S LABEL + TIER, and the row id is EVIDENCE** — the labelled shape
/// (`key_prefix` empty), which `vike-cli secrets confirm` resolves through the store's own
/// `UNIQUE (venue, tier, label)` index.
///
/// `account.id` is a rowid stable only within one database FILE, and a parked record outlives a
/// recreated database; keying on it would let a fold stamp a stranger's row. This is the assertion
/// that keeps that straight.
#[test]
fn a_parked_confirmation_is_addressed_by_label_and_tier_not_by_row_id() {
    let account_row = account_row_of(8, Some("EU"), Some("3716974"));
    let rec = confirmation_for(&account_row, "DEMO2cGyrc", 1_787_356_800_000)
        .expect("a labelled account parks");
    assert_eq!(rec.venue, VENUE);
    assert_eq!(rec.key_prefix, "", "the labelled shape carries no key prefix");
    assert_eq!(rec.label.as_deref(), Some("EU"), "the ADDRESS, half one");
    assert_eq!(rec.tier, Some(AccountTier::Demo), "the ADDRESS, half two");
    assert_eq!(rec.observed_row, Some(8), "the row id is carried as evidence");
    assert_eq!(rec.observed_book.as_deref(), Some("3716974"), "…and so is what the store said");
    assert_eq!(rec.handshake_account_id, "DEMO2cGyrc", "…and the venue's answer, verbatim");
    assert_eq!(rec.at_ms, 1_787_356_800_000, "the HANDSHAKE's instant, supplied by the caller");

    // The two accounts park under DIFFERENT addresses: two accounts, two books, two entries that
    // cannot overwrite each other.
    let other =
        confirmation_for(&account_row_of(7, Some("SWISS"), None), "DEMO1x", 0).expect("labelled");
    assert_ne!(rec.address(), other.address());
}

/// An UNLABELLED account has no label to be addressed by, so nothing is parked for it.
#[test]
fn an_unlabelled_account_parks_nothing() {
    assert_eq!(confirmation_for(&account_row_of(6, None, Some("1111111")), "DEMO1x", 0), None);
}

/// The three verdicts as this arm will meet them. The DISAGREEMENT row is the one that matters:
/// the store says one book, the venue answers another, and nothing is written.
#[test]
fn the_verdict_is_computed_from_the_row_the_start_actually_used() {
    use vike_secrets::account_confirmation::{Verdict, verdict};
    let learns = account_row_of(7, Some("SWISS"), None);
    assert_eq!(verdict("DEMO1x", learns.book.as_deref()), Verdict::Learns);

    let confirms = account_row_of(7, Some("SWISS"), Some("DEMO1x"));
    assert_eq!(verdict("DEMO1x", confirms.book.as_deref()), Verdict::Confirms);

    let disagrees = account_row_of(7, Some("SWISS"), Some("3709890"));
    assert_eq!(
        verdict("DEMO1x", disagrees.book.as_deref()),
        Verdict::Disagrees { stored: "3709890".to_string() },
        "a stored number against a login-shaped answer is REPORTED, not classified away"
    );
}
