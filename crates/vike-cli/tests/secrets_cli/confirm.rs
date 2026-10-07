//! `confirm`: the HANDSHAKE fold over a parked confirmation file, end to end.

use super::support::{BOOK_STORE, SetCase, exit_code, rung};
use super::{stderr, stdout};

// ── confirm — the HANDSHAKE fold, end to end through the shipped binary ─────────────────────────
//
// ⚠ These cases plant the parked file a LIVE MOUNT would write, because the mount needs a JForex
// sidecar, an SDK and a network and none of the three is available to a test. What is proven here
// is everything downstream of that file: the addressing, the three verdicts, the two columns, the
// ledger and what is left parked. The mount's own half — that it writes a record of this shape, at
// this address — is `crates/bridges/dukascopy/src/account.rs`'s `confirmation_for` tests, and the file
// format is `vike_model::accounts::account_confirmation`'s own suite. Nothing here asserts what the JForex
// handshake actually RETURNS: that is unmeasured (the credential-schema spec §9) and a test that
// invented it would be pinning the invention.

/// **A box with nothing parked says so and exits ZERO.** The ordinary state of every box that has
/// not mounted a confirming venue — not a failure, and not an empty table either.
#[test]
fn confirm_with_nothing_parked_is_an_answer_rather_than_an_error() {
    let c = SetCase::new("confirm-empty");
    let _ = c.seeded_books();

    let out = c.run(&["confirm"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("nothing parked"), "{text}");
    assert!(text.contains("vike.db"), "it must still name the store it would have written: {text}");
}

/// **LEARNS: a row with no book takes the venue's own answer and is stamped verified**, addressed
/// by its credential key PREFIX and not by a row id — and the ledger files it as an `account_book`
/// written by the VENUE.
#[test]
fn confirm_learns_a_book_from_the_venue_and_stamps_the_session() {
    let c = SetCase::new("confirm-learns");
    let ids = c.seeded_books();
    let before_store = c.read_store();
    c.park(&[("dukascopy", "DUKASCOPY_DEMO1_", "DEMO1abcd")]);

    let out = c.run(&["confirm"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("LEARNS"), "{text}");
    assert!(text.contains("DEMO1abcd"), "the venue's answer is echoed: {text}");
    assert!(text.contains("1 row(s) folded, 0 disagreement(s)"), "{text}");

    // The BOOK landed on exactly one row, and the OTHER dukascopy row is untouched — which is what
    // says the key prefix addressed it rather than a sweep.
    let rows = c.account_rows();
    assert_eq!(rows.matches("DEMO1abcd").count(), 1, "exactly ONE row may learn this book: {rows}");
    assert!(
        stdout(&c.run(&["accounts"], None, &[])).contains("2 with no venue account id yet"),
        "{rows}"
    );
    assert_ne!(ids[0], ids[1]);

    // The record is CONSUMED, so a second run has nothing to do.
    assert!(
        !c.parked_text().contains("DEMO1abcd"),
        "a folded record stayed parked: {}",
        c.parked_text()
    );
    let again = stdout(&c.run(&["confirm"], None, &[]));
    assert!(again.contains("nothing parked"), "{again}");

    // The credential FILE is byte-identical: nothing on this path opens it.
    assert_eq!(c.read_store(), before_store, "the credential file was touched");

    // The ledger: ONE `account_book`, by the VENUE rather than the CLI, and no value anywhere.
    let books = c.book_records();
    assert_eq!(books.len(), 1, "one record for one fold: {books:?}");
    // ⚠ The ACTOR, matched on the serialized tag rather than on the word `dukascopy` — that word
    // appears in `target.venue` on every one of these records, so a bare `contains("dukascopy")`
    // could not fail for its stated reason and would pass for an `Actor::cli` record too.
    assert!(
        books[0].contains(r#""actor":{"origin":"venue","venue":"dukascopy"}"#),
        "the fold's actor must be the VENUE — the value came from it and this process only \
         carried it: {}",
        books[0]
    );
    assert!(!books[0].contains(r#""origin":"cli""#), "{}", books[0]);
    for v in ["login-one", "login-two", "pass-one", "pass-two", "key-one"] {
        assert!(!books[0].contains(v), "a VALUE reached the ledger: {}", books[0]);
    }
}

/// ⚠ **CONFIRMS: the book already matches, and the timestamp still moves.**
///
/// The case the column exists for, and the one the tree had no way to record at all: without it
/// *never verified* and *verified three weeks ago* go on looking identical to *fine*. The book must
/// not move and the ledger must stay silent — a verification changes no routing.
#[test]
fn confirm_stamps_a_row_whose_book_already_matches_and_journals_nothing() {
    let c = SetCase::new("confirm-confirms");
    let ids = c.seeded_books();
    let id = ids[0].to_string();
    assert_eq!(
        exit_code(&c.run(&["set-book", "--id", &id, "--venue-account-id", "DEMO1abcd"], None, &[])),
        rung(vike_cli::exit::Exit::Ok)
    );
    let books_before = c.book_records().len();

    c.park(&[("dukascopy", "DUKASCOPY_DEMO1_", "DEMO1abcd")]);
    let out = c.run(&["confirm"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("CONFIRMS"), "{text}");
    assert!(text.contains("1 row(s) folded, 0 disagreement(s)"), "{text}");
    assert!(text.contains("verified=2026-08-22T00:00:00Z"), "the HANDSHAKE's instant: {text}");

    // The row now carries the verification, and a SECOND `confirm` run sees the previous one —
    // which is the whole point of the column.
    c.park(&[("dukascopy", "DUKASCOPY_DEMO1_", "DEMO1abcd")]);
    let second = stdout(&c.run(&["confirm"], None, &[]));
    assert!(second.contains("last verified=2026-08-22T00:00:00Z"), "{second}");

    // ⚠ NOTHING was journalled by either run: `changed` is a claim about the BOOK, the book did not
    // move, and a ledger line saying it did would be the misreading the rule forbids.
    let books = c.book_records();
    assert_eq!(
        books.len(),
        books_before,
        "a confirmation that moved no book must journal nothing: {books:?}"
    );
}

/// ⚠ **DISAGREES: the store says one account and the venue says another — and NOTHING is written.**
///
/// Not the book (a fold has no operator in front of it to permit re-pointing an armed account at
/// another broker) and not the timestamp (a row the venue has just contradicted must not read as
/// verified). The record is KEPT so the finding does not vanish with the run that found it, the
/// exit code stays ZERO because the session that produced the confirmation authenticated, and the
/// report names the one-command repair and says the disagreement is not yet proof of a wrong
/// broker.
#[test]
fn confirm_refuses_to_overwrite_a_disagreeing_book_and_keeps_the_record() {
    let c = SetCase::new("confirm-disagrees");
    let ids = c.seeded_books();
    let id = ids[0].to_string();
    assert_eq!(
        exit_code(&c.run(&["set-book", "--id", &id, "--venue-account-id", "3709890"], None, &[])),
        rung(vike_cli::exit::Exit::Ok)
    );
    let books_before = c.book_records().len();

    c.park(&[("dukascopy", "DUKASCOPY_DEMO1_", "DEMO1abcd")]);
    let out = c.run(&["confirm"], None, &[]);
    // ⚠ ZERO. A disagreement is a REPORT: the rows that folded folded, and an exit code here would
    // make a scripted `confirm` fail on a box that is one hand-written number out of date.
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("DISAGREEMENT"), "{text}");
    assert!(text.contains("3709890") && text.contains("DEMO1abcd"), "both strings: {text}");
    assert!(text.contains("NOTHING IS WRITTEN"), "{text}");
    assert!(text.contains("--replace"), "the one-command repair must be printed: {text}");
    assert!(text.contains("not yet proof of a wrong broker"), "the FORM residual: {text}");
    assert!(text.contains("0 row(s) folded, 1 disagreement(s)"), "{text}");

    // NEITHER column moved. ⚠ The TABLE ROWS, not the whole listing: the listing's own parked
    // notice names `DEMO1abcd` (that is what the notice is FOR), so a whole-output assertion would
    // fail here for the wrong reason.
    let rows = c.account_rows();
    assert!(rows.contains("3709890"), "the stored book was overwritten: {rows}");
    assert!(!rows.contains("DEMO1abcd"), "the handshake's answer was written: {rows}");
    assert_eq!(c.book_records().len(), books_before, "a refused fold must journal nothing");

    // …and the record is KEPT, so `accounts` goes on surfacing it and a later run can act on it.
    assert!(c.parked_text().contains("DEMO1abcd"), "the record was consumed: {}", c.parked_text());
    assert!(
        stdout(&c.run(&["accounts"], None, &[])).contains("parked by a live mount"),
        "the listing must surface a waiting fold"
    );
}

/// **`--dry-run` prints every verdict and writes nothing** — neither a row nor the parked file.
///
/// It matters more on this verb than on `set-book`: the rows about to be written are named by a
/// file a daemon wrote, not by the operator, so this is the only way to see which accounts are
/// about to move before they do.
#[test]
fn confirm_dry_run_writes_neither_a_row_nor_the_parked_file() {
    let c = SetCase::new("confirm-dry");
    let _ = c.seeded_books();
    c.park(&[("dukascopy", "DUKASCOPY_DEMO2_", "DEMO2cGyrc")]);
    let parked_before = c.parked_text();

    let out = c.run(&["confirm", "--dry-run"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("LEARNS"), "{text}");
    assert!(text.contains("dry run"), "{text}");
    assert!(text.contains("DRY RUN, nothing was written"), "{text}");

    // ⚠ The TABLE ROWS, not the whole listing — the parked notice names the record on purpose.
    let rows = c.account_rows();
    assert!(!rows.contains("DEMO2cGyrc"), "a dry run wrote a row: {rows}");
    assert_eq!(c.parked_text(), parked_before, "a dry run consumed a record");
    assert!(c.book_records().is_empty(), "a dry run journalled a book write");
}

/// **A record no ACTIVE row owns is KEPT and reported, never guessed at.** This is what a
/// re-migration looks like from the fold's side: the confirmations are still good evidence and the
/// numbering under them moved, so the address either resolves or it does not.
#[test]
fn confirm_keeps_a_record_whose_address_names_no_row() {
    let c = SetCase::new("confirm-orphan");
    let _ = c.seeded_books();
    c.park(&[("dukascopy", "DUKASCOPY_DEMO9_", "DEMO9zzzz")]);

    let out = c.run(&["confirm"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("no ACTIVE dukascopy row owns the credential keys"), "{text}");
    assert!(text.contains("0 row(s) folded"), "{text}");
    assert!(c.parked_text().contains("DEMO9zzzz"), "the record must be KEPT: {}", c.parked_text());
}

/// **An UNMIGRATED box refuses, writes nothing, creates no database and KEEPS every record.**
///
/// A file store has no `account` table to stamp, and a per-KEY fallback is what `Backend` forbids.
/// The confirmations are perfectly good evidence — it is FOLDING that waits for the migration, not
/// recording.
#[test]
fn confirm_on_an_unmigrated_box_refuses_and_keeps_every_record() {
    let c = SetCase::new("confirm-unmigrated");
    c.write_store(BOOK_STORE);
    c.park(&[("dukascopy", "DUKASCOPY_DEMO1_", "DEMO1abcd")]);
    let parked_before = c.parked_text();

    let out = c.run(&["confirm"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Failed), "{}", stderr(&out));
    let e = stderr(&out);
    assert!(e.contains("NOTHING WAS WRITTEN"), "{e}");
    assert!(e.contains("are KEPT"), "{e}");
    assert!(e.contains("secrets migrate"), "the way through must be named: {e}");
    assert_eq!(c.parked_text(), parked_before, "a refused run consumed a record");
    assert!(
        !c.settings().join("db").join("vike.db").exists(),
        "this verb must never create a database"
    );
}

// ── the two READERS the writers were missing ────────────────────────────────────────────────────
//
// A column with a writer and no reader, and a record parked where nobody is told about it, are the
// same defect wearing two hats: something was learned and the operator cannot see it. Each case
// below drives the shipped binary end to end and asserts what an operator actually reads.

/// **An UNMIGRATED box is TOLD about parked confirmations** — the population the notice was written
/// for, and the one it did not reach.
///
/// `run_accounts` returns early on `Accounts::Unanswerable`, which is a `Backend::Files` box, and
/// the parked notice used to sit BELOW that return. A mount parks a record on such a box exactly as
/// it does on a migrated one (parking is not what waits for the migration; folding is), so the
/// operator with no other way to learn a fold is waiting was the one never told.
///
/// ⚠ It must also say that `confirm` will REFUSE here rather than work, or the notice sends
/// somebody to a verb that exits non-zero and reads as a broken tool.
#[test]
fn accounts_on_an_unmigrated_box_still_reports_what_a_mount_parked() {
    let c = SetCase::new("accounts-unmigrated-parked");
    c.write_store(BOOK_STORE);
    c.park(&[("dukascopy", "DUKASCOPY_DEMO1_", "DEMO1abcd")]);

    let out = c.run(&["accounts"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("no account table"), "the store answer is unchanged: {text}");

    // THE FINDING: the record is named at all.
    assert!(text.contains("parked by a live mount"), "the parked notice never rendered: {text}");
    assert!(text.contains("DUKASCOPY_DEMO1_"), "the notice must name the address: {text}");
    assert!(text.contains("DEMO1abcd"), "...and what the venue answered: {text}");

    // ...and the disposition, which differs from a migrated box's and is the reason the notice
    // takes a parameter rather than printing one sentence for both.
    assert!(text.contains("NOTHING ON THIS BOX CAN FOLD THESE YET"), "{text}");
    assert!(text.contains("KEEPS every record"), "the records must be said to be safe: {text}");
    assert!(text.contains("secrets migrate"), "the way through must be named: {text}");

    // A read may not write, and this one still creates nothing.
    assert!(!c.db().exists(), "a listing created a database: {text}");
}

/// **Nothing parked prints nothing**, on the unmigrated path as on the migrated one — the notice is
/// a NOTICE, and a box that has mounted no confirming venue is the ordinary case rather than a
/// state worth a paragraph.
#[test]
fn accounts_says_nothing_about_confirmations_when_none_are_parked() {
    let c = SetCase::new("accounts-unparked");
    c.write_store(BOOK_STORE);

    let text = stdout(&c.run(&["accounts"], None, &[]));
    assert!(text.contains("no account table"), "{text}");
    assert!(!text.contains("parked by a live mount"), "an empty box printed the notice: {text}");
}

/// **`accounts` prints `last verified`, and NEVER VERIFIED is visibly a state rather than a blank.**
///
/// The column gained its first writer and no reader in the listing an operator actually reads, so
/// the incident it exists to remove survived it: with nothing rendered, a row nothing had ever
/// authenticated as and a row that authenticated three weeks ago both read as *fine*. This asserts
/// the distinction is VISIBLE — a freshly migrated row says so in words, and a folded row carries
/// the venue handshake's own instant.
#[test]
fn accounts_prints_never_verified_until_a_venue_confirms_the_row() {
    let c = SetCase::new("accounts-verified");
    let _ = c.seeded_books();

    let before = stdout(&c.run(&["accounts"], None, &[]));
    assert!(before.contains("last verified"), "the column must be in the header: {before}");
    assert!(before.contains("NEVER VERIFIED"), "an unverified row must SAY so: {before}");
    assert!(
        before.contains("is not a fault") && before.contains("not the same as *fine*"),
        "the footer must say what the state means, or the column is an always-on alarm: {before}"
    );
    // ⚠ Every TABLE ROW reads the same way on a fresh box. Asserted over the rows rather than the
    // whole listing so the footer paragraph cannot satisfy it.
    for row in c.account_rows().lines() {
        assert!(row.contains("NEVER VERIFIED"), "a fresh row rendered as something else: {row}");
    }

    // Now let a venue confirm ONE of them.
    c.park(&[("dukascopy", "DUKASCOPY_DEMO1_", "DEMO1abcd")]);
    let folded = c.run(&["confirm"], None, &[]);
    assert_eq!(exit_code(&folded), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&folded));
    let fold_text = stdout(&folded);
    assert!(fold_text.contains("1 row(s) folded"), "{fold_text}");

    // ⚠ The instant is taken from the FOLD's own report rather than written down here: the two
    // readers must agree about the same value, and a literal date would pass while they disagreed.
    // ⚠ The `written:` line specifically. The per-record ECHO a few lines above it carries
    // `last verified=(never)` — the row's state BEFORE the fold — so a `find_map` over every line
    // holding `verified=` picks that one up and reads the value as `(never)`.
    let stamped = fold_text
        .lines()
        .find(|l| l.trim_start().starts_with("written:"))
        .and_then(|l| l.split("verified=").nth(1))
        .map(|t| t.trim().to_string())
        .expect("the fold must report what it stamped");
    assert!(stamped.ends_with('Z'), "the stamp must be an RFC 3339 instant: {stamped}");

    let after = c.account_rows();
    let confirmed: Vec<&str> = after.lines().filter(|l| l.contains("DEMO1abcd")).collect();
    assert_eq!(confirmed.len(), 1, "exactly one row must have learned the book: {after}");
    assert!(
        confirmed[0].contains(&stamped),
        "the confirmed row must carry the HANDSHAKE's instant ({stamped}), not a dash: {after}"
    );
    assert!(
        !confirmed[0].contains("NEVER VERIFIED"),
        "a confirmed row must stop reading as never verified: {after}"
    );
    // ...and the rows nothing authenticated as are UNCHANGED, which is what makes the column
    // informative rather than decorative.
    assert!(
        after.lines().any(|l| l.contains("NEVER VERIFIED")),
        "the rows no venue confirmed must still say so: {after}"
    );
}

/// **The FIRST fold on a hand-written box reads as a SHAPE question, and prints the commands.**
///
/// Every stored book in this tree was typed in by an operator off the venue's own page — numeric on
/// dukascopy — while the sidecar sends `IAccount.getAccountId()`, whose one pinned frame is
/// login-shaped. `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §9 leaves that
/// unsettled, so the first fold on such a box disagrees on EVERY row at once. That is cell for cell
/// what a wrong-broker credential set looks like, so the report has to say which of the two it
/// cannot tell — and then be ACTIONABLE, because a summary ending in a count sends an operator
/// scrolling back to reassemble an `--id`.
///
/// ⚠ It must NOT decide the two spellings are the same account: nobody has measured that, and
/// guessing permissively is the failure the alarm exists to catch.
#[test]
fn the_first_fold_of_a_hand_written_box_reads_as_a_shape_question_with_its_commands() {
    let c = SetCase::new("confirm-first-fold");
    let ids = c.seeded_books();

    // The hand-written state: both dukascopy rows carry the NUMBERS off the venue's page.
    for (id, book) in ids.iter().zip(["3709890", "3716974"]) {
        let out =
            c.run(&["set-book", "--id", &id.to_string(), "--venue-account-id", book], None, &[]);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    }
    // ...and the venue answers login-shaped, for both.
    c.park(&[
        ("dukascopy", "DUKASCOPY_DEMO1_", "DEMO1abcd"),
        ("dukascopy", "DUKASCOPY_DEMO2_", "DEMO2cGyrc"),
    ]);

    let out = c.run(&["confirm"], None, &[]);
    // ⚠ ZERO, not a failure: every session that produced these authenticated. A non-zero here
    // trains an operator to append `|| true`.
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("0 row(s) folded, 2 disagreement(s)"), "{text}");

    // THE FIRST-RUN FRAMING.
    assert!(text.contains("EVERY confirmation offered (2) disagreed"), "{text}");
    assert!(text.contains("SHAPE question"), "it must be framed as a shape question: {text}");
    assert!(text.contains("§9"), "...anchored on the record that leaves it open: {text}");
    assert!(
        text.contains("NOTHING HERE ASSUMES THE TWO ARE THE SAME ACCOUNT"),
        "the report must refuse to guess, out loud: {text}"
    );

    // THE COMMANDS — one complete, paste-ready line per finding, `--replace` already on it.
    //
    // ⚠ Asserted WITHOUT assuming which row id owns which key family: the migration numbers rows in
    // the order it meets credential key names, which is an implementation fact this case has no
    // business pinning. What it does pin is that each command is whole and names a row that is
    // really in this store.
    for answered in ["DEMO1abcd", "DEMO2cGyrc"] {
        let needle = format!("--venue-account-id {answered} --replace");
        let line = text
            .lines()
            .find(|l| l.contains(&needle))
            .unwrap_or_else(|| panic!("no resolving command for {answered}: {text}"));
        assert!(line.contains("vike-cli secrets set-book --id "), "incomplete command: {line}");
        let id: i64 = line
            .split("--id ")
            .nth(1)
            .and_then(|t| t.split_whitespace().next())
            .and_then(|t| t.parse().ok())
            .unwrap_or_else(|| panic!("the command must carry a real row id: {line}"));
        assert!(ids.contains(&id), "the command named a row not in this store: {line}");
    }
    assert!(text.contains("only AFTER you have looked"), "{text}");

    // NOTHING MOVED: not a book, not a timestamp, not a record.
    let rows = c.account_rows();
    for numeric in ["3709890", "3716974"] {
        assert!(rows.contains(numeric), "a stored book was overwritten: {rows}");
    }
    for row in rows.lines().filter(|l| l.contains("dukascopy")) {
        assert!(row.contains("NEVER VERIFIED"), "a contradicted row was stamped verified: {row}");
    }
    // ⚠ CONTENT, not bytes. A non-dry run always re-writes the parked file through
    // `account_confirmation::replace_all` — an empty keep set still writes it, which is how a later
    // reader tells *nothing parked* from *never looked* — so the pretty-printed result differs from
    // the fixture's compact JSON while holding exactly the same records. What must hold is that
    // BOTH survive: a disagreement is kept so a later run can act on it.
    let parked = c.parked_text();
    for answered in ["DEMO1abcd", "DEMO2cGyrc"] {
        assert!(parked.contains(answered), "a disagreeing record was consumed: {parked}");
    }
    assert_eq!(parked.matches("key_prefix").count(), 2, "the set must still be two: {parked}");
}
