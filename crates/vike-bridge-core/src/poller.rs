//! `poller`: [`load_ledger_keys`], the read half of the venue settlement pollers' at-most-once
//! ledger (polymarket `resolve`, hyperliquid `outcome_settlement`).
//!
//! The stop-aware poller-thread scaffold those pollers and the depth pumps run on (the tick sleep,
//! the `Drop`-joining `StopHandle`, the named-thread spawn) lived here too; it is
//! `vike_log::poller` now, the one copy the tradehub's Telegram poller shares (that daemon links no
//! `vike-bridge-core`).

use std::collections::HashSet;
use std::path::Path;

/// Load an at-most-once ledger file: one key per line, each trimmed, blank lines skipped. A missing
/// or unreadable file is an EMPTY ledger — the ordinary first-run state, never an error.
///
/// The read half of the persisted set both settlement pollers keep (`SettlementLedger::open` in
/// polymarket's `exec_plane::settlement::resolve` and hyperliquid's `outcome_settlement`). The write
/// half (key shape, log wording, in-memory guard) stays with each venue.
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
