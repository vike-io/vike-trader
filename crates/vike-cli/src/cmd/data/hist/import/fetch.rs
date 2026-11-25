//! `import … --fetch`: the datahub first DOWNLOADS the window's missing daily files of the vendor's
//! bucket into the dataset's folder on ITS box (`Request::FetchArchive`, the owner's ruling of
//! 2026-11-13), and the import then runs unchanged. Every line it prints, the cost estimate and the
//! grammar's one rule live here as pure functions; `crate::cmd::data`'s `execute_import` sends.
//!
//! # The flow
//!
//! 1. ONE plan request over the whole window: the datahub lists the bucket month by month and
//!    compares it with the folder. The plan is printed with its ESTIMATED AWS COST, always.
//! 2. `--dry-run` stops there when anything is missing: nothing is downloaded.
//! 3. The download is confirmed by typing `fetch N files` (or `--yes`), because the bill is the
//!    operator's. Then ONE REQUEST PER CALENDAR MONTH that holds a missing file, each answering with
//!    a progress line; a killed run resumes from the same command, since a file in the folder is
//!    never downloaded again.
//! 4. The import's own flow, from its plan on.
//!
//! # The cost
//!
//! An ESTIMATE at AWS's list prices for eu-west-1 (read 2026-11-13): GET $0.0004 per 1,000
//! requests (the vendor page's figure too), LIST $0.005 per 1,000, and internet egress $0.09 per GB.
//! The vendor's page prices transfer at $0.02 per GB, which is AWS's price to another AWS region; a
//! box outside AWS pays the internet rate, so the estimate takes the higher one. AWS's free tier is
//! not subtracted, so the bill is at most this.

use vike_datahub_client::archive_fetch::{
    FETCH_DEFAULT_CONCURRENCY, FETCH_MAX_CONCURRENCY, FetchOutcome, FetchPlan,
};
use vike_model::time::epoch_ms_to_utc_date;
use vike_utility::bytes::iec_bytes;

use super::{counted, month_label, thousands};

/// S3 Standard GET, dollars per 1,000 requests (eu-west-1 list price).
const GET_DOLLARS_PER_THOUSAND: f64 = 0.0004;
/// S3 Standard LIST, dollars per 1,000 requests (eu-west-1 list price).
const LIST_DOLLARS_PER_THOUSAND: f64 = 0.005;
/// Data transfer out to the internet from eu-west-1, dollars per GB (the first 10 TB a month).
const EGRESS_DOLLARS_PER_GB: f64 = 0.09;
/// A GB, as AWS bills it.
const GB: f64 = 1_073_741_824.0;

/// `--fetch`/`--concurrency` resolved: `Some(concurrency)` with `--fetch`, `None` without. A download
/// needs a window the operator named, because the bucket bills it to him: `--from` and `--to` are
/// REQUIRED here, where an import alone defaults them to the dataset's own span.
pub(crate) fn parse(
    fetch: bool,
    concurrency: Option<&str>,
    from_day: Option<i64>,
    to_day: Option<i64>,
) -> Result<Option<u32>, String> {
    if !fetch {
        return match concurrency {
            Some(_) => Err("--concurrency is a knob of --fetch: it sets how many files the \
                            datahub downloads at once. Add --fetch, or drop --concurrency"
                .to_string()),
            None => Ok(None),
        };
    }
    if from_day.is_none() || to_day.is_none() {
        return Err(
            "--fetch downloads a window you name, billed to the AWS account whose key the \
                    datahub holds: give both --from and --to (inclusive UTC days), e.g. \
                    `vike-cli data history import dukascopy-bi5 EURUSD --from 2024-01-02 --to \
                    2024-01-31 --fetch --dry-run`"
                .to_string(),
        );
    }
    let Some(raw) = concurrency else { return Ok(Some(FETCH_DEFAULT_CONCURRENCY)) };
    match raw.trim().parse::<u32>() {
        Ok(n) if (1..=FETCH_MAX_CONCURRENCY).contains(&n) => Ok(Some(n)),
        _ => Err(format!(
            "--concurrency {raw:?} is not a number of parallel downloads from 1 to \
             {FETCH_MAX_CONCURRENCY} (the vendor's page recommends at most {FETCH_MAX_CONCURRENCY}); \
             omit it for {FETCH_DEFAULT_CONCURRENCY}"
        )),
    }
}

/// The estimated bill, in dollars, of `lists` LIST requests, `gets` GET requests and `bytes` out.
pub(crate) fn cost_dollars(lists: u64, gets: u64, bytes: u64) -> f64 {
    lists as f64 / 1000.0 * LIST_DOLLARS_PER_THOUSAND
        + gets as f64 / 1000.0 * GET_DOLLARS_PER_THOUSAND
        + bytes as f64 / GB * EGRESS_DOLLARS_PER_GB
}

/// Dollars to a reader: four decimals below a cent, two above.
fn dollars(d: f64) -> String {
    if d < 0.01 { format!("${d:.4}") } else { format!("${d:.2}") }
}

/// The plan as the operator reads it, every number the SERVER's.
pub(crate) fn plan_lines(plan: &FetchPlan, addr: &str) -> Vec<String> {
    let bytes = plan.missing_bytes();
    let gets = plan.missing.len() as u64;
    let mut out = vec![
        format!("archive download — datahub at {addr}"),
        format!(
            "  bucket      {}   (Requester Pays: billed to the AWS key stored on the datahub's box)",
            plan.source
        ),
        format!("  folder      {}   (on the DATAHUB's box)", plan.server_dir),
        format!(
            "  listed      {} in the window, {} — {} sent for this plan, {}",
            counted(plan.listed, "daily file"),
            iec_bytes(plan.listed_bytes),
            counted(plan.list_requests, "LIST request"),
            dollars(cost_dollars(plan.list_requests, 0, 0))
        ),
        format!(
            "  held        {} already in the folder, never downloaded again",
            thousands(plan.held)
        ),
    ];
    let span = match (plan.missing.first(), plan.missing.last()) {
        (Some(a), Some(b)) => {
            format!(" ({} .. {})", epoch_ms_to_utc_date(a.day), epoch_ms_to_utc_date(b.day))
        }
        _ => String::new(),
    };
    out.push(format!(
        "  download    {}{span}, {} — estimated {} ({} at $0.0004 per 1,000 plus {} out at \
         $0.09 per GB: eu-west-1 list prices, free tier not subtracted)",
        counted(gets, "file"),
        iec_bytes(bytes),
        dollars(cost_dollars(0, gets, bytes)),
        counted(gets, "GET"),
        iec_bytes(bytes)
    ));
    out
}

/// The plan of a download nobody is billed for (the SEC EDGAR formats): the source, the folder, what
/// the window holds and what is missing, every number the server's. [`plan_lines`] is Dukascopy's,
/// with its AWS bill.
pub(crate) fn free_plan_lines(plan: &FetchPlan, addr: &str) -> Vec<String> {
    let span = match (plan.missing.first(), plan.missing.last()) {
        (Some(a), Some(b)) => {
            format!(" ({} .. {})", epoch_ms_to_utc_date(a.day), epoch_ms_to_utc_date(b.day))
        }
        _ => String::new(),
    };
    vec![
        format!("archive download — datahub at {addr}"),
        format!(
            "  source      {}   (no charge; the datahub's own request rate limit)",
            plan.source
        ),
        format!("  folder      {}   (on the DATAHUB's box)", plan.server_dir),
        format!(
            "  listed      {} in the window, {} — {} sent for this plan",
            counted(plan.listed, "file"),
            iec_bytes(plan.listed_bytes),
            counted(plan.list_requests, "index request")
        ),
        format!(
            "  held        {} already in the folder, never downloaded again",
            thousands(plan.held)
        ),
        format!("  download    {}{span}", counted(plan.missing.len() as u64, "file")),
    ]
}

/// The summary of a download nobody is billed for ([`free_plan_lines`]).
pub(crate) fn free_summary_line(done: &FetchOutcome) -> String {
    format!(
        "downloaded {}, {}, with {}",
        counted(done.fetched, "file"),
        iec_bytes(done.bytes),
        counted(done.get_requests, "request")
    )
}

/// The typed confirmation of a download, bound to the plan's own count.
pub(crate) fn confirmation(plan: &FetchPlan) -> String {
    format!("fetch {} files", plan.missing.len())
}

/// The calendar months of `months` that hold a missing file of `plan`: the only ones asked for.
pub(crate) fn months_to_fetch(plan: &FetchPlan, months: &[(i64, i64)]) -> Vec<(i64, i64)> {
    months
        .iter()
        .copied()
        .filter(|(a, b)| plan.missing.iter().any(|f| (*a..=*b).contains(&f.day)))
        .collect()
}

/// One month's progress line, on stderr.
pub(crate) fn progress_line(index: usize, count: usize, from: i64, done: &FetchOutcome) -> String {
    let failed = match done.failed.len() {
        0 => String::new(),
        n => format!(", {} FAILED", counted(n as u64, "file")),
    };
    format!(
        "  [{index}/{count}] {}  downloaded {}, {}{failed}",
        month_label(from),
        counted(done.fetched, "file"),
        iec_bytes(done.bytes)
    )
}

/// The download's summary line, and its bill at the same prices.
pub(crate) fn summary_line(plan_lists: u64, done: &FetchOutcome, month_lists: u64) -> String {
    let lists = plan_lists + month_lists;
    format!(
        "downloaded {}, {}, with {} and {} — estimated {}",
        counted(done.fetched, "file"),
        iec_bytes(done.bytes),
        counted(done.get_requests, "GET"),
        counted(lists, "LIST"),
        dollars(cost_dollars(lists, done.get_requests, done.bytes))
    )
}

/// The sentence a download with failed files ends on: every one by day, and how to resume.
pub(crate) fn failure(done: &FetchOutcome) -> String {
    let days: Vec<String> = done
        .failed
        .iter()
        .map(|f| format!("\n  {}  {}", epoch_ms_to_utc_date(f.day), f.why))
        .collect();
    format!(
        "{} could not be downloaded; every other file is in the folder. Run the same command again \
         to retry them (nothing downloaded is fetched twice), then the import runs:{}",
        counted(done.failed.len() as u64, "file"),
        days.concat()
    )
}

#[cfg(test)]
#[path = "fetch_tests.rs"]
mod tests;
