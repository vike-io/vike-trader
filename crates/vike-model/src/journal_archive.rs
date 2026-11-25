//! What the journal's ARCHIVE does once it reaches its limit: the `config.journal_archive_full` row's words
//! (`docs/decisions/0304-a-boot-restores-from-the-newest-snapshot-and-its-tail-and-old-segments-are-archived-never-deleted.md`,
//! verdict 2).
//!
//! Here because the settings loader (`vike-config`, layer 20) refuses a word outside the set and the journal
//! (`vike-journal`, layer 25) acts on it: the one vocabulary sits below both.
//!
//! ⚠ `keep`, the default, deletes nothing. `drop_oldest` DELETES the user's archived data, so it is an operator's
//! opt-in and never the default (the owner's ruling of 2026-11-24: "by default no").

use serde::{Deserialize, Serialize};

/// What the journal's archive pass does when the archive holds `config.journal_archive_max_gib` or a file older
/// than `config.journal_archive_max_days` (`vike_journal::Journal::prune_before_latest_snap`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalArchiveFull {
    /// `keep`: archive nothing more, so every newer segment stays in the live directory, and say so at ERROR;
    /// nothing in the archive is touched.
    #[default]
    Keep,
    /// `drop_oldest` (opt-in): delete the oldest FINISHED compressed archived segments (a `journal-*.vjl.gz` with no
    /// plain file beside it), oldest first, each said at INFO with its name and size, until the archive is under
    /// both limits, then archive as usual. Never a live segment (none is under `archive/`), an archived one not
    /// compressed yet, or a `.part` being written or verified.
    DropOldest,
}

vike_utility::word_enum! {
    impl JournalArchiveFull {
        Keep => "keep",
        DropOldest => "drop_oldest",
    }
}
