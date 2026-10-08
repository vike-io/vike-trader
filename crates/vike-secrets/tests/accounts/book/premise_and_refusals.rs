//! PROOFS 1-3 — the premise (two rows a name cannot separate) and the refusals that stand between a typo and the wrong broker.

use vike_secrets::{Backend, DbErrorKind};

use super::fixture::{DEMO1_BOOK, DEMO2_BOOK, FIXTURE_KEYS, Fixture};
use crate::support::fake_value;

// ---------------------------------------------------------------------------------------------
// PROOF 1 — the premise: two rows a NAME cannot separate, each told which book it is
// ---------------------------------------------------------------------------------------------

/// **The two dukascopy rows start with NO book, learn DIFFERENT ones, and are still two rows.**
///
/// This is the assertion the column exists for. Before the write the pair is distinguishable only
/// by `id` — same venue, same tier, both labels `NULL` — which is exactly the state
/// `tests/account_reader.rs`'s
/// `dukascopy_is_two_rows_of_one_venue_at_one_tier_and_only_the_id_tells_them_apart` pins. After it,
/// each row carries the venue's own answer for which BOOK it trades, and the two answers differ.
///
/// ⚠ It also pins what did NOT happen: no label was invented, no row was created or removed, and
/// every other account's book is still unknown. A writer that "helpfully" filled the rest would be
/// asserting books nobody measured.
#[test]
fn the_two_dukascopy_rows_learn_different_books_and_stay_two_rows() {
    let fx = Fixture::migrated();
    let (first, second) = fx.dukascopy_ids();
    let before = fx.accounts();

    assert!(
        before.iter().all(|a| a.venue_account_id.is_none()),
        "a migrated store must start with every book unknown — §11 steps 3 and 4 are not \
         performed: {before:?}"
    );

    let a = fx.set_book(first, DEMO1_BOOK, false).expect("the first row learns its book");
    assert!(a.changed, "the first write must change the row");
    assert_eq!(a.before.id, first, "the echo must name the row that was written");
    assert_eq!(a.before.venue, "dukascopy");
    assert_eq!(a.before.venue_account_id, None, "the echo carries the PREVIOUS value");

    let b = fx.set_book(second, DEMO2_BOOK, false).expect("the second row learns its own");
    assert!(b.changed);

    assert_eq!(fx.book_of(first).as_deref(), Some(DEMO1_BOOK));
    assert_eq!(fx.book_of(second).as_deref(), Some(DEMO2_BOOK));
    assert_ne!(
        fx.book_of(first),
        fx.book_of(second),
        "the two accounts must name DIFFERENT books — that is the whole measurement"
    );

    // Nothing else moved: the same rows, the same labels, the same tiers, and every other book
    // still unknown.
    let after = fx.accounts();
    assert_eq!(after.len(), before.len(), "a write created or removed an account row");
    assert!(after.iter().all(|a| a.label.is_none()), "a label was synthesised: {after:?}");
    for row in &after {
        if row.id != first && row.id != second {
            assert_eq!(
                row.venue_account_id, None,
                "account {} learned a book nobody wrote: {row:?}",
                row.id
            );
        }
    }
}

/// **Writing the SAME book again writes nothing, and says so rather than failing.**
///
/// A script that re-asserts a known book on every run is not making a mistake, and refusing it
/// would make the second run of a correct command an error. `changed: false` is how the caller
/// tells the two apart — `vike-cli secrets set-book` prints "unchanged" and journals nothing,
/// because a ledger line for a no-op reads as a re-pointing that did not happen.
#[test]
fn writing_the_same_book_twice_writes_nothing_and_says_so() {
    let fx = Fixture::migrated();
    let (first, _) = fx.dukascopy_ids();

    assert!(fx.set_book(first, DEMO1_BOOK, false).expect("first").changed);
    let again = fx.set_book(first, DEMO1_BOOK, false).expect("the same value is not a conflict");
    assert!(!again.changed, "a repeat write must report no change");
    assert_eq!(
        again.before.venue_account_id.as_deref(),
        Some(DEMO1_BOOK),
        "the echo of a no-op still describes the row"
    );
    assert_eq!(fx.book_of(first).as_deref(), Some(DEMO1_BOOK));

    // …and the trimmed spelling of the same value is the SAME value, not a conflicting one.
    let padded = fx.set_book(first, "  4100017\t", false).expect("a pasted value is trimmed");
    assert!(!padded.changed, "a trailing space must not read as a different account");
}

// ---------------------------------------------------------------------------------------------
// PROOF 2 — ⚠ the refusal that stands between a typo and the wrong broker
// ---------------------------------------------------------------------------------------------

/// **A row that already names a DIFFERENT book is REFUSED**, and `--replace` is the operator saying
/// out loud that the stored number is the wrong one.
///
/// The failure this stops is not exotic: `--id` is an integer with no roster behind it, so a
/// mistyped one names some OTHER account, and overwriting that account's book re-points it at
/// another broker with nothing said. The refusal names the row and the number it already holds —
/// both are identifiers, not secrets — and deliberately does NOT echo the offered one.
#[test]
fn a_row_that_names_a_different_book_is_refused_until_replace_says_so() {
    let fx = Fixture::migrated();
    let (first, _) = fx.dukascopy_ids();
    fx.set_book(first, DEMO1_BOOK, false).expect("the row learns its book");

    let err = fx.set_book(first, DEMO2_BOOK, false).expect_err("a different book must be refused");
    match &err.kind {
        DbErrorKind::BookAlreadyKnown { id, venue, current, .. } => {
            assert_eq!(*id, first);
            assert_eq!(venue, "dukascopy");
            assert_eq!(current, DEMO1_BOOK);
        }
        other => panic!("expected BookAlreadyKnown, got {other:?}"),
    }
    let text = err.to_string();
    assert!(text.contains("BROKER"), "the refusal must say what is at stake: {text}");
    assert!(text.contains("--replace"), "…and how to proceed deliberately: {text}");
    assert!(!text.contains(DEMO2_BOOK), "the OFFERED value must not be echoed back: {text}");
    assert_eq!(fx.book_of(first).as_deref(), Some(DEMO1_BOOK), "a refused write changed the row");

    // …and with the operator saying so, it lands, carrying the previous value in the echo so the
    // change is reviewable.
    let done = fx.set_book(first, DEMO2_BOOK, true).expect("--replace permits it");
    assert!(done.changed);
    assert_eq!(done.before.venue_account_id.as_deref(), Some(DEMO1_BOOK));
    assert_eq!(fx.book_of(first).as_deref(), Some(DEMO2_BOOK));
}

/// **Ruling 11 (§8): two ACTIVE accounts of one venue may not name one book** — and the refusal
/// names both rows rather than arriving as `UNIQUE constraint failed`.
///
/// The hazard is concrete: two engines on one ledger each read *"I hold 1"* while the venue holds
/// 2, and reconcile's auto-applied `PositionDrift` then rewrites each engine's local size onto a
/// total that includes the other's.
#[test]
fn two_active_accounts_of_one_venue_may_not_name_one_book() {
    let fx = Fixture::migrated();
    let (first, second) = fx.dukascopy_ids();
    fx.set_book(first, DEMO1_BOOK, false).expect("the first row learns its book");

    let err =
        fx.set_book(second, DEMO1_BOOK, false).expect_err("one book, two active accounts: refuse");
    match &err.kind {
        DbErrorKind::BookHeldByAnother { id, venue, holder } => {
            assert_eq!(*id, second);
            assert_eq!(*holder, first, "the refusal must name the row that already holds it");
            assert_eq!(venue, "dukascopy");
        }
        other => panic!("expected BookHeldByAnother, got {other:?}"),
    }
    assert_eq!(fx.book_of(second), None, "a refused write changed the row");

    // ⚠ `--replace` is NOT a way past this one, and that is deliberate: it permits re-pointing a
    // row whose OWN book is wrong, never two rows onto one book.
    fx.set_book(second, "3716000", false).expect("the second row learns a book of its own");
    let err =
        fx.set_book(second, DEMO1_BOOK, true).expect_err("--replace must not defeat ruling 11");
    assert!(
        matches!(err.kind, DbErrorKind::BookHeldByAnother { .. }),
        "expected BookHeldByAnother, got {:?}",
        err.kind
    );
}

/// **An `id` no row carries is refused, and NO account is created.**
///
/// `id` is the identity. A row invented for a mistyped id is a book landing on an account nobody
/// has, and it would then be indistinguishable from one a migration wrote.
#[test]
fn an_id_no_row_carries_is_refused_and_creates_no_account() {
    let fx = Fixture::migrated();
    let before = fx.accounts().len();

    let err = fx.set_book(9999, DEMO1_BOOK, false).expect_err("no such row");
    match err.kind {
        DbErrorKind::NoSuchAccount { id } => assert_eq!(id, 9999),
        other => panic!("expected NoSuchAccount, got {other:?}"),
    }
    assert_eq!(fx.accounts().len(), before, "a refused write created an account row");
    assert!(err.to_string().contains("secrets accounts"), "the refusal must name the listing");
}

// ---------------------------------------------------------------------------------------------
// PROOF 3 — the stores this writer will NOT write
// ---------------------------------------------------------------------------------------------

/// **A `Backend::Absent` box is REFUSED, and no database is created.**
///
/// The second half is the one that would be expensive to get wrong. `crate::db::open_for_write`
/// CREATES a database when the path is empty, so a writer that reached it on an unmigrated box
/// would leave a finished, version-stamped database holding ONE account row — from which moment
/// `vike_secrets::backend_at` answers `Database` for every process on the box and every credential
/// in the file beside it is retired, silently, with every venue dropping to paper.
///
/// The first half is the per-RUN rule: there is no `account` table on a file store and no
/// second place to put the book, so the answer is a refusal rather than a quiet fallback.
#[test]
fn a_file_store_is_refused_and_no_database_is_created() {
    let fx = Fixture::file_store();
    assert_eq!(
        vike_secrets::backend_in(fx.dir()),
        Backend::Absent,
        "the fixture must be unmigrated"
    );

    let err = fx.set_book(1, DEMO1_BOOK, false).expect_err("a file store has no account table");
    match &err.kind {
        DbErrorKind::NoDatabase => {}
        other => panic!("expected NoDatabase, got {other:?}"),
    }
    let text = err.to_string();
    assert!(text.contains("secrets migrate"), "the refusal must name the way through: {text}");
    assert!(text.contains("NOTHING WAS WRITTEN"), "{text}");

    assert!(
        !fx.db().exists(),
        "A REFUSED WRITE CREATED THE DATABASE — an EMPTY store minted by a command about filing"
    );
    assert!(!fx.db().parent().unwrap().exists(), "…nor even the `db/` directory");
    assert_eq!(
        std::fs::read_to_string(fx.store()).unwrap(),
        Fixture::store_text(),
        "the credential file was touched"
    );
    assert_eq!(vike_secrets::backend_in(fx.dir()), Backend::Absent, "the backend choice moved");
}

/// **A schema-1 database says it predates the account table**, rather than handing back the
/// engine's own `no such table: account`.
///
/// `READABLE_SCHEMA_VERSIONS` keeps such a store readable on purpose, so a box that has simply not
/// run the migration is an ordinary state and not a malfunction. The write twin of
/// `tests/account_reader.rs`'s `a_schema_1_database_says_it_predates_the_table`.
#[test]
fn a_schema_1_store_says_it_predates_the_account_table() {
    let fx = Fixture::file_store();
    let rows: Vec<(String, String)> =
        FIXTURE_KEYS.iter().map(|k| ((*k).to_string(), fake_value(k))).collect();
    vike_secrets::plant_schema_1(&fx.db(), &rows, &[]).expect("plant a schema-1 store");

    let err = fx.set_book(1, DEMO1_BOOK, false).expect_err("schema 1 has no account table");
    match err.kind {
        DbErrorKind::NoAccountTable { found } => assert_eq!(found, 1),
        other => panic!("expected NoAccountTable, got {other:?}"),
    }
    assert!(err.to_string().contains("secrets migrate"), "the refusal must name the way through");
}
