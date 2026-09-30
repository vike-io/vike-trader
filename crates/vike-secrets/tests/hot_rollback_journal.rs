//! **What the read-only opener does when a rollback journal survives a killed writer.**
//!
//! `docs/decisions/0054-settings-move-into-one-database.md` moves the settings into this store, and
//! decision record 0057 — the accepted extension that answers the seven settings files one at a
//! time, not yet on `main` and therefore deliberately not cited here by path — names this as one of
//! two measurements it *"leans on nobody"* and requires before its Phase 1 is trusted. It states
//! the answer as DERIVED rather than measured. This file is the measurement, and it is here rather
//! than in a report because the property it pins is one a dependency bump can silently change: the
//! whole read half of that migration rests on `crates/vike-secrets/src/db.rs`'s `open_for_read`
//! being able to answer with the daemon's `settings/` mounted read-only, and a hot journal is the
//! one on-disk state in which it cannot.
//!
//! # The state under test, and why it is built by COPY rather than by a kill
//!
//! `journal_mode = DELETE` writes `vike.db-journal` beside the database for the duration of a write
//! and removes it on a clean close — `crates/vike-secrets/tests/database_migration.rs`'s
//! `no_sidecar_survives_a_clean_close` is the at-rest half of that. A writer that dies mid-write
//! leaves the journal behind, and if the database file was already modified the journal is **HOT**:
//! it must be replayed before anything may read the file.
//!
//! MEASURED on the the CI box box (2026-09-15, rusqlite 0.40.2 / libsqlite3-sys 0.38.2, the workspace's
//! own pins) by SIGKILLing a real writer, in two shapes:
//!
//! * a killed writer whose transaction had **not yet touched the database file** leaves a journal
//!   whose first eight bytes are ZERO. That is SQLite marking it not-yet-valid, and a read-only
//!   open of that store succeeds and returns the last committed rows. This is the common case for a
//!   store this small, and it is not a failure at all.
//! * a killed writer whose dirty pages had reached the database file — because the transaction
//!   outgrew the page cache, or because the kill landed inside the COMMIT — leaves a journal
//!   carrying SQLite's real header magic. **That** is the state this file builds.
//!
//! Killing a process is not a thing a test in this workspace should do, and racing a commit is not a
//! thing it should depend on. So the hot pair is built by taking a byte COPY of the database and its
//! journal while a transaction is in flight, which reaches the identical on-disk state with no
//! signals, no child process and no timing: [`hot_pair`]. The copy was verified against the killed
//! writer on the same box and produces the same journal state and the same error.
//!
//! # What this pins
//!
//! The refusal is **immediate, total and NOT the busy arm.** `crates/vike-secrets/src/db.rs`'s
//! `DbError::sql` classifies `DatabaseBusy` and `DatabaseLocked` into the named
//! `crates/vike-secrets/src/db.rs`'s `DbErrorKind::StoreBusy`, whose message tells the operator
//! nothing was written and to run the command again. A hot journal is neither of those codes — it is
//! `SQLITE_READONLY_ROLLBACK` — so it falls through to `crates/vike-secrets/src/db.rs`'s
//! `DbErrorKind::Sqlite` and the operator is handed the engine's own six words, *attempt to write a
//! readonly database*, about a store whose permissions are perfect and which nothing in the daemon's
//! own mount namespace can repair.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

/// SQLite's rollback-journal header magic. A journal carrying it is one the engine must replay; a
/// journal whose first eight bytes are zero is one the engine has deliberately marked not-yet-valid.
const JOURNAL_MAGIC: [u8; 8] = [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];

/// A settings directory holding a real, migrated store — built through the production path
/// (`vike_secrets::migrate`) so the schema, the `PRAGMA user_version` stamp and the modes are the
/// ones a live box carries.
struct Store {
    _dir: tempfile::TempDir,
    settings: PathBuf,
}

impl Store {
    fn new() -> Store {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = dir.path().join("settings");
        std::fs::create_dir_all(&settings).expect("settings dir");
        std::fs::write(
            settings.join("secrets.env"),
            "BINANCE_DEMO_API_KEY=k\nBINANCE_DEMO_API_SECRET=s\nBYBIT_DEMO_API_KEY=k2\n",
        )
        .expect("write credential file");
        let store = Store { _dir: dir, settings };
        let classify = |name: &str| vike_secrets::Classification::unrecognised(name);
        match vike_secrets::migrate(store.arg(), |_| false, &classify) {
            Ok(_) => store,
            Err(e) => panic!("migration refused: {e}"),
        }
    }

    fn arg(&self) -> Option<&str> {
        Some(self.settings.to_str().expect("utf-8 temp path"))
    }

    fn db(&self) -> PathBuf {
        self.settings.join("db").join("vike.db")
    }

    fn journal(&self) -> PathBuf {
        self.settings.join("db").join("vike.db-journal")
    }
}

/// Leave `dst` holding a database and a HOT rollback journal.
///
/// `PRAGMA cache_size = 1` is what makes this deterministic: it forces the transaction's dirty pages
/// out of the cache and into the database FILE before the commit, which is the condition that makes
/// SQLite stamp the journal's real header magic. The pair is copied out while that transaction is
/// still open, so `dst` is exactly what a box finds after a writer died at that instant — and no
/// process ever held a lock on `dst` itself, so the journal there is nobody's live write.
fn hot_pair(src: &Store, dst: &Path) {
    std::fs::create_dir_all(dst).expect("dst dir");
    {
        let conn = Connection::open(src.db()).expect("open write connection");
        let mode: String = conn
            .query_row("PRAGMA journal_mode = DELETE", [], |r| r.get(0))
            .expect("set journal mode");
        assert!(mode.eq_ignore_ascii_case("delete"), "journal_mode answered {mode}");
        conn.execute_batch("PRAGMA cache_size = 1;").expect("shrink the page cache");
        conn.execute_batch("BEGIN IMMEDIATE").expect("begin");
        {
            let mut st = conn
                .prepare(
                    "INSERT INTO credential (name, field, value, venue) \
                     VALUES (?1, 'api_key', ?2, 'binance')",
                )
                .expect("prepare");
            for i in 0..20_000 {
                st.execute(rusqlite::params![format!("INFLIGHT_{i}"), format!("v{i}")])
                    .expect("insert");
            }
        }
        assert!(src.journal().is_file(), "the in-flight write left no journal to copy");
        std::fs::copy(src.db(), dst.join("vike.db")).expect("copy database");
        std::fs::copy(src.journal(), dst.join("vike.db-journal")).expect("copy journal");
        // The connection is dropped here, which rolls the source transaction back and removes the
        // SOURCE journal. `dst` keeps the pair.
    }
    let head = std::fs::read(dst.join("vike.db-journal")).expect("read copied journal");
    assert!(
        head.len() >= 8 && head[..8] == JOURNAL_MAGIC,
        "the copied journal is not HOT — its first eight bytes are {:02x?}, not SQLite's header \
         magic, so this test would be asserting about a state the engine does not consider a \
         rollback. The engine's behaviour here has changed; re-take the measurement rather than \
         relaxing this assertion",
        &head[..head.len().min(8)]
    );
}

/// The daemon's read of the store, against `settings` — `vike_secrets::resolve_project` is what
/// `vike_bridge_core::credentials::try_load_workspace_secrets_at` calls, and it reaches
/// `crates/vike-secrets/src/db.rs`'s `open_for_read` through
/// `crates/vike-secrets/src/store.rs`'s `Backend`.
fn read_as_the_daemon(settings: &Path) -> Result<vike_secrets::Resolved, String> {
    vike_secrets::resolve_project(Some(settings.to_str().expect("utf-8 temp path")))
        .map_err(|e| e.to_string())
}

/// Copy a hot pair into a settings tree of its own, so the read goes through the ordinary
/// project-settings door rather than at a bare path.
fn hot_store(src: &Store, into: &tempfile::TempDir) -> PathBuf {
    let settings = into.path().join("settings");
    hot_pair(src, &settings.join("db"));
    settings
}

#[test]
fn a_surviving_rollback_journal_refuses_the_whole_store_to_a_read_only_opener() {
    let src = Store::new();
    assert!(read_as_the_daemon(&src.settings).is_ok(), "the baseline store must read");

    let holder = tempfile::tempdir().expect("tempdir");
    let settings = hot_store(&src, &holder);

    // The Ok side is folded to a COUNT rather than carried: `expect_err` renders it on failure, and
    // nothing in this file needs a credential map in a panic message.
    let err = read_as_the_daemon(&settings)
        .map(|r| r.secrets.len())
        .expect_err("a store with a HOT rollback journal must NOT read as an ordinary store");

    assert!(
        err.contains("attempt to write a readonly database"),
        "the engine's verbatim answer is what the operator is handed; got: {err}"
    );
    // The whole store, not one row: nothing is returned partially.
    assert!(err.contains("vike.db"), "the refusal must name the store; got: {err}");
}

#[test]
fn the_whole_operator_facing_string_is_pinned_because_its_opacity_is_the_finding() {
    let src = Store::new();
    let holder = tempfile::tempdir().expect("tempdir");
    let settings = hot_store(&src, &holder);
    let db = settings.join("db").join("vike.db");

    let err = read_as_the_daemon(&settings)
        .map(|r| r.secrets.len())
        .expect_err("hot journal must refuse");

    // ⚠ **This pin was written against the DEFECT and is now pinned against the FIX.** What it used
    // to hold, verbatim, was:
    //
    //     credential store {db} could not be read: settings database {db} could not be read:
    //     attempt to write a readonly database
    //
    // Three `Display` impls composed that and none knew what the one below it had already said, so
    // the store was named TWICE, *could not be read* appeared twice, and an operator whose
    // permissions were perfect was told a write had been attempted. The pin existed so that a
    // message nobody had written down could not stay a message nobody improved.
    //
    // It has been improved, and the assertion moves with it rather than being deleted — deleting it
    // would give the next opacity nowhere to be noticed. What a reader should check here is that the
    // store is still named, that the engine's own confusing six words are QUOTED and disowned rather
    // than left to speak for themselves, and that the repair is named in the same breath.
    let db = db.display();
    assert_eq!(
        err,
        format!(
            "credential store {db} could not be read: a writer of the settings database was \
             killed mid-write and left SQLite's rollback journal at {db}-journal. NOTHING IS \
             CORRUPT and nothing is lost: that journal holds the pages needed to put the database \
             back to its last committed state, and the engine REPLAYS it — and deletes it — on the \
             first read-WRITE open. What was refused is the READ-ONLY open, which may not write and \
             therefore may not replay. ⚠ The engine's own words for this are `attempt to write a \
             readonly database`; nothing tried to write, and your file permissions are not the \
             problem. ⚠ A DAEMON CANNOT REPAIR THIS ITSELF — it opens this store read-only and its \
             settings directory is read-only in its own mount namespace, so restarting it just \
             fails the same open again. Repair it from an OPERATOR SHELL, where the directory is \
             writable: ANY `vike-cli secrets` command opens the store read-write and replays the \
             journal, so `vike-cli secrets list` is enough. Then restart the daemon."
        )
    );
}

#[test]
fn the_refusal_is_not_the_named_busy_arm_so_it_names_no_repair() {
    let src = Store::new();
    let holder = tempfile::tempdir().expect("tempdir");
    let settings = hot_store(&src, &holder);

    let err = read_as_the_daemon(&settings)
        .map(|r| r.secrets.len())
        .expect_err("hot journal must refuse");

    // `DbErrorKind::StoreBusy` says NOTHING WAS WRITTEN and tells the operator to run the command
    // again. That is the arm a reader hits when a writer holds the store, and it is the only arm
    // `DbError::sql` pulls out of the engine. A hot journal is a different condition with a
    // different repair, and it currently borrows neither.
    assert!(
        !err.contains("NOTHING WAS WRITTEN"),
        "a hot journal reached the StoreBusy arm — if that is now deliberate, this test is the \
         place the change is argued; got: {err}"
    );
    // ⚠ **This assertion INVERTED when the message was fixed, and the inversion is the point.** It
    // used to demand the word `permission` be ABSENT, on the reasoning that a hot journal is not a
    // permissions fault and the message must not read as one. That mechanism was right about the
    // goal and wrong about the means: the message now says *your file permissions are not the
    // problem*, which serves the goal by NAMING the wrong conclusion and denying it — far better
    // than staying silent and letting an operator reach it unaided, since the engine's own words
    // (`attempt to write a readonly database`) point straight at permissions.
    //
    // So the test asks the question it always meant: is permissions offered as the DIAGNOSIS, or
    // ruled out? A message that merely avoided the word would pass the old assertion and still
    // leave the operator to guess.
    let lower = err.to_ascii_lowercase();
    assert!(
        lower.contains("permissions are not the problem"),
        "the message must RULE OUT permissions rather than leave an operator to infer them from \
         the engine's own wording; got: {err}"
    );
}

#[test]
fn only_a_writer_clears_it_and_it_clears_on_the_open_alone() {
    let src = Store::new();
    let holder = tempfile::tempdir().expect("tempdir");
    let settings = hot_store(&src, &holder);
    let db = settings.join("db").join("vike.db");

    assert!(read_as_the_daemon(&settings).is_err(), "hot journal must refuse the read-only open");

    // An operator process that CAN write — `vike-cli` on the box, outside the daemon's mount
    // namespace — replays and removes the journal by opening the database at all. No verb is
    // needed and no row changes.
    {
        let conn = Connection::open(&db).expect("open write connection");
        let _: String =
            conn.query_row("PRAGMA journal_mode = DELETE", [], |r| r.get(0)).expect("pragma");
    }

    assert!(
        !settings.join("db").join("vike.db-journal").exists(),
        "a read-write open must have replayed and removed the journal"
    );
    let resolved =
        read_as_the_daemon(&settings).expect("the store must read once the journal is gone");
    assert!(
        resolved.secrets.into_map().contains_key("BINANCE_DEMO_API_KEY"),
        "the rolled-back store must still hold its committed rows"
    );
}
