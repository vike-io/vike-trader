//! PROOF 7 — the HANDSHAKE source: the venue's own answer, and the column it may stamp.

use super::fixture::{DEMO1_BOOK, DEMO2_BOOK, Fixture};

// ---------------------------------------------------------------------------------------------
// PROOF 7 — the HANDSHAKE source: the venue's own answer, and the column it may stamp
// ---------------------------------------------------------------------------------------------

/// The instant a handshake is claimed to have happened at. A literal, because these tests assert
/// what was STORED and not what a clock said.
const HANDSHAKE_AT: &str = "2026-09-15T08:30:00Z";
const LATER_AT: &str = "2026-09-16T08:30:00Z";

/// **An OPERATOR write never stamps `last_verified_at` — before this parameter existed and after
/// it.**
///
/// The byte-identity half: `vike-cli secrets set-book` passes `BookSource::Operator`, and a row it
/// writes must be indistinguishable from one written before the enum existed. §4.5 gives the column
/// to a successful authenticated SESSION, and an operator typing a number read off a web page has
/// performed none — a row that read *verified* on that evidence is the false confidence §1 is about.
#[test]
fn an_operator_write_leaves_last_verified_at_alone() {
    let fx = Fixture::migrated();
    let (first, _) = fx.dukascopy_ids();
    assert_eq!(fx.verified_of(first), None, "a migrated row starts unverified");

    let done = fx.set_book(first, DEMO1_BOOK, false).expect("the operator door still works");
    assert!(done.changed);
    assert_eq!(done.verified_at, None, "an operator write stamps nothing");
    assert_eq!(
        fx.verified_of(first),
        None,
        "an operator write must leave last_verified_at exactly as it was"
    );
}

/// **A HANDSHAKE write LEARNS the book and stamps the column, in one transaction.**
///
/// The `venue_account_id IS NULL → written by the venue at the first successful session` path of
/// §4.5. Both columns move, and the timestamp is the one the CALLER supplied — the handshake's
/// instant, never this process's `now`.
#[test]
fn a_handshake_learns_the_book_and_stamps_the_session() {
    let fx = Fixture::migrated();
    let (first, _) = fx.dukascopy_ids();

    let done = fx.confirm_book(first, DEMO1_BOOK, HANDSHAKE_AT).expect("the handshake folds");
    assert!(done.changed, "the book was not known, so it moved");
    assert_eq!(done.verified_at.as_deref(), Some(HANDSHAKE_AT));
    assert_eq!(fx.book_of(first).as_deref(), Some(DEMO1_BOOK));
    assert_eq!(fx.verified_of(first).as_deref(), Some(HANDSHAKE_AT));
}

/// ⚠ **THE CONFIRMATION CASE — the book already matches, and the stamp still lands.**
///
/// This is the case the column exists for, and the one an early-return would have lost: before the
/// `BookSource` parameter, `before.venue_account_id == value` returned without writing anything at
/// all, so a correctly-configured account would have read *never verified* however many sessions it
/// authenticated. `changed` stays `false` — a claim about the BOOK, which did not move — and
/// `verified_at` is `Some`, which is how a caller tells a confirmation from a true no-op.
#[test]
fn a_confirming_handshake_stamps_even_though_the_book_did_not_move() {
    let fx = Fixture::migrated();
    let (first, _) = fx.dukascopy_ids();
    fx.set_book(first, DEMO1_BOOK, false).expect("the operator writes it first");
    assert_eq!(fx.verified_of(first), None);

    let done = fx.confirm_book(first, DEMO1_BOOK, HANDSHAKE_AT).expect("the confirmation folds");
    assert!(!done.changed, "the BOOK did not move, and `changed` is a claim about the book");
    assert_eq!(done.verified_at.as_deref(), Some(HANDSHAKE_AT), "…and the SESSION was recorded");
    assert_eq!(fx.book_of(first).as_deref(), Some(DEMO1_BOOK), "the book is untouched");
    assert_eq!(fx.verified_of(first).as_deref(), Some(HANDSHAKE_AT));

    // A later session re-stamps, because the question is *when did this credential last work*.
    let again = fx.confirm_book(first, DEMO1_BOOK, LATER_AT).expect("the next session folds");
    assert!(!again.changed);
    assert_eq!(fx.verified_of(first).as_deref(), Some(LATER_AT));
}

/// ⚠ **A REFUSED write stamps NOTHING**, and this is the assertion the wrong-broker alarm rests on.
///
/// A handshake naming a book the row does not hold is refused by `BookAlreadyKnown` exactly as an
/// operator's would be — the refusal is about the CLAIM, not about who makes it — and a refused
/// transaction writes no column, the timestamp included. A session whose identity claim the store
/// just refused is the single row that must not read *verified*: stamping it would be a NEW way for
/// a wrong row to look fine, which is what this whole path exists to remove.
///
/// The FOLD never reaches this branch — `vike_model::accounts::account_confirmation::verdict` classifies the
/// disagreement and declines to call at all — so this is the belt behind it: a future caller that
/// folded blind gets a refusal rather than a re-pointed broker.
#[test]
fn a_disagreeing_handshake_is_refused_and_stamps_nothing() {
    let fx = Fixture::migrated();
    let (first, _) = fx.dukascopy_ids();
    fx.set_book(first, DEMO1_BOOK, false).expect("the row names a book");

    let err = fx
        .confirm_book(first, DEMO2_BOOK, HANDSHAKE_AT)
        .expect_err("a different book is refused without --replace, whoever claims it");
    let rendered = err.to_string();
    assert!(rendered.contains(DEMO1_BOOK), "the refusal names what the row holds: {rendered}");

    assert_eq!(fx.book_of(first).as_deref(), Some(DEMO1_BOOK), "the stored book is untouched");
    assert_eq!(
        fx.verified_of(first),
        None,
        "a REFUSED write must stamp nothing — a refused claim is not a verification"
    );
}

/// **A CLEAR under a handshake is not a thing the fold does, and if it happened it would stamp.**
///
/// Pinned rather than argued away: `BookSource` and the CLEAR are orthogonal parameters, so the
/// combination is reachable by construction and a reader should not have to guess what it does. It
/// clears the book and stamps the session, which is the literal reading of both parameters. Nothing
/// in this tree passes it — `vike-cli secrets set-book --clear` is an operator act.
#[test]
fn a_clear_and_a_handshake_are_orthogonal_parameters() {
    let fx = Fixture::migrated();
    let (first, _) = fx.dukascopy_ids();
    fx.set_book(first, DEMO1_BOOK, false).expect("the row names a book");

    let done = vike_secrets::set_venue_account_id_in(
        fx.dir(),
        first,
        None,
        false,
        vike_secrets::BookSource::Handshake { verified_at: HANDSHAKE_AT },
    )
    .expect("a clear is never refused");
    assert!(done.changed);
    assert_eq!(fx.book_of(first), None);
    assert_eq!(fx.verified_of(first).as_deref(), Some(HANDSHAKE_AT));
}

/// **A `Backend::Absent` box refuses the handshake source exactly as it refuses the operator's.**
///
/// There is no `account` table to stamp, and a per-KEY fallback is what `Backend` forbids. The
/// handshake fold must therefore behave on a file store exactly as the tree did before it existed:
/// nothing written, nothing created, and a refusal that names the file that IS answering.
#[test]
fn a_file_store_refuses_a_handshake_fold_rather_than_falling_back() {
    let fx = Fixture::file_store();
    let err = vike_secrets::set_venue_account_id_in(
        fx.dir(),
        1,
        Some(DEMO1_BOOK),
        false,
        vike_secrets::BookSource::Handshake { verified_at: HANDSHAKE_AT },
    )
    .expect_err("a file store has no account table to write");
    assert!(
        matches!(err.kind, vike_secrets::DbErrorKind::NoDatabase),
        "a file store must refuse by NAME, never fall back: {err:?}"
    );
}
