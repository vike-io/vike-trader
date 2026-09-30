//! **The `account` table gets a WRITER for one column** — `venue_account_id`, the BOOK as the venue
//! names it — and every test here is about a way that writer could be wrong while every other test
//! in this crate stayed green.
//!
//! `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §4.5 records this column as one
//! *nothing in this tree writes*, and names two writers it should eventually have: the migration's
//! FOLD of the ten stored keys that already are the book (§11 step 3) and the venue's own
//! HANDSHAKE (§12). `vike_secrets::set_venue_account_id` is NEITHER. It is the third source, for
//! the one case those two cannot reach: a book that is in no key and derivable from nothing in the
//! store.
//!
//! ⚠ **That case is dukascopy, and it is the whole reason this file exists.** After a migration the
//! two dukascopy demo accounts are `(dukascopy, demo, label = NULL)` twice over — `UNIQUE (venue,
//! tier, label)` does not separate them, because NULLs are distinct in SQLite — so `id` is the only
//! handle that tells them apart, and nothing records which row is which BOOK. They are two legal
//! entities (Dukascopy Bank SA and Dukascopy Europe IBS AS), so a book written to the wrong row
//! routes orders to the wrong BROKER.
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`the_two_dukascopy_rows_learn_different_books_and_stay_two_rows`] | the premise: two accounts a name cannot separate, separated and then IDENTIFIED |
//! | [`writing_the_same_book_twice_writes_nothing_and_says_so`] | a re-run reported as a change, or refused as a conflict with itself |
//! | [`a_row_that_names_a_different_book_is_refused_until_replace_says_so`] | ⚠ the one that matters most: a mistyped id silently re-pointing an armed account at another broker |
//! | [`two_active_accounts_of_one_venue_may_not_name_one_book`] | ruling 11 (§8) arriving as an opaque `UNIQUE constraint failed`, or not at all |
//! | [`an_id_no_row_carries_is_refused_and_creates_no_account`] | a typo inventing an account, and a book landing on one nobody has |
//! | [`a_file_store_is_refused_and_no_database_is_created`] | the per-KEY fallback `Backend` forbids — or worse, a MINTED database that retires every credential on the box |
//! | [`a_schema_1_store_says_it_predates_the_account_table`] | an unmigrated box handed the engine's own `no such table` as if the store had malfunctioned |
//! | [`a_malformed_book_is_refused_and_the_refusal_echoes_no_token`] | a pasted secret quoted back into the terminal by the refusal that was meant to protect it |
//! | [`the_write_touches_no_credential_no_file_and_no_other_column`] | a writer that "tidied" a neighbouring column, a row, or the credential file |
//! | [`the_normalizer_trims_the_paste_and_refuses_everything_else`] | `"4100017 "` and `"4100017"` colliding — or NOT colliding — by an invisible byte |
//! | [`an_invisible_character_is_trimmed_at_the_edge_and_refused_in_the_middle`] | ⚠ a pasted byte-order mark STORED: two books identical on every screen, different to the index |
//! | [`a_swapped_pair_is_repaired_by_clearing_one_row_first`] | ⚠ a pair written the wrong way round with no repair in the tree — refused in BOTH directions, forever |
//! | [`a_clear_is_idempotent_and_local`] | a clear that needed permission it cannot need, or that reached a neighbouring row |
//! | [`a_clear_of_a_missing_row_is_refused_and_creates_nothing`] | the create this writer may never perform, reached through the one path that takes no value |
//! | [`the_key_names_are_what_tell_the_two_identical_rows_apart`] | ⚠ the premise of the whole verb: two rows an operator CANNOT choose between, and a writer addressed by a handle nobody can resolve |
//! | [`a_file_store_cannot_be_keyed_and_says_so_rather_than_answering_empty`] | *no keys* and *no table* merged into one blank column |
//!
//! ⚠ **Every value in this file is FICTIONAL, the two dukascopy books included.** An earlier draft
//! carried the owner's real measured numbers and his two real demo LOGIN ids, on the stated grounds
//! that a test is not published. **That ground is false and was measured:**
//! `scripts/publish_mirror.sh`'s `ALLOW` carries `crates` wholesale and its exclusions reach
//! `crates/vike-ops/tests` plus a handful of named files — this path is in neither, so this file
//! ships to the public mirror like any source file. The login half was the worse of the two: that
//! venue's password convention is the login's own last five characters, so publishing a login
//! publishes its password.
//!
//! Nothing here needs the real values. What the test proves is that the STORE can hold two
//! DIFFERENT books on two rows no name can separate — a property of any two distinct strings. The
//! real mapping lives in `docs/superpowers/specs/2026-09-14-the-credential-schema.md`, and `docs/`
//! is never published.

use std::path::{Path, PathBuf};

use vike_secrets::{Accounts, Backend, DbErrorKind};

// ---------------------------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------------------------

/// Eight names: dukascopy's two indistinguishable demo accounts, an ordinary single account, a
/// second account of that same venue at another TIER, and one key that owns no account at all.
const FIXTURE_KEYS: [&str; 8] = [
    "BINANCE_DEMO_API_KEY",
    "BINANCE_DEMO_API_SECRET",
    "BINANCE_LIVE_API_KEY",
    "CLOUDFLARE_API_TOKEN",
    "DUKASCOPY_DEMO1_LOGIN",
    "DUKASCOPY_DEMO1_PASSWORD",
    "DUKASCOPY_DEMO2_LOGIN",
    "DUKASCOPY_DEMO2_PASSWORD",
];

/// Two FICTIONAL books, standing in for the pair a dukascopy store actually holds.
///
/// `DUKASCOPY_DEMO1_*` is Dukascopy Bank SA and `DUKASCOPY_DEMO2_*` is Dukascopy Europe IBS AS —
/// two different legal entities, which is why writing a book to the wrong row is a routing error
/// and not a labelling one. That distinction is the whole subject of this file, and it needs no
/// real number: what is proved is that the STORE can hold two DIFFERENT books on two rows no name
/// can separate, which is a property of any two distinct strings.
///
/// ⚠ **Do not paste the owner's measured numbers or logins back in.** This file is published —
/// see the module doc for the measurement — and that venue's password is the login's own last five
/// characters. The real mapping belongs in
/// `docs/superpowers/specs/2026-09-14-the-credential-schema.md`, which is never published.
const DEMO1_BOOK: &str = "4100017";
const DEMO2_BOOK: &str = "4100023";

fn is_node_key(_key: &str) -> bool {
    false
}

/// The account classification the production caller passes
/// (`vike_bridge_core::credentials::classify_credential_name`), spelled here because this crate
/// cannot link the crate that owns it — the same seam, and the same reason, as
/// `tests/account_reader.rs`' own copy. Deliberately simpler than the production rule: every
/// assertion below is about what the WRITER does to an `account` table, never about how a name
/// reached one.
fn classify(name: &str) -> vike_secrets::Classification {
    use vike_secrets::{AccountKey, Classification, Placement};

    let account = |venue: &str, tier: &str, disc: Option<&str>, field: &str| Classification {
        placement: Placement::Account(AccountKey {
            venue: venue.to_string(),
            tier: tier.to_string(),
            // ⚠ NEVER a label. The owner refused the provisional `DEMO1`/`DEMO2` spellings at the
            // spec's signature — *labels are informative and optional, `id` is the identity*.
            label: None,
            discriminator: disc.map(str::to_string),
        }),
        field: field.to_string(),
        secret: true,
        recognised: true,
        pending_move: None,
    };

    for (prefix, disc) in [("DUKASCOPY_DEMO1_", "DEMO1"), ("DUKASCOPY_DEMO2_", "DEMO2")] {
        if let Some(field) = name.strip_prefix(prefix) {
            return account("dukascopy", "demo", Some(disc), field);
        }
    }
    for (prefix, venue, tier) in
        [("BINANCE_DEMO_", "binance", "demo"), ("BINANCE_LIVE_", "binance", "live")]
    {
        if let Some(field) = name.strip_prefix(prefix) {
            return account(venue, tier, None, field);
        }
    }
    // `CLOUDFLARE_API_TOKEN` lands here: infrastructure, no venue, no account row.
    Classification::unrecognised(name)
}

fn fake_value(key: &str) -> String {
    format!("value-for-{key}")
}

struct Fixture {
    _dir: tempfile::TempDir,
    settings: PathBuf,
}

impl Fixture {
    /// A settings directory holding a credential FILE with [`FIXTURE_KEYS`] and no database.
    fn file_store() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = dir.path().join("settings");
        std::fs::create_dir_all(&settings).expect("settings dir");
        std::fs::write(settings.join("secrets.env"), Fixture::store_text())
            .expect("write fixture store");
        Fixture { _dir: dir, settings }
    }

    fn store_text() -> String {
        FIXTURE_KEYS.iter().map(|k| format!("{k}={}\n", fake_value(k))).collect()
    }

    /// …and the same directory after `vike-cli secrets migrate` would have run over it.
    fn migrated() -> Fixture {
        let fx = Fixture::file_store();
        match vike_secrets::migrate(fx.arg(), is_node_key, &classify) {
            Ok(_) => fx,
            Err(e) => panic!("migration refused: {e}"),
        }
    }

    fn arg(&self) -> Option<&str> {
        Some(self.settings.to_str().expect("utf-8 temp path"))
    }

    fn dir(&self) -> &Path {
        &self.settings
    }

    fn db(&self) -> PathBuf {
        self.settings.join("db").join("vike.db")
    }

    fn store(&self) -> PathBuf {
        self.settings.join("secrets.env")
    }

    fn accounts(&self) -> Vec<vike_secrets::Account> {
        match vike_secrets::resolve_accounts_in(self.dir()).expect("the store opened") {
            Accounts::Known(rows) => rows,
            Accounts::Unanswerable(why) => panic!("the store could not be asked: {why}"),
        }
    }

    /// The two dukascopy demo rows, `id`-ordered — the pair this whole file is about.
    fn dukascopy_ids(&self) -> (i64, i64) {
        let rows = self.accounts();
        let duka: Vec<&vike_secrets::Account> =
            rows.iter().filter(|a| a.venue == "dukascopy").collect();
        assert_eq!(duka.len(), 2, "the fixture must carry TWO dukascopy accounts: {duka:?}");
        (duka[0].id, duka[1].id)
    }

    fn book_of(&self, id: i64) -> Option<String> {
        self.accounts()
            .into_iter()
            .find(|a| a.id == id)
            .unwrap_or_else(|| panic!("no account {id}"))
            .venue_account_id
    }

    fn verified_of(&self, id: i64) -> Option<String> {
        self.accounts()
            .into_iter()
            .find(|a| a.id == id)
            .unwrap_or_else(|| panic!("no account {id}"))
            .last_verified_at
    }

    /// The write, through the door a production caller uses — the Backend-aware router, never the
    /// db function directly, so every test here exercises the store choice as well as the write.
    ///
    /// `BookSource::Operator`, which is what every test below that is not about the handshake wants:
    /// it is the door `vike-cli secrets set-book` uses, and it leaves `last_verified_at` alone.
    fn set_book(
        &self,
        id: i64,
        book: &str,
        replace: bool,
    ) -> Result<vike_secrets::BookWrite, vike_secrets::DbError> {
        vike_secrets::set_venue_account_id_in(
            self.dir(),
            id,
            Some(book),
            replace,
            vike_secrets::BookSource::Operator,
        )
    }

    /// The same write claiming to be a VENUE HANDSHAKE rather than an operator — the one source
    /// that may stamp `last_verified_at`.
    fn confirm_book(
        &self,
        id: i64,
        book: &str,
        at: &str,
    ) -> Result<vike_secrets::BookWrite, vike_secrets::DbError> {
        vike_secrets::set_venue_account_id_in(
            self.dir(),
            id,
            Some(book),
            false,
            vike_secrets::BookSource::Handshake { verified_at: at },
        )
    }

    /// …and the CLEAR, through the same door. `None` is the whole of the difference.
    fn clear_book(&self, id: i64) -> Result<vike_secrets::BookWrite, vike_secrets::DbError> {
        vike_secrets::set_venue_account_id_in(
            self.dir(),
            id,
            None,
            false,
            vike_secrets::BookSource::Operator,
        )
    }

    /// The credential key names each row owns, as the listing renders them.
    fn keys_of(&self, id: i64) -> vike_secrets::AccountKeys {
        let mut map = vike_secrets::resolve_account_keys_in(self.dir())
            .expect("the store opened")
            .expect("a migrated store can be keyed");
        map.remove(&id).unwrap_or_default()
    }
}

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

/// **A `Backend::Files` box is REFUSED, and no database is created.**
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
        Backend::Files,
        "the fixture must be unmigrated"
    );

    let err = fx.set_book(1, DEMO1_BOOK, false).expect_err("a file store has no account table");
    match &err.kind {
        DbErrorKind::NoDatabase { file } => {
            assert_eq!(file, &fx.store(), "the refusal must name the store that IS answering");
        }
        other => panic!("expected NoDatabase, got {other:?}"),
    }
    let text = err.to_string();
    assert!(text.contains("secrets migrate"), "the refusal must name the way through: {text}");
    assert!(text.contains("NOTHING WAS WRITTEN"), "{text}");

    assert!(
        !fx.db().exists(),
        "A REFUSED WRITE CREATED THE DATABASE — every credential on this \
                                box would stop being read"
    );
    assert!(!fx.db().parent().unwrap().exists(), "…nor even the `db/` directory");
    assert_eq!(
        std::fs::read_to_string(fx.store()).unwrap(),
        Fixture::store_text(),
        "the credential file was touched"
    );
    assert_eq!(vike_secrets::backend_in(fx.dir()), Backend::Files, "the backend choice moved");
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

// ---------------------------------------------------------------------------------------------
// PROOF 4 — nothing here is, or can print, a credential
// ---------------------------------------------------------------------------------------------

/// **A malformed book is refused and the refusal echoes NO token.**
///
/// The commonest way to reach this arm is pasting something into the wrong flag, and a refusal that
/// helpfully quoted the offending value would write a credential into the terminal scrollback of
/// the very session it exists to protect — the same reasoning as `vike-cli`'s `ARGV_VALUE_REFUSAL`.
#[test]
fn a_malformed_book_is_refused_and_the_refusal_echoes_no_token() {
    let fx = Fixture::migrated();
    let (first, _) = fx.dukascopy_ids();

    for bad in ["", "   ", "sk-live-abcdef", "two tokens", "line\nbreak", "tab\there"] {
        // `sk-live-abcdef` is a single token and IS accepted by the predicate — it is included here
        // to make the echo assertion below cover the one input an operator most regrets.
        let result = fx.set_book(first, bad, false);
        match result {
            Err(e) => {
                let text = e.to_string();
                assert!(
                    !text.contains(bad) || bad.trim().is_empty(),
                    "the refusal echoed the offered token: {text}"
                );
            }
            Ok(done) => {
                assert_eq!(
                    bad, "sk-live-abcdef",
                    "only the single-token case may be accepted; {bad:?} was"
                );
                // …and undo it, so the loop's later cases still see a row with a known book.
                assert!(done.changed);
                fx.set_book(first, DEMO1_BOOK, true).expect("restore");
            }
        }
    }

    // The refusal's own words, on a case that is unambiguously malformed.
    let err = fx.set_book(first, "two tokens", false).expect_err("two tokens is not one");
    assert!(matches!(err.kind, DbErrorKind::BookMalformed), "{:?}", err.kind);
    let text = err.to_string();
    assert!(text.contains("ONE token"), "{text}");
    assert!(!text.contains("two tokens"), "the refusal echoed the token: {text}");
}

/// **The write touches no credential, no file, and no other column of the row.**
///
/// Four claims in one test because they are one property: this is a targeted `UPDATE` of one column
/// and nothing else. `last_verified_at` in particular stays `None` — §4.5 gives that column to *a
/// successful authenticated session*, and an operator typing a number read off a web page has
/// performed none, so stamping it would make a hand-entered row indistinguishable from a confirmed
/// one.
#[test]
fn the_write_touches_no_credential_no_file_and_no_other_column() {
    let fx = Fixture::migrated();
    let (first, _) = fx.dukascopy_ids();
    let before_file = std::fs::read_to_string(fx.store()).unwrap();
    let before_row = fx.accounts().into_iter().find(|a| a.id == first).expect("the row");
    let before_keys: Vec<String> =
        vike_secrets::read_table(&fx.db(), vike_secrets::Table::Credential)
            .expect("the credential table")
            .keys()
            .map(str::to_string)
            .collect();

    let done = fx.set_book(first, DEMO1_BOOK, false).expect("the write lands");

    // 1. The credential file is byte-identical — this writer never opens it in any branch.
    assert_eq!(std::fs::read_to_string(fx.store()).unwrap(), before_file);

    // 2. Every credential name survives, and the map still answers.
    let after_keys: Vec<String> =
        vike_secrets::read_table(&fx.db(), vike_secrets::Table::Credential)
            .expect("the credential table")
            .keys()
            .map(str::to_string)
            .collect();
    assert_eq!(after_keys, before_keys, "a credential row moved");

    // 3. Only `venue_account_id` changed on the row.
    let after_row = fx.accounts().into_iter().find(|a| a.id == first).expect("the row");
    assert_eq!(after_row.venue, before_row.venue);
    assert_eq!(after_row.tier, before_row.tier);
    assert_eq!(after_row.label, before_row.label, "a label was written");
    assert_eq!(after_row.parent_id, before_row.parent_id);
    assert_eq!(after_row.active, before_row.active);
    assert_eq!(
        after_row.last_verified_at, None,
        "last_verified_at belongs to a successful authenticated SESSION, not to a hand-entered book"
    );
    assert_eq!(after_row.venue_account_id.as_deref(), Some(DEMO1_BOOK));

    // 4. Nothing the writer returns is a credential value.
    let rendered = format!("{done:?}");
    for key in FIXTURE_KEYS {
        assert!(
            !rendered.contains(&fake_value(key)),
            "a credential value reached the write's own Debug: {rendered}"
        );
    }
}

/// **The normalizer trims the paste and refuses everything else** — the ONE predicate the CLI's
/// early refusal and the store's write-path refusal both ask.
///
/// It is a pure function and it is tested as one, because everything above depends on it agreeing
/// with itself: `"4100017 "` and `"4100017"` must be the same book, or `account_one_account_per_book`
/// would be deciding a collision by an invisible byte.
#[test]
fn the_normalizer_trims_the_paste_and_refuses_everything_else() {
    use vike_secrets::normalized_venue_account_id as norm;

    assert_eq!(norm("4100017").as_deref(), Some("4100017"));
    assert_eq!(norm("  4100017 \n").as_deref(), Some("4100017"), "a pasted value is trimmed");
    assert_eq!(norm("DU186573").as_deref(), Some("DU186573"), "IBKR's shape");
    assert_eq!(
        norm("0x1234567890abcdef1234567890abcdef12345678").as_deref(),
        Some("0x1234567890abcdef1234567890abcdef12345678"),
        "an EVM address is the widest real shape"
    );

    for bad in ["", "   ", "\n", "two tokens", "a\tb", "a\nb", "with\u{0}nul"] {
        assert_eq!(norm(bad), None, "{bad:?} must be refused");
    }
    let too_long = "9".repeat(vike_secrets::VENUE_ACCOUNT_ID_MAX_BYTES + 1);
    assert_eq!(norm(&too_long), None, "a whole file pasted into the flag must not become a row");
    let at_the_cap = "9".repeat(vike_secrets::VENUE_ACCOUNT_ID_MAX_BYTES);
    assert!(norm(&at_the_cap).is_some(), "the cap itself is accepted");
}

/// **The INVISIBLE characters a paste carries — trimmed at the edges, REFUSED in the middle.**
///
/// ⚠ The predicate this file tested above used to be `is_whitespace() || is_control()`, and that
/// pair classifies **none of Unicode's FORMAT characters**: `char::is_control` is category Cc alone,
/// `is_whitespace` is the `White_Space` property, and `str::trim` strips only the second. So
/// U+FEFF (a byte-order mark), U+200B (a zero-width space) and U+200E (a left-to-right mark) all
/// passed, and a number pasted off a venue's own web page with a leading BOM was STORED with it.
///
/// That is not cosmetic here and is not a display bug. This column is the value
/// `account_one_account_per_book` compares two rows by, so `"\u{FEFF}4100017"` and `"4100017"`
/// render identically on every screen an operator can read and are DIFFERENT to the index: the one
/// rule standing between two accounts of one venue and one book would be defeated by a character
/// nobody can see, on the one venue where the two accounts are two legal entities.
///
/// Both halves are asserted, because they are different answers on purpose: an edge invisible is
/// what a paste actually produces and is trimmed (the operator gets the book they meant), while an
/// INTERIOR one is refused outright (there is no reading of it that is the operator's intent).
#[test]
fn an_invisible_character_is_trimmed_at_the_edge_and_refused_in_the_middle() {
    use vike_secrets::normalized_venue_account_id as norm;

    // Built from [`DEMO1_BOOK`] rather than re-typed: the number is one operator's account data and
    // this file already carries it once, as the subject.
    let edged = |lead: &str, trail: &str| format!("{lead}{DEMO1_BOOK}{trail}");
    let (head, tail) = DEMO1_BOOK.split_at(3);
    let inside = |c: &str| format!("{head}{c}{tail}");

    // The edge: the paste artefacts. Each must yield the clean book, byte-identical to the
    // hand-typed one — which is the property that keeps the index honest.
    for (raw, what) in [
        (edged("\u{FEFF}", ""), "a leading byte-order mark"),
        (edged("", "\u{FEFF}"), "a trailing byte-order mark"),
        (edged("\u{200B}", "\u{200B}"), "zero-width spaces at both ends"),
        (edged("\u{200E}", ""), "a left-to-right mark"),
        (edged("\u{00AD}", " "), "a soft hyphen and a space"),
        (edged("\u{2066}", "\u{2069}"), "a bidi isolate wrapping the number"),
    ] {
        assert_eq!(norm(&raw).as_deref(), Some(DEMO1_BOOK), "{what} must be trimmed away");
    }

    // The middle: no reading of this is the operator's intent, and storing it would make two books
    // that look the same compare unequal.
    for (raw, what) in [
        (inside("\u{FEFF}"), "a byte-order mark inside the number"),
        (inside("\u{200B}"), "a zero-width space inside the number"),
        (inside("\u{200E}"), "a left-to-right mark inside the number"),
    ] {
        assert_eq!(norm(&raw), None, "{what} must be REFUSED, never silently stored");
    }

    // …and the homoglyph case the ASCII rule closes for free: a Cyrillic `о` renders exactly like a
    // Latin `o` and is a different book to the index.
    assert_eq!(norm("DU18657\u{043E}"), None, "a Cyrillic letter must not pass for a Latin one");

    // ⚠ The measurement behind the whole test: the OLD predicate accepted every case above, edge
    // and interior alike — which is why `is_whitespace() || is_control()` is not the predicate any
    // more. `char::is_control` is category Cc alone and `is_whitespace` is `White_Space`; Cf is in
    // neither, and `str::trim` strips only the second.
    for raw in [edged("\u{FEFF}", ""), edged("", "\u{200B}"), inside("\u{200B}")] {
        assert!(
            !raw.trim().chars().any(|c| c.is_whitespace() || c.is_control()),
            "{raw:?} passes `trim` + is_whitespace/is_control"
        );
    }
}

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
/// The FOLD never reaches this branch — `vike_model::account_confirmation::verdict` classifies the
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

/// **A `Backend::Files` box refuses the handshake source exactly as it refuses the operator's.**
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
        matches!(err.kind, vike_secrets::DbErrorKind::NoDatabase { .. }),
        "a file store must refuse by NAME, never fall back: {err:?}"
    );
}
