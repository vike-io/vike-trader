use super::*;
use std::assert_matches;

fn row(id: i64, book: Option<&str>, label: Option<&str>, active: bool) -> Account {
    Account {
        id,
        venue: VENUE.to_string(),
        tier: "demo".to_string(),
        label: label.map(str::to_string),
        venue_account_id: book.map(str::to_string),
        parent_id: None,
        active,
        last_verified_at: None,
        max_exposure: None,
    }
}

/// ⚠ The two prefixes are taken from [`DukascopyAccount`] rather than spelled here, for two
/// reasons: a fixture that re-spelled them could drift from the loader it is meant to describe,
/// and `crates/vike-ops/tests/settings_secrets/settings_registry.rs` harvests env-shaped string literals out of
/// this tree and would read a hand-written `"DUKASCOPY_DEMO1_"` as a variable this crate reads.
const SWISS_KEYS: &str = DukascopyAccount::Demo1.key_prefix();
const EU_KEYS: &str = DukascopyAccount::Demo2.key_prefix();

fn keys(rows: &[(i64, &[&str])]) -> BTreeMap<i64, AccountKeys> {
    rows.iter()
        .map(|(id, prefixes)| {
            (
                *id,
                AccountKeys {
                    prefixes: prefixes.iter().map(|p| (*p).to_string()).collect(),
                    names: Vec::new(),
                },
            )
        })
        .collect()
}

/// The live shape: two unlabelled demo rows, distinguishable only by their key prefixes, with
/// the books an operator wrote against them.
fn live_store() -> (Accounts, BTreeMap<i64, AccountKeys>) {
    (
        Accounts::Known(vec![
            row(7, Some("3709890"), None, true),
            row(8, Some("3716974"), None, true),
        ]),
        keys(&[(7, &[SWISS_KEYS]), (8, &[EU_KEYS])]),
    )
}

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).expect("a legal label")
}

/// **The whole point of the change**: the row decides the broker, and the two rows decide
/// differently.
#[test]
fn each_row_maps_to_its_own_broker() {
    let (accounts, keys) = live_store();
    let eu = resolve_account(&label("3716974"), &accounts, Some(&keys)).expect("row 8");
    assert_eq!(eu.account, DukascopyAccount::Demo2);
    assert_eq!(eu.row, Some(8));
    assert_eq!(eu.book.as_deref(), Some("3716974"));

    let swiss = resolve_account(&label("3709890"), &accounts, Some(&keys)).expect("row 7");
    assert_eq!(swiss.account, DukascopyAccount::Demo1);
    assert_eq!(swiss.row, Some(7));
}

/// …and it is keyed on the PREFIX, not on the book: swap which row carries which key family and
/// the same address resolves to the other broker.
#[test]
fn the_key_prefix_decides_the_broker_not_the_book() {
    let (accounts, _) = live_store();
    let swapped = keys(&[(7, &[EU_KEYS]), (8, &[SWISS_KEYS])]);
    let mount = resolve_account(&label("3716974"), &accounts, Some(&swapped)).expect("row 8");
    assert_eq!(mount.account, DukascopyAccount::Demo1, "row 8 now owns the DEMO1 keys");
}

/// A row an operator DID name is addressable by that name too, without this function changing.
#[test]
fn a_written_label_addresses_its_row() {
    let accounts = Accounts::Known(vec![row(7, None, None, true), row(8, None, Some("EU"), true)]);
    let k = keys(&[(7, &[SWISS_KEYS]), (8, &[EU_KEYS])]);
    let mount = resolve_account(&label("EU"), &accounts, Some(&k)).expect("row 8");
    assert_eq!(mount.account, DukascopyAccount::Demo2);
    assert_eq!(mount.row, Some(8));
}

/// **The DEFAULT account is Demo1 on every store**, including one whose rows say nothing useful
/// and one that cannot be asked at all.
#[test]
fn the_default_account_is_always_the_swiss_bank() {
    let (accounts, k) = live_store();
    assert_eq!(
        resolve_account(&AccountLabel::Default, &accounts, Some(&k)).expect("default").account,
        DukascopyAccount::Demo1
    );
    // …and it carries the row, so the mount can say which one it is.
    assert_eq!(
        resolve_account(&AccountLabel::Default, &accounts, Some(&k)).expect("default").row,
        Some(7)
    );

    // A store that cannot be asked — a box with no database. Same broker, no row.
    let files = Accounts::Unanswerable(NoAccountTable::NoStore {
        db: std::path::PathBuf::from("/p/settings/db/vike.db"),
    });
    let mount = resolve_account(&AccountLabel::Default, &files, None).expect("default");
    assert_eq!(mount.account, DukascopyAccount::Demo1);
    assert_eq!(mount.row, None);
    assert_eq!(mount.book, None);

    // …and a table that exists and holds NO dukascopy row at all.
    let empty = Accounts::Known(Vec::new());
    assert_eq!(
        resolve_account(&AccountLabel::Default, &empty, None).expect("default").account,
        DukascopyAccount::Demo1
    );

    // ⚠ Even a store whose ONLY dukascopy row owns the DEMO2 keys: the default account is not
    // re-pointed at another broker by a migration nobody asked to change routing.
    let only_eu = Accounts::Known(vec![row(8, Some("3716974"), None, true)]);
    let mount = resolve_account(&AccountLabel::Default, &only_eu, Some(&keys(&[(8, &[EU_KEYS])])))
        .expect("default");
    assert_eq!(mount.account, DukascopyAccount::Demo1);
    assert_eq!(mount.row, None, "no row owns the DEMO1 keys, so none is attached");
}

/// A labelled account on a store with no `account` table is REFUSED — never coerced onto the
/// default account's broker.
#[test]
fn a_labelled_account_without_a_table_is_refused() {
    let files = Accounts::Unanswerable(NoAccountTable::NoStore {
        db: std::path::PathBuf::from("/p/settings/db/vike.db"),
    });
    let err = resolve_account(&label("3716974"), &files, None).expect_err("refused");
    assert_matches!(err, DukascopyRefusal::NoAccountTable { .. });
    let text = err.to_string();
    assert!(text.contains("3716974"), "{text}");
    assert!(text.contains("PAPER"), "{text}");
    assert!(text.contains("LEGAL ENTITIES"), "{text}");
}

/// An address that names no row is refused, and the refusal NAMES the rows that do exist so an
/// operator can see what they could have written.
#[test]
fn an_unknown_address_is_refused_and_lists_the_rows() {
    let (accounts, k) = live_store();
    let err = resolve_account(&label("9999999"), &accounts, Some(&k)).expect_err("refused");
    let text = err.to_string();
    assert_matches!(err, DukascopyRefusal::NoSuchAccount { .. });
    for needle in ["9999999", "id 7", "id 8", "3709890", "3716974", SWISS_KEYS] {
        assert!(text.contains(needle), "the refusal must carry {needle:?}: {text}");
    }
    assert!(text.contains("NOT routed to the default"), "{text}");
}

/// An INACTIVE row is not a candidate — a retired account must not be mountable by the address
/// that used to reach it.
#[test]
fn an_inactive_row_is_not_addressable() {
    let accounts = Accounts::Known(vec![
        row(7, Some("3709890"), None, true),
        row(8, Some("3716974"), None, false),
    ]);
    let k = keys(&[(7, &[SWISS_KEYS]), (8, &[EU_KEYS])]);
    let err = resolve_account(&label("3716974"), &accounts, Some(&k)).expect_err("refused");
    assert_matches!(err, DukascopyRefusal::NoSuchAccount { .. }, "{err}");
}

/// A row whose key names name neither family — or BOTH — is refused rather than guessed at.
#[test]
fn a_row_that_names_no_broker_is_refused() {
    let accounts = Accounts::Known(vec![row(8, Some("3716974"), None, true)]);

    // …no key family at all; BOTH families; and ANOTHER venue's family (built rather than
    // spelled, for the reason `SWISS_KEYS` above carries).
    let other_venue = SWISS_KEYS.replace("DUKASCOPY", "SOMEVENUE");
    for prefixes in [&[][..], &[SWISS_KEYS, EU_KEYS][..], &[other_venue.as_str()][..]] {
        let k = keys(&[(8, prefixes)]);
        let err = resolve_account(&label("3716974"), &accounts, Some(&k)).expect_err("refused");
        assert_matches!(err, DukascopyRefusal::UnmappableRow { .. }, "{err}");
        assert!(err.to_string().contains("row 8"), "{err}");
    }

    // …and a row the key reader knows nothing about at all.
    let err = resolve_account(&label("3716974"), &accounts, None).expect_err("refused");
    assert_matches!(err, DukascopyRefusal::UnmappableRow { .. }, "{err}");
}

/// Two rows answering to one address is refused, not picked.
#[test]
fn two_rows_on_one_address_are_refused() {
    let accounts = Accounts::Known(vec![
        row(7, Some("3709890"), None, true),
        row(8, None, Some("3709890"), true),
    ]);
    let k = keys(&[(7, &[SWISS_KEYS]), (8, &[EU_KEYS])]);
    let err = resolve_account(&label("3709890"), &accounts, Some(&k)).expect_err("refused");
    assert_matches!(err, DukascopyRefusal::Ambiguous { .. }, "{err}");
    assert!(err.to_string().contains("more than one"), "{err}");
}

/// Every refusal renders a sentence that says PAPER, so no arm can be read as a startup failure
/// or as a silent fallback.
#[test]
fn every_refusal_says_it_stays_paper() {
    for refusal in [
        DukascopyRefusal::NoAccountTable { why: "w".into(), label: "L".into() },
        DukascopyRefusal::NoSuchAccount { label: "L".into(), known: Vec::new() },
        DukascopyRefusal::Ambiguous { label: "L".into(), matched: Vec::new() },
        DukascopyRefusal::UnmappableRow { label: "L".into(), row: 1, prefixes: Vec::new() },
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

fn mounted(account: DukascopyAccount, row: Option<i64>, book: Option<&str>) -> DukascopyMount {
    DukascopyMount { account, row, book: book.map(str::to_string) }
}

/// ⚠ **THE ADDRESS IS THE KEY PREFIX, and the row id is EVIDENCE.**
///
/// `account.id` is a rowid stable only within one database FILE, and a parked record outlives a
/// re-migration; keying on it would let a fold stamp a stranger's row, which on this venue is
/// the wrong legal entity. This is the assertion that keeps that straight.
#[test]
fn a_parked_confirmation_is_addressed_by_key_prefix_not_by_row_id() {
    let rec = confirmation_for(
        &mounted(DukascopyAccount::Demo2, Some(8), Some("3716974")),
        "DEMO2cGyrc",
        1_787_356_800_000,
    );
    assert_eq!(rec.venue, VENUE);
    assert_eq!(rec.key_prefix, DukascopyAccount::Demo2.key_prefix(), "the ADDRESS");
    assert_eq!(rec.observed_row, Some(8), "the row id is carried as evidence");
    assert_eq!(rec.observed_book.as_deref(), Some("3716974"), "…and so is what the store said");
    assert_eq!(rec.handshake_account_id, "DEMO2cGyrc", "…and the venue's answer, verbatim");
    assert_eq!(rec.at_ms, 1_787_356_800_000, "the HANDSHAKE's instant, supplied by the caller");

    // The two accounts park under DIFFERENT addresses, which is the property the whole schema
    // needs: two credential sets, two books, two entries that cannot overwrite each other.
    let other = confirmation_for(&mounted(DukascopyAccount::Demo1, Some(7), None), "DEMO1x", 0);
    assert_ne!(rec.key_prefix, other.key_prefix);
}

/// A mount the store could not answer for still parks a record: the key prefix is known from
/// the BROKER the mount resolved, and it is the address. `observed_row` is `None` — a
/// `Backend::Absent` box, or a process that declared no project.
#[test]
fn a_row_less_mount_still_has_an_address() {
    let rec = confirmation_for(&mounted(DukascopyAccount::Demo1, None, None), "DEMO1x", 0);
    assert_eq!(rec.key_prefix, DukascopyAccount::Demo1.key_prefix());
    assert_eq!(rec.observed_row, None);
    assert_eq!(rec.observed_book, None);
}

/// The three verdicts as this arm will meet them, over the real key prefixes. The DISAGREEMENT
/// row is the one that matters: the store says one book, the venue answers another, and nothing
/// is written.
#[test]
fn the_verdict_is_computed_from_the_row_the_mount_actually_used() {
    use vike_model::accounts::account_confirmation::{Verdict, verdict};
    let learns = mounted(DukascopyAccount::Demo1, Some(7), None);
    assert_eq!(verdict("DEMO1x", learns.book.as_deref()), Verdict::Learns);

    let confirms = mounted(DukascopyAccount::Demo1, Some(7), Some("DEMO1x"));
    assert_eq!(verdict("DEMO1x", confirms.book.as_deref()), Verdict::Confirms);

    let disagrees = mounted(DukascopyAccount::Demo1, Some(7), Some("3709890"));
    assert_eq!(
        verdict("DEMO1x", disagrees.book.as_deref()),
        Verdict::Disagrees { stored: "3709890".to_string() },
        "a stored number against a login-shaped answer is REPORTED, not classified away"
    );
}
