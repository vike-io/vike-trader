//! `accounts` and `set-book`: the account table with its ids, and the one-row book write.

use super::support::{SetCase, exit_code, rung};
use super::{stderr, stdout};

// ── accounts / set-book ─────────────────────────────────────────────────────────────────────────
//
// ⚠ Every case here drives its own temp settings directory through `$VIKE_SETTINGS_DIR`, like the
// `set` and `migrate` cases above and doubly so: `set-book` writes a row in a credential DATABASE,
// and a case that fell through to the walk would write one inside the developer's checkout.

/// The OWNER PREFIX the store reports for an account's keys (`vike_secrets::AccountKeys`'
/// `prefixes`: a row's name less its field) — `dukascopy#2.` for the row `dukascopy#2.LOGIN`.
///
/// Composed through `vike_secrets::account_credential_name` with an EMPTY field rather than spelled
/// here, so these assertions follow the writer's one naming scheme. Shared with `confirm.rs`, whose
/// parked records address an UNLABELLED account by exactly this string.
pub(super) fn key_prefix(venue: &str, id: i64) -> String {
    vike_secrets::account_credential_name(venue, id, "")
}

/// **`accounts` prints the table, with the `id` that `set-book` takes and a book that is not yet
/// known.**
///
/// `list` prints the same rows in brief — id, venue, tier, label and the key names filed against
/// each — and not the BOOK: the cell `set-book` writes, which this listing shows beside the id.
#[test]
fn accounts_prints_the_rows_with_their_ids_and_no_value() {
    let c = SetCase::new("accounts");
    let ids = c.seeded_books();
    assert_eq!(ids.len(), 2, "the fixture plants TWO dukascopy rows: {ids:?}");
    assert_ne!(ids[0], ids[1], "the two rows must differ by id — the handle `set-book` takes");

    let out = c.run(&["accounts"], None, &[]);
    let text = stdout(&out);
    assert!(text.contains("dukascopy"), "{text}");
    assert!(text.contains("binance"), "the listing must not be dukascopy-only: {text}");
    assert!(text.contains("(not yet known)"), "an unwritten book must say so: {text}");
    assert!(text.contains("vike.db"), "the listing must name the store it read: {text}");
    assert!(
        text.contains("SWISS") && text.contains("EUROPE"),
        "each dukascopy row must carry the label it was added with: {text}"
    );
    // A VALUE cannot reach either stream — the reader selects from `account` alone.
    for v in ["login-one", "login-two", "key-one", "pass-one", "pass-two"] {
        assert!(!text.contains(v), "a VALUE reached the listing: {text}");
    }
}

/// **A box with no store says it has no account table and exits ZERO.**
///
/// Not a failure: an absent store is the live gate, an ordinary state, and `Known(vec![])` there
/// would be an assertion about a store that does not exist. The message names the reader that DOES
/// apply and the way to create the store.
#[test]
fn accounts_on_an_unmigrated_box_names_the_store_that_answers() {
    let c = SetCase::new("accounts-unmigrated");

    let out = c.run(&["accounts"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("no account table"), "{text}");
    assert!(text.contains("secrets list"), "it must name the reader that applies: {text}");
    assert!(text.contains("secrets init"), "…and the way through: {text}");
}

/// **The two dukascopy rows learn DIFFERENT books through the shipped binary**, each run echoing
/// the row it is about, and the change journal records each write as an `account_book` — never as a
/// credential write.
#[test]
fn set_book_writes_one_row_echoes_it_and_journals_the_change() {
    let c = SetCase::new("set-book");
    let ids = c.seeded_books();
    let before_store = c.read_store();

    let out = c.run(
        &["set-book", "--id", &ids[0].to_string(), "--venue-account-id", "1234567"],
        None,
        &[],
    );
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    // ⚠ The ECHO: which row this is about, before the outcome.
    assert!(text.contains(&format!("account {}", ids[0])), "{text}");
    assert!(text.contains("venue=dukascopy"), "{text}");
    assert!(text.contains("tier=demo"), "{text}");
    assert!(text.contains("(not yet known) -> 1234567"), "the before and after: {text}");
    assert!(text.contains("written to"), "{text}");

    let out = c.run(
        &["set-book", "--id", &ids[1].to_string(), "--venue-account-id", "7654321"],
        None,
        &[],
    );
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));

    // Both landed, on the right rows, and the listing now tells them apart — while the binance row
    // is untouched, which is what says the write was targeted rather than a sweep.
    let listed = stdout(&c.run(&["accounts"], None, &[]));
    assert!(listed.contains("1234567") && listed.contains("7654321"), "{listed}");
    assert!(listed.contains("1 with no venue account id yet"), "binance must stay blank: {listed}");

    // ⚠ The credential rows are unchanged: this writer writes the account row and nothing else.
    assert_eq!(c.read_store(), before_store, "a credential row was touched");

    // The ledger: two `account_book` records, ids and books only, and no `credential_write`.
    let lines = c.journal_lines();
    let books: Vec<&String> = lines.iter().filter(|l| l.contains("account_book")).collect();
    assert_eq!(books.len(), 2, "one record per write: {lines:?}");
    assert!(books.iter().any(|l| l.contains("1234567")), "{books:?}");
    for line in &books {
        assert!(!line.contains("credential_write"), "an account write was filed as a credential");
        for v in ["login-one", "login-two", "pass-one", "pass-two", "key-one"] {
            assert!(!line.contains(v), "a VALUE reached the ledger: {line}");
        }
    }
}

/// **`--dry-run` prints the row and writes nothing** — the step that answers *is this the account I
/// think it is* before a broker is decided.
#[test]
fn set_book_dry_run_shows_the_row_and_changes_nothing() {
    let c = SetCase::new("set-book-dry");
    let ids = c.seeded_books();

    let out = c.run(
        &["set-book", "--id", &ids[0].to_string(), "--venue-account-id", "1234567", "--dry-run"],
        None,
        &[],
    );
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains(&format!("account {}", ids[0])), "the row must be echoed: {text}");
    assert!(text.contains("DRY RUN"), "{text}");
    // ⚠ **THE STORE, NAMED IN THE REHEARSAL.** It used to appear only on a completed write, so a
    // dry run on the wrong box — the wrong checkout, an inherited $VIKE_SETTINGS_DIR, one ssh hop
    // too far — read exactly like a dry run on the right one, which is the class of mistake the
    // rehearsal exists to catch.
    let db = c.db();
    let db = db.display().to_string();
    assert!(text.contains(&db), "the rehearsal must name the store it would write: {text}");
    assert!(text.contains("store:"), "…up front, before the row: {text}");
    // …and the row's own credential keys — the prefix and the full names, filed against ITS id —
    // and never the other dukascopy row's.
    let own = key_prefix("dukascopy", ids[0]);
    assert!(
        text.contains(&format!("credential keys: {own}")),
        "the echo must identify the row by its keys: {text}"
    );
    assert!(
        text.contains(&vike_secrets::account_credential_name("dukascopy", ids[0], "LOGIN")),
        "…by the full key names too, not only the prefix: {text}"
    );
    assert!(
        !text.contains(&key_prefix("dukascopy", ids[1])),
        "the echo named the OTHER row's keys: {text}"
    );
    for v in ["login-one", "login-two", "pass-one", "pass-two", "key-one"] {
        assert!(!text.contains(v), "a VALUE reached the echo: {text}");
    }

    let listed = stdout(&c.run(&["accounts"], None, &[]));
    assert!(!listed.contains("1234567"), "A DRY RUN WROTE THE BOOK: {listed}");
    // What a rehearsal must add is no `account_book` line.
    assert!(
        !c.journal_lines().iter().any(|l| l.contains("account_book")),
        "a rehearsal recorded an account write: {:?}",
        c.journal_lines()
    );
}

/// **A row that already names a DIFFERENT book is refused, and the refusal says what is at stake.**
///
/// The failure it stops: `--id` is an integer with no roster behind it, so a mistyped one names some
/// OTHER account — and dukascopy's two demo accounts are two legal entities, so overwriting the
/// wrong row's book re-points it at another BROKER with nothing said.
#[test]
fn set_book_refuses_to_repoint_a_row_until_replace_says_so() {
    let c = SetCase::new("set-book-repoint");
    let ids = c.seeded_books();
    let id = ids[0].to_string();

    assert_eq!(
        exit_code(&c.run(&["set-book", "--id", &id, "--venue-account-id", "1234567"], None, &[])),
        rung(vike_cli::exit::Exit::Ok)
    );

    let out = c.run(&["set-book", "--id", &id, "--venue-account-id", "7654321"], None, &[]);
    assert_ne!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "a re-point must be refused");
    let e = stderr(&out);
    assert!(e.contains("1234567"), "the refusal must name the stored book: {e}");
    assert!(e.contains("BROKER"), "…and what is at stake: {e}");
    assert!(e.contains("--replace"), "…and the deliberate way through: {e}");

    let listed = stdout(&c.run(&["accounts"], None, &[]));
    assert!(listed.contains("1234567") && !listed.contains("7654321"), "a refusal wrote: {listed}");
    assert_eq!(
        c.journal_lines().iter().filter(|l| l.contains("account_book")).count(),
        1,
        "a REFUSED write was journalled — only the first, applied, write may be"
    );

    // …and with `--replace` it lands.
    let out =
        c.run(&["set-book", "--id", &id, "--venue-account-id", "7654321", "--replace"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    assert!(stdout(&out).contains("1234567 -> 7654321"), "{}", stdout(&out));
}

/// **A box with no store REFUSES the write and creates no database.**
///
/// The second half is the expensive one: the database opener CREATES a file when the path is empty,
/// so a writer reaching it here would leave a finished, version-stamped store holding one account
/// row and no credential — the creation `vike-cli secrets init` alone may perform.
#[test]
fn set_book_on_an_unmigrated_box_refuses_and_creates_no_database() {
    let c = SetCase::new("set-book-files");

    let out = c.run(&["set-book", "--id", "1", "--venue-account-id", "1234567"], None, &[]);
    assert_ne!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "an absent store must be refused");
    let e = stderr(&out);
    assert!(e.contains("NOTHING WAS WRITTEN"), "{e}");
    assert!(e.contains("secrets init"), "the refusal must name the way through: {e}");

    assert!(!c.db().exists(), "A REFUSED WRITE CREATED THE DATABASE");
    assert!(!c.db().parent().unwrap().exists(), "…nor even the `db/` directory");
    assert!(c.journal_lines().is_empty(), "a refused write was journalled");
}

/// **An `id` no row carries is refused on the USAGE rung and creates no account.**
#[test]
fn set_book_refuses_an_unknown_id_and_creates_no_account() {
    let c = SetCase::new("set-book-unknown-id");
    c.seeded_books();

    let out = c.run(&["set-book", "--id", "9999", "--venue-account-id", "1234567"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let e = stderr(&out);
    assert!(e.contains("9999"), "{e}");
    assert!(e.contains("secrets accounts"), "the refusal must name the listing: {e}");

    let listed = stdout(&c.run(&["accounts"], None, &[]));
    assert!(listed.contains("3 account(s)"), "a refused write created a row: {listed}");
}

/// **The listing SEPARATES the two dukascopy rows** — by the LABEL each was added with and by the
/// credential keys filed against each — which is the property `set-book` is unusable without, and
/// it does it with key NAMES, never a value.
///
/// ⚠ The pair used to be two rows with both labels blank, and `accounts` rendered them as two lines
/// differing only by an opaque integer: an operator asked to write 1234567 to "the Swiss one" had
/// nothing in the output to choose from, and choosing wrongly points an account at the other legal
/// entity — the whole failure this verb exists to prevent. Since decision 0156 item 6 the store
/// refuses a second UNLABELLED row at one `(venue, tier)`, so two accounts of one venue at one tier
/// are LABELLED; and an account's keys are named for the row they are filed against
/// (`vike_secrets::account_credential_name`), so the `credential keys` cell names its own row.
#[test]
fn accounts_tells_the_two_dukascopy_demo_rows_apart_by_label_and_key_names() {
    let c = SetCase::new("accounts-discriminator");
    let ids = c.seeded_books();

    let text = stdout(&c.run(&["accounts"], None, &[]));

    // The premise, asserted rather than assumed: BOTH rows are `dukascopy demo` with no book, so
    // the venue, tier and book cells do not separate them.
    // ⚠ `starts_with(' ')` is what makes this a TABLE-ROW filter rather than a line filter: the
    // `last verified` footer opens with a count and names dukascopy, so it parses as an id and
    // mentions the venue exactly as a row does. Every table row is indented; every footer line is
    // flush left. Same clause, same reason, as `SetCase::account_rows`.
    let rows: Vec<&str> = text
        .lines()
        .filter(|l| {
            l.starts_with(' ')
                && l.split_whitespace().next().and_then(|t| t.parse::<i64>().ok()).is_some()
                && l.contains("dukascopy")
        })
        .collect();
    assert_eq!(rows.len(), 2, "{text}");
    for r in &rows {
        assert!(r.contains("demo") && r.contains("(not yet known)"), "{r}");
    }

    // The LABEL does: each row carries its own in the label cell (`id venue tier label …`).
    let mut labels: Vec<&str> = rows.iter().filter_map(|r| r.split_whitespace().nth(3)).collect();
    labels.sort_unstable();
    assert_eq!(labels, ["EUROPE", "SWISS"], "each row must carry its own label: {text}");

    // …and so do the key names: each row's `credential keys` cell is the owner prefix of the keys
    // filed against THAT row's id, and never the other row's.
    assert!(text.contains("credential keys"), "the column must be headed: {text}");
    for id in &ids {
        let row = rows
            .iter()
            .find(|r| r.split_whitespace().next() == Some(id.to_string().as_str()))
            .unwrap_or_else(|| panic!("no row {id} in {text}"));
        assert!(row.contains(&key_prefix("dukascopy", *id)), "row {id} must show its keys: {row}");
        for other in ids.iter().filter(|o| *o != id) {
            assert!(
                !row.contains(&key_prefix("dukascopy", *other)),
                "row {id} shows row {other}'s keys: {row}"
            );
        }
    }
    // ⚠ …so nothing is called inseparable: the block beneath the table fires only for a
    // `(venue, tier, label)` more than one row shares, and two labelled rows share none.
    assert!(!text.contains("rows share"), "two labelled rows were called inseparable: {text}");

    // ⚠ The scope of an id, printed where somebody is about to write one down.
    assert!(
        text.contains("stable for the life of this database file only"),
        "the listing must say what an id is NOT: {text}"
    );

    // NAMES ONLY — no value on either stream, the same guarantee the reader has by construction.
    for v in ["login-one", "login-two", "pass-one", "pass-two", "key-one"] {
        assert!(!text.contains(v), "a VALUE reached the listing: {text}");
    }
}

/// **A pair written the WRONG WAY ROUND is repairable through the shipped binary** — and it was not
/// before `--clear` existed.
///
/// ⚠ With both rows set and crossed, every direct correction is refused in BOTH directions:
/// `--replace` gets past *this row already names a different book*, and ruling 11's
/// one-account-per-book index then finds the other row. The refusal used to end by telling the
/// operator to "deactivate or correct the other", and NEITHER act was reachable from any command in
/// this tree. This test pins the dead end first, then the way out.
#[test]
fn a_swapped_pair_is_repairable_and_the_refusal_names_the_command_that_does_it() {
    let c = SetCase::new("set-book-swap");
    let ids = c.seeded_books();
    let (a, b) = (ids[0].to_string(), ids[1].to_string());

    // The mistake.
    for (id, book) in [(&a, "7654321"), (&b, "1234567")] {
        let out = c.run(&["set-book", "--id", id, "--venue-account-id", book], None, &[]);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    }

    // THE DEAD END, both directions, with --replace granted.
    for (id, book) in [(&a, "1234567"), (&b, "7654321")] {
        let out =
            c.run(&["set-book", "--id", id, "--venue-account-id", book, "--replace"], None, &[]);
        assert_ne!(
            exit_code(&out),
            rung(vike_cli::exit::Exit::Ok),
            "the crossed state must refuse"
        );
        let e = stderr(&out);
        assert!(e.contains("may not name one book"), "{e}");
        // …and the refusal names a REPAIR THAT EXISTS.
        assert!(e.contains("--clear"), "the refusal must name a command that exists: {e}");
    }

    // The way out: clear, write, write.
    let out = c.run(&["set-book", "--id", &a, "--clear"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("7654321 -> (cleared"), "the echo must say what went: {text}");
    assert!(text.contains("cleared in"), "{text}");

    for (id, book) in [(&b, "7654321"), (&a, "1234567")] {
        let mut argv = vec!["set-book", "--id", id.as_str(), "--venue-account-id", book];
        if id == &b {
            argv.push("--replace");
        }
        let out = c.run(&argv, None, &[]);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    }

    let listed = stdout(&c.run(&["accounts"], None, &[]));
    let row_of = |id: &str| -> String {
        listed
            .lines()
            .find(|l| l.split_whitespace().next() == Some(id))
            .unwrap_or_else(|| panic!("no row {id} in {listed}"))
            .to_string()
    };
    assert!(row_of(&a).contains("1234567"), "{listed}");
    assert!(row_of(&b).contains("7654321"), "{listed}");

    // The ledger reads as a repair: the clear is recorded with its OLD book and no new one.
    let books: Vec<String> =
        c.journal_lines().into_iter().filter(|l| l.contains("account_book")).collect();
    assert_eq!(books.len(), 5, "two mistakes, one clear, two corrections: {books:?}");
    assert!(
        books.iter().any(|l| l.contains("\"old\":\"7654321\"") && !l.contains("\"new\":")),
        "the CLEAR must be recorded as old-without-new: {books:?}"
    );
}

/// **`--clear` and `--venue-account-id` are mutually exclusive, and so are `--clear` and
/// `--replace`** — refused by the shipped binary on the USAGE rung, with nothing written.
#[test]
fn clear_refuses_a_value_and_refuses_replace_on_the_shipped_binary() {
    let c = SetCase::new("set-book-clear-conflicts");
    let ids = c.seeded_books();
    let id = ids[0].to_string();
    assert_eq!(
        exit_code(&c.run(&["set-book", "--id", &id, "--venue-account-id", "1234567"], None, &[])),
        rung(vike_cli::exit::Exit::Ok)
    );

    for argv in [
        vec!["set-book", "--id", id.as_str(), "--clear", "--venue-account-id", "7654321"],
        vec!["set-book", "--id", id.as_str(), "--clear", "--replace"],
    ] {
        let out = c.run(&argv, None, &[]);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{argv:?}");
        assert!(stderr(&out).contains("Nothing was written"), "{}", stderr(&out));
    }

    // …and the row is exactly as it was.
    let listed = stdout(&c.run(&["accounts"], None, &[]));
    assert!(listed.contains("1234567"), "a refused command changed the row: {listed}");
}

/// **An invisible character pasted into the middle of the number is REFUSED, and the refusal never
/// echoes it.**
///
/// ⚠ `char::is_control` is category Cc alone and `is_whitespace` is `White_Space`, so neither
/// classifies a zero-width space — and `trim` does not strip one. A value pasted off a venue's web
/// page with one inside it used to be STORED: identical to another row's book on every screen, and
/// unequal to it in the index that is the only thing keeping two accounts of one venue off one
/// book. An edge one is a paste artefact and is trimmed; an interior one has no benign reading.
#[test]
fn set_book_refuses_an_interior_invisible_character_and_trims_an_edge_one() {
    let c = SetCase::new("set-book-invisible");
    let ids = c.seeded_books();
    let id = ids[0].to_string();

    let out = c.run(&["set-book", "--id", &id, "--venue-account-id", "123\u{200B}4567"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let e = stderr(&out);
    assert!(e.contains("invisible"), "the refusal must say what is most likely wrong: {e}");
    assert!(!e.contains('\u{200B}'), "the refusal echoed the token: {e:?}");
    let listed = stdout(&c.run(&["accounts"], None, &[]));
    assert!(!listed.contains("4567"), "a refused value was written: {listed}");

    // …and the same character at the EDGE is trimmed, storing the clean book.
    let out = c.run(&["set-book", "--id", &id, "--venue-account-id", "\u{FEFF}1234567"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let listed = stdout(&c.run(&["accounts"], None, &[]));
    let row = listed
        .lines()
        .find(|l| l.split_whitespace().next() == Some(id.as_str()))
        .unwrap_or_else(|| panic!("no row {id} in {listed}"));
    assert!(row.contains("1234567"), "{row}");
    assert!(!row.contains('\u{FEFF}'), "the byte-order mark was STORED: {row:?}");
}
