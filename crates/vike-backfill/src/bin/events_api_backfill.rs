//! `data.vike.io` LIVE events API backfill CLI — the no-ClickHouse-no-multi-GB-download path.
//!
//! Data source: `data.vike.io/v1/events?token_id=<id>&from_ms=<ms>&to_ms=<ms>&limit=<n>
//! [&cursor=<c>]`, paged JSON, one token at a time. Auth is an `X-API-Key` header; key from the
//! workspace `.env` (`VIKE_ARCHIVE_API_KEY` — the SAME key the sibling `vike_archive_backfill` bin
//! uses), read HERE (the bin) and passed into the library as a plain parameter —
//! `vike_backfill::events_api` itself never touches the environment.
//!
//! Usage:
//!   events_api_backfill --from YYYY-MM-DD --to YYYY-MM-DD --tokens-file PATH [--tokens T1,T2,...]
//!       [--store DIR] [--base URL] [--kind book|trade|quote|all] [--concurrency N] [--dry-run]
//!
//! One (date, token) pair is one unit of work: the window is that UTC calendar day
//! `[midnight, next midnight)`, and [`vike_backfill::events_api::EventsClient::fetch_all_single`]
//! pages it to completion. `--concurrency` (default 6) runs that many work units' HTTP fetch+page
//! loop concurrently across worker threads — the measured ~0.81s/market round trip is latency-
//! bound, not CPU-bound, so overlapping requests is the win. Every unit's decode + `HistStore`
//! append happens back on the MAIN thread (single-writer; workers only fetch), so concurrency never
//! touches the store concurrently. `--kind` (default `all`) selects which of the three derived
//! series (`book`/`trade`/`quote`) get ingested from each fetched page — note `quote` is currently
//! ALWAYS EMPTY for this endpoint (verified live: `best_bid`/`best_ask` are an unpopulated `0.0`
//! sentinel today, see `events_api`'s module doc) — selecting it is harmless, just a no-op lane.
//!
//! `--dry-run` prints the (date, token) work-unit count and the base URL — no network call, no
//! store opened, nothing fetched or written.
//!
//! Multi-token (`token_ids=`) is detected, not assumed: before the main run, if 2+ tokens are given,
//! one [`vike_backfill::events_api::EventsClient::probe_multi_token`] call checks whether the server
//! accepts it (verified live 2026-07-28: not yet — `HTTP 400 {"error":"get_events requires
//! token_id"}`). Detected `true` batches EVERY token into ONE work unit (fewer, larger requests);
//! `false` (today's reality) falls back to one work unit per token, which is what actually runs.

use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Mutex;
use std::sync::mpsc;

use vike_backfill::CollectError;
use vike_backfill::cli::{CliSpec, arg, has_flag, log_config, store_root};
use vike_backfill::events_api::{
    DEFAULT_BASE, EventRow, EventsClient, FetchStats, IngestCounts, KindsMask, ingest_rows,
};
use vike_bridge_core::credentials::load_workspace_secrets_from_env;
use vike_data::DataFusionHist;
use vike_model::time::{days_in_range, parse_date_label};

/// One UTC calendar day's `[start_ms, end_ms)` half-open window.
fn day_window_ms(date: &str) -> Result<(i64, i64), String> {
    let start = parse_date_label(date)?;
    Ok((start, start + 86_400_000))
}

/// `YYYY-MM-DD` labels for every UTC day in `[from, to]` inclusive (same helper the sibling
/// `vike_archive_backfill` bin carries; small enough not to be worth a shared home yet).
fn day_labels(from: &str, to: &str) -> Vec<String> {
    let (Ok((fy, fm, fd)), Ok((ty, tm, td))) =
        (vike_model::parse_ymd(from), vike_model::parse_ymd(to))
    else {
        return Vec::new();
    };
    days_in_range((fy, fm, fd), (ty, tm, td))
        .into_iter()
        .map(|(y, m, d)| format!("{y:04}-{m:02}-{d:02}"))
        .collect()
}

/// One id per line, whitespace trimmed, blank/`#`-comment lines skipped (the `pmxt_backfill`/
/// `vike_archive_backfill` `--tokens-file` shape).
fn load_tokens_file(path: &Path) -> std::io::Result<Vec<String>> {
    let contents = std::fs::read_to_string(path)?;
    Ok(contents
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect())
}

/// `--tokens` (comma list) and `--tokens-file` union into one de-duplicated, order-preserving list;
/// `Err` when neither is given (unlike the archive bin, an events-API pull is inherently per-token,
/// so "no filter" has no meaning here).
fn resolve_tokens(args: &[String]) -> Result<Vec<String>, String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    let mut push = |t: String| {
        if seen.insert(t.clone()) {
            out.push(t);
        }
    };
    if let Some(s) = arg(args, "--tokens") {
        for t in s.split(',').map(str::trim).filter(|t| !t.is_empty()) {
            push(t.to_string());
        }
    }
    if let Some(path) = arg(args, "--tokens-file") {
        let ids = load_tokens_file(Path::new(&path))
            .map_err(|e| format!("failed to read --tokens-file {path}: {e}"))?;
        for t in ids {
            push(t);
        }
    }
    if out.is_empty() {
        return Err("no tokens given — need --tokens T1,T2,... and/or --tokens-file PATH".into());
    }
    Ok(out)
}

/// One fetch work unit.
struct Unit {
    date: String,
    token: String,
    from_ms: i64,
    to_ms: i64,
}

const USAGE: &str = "\
usage: events_api_backfill --from YYYY-MM-DD --to YYYY-MM-DD
                           --tokens-file PATH [--tokens T1,T2,...]
                           [--store DIR] [--base URL] [--kind book|trade|quote|all]
                           [--concurrency N] [--dry-run]

Backfill from the data.vike.io LIVE events API — paged JSON /v1/events, one token at a time, with
no ClickHouse and no multi-GB day-file download (its bulk-Parquet sibling is
vike_archive_backfill). Work is one unit per (day x token).

  --from DATE       first UTC day, YYYY-MM-DD (inclusive)
  --to DATE         last UTC day, YYYY-MM-DD (inclusive)
  --tokens-file P   token ids to fetch, one per line
  --tokens T1,T2    the same as a comma list; unions with --tokens-file
  --store DIR       hist-store root (default: $VIKE_HIST_STORE, else <repo>/market_data/hist)
  --base URL        API root override
  --kind K          book | trade | quote | all (default all)
  --concurrency N   parallel work units (default 6, minimum 1)
  --dry-run         print the work plan and exit without fetching or writing
  -h, --help        print this and exit 0
  -V, --version     print the version and exit 0

environment:
  VIKE_ARCHIVE_API_KEY   the X-API-Key credential, from the credential store
  VIKE_HIST_STORE        hist-store root when --store is absent";

const SPEC: CliSpec = CliSpec {
    bin: "events_api_backfill",
    usage: USAGE,
    valued: &[
        "--from",
        "--to",
        "--tokens-file",
        "--tokens",
        "--store",
        "--base",
        "--kind",
        "--concurrency",
    ],
    toggles: &["--dry-run"],
    positionals: 0,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = SPEC.short_circuit(&args) {
        return code;
    }
    let _guards = vike_log::init(log_config("events-api-backfill"));

    let (Some(from), Some(to)) = (arg(&args, "--from"), arg(&args, "--to")) else {
        eprintln!("events_api_backfill: --from and --to are required\n\n{USAGE}");
        return ExitCode::from(2);
    };
    let dates = day_labels(&from, &to);
    if dates.is_empty() {
        eprintln!("bad --from/--to (want YYYY-MM-DD) or --from after --to: {from}..{to}");
        return ExitCode::FAILURE;
    }
    let tokens = match resolve_tokens(&args) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let kinds = match KindsMask::from_kind(arg(&args, "--kind").as_deref().unwrap_or("all")) {
        Some(k) => k,
        None => {
            eprintln!("unknown --kind (want book|trade|quote|all)");
            return ExitCode::FAILURE;
        }
    };
    let concurrency: usize =
        arg(&args, "--concurrency").and_then(|s| s.parse().ok()).unwrap_or(6).max(1);
    let base = arg(&args, "--base").unwrap_or_else(|| DEFAULT_BASE.to_string());
    let dry_run = has_flag(&args, "--dry-run");

    let mut units: Vec<Unit> = Vec::with_capacity(dates.len() * tokens.len());
    for date in &dates {
        let (from_ms, to_ms) = match day_window_ms(date) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("bad date {date}: {e}");
                return ExitCode::FAILURE;
            }
        };
        for token in &tokens {
            units.push(Unit { date: date.clone(), token: token.clone(), from_ms, to_ms });
        }
    }

    tracing::info!(
        "events-api-backfill: {} day(s) x {} token(s) = {} work unit(s), kinds={:?}, \
         concurrency={concurrency}, base={base}, dry_run={dry_run}",
        dates.len(),
        tokens.len(),
        units.len(),
        arg(&args, "--kind").unwrap_or_else(|| "all".to_string()),
    );

    if dry_run {
        println!(
            "dry-run: {} day(s) [{from}, {to}] x {} token(s) = {} work unit(s) against {base} \
             (>= 1 request each, more if a token/day pulls more than one page) — \
             nothing fetched, nothing written",
            dates.len(),
            tokens.len(),
            units.len()
        );
        for u in units.iter().take(5) {
            println!("  e.g. {} / {} -> [{}, {})", u.date, u.token, u.from_ms, u.to_ms);
        }
        if units.len() > 5 {
            println!("  ... and {} more", units.len() - 5);
        }
        return ExitCode::SUCCESS;
    }

    // The bin is the ONLY place that reads the environment or a credential store — events_api's
    // library API takes the key as a plain parameter (see the module doc's "libraries take config
    // as parameters" rule). ONE process-environment sweep serves both the credential chain (whose
    // settings-directory override comes out of it) and the
    // hist-store root resolved below.
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let creds = load_workspace_secrets_from_env(&env);
    let api_key = creds.get("VIKE_ARCHIVE_API_KEY").cloned();
    if api_key.is_none() {
        tracing::warn!("VIKE_ARCHIVE_API_KEY not set (credential store) — every request will 401");
    }
    let client = EventsClient::new(base.clone(), api_key);

    // Multi-token detection: one probe call, only worth trying with 2+ distinct tokens. Verified
    // live 2026-07-28: not yet supported (HTTP 400) — see the module doc. Detected-false is the
    // expected, correct outcome today, not a failure.
    let multi_supported = tokens.len() > 1
        && client.probe_multi_token(
            &tokens[..tokens.len().min(2)],
            units[0].from_ms,
            units[0].to_ms,
        );
    tracing::info!(multi_token_supported = multi_supported, "server capability probe");

    let root = store_root(arg(&args, "--store").as_deref(), &env);
    let store = match DataFusionHist::open(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("open hist store at {}: {e}", root.display());
            return ExitCode::FAILURE;
        }
    };

    // Work queue + result channel: workers only fetch (network + JSON page decode); the MAIN thread
    // does every `store.append_*` call, so the store is never touched from more than one thread.
    let queue: Mutex<VecDeque<Unit>> = Mutex::new(units.into_iter().collect());
    let (tx, rx) =
        mpsc::channel::<(String, String, Result<(Vec<EventRow>, FetchStats), CollectError>)>();

    std::thread::scope(|scope| {
        for _ in 0..concurrency {
            let queue = &queue;
            let client = client.clone();
            let tx = tx.clone();
            scope.spawn(move || {
                loop {
                    let unit = {
                        let mut q = queue.lock().unwrap();
                        q.pop_front()
                    };
                    let Some(unit) = unit else { break };
                    let result =
                        client.fetch_all_single(&unit.token, unit.from_ms, unit.to_ms, 2000);
                    if tx.send((unit.date, unit.token, result)).is_err() {
                        break; // main thread gone — nothing left to do
                    }
                }
            });
        }
        drop(tx); // let the receive loop end once every worker's clone is dropped

        let mut total_counts = IngestCounts::default();
        let mut total_stats = FetchStats::default();
        let (mut ok, mut failed) = (0usize, 0usize);
        for (date, token, result) in rx {
            match result {
                Ok((rows, stats)) => {
                    total_stats += stats;
                    match ingest_rows(&store, &token, &date, &rows, &kinds) {
                        Ok(counts) => {
                            tracing::info!(
                                %date, %token, rows = rows.len(), requests = stats.requests,
                                bytes = stats.bytes, book = counts.book, trade = counts.trade,
                                quote = counts.quote, "ingested"
                            );
                            total_counts += counts;
                            ok += 1;
                        }
                        Err(e) => {
                            tracing::error!(%date, %token, "ingest failed: {e} — skipping");
                            failed += 1;
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(%date, %token, "fetch failed: {e} — skipping");
                    failed += 1;
                }
            }
        }
        tracing::info!(
            "events-api-backfill done: {ok} ok, {failed} failed — book={} trade={} quote={} rows, \
             {} request(s), {} bytes received (into {})",
            total_counts.book,
            total_counts.trade,
            total_counts.quote,
            total_stats.requests,
            total_stats.bytes,
            root.display()
        );
        println!(
            "events-api-backfill: {ok} ok, {failed} failed, {} request(s), {} bytes, \
             book={} trade={} quote={} rows ingested",
            total_stats.requests,
            total_stats.bytes,
            total_counts.book,
            total_counts.trade,
            total_counts.quote
        );
        if failed > 0 && ok == 0 { ExitCode::FAILURE } else { ExitCode::SUCCESS }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_labels_spans_a_range_inclusive() {
        assert_eq!(
            day_labels("2026-07-26", "2026-07-27"),
            vec!["2026-07-26".to_string(), "2026-07-27".to_string()]
        );
        assert_eq!(day_labels("2026-07-27", "2026-07-27"), vec!["2026-07-27".to_string()]);
    }

    #[test]
    fn day_labels_empty_on_bad_or_reversed_range() {
        assert!(day_labels("not-a-date", "2026-07-27").is_empty());
        assert!(day_labels("2026-07-27", "2026-07-26").is_empty(), "from after to -> empty");
    }

    #[test]
    fn day_window_ms_spans_exactly_one_day() {
        let (start, end) = day_window_ms("2026-07-28").unwrap();
        assert_eq!(end - start, 86_400_000);
    }

    #[test]
    fn resolve_tokens_unions_inline_and_file_deduped() {
        let dir =
            std::env::temp_dir().join(format!("events_api_backfill_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tokens.txt");
        std::fs::write(&path, "# ids\nid_b\n\n  id_a\n").unwrap();

        let args: Vec<String> =
            ["prog", "--tokens", "id_a, id_b", "--tokens-file", path.to_str().unwrap()]
                .iter()
                .map(|s| s.to_string())
                .collect();
        let got = resolve_tokens(&args).unwrap();
        assert_eq!(got, vec!["id_a".to_string(), "id_b".to_string()], "de-duplicated, order kept");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_tokens_errors_when_neither_flag_given() {
        let args: Vec<String> =
            ["prog", "--from", "2026-01-01"].iter().map(|s| s.to_string()).collect();
        assert!(resolve_tokens(&args).is_err());
    }
}
