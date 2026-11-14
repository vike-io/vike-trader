//! **What the read-only opener does when a rollback journal survives a killed writer.**
//!
//! `docs/decisions/0054-settings-move-into-one-database.md` moves the settings into this store, and
//! decision record 0057 — the accepted extension that answers the seven settings files one at a
//! time, not yet on `main` and therefore deliberately not cited here by path — names this as one of
//! two measurements it *"leans on nobody"* and requires before its Phase 1 is trusted. It states
//! the answer as DERIVED rather than measured. This file is the measurement, and it is here rather
//! than in a report because the property it pins is one a dependency bump can silently change: the
//! whole read half of that migration rests on `crates/vike-secrets/src/db/open.rs`'s `open_for_read`
//! being able to answer with the daemon's `settings/` mounted read-only, and a hot journal is the
//! one on-disk state in which it cannot.
//!
//! # The state under test, and why it is built by COPY rather than by a kill
//!
//! `journal_mode = DELETE` writes `vike.db-journal` beside the database for the duration of a write
//! and removes it on a clean close — `crates/vike-secrets/tests/store/database/roundtrip.rs`'s
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
//! A read that meets a hot journal replays it itself, through `crates/vike-secrets/src/db/open.rs`'s
//! `replay_hot_journal`, and answers with every committed key: a killed writer must not read as a box with no keys. Only a
//! process that truly may not write is still refused, by `crates/vike-secrets/src/db/error.rs`'s
//! `StoreErrorKind::ReadOnlyRollback`, and it changes no byte.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::support::Fixture;

/// SQLite's rollback-journal header magic. A journal carrying it is one the engine must replay; a
/// journal whose first eight bytes are zero is one the engine has deliberately marked not-yet-valid.
const JOURNAL_MAGIC: [u8; 8] = [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];

/// A settings directory holding a real store — built through the production path
/// (`vike_secrets::create_store`, then the name-addressed credential writer) so the schema, the
/// `PRAGMA user_version` stamp and the modes are the ones a live box carries.
fn seeded_store() -> Fixture {
    Fixture::seeded_with(
        [
            ("BINANCE_DEMO_API_KEY", "k"),
            ("BINANCE_DEMO_API_SECRET", "s"),
            ("BYBIT_DEMO_API_KEY", "k2"),
        ],
        |_| false,
    )
}

/// The rollback journal a writer on `src`'s database leaves beside it while a transaction is open.
fn journal(src: &Fixture) -> PathBuf {
    src.dir().join("db").join("vike.db-journal")
}

/// Leave `dst` holding a database and a HOT rollback journal.
///
/// `PRAGMA cache_size = 1` is what makes this deterministic: it forces the transaction's dirty pages
/// out of the cache and into the database FILE before the commit, which is the condition that makes
/// SQLite stamp the journal's real header magic. The pair is copied out while that transaction is
/// still open, so `dst` is exactly what a box finds after a writer died at that instant — and no
/// process ever held a lock on `dst` itself, so the journal there is nobody's live write.
fn hot_pair(src: &Fixture, dst: &Path) {
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
                // The venue by its NUMBER: `credential` has no text `venue` since the venue-links
                // plan's second release.
                .prepare(
                    "INSERT INTO credential (name, field, value, venue_id) \
                     VALUES (?1, 'api_key', ?2, (SELECT id FROM venue WHERE name = 'binance'))",
                )
                .expect("prepare");
            // Insert until the journal is HOT, at most 20,000 rows. The first cache spill syncs the
            // journal (that is when SQLite stamps the header magic) and then writes the spilled
            // page into the database file, both inside one `execute` — so a stamped header seen
            // after an insert already means a modified database file. Every row past that point
            // only repeats spills against a 1-page cache: MEASURED on a the latency box lane (2026-10-03) at
            // ~270k pread/pwrite syscalls per test for the full 20,000. The state handed on is the
            // same KIND the fixed count produced (a stamped journal beside a modified database
            // file, only shorter), and the magic assertion below still checks it.
            for i in 0..20_000 {
                st.execute(rusqlite::params![format!("INFLIGHT_{i}"), format!("v{i}")])
                    .expect("insert");
                if i % 64 == 63 && journal_is_stamped(&journal(src)) {
                    break;
                }
            }
        }
        assert!(journal(src).is_file(), "the in-flight write left no journal to copy");
        std::fs::copy(src.db(), dst.join("vike.db")).expect("copy database");
        std::fs::copy(journal(src), dst.join("vike.db-journal")).expect("copy journal");
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
/// `vike_secrets::credentials::try_load_workspace_secrets_at` calls, and it reaches
/// `crates/vike-secrets/src/db/open.rs`'s `open_for_read` through
/// `crates/vike-secrets/src/store/backend.rs`'s `StoreBackend`.
/// Does the journal at `path` carry SQLite's real header magic yet? Reads eight bytes, never the
/// whole journal; a missing or shorter file is "not yet".
fn journal_is_stamped(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 8];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)).is_ok()
        && head == JOURNAL_MAGIC
}

fn read_as_the_daemon(settings: &Path) -> Result<vike_secrets::CredentialRead, String> {
    crate::support::store_from(Some(settings.to_str().expect("utf-8 temp path")))
        .read_all()
        .map_err(|e| e.to_string())
}

/// Copy a hot pair into a settings tree of its own, so the read goes through the ordinary
/// project-settings door rather than at a bare path.
fn hot_store(src: &Fixture, into: &tempfile::TempDir) -> PathBuf {
    let settings = into.path().join("settings");
    hot_pair(src, &settings.join("db"));
    settings
}

/// ⚠ **The whole of the rule: a killed writer costs no read, no key and no row.** The read that meets the journal replays it
/// through one brief read-write open and answers, as if nothing had happened. This is what stops a killed gatewayhub from
/// reading, to every later reader, as a box with no keys.
#[test]
fn a_read_replays_a_killed_writers_journal_by_itself_and_every_key_stands() {
    let src = seeded_store();
    let baseline =
        read_as_the_daemon(src.dir()).expect("the baseline store reads").secrets.into_map();

    let holder = tempfile::tempdir().expect("tempdir");
    let settings = hot_store(&src, &holder);
    let journal = settings.join("db").join("vike.db-journal");
    assert!(journal.is_file(), "the state under test");

    let after =
        read_as_the_daemon(&settings).expect("the read replays it and answers").secrets.into_map();
    assert!(after == baseline, "every committed key, and none of the killed writer's");
    assert!(!journal.exists(), "the replay removes the journal");
    let stamp: i64 = Connection::open(settings.join("db").join("vike.db"))
        .expect("open")
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .expect("stamp");
    assert_eq!(stamp, vike_secrets::SCHEMA_VERSION, "a replay converts nothing");
}

/// ⚠ **A process that truly may not write keeps today's refusal, by name, and touches nothing.** That is a unit with no
/// `settings/db` grant (the datahub, the backtesthub) until a process that may write has read the store. Unix only, and it
/// skips LOUDLY where mode bits do not take (a run as root), as `read_only_store.rs` does.
#[cfg(unix)]
#[test]
fn a_process_that_may_not_write_keeps_the_refusal_by_name_and_leaves_every_byte() {
    use std::os::unix::fs::PermissionsExt;

    /// Gives the temp tree back to its own removal, whatever the test did.
    struct Writable(PathBuf);
    impl Drop for Writable {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
    }

    let src = seeded_store();
    let holder = tempfile::tempdir().expect("tempdir");
    let settings = hot_store(&src, &holder);
    let (dir, db) = (settings.join("db"), settings.join("db").join("vike.db"));
    let journal = dir.join("vike.db-journal");
    let _writable = Writable(dir.clone());
    for (path, mode) in [(&db, 0o444), (&journal, 0o444), (&dir, 0o555)] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    }
    if std::fs::OpenOptions::new().append(true).open(&db).is_ok() {
        eprintln!(
            "SKIP: this process ignores mode bits (root), so the read-only shape cannot be built here"
        );
        return;
    }
    let before = (std::fs::read(&db).expect("db"), std::fs::read(&journal).expect("journal"));

    let err =
        read_as_the_daemon(&settings).map(|r| r.secrets.len()).expect_err("it may not replay");

    assert!(err.contains("killed mid-write"), "the refusal is the named one: {err}");
    assert!(err.contains(&format!("{}-journal", db.display())), "and it names the file: {err}");
    let after = (std::fs::read(&db).expect("db"), std::fs::read(&journal).expect("journal"));
    assert!(after == before, "a process that may not write changed a byte");
}

/// A pin against the refusal's opacity: nobody improves a message nobody wrote down.
#[test]
fn the_whole_operator_facing_string_is_pinned_because_its_opacity_is_the_finding() {
    let db = PathBuf::from("/x/settings/db/vike.db");
    let err = vike_secrets::StoreError {
        path: db.clone(),
        kind: vike_secrets::StoreErrorKind::ReadOnlyRollback,
    }
    .to_string();
    let db = db.display();
    assert_eq!(
        err,
        format!(
            "a writer of the settings database was killed mid-write and left SQLite's rollback journal at \
             {db}-journal. NOTHING IS CORRUPT and nothing is lost: that journal holds the pages needed to put the \
             database back to its last committed state, and the engine REPLAYS it — and deletes it — on the first \
             read-WRITE open. Every read of this store tries that by itself, and this one could not: the file stayed \
             busy, or this process may not write beside the database (a unit with no `settings/db` grant: the \
             datahub, the backtesthub). The gatewayhub and the tradehub replay it on their own next read. ⚠ The \
             engine's own words for this are `attempt to write a readonly database`; nothing tried to write a row. \
             Repair it from an OPERATOR SHELL, where the directory is writable: ANY `vike-cli secrets` command \
             replays the journal, so `vike-cli secrets list` is enough. Then restart the daemon. ⚠ NEVER restore a \
             backup for this: it would lose every commit since."
        )
    );
}

/// ⚠ **Another reader's `SHARED` delays a replay and does not refuse it**, measured here because the pager's own code reads
/// otherwise: it takes that lock with no busy handler, and the btree above it retries through the connection's. Held past the
/// busy timeout, the read is refused and names no cause it did not see; released in time, the read replays and answers.
#[test]
fn another_readers_shared_lock_delays_the_replay_and_is_not_called_a_permission() {
    let src = seeded_store();
    let holder = tempfile::tempdir().expect("tempdir");
    let settings = hot_store(&src, &holder);
    let db = settings.join("db").join("vike.db");
    let journal = settings.join("db").join("vike.db-journal");

    // A reader that was in its read before the journal appeared: the only way to hold SHARED beside a hot journal.
    let bytes = std::fs::read(&journal).expect("the journal");
    std::fs::remove_file(&journal).expect("aside");
    std::fs::copy(src.db(), &db).expect("the committed database");
    let reader = Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("a reader");
    reader.execute_batch("BEGIN; SELECT count(*) FROM sqlite_master;").expect("holds SHARED");
    std::fs::write(&journal, bytes).expect("the journal back");

    let started = std::time::Instant::now();
    let err = read_as_the_daemon(&settings).map(|r| r.secrets.len()).expect_err("still held");
    assert!(err.contains("the file stayed busy, or this process may not write"), "{err}");
    let waited = started.elapsed();
    assert!(
        waited >= std::time::Duration::from_secs(4),
        "the busy timeout was honoured: {waited:?}"
    );
    assert!(waited < std::time::Duration::from_secs(30), "and it is one wait: {waited:?}");
    assert!(journal.exists(), "nothing was replayed");

    let released = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        reader.execute_batch("COMMIT").expect("releases SHARED");
    });
    read_as_the_daemon(&settings).expect("the replay outlasts another reader's moment");
    released.join().expect("no panic");
    assert!(!journal.exists(), "the replay removes the journal");
}

/// A writer's own open replays it too, with no verb and no flag: what an older binary's operator has.
#[test]
fn a_read_write_open_clears_it_with_no_verb_and_no_row_changes() {
    let src = seeded_store();
    let holder = tempfile::tempdir().expect("tempdir");
    let settings = hot_store(&src, &holder);
    let db = settings.join("db").join("vike.db");
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
