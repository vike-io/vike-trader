//! `poller`: [`SettlementLedger`], the venue settlement pollers' at-most-once ledger (polymarket
//! `resolve`, hyperliquid `outcome_settlement`), and [`load_ledger_keys`], its read half.
//!
//! The KEY's shape and the LOG's words stay each venue's: a bridge composes its own key string, and
//! hands [`SettlementLedger::open`] its own [`SettlementLedgerLog`], whose lines are written from the
//! bridge's module, so they keep its target.
//!
//! The stop-aware poller-thread scaffold those pollers and the depth pumps run on (the tick sleep,
//! the `Drop`-joining `StopHandle`, the named-thread spawn) lived here too; it is
//! `vike_utility::poller` now, the one copy the tradehub's Telegram poller shares (that daemon links no
//! `vike-bridge-core`).

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// **What a [`SettlementLedger`] says when its best-effort persistence fails, in the OWNING venue's words.**
/// Each is a `tracing` line the bridge writes from its own module (a non-capturing closure in a `const`), so the
/// line carries that bridge's target and wording, never this module's.
pub struct SettlementLedgerLog {
    /// The open found a torn last line (a crash mid-write) and could not end it.
    pub torn_line: fn(&std::io::Error),
    /// The key (the first argument) could not be written to the opened file; the in-memory guard holds.
    pub persist: fn(&str, &std::io::Error),
    /// The file could not be opened for append to write the key (the first argument).
    pub open_for_append: fn(&str, &std::io::Error),
}

/// **A persisted set of already-settled keys: the at-most-once store across process restarts.** One key per
/// line, thread-safe, best-effort persistence: a write failure is logged through the venue's
/// [`SettlementLedgerLog`] and never kills the poller, and the in-memory set still guards the running session.
pub struct SettlementLedger {
    path: PathBuf,
    seen: Mutex<HashSet<String>>,
    log: &'static SettlementLedgerLog,
}

impl SettlementLedger {
    /// Open, loading any existing keys ([`load_ledger_keys`]). A missing/unreadable file is an empty ledger.
    pub fn open(path: PathBuf, log: &'static SettlementLedgerLog) -> Self {
        let seen = load_ledger_keys(&path);
        // A crash can leave the last key with no newline, and the next key would join it and be lost.
        // An unreadable file is left as it is, like the load above.
        if vike_utility::atomic_file::ends_mid_line(&path).unwrap_or(false) {
            let ended =
                std::fs::OpenOptions::new().append(true).open(&path).and_then(|mut f| writeln!(f));
            if let Err(e) = ended {
                (log.torn_line)(&e);
            }
        }
        SettlementLedger { path, seen: Mutex::new(seen), log }
    }

    /// Whether `key` (the venue's own composition) is settled.
    pub fn contains(&self, key: &str) -> bool {
        self.seen.lock().unwrap().contains(key)
    }

    /// Record `key` as settled (in-memory + append to disk). Idempotent; a persist error is logged only — the
    /// in-memory guard still holds for the session.
    pub fn mark(&self, key: &str) {
        let newly = self.seen.lock().unwrap().insert(key.to_string());
        if !newly {
            return;
        }
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
            Ok(mut f) => {
                if let Err(e) = writeln!(f, "{key}") {
                    (self.log.persist)(key, &e);
                }
            }
            Err(e) => (self.log.open_for_append)(key, &e),
        }
    }
}

/// Load an at-most-once ledger file: one key per line, each trimmed, blank lines skipped. A missing
/// or unreadable file is an EMPTY ledger — the ordinary first-run state, never an error.
///
/// The read half of [`SettlementLedger`].
#[must_use]
pub fn load_ledger_keys(path: &Path) -> HashSet<String> {
    let mut seen = HashSet::new();
    if let Ok(txt) = std::fs::read_to_string(path) {
        for line in txt.lines() {
            let k = line.trim();
            if !k.is_empty() {
                seen.insert(k.to_string());
            }
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ledger file is one key per line, trimmed, blanks skipped, repeats collapsed; a file that
    /// does not exist yet is the ordinary first-run EMPTY ledger, not an error.
    #[test]
    fn load_ledger_keys_trims_skips_blanks_and_reads_a_missing_file_as_empty() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let path = dir.path().join("ledger.txt");
        assert!(load_ledger_keys(&path).is_empty(), "a missing file is an empty ledger");
        std::fs::write(&path, "a:1:2\n  b:3:4  \n\n   \na:1:2\n").expect("write the ledger");
        let keys = load_ledger_keys(&path);
        assert_eq!(keys.len(), 2, "{keys:?}");
        assert!(keys.contains("a:1:2") && keys.contains("b:3:4"), "{keys:?}");
    }
}
