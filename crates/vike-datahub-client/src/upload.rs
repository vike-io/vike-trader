//! **The CSV UPLOAD verb's vocabulary**: a user's file sent from his PC into a `user-*` lane of the
//! datahub's store (decision 0272), so a user whose datahub is on a remote box copies nothing there by
//! hand. The request, the answer, and the rules both ends need for [`Request::CsvUpload`],
//! [`Request::CsvUploadChunk`] and [`Request::CsvUploadEnd`].
//!
//! [`Request::CsvUpload`]: crate::proto::Request::CsvUpload
//! [`Request::CsvUploadChunk`]: crate::proto::Request::CsvUploadChunk
//! [`Request::CsvUploadEnd`]: crate::proto::Request::CsvUploadEnd
//!
//! # The exchange, on ONE connection
//!
//! 1. `CsvUpload(`[`CsvUploadSpec`]`)`: the series (an [`ImportTarget`], `csv-bars`' own), the file's
//!    size in bytes, whether it is a dry run, and whether it is only the file's HEAD (a preview). The
//!    server checks everything it can before a byte of the file travels (the arming, the target, the
//!    size cap, the series already holding rows, the server's count of user series, the one import
//!    slot) and answers `CsvUploadReady` or refuses.
//! 2. `CsvUploadChunk { text }`, as many as the file needs, each at most [`UPLOAD_CHUNK_BYTES`] of its
//!    text, in order; each is answered `CsvUploadReceived { bytes }`, the running total.
//! 3. `CsvUploadEnd` once every declared byte is sent: the server decodes the file ONCE with the one CSV
//!    decoder (`vike_data::csv_bars`), and, unless it is a dry run, stores each UTC day it holds under the
//!    lane's day key. Answered `CsvUploaded(`[`CsvUploadDone`]`)`.
//!
//! The file's bytes are held in the datahub's MEMORY for that one request and are never written to its
//! disk: what persists is the store's own rows. So any other frame inside the session, a dropped
//! connection, an idle gap past the server's read timeout, or an `End` before every byte arrived ends the
//! session with NOTHING stored and no commit key spent, and sending the file again is clean.
//!
//! # Scope: Read, the observe key the PC already holds
//!
//! The verb writes the store, so decision 0058's rule binds it: a write may be Observe only when it is
//! additive, idempotent, armed by the operator, and bounded on every dimension the request names, all
//! four together. It is ADDITIVE: it only creates a series that holds no row, and no request can alter
//! or remove one (a series that holds rows is refused, whoever wrote them). It is IDEMPOTENT: a repeat is
//! refused and writes nothing. It is ARMED: by the `flags.datahub_csv_upload` row, off by default, and an
//! unarmed server does not advertise [`crate::proto::FEATURE_CSV_UPLOAD`] and refuses the verb having
//! read nothing. It is BOUNDED: the venue, the interval and the format words are enumerable, the bytes are
//! capped per file and one upload or import runs at a time, and the two free names (the symbol and the
//! label) are bounded by a COUNT, the server's cap on user series (decision 0058's own residual for the
//! chart seed's symbol). `crates/vike-datahub-client/src/proto/verbs.rs`'s `required_scope` carries the
//! argument at the arm.
//!
//! # What the request cannot name
//!
//! - **A source.** The series is `kind=bar/venue=<v>/source=user-<label>/…`, composed by the server from
//!   the label through `vike_data::store::store_kind::user_lane`, so no request reaches a venue's own
//!   series, a `Backfill`, an `ImportArchive` or a `DeleteSeries`: those stay `VerbScope::Write`.
//! - **A path.** The bytes ARE the file.

use serde::{Deserialize, Serialize};

use crate::archive::{
    ImportOutcomeDay, ImportPlanPreviewRow, ImportTarget, validate_import_target,
};

/// The most bytes of the file's text ONE [`crate::proto::Request::CsvUploadChunk`] carries. Eight MiB:
/// JSON escapes a byte to at most six (`\u0001`), so even a chunk of control characters is 48 MiB, under
/// the frame cap, and an ordinary CSV grows by about one byte per line (its `\n`).
pub const UPLOAD_CHUNK_BYTES: usize = 8 * 1024 * 1024;

const _: () = assert!(
    UPLOAD_CHUNK_BYTES * 6 + 64 * 1024 < vike_hub_protocol::frame::MAX_FRAME_LEN as usize,
    "a chunk escaped at its worst must still fit one frame"
);

/// How much of a file a PREVIEW sends ([`CsvUploadSpec::head`]): its first lines, enough for the header,
/// the first rows and the step between them. A file no larger is sent whole.
pub const UPLOAD_HEAD_BYTES: usize = 256 * 1024;

/// **The `CsvUpload` request**: the series and the size of the file about to be sent.
///
/// ⚠ **`dry_run` has NO serde default, deliberately** ([`crate::archive::ImportSpec`]'s rule): a frame
/// that dropped it would otherwise decode as a store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CsvUploadSpec {
    /// Where the bars land: always the lane `user-<label>`, under the target's venue, symbol and
    /// interval; the file's written form and its column mapping.
    pub target: ImportTarget,
    /// The size of the text the chunks will carry, in bytes: the server refuses a file over its cap
    /// here, before any is sent, and an `End` before this many arrived.
    pub bytes: u64,
    /// Decode and plan, store nothing.
    pub dry_run: bool,
    /// The text is the file's first [`UPLOAD_HEAD_BYTES`] only, cut anywhere: the server reads it up to
    /// its last whole line and judges no day of it. Only with `dry_run`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub head: bool,
}

/// The rules both doors run, cheapest first: the target's ([`validate_import_target`]), a file of at
/// least one byte, and a head only as a dry run.
pub fn validate_upload_spec(spec: &CsvUploadSpec) -> Result<(), String> {
    validate_import_target(&spec.target)?;
    if spec.bytes == 0 {
        return Err("the file is empty: it holds no header and no bar".to_string());
    }
    if spec.head && !spec.dry_run {
        return Err(
            "a preview of the file's head stores nothing: a head is only a dry run".to_string()
        );
    }
    Ok(())
}

/// **The `CsvUploaded` answer**: how the datahub read the file and what it stored.
///
/// A file the decoder refuses is not an error frame: the answer carries the refusal AND the header, so a
/// client can offer the file's own column names to map it by.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CsvUploadDone {
    /// The file's first line split into cells as the decoder split it (at most
    /// `vike_data::csv_bars::HEADER_CELLS_MAX`, each cut to `HEADER_CELL_MAX_BYTES`): the column choices.
    pub header: Vec<String>,
    /// The decoder's refusal of the file, naming the line and the rule. `Some` stored nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
    /// The file's first bars as the decoder read them, at most `IMPORT_PREVIEW_ROWS`.
    pub preview: Vec<ImportPlanPreviewRow>,
    /// The bars the file decoded to.
    pub bars: u64,
    /// The UTC days the file holds a bar of.
    pub days: u64,
    /// The first and last of them, each the epoch-ms start of its UTC day.
    pub first_day: Option<i64>,
    /// See [`Self::first_day`].
    pub last_day: Option<i64>,
    /// Days and bars stored: zero for a dry run and for a refused file.
    pub stored_days: u64,
    /// See [`Self::stored_days`].
    pub stored_bars: u64,
    /// The days not stored (a dry run: that would not be), each with the reason: a day that has not
    /// ended yet is too recent.
    pub not_stored: Vec<ImportOutcomeDay>,
}

/// The file's bytes as the text the chunks carry, or the sentence refusing a file that is not UTF-8.
pub fn file_text(bytes: &[u8]) -> Result<&str, String> {
    std::str::from_utf8(bytes).map_err(|e| {
        format!(
            "the file is not UTF-8 text (at byte {}): save it as UTF-8 (a CSV export's \"UTF-8\" \
             option) and send it again",
            e.valid_up_to()
        )
    })
}

/// The first [`UPLOAD_HEAD_BYTES`] of a file as text, cut back to a whole character, and whether that is
/// the whole file: what a preview sends. A file that is not UTF-8 before the cut is refused.
pub fn file_head(bytes: &[u8]) -> Result<(&str, bool), String> {
    if bytes.len() <= UPLOAD_HEAD_BYTES {
        return file_text(bytes).map(|text| (text, true));
    }
    let cut = &bytes[..UPLOAD_HEAD_BYTES];
    match std::str::from_utf8(cut) {
        Ok(text) => Ok((text, false)),
        // A character the cut split in two: the text before it.
        Err(e) if e.error_len().is_none() => {
            Ok((std::str::from_utf8(&cut[..e.valid_up_to()]).unwrap_or_default(), false))
        }
        Err(_) => file_text(cut).map(|text| (text, false)),
    }
}

/// `text` in pieces of at most [`UPLOAD_CHUNK_BYTES`], each cut on a character boundary.
pub fn chunks(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let mut end = rest.len().min(UPLOAD_CHUNK_BYTES);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let (piece, tail) = rest.split_at(end);
        rest = tail;
        Some(piece)
    })
}

#[path = "upload_tests.rs"]
#[cfg(test)]
mod upload_tests;
