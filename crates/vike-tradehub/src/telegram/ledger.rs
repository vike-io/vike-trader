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
//! resolves to (`crates/vike-model/src/paths/state_path.rs`'s `project_state_dir`), resolved by the
//! binary (it owns the environment, per `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s
//! rule) and passed to [`UpdateLedger::open`]. It used to sit beside the EXECUTABLE, which is
//! read-only under `deploy/vike-tradehub.service`'s `ProtectSystem` — so in production every append
//! failed and every failure was swallowed. That old file is read by nothing (decision 0117).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::LEDGER_COMPACT_AT;

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
    /// Open the ledger at `path`: read the mark, and PROVE the file is appendable — the
    /// writability check that makes [`crate::telegram::maybe_spawn`] able to refuse to arm rather
    /// than degrade silently.
    ///
    /// `Err` when the file EXISTS but cannot be read, or when it cannot be created or appended to.
    /// An existing ledger we cannot read has an UNKNOWN mark, and starting from an unknown mark IS
    /// the replay this type exists to prevent — so it is never treated as a fresh ledger. A file
    /// that is simply absent IS a fresh ledger, and is `Ok`.
    pub fn open(path: &Path) -> Result<Self, String> {
        let (last, lines) = scan(path)?.unwrap_or((0, 0));

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                format!("{}: the ledger directory could not be created: {e}", parent.display())
            })?;
        }
        // Opening for append PROVES writability; nothing is written here.
        drop(open_for_append(path)?);

        let ledger = UpdateLedger { path: path.to_path_buf(), last: Mutex::new(last) };
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
            ));
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

    #[test]
    fn ledger_is_monotonic_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram_updates.ledger");
        let ledger = UpdateLedger::open(&path).expect("a writable directory");
        assert_eq!(ledger.last(), 0);
        assert_eq!(ledger.offset(), 1, "a fresh ledger asks Telegram for everything pending");
        assert!(!ledger.is_processed(1));

        ledger.mark(7).expect("the append lands");
        assert!(ledger.is_processed(7) && ledger.is_processed(3) && !ledger.is_processed(8));
        assert_eq!(ledger.offset(), 8);
        ledger.mark(3).expect("idempotent"); // out of order / replayed — never lowers the mark
        assert_eq!(ledger.last(), 7);

        // A RESTART must not replay: the mark is read back off disk.
        let reopened = UpdateLedger::open(&path).expect("still writable");
        assert_eq!(reopened.last(), 7);
        assert!(reopened.is_processed(7));
    }

    /// ⚠ The gate this file exists for: a ledger path that cannot be used is an `Err`, never a
    /// ledger that silently records nothing. Portable half — a plain FILE where the ledger's parent
    /// directory should be (`crates/vike-model/src/paths/state_path/tests/state_and_log.rs`'s
    /// `an_uncreatable_state_dir_errors_instead_of_panicking` uses the same stand-in).
    #[test]
    fn an_unusable_ledger_path_is_an_error_not_a_silent_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "not a directory").unwrap();
        let path = blocker.join("state").join("telegram_updates.ledger");

        let err = UpdateLedger::open(&path).expect_err("an unusable ledger must not open");
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
        let opened = UpdateLedger::open(&state.join("telegram_updates.ledger"));
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

        let err = UpdateLedger::open(&path).expect_err("an unreadable ledger must not open");
        assert!(err.contains("could not be read"), "{err}");
    }

    /// A mark that cannot be persisted mid-session reports `Err` — the runtime half of the same
    /// rule, for a directory that became unwritable after the channel armed.
    #[test]
    fn a_mark_that_cannot_be_persisted_reports_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram_updates.ledger");
        let ledger = UpdateLedger::open(&path).expect("writable at open");
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
