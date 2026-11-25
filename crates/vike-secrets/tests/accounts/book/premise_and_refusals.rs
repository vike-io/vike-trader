//! PROOFS 1-3 — the premise (two rows a name cannot separate) and the refusals that stand between a typo and the wrong broker.

use std::assert_matches;
use vike_secrets::{StoreBackend, StoreErrorKind};

use super::fixture::{DEMO1_BOOK, DEMO2_BOOK, Fixture};

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
    let fx = Fixture::seeded();
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
/// tells the two apart — `vike-cli account set-book` prints "unchanged" and journals nothing,
/// because a ledger line for a no-op reads as a re-pointing that did not happen.
#[test]
fn writing_the_same_book_twice_writes_nothing_and_says_so() {
    let fx = Fixture::seeded();
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
    let fx = Fixture::seeded();
    let (first, _) = fx.dukascopy_ids();
    fx.set_book(first, DEMO1_BOOK, false).expect("the row learns its book");

    let err = fx.set_book(first, DEMO2_BOOK, false).expect_err("a different book must be refused");
    match &err.kind {
        StoreErrorKind::BookAlreadyKnown { id, venue, current, .. } => {
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

/// **The SAME book in another letter case is not a different book** (BookId audit, row 15): the
/// refusal above compares `vike_model::BookId`s, so `DU1234567` over `du1234567` re-points nothing
/// and is written WITHOUT `replace`, carrying the previous spelling in the echo. A different book is
/// still refused (the test above).
#[test]
fn the_same_book_respelled_in_another_letter_case_is_written_without_replace() {
    let fx = Fixture::seeded();
    let (first, _) = fx.dukascopy_ids();
    fx.set_book(first, "du1234567", false).expect("the row learns its book");

    let done = fx
        .set_book(first, "DU1234567", false)
        .expect("the same book in another letter case is not a DIFFERENT book");
    assert!(done.changed, "the spelling moved, so the column was written");
    assert_eq!(done.before.venue_account_id.as_deref(), Some("du1234567"));
    assert_eq!(fx.book_of(first).as_deref(), Some("DU1234567"), "the new spelling is stored");

    // …and re-writing that exact spelling is the ordinary no-op.
    assert!(!fx.set_book(first, "DU1234567", false).expect("a repeat").changed);
}

/// **Decision 0274: two ACTIVE accounts of one venue MAY name one book** — the venue account number
/// is a field, never a uniqueness key (hyperliquid's demo and live rows on one wallet address, N
/// labelled rows on one venue account). A same-tier pair is the start's shared-book WARNING
/// (`vike_config::shared_books`), never a refusal here. `--replace` still guards the row's OWN book.
#[test]
fn two_active_accounts_of_one_venue_may_name_one_book() {
    let fx = Fixture::seeded();
    let (first, second) = fx.dukascopy_ids();
    fx.set_book(first, DEMO1_BOOK, false).expect("the first row learns its book");

    let done = fx.set_book(second, DEMO1_BOOK, false).expect("one book, two active rows: written");
    assert!(done.changed);
    assert_eq!(fx.book_of(first).as_deref(), Some(DEMO1_BOOK));
    assert_eq!(fx.book_of(second).as_deref(), Some(DEMO1_BOOK));

    // The row's OWN known book is still the interlock: a different one needs `--replace`.
    let err = fx.set_book(second, DEMO2_BOOK, false).expect_err("a known book is not replaced");
    assert_matches!(err.kind, StoreErrorKind::BookAlreadyKnown { .. }, "{:?}", err.kind);
}

/// **An older store still carrying `account_one_account_per_book` loses it at its first write-open,
/// and nothing else moves** (decision 0274): the shared book then lands, every row and credential is
/// where it was, and `SCHEMA_VERSION` is untouched (a bump would refuse every existing store).
#[test]
fn the_retired_book_index_is_dropped_at_the_first_write_and_nothing_else_moves() {
    let fx = Fixture::seeded();
    let (first, second) = fx.dukascopy_ids();
    fx.set_book(first, DEMO1_BOOK, false).expect("the first row learns its book");
    let index = "account_one_account_per_book";
    // The index exactly as the store's DDL declared it before decision 0274.
    fx.conn()
        .execute_batch(&format!(
            "CREATE UNIQUE INDEX {index} ON account (venue_id, venue_account_id) \
             WHERE active = 1 AND venue_account_id IS NOT NULL;"
        ))
        .expect("plant the retired index");
    let names = fx.credential_names();
    let version = crate::support::sql::user_version(&fx.conn());

    fx.set_book(second, DEMO1_BOOK, false).expect("the index is gone, so the shared book lands");

    let conn = fx.conn();
    let left: i64 = conn
        .query_row("SELECT count(*) FROM sqlite_master WHERE name = ?1", [index], |r| r.get(0))
        .expect("ask the schema");
    assert_eq!(left, 0, "the retired index survived a write-open");
    assert_eq!(crate::support::sql::user_version(&conn), version, "the schema version moved");
    assert_eq!(fx.credential_names(), names, "a credential row moved");
    assert_eq!(fx.book_of(second).as_deref(), Some(DEMO1_BOOK));
}

/// **An `id` no row carries is refused, and NO account is created.**
///
/// `id` is the identity. A row invented for a mistyped id is a book landing on an account nobody
/// has, and it would then be indistinguishable from one a migration wrote.
#[test]
fn an_id_no_row_carries_is_refused_and_creates_no_account() {
    let fx = Fixture::seeded();
    let before = fx.accounts().len();

    let err = fx.set_book(9999, DEMO1_BOOK, false).expect_err("no such row");
    match err.kind {
        StoreErrorKind::NoSuchAccount { id } => assert_eq!(id, 9999),
        other => panic!("expected NoSuchAccount, got {other:?}"),
    }
    assert_eq!(fx.accounts().len(), before, "a refused write created an account row");
    assert!(err.to_string().contains("account ls"), "the refusal must name the listing");
}

// ---------------------------------------------------------------------------------------------
// PROOF 3 — the stores this writer will NOT write
// ---------------------------------------------------------------------------------------------

/// **A `StoreBackend::Absent` box is REFUSED, and no database is created.**
///
/// The second half is the one that would be expensive to get wrong. SQLite's default open CREATES a
/// database when the path is empty, so a writer that opened that way on a box with no store would
/// leave a finished, version-stamped database holding ONE account row — from which moment
/// `vike_secrets::backend_at` answers `Database` for every process on the box, an EMPTY store
/// nobody created on purpose.
///
/// The first half is the per-RUN rule: there is no `account` table on a box with no store and no
/// second place to put the book, so the answer is a refusal rather than a quiet fallback.
#[test]
fn a_box_with_no_store_is_refused_and_no_database_is_created() {
    let fx = Fixture::absent();
    assert_eq!(
        vike_secrets::backend_in(fx.dir()),
        StoreBackend::Absent,
        "the fixture must hold no store"
    );

    let err =
        fx.set_book(1, DEMO1_BOOK, false).expect_err("a box with no store has no account table");
    match &err.kind {
        StoreErrorKind::NoDatabase => {}
        other => panic!("expected NoDatabase, got {other:?}"),
    }
    let text = err.to_string();
    assert!(text.contains("secrets init"), "the refusal must name the way through: {text}");
    assert!(text.contains("NOTHING WAS WRITTEN"), "{text}");

    assert!(
        !fx.db().exists(),
        "A REFUSED WRITE CREATED THE DATABASE — an EMPTY store minted by a command about filing"
    );
    assert!(!fx.db().parent().unwrap().exists(), "…nor even the `db/` directory");
    assert_eq!(
        vike_secrets::backend_in(fx.dir()),
        StoreBackend::Absent,
        "the backend choice moved"
    );
}
