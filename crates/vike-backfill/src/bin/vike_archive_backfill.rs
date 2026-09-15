//! `data.vike.io` Polymarket L2 archive backfill CLI.
//!
//! Data source: `data.vike.io/archive` — full L2 (`book_events`), taker tape (`trades`), and
//! derived top-of-book (`l1_quotes`), one UTC day per Hive-style Parquet partition. Auth is an
//! `X-API-Key` header; key from the credential store (`VIKE_ARCHIVE_API_KEY` in
//! `<project>/settings/secrets.env`), read HERE (the bin) and passed into the
//! library as a plain parameter — `vike_backfill::vike_archive` itself never touches the
//! environment.
//!
//! Usage:
//!   vike_archive_backfill --from YYYY-MM-DD --to YYYY-MM-DD [--kind book|trade|quote|all]
//!       [--tokens T1,T2,...] [--tokens-file PATH] [--store DIR] [--base URL] [--dry-run]
//!       [--asset NAME --tenor NAME] [--discovery-base URL]
//!       [--bulk [--bulk-max-batches N] [--bulk-max-rows N] [--bulk-max-bytes N]]
//!       [--file PATH [--jobs N]]
//!
//! **`--asset`/`--tenor`** (e.g. `--asset btc --tenor 5m`) switch from the flat layout to the
//! FAMILY-partitioned one (`venue=polymarket/asset=<asset>/tenor=<tenor>/date=.../{stream}.parquet`
//! — one asset+tenor per file, dramatically fewer distinct `token_id`s per row group than the flat
//! superset: ~12-21 vs ~450, which is what makes an already-narrow family day's ingest cost minutes
//! rather than hours). Both flags must be given together — a lone `--asset` or `--tenor` is a
//! parse-time error, never a silent guess. The concrete file URL is resolved via the `/v1/`
//! discovery API (`GET .../v1/archive/datasets` / `GET .../v1/archive/url` — see
//! `vike_archive`'s module doc for the full design rationale: discovery is preferred over
//! constructing the path locally, because the server is the single source of truth for the
//! layout and this client should never have to re-derive it). `--discovery-base` overrides the
//! discovery API's root (default: `--base` with its trailing `/archive` stripped, i.e.
//! `https://data.vike.io` for the default `--base`). A discovery-call failure (network hiccup, not
//! a genuine "no such dataset") falls back to the directly-constructed family path rather than
//! failing the whole run. Omitting both flags is the pre-existing flat behavior, byte-for-byte
//! unchanged — zero discovery calls happen in that case.
//!
//! `--kind` (default `all`) selects which of the three streams to ingest per day. `--tokens`/
//! `--tokens-file` union into one filter set exactly like `pmxt_backfill`'s flags (comma list +
//! one-id-per-line file; neither given means no filter, i.e. every token in every day's file).
//! With a filter given, ingest first prunes Parquet ROW GROUPS by the `token_id` column's own
//! min/max statistics (`vike_archive::select_row_groups`) before ever reading row data — a
//! single-token day costs roughly one row group, not the whole multi-GB file.
//!
//! `--dry-run` fetches the open `manifest.json` and reports, for each requested date/stream: the
//! manifest's advertised byte size (no auth needed, no per-file network). When BOTH a token filter
//! AND `VIKE_ARCHIVE_API_KEY` are present, it ALSO opens each requested file's footer over a real
//! ranged-HTTP round trip (~1.17 MB, never the row data) to report real row-group pruning counts —
//! "how many row groups would actually be read vs the full file". Nothing is ingested; the store is
//! never opened.
//!
//! **Memory:** ingest (`vike_archive::ArchiveClient::ingest_stream`) is strictly SEQUENTIAL and
//! streams one Parquet row group at a time — fetch, decode, write to the store, drop, then the
//! next — so resident memory stays bounded by one row group's decoded rows (tens of MB) regardless
//! of how many days/tokens/row groups the whole run covers. No concurrent fetch/decode overlap
//! (no `--max-inflight`-style knob) — the earlier whole-file-buffered version proved that decode +
//! accumulate, not network throughput, dominated wall-clock, so overlap would trade a memory bug
//! for concurrency complexity without addressing the actual bottleneck. Progress (row group
//! index/total, bytes, rows this group, cumulative rows, elapsed) logs at INFO periodically via
//! `tracing` — see `vike_archive::ingest_stream_over`'s doc for the full design and why the
//! idempotency commit key is now scoped per row group rather than per date.
//!
//! **`--bulk`** switches the write path from the SAFE (live, WAL+fsync-per-commit) profile to the
//! opt-in bulk/offline profile (`vike_data::BulkIngestSession`, see its module doc): rows are
//! staged across many row groups and committed once per series per window instead of once per
//! series per row group — the fix for the measured ~450-distinct-token/row-group fixed-commit-cost
//! blowup on a flat archive day (`.superpowers/sdd/2026-07-28-poly-mm-latency-batch/
//! ingest-profile-report.md`). Off by default — a plain run is byte-for-byte the SAFE live path.
//! `--bulk-max-batches`/`--bulk-max-rows`/`--bulk-max-bytes` override the window (defaults:
//! `vike_data::BulkConfig::default()` — 20 row groups / 2M rows / 256 MB estimated, whichever
//! comes first); ignored without `--bulk`. Crash safety: see `vike_data::bulk`'s module doc — a
//! `--bulk` run interrupted mid-flight is safely re-runnable (idempotent commit keys mean it
//! resumes rather than duplicates), but a window that was mid-flush when it died is simply ABSENT
//! until the run is repeated, not partially visible.
//!
//! **`--file PATH`** ingests an already-downloaded local Parquet file instead of range-fetching
//! the same (date, stream) over HTTP — the measured fix for the archive's actual bottleneck: a
//! `book_events` day costs ~419 row groups × one HTTP round trip each through Cloudflare (hundreds
//! of seconds of pure network latency, NOT compute — see
//! `.superpowers/sdd/2026-07-28-poly-mm-latency-batch/local-file-ingest-report.md`), while curling
//! the whole file once and importing it locally pays that latency exactly ONCE. `--file` feeds the
//! IDENTICAL row-group-at-a-time decode/map/commit pipeline the URL path uses
//! (`vike_archive::ingest_stream_over` — see its doc); only the `ChunkReader` differs
//! (`vike_archive::LocalFileReader`, a local `seek`+`read` over `Arc<std::fs::File>`, vs
//! `HttpRangeReader`'s ranged GETs), so a local-file run and a URL run of the same (date, stream)
//! commit byte-identical rows under the same idempotent `vikearchive:{kind}:{date}:rg{idx}` keys.
//!
//! `--file` requires an UNAMBIGUOUS single target rather than guessing one from the file name: one
//! local Parquet file is one stream for one date, so `--from`/`--to` must name exactly ONE day
//! (`--from == --to`) and `--kind` must be an explicit `book`/`trade`/`quote` (never the default
//! `all` three-stream expansion) — both are REQUIRED alongside `--file`, and a multi-day range or
//! an implicit/`all` `--kind` is rejected with a clear error rather than silently picking one date
//! or guessing the stream from the file's name. (The file `--file` was built to measure,
//! `btc5m_2026-07-28.parquet`, doesn't match the archive's own `{stream}.parquet` naming at all —
//! filename inference would either fail outright or, worse, guess wrong on a file named
//! plausibly-but-incorrectly, so this design never attempts it.) `--tokens`/`--tokens-file` and
//! `--bulk` (+ its `--bulk-max-*` overrides) apply identically to `--file` as to the URL path;
//! `--base` is ignored (no network call is made) and `--dry-run` is incompatible with `--file`
//! (the file is already local — there is nothing to plan).
//!
//! **`--file` and `--asset`/`--tenor` are MUTUALLY EXCLUSIVE** (a hard, clearly-messaged parse
//! error, not a silently-preferred winner — `validate_file_family_exclusion`). The family pair's
//! ONE job is deciding which remote URL to fetch — discovery first, the constructed
//! `venue=.../asset=.../tenor=.../date=...` path as a fallback — and `--file` makes no network
//! call at all, so under `--file` the pair steers nothing. It cannot even contribute store
//! partition keys, because none of the ingest's keys come from the CLI: every row lands under
//! `venue=polymarket` (`vike_archive::VENUE`, a constant) at `symbol=<token_id>` read PER ROW out
//! of the Parquet itself, and the idempotency key is `vikearchive:{kind}:{date}:rg{idx}` — asset
//! and tenor appear in NONE of them. A family day and a flat day ingest into exactly the same
//! series; the family layout is a cheaper way to reach the same rows, not a different destination.
//! So the accepted-and-ignored alternative would read like it narrowed the ingest while doing
//! nothing — the same silent-guess failure a lone `--asset` (`resolve_family_args`) and a
//! filename-inferred stream (`validate_file_target`) are both rejected to avoid. The supported
//! shapes are therefore: `--asset btc --tenor 5m` to have the tool RESOLVE and range-fetch the
//! family file, or download that same file yourself and `--file` it (`--tokens`/`--kind`/`--bulk`/
//! `--jobs` all behave identically either way). `--discovery-base` needs the family pair to mean
//! anything, so it is likewise inert under `--file` — same as `--base`, and not separately
//! rejected.
//!
//! **`--jobs N`** (`--file` runs, with or without `--bulk`) fans the local
//! ingest's row-group loop across `N` worker threads via
//! `vike_archive::ingest_local_file_parallel` instead of the single-threaded
//! `ingest_local_file` — the measured fix for the `--file` path's remaining bottleneck: once
//! network latency was solved, decode+encode CPU is one core's worth of serial work (measured:
//! 1m44.8s / 98% CPU of 32 / 500 MB peak RSS for one BTC-5m `book_events` day). Defaults to
//! `min(4, available_parallelism)` when omitted or given as `0`/unparsable — pass `--jobs 1`
//! for the old strictly-serial behavior (byte-identical to a build predating this flag). Memory
//! scales with `--jobs` (each worker holds at most one row group's decoded rows resident at a
//! time, so peak RSS is roughly `jobs ×` the serial figure, not the whole file) — see
//! `.superpowers/sdd/2026-07-28-poly-mm-latency-batch/ingest-parallel-report.md` for the measured
//! scaling table and where it plateaus.
//!
//! **`--bulk` and `--jobs` COMPOSE.** They previously could not: `BulkIngestSession` takes
//! `&mut self`, so a `--bulk --file --jobs N` run logged a warning and ingested serially — you
//! could have the bulk profile's -42% write cost OR the fan-out's 3.4x, never both. `--jobs N > 1`
//! with `--bulk` now runs `vike_archive::ingest_local_file_bulk_parallel`: one
//! `BulkIngestSession` PER WORKER, each under its own commit-key namespace. That namespacing is
//! load-bearing rather than cosmetic — a session stamps keys `{prefix}:w{n}` from its OWN counter
//! starting at 0, so N sessions sharing a prefix would mint the SAME key for DIFFERENT rows, and
//! the store treats a seen key as a durable no-op: the second worker to reach a given series would
//! have its rows silently DROPPED. Each worker flushes its own trailing window.
//!
//! ⚠ **A resumed `--bulk` run must repeat the SAME `--jobs`.** Bulk idempotency is by commit key,
//! and the keys embed the chunk index, which is a function of `(selected row groups, jobs)`.
//! Re-running a crashed `--jobs 4` import as `--jobs 8` re-chunks, mints different keys, and
//! re-writes rows already committed — DUPLICATES, not a no-op, since this store never dedups by
//! row value. This extends the constraint `--bulk` already carried (the same `--bulk-max-*`
//! window, since flush boundaries decide which rows share a key) rather than adding a new class of
//! hazard, but it is one more flag that must match on a retry.

use std::collections::{BTreeSet, HashSet};
use std::path::Path;
use std::process::ExitCode;

use vike_backfill::cli::{CliSpec, arg, has_flag, log_config, store_root};
use vike_backfill::vike_archive::{
    ArchiveClient, DEFAULT_BASE, FamilyTarget, Stream, VENUE, default_discovery_base,
    ingest_local_file_bulk, ingest_local_file_bulk_parallel, ingest_local_file_parallel,
};
use vike_bridge_core::credentials::load_workspace_secrets_from_env;
use vike_data::{BulkConfig, DataError, DataFusionHist};
use vike_model::time::days_in_range;

/// `--asset NAME --tenor NAME` select the FAMILY-partitioned layout
/// (`venue=polymarket/asset=<asset>/tenor=<tenor>/date=.../{stream}.parquet`) instead of today's
/// flat one; both must be given together (the server itself rejects a partial pair as ambiguous —
/// this parses that same rule client-side so a typo'd single flag fails fast with a clear message
/// rather than reaching the network first). Absent (both `None`) is the pre-existing flat
/// behavior, byte-for-byte — this function returning `Ok(None)` never changes anything downstream.
fn resolve_family_args(args: &[String]) -> Result<Option<(String, String)>, String> {
    match (arg(args, "--asset"), arg(args, "--tenor")) {
        (Some(a), Some(t)) => Ok(Some((a, t))),
        (None, None) => Ok(None),
        (Some(_), None) => Err("--asset given without --tenor (both or neither)".to_string()),
        (None, Some(_)) => Err("--tenor given without --asset (both or neither)".to_string()),
    }
}

/// Parse `--bulk-max-batches`/`--bulk-max-rows`/`--bulk-max-bytes` overrides onto
/// [`BulkConfig::default`]; an absent or unparsable flag keeps that field's default (never fails
/// the run over a typo'd override — the safe fallback is the well-tested default window).
fn bulk_config_from_args(args: &[String]) -> BulkConfig {
    let mut cfg = BulkConfig::default();
    if let Some(n) = arg(args, "--bulk-max-batches").and_then(|s| s.parse().ok()) {
        cfg.max_batches = n;
    }
    if let Some(n) = arg(args, "--bulk-max-rows").and_then(|s| s.parse().ok()) {
        cfg.max_rows = n;
    }
    if let Some(n) = arg(args, "--bulk-max-bytes").and_then(|s| s.parse().ok()) {
        cfg.max_bytes = n;
    }
    cfg
}

/// Resolve `--jobs N` — the `--file` local-ingest row-group fan-out width. An explicit positive
/// integer wins; anything else (absent, `0`, unparsable) falls back to
/// `min(4, available_cores)` (floored at 1, so a single-core box still gets one worker). Kept
/// pure/testable by taking the core count as a parameter — the one real call site passes
/// `std::thread::available_parallelism()`, per the repo's "libraries take config as parameters"
/// rule (this is a bin-local helper, not a library fn, but there's no reason to make it any less
/// testable than one).
///
/// WHY 4 AND NOT MORE (measured 2026-07-29 on the CI box, 32 cores, one BTC-5m day = 74,420,802 rows):
///
/// | jobs | wall     | %CPU  | peak RSS |
/// |------|----------|-------|----------|
/// | 1    | 1m49.5s  |   98% |   533 MB |
/// | 4    |   31.8s  |  388% |  1.57 GB |
/// | 8    |   32.4s  |  729% |  2.89 GB |
/// | 16   |   29.3s  | 1395% |  5.66 GB |
///
/// Essentially the entire ~3.4x speedup is captured at 4; 8 and 16 buy ~2s (inside the noise of a
/// shared box whose load swung 8->120 during the run) while costing 2-3.6x the memory. The plateau
/// is NOT write-lock contention (threads measured runnable, not blocked): `DataFusionHist` spawns
/// its own 32-worker tokio runtime for the writes regardless of this flag, so `--jobs` only fans
/// out the DECODE, which stops being the constraint past ~4. Raise it only on a quiet dedicated
/// box AND only with a measurement in hand — the memory cost is certain, the speedup is not.
fn resolve_jobs(raw: Option<&str>, available_cores: usize) -> usize {
    match raw.and_then(|s| s.parse::<usize>().ok()) {
        Some(n) if n > 0 => n,
        _ => available_cores.clamp(1, 4),
    }
}

/// `YYYY-MM-DD` labels for every UTC day in `[from, to]` inclusive; empty on an unparsable bound or
/// `from` after `to` (mirrors `vike_model::time::hours_in_range`'s empty-on-bad-input contract).
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

/// `--gaps`: take the days to fetch from the STORE's own coverage report instead of a typed range.
///
/// This is the difference between a gap-FILL and a re-download. A `--from/--to` range re-fetches
/// every day in it, including the ones already recorded — and the store accepts them, because a
/// different writer means a different commit key and it never dedups by row VALUE. The result is
/// both copies in the series and a backtest quietly seeing double the volume. Fetching only the
/// missing days means there is no overlap to resolve at all. (The store's persisted source policy
/// can resolve one after the fact; not creating it is strictly better.)
///
/// Returns the UTC-day labels this source can close, plus the kinds it can NEVER serve. An empty day
/// list with a NON-empty `unavailable` is not completeness, and `main` says so rather than printing
/// "nothing to do".
fn gap_day_labels(store: &DataFusionHist) -> Result<(Vec<String>, Vec<&'static str>), DataError> {
    use vike_backfill::caps::Source;
    use vike_backfill::gapfill::plan_fill;

    let mut days: BTreeSet<i64> = BTreeSet::new();
    let mut unavailable: BTreeSet<&'static str> = BTreeSet::new();
    for cov in store.coverage_report()?.iter().filter(|c| c.key.venue == VENUE) {
        let plan = plan_fill(cov, Source::Archive);
        days.extend(plan.days.keys().copied());
        unavailable.extend(plan.unavailable.iter().copied());
    }
    let labels =
        days.into_iter().map(|d| vike_model::time::epoch_ms_to_utc_date(d * 86_400_000)).collect();
    Ok((labels, unavailable.into_iter().collect()))
}

/// Parse a `--kind` value (`book`/`trade`/`quote`/`all`) into the streams to run.
fn kinds_from_arg(raw: Option<&str>) -> Result<Vec<Stream>, String> {
    match raw {
        None | Some("all") => Ok(Stream::all().to_vec()),
        Some(k) => Stream::from_kind(k)
            .map(|s| vec![s])
            .ok_or_else(|| format!("unknown --kind '{k}': want book|trade|quote|all")),
    }
}

/// Load token ids from `path`: one id per line, whitespace trimmed, blank lines and `#`-comment
/// lines skipped — the same file shape `pmxt_backfill`'s `--tokens-file` uses.
fn load_tokens_file(path: &Path) -> std::io::Result<HashSet<String>> {
    let contents = std::fs::read_to_string(path)?;
    Ok(contents
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect())
}

/// `--tokens` (comma list) and `--tokens-file` union into ONE filter set; both absent means no
/// filter (`None`) — the pmxt idiom.
fn resolve_tokens(args: &[String]) -> Result<Option<HashSet<String>>, String> {
    let inline = arg(args, "--tokens");
    let tokens_file = arg(args, "--tokens-file");
    if inline.is_none() && tokens_file.is_none() {
        return Ok(None);
    }
    let mut set: HashSet<String> = HashSet::new();
    if let Some(s) = inline {
        set.extend(s.split(',').map(str::trim).filter(|t| !t.is_empty()).map(str::to_string));
    }
    if let Some(path) = tokens_file {
        let ids = load_tokens_file(Path::new(&path))
            .map_err(|e| format!("failed to read --tokens-file {path}: {e}"))?;
        set.extend(ids);
    }
    Ok(Some(set))
}

/// `--file` needs an unambiguous single (date, stream) target — see the module doc's `--file`
/// section for why this is required rather than inferred from the file name. `dates`/`kinds` are
/// the ALREADY-parsed `--from`/`--to`/`--kind` values (so this reuses `kinds_from_arg`'s own
/// `all`-expansion rather than re-parsing the raw string): exactly one day and exactly one
/// explicit stream. A no-op (`Ok(())`) when `--file` wasn't given at all.
fn validate_file_target(
    file_given: bool,
    dry_run: bool,
    dates: &[String],
    kinds: &[Stream],
) -> Result<(), String> {
    if !file_given {
        return Ok(());
    }
    if dry_run {
        return Err(
            "--file and --dry-run are mutually exclusive: the file is already local, there is \
             nothing to plan"
                .to_string(),
        );
    }
    if dates.len() != 1 {
        return Err(format!(
            "--file requires exactly one day (--from == --to); got a {}-day range \
             [{from}, {to}] — one local Parquet file is one date, not a range",
            dates.len(),
            from = dates.first().map(String::as_str).unwrap_or("?"),
            to = dates.last().map(String::as_str).unwrap_or("?"),
        ));
    }
    if kinds.len() != 1 {
        return Err(
            "--file requires an explicit --kind book|trade|quote (not the default 'all') — one \
             local Parquet file is one stream, not the archive's three-stream set"
                .to_string(),
        );
    }
    Ok(())
}

/// `--file` and `--asset`/`--tenor` are MUTUALLY EXCLUSIVE — see the module doc's "`--file` and
/// `--asset`/`--tenor`" section for the full reasoning. The short version: the family pair's ONLY
/// job is choosing which remote URL to fetch (discovery, or the constructed family path as a
/// fallback), and `--file` makes no network call at all, so under `--file` the pair would steer
/// nothing. It cannot even contribute store partition keys, because there are none to contribute:
/// every ingested row lands under `venue=polymarket` (`vike_archive::VENUE`, a constant) at
/// `symbol=<token_id>` read PER ROW out of the file itself. Accepting the combination would
/// therefore be a silent no-op that reads like it narrowed the ingest — exactly the "never a silent
/// guess" failure this bin rejects elsewhere (`resolve_family_args`' lone-flag error,
/// `validate_file_target`'s refusal to infer a stream from a file name). `family` is the
/// ALREADY-parsed [`resolve_family_args`] result, so a lone `--asset`/`--tenor` has already failed
/// before this runs. A no-op (`Ok(())`) unless BOTH were given.
fn validate_file_family_exclusion(
    file_given: bool,
    family: Option<&(String, String)>,
) -> Result<(), String> {
    match (file_given, family) {
        (true, Some((asset, tenor))) => Err(format!(
            "--file and --asset/--tenor are mutually exclusive: --file names the local Parquet \
             file outright, so there is no URL left for the family target (--asset {asset} \
             --tenor {tenor}) to resolve, and asset/tenor are not store partition keys (rows land \
             under venue=polymarket at symbol=<token_id> read from the file itself) — drop \
             --asset/--tenor, or drop --file to fetch the family file over HTTP"
        )),
        _ => Ok(()),
    }
}

const USAGE: &str = "\
usage: vike_archive_backfill (--from YYYY-MM-DD --to YYYY-MM-DD | --gaps)
                             [--kind book|trade|quote|all] [--tokens T1,T2,...] [--tokens-file PATH]
                             [--store DIR] [--base URL] [--dry-run]
                             [--asset NAME --tenor NAME] [--discovery-base URL]
                             [--bulk [--bulk-max-batches N] [--bulk-max-rows N] [--bulk-max-bytes N]]
                             [--file PATH [--jobs N]]

Backfill the data.vike.io archive (Polymarket L2 book_events/trades/l1_quotes, date-partitioned
Parquet) into the hist store. A ranged-HTTP ChunkReader fetches only the footer and the
row-group-pruned column chunks of each file, never the whole multi-GB day.

  --from DATE          first UTC day, YYYY-MM-DD (inclusive)
  --to DATE            last UTC day, YYYY-MM-DD (inclusive)
  --gaps               take the day list from the STORE own coverage report instead — a gap-FILL
                       rather than a re-download. Mutually exclusive with --from/--to.
  --kind K             book | trade | quote | all (default all)
  --tokens T1,T2       narrow to these token ids (comma list)
  --tokens-file PATH   the same, one id per line
  --store DIR          hist-store root (default: $VIKE_HIST_STORE, else <repo>/market_data/hist)
  --base URL           archive root override
  --dry-run            print the plan and exit without fetching or writing
  --asset NAME         select the family-partitioned layout (e.g. --asset btc --tenor 5m) instead
  --tenor NAME         of the flat one; BOTH must be given together. The concrete URL is resolved
                       through the /v1/archive/datasets|url discovery API.
  --discovery-base URL override the discovery API root (default derived from --base)
  --bulk               bulk-batch mode, with three bounds:
  --bulk-max-batches N   stop after N batches
  --bulk-max-rows N      per-batch row bound
  --bulk-max-bytes N     per-batch byte bound
  --file PATH          ingest an ALREADY-LOCAL Parquet file and make NO network call. Requires
                       exactly one day (--from == --to) and an explicit --kind; mutually exclusive
                       with --dry-run and with --asset/--tenor.
  --jobs N             fan the --file row-group decode across N threads (default min(4, cores);
                       no effect under --bulk)
  -h, --help           print this and exit 0
  -V, --version        print the version and exit 0

environment:
  VIKE_ARCHIVE_API_KEY   the X-API-Key credential, from the credential store
  VIKE_HIST_STORE        hist-store root when --store is absent";

const SPEC: CliSpec = CliSpec {
    bin: "vike_archive_backfill",
    usage: USAGE,
    valued: &[
        "--from",
        "--to",
        "--kind",
        "--tokens",
        "--tokens-file",
        "--store",
        "--base",
        "--asset",
        "--tenor",
        "--discovery-base",
        "--bulk-max-batches",
        "--bulk-max-rows",
        "--bulk-max-bytes",
        "--file",
        "--jobs",
    ],
    toggles: &["--gaps", "--dry-run", "--bulk"],
    positionals: 0,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = SPEC.short_circuit(&args) {
        return code;
    }
    let _guards = vike_log::init(log_config("vike-archive-backfill"));
    // ONE process-environment sweep, at the root, hoisted above every branch that needs it: the
    // hist-store root (resolved in two places below) and the credential chain's two inputs — the
    // settings-directory override comes out of this map.
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();

    // `--gaps` replaces --from/--to with the days the STORE says are missing — a gap-FILL rather
    // than a re-download. Resolved before the usage check so `--gaps` alone is a complete invocation.
    let gaps_mode = args.iter().any(|a| a == "--gaps");
    if gaps_mode && (arg(&args, "--from").is_some() || arg(&args, "--to").is_some()) {
        eprintln!("--gaps takes the day list from the store's coverage report; drop --from/--to");
        return ExitCode::FAILURE;
    }

    let (Some(from), Some(to)) = (
        arg(&args, "--from").or_else(|| gaps_mode.then(String::new)),
        arg(&args, "--to").or_else(|| gaps_mode.then(String::new)),
    ) else {
        eprintln!(
            "vike_archive_backfill: either --from and --to, or --gaps, is required\n\n{USAGE}"
        );
        return ExitCode::from(2);
    };
    let dates = if gaps_mode {
        let root = store_root(arg(&args, "--store").as_deref(), &env);
        let store = match DataFusionHist::open(&root) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("--gaps: opening store {}: {e}", root.display());
                return ExitCode::FAILURE;
            }
        };
        match gap_day_labels(&store) {
            Ok((days, unavailable)) => {
                if days.is_empty() {
                    // An empty plan is NOT completeness when a kind is unservable — say which.
                    if unavailable.is_empty() {
                        println!("--gaps: no gaps the archive can fill; nothing to do");
                    } else {
                        println!(
                            "--gaps: no gaps the archive can fill, but it serves NONE of [{}] for \
                             this venue — those holes need a different source",
                            unavailable.join(", ")
                        );
                    }
                    return ExitCode::SUCCESS;
                }
                if !unavailable.is_empty() {
                    tracing::warn!(
                        unavailable = ?unavailable,
                        "--gaps: the archive cannot serve these kinds — their gaps stay open"
                    );
                }
                println!("--gaps: {} day(s) missing: {}", days.len(), days.join(", "));
                days
            }
            Err(e) => {
                eprintln!("--gaps: reading coverage: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        day_labels(&from, &to)
    };
    if dates.is_empty() {
        eprintln!("bad --from/--to (want YYYY-MM-DD) or --from after --to: {from}..{to}");
        return ExitCode::FAILURE;
    }
    let kinds = match kinds_from_arg(arg(&args, "--kind").as_deref()) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let tokens = match resolve_tokens(&args) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    if tokens.is_none() {
        tracing::warn!(
            "no --tokens/--tokens-file filter given: ingesting EVERY token in every day's \
             file — this is a lot of volume (book_events runs ~7.4 GB/day); consider \
             --tokens T1,T2,... or --tokens-file PATH"
        );
    }
    let base = arg(&args, "--base").unwrap_or_else(|| DEFAULT_BASE.to_string());
    let dry_run = has_flag(&args, "--dry-run");
    let family = match resolve_family_args(&args) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let discovery_base =
        arg(&args, "--discovery-base").unwrap_or_else(|| default_discovery_base(&base));
    let file_arg = arg(&args, "--file");

    if let Err(e) = validate_file_target(file_arg.is_some(), dry_run, &dates, &kinds) {
        eprintln!("{e}");
        return ExitCode::FAILURE;
    }
    if let Err(e) = validate_file_family_exclusion(file_arg.is_some(), family.as_ref()) {
        eprintln!("{e}");
        return ExitCode::FAILURE;
    }

    // The bin is the ONLY place that reads the environment or a credential store — vike_archive's
    // library API takes the key as a plain parameter (see the module doc's "libraries take config
    // as parameters" rule). The store is
    // `<repo>/.env`, resolved from the `env` sweep taken once at the top of `main`.
    let creds = load_workspace_secrets_from_env(&env);
    let api_key = creds.get("VIKE_ARCHIVE_API_KEY").cloned();
    if api_key.is_none() && file_arg.is_none() {
        tracing::warn!(
            "VIKE_ARCHIVE_API_KEY not set (credential store) — manifest.json/samples still work \
             (open, keyless), but every date-partitioned file will 401"
        );
    }
    let client = ArchiveClient::new(base.clone(), api_key.clone());

    tracing::info!(
        "vike-archive-backfill: {} day(s) [{from}, {to}], kinds={:?}, base={base}, dry_run={dry_run}",
        dates.len(),
        kinds.iter().map(|s| s.kind()).collect::<Vec<_>>(),
    );

    if dry_run {
        if let Some((asset, tenor)) = &family {
            // Family dry-run: resolve each (date, stream) through the SAME discovery call the
            // real ingest path uses (never a hand-built path) — the dry run previews exactly what
            // would be fetched, not a re-derived guess.
            for date in &dates {
                for stream in &kinds {
                    match client.resolve_dataset(
                        &discovery_base,
                        date,
                        *stream,
                        Some(asset),
                        Some(tenor),
                    ) {
                        Ok(d) => println!(
                            "dry-run: {date}/{} (asset={asset} tenor={tenor}) -> {} \
                             ({} bytes, {} rows, layout={})",
                            stream.file_name(),
                            d.url,
                            d.bytes,
                            d.rows.map(|r| r.to_string()).unwrap_or_else(|| "unknown".to_string()),
                            d.layout
                        ),
                        Err(e) => println!(
                            "dry-run: {date}/{} (asset={asset} tenor={tenor}) — discovery \
                             resolve failed: {e}",
                            stream.file_name()
                        ),
                    }
                }
            }
            println!("dry-run: nothing downloaded, nothing written");
            return ExitCode::SUCCESS;
        }

        let manifest = match client.fetch_manifest() {
            Ok(m) => m,
            Err(e) => {
                eprintln!("dry-run: fetch manifest.json failed: {e}");
                return ExitCode::FAILURE;
            }
        };
        for date in &dates {
            let entry = manifest.dates.iter().find(|d| &d.date == date);
            for stream in &kinds {
                let advertised = entry
                    .and_then(|e| e.streams.get(stream.file_name()))
                    .map(|s| s.bytes.to_string())
                    .unwrap_or_else(|| "not in manifest".to_string());
                print!(
                    "dry-run: {date}/{} -> {} (manifest bytes: {advertised})",
                    stream.file_name(),
                    stream_url_for_log(&base, date, *stream),
                );
                // A real row-group-pruning number needs a per-file footer round trip, which needs
                // the key AND a token filter to be worth doing (unfiltered = every row group
                // anyway). Never fails the whole dry run — one date/stream's plan failing (e.g. the
                // date genuinely has no file yet) is reported inline, not fatal.
                match (&tokens, &api_key) {
                    (Some(t), Some(_)) => match client.plan_stream(date, *stream, Some(t)) {
                        Ok(plan) => println!(
                            " — row groups {}/{} ({} / {} compressed bytes)",
                            plan.selected_row_groups,
                            plan.total_row_groups,
                            plan.selected_compressed_bytes,
                            plan.total_compressed_bytes
                        ),
                        Err(e) => println!(" — row-group plan unavailable: {e}"),
                    },
                    (Some(_), None) => {
                        println!(" — row-group plan skipped: VIKE_ARCHIVE_API_KEY not set")
                    }
                    (None, _) => println!(" — row-group plan skipped: no --tokens filter given"),
                }
            }
        }
        println!("dry-run: nothing downloaded, nothing written");
        return ExitCode::SUCCESS;
    }

    let root = store_root(arg(&args, "--store").as_deref(), &env);
    let store = match DataFusionHist::open(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("open hist store at {}: {e}", root.display());
            return ExitCode::FAILURE;
        }
    };

    let bulk = has_flag(&args, "--bulk");
    let jobs_arg = arg(&args, "--jobs");
    let jobs = resolve_jobs(
        jobs_arg.as_deref(),
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
    );

    // `--file`: ingest one already-local Parquet file instead of range-fetching over HTTP — see
    // the module doc's `--file` section. `validate_file_target` already proved (or this bin would
    // have exited above) that `dates`/`kinds` each hold EXACTLY one entry whenever `file_arg` is
    // `Some`, so indexing `[0]` here can't panic.
    if let Some(file_path) = &file_arg {
        let date = &dates[0];
        let stream = kinds[0];
        let path = Path::new(file_path);
        tracing::info!(
            %date, kind = stream.kind(), file = %path.display(), bulk, jobs,
            "vike-archive-backfill: --file local ingest starting"
        );
        let result: Result<usize, String> = if bulk {
            let bulk_cfg = bulk_config_from_args(&args);
            tracing::info!(
                "vike-archive-backfill: --bulk write profile (max_batches={} max_rows={} \
                 max_bytes={}) — see vike_data::bulk's module doc for the crash-safety semantics \
                 this trades for speed",
                bulk_cfg.max_batches,
                bulk_cfg.max_rows,
                bulk_cfg.max_bytes,
            );
            let key_prefix = format!("vikearchive:{}:{date}:bulk", stream.kind());
            if jobs > 1 {
                // Bulk + fan-out compose: one session PER WORKER, each under its own key namespace
                // (`{prefix}:j{chunk}`) — see `ingest_local_file_bulk_parallel`'s doc for why a
                // shared prefix would silently drop rows. Each worker flushes its own trailing
                // window, so there is no session left here to flush.
                ingest_local_file_bulk_parallel(
                    &store,
                    path,
                    date,
                    stream,
                    tokens.as_ref(),
                    &key_prefix,
                    jobs,
                    bulk_cfg,
                )
                .inspect(|&staged| {
                    tracing::info!(
                        %date, kind = stream.kind(), staged, jobs,
                        "bulk local-file ingested (parallel)"
                    );
                })
                .map_err(|e| format!("bulk parallel local-file ingest failed: {e}"))
            } else {
                let mut session = store.bulk_session(bulk_cfg);
                let stage_result = ingest_local_file_bulk(
                    &mut session,
                    path,
                    date,
                    stream,
                    tokens.as_ref(),
                    &key_prefix,
                );
                // ALWAYS flush the trailing partial window, even on a decode error partway through
                // — whatever already staged should still become durable (mirrors the multi-day
                // --bulk loop below).
                let flush_result = session.flush(&key_prefix);
                match (stage_result, flush_result) {
                    (Ok(staged), Ok(report)) => {
                        tracing::info!(
                            %date, kind = stream.kind(), staged, committed = report.rows_written,
                            series = report.series_flushed, "bulk local-file ingested"
                        );
                        Ok(report.rows_written)
                    }
                    (Ok(_), Err(e)) => Err(format!("bulk flush failed: {e}")),
                    (Err(e), _) => Err(format!("bulk local-file ingest failed: {e}")),
                }
            }
        } else {
            ingest_local_file_parallel(&store, path, date, stream, tokens.as_ref(), jobs)
                .map_err(|e| e.to_string())
        };
        return match result {
            Ok(n) => {
                tracing::info!(
                    %date, kind = stream.kind(), rows = n, store = %root.display(),
                    "vike-archive-backfill done (local file)"
                );
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("local-file ingest failed: {e}");
                ExitCode::FAILURE
            }
        };
    }

    let mut totals = [0usize; 3]; // book, trade, quote — indexed by Stream::all() order
    let (mut ok, mut failed) = (0usize, 0usize);

    if bulk {
        let bulk_cfg = bulk_config_from_args(&args);
        tracing::info!(
            "vike-archive-backfill: --bulk write profile (max_batches={} max_rows={} max_bytes={}) \
             — see vike_data::bulk's module doc for the crash-safety semantics this trades for speed",
            bulk_cfg.max_batches,
            bulk_cfg.max_rows,
            bulk_cfg.max_bytes,
        );
        let mut session = store.bulk_session(bulk_cfg);
        for date in &dates {
            for stream in &kinds {
                let key_prefix = format!("vikearchive:{}:{date}:bulk", stream.kind());
                let stage_result = match &family {
                    Some((asset, tenor)) => {
                        let target = FamilyTarget { discovery_base: &discovery_base, asset, tenor };
                        client.ingest_family_stream_bulk(
                            &mut session,
                            &target,
                            date,
                            *stream,
                            tokens.as_ref(),
                            &key_prefix,
                        )
                    }
                    None => client.ingest_stream_bulk(
                        &mut session,
                        date,
                        *stream,
                        tokens.as_ref(),
                        &key_prefix,
                    ),
                };
                // ALWAYS flush the trailing partial window for this stream, even on a decode/fetch
                // error partway through — whatever this stream already staged should still become
                // durable rather than silently discarded when the session is dropped.
                let flush_result = session.flush(&key_prefix);
                match (stage_result, flush_result) {
                    (Ok(staged), Ok(report)) => {
                        tracing::info!(
                            %date, kind = stream.kind(), staged, committed = report.rows_written,
                            series = report.series_flushed, "bulk ingested"
                        );
                        totals[stream_index(*stream)] += report.rows_written;
                        ok += 1;
                    }
                    (Ok(_), Err(e)) => {
                        tracing::error!(%date, kind = stream.kind(), "bulk flush failed: {e} — skipping");
                        failed += 1;
                    }
                    (Err(e), _) => {
                        tracing::error!(%date, kind = stream.kind(), "bulk ingest failed: {e} — skipping");
                        failed += 1;
                    }
                }
            }
        }
        tracing::info!(
            "vike-archive-backfill done (bulk): {ok} ok, {failed} failed — book={} trade={} \
             quote={} rows, {} total commits (into {})",
            totals[0],
            totals[1],
            totals[2],
            session.total_commits(),
            root.display()
        );
    } else {
        for date in &dates {
            for stream in &kinds {
                let result = match &family {
                    Some((asset, tenor)) => {
                        let target = FamilyTarget { discovery_base: &discovery_base, asset, tenor };
                        client.ingest_family_stream(&store, &target, date, *stream, tokens.as_ref())
                    }
                    None => client.ingest_stream(&store, date, *stream, tokens.as_ref()),
                };
                match result {
                    Ok(n) => {
                        tracing::info!(%date, kind = stream.kind(), rows = n, "ingested");
                        totals[stream_index(*stream)] += n;
                        ok += 1;
                    }
                    Err(e) => {
                        tracing::error!(%date, kind = stream.kind(), "ingest failed: {e} — skipping");
                        failed += 1;
                    }
                }
            }
        }
        tracing::info!(
            "vike-archive-backfill done: {ok} ok, {failed} failed — book={} trade={} quote={} rows \
             (into {})",
            totals[0],
            totals[1],
            totals[2],
            root.display()
        );
    }

    if failed > 0 && ok == 0 {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn stream_index(s: Stream) -> usize {
    match s {
        Stream::Book => 0,
        Stream::Trade => 1,
        Stream::Quote => 2,
    }
}

fn stream_url_for_log(base: &str, date: &str, stream: Stream) -> String {
    vike_backfill::vike_archive::stream_url(base, date, stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- --asset/--tenor (family-layout selection) --------------------------------------------

    #[test]
    fn resolve_family_args_none_when_neither_flag_given() {
        let args: Vec<String> =
            ["prog", "--from", "2026-01-01"].iter().map(|s| s.to_string()).collect();
        assert_eq!(resolve_family_args(&args).unwrap(), None);
    }

    #[test]
    fn resolve_family_args_pairs_both_flags() {
        let args: Vec<String> =
            ["prog", "--asset", "btc", "--tenor", "5m"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            resolve_family_args(&args).unwrap(),
            Some(("btc".to_string(), "5m".to_string()))
        );
    }

    #[test]
    fn resolve_family_args_rejects_a_lone_asset_or_tenor() {
        let asset_only: Vec<String> =
            ["prog", "--asset", "btc"].iter().map(|s| s.to_string()).collect();
        assert!(resolve_family_args(&asset_only).is_err());

        let tenor_only: Vec<String> =
            ["prog", "--tenor", "5m"].iter().map(|s| s.to_string()).collect();
        assert!(resolve_family_args(&tenor_only).is_err());
    }

    // ---- resolve_jobs: --jobs N default/clamp resolution -----------------------------------------

    #[test]
    fn resolve_jobs_uses_an_explicit_positive_value() {
        assert_eq!(resolve_jobs(Some("3"), 32), 3);
        assert_eq!(resolve_jobs(Some("16"), 4), 16, "explicit --jobs is never clamped by cores");
    }

    #[test]
    fn resolve_jobs_defaults_to_min_4_cores_when_absent_zero_or_unparsable() {
        assert_eq!(resolve_jobs(None, 32), 4, "min(4, 32) — the measured sweet spot");
        assert_eq!(resolve_jobs(None, 2), 2, "min(4, 2) — never more workers than cores");
        assert_eq!(resolve_jobs(Some("0"), 32), 4, "0 falls back to the default, not 0 workers");
        assert_eq!(resolve_jobs(Some("garbage"), 32), 4);
        assert_eq!(resolve_jobs(None, 0), 1, "never fewer than one worker");
    }

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
    fn kinds_from_arg_defaults_to_all_three() {
        assert_eq!(kinds_from_arg(None).unwrap(), Stream::all().to_vec());
        assert_eq!(kinds_from_arg(Some("all")).unwrap(), Stream::all().to_vec());
        assert_eq!(kinds_from_arg(Some("book")).unwrap(), vec![Stream::Book]);
        assert_eq!(kinds_from_arg(Some("trade")).unwrap(), vec![Stream::Trade]);
        assert_eq!(kinds_from_arg(Some("quote")).unwrap(), vec![Stream::Quote]);
        assert!(kinds_from_arg(Some("garbage")).is_err());
    }

    #[test]
    fn resolve_tokens_unions_inline_and_file() {
        let dir =
            std::env::temp_dir().join(format!("vike_archive_backfill_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tokens.txt");
        std::fs::write(&path, "# ids\nid_b\n\n  id_c\n").unwrap();

        let args: Vec<String> =
            ["prog", "--tokens", "id_a, id_b", "--tokens-file", path.to_str().unwrap()]
                .iter()
                .map(|s| s.to_string())
                .collect();
        let got = resolve_tokens(&args).unwrap().unwrap();
        assert_eq!(
            got,
            ["id_a", "id_b", "id_c"].iter().map(|s| s.to_string()).collect::<HashSet<_>>()
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_tokens_none_when_neither_flag_given() {
        let args: Vec<String> =
            ["prog", "--from", "2026-01-01"].iter().map(|s| s.to_string()).collect();
        assert_eq!(resolve_tokens(&args).unwrap(), None);
    }

    #[test]
    fn resolve_tokens_reports_a_missing_tokens_file() {
        let args: Vec<String> = ["prog", "--tokens-file", "/no/such/path/vike-archive-test"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(resolve_tokens(&args).is_err());
    }

    // ---- validate_file_target: --file's single-(date, stream)-target requirement --------------

    #[test]
    fn validate_file_target_is_a_no_op_when_file_not_given() {
        // Even a multi-day range / all-kinds / dry-run combination is fine when --file is absent
        // — this validation exists ONLY to constrain --file.
        assert!(
            validate_file_target(
                false,
                true,
                &["2026-07-27".to_string(), "2026-07-28".to_string()],
                &Stream::all(),
            )
            .is_ok()
        );
    }

    #[test]
    fn validate_file_target_accepts_one_day_and_one_explicit_kind() {
        assert!(
            validate_file_target(true, false, &["2026-07-28".to_string()], &[Stream::Book]).is_ok()
        );
    }

    #[test]
    fn validate_file_target_rejects_dry_run() {
        assert!(
            validate_file_target(true, true, &["2026-07-28".to_string()], &[Stream::Book]).is_err()
        );
    }

    #[test]
    fn validate_file_target_rejects_a_multi_day_range() {
        let dates = ["2026-07-27".to_string(), "2026-07-28".to_string()];
        assert!(validate_file_target(true, false, &dates, &[Stream::Book]).is_err());
    }

    #[test]
    fn validate_file_target_rejects_zero_days() {
        assert!(validate_file_target(true, false, &[], &[Stream::Book]).is_err());
    }

    #[test]
    fn validate_file_target_rejects_the_default_all_kind_expansion() {
        // kinds_from_arg(None) / kinds_from_arg(Some("all")) both expand to all three streams —
        // exactly the ambiguous case --file must reject.
        assert!(
            validate_file_target(true, false, &["2026-07-28".to_string()], &Stream::all()).is_err()
        );
    }

    // ---- validate_file_family_exclusion: --file × --asset/--tenor ------------------------------

    #[test]
    fn file_without_a_family_target_is_fine() {
        assert!(validate_file_family_exclusion(true, None).is_ok());
    }

    #[test]
    fn a_family_target_without_file_is_fine() {
        let family = ("btc".to_string(), "5m".to_string());
        assert!(
            validate_file_family_exclusion(false, Some(&family)).is_ok(),
            "--asset/--tenor over HTTP is the whole point of the family layout"
        );
    }

    #[test]
    fn file_plus_a_family_target_is_rejected_not_silently_ignored() {
        // The family pair only picks a URL, and --file makes no network call — so accepting the
        // combination would be a no-op that LOOKS like it narrowed the ingest.
        let family = ("btc".to_string(), "5m".to_string());
        let err = validate_file_family_exclusion(true, Some(&family)).unwrap_err();
        assert!(err.contains("--file"), "the message names both offending flags: {err}");
        assert!(err.contains("--asset"), "the message names both offending flags: {err}");
    }

    #[test]
    fn neither_file_nor_family_is_fine() {
        assert!(
            validate_file_family_exclusion(false, None).is_ok(),
            "the plain flat-layout URL run must stay unaffected"
        );
    }
}
