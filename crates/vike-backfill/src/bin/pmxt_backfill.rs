//! Runnable pmxt Polymarket-L2 order-book backfill into the DataFusion hist store, or (with
//! `--clickhouse`) into the `polymarket` ClickHouse tables the live L2 recorder writes.
//!
//! Data source: the [pmxt](https://archive.pmxt.dev) Polymarket order-book archive, licensed
//! [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). This tool downloads and ingests
//! Parquet files published by **pmxt** (`r2v2.pmxt.dev`); pmxt is not affiliated with this
//! project — attribution per the license terms.
//!
//! Usage:
//!   pmxt_backfill --from YYYY-MM-DDTHH --to YYYY-MM-DDTHH [--tokens T1,T2,...]
//!                 [--tokens-file PATH] [--store DIR] [--dry-run] [--rate-ms N]
//!                 [--clickhouse [--ch-bin BIN] [--min-local-ts MS] [--max-local-ts MS]]
//!
//! Iterates every UTC hour in `[--from, --to]` inclusive. For each hour: downloads that hour's
//! Parquet part (a 404 means pmxt hasn't published it — logged and skipped, not fatal), decodes
//! it row-group-at-a-time (bounded memory — parts run 130-400 MB), and appends the resulting
//! book/trade events into the hist store rooted at `--store` (else `$VIKE_HIST_STORE`, else
//! `<repo>/market_data/hist`; see `vike_backfill::cli::store_root`) — OR, with `--clickhouse`, INSERTs them
//! into the `polymarket` ClickHouse tables the live L2 recorder writes (see
//! `vike_backfill::pmxt::ingest_file_clickhouse`'s doc for the non-idempotency caveat: unlike the
//! hist-store path, re-running the same hour under `--clickhouse` duplicates rows — track completed
//! ranges yourself). A download/decode failure for one hour is logged and skipped — one bad hour
//! never aborts the whole range. `--tokens` narrows ingest to specific Polymarket asset (token) ids
//! (comma-separated); `--tokens-file PATH` loads more ids from a file (ONE id per line, whitespace
//! trimmed, blank lines and `#` comments skipped) — the two UNION into one filter set, so a real
//! run whose ~78k ids blow past the argv length limit passes them by file. Omit BOTH to ingest
//! every asset in each file — a lot of volume, so a warning is logged when neither is given. The
//! downloaded temp file is deleted after each hour whether the ingest succeeded or failed.
//!
//! `--dry-run` resolves and prints the hour range, the store root (or `clickhouse via {ch-bin}` in
//! `--clickhouse` mode) and each hour's archive URL, then exits WITHOUT opening the store/spawning
//! `clickhouse-client` or touching the network — the "what would this fetch" rehearsal for a range
//! before committing to hundreds of MB per hour. `--rate-ms N` sleeps N milliseconds before every
//! hour fetch EXCEPT the first (the `ibkr_backfill` pacing shape), to go easy on the archive across
//! long ranges; omitted or `0` means no sleep, so the default run is byte-identical to before these
//! flags existed.
//!
//! `--clickhouse` switches the write target from the DataFusion hist store to `clickhouse-client`
//! INSERTs (auth delegated entirely to the CLI's own config, same as the read-side
//! `clickhouse_poly`/`clickhouse_spot` collectors — no host/user/password in this tool or the
//! workspace `.env`). `--ch-bin BIN` overrides the `clickhouse-client` binary name/path (default
//! `clickhouse-client`); ignored without `--clickhouse`. `--store`/`$VIKE_HIST_STORE` are ignored in
//! `--clickhouse` mode (nothing is opened under `<repo>/market_data/hist`).
//!
//! `--min-local-ts MS`/`--max-local-ts MS` (epoch-milliseconds, both inclusive, both optional,
//! both ignored — with a warning — outside `--clickhouse` mode) narrow a write to rows whose
//! `local_ts_ms` (the archive's own ingest time — what `polymarket.book_events.local_ts_dt` is
//! materialized from) falls inside the window, INDEPENDENTLY of which hour-file each row was
//! fetched from. This is the tool the non-idempotency caveat above asks a caller to reach for: a
//! recorder outage rarely starts and ends on an hour boundary, so backfilling a WHOLE degraded
//! hour re-inserts every minute of it that was already captured — this write path has no dedup
//! key, so that re-insertion duplicates those minutes rather than filling a gap. Pass the
//! outage's actual `[start, end]` in epoch-ms (e.g. from
//! `SELECT toUnixTimestamp64Milli(toDateTime64('2026-07-28 07:44:00', 3))` against the SAME
//! ClickHouse this write targets) alongside an `[--from, --to]` hour range wide enough to cover
//! it — every fetched hour is still downloaded and decoded in full (the per-asset mapper state
//! must see every row to stay correct — see `vike_backfill::pmxt::ingest_file_clickhouse`'s doc),
//! but only rows inside the window are written.

use std::collections::HashSet;
use std::path::Path;
use std::process::ExitCode;

use vike_backfill::cli::{arg, has_flag, log_config, scratch_root, store_root, CliSpec};
use vike_backfill::pmxt::{download_hour, hour_url, ingest_file, ingest_file_clickhouse};
use vike_data::DataFusionHist;
use vike_model::scratch::ScratchDir;
use vike_model::time::hours_in_range;

const USAGE: &str = "\
usage: pmxt_backfill --from YYYY-MM-DDTHH --to YYYY-MM-DDTHH
                     [--tokens T1,T2,...] [--tokens-file PATH] [--store DIR]
                     [--rate-ms N] [--dry-run]
                     [--clickhouse [--ch-bin BIN] [--min-local-ts MS] [--max-local-ts MS]]

Ingest hourly Parquet parts from the pmxt Polymarket L2 archive (CC BY 4.0, archive-only — never
the metered API) as kind=book and kind=trade under venue=polymarket/symbol=token_id. Idempotent
per hour by commit key. An hour the archive has not published (404) is skipped, not fatal.

  --from HOUR         first hour, YYYY-MM-DDTHH (inclusive)
  --to HOUR           last hour, YYYY-MM-DDTHH (inclusive)
  --tokens T1,T2      narrow the ingest to these token ids (comma list)
  --tokens-file P     the same, one id per line — a ~78k-id run cannot fit on argv.
                      --tokens and --tokens-file UNION; neither means EVERY asset in every hour,
                      which is a great deal of volume.
  --store DIR         hist-store root (default: $VIKE_HIST_STORE, else <repo>/market_data/hist)
  --rate-ms N         sleep N ms between hour fetches (0 = no pacing, the default)
  --dry-run           report what would be fetched and where, then stop BEFORE opening the store,
                      spawning a subprocess or making any network call
  --clickhouse        write to the polymarket ClickHouse tables instead of the hist store.
                      NOT idempotent — do not re-run a range in this mode.
  --ch-bin BIN        the clickhouse client to spawn (default clickhouse-client)
  --min-local-ts MS   --clickhouse only: only write rows with local_ts_ms >= MS (epoch-ms,
                      inclusive). Narrows WITHIN each fetched hour, so a partially-degraded
                      hour can be backfilled without re-inserting its already-covered minutes.
  --max-local-ts MS   --clickhouse only: the same, local_ts_ms <= MS.
  -h, --help          print this and exit 0
  -V, --version       print the version and exit 0";

const SPEC: CliSpec = CliSpec {
    bin: "pmxt_backfill",
    usage: USAGE,
    valued: &[
        "--from",
        "--to",
        "--tokens",
        "--tokens-file",
        "--store",
        "--rate-ms",
        "--ch-bin",
        "--min-local-ts",
        "--max-local-ts",
    ],
    toggles: &["--dry-run", "--clickhouse"],
    positionals: 0,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = SPEC.short_circuit(&args) {
        return code;
    }
    let _log_guards = vike_log::init(log_config("pmxt-backfill"));
    tracing::info!("pmxt-backfill starting (data: pmxt archive.pmxt.dev, CC BY 4.0)");

    let (Some(from), Some(to)) = (arg(&args, "--from"), arg(&args, "--to")) else {
        eprintln!("pmxt_backfill: --from and --to are required\n\n{USAGE}");
        return ExitCode::from(2);
    };
    let dry_run = has_flag(&args, "--dry-run");
    let rate_ms: u64 = match arg(&args, "--rate-ms") {
        None => 0,
        Some(raw) => match raw.parse() {
            Ok(ms) => ms,
            Err(e) => {
                tracing::error!("bad --rate-ms {raw:?} (want a non-negative integer): {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    // `--clickhouse` retargets the write path from the DataFusion hist store to the SAME
    // `polymarket` ClickHouse tables the live L2 recorder writes (see the module doc's
    // non-idempotency caveat). `--store`/`$VIKE_HIST_STORE` are irrelevant in this mode.
    let clickhouse = has_flag(&args, "--clickhouse");
    let ch_bin = arg(&args, "--ch-bin").unwrap_or_else(|| "clickhouse-client".to_string());

    // `--min-local-ts`/`--max-local-ts` (epoch-ms, inclusive) narrow a `--clickhouse` write to a
    // window WITHIN each fetched hour — the tool the non-idempotency caveat asks callers to use:
    // a recorder gap rarely aligns to an hour boundary, and re-inserting the WHOLE hour on top of
    // minutes that are already captured would double-count them (the venue has no dedup key on
    // this write path). Both bounds are optional and independent; either alone bounds only that
    // side. Ignored (with a warning) outside `--clickhouse` mode — `ingest_file`'s hist-store path
    // is already idempotent per hour, so narrowing it buys nothing and would just be a second
    // no-op knob to explain.
    let parse_ts_bound = |flag: &str| -> Result<Option<i64>, ()> {
        match arg(&args, flag) {
            None => Ok(None),
            Some(raw) => match raw.parse::<i64>() {
                Ok(ms) => Ok(Some(ms)),
                Err(e) => {
                    tracing::error!("bad {flag} {raw:?} (want an epoch-millisecond integer): {e}");
                    Err(())
                }
            },
        }
    };
    let (Ok(min_local_ts), Ok(max_local_ts)) =
        (parse_ts_bound("--min-local-ts"), parse_ts_bound("--max-local-ts"))
    else {
        return ExitCode::FAILURE;
    };
    if let (Some(min), Some(max)) = (min_local_ts, max_local_ts) {
        if min > max {
            tracing::error!(
                "--min-local-ts {min} is after --max-local-ts {max} — nothing would match"
            );
            return ExitCode::FAILURE;
        }
    }
    if !clickhouse && (min_local_ts.is_some() || max_local_ts.is_some()) {
        tracing::warn!(
            "--min-local-ts/--max-local-ts have no effect without --clickhouse (the hist-store \
             path is already idempotent per hour) — ignoring"
        );
    }
    // ONE environment sweep for this bin, shared by the store root and the scratch root — so the
    // two cannot resolve against different `$VIKE_SETTINGS_DIR` answers.
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let root = store_root(arg(&args, "--store").as_deref(), &env);

    // `--tokens` (comma list) and `--tokens-file` (one id per line) UNION into the SAME filter set;
    // either alone works and neither given means no filter (the pre-flag behavior). A real
    // ~78k-id run cannot fit on the argv (Linux `MAX_ARG_STRLEN` is 128 KB), so it passes the ids
    // by file instead.
    let inline = arg(&args, "--tokens");
    let tokens_file = arg(&args, "--tokens-file");
    let tokens: Option<HashSet<String>> = if inline.is_none() && tokens_file.is_none() {
        None
    } else {
        let mut set: HashSet<String> = HashSet::new();
        if let Some(s) = inline {
            set.extend(s.split(',').map(str::trim).filter(|t| !t.is_empty()).map(str::to_string));
        }
        if let Some(path) = tokens_file {
            match load_tokens_file(Path::new(&path)) {
                Ok(ids) => set.extend(ids),
                Err(e) => {
                    tracing::error!("failed to read --tokens-file {path}: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
        Some(set)
    };
    if tokens.is_none() {
        tracing::warn!(
            "no --tokens/--tokens-file filter given: ingesting EVERY asset in every hour's file — \
             this is a lot of volume; consider --tokens T1,T2,... or --tokens-file PATH"
        );
    }

    let hours = hours_in_range(&from, &to);
    if hours.is_empty() {
        tracing::error!("bad --from/--to (want YYYY-MM-DDTHH) or --from after --to: {from}..{to}");
        return ExitCode::FAILURE;
    }
    // The write-target description used in every log line below — the DataFusion store path, or
    // (with `--clickhouse`) the ClickHouse tables + the non-idempotency reminder.
    let target = if clickhouse {
        format!("clickhouse via {ch_bin} (polymarket.book_events / polymarket.l1_quotes, NOT idempotent — see module doc)")
    } else {
        root.display().to_string()
    };
    // Rendered once, reused by both the startup line and the dry-run summary, so the two can never
    // disagree about what window is in effect.
    let window_desc = match (min_local_ts, max_local_ts) {
        (None, None) => String::new(),
        (min, max) => format!(
            " filtered to local_ts_ms in [{}, {}]",
            min.map(|v| v.to_string()).unwrap_or_else(|| "-inf".to_string()),
            max.map(|v| v.to_string()).unwrap_or_else(|| "+inf".to_string()),
        ),
    };
    tracing::info!("backfilling {} hour(s) [{from}, {to}] -> {target}{window_desc}", hours.len());

    // Rehearsal: report exactly what a real run would fetch and where it would land, then stop
    // BEFORE opening the store / spawning `clickhouse-client` (no directory is created, no
    // subprocess is spawned) and before any network call.
    if dry_run {
        for hour in &hours {
            tracing::info!("dry-run: would fetch {hour} -> {}", hour_url(hour));
        }
        tracing::info!(
            "dry-run: {} hour(s) would be ingested into {target}{window_desc} (rate_ms={rate_ms}); \
             nothing downloaded, nothing written",
            hours.len(),
        );
        return ExitCode::SUCCESS;
    }

    // `--clickhouse` never opens the DataFusion hist store at all — `store` stays `None` and the
    // hour loop below calls `ingest_file_clickhouse` instead of `ingest_file`.
    let store = if clickhouse {
        None
    } else {
        match DataFusionHist::open(&root) {
            Ok(h) => Some(h),
            Err(e) => {
                tracing::error!("open hist store at {}: {e}", root.display());
                return ExitCode::FAILURE;
            }
        }
    };

    // An OWNED staging directory under `<project>/tmp`, removed when this guard drops at the end of
    // `main` — including on the panic path. Each downloaded hour is already deleted after ingest
    // (below); the guard is what removes the ones an abort skipped, which is precisely the
    // population that used to accumulate. See `vike_backfill::cli::scratch_root`.
    let tmp_dir = match ScratchDir::create_in(&scratch_root(&env), "pmxt_backfill") {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("create scratch dir: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (mut books_total, mut trades_total, mut quotes_total, mut hours_ok, mut hours_skipped) =
        (0usize, 0usize, 0usize, 0usize, 0usize);

    for (i, hour) in hours.iter().enumerate() {
        // Archive pacing: sleep before every fetch except the first, so a single-hour run is
        // never delayed (and `--rate-ms` absent/0 is a no-op — the pre-flag behavior exactly).
        if i > 0 && rate_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(rate_ms));
        }
        let downloaded = match download_hour(hour, &tmp_dir) {
            Ok(Some(path)) => path,
            Ok(None) => {
                tracing::warn!("{hour}: not published (404) — skipping");
                hours_skipped += 1;
                continue;
            }
            Err(e) => {
                tracing::error!("{hour}: download failed: {e} — skipping");
                hours_skipped += 1;
                continue;
            }
        };
        let result = match &store {
            Some(store) => ingest_file(store, &downloaded, hour, tokens.as_ref())
                .map(|(books, trades)| (books, trades, 0usize)),
            None => ingest_file_clickhouse(
                &ch_bin,
                &downloaded,
                tokens.as_ref(),
                min_local_ts,
                max_local_ts,
            ),
        };
        // Best-effort cleanup regardless of ingest outcome — these are large temp files.
        if let Err(e) = std::fs::remove_file(&downloaded) {
            tracing::warn!("{hour}: failed to remove temp file {}: {e}", downloaded.display());
        }
        match result {
            Ok((books, trades, quotes)) => {
                tracing::info!("{hour}: {books} book events, {trades} trades, {quotes} L1 quotes");
                books_total += books;
                trades_total += trades;
                quotes_total += quotes;
                hours_ok += 1;
            }
            Err(e) => {
                tracing::error!("{hour}: ingest failed: {e} — skipping");
                hours_skipped += 1;
            }
        }
    }

    if clickhouse {
        tracing::info!(
            "pmxt-backfill done: {hours_ok} hour(s) ok, {hours_skipped} skipped, \
             {books_total} book events, {trades_total} trades, {quotes_total} L1 quotes \
             (written to ClickHouse via {ch_bin}; NOT idempotent — do not re-run this range)"
        );
    } else {
        tracing::info!(
            "pmxt-backfill done: {hours_ok} hour(s) ok, {hours_skipped} skipped, \
             {books_total} book events, {trades_total} trades (into {})",
            root.display()
        );
    }
    ExitCode::SUCCESS
}
// The hour-range iterator (formerly this bin's `hours_between`) now lives in
// `vike_model::time::hours_in_range`, tested there.

/// Load CLOB token_ids from `path`: ONE id per line, whitespace trimmed, blank lines and
/// `#`-comment lines skipped. Returns them as a set — the SAME collection `--tokens` fills — so the
/// two flags union naturally. The file route exists because a real run's ~78k ids overflow the
/// argv length limit. Kept as a small path-in/set-out helper so it is unit-testable off a temp
/// file, without going through argv parsing.
fn load_tokens_file(path: &Path) -> std::io::Result<HashSet<String>> {
    Ok(parse_token_lines(&std::fs::read_to_string(path)?))
}

/// Pure line parser behind [`load_tokens_file`]: trim each line, drop blanks and `#` comments.
/// Split out from the file read so the parsing rule is testable with no filesystem at all.
fn parse_token_lines(contents: &str) -> HashSet<String> {
    contents
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_token_lines_trims_and_skips_blanks_and_comments() {
        let contents = "\
# a header comment
t1
  t2

t3
   # indented comment
";
        let got = parse_token_lines(contents);
        assert_eq!(got, ["t1", "t2", "t3"].iter().map(|s| s.to_string()).collect());
    }

    #[test]
    fn load_tokens_file_reads_ids_from_a_temp_file() {
        let dir = std::env::temp_dir().join(format!("pmxt_tokens_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tokens.txt");
        std::fs::write(&path, "# ids\nid_a\n\n  id_b\nid_c\n").unwrap();

        let got = load_tokens_file(&path).unwrap();
        assert_eq!(got, ["id_a", "id_b", "id_c"].iter().map(|s| s.to_string()).collect());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The union contract: `--tokens` ids and `--tokens-file` ids merge into ONE set (dedup on
    /// overlap), which is exactly how `main` builds the filter — modeled here without argv.
    #[test]
    fn tokens_and_tokens_file_union_into_one_set() {
        let inline: HashSet<String> =
            "t1, t2 ,t3".split(',').map(str::trim).map(str::to_string).collect();
        let from_file = parse_token_lines("# more\nt3\nt4\n");

        let mut set = inline;
        set.extend(from_file);
        assert_eq!(
            set,
            ["t1", "t2", "t3", "t4"].iter().map(|s| s.to_string()).collect::<HashSet<_>>(),
            "t3 appears in both and is not double-counted"
        );
    }
}
