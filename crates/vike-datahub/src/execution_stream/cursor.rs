//! Where one journal's stream stands: the cursor row of `vike.db` (`vike_secrets::execution_streams`), opened once
//! at the open (created on the journal's first), and what a session learned since. The row is the AUTHORITY: the
//! tradehub keeps no resume point of its own, and this module is the datahub's one reader of it at an open.

use std::path::Path;

use vike_secrets::execution_streams::{ExecutionStreamIdentity, open_execution_stream_row};

/// A window the datahub began and did not finish (it died between `writing_through` and `stored_through`): the
/// next window is exactly `(after, through]`, under the same commit keys, so every series that already landed is a
/// store no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PinnedWindow {
    /// `stored_through` when the window began (`None`: nothing was stored, the window starts the journal).
    pub(super) after: Option<u64>,
    /// Its last record.
    pub(super) through: u64,
}

/// One journal's position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StreamCursor {
    /// Every record up to it is stored; `None` while nothing is, the one state in which a record chained from `None`
    /// (the journal's start) is taken.
    pub(super) stored_through: Option<u64>,
    /// The `recorded_ms` of the record at `stored_through`, when one was stored there.
    pub(super) recorded_ms: Option<i64>,
    /// The window a crash left half written, until it is rewritten.
    pub(super) pinned: Option<PinnedWindow>,
    /// The journal's head at its FIRST open, kept in the row: a record up to it whose stream row is already in its
    /// series (a journal re-identified after a copy) is dropped at the write, however many reconnects the catch-up
    /// takes.
    pub(super) first_contact_through: u64,
    /// The journal's generation: which of the datahub's journals this one's sequences count in.
    pub(super) generation: i64,
}

impl StreamCursor {
    /// The cursor of `stream`'s journal, as `vike.db` holds it, its row created on the journal's first open with
    /// `head_sequence` as the first-contact boundary. A store that will not read or write is an error, never first
    /// contact: answering `None` would make the tradehub resend its whole journal.
    pub(super) fn open(
        settings_dir: &Path,
        stream: &ExecutionStreamIdentity,
        head_sequence: u64,
    ) -> Result<Self, String> {
        let row =
            open_execution_stream_row(settings_dir, stream, head_sequence, vike_model::now_ms())
                .map_err(|e| e.to_string())?;
        Ok(StreamCursor {
            stored_through: row.stored_through,
            recorded_ms: row.stored_through_recorded_ms,
            // `None` orders below every sequence: a window begun from nothing is pinned too
            pinned: row
                .writing_through
                .filter(|&w| Some(w) > row.stored_through)
                .map(|through| PinnedWindow { after: row.stored_through, through }),
            first_contact_through: row.first_contact_through,
            generation: row.generation,
        })
    }

    /// The `after` of the next window's keys: the pinned window's, else the cursor.
    pub(super) fn window_after(&self) -> Option<u64> {
        self.pinned.map_or(self.stored_through, |p| p.after)
    }
}
