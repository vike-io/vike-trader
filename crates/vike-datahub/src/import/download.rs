//! **Dukascopy's archive DOWNLOAD** — the datahub lists the vendor's Requester-Pays S3 bucket with
//! the operator's own AWS key and fetches the missing daily `.bi5` files into
//! `<imports root>/dukascopy-bi5/<SYMBOL>/`, where the archive import then reads them unchanged. The
//! owner's ruling of 2026-11-13, `docs/decisions/0100-the-archive-import-reads-the-datahubs-own-box-and-a-day-has-one-owner.md`'s
//! amendment of that date; the wire is `vike_datahub_client::archive_fetch`'s.
//!
//! # What is configuration, and where it lives
//!
//! - **The bucket** is a VENDOR ENDPOINT: the `venue_url.dukascopy.live.history.rest.archive` row
//!   (`https://<bucket>.s3.<region>.amazonaws.com`, seeded by `vike_venues::venue_urls::VENUE_URL_SEED`),
//!   read ONCE by the binary at start ([`mount`]); the region is read off its host.
//! - **The key** is two DEPLOYMENT names of the box's credential store, [`ACCESS_KEY_ID_NAME`] and
//!   [`SECRET_ACCESS_KEY_NAME`], read PER REQUEST through the one-name scoped reader the binary hands
//!   down (`crate::tool_feeds::ToolFeedKeyReaderStore`, decision 0097's pattern): a key stored,
//!   rotated or removed is what the next request sees, no restart. It is never a venue's credential:
//!   this process reads none (`CredentialScope::NoVenueKeys`).
//! - **The layout** is `SYMBOL/YYYY/MM/DD_ticks.bi5` with a zero-based month, the import's own grammar
//!   (`vike_bi5::classify_bi5_path`): a listed key is fetched only when it is a DAILY file of the
//!   window, so no key can name a path outside the dataset's folder.
//!
//! # What a request costs, and who pays
//!
//! A plan sends one LIST per calendar month of the window (more for a month over a thousand objects);
//! a download one GET per missing file, plus its retries. The bucket bills both, and the egress, to the
//! key's AWS account. A file already in the folder is never fetched again, whatever its size: the
//! import keys each day once, so a re-fetched day would not be imported again anyway.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_bi5::{Bi5Path, classify_bi5_path};
use vike_catalog::HistoryChannelLane;
use vike_data::archive_day::DUKASCOPY_BI5_FORMAT;
use vike_datahub_client::archive_fetch::{
    FetchDone, FetchFailure, FetchFile, FetchOutcome, FetchPlan, FetchSpec,
};
use vike_model::MS_PER_DAY;
use vike_utility::calendar::civil_from_days;

use super::ArchiveDownload;
use super::s3::{S3Bucket, S3Error, S3Key, region_and_bucket};
use crate::tool_feeds::{ToolFeedKeyRead, ToolFeedKeyReader};

/// The credential-store name of the operator's AWS access key id. ⚠ The settings registry reads this
/// literal as THIS crate reading the credential (`crates/vike-config/src/settings/rows/daemons.rs`),
/// and `vike_venues::credential_keys::STORE_KEYS_OUTSIDE_THE_GRID` is what lets `vike-cli secrets set`
/// write it: re-key all three together.
pub const ACCESS_KEY_ID_NAME: &str = "DUKASCOPY_ARCHIVE_AWS_ACCESS_KEY_ID";

/// The credential-store name of the operator's AWS secret access key (see [`ACCESS_KEY_ID_NAME`]).
pub const SECRET_ACCESS_KEY_NAME: &str = "DUKASCOPY_ARCHIVE_AWS_SECRET_ACCESS_KEY";

/// The `venue_url` row's product word: `venue_url.dukascopy.live.history.rest.archive`.
pub const URL_PRODUCT: &str = "archive";

/// The history table's `Built` row this download is: `crates/vike-datahub/tests/history_channels_gate.rs`
/// holds the row to this line.
pub const DOWNLOAD_LANE: (&str, HistoryChannelLane) =
    ("dukascopy", HistoryChannelLane::ArchiveDownload);

/// How often the request's stop probe is asked while the files download.
const STOP_POLL: Duration = Duration::from_millis(100);

/// **The download over one bucket**, its key read per request through `keys`.
pub struct DukascopyDownload {
    base: String,
    region: String,
    keys: Arc<dyn ToolFeedKeyReader>,
}

impl DukascopyDownload {
    /// Over the bucket at `base` in `region`, the key read through `keys` on every request.
    pub fn new(base: &str, region: &str, keys: Arc<dyn ToolFeedKeyReader>) -> Self {
        Self { base: base.to_string(), region: region.to_string(), keys }
    }

    /// The bucket's name, for a message: the first label of a virtual-hosted base.
    fn bucket_name(&self) -> String {
        region_and_bucket(&self.base).map_or_else(|| self.base.clone(), |(_, bucket)| bucket)
    }

    /// The key pair, or the whole-request refusal that says what to do.
    fn key(&self) -> Result<S3Key, String> {
        match (self.keys.read(ACCESS_KEY_ID_NAME), self.keys.read(SECRET_ACCESS_KEY_NAME)) {
            (
                ToolFeedKeyRead::Present(access_key_id),
                ToolFeedKeyRead::Present(secret_access_key),
            ) => Ok(S3Key { access_key_id, secret_access_key }),
            (ToolFeedKeyRead::Unreadable, _) | (_, ToolFeedKeyRead::Unreadable) => {
                Err("the credential store on the datahub's box exists and could not be read (the \
                 datahub's log says why); that is NOT the same as no key. Nothing was listed and \
                 nothing was downloaded."
                    .to_string())
            }
            _ => Err(no_key_message()),
        }
    }

    /// What the bucket answered, in the words an operator acts on.
    fn refusal(&self, what: &str, error: &S3Error) -> String {
        let bucket = self.bucket_name();
        match error {
            S3Error::Status { status: 403, code } => {
                let code = code.as_deref().unwrap_or("no error code");
                let wrong_pair = matches!(code, "InvalidAccessKeyId" | "SignatureDoesNotMatch");
                let cause = if wrong_pair {
                    format!(
                        "the stored key pair is wrong: {ACCESS_KEY_ID_NAME} names no key AWS knows, \
                         or {SECRET_ACCESS_KEY_NAME} is not its secret. Store the pair again"
                    )
                } else {
                    format!(
                        "the stored AWS key lacks S3 rights on the bucket {bucket}. Its IAM user \
                         needs s3:ListBucket on arn:aws:s3:::{bucket} and s3:GetObject on \
                         arn:aws:s3:::{bucket}/*, and its account pays every request (Requester \
                         Pays)"
                    )
                };
                format!(
                    "{what}: the bucket {bucket} answered HTTP 403 ({code}): {cause}. Nothing more \
                     was downloaded."
                )
            }
            other => format!("{what}: the bucket {bucket} answered {other}, after its retries"),
        }
    }

    /// Steps one and two: list each month of the window and compare it with `dir`.
    fn plan(&self, bucket: &S3Bucket, dir: &Path, spec: &FetchSpec) -> Result<FetchPlan, String> {
        let now = vike_model::now_ms();
        let mut plan = FetchPlan {
            source: self.base.clone(),
            server_dir: dir.display().to_string(),
            list_requests: 0,
            listed: 0,
            listed_bytes: 0,
            held: 0,
            missing: Vec::new(),
        };
        let root = format!("{}/", spec.dataset);
        for prefix in month_prefixes(&spec.dataset, spec.from_day, spec.to_day) {
            let (listed, requests) = bucket.list(&prefix, &std::thread::sleep);
            plan.list_requests += requests;
            let objects = listed.map_err(|e| self.refusal(&format!("listing {prefix}"), &e))?;
            for object in objects {
                let Some(rel) = object.key.strip_prefix(&root) else { continue };
                let segments: Vec<&str> = rel.split('/').collect();
                let Bi5Path::Daily(day) = classify_bi5_path(&segments, now) else { continue };
                let day = day.start_ms();
                if day < spec.from_day || day > spec.to_day {
                    continue;
                }
                plan.listed += 1;
                plan.listed_bytes += object.bytes;
                if dir.join(rel).is_file() {
                    plan.held += 1;
                } else {
                    plan.missing.push(FetchFile { day, key: object.key, bytes: object.bytes });
                }
            }
        }
        plan.missing.sort_by_key(|f| f.day);
        Ok(plan)
    }

    /// Step three: fetch `plan.missing` with `concurrency` workers, each file written atomically.
    fn download(
        &self,
        bucket: &S3Bucket,
        dir: &Path,
        spec: &FetchSpec,
        plan: &FetchPlan,
        cap: u64,
        should_stop: &dyn Fn() -> bool,
    ) -> FetchOutcome {
        let next = AtomicUsize::new(0);
        let stop = AtomicBool::new(false);
        let outcome = Mutex::new(FetchOutcome::default());
        let root = format!("{}/", spec.dataset);
        let workers = (spec.concurrency as usize).min(plan.missing.len()).max(1);
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..workers)
                .map(|_| {
                    scope.spawn(|| {
                        while !stop.load(Ordering::Acquire) {
                            let Some(file) = plan.missing.get(next.fetch_add(1, Ordering::AcqRel))
                            else {
                                break;
                            };
                            let (got, requests) =
                                bucket.object(&file.key, cap, &std::thread::sleep);
                            let written = got
                                .map_err(|e| self.refusal(&format!("downloading {}", file.key), &e))
                                .and_then(|bytes| {
                                    let rel = file.key.strip_prefix(&root).unwrap_or(&file.key);
                                    write(&dir.join(rel), &bytes).map(|()| bytes.len() as u64)
                                });
                            let mut o = outcome.lock().expect("the outcome lock");
                            o.get_requests += requests;
                            match written {
                                Ok(bytes) => {
                                    o.fetched += 1;
                                    o.bytes += bytes;
                                }
                                Err(why) => o.failed.push(FetchFailure { day: file.day, why }),
                            }
                        }
                    })
                })
                .collect();
            while handles.iter().any(|h| !h.is_finished()) {
                if should_stop() {
                    stop.store(true, Ordering::Release);
                }
                std::thread::sleep(STOP_POLL);
            }
        });
        let mut outcome = outcome.into_inner().expect("the outcome lock");
        outcome.failed.sort_by_key(|f| f.day);
        outcome
    }
}

impl ArchiveDownload for DukascopyDownload {
    fn format(&self) -> &'static str {
        DUKASCOPY_BI5_FORMAT
    }

    fn fetch(
        &self,
        dataset_dir: &Path,
        spec: &FetchSpec,
        cap: u64,
        should_stop: &dyn Fn() -> bool,
    ) -> Result<FetchDone, String> {
        let bucket = S3Bucket::new(&self.base, &self.region, self.key()?)?;
        let plan = self.plan(&bucket, dataset_dir, spec)?;
        let fetched = (spec.go && !plan.missing.is_empty())
            .then(|| self.download(&bucket, dataset_dir, spec, &plan, cap, should_stop));
        Ok(FetchDone { plan, fetched })
    }
}

/// The whole-request refusal with no key stored: the two lines that store one.
pub fn no_key_message() -> String {
    format!(
        "this datahub holds no AWS key for Dukascopy's archive. Store the key pair on the DATAHUB's \
         box, each value typed or piped on stdin and never on the command line:\n  \
         vike-cli secrets set {ACCESS_KEY_ID_NAME}\n  \
         vike-cli secrets set {SECRET_ACCESS_KEY_NAME}\n\
         Its IAM user needs s3:ListBucket and s3:GetObject on the bucket, and its AWS account pays \
         every request (Requester Pays). The next request reads the pair: no restart. Nothing was \
         listed and nothing was downloaded."
    )
}

/// One listing prefix per calendar month of `[from_day, to_day]`: `SYMBOL/YYYY/MM/`, the month
/// ZERO-based as the bucket spells it.
fn month_prefixes(dataset: &str, from_day: i64, to_day: i64) -> Vec<String> {
    let month = |day: i64| {
        let (y, m, _) = civil_from_days(day.div_euclid(MS_PER_DAY));
        y * 12 + i64::from(m) - 1
    };
    (month(from_day)..=month(to_day))
        .map(|n| format!("{dataset}/{:04}/{:02}/", n.div_euclid(12), n.rem_euclid(12)))
        .collect()
}

/// Write `bytes` at `path` atomically, its folders created: a reader sees the whole file or none.
fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let shown = path.display();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    vike_utility::atomic_file::write_atomic(path, bytes)
        .map_err(|e| format!("writing {shown}: {e}"))
}

/// **Mount the download at start**: the bucket from `settings_dir`'s `venue_url` rows (the seed's on
/// a box with no database), the key read per request through `keys`. `Err` is the startup line that
/// says why none is mounted.
pub fn mount(
    settings_dir: &Path,
    keys: Arc<dyn ToolFeedKeyReader>,
) -> Result<DukascopyDownload, String> {
    use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlType};
    let rows = vike_secrets::venue_url::read_venue_urls(settings_dir)
        .map_err(|e| format!("the venue_url rows could not be read: {e}"))?;
    let base = vike_secrets::venue_url::venue_urls_of(&rows, DOWNLOAD_LANE.0)
        .url(UrlType::Live, UrlChannel::History, UrlProtocol::Rest, URL_PRODUCT)
        .map_err(|e| e.to_string())?
        .to_string();
    let (region, _) = region_and_bucket(&base).ok_or_else(|| {
        format!(
            "venue_url.dukascopy.live.history.rest.{URL_PRODUCT} is {base}, which is not \
             https://<bucket>.s3.<region>.amazonaws.com"
        )
    })?;
    Ok(DukascopyDownload::new(&base, &region, keys))
}

#[cfg(test)]
#[path = "download_tests.rs"]
mod tests;
