//! PROOFS 9 and 10 — the CLEAR, the swapped pair it repairs, and the key names that tell two identical rows apart.

use super::fixture::{DEMO1_BOOK, DEMO2_BOOK, Fixture};

// ---------------------------------------------------------------------------------------------
// PROOF 9 — the CLEAR, and the swapped pair it exists to repair
// ---------------------------------------------------------------------------------------------

/// **A pair written the WRONG WAY ROUND can be corrected, and clearing one row is the only way.**
///
/// ⚠ This is a repair that did not exist. With both rows set and crossed, every direct correction
/// is refused in BOTH directions: `--replace` gets past *this row already names a different book*,
/// and ruling 11's one-account-per-book check then finds the other row holding the number being
/// written. There is no ordering of two writes that escapes it — asserted below before the repair
/// is performed, so this test fails if the dead end is ever reopened.
///
/// The third move is a CLEAR. It asserts nothing about a broker (`NULL` is where every migrated row
/// starts), it asks neither guard, and it leaves a state ruling 11's index accepts — which is what
/// makes *clear one, write the other, write the first* terminate.
#[test]
fn a_swapped_pair_is_repaired_by_clearing_one_row_first() {
    let fx = Fixture::migrated();
    let (first, second) = fx.dukascopy_ids();
    let rows_before = fx.accounts().len();

    // The mistake: each row holds the other's book.
    fx.set_book(first, DEMO2_BOOK, false).expect("the wrong way round, but it writes");
    fx.set_book(second, DEMO1_BOOK, false).expect("…and so does the other half");

    // THE DEAD END, both directions, with `--replace` granted. Each one is ruling 11 refusing the
    // intermediate state, which is CORRECT — and which is why a third move is needed.
    for (id, book) in [(first, DEMO1_BOOK), (second, DEMO2_BOOK)] {
        let err = fx.set_book(id, book, true).expect_err("the crossed state must refuse");
        let msg = err.to_string();
        assert!(msg.contains("may not name one book"), "{msg}");
        // …and the refusal NAMES the repair, as a command that exists. It used to say "deactivate
        // or correct the other", and neither act was reachable from any verb in this tree.
        assert!(msg.contains("--clear"), "the refusal must name a repair that exists: {msg}");
    }
    assert_eq!(fx.book_of(first).as_deref(), Some(DEMO2_BOOK), "nothing was written");
    assert_eq!(fx.book_of(second).as_deref(), Some(DEMO1_BOOK), "nothing was written");

    // The repair: clear, write, write.
    let cleared = fx.clear_book(first).expect("a clear is always available");
    assert!(cleared.changed);
    assert_eq!(cleared.before.venue_account_id.as_deref(), Some(DEMO2_BOOK), "the echo says what");
    assert_eq!(cleared.venue_account_id, None, "and what it holds now");
    assert_eq!(fx.book_of(first), None, "the row is back to not-yet-known");

    fx.set_book(second, DEMO2_BOOK, true).expect("with the other row blank, this lands");
    fx.set_book(first, DEMO1_BOOK, false).expect("…and the blank row takes the remaining book");

    assert_eq!(fx.book_of(first).as_deref(), Some(DEMO1_BOOK), "repaired");
    assert_eq!(fx.book_of(second).as_deref(), Some(DEMO2_BOOK), "repaired");
    assert_eq!(
        fx.accounts().len(),
        rows_before,
        "the repair must move books between rows, never create or drop one"
    );
}

/// **A clear needs no `replace`, is idempotent, and touches no other row.**
#[test]
fn a_clear_is_idempotent_and_local() {
    let fx = Fixture::migrated();
    let (first, second) = fx.dukascopy_ids();
    fx.set_book(first, DEMO1_BOOK, false).expect("first");
    fx.set_book(second, DEMO2_BOOK, false).expect("second");

    // No `--replace`, even though the row holds a DIFFERENT value than the one being written
    // (`None`): a clear removes an assertion rather than making one.
    assert!(fx.clear_book(first).expect("a set row clears").changed);

    // Twice is not an error — the same posture re-writing a known value takes.
    let again = fx.clear_book(first).expect("clearing a blank row is not a conflict");
    assert!(!again.changed, "a repeat clear must report no change");
    assert_eq!(again.before.venue_account_id, None);

    // …and a row that never held one.
    let untouched: Vec<i64> = fx
        .accounts()
        .into_iter()
        .filter(|a| a.venue_account_id.is_none() && a.id != first)
        .map(|a| a.id)
        .collect();
    assert!(!untouched.is_empty(), "the fixture must hold other book-less rows");
    assert!(!fx.clear_book(untouched[0]).expect("a blank row clears").changed);

    assert_eq!(fx.book_of(second).as_deref(), Some(DEMO2_BOOK), "the other row is untouched");
}

/// **A clear still refuses an `id` no row carries** — it is a write like any other, and it creates
/// nothing.
#[test]
fn a_clear_of_a_missing_row_is_refused_and_creates_nothing() {
    let fx = Fixture::migrated();
    let before = fx.accounts().len();
    let err = fx.clear_book(9999).expect_err("no such row");
    assert!(err.to_string().contains("9999"), "{err}");
    assert_eq!(fx.accounts().len(), before, "no row was created");
}

// ---------------------------------------------------------------------------------------------
// PROOF 10 — the thing that tells two identical rows apart
// ---------------------------------------------------------------------------------------------

/// **The two dukascopy rows are separable — by their CREDENTIAL KEY NAMES, and by nothing else.**
///
/// ⚠ This is the premise the writer was unusable without. `set_venue_account_id` addresses a row by
/// `id`, and `id` is a database surrogate: for this pair the account table's every other cell is
/// equal (`dukascopy`, `demo`, `label NULL`, `book NULL`), so a listing built from
/// `vike_secrets::Account` alone shows two lines differing by an opaque integer and offers no way
/// to choose. Choosing wrongly points an account at the other legal entity.
///
/// `resolve_account_keys_in` is the answer, and the fact was in the store the whole time: each row's
/// `credential` rows keep their LEGACY NAMES, so `DUKASCOPY_DEMO1_*` belongs to one row and
/// `DUKASCOPY_DEMO2_*` to the other. The owner PREFIX is the same derivation the migration's own
/// resolver uses to re-find an account across runs, which is what makes it the store's own idea of
/// who a row is rather than a rendering convenience.
#[test]
fn the_key_names_are_what_tell_the_two_identical_rows_apart() {
    let fx = Fixture::migrated();
    let (first, second) = fx.dukascopy_ids();

    // The premise: every other cell is equal.
    let rows = fx.accounts();
    let a = rows.iter().find(|r| r.id == first).expect("first");
    let b = rows.iter().find(|r| r.id == second).expect("second");
    assert_eq!((&a.venue, &a.tier, &a.label), (&b.venue, &b.tier, &b.label));
    assert_eq!((a.venue_account_id.as_deref(), b.venue_account_id.as_deref()), (None, None));

    // …and the key names are not.
    let ka = fx.keys_of(first);
    let kb = fx.keys_of(second);
    assert_eq!(ka.prefixes, vec!["DUKASCOPY_DEMO1_".to_string()], "{ka:?}");
    assert_eq!(kb.prefixes, vec!["DUKASCOPY_DEMO2_".to_string()], "{kb:?}");
    assert_ne!(ka.prefixes, kb.prefixes, "the two rows must be separable");
    assert!(ka.names.iter().all(|n| n.starts_with("DUKASCOPY_DEMO1_")), "{ka:?}");
    assert!(kb.names.iter().all(|n| n.starts_with("DUKASCOPY_DEMO2_")), "{kb:?}");

    // ⚠ NAMES ONLY. The reader selects `name` and `field`; `value` is not in the statement, so no
    // rendering of this type can be a credential. Every fixture value is `value-for-<KEY>`.
    for k in [&ka, &kb] {
        for rendered in k.names.iter().chain(k.prefixes.iter()) {
            assert!(
                !rendered.contains("value-for-"),
                "a VALUE reached the key listing: {rendered}"
            );
        }
    }

    // A single-account venue is keyed too — the column is not a dukascopy special case.
    let binance = rows.iter().find(|r| r.venue == "binance" && r.tier == "demo").expect("binance");
    assert_eq!(fx.keys_of(binance.id).prefixes, vec!["BINANCE_DEMO_".to_string()]);
}

/// **A FILE store answers `None` here, and `None` is not an empty map.**
///
/// The same three-state discipline `Accounts` holds: *this store has no account table to key* and
/// *the table is there and this row owns no keys* are different answers, and a renderer that merged
/// them would print a blank discriminator column for a box whose accounts are perfectly well
/// identified — by the key names themselves, which is what `secrets list` prints there.
#[test]
fn a_file_store_cannot_be_keyed_and_says_so_rather_than_answering_empty() {
    let fx = Fixture::file_store();
    assert!(
        vike_secrets::resolve_account_keys_in(fx.dir())
            .expect("a file store is not an error")
            .is_none(),
        "a file store must answer None, never an empty map"
    );
}
