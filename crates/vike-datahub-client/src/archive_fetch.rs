//! **The ARCHIVE DOWNLOAD verb's vocabulary** — [`Request::FetchArchive`](crate::proto::Request::FetchArchive)
//! / [`Response::ArchiveFetched`](crate::proto::Response::ArchiveFetched): the datahub downloads the
//! daily files of ONE dataset of a vendor's bucket into its OWN imports folder, so the archive import
//! (`crate::archive`) then reads them unchanged.
//!
//! The owner's ruling of 2026-11-13 (`docs/decisions/0100-the-archive-import-reads-the-datahubs-own-box-and-a-day-has-one-owner.md`'s
//! amendment of that date): the datahub downloads Dukascopy's Requester-Pays S3 archive itself, with
//! the operator's AWS key read from its own box's settings database, replacing the `aws s3 sync` the
//! operator ran by hand. It is the OPERATOR's AWS bill, so a request either PLANS (lists the bucket,
//! compares it with the folder, downloads nothing) or, with [`FetchSpec::go`], downloads what the plan
//! found missing.
//!
//! # What the request names
//!
//! The import's own two words, a FORMAT the server registers and ONE validated dataset NAME
//! ([`crate::archive::validate_import_dataset`]), never a path, a bucket or a key; and an inclusive
//! window of UTC days, REQUIRED here (an import's omitted bound is the dataset's own first or last
//! file, and a download has no dataset yet). The bucket is the server's `venue_url` row, the key the
//! server's credential store's. A downloading request is held to [`FETCH_MAX_DAYS`], so a long window
//! is one request per calendar month, each answering with its progress; a plan is not, because it
//! downloads nothing.

use serde::{Deserialize, Serialize};
use vike_model::MS_PER_DAY;

use crate::archive::{IMPORT_MAX_DAYS, validate_import_dataset};

/// The most days ONE downloading request may cover: the import's own cap, a calendar month, so the
/// client's month cut serves both verbs and each request answers while its caller still waits.
pub const FETCH_MAX_DAYS: i64 = IMPORT_MAX_DAYS;

/// The parallel downloads a request runs when its caller names none. Below the vendor's page's
/// 20 to 30, on purpose: the bill is the operator's and a shared box's bandwidth is too.
pub const FETCH_DEFAULT_CONCURRENCY: u32 = 8;

/// The most parallel downloads a request may ask for: the top of the vendor page's recommendation.
pub const FETCH_MAX_CONCURRENCY: u32 = 30;

/// One [`Request::FetchArchive`](crate::proto::Request::FetchArchive).
///
/// ⚠ **`go` has NO serde default, deliberately** (`crate::archive::ImportSpec::dry_run`'s rule): a
/// frame that dropped it must fail to decode rather than read as a plan, or as a download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchSpec {
    /// The import format whose folder the files land in and whose archive is downloaded:
    /// `"dukascopy-bi5"`.
    pub format: String,
    /// ONE dataset name: the vendor's own folder, `"EURUSD"`.
    pub dataset: String,
    /// Inclusive first day: the epoch-ms START of a UTC day.
    pub from_day: i64,
    /// Inclusive last day: the epoch-ms START of a UTC day.
    pub to_day: i64,
    /// `false`: list and compare, download nothing. `true`: download every missing file of the window.
    pub go: bool,
    /// Parallel downloads, `1..=FETCH_MAX_CONCURRENCY`.
    pub concurrency: u32,
}

/// Hold `spec` to the wire's rules — the client runs it before it sends and the server at its door.
pub fn validate_fetch_spec(spec: &FetchSpec) -> Result<(), String> {
    validate_import_dataset(&spec.dataset)?;
    for (name, day) in [("from", spec.from_day), ("to", spec.to_day)] {
        if day < 0 || day.rem_euclid(MS_PER_DAY) != 0 {
            return Err(format!(
                "a download's `{name}` bound is not the START of a UTC day on or after 1970: \
                 a download takes whole days"
            ));
        }
    }
    if spec.from_day > spec.to_day {
        return Err("a download's first day is AFTER its last: the window holds no day".to_string());
    }
    let days = (spec.to_day - spec.from_day) / MS_PER_DAY + 1;
    if spec.go && days > FETCH_MAX_DAYS {
        return Err(format!(
            "a downloading request covers {days} days, over FETCH_MAX_DAYS = {FETCH_MAX_DAYS}: send \
             one request per calendar month (a plan has no cap)"
        ));
    }
    if !(1..=FETCH_MAX_CONCURRENCY).contains(&spec.concurrency) {
        return Err(format!(
            "a download's concurrency of {} is outside 1 to {FETCH_MAX_CONCURRENCY} (the vendor's \
             page recommends at most {FETCH_MAX_CONCURRENCY} parallel requests)",
            spec.concurrency
        ));
    }
    Ok(())
}

/// The answer to one [`Request::FetchArchive`](crate::proto::Request::FetchArchive): the plan, always,
/// and what the download did when the request said [`FetchSpec::go`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchDone {
    /// What the bucket holds for the window and what the folder lacks.
    pub plan: FetchPlan,
    /// `None` for a plan; the download's result otherwise.
    pub fetched: Option<FetchOutcome>,
}

/// What a download would fetch: the bucket's daily files of the window, against the folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchPlan {
    /// The archive's base address, the server's `venue_url` row (`https://<bucket>.s3.<region>.amazonaws.com`).
    pub source: String,
    /// The dataset's folder ON THE DATAHUB'S BOX, where the files land and the import reads them.
    pub server_dir: String,
    /// The LIST requests this plan sent (one per calendar month of the window, more for a month of
    /// over a thousand objects). Each is billed to the operator's AWS account.
    pub list_requests: u64,
    /// Daily files the bucket holds inside the window, and their bytes.
    pub listed: u64,
    pub listed_bytes: u64,
    /// Of those, the files already in the folder: never downloaded again.
    pub held: u64,
    /// The files the folder lacks, in day order: what a download fetches.
    pub missing: Vec<FetchFile>,
}

impl FetchPlan {
    /// The bytes of every missing file.
    pub fn missing_bytes(&self) -> u64 {
        self.missing.iter().map(|f| f.bytes).sum()
    }
}

/// One daily file of the bucket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchFile {
    /// The UTC day it holds, the epoch-ms of its midnight.
    pub day: i64,
    /// Its object key in the bucket (`EURUSD/2024/00/02_ticks.bi5`): also its path under the
    /// format's folder.
    pub key: String,
    /// Its size, as the bucket listed it.
    pub bytes: u64,
}

/// What a downloading request did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchOutcome {
    /// Files downloaded and written, and their bytes.
    pub fetched: u64,
    pub bytes: u64,
    /// The GET requests sent, retries included: each is billed.
    pub get_requests: u64,
    /// Files that could not be downloaded after the retries, each with why. Every other file of the
    /// plan is in the folder. (A client that goes away gets no answer: the request stops before its
    /// next file, and the files written before then stay.)
    pub failed: Vec<FetchFailure>,
}

/// One file a download gave up on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchFailure {
    /// The day, the epoch-ms of its midnight.
    pub day: i64,
    /// Why, in words.
    pub why: String,
}

#[cfg(test)]
#[path = "archive_fetch_tests.rs"]
mod tests;
