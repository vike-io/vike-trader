//! `data.vike.io` LIVE events API backfill CLI — the no-ClickHouse-no-multi-GB-download path.
//!
//! Data source: `data.vike.io/v1/events?token_id=<id>&from_ms=<ms>&to_ms=<ms>&limit=<n>
//! [&cursor=<c>]`, paged JSON, one token at a time. Auth is an `X-API-Key` header; key from the
//! credential store (the settings database) via
//! `vike_backfill::cli::scoped_credentials` (`VIKE_ARCHIVE_API_KEY` — the SAME key the sibling
//! `vike_archive_backfill` bin uses), read HERE (the bin) and passed into the library as a plain
//! parameter — `vike_backfill::events_api` itself never touches the environment.
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
//! touches the store concurrently. The workers hand results to it over a BOUNDED channel
//! ([`vike_backfill::events_api::run_pool`]), so a slow store stalls the fetches rather than letting
//! finished units pile up in memory. `--kind` (default `all`) selects which of the three derived
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

use std::collections::HashSet;
use std::ops::ControlFlow;
use std::path::Path;
use std::process::ExitCode;

use vike_backfill::cli::{day_labels, log_config, parse, scoped_credentials, store_root};
use vike_backfill::events_api::{
    DEFAULT_BASE, EventsClient, FetchStats, IngestCounts, KindsMask, ingest_rows, run_pool,
};
use vike_model::parse_date_label;
use vike_secrets::KeyScope;
use vike_store::DataFusionHist;

/// **Every credential name this binary declares** — owner ruling 2026-09-16, a process
/// materialises only what it asked for.
///
/// One name, the same `X-API-Key` the sibling `vike_archive_backfill` bin reads. This run used to
/// resolve the whole credential store into memory to find it.
const SCOPE: [&str; 1] = ["VIKE_ARCHIVE_API_KEY"];

/// The `X-API-Key` credential out of the SCOPED credential answer.
///
/// # ⚠ `Ok(None)` and `Err` are different events
///
/// `Ok(None)` is the credential genuinely not being in the store — `main` warns that every request
/// will 401 and carries on. `Err` is the name having fallen out of [`SCOPE`], which is a defect in
/// this binary rather than a fact about the box; an `Option` return would have made the two
/// indistinguishable, which is the failure `vike_secrets::Lookup` exists to prevent.
///
/// # Errors
/// [`vike_secrets::UndeclaredKey`] when the name asked for is outside [`SCOPE`].
fn api_key(
    store: &vike_secrets::ScopedSecrets,
) -> Result<Option<String>, vike_secrets::UndeclaredKey> {
    Ok(store.get("VIKE_ARCHIVE_API_KEY").declared()?.map(str::to_string))
}

/// One UTC calendar day's `[start_ms, end_ms)` half-open window.
fn day_window_ms(date: &str) -> Result<(i64, i64), String> {
    let start = parse_date_label(date)?;
    Ok((start, start + 86_400_000))
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
fn resolve_tokens(tokens: Option<&str>, tokens_file: Option<&str>) -> Result<Vec<String>, String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    let mut push = |t: String| {
        if seen.insert(t.clone()) {
            out.push(t);
        }
    };
    if let Some(s) = tokens {
        for t in s.split(',').map(str::trim).filter(|t| !t.is_empty()) {
            push(t.to_string());
        }
    }
    if let Some(path) = tokens_file {
        let ids = load_tokens_file(Path::new(path))
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

/// Backfill from the data.vike.io LIVE events API — paged JSON /v1/events, one token at a time,
/// with no ClickHouse and no multi-GB day-file download (its bulk-Parquet sibling is
/// vike_archive_backfill). Work is one unit per (day x token).
#[derive(clap::Parser, Debug)]
#[command(
    after_help = "environment:\n  VIKE_ARCHIVE_API_KEY   the X-API-Key credential, from the \
                        credential store"
)]
struct Args {
    /// First UTC day, YYYY-MM-DD (inclusive)
    #[arg(long)]
    from: String,
    /// Last UTC day, YYYY-MM-DD (inclusive)
    #[arg(long)]
    to: String,
    /// Token ids to fetch, one per line
    #[arg(long)]
    tokens_file: Option<String>,
    /// The same as a comma list; unions with --tokens-file
    #[arg(long)]
    tokens: Option<String>,
    /// Hist-store root (default: <repo>/market_data/hist, else the project's own)
    #[arg(long)]
    store: Option<String>,
    /// API root override
    #[arg(long)]
    base: Option<String>,
    /// book | trade | quote | all
    #[arg(long, default_value = "all")]
    kind: String,
    /// Parallel work units (minimum 1)
    #[arg(long, default_value_t = 6)]
    concurrency: usize,
    /// Print the work plan and exit without fetching or writing
    #[arg(long)]
    dry_run: bool,
}

fn main() -> ExitCode {
    // ⚠ FIRST — `--help`, `--version` and a usage error answer before any log file, store or call.
    let Args { from, to, tokens, tokens_file, store, base, kind, concurrency, dry_run } =
        parse("events_api_backfill");
    // ONE process-environment sweep, at the root — see the note where the key is read below.
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let _guards = vike_log::init(log_config("events-api-backfill", &env));

    let dates = day_labels(&from, &to);
    if dates.is_empty() {
        eprintln!("bad --from/--to (want YYYY-MM-DD) or --from after --to: {from}..{to}");
        return ExitCode::FAILURE;
    }
    let tokens = match resolve_tokens(tokens.as_deref(), tokens_file.as_deref()) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let kinds = match KindsMask::from_kind(&kind) {
        Some(k) => k,
        None => {
            eprintln!("unknown --kind (want book|trade|quote|all)");
            return ExitCode::FAILURE;
        }
    };
    let concurrency = concurrency.max(1);
    let base = base.unwrap_or_else(|| DEFAULT_BASE.to_string());

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
        kind,
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
    // as parameters" rule). ONE process-environment sweep (`env`, taken first in `main`) serves the
    // log level, the credential chain (whose settings-directory override comes out of it) and the
    // hist-store root resolved below.
    let creds = match scoped_credentials(&env, &KeyScope::of(SCOPE)) {
        Ok(creds) => creds,
        Err(e) => {
            eprintln!("events_api_backfill: {e}");
            return ExitCode::FAILURE;
        }
    };
    // ⚠ An UNDECLARED name is a defect in THIS BINARY, not an unconfigured box, so it must never
    // degrade into the live gate's silence — which is exactly what an `Option` here would make it.
    let api_key = match api_key(&creds) {
        Ok(v) => v,
        Err(undeclared) => {
            eprintln!("credential scope defect: {undeclared}");
            return ExitCode::FAILURE;
        }
    };
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

    let root = store_root(store.as_deref(), &env);
    let store = match DataFusionHist::open(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("open hist store at {}: {e}", root.display());
            return ExitCode::FAILURE;
        }
    };

    // Workers only fetch (network + JSON page decode); the MAIN thread does every `store.append_*`
    // call, so the store is never touched from more than one thread. `run_pool`'s result channel is
    // bounded, so a store that falls behind stalls the fetches instead of buffering them.
    let mut total_counts = IngestCounts::default();
    let mut total_stats = FetchStats::default();
    let (mut ok, mut failed) = (0usize, 0usize);
    run_pool(
        units,
        concurrency,
        |unit: Unit| {
            let result = client.fetch_all_single(&unit.token, unit.from_ms, unit.to_ms, 2000);
            (unit.date, unit.token, result)
        },
        |(date, token, result)| {
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
            ControlFlow::Continue(())
        },
    );
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
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let got = resolve_tokens(Some("id_a, id_b"), path.to_str()).unwrap();
        assert_eq!(got, vec!["id_a".to_string(), "id_b".to_string()], "de-duplicated, order kept");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_tokens_errors_when_neither_flag_given() {
        assert!(resolve_tokens(None, None).is_err());
    }

    fn parsed(v: &[&str]) -> Result<Args, vike_util::cli::CliExit> {
        vike_backfill::cli::try_parse_from("events_api_backfill", v.iter().copied())
    }

    /// Every flag lands in its own field, in both spellings, and the numeric default is 6.
    #[test]
    fn every_flag_maps_in_both_spellings() {
        let spaced = parsed(&[
            "--from",
            "2026-01-01",
            "--to",
            "2026-01-02",
            "--tokens-file",
            "/t",
            "--tokens",
            "a,b",
            "--store",
            "/s",
            "--base",
            "http://h",
            "--kind",
            "book",
            "--concurrency",
            "2",
            "--dry-run",
        ])
        .expect("the documented flag set parses");
        let inline = parsed(&[
            "--from=2026-01-01",
            "--to=2026-01-02",
            "--tokens-file=/t",
            "--tokens=a,b",
            "--store=/s",
            "--base=http://h",
            "--kind=book",
            "--concurrency=2",
            "--dry-run",
        ])
        .expect("the inline spelling parses");
        for a in [&spaced, &inline] {
            assert_eq!((a.from.as_str(), a.to.as_str()), ("2026-01-01", "2026-01-02"));
            assert_eq!(a.tokens.as_deref(), Some("a,b"));
            assert_eq!(a.tokens_file.as_deref(), Some("/t"));
            assert_eq!((a.kind.as_str(), a.concurrency, a.dry_run), ("book", 2, true));
            assert_eq!(a.base.as_deref(), Some("http://h"));
        }
        let defaults = parsed(&["--from", "a", "--to", "b"]).expect("two flags are enough");
        assert_eq!(
            (defaults.kind.as_str(), defaults.concurrency, defaults.dry_run),
            ("all", 6, false)
        );
    }

    /// `--concurrency abc` used to fall back to 6 in silence; it is a usage error now. `0` still
    /// parses and is lifted to 1 in `main`.
    #[test]
    fn a_malformed_concurrency_is_an_error_not_the_default() {
        let e = parsed(&["--from", "a", "--to", "b", "--concurrency", "abc"])
            .expect_err("`abc` is not a number");
        assert_eq!(e.code, 2);
        assert!(e.text.contains("--concurrency"), "{}", e.text);
        let zero = parsed(&["--from", "a", "--to", "b", "--concurrency", "0"]).unwrap();
        assert_eq!(zero.concurrency, 0, "0 parses; main floors it at 1");
    }

    #[test]
    fn the_range_is_required_and_a_typo_is_refused() {
        assert_eq!(parsed(&["--from", "a"]).expect_err("--to is required").code, 2);
        let e = parsed(&["--from", "a", "--to", "b", "--dryrun"]).expect_err("a typo");
        assert!(e.text.contains("--dryrun"), "{}", e.text);
    }
}

/// **The declared credential SCOPE, exercised through this binary's own resolver.**
///
/// Owner ruling 2026-09-16: a process materialises only what it asked for. The hazard that
/// introduces is that a name this binary forgot to DECLARE would otherwise look exactly like a name
/// that is not in the store — and in this workspace an absent credential is the LIVE GATE, so a
/// forgotten declaration would degrade in silence instead of failing.
///
/// Both tests call the real `api_key`, so neither restates the key name: the name under test is
/// whatever that function actually asks for.
#[cfg(test)]
mod scope_tests {
    use super::*;
    use vike_secrets::{KeyScope, ScopedSecrets, Source};

    /// ⚠ **The mutation target for "a migrated caller's declared set omits a name it needs".**
    ///
    /// Dropping the name from `SCOPE` makes this red, naming the scope — instead of the binary
    /// quietly reporting the credential missing on a box where it IS configured.
    #[test]
    fn the_declared_scope_covers_the_name_this_binary_reads() {
        let scoped = ScopedSecrets::empty(&KeyScope::of(SCOPE), Source::None);
        match api_key(&scoped) {
            Ok(_) => {}
            Err(undeclared) => panic!(
                "this binary reads a credential its own SCOPE does not declare: {undeclared}"
            ),
        }
    }

    /// ...and the other direction, so the test above is not vacuous: an UNDECLARED name really is a
    /// distinct answer from an absent one, at this call site and not merely inside the store crate.
    #[test]
    fn a_name_outside_the_scope_refuses_rather_than_reading_as_absent() {
        // ⚠ A name with no `vike_model::scan` prefix, deliberately: an env-SHAPED literal here would be
        // a sighting in a `src/bin` file, which that scanner scores `Layer::Binary` (a `#[cfg(test)]`
        // module under `src/bin` is NOT test-classified), and would demand a registry row asserting
        // a read this binary does not make. MEASURED — the first spelling used `VIKE_SETTINGS_DIR`
        // and reddened `declared_layer_matches_the_path` in five bins at once.
        let foreign =
            ScopedSecrets::empty(&KeyScope::of(["A_NAME_THIS_BINARY_DOES_NOT_READ"]), Source::None);
        assert!(
            api_key(&foreign).is_err(),
            "a scope that declares something else must REFUSE, never answer `no credential` — \
             absent credentials are the live gate and this is not that"
        );
    }
}
