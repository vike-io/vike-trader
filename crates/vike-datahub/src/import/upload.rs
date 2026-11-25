//! **A user's CSV UPLOAD** — `Request::CsvUpload`'s server half (decision
//! `docs/decisions/0272-a-users-csv-is-uploaded-into-a-user-lane-under-the-observe-key.md`): the door,
//! the session that gathers the file's text, and the seam a format that takes an upload implements
//! ([`CsvUploadFormat`], `csv-bars` alone). The wire's exchange is `vike_datahub_client::upload`'s module
//! doc; the socket half (reading the chunks off the connection) is `crate::server`'s.
//!
//! # The order of the door, cheapest refusal first
//!
//! 1. no import lane mounted, or none whose format takes an upload → the capability refusal;
//! 2. the upload NOT ARMED (the `flags.datahub_csv_upload` row off) → refused having read nothing;
//! 3. the shared validator (`vike_datahub_client::upload::validate_upload_spec`), the SAME the client ran;
//! 4. the declared size over the format's file cap;
//! 5. the format's own admission ([`CsvUploadFormat::admit_upload`]): the series must hold no row, and a
//!    new user series must fit the server's count cap;
//! 6. the ONE slot the archive import takes too (`DatahubRefusal::ImportBusy`'s rule: one decoding request at a
//!    time, each holding a whole file in memory).
//!
//! Each refusal is a `vike_datahub_client::refusals::DatahubRefusal` (`datahub.csv_upload.*`), answered as the
//! refusal frame. Every refusal comes before a byte of the file is sent. The session then holds the file's text in
//! MEMORY, never on disk: a session that ends any other way than a complete [`CsvUploadSession::end`]
//! drops it, and nothing is stored.

use vike_datahub_client::proto::Response;
use vike_datahub_client::refusals::DatahubRefusal;
use vike_datahub_client::upload::{CsvUploadDone, CsvUploadSpec, validate_upload_spec};

use super::{ImportLane, Slot};

/// A format that takes a user's file SENT over the wire rather than placed under the imports root.
pub trait CsvUploadFormat: Send + Sync {
    /// Refuse an upload whose series this format may not create, before a byte of the file is sent:
    /// one that already holds rows (an upload creates a series and never adds to one), and a new user
    /// series past the server's count cap.
    fn admit_upload(&self, spec: &CsvUploadSpec) -> Result<(), String>;
    /// Decode `text` (the whole file, or a preview's head) once, plan its days and — unless the
    /// upload is a dry run — store them. `Err` is the store failing, not the file: a file the decoder
    /// refuses is `Ok` with [`CsvUploadDone::refused`].
    fn run_upload(
        &self,
        spec: &CsvUploadSpec,
        text: &str,
        now_ms: i64,
    ) -> Result<CsvUploadDone, String>;
    /// The largest file this format takes, in bytes.
    fn max_upload_bytes(&self) -> u64;
}

/// One open upload: the admitted request, the text received so far, and the held slot.
pub struct CsvUploadSession<'l> {
    format: &'l dyn CsvUploadFormat,
    spec: CsvUploadSpec,
    text: String,
    _slot: Slot<'l>,
}

/// **The door** — see the module doc for the order. `Ok` holds the one slot until the session drops.
pub fn begin(
    lane: Option<&ImportLane>,
    spec: CsvUploadSpec,
) -> Result<CsvUploadSession<'_>, DatahubRefusal> {
    let Some((lane, format)) = lane.and_then(|l| l.upload_format().map(|f| (l, f))) else {
        return Err(DatahubRefusal::UploadNoLane);
    };
    if !lane.upload_armed {
        return Err(DatahubRefusal::UploadNotArmed);
    }
    validate_upload_spec(&spec).map_err(|why| DatahubRefusal::UploadSpecRefused { why })?;
    let cap = format.max_upload_bytes();
    if spec.bytes > cap {
        return Err(DatahubRefusal::UploadTooLarge { bytes: spec.bytes, cap });
    }
    format.admit_upload(&spec).map_err(|why| DatahubRefusal::UploadFormatRefused { why })?;
    let Some(slot) = lane.take_slot() else {
        return Err(DatahubRefusal::UploadBusy);
    };
    tracing::info!(
        venue = %spec.target.venue,
        symbol = %spec.target.symbol,
        interval = %spec.target.interval,
        label = %spec.target.label,
        bytes = spec.bytes,
        dry_run = spec.dry_run,
        head = spec.head,
        "vike-datahub: CsvUpload opened"
    );
    Ok(CsvUploadSession { format, text: String::new(), spec, _slot: slot })
}

impl CsvUploadSession<'_> {
    /// Take the next piece of the file: the running total, or the refusal of a piece that runs past
    /// the declared size (the session is then over and nothing is stored).
    pub fn push(&mut self, piece: &str) -> Result<u64, DatahubRefusal> {
        let total = self.text.len() as u64 + piece.len() as u64;
        if total > self.spec.bytes {
            return Err(DatahubRefusal::UploadOverrun { declared: self.spec.bytes, total });
        }
        self.text.push_str(piece);
        Ok(total)
    }

    /// End the upload: decode and — unless a dry run — store, when every declared byte arrived;
    /// refused, with nothing stored, when some did not.
    pub fn end(self, now_ms: i64) -> Response {
        let got = self.text.len() as u64;
        if got != self.spec.bytes {
            let declared = self.spec.bytes;
            return Response::Refused(DatahubRefusal::UploadShort { got, declared }.into());
        }
        match self.format.run_upload(&self.spec, &self.text, now_ms) {
            Ok(done) => {
                tracing::info!(
                    label = %self.spec.target.label,
                    bars = done.bars,
                    stored_days = done.stored_days,
                    refused = done.refused.is_some(),
                    dry_run = self.spec.dry_run,
                    "vike-datahub: CsvUpload finished"
                );
                Response::CsvUploaded(Box::new(done))
            }
            Err(why) => Response::Refused(DatahubRefusal::UploadFailed { why }.into()),
        }
    }
}

#[path = "upload_tests.rs"]
#[cfg(test)]
mod upload_tests;
