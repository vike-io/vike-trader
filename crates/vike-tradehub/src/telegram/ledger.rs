//! ⚠ **SECURITY-CRITICAL** — at-most-once across a process restart: the persisted `update_id`
//! high-water mark, which is simultaneously the Telegram ACK.
//!
//! A bug here means a daemon restart REPLAYS an update on a REMOTE ORDER-ORIGINATION path. See the
//! module doc of [`crate::telegram`] for why the mark is written BEFORE the update is acted on.
//!
//! # Nothing here is best-effort, and that is the point
//!
//! It used to be. Every write was `let _ = …` or a `tracing::warn!`, on the reasoning that the
//! in-memory mark still guards the running session — which is true, and irrelevant: the running
//! session is the one case this file is NOT for. The persisted mark is the whole product, so a
//! write that cannot land is an ERROR, reported to the caller, twice over:
//!
//! * [`UpdateLedger::open`] PROVES the file is readable and appendable before the channel arms.
//!   `Err` ⇒ [`crate::telegram::maybe_spawn`] refuses to start the channel at all.
//! * [`UpdateLedger::mark`] returns `Err` when the append did not reach disk, and
//!   [`crate::telegram::poll_once`] then does not act on that update.
//!
//! The exception, stated so it is not mistaken for an oversight: **compaction stays best-effort.**
//! A failed compaction leaves a longer file holding the same mark, which is a tidiness problem, not
//! a correctness one.
//!
//! # Where the file lives
//!
//! `<project>/settings/state/telegram_updates.ledger` — the one root every program-written file
//! resolves to (`crates/vike-model/src/state_path.rs`'s `project_state_dir`), passed in by the
//! binary as [`LedgerPaths`]. It used to sit beside the EXECUTABLE, which is read-only under
//! `deploy/vike-tradehub.service`'s `ProtectSystem` — so in production every append failed and
//! every failure was swallowed.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::LEDGER_COMPACT_AT;

/// Where the ledger lives, plus where an un-migrated install's copy may still be.
///
/// Two paths rather than one because the file MOVED, and a ledger read from the wrong path is an
/// at-most-once guarantee silently reset. The CALLER resolves both (the binary owns the
/// environment, per `crates/vike-ops/tests/settings_registry.rs`'s rule); a struct rather than two
/// parameters so the destination and the fallback cannot be swapped at a call site.
#[derive(Debug, Clone)]
pub struct LedgerPaths {
    /// The file every mark is APPENDED to: `<project>/settings/state/telegram_updates.ledger`.
    pub path: PathBuf,
    /// The pre-move location, `<exe_dir>/telegram_updates.log` — READ once at open and never
    /// written. `None` when the caller cannot resolve one (nothing to migrate).
    pub legacy: Option<PathBuf>,
}

/// Persisted high-water mark of processed `update_id`s — the at-most-once store across process
/// restarts, and simultaneously the Telegram ACK (`offset = last + 1`).
///
/// Append-only lines, max-on-load, thread-safe. Append-only rather than truncate-rewrite because a
/// torn rewrite would read back as `0` — i.e. REPLAY EVERYTHING, the one direction a control
/// channel must not fail in. [`LEDGER_COMPACT_AT`] keeps that bounded.
///
/// `Debug` is derived here and deliberately absent on `ProdTelegramDeps`: this type holds a path
/// and an integer, neither secret (the path is already logged at arm time), while that one embeds
/// the bot token in a URL.
#[derive(Debug)]
pub struct UpdateLedger {
    path: PathBuf,
    last: Mutex<i64>,
}

impl UpdateLedger {
    /// Open the ledger: read the mark, migrate an un-migrated install, and PROVE the file is
    /// appendable — the writability check that makes [`crate::telegram::maybe_spawn`] able to
    /// refuse to arm rather than degrade silently.
    ///
    /// The mark is the **MAX over both paths**, which is deliberately not
    /// `crates/vike-model/src/state_path.rs`'s `read_path` either/or rule. For this data type the
    /// max is strictly the safer merge: it can never be LOWER than either file's own mark, so
    /// neither an empty new file (which the writability proof itself creates) nor an old binary
    /// having appended to the legacy copy after a new one migrated can lower it. Taking one file
    /// and ignoring the other can do both.
    ///
    /// A migration is then durable immediately rather than on the next `mark`: when the legacy copy
    /// holds the higher mark it is APPENDED to the new file here, so a later run never depends on
    /// the legacy file still being readable. The legacy file is never written and never deleted —
    /// a delete that races a rollback loses the mark.
    ///
    /// `Err` when either file EXISTS but cannot be read, or when the destination cannot be created
    /// or appended to. An existing ledger we cannot read has an UNKNOWN mark, and starting from an
    /// unknown mark IS the replay this type exists to prevent — so it is never treated as a fresh
    /// ledger. A file that is simply absent IS a fresh ledger, and is `Ok`.
    pub fn open(paths: &LedgerPaths) -> Result<Self, String> {
        let (mut last, lines) = scan(&paths.path)?.unwrap_or((0, 0));
        let mine = last;
        if let Some(legacy) = paths.legacy.as_deref() {
            if let Some((legacy_last, _)) = scan(legacy)? {
                last = last.max(legacy_last);
            }
        }

        if let Some(parent) = paths.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                format!("{}: the ledger directory could not be created: {e}", parent.display())
            })?;
        }
        // Opening for append PROVES writability; the migration line is the only thing written, and
        // only when the legacy copy actually carried a higher mark.
        let mut f = open_for_append(&paths.path)?;
        if last > mine {
            writeln!(f, "{last}").map_err(|e| {
                format!("{}: the legacy mark could not be migrated: {e}", paths.path.display())
            })?;
            tracing::info!(
                mark = last,
                from = ?paths.legacy,
                to = ?paths.path,
                "telegram update ledger: migrated the at-most-once mark to the project state \
                 directory (the legacy file is left in place, never deleted)"
            );
        }
        drop(f);

        let ledger = UpdateLedger { path: paths.path.clone(), last: Mutex::new(last) };
        if lines > LEDGER_COMPACT_AT {
            // The ONE best-effort write in this file, and the only one whose failure costs nothing:
            // a longer file holding the same mark is untidy, not wrong.
            if let Err(e) = std::fs::write(&ledger.path, format!("{last}\n")) {
                tracing::warn!(%e, "telegram update ledger: compaction failed (mark still held)");
            }
        }
        Ok(ledger)
    }

    /// The file every mark is appended to — for the log line that discloses where it resolved.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The highest `update_id` ever processed (0 on a fresh ledger).
    pub fn last(&self) -> i64 {
        *self.last.lock().expect("telegram update ledger poisoned")
    }

    /// The `offset` to pass to the next `getUpdates` — which is ALSO how Telegram is ACKed: it
    /// drops every update below the offset from its own queue.
    pub fn offset(&self) -> i64 {
        self.last().saturating_add(1)
    }

    /// Has this update already been consumed by an earlier pass (or an earlier process)?
    pub fn is_processed(&self, update_id: i64) -> bool {
        update_id <= self.last()
    }

    /// Record this update as consumed, DURABLY. Monotonic (an out-of-order id never lowers the
    /// mark) and idempotent (an already-marked id is `Ok` and writes nothing).
    ///
    /// ⚠ **The in-memory mark advances even when the append fails, and the `Err` is still an
    /// `Err`.** The two are not in tension: `offset()` is the Telegram ACK, and a mark that refused
    /// to advance would re-fetch the identical backlog at the poll rate forever, hammering the API
    /// and re-logging every update. So the update is consumed either way — what the `Err` says is
    /// that the record did not reach DISK, which is what makes ACTING on it unsafe.
    /// `crate::telegram::poll_once` therefore drops the update instead of dispatching it: losing a
    /// command is the documented safe direction, placing one that cannot be recorded is not.
    ///
    /// `sync_all` because the guarantee claimed is at-most-once across a RESTART, and a trading box
    /// restarts for reasons that include losing power. It costs one fsync of a ~10-byte append, on
    /// a poller thread that sees human-typed chat messages — nowhere near the core fold the p99
    /// gate protects.
    ///
    /// The directory is deliberately NOT re-created here: [`Self::open`] proved it, and silently
    /// re-creating a directory that vanished under a running daemon is the best-effort reflex this
    /// file no longer has.
    pub fn mark(&self, update_id: i64) -> Result<(), String> {
        let mut guard = self.last.lock().expect("telegram update ledger poisoned");
        if update_id <= *guard {
            return Ok(());
        }
        *guard = update_id;
        let mut f = open_for_append(&self.path)?;
        writeln!(f, "{update_id}")
            .and_then(|()| f.sync_all())
            .map_err(|e| format!("{}: the mark could not be persisted: {e}", self.path.display()))
    }
}

/// Open `path` for append, creating it if absent. The one place the ledger is opened for writing,
/// so the writability proof in [`UpdateLedger::open`] and the append in [`UpdateLedger::mark`]
/// cannot disagree about what "writable" means.
fn open_for_append(path: &Path) -> Result<std::fs::File, String> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("{}: the ledger is not appendable: {e}", path.display()))
}

/// The max `update_id` in `path` and how many non-empty lines it has.
///
/// `Ok(None)` **only** when the file does not exist — a fresh ledger, the normal first-run state.
/// `Err` for every OTHER read failure: a ledger that cannot be read has an unknown mark, and the
/// one thing that must never happen is treating "unknown" as "0", which is "replay everything
/// Telegram still holds". An unparseable LINE is still ignored (never a reset) — a torn tail must
/// not lower a mark the rest of the file establishes.
fn scan(path: &Path) -> Result<Option<(i64, usize)>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(format!(
                "{}: the ledger could not be read, and only a MISSING file counts as a fresh \
                 ledger: {e}",
                path.display()
            ))
        }
    };
    let mut last = 0i64;
    let mut lines = 0usize;
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        lines += 1;
        if let Ok(v) = t.parse::<i64>() {
            last = last.max(v);
        }
    }
    Ok(Some((last, lines)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `LedgerPaths` with no legacy half — the steady state after migration.
    fn at(path: PathBuf) -> LedgerPaths {
        LedgerPaths { path, legacy: None }
    }

    #[test]
    fn ledger_is_monotonic_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram_updates.ledger");
        let ledger = UpdateLedger::open(&at(path.clone())).expect("a writable directory");
        assert_eq!(ledger.last(), 0);
        assert_eq!(ledger.offset(), 1, "a fresh ledger asks Telegram for everything pending");
        assert!(!ledger.is_processed(1));

        ledger.mark(7).expect("the append lands");
        assert!(ledger.is_processed(7) && ledger.is_processed(3) && !ledger.is_processed(8));
        assert_eq!(ledger.offset(), 8);
        ledger.mark(3).expect("idempotent"); // out of order / replayed — never lowers the mark
        assert_eq!(ledger.last(), 7);

        // A RESTART must not replay: the mark is read back off disk.
        let reopened = UpdateLedger::open(&at(path)).expect("still writable");
        assert_eq!(reopened.last(), 7);
        assert!(reopened.is_processed(7));
    }

    /// The MOVE. An install whose mark is still at the legacy path keeps it, the new file carries
    /// it forward on its own from then on, and the legacy file is left untouched.
    #[test]
    fn the_legacy_mark_migrates_and_is_never_replayed() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("exe_dir").join("telegram_updates.log");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "11\n42\n7\n").unwrap();
        let path = dir.path().join("state").join("telegram_updates.ledger");

        let paths = LedgerPaths { path: path.clone(), legacy: Some(legacy.clone()) };
        let ledger = UpdateLedger::open(&paths).expect("a writable state directory");
        assert_eq!(ledger.last(), 42, "the legacy mark carried over — 42 is not replayed");
        assert!(ledger.is_processed(42) && !ledger.is_processed(43));

        // …and it is DURABLE at the new path on its own: reopening with no legacy half still finds
        // it, so the migration does not depend on the old file surviving.
        let reopened = UpdateLedger::open(&at(path)).expect("still writable");
        assert_eq!(reopened.last(), 42);
        assert_eq!(
            std::fs::read_to_string(&legacy).unwrap(),
            "11\n42\n7\n",
            "the legacy file is READ, never written and never deleted"
        );
    }

    /// The max-over-both rule, in the direction `read_path`'s either/or would get wrong: an old
    /// binary appended to the legacy copy AFTER a new one had already migrated.
    #[test]
    fn the_mark_is_the_max_over_both_paths_never_just_one() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("telegram_updates.log");
        let path = dir.path().join("telegram_updates.ledger");
        std::fs::write(&path, "100\n").unwrap();
        std::fs::write(&legacy, "200\n").unwrap();

        let ledger = UpdateLedger::open(&LedgerPaths { path, legacy: Some(legacy) })
            .expect("both readable, destination writable");
        assert_eq!(ledger.last(), 200, "taking the NEW file alone would replay 101..=200");
    }

    /// ⚠ The gate this file exists for: a ledger path that cannot be used is an `Err`, never a
    /// ledger that silently records nothing. Portable half — a plain FILE where the ledger's parent
    /// directory should be (`crates/vike-model/src/state_path.rs`'s
    /// `an_uncreatable_state_dir_errors_instead_of_panicking` uses the same stand-in).
    #[test]
    fn an_unusable_ledger_path_is_an_error_not_a_silent_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "not a directory").unwrap();
        let path = blocker.join("state").join("telegram_updates.ledger");

        let err =
            UpdateLedger::open(&at(path.clone())).expect_err("an unusable ledger must not open");
        assert!(err.contains(&path.display().to_string()), "the error names the path: {err}");
    }

    /// ⚠ **The PRODUCTION shape, exactly**: the ledger's directory is READ-ONLY, which is what
    /// `deploy/vike-tradehub.service`'s `ProtectSystem` does to everything `ReadWritePaths` does not
    /// name. The file is absent, so the read arm is happy and `create_dir_all` is a no-op on the
    /// existing directory — only the append PROOF catches it, which is the arm this test exists to
    /// pin. Unix only: on Windows a directory's read-only attribute does not stop file creation, so
    /// there is nothing to assert there.
    #[cfg(unix)]
    #[test]
    fn a_read_only_directory_is_caught_by_the_append_proof() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o555)).unwrap();

        // Running as root defeats the mode bits entirely; say so rather than assert something
        // false. (CI runs as an ordinary user, so this is a local-run guard, not an escape hatch.)
        let root = std::fs::write(state.join(".probe"), b"").is_ok();
        let opened = UpdateLedger::open(&at(state.join("telegram_updates.ledger")));
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
        if root {
            eprintln!("skipped: running as root, a 0555 directory is still writable");
            return;
        }

        let err = opened.expect_err("a read-only ledger directory must refuse to open");
        assert!(err.contains("not appendable"), "the APPEND proof is what caught it: {err}");
    }

    /// An existing ledger that cannot be READ is an `Err` too — "unknown mark" must never be
    /// rounded down to `0`, which would replay everything Telegram still holds.
    #[test]
    fn an_unreadable_ledger_is_an_error_not_a_fresh_mark() {
        let dir = tempfile::tempdir().unwrap();
        // A DIRECTORY where the ledger file should be: `read_to_string` fails with something that
        // is NOT NotFound, on every platform.
        let path = dir.path().join("telegram_updates.ledger");
        std::fs::create_dir_all(&path).unwrap();

        let err = UpdateLedger::open(&at(path)).expect_err("an unreadable ledger must not open");
        assert!(err.contains("could not be read"), "{err}");

        // The same rule applies to the LEGACY half: a mark we cannot read is not a mark of 0.
        let legacy_dir = tempfile::tempdir().unwrap();
        let legacy = legacy_dir.path().join("telegram_updates.log");
        std::fs::create_dir_all(&legacy).unwrap();
        let err = UpdateLedger::open(&LedgerPaths {
            path: legacy_dir.path().join("telegram_updates.ledger"),
            legacy: Some(legacy),
        })
        .expect_err("an unreadable legacy half must not be silently skipped");
        assert!(err.contains("could not be read"), "{err}");
    }

    /// A mark that cannot be persisted mid-session reports `Err` — the runtime half of the same
    /// rule, for a directory that became unwritable after the channel armed.
    #[test]
    fn a_mark_that_cannot_be_persisted_reports_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram_updates.ledger");
        let ledger = UpdateLedger::open(&at(path.clone())).expect("writable at open");
        ledger.mark(1).expect("the first append lands");

        // Pull the file out from under the running ledger and put a DIRECTORY in its place, so the
        // append can no longer open it.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir_all(&path).unwrap();

        let err = ledger.mark(2).expect_err("an append that cannot land must report it");
        assert!(err.contains("not appendable"), "{err}");
        assert!(
            ledger.is_processed(2),
            "the in-memory mark still advanced: the update is consumed and ACKed to Telegram, and \
             the Err is about DISK — see this method's doc"
        );
    }
}
