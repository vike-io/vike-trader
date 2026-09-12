//! Runnable pmxt Polymarket-L2 order-book backfill into the DataFusion hist store.
//!
//! Data source: the [pmxt](https://archive.pmxt.dev) Polymarket order-book archive, licensed
//! [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). This tool downloads and ingests
//! Parquet files published by **pmxt** (`r2v2.pmxt.dev`); pmxt is not affiliated with this
//! project — attribution per the license terms.
//!
//! Usage:
//!   pmxt_backfill --from YYYY-MM-DDTHH --to YYYY-MM-DDTHH [--tokens T1,T2,...]
//!                 [--tokens-file PATH] [--store DIR] [--dry-run] [--rate-ms N]
//!
//! Iterates every UTC hour in `[--from, --to]` inclusive. For each hour: downloads that hour's
//! Parquet part (a 404 means pmxt hasn't published it — logged and skipped, not fatal), decodes
//! it row-group-at-a-time (bounded memory — parts run 130-400 MB), and appends the resulting
//! book/trade events into the hist store rooted at `--store` (else `$VIKE_HIST_STORE`, else
//! `<repo>/market_data/hist`; see `vike_backfill::cli::store_root`). A download/decode failure for
//! one hour is logged and skipped — one bad hour never aborts the whole range. `--tokens` narrows
//! ingest to specific Polymarket asset (token) ids (comma-separated); `--tokens-file PATH` loads
//! more ids from a file (ONE id per line, whitespace trimmed, blank lines and `#` comments
//! skipped) — the two UNION into one filter set, so a real run whose ~78k ids blow past the argv
//! length limit passes them by file. Omit BOTH to ingest every asset in each file — a lot of
//! volume, so a warning is logged when neither is given. The downloaded temp file is deleted after
//! each hour whether the ingest succeeded or failed.
//!
//! **The whole run is idempotent per hour**, by the hist store's own commit key — re-running a
//! range costs the download and decodes nothing new into the store.
//!
//! `--dry-run` resolves and prints the hour range, the store root and each hour's archive URL, then
//! exits WITHOUT opening the store or touching the network — the "what would this fetch" rehearsal
//! for a range before committing to hundreds of MB per hour. `--rate-ms N` sleeps N milliseconds
//! before every hour fetch EXCEPT the first (the `ibkr_backfill` pacing shape), to go easy on the
//! archive across long ranges; omitted or `0` means no sleep.
//!
//! ⚠ **This tool had a SECOND write target until 2026-09-09** — `--clickhouse` (with `--ch-bin`,
//! `--min-local-ts` and `--max-local-ts`) INSERTed the same decoded rows into the `polymarket`
//! ClickHouse tables the live L2 recorder writes, and that path was NOT idempotent: it had no dedup
//! key, so re-running an hour duplicated its rows, and it duplicated 33.5M of them once. Nothing
//! scheduled it. It is gone, and `vike_backfill::pmxt`'s module doc carries the argument. There is
//! one write target now and it is the idempotent one.

use std::collections::HashSet;
use std::path::Path;
use std::process::ExitCode;

use vike_backfill::cli::{CliSpec, arg, has_flag, log_config, scratch_root, store_root};
use vike_backfill::pmxt::{download_hour, hour_url, ingest_file};
use vike_data::DataFusionHist;
use vike_model::scratch::ScratchDir;
use vike_model::time::hours_in_range;

const USAGE: &str = "\
usage: pmxt_backfill --from YYYY-MM-DDTHH --to YYYY-MM-DDTHH
                     [--tokens T1,T2,...] [--tokens-file PATH] [--store DIR]
                     [--rate-ms N] [--dry-run]

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
  --dry-run           report what would be fetched and where, then stop BEFORE opening the store
                      or making any network call
  -h, --help          print this and exit 0
  -V, --version       print the version and exit 0";

const SPEC: CliSpec = CliSpec {
    bin: "pmxt_backfill",
    usage: USAGE,
    valued: &["--from", "--to", "--tokens", "--tokens-file", "--store", "--rate-ms"],
    toggles: &["--dry-run"],
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
    // ⚠ `--clickhouse`, `--ch-bin`, `--min-local-ts` and `--max-local-ts` were parsed here until
    // 2026-09-09. They selected a second write path — the same download/decode/map pipeline with
    // the rows INSERTed into the `polymarket` ClickHouse tables the live L2 recorder writes — and
    // the two timestamp bounds existed ONLY to make that path survivable, by narrowing a re-run to
    // the minutes a recorder gap actually missed. That is the tell: a knob whose whole purpose is
    // to stop the operator double-counting is a knob compensating for a write path with no dedup
    // key. It duplicated 33.5M rows once. The path is gone; see `vike_backfill::pmxt`'s module doc.
    //
    // The hist-store path below is idempotent per hour by the store's own commit key, so nothing
    // replaces them and there is no window to narrow.

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
    // The write-target description used in every log line below. ⚠ It used to be a `match` on
    // `--clickhouse`, whose other arm named the ClickHouse tables and carried a NOT-idempotent
    // reminder in the log line itself. There is one target now, and it is idempotent, so there is
    // nothing to remind anyone of.
    let target = root.display().to_string();
    tracing::info!("backfilling {} hour(s) [{from}, {to}] -> {target}", hours.len());

    // Rehearsal: report exactly what a real run would fetch and where it would land, then stop
    // BEFORE opening the store (no directory is created) and before any network call.
    if dry_run {
        for hour in &hours {
            tracing::info!("dry-run: would fetch {hour} -> {}", hour_url(hour));
        }
        tracing::info!(
            "dry-run: {} hour(s) would be ingested into {target} (rate_ms={rate_ms}); \
             nothing downloaded, nothing written",
            hours.len(),
        );
        return ExitCode::SUCCESS;
    }

    // ⚠ This was `if clickhouse { None } else { … }` — the ClickHouse path never opened the hist
    // store at all. With one write path the store is unconditional and no longer an `Option`.
    let store = match DataFusionHist::open(&root) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("open hist store at {}: {e}", root.display());
            return ExitCode::FAILURE;
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
    let (mut books_total, mut trades_total, mut hours_ok, mut hours_skipped) =
        (0usize, 0usize, 0usize, 0usize);

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
        // ⚠ This was a `match &store` whose other arm called the ClickHouse writer, and the arms
        // were reconciled to a 3-tuple because only THAT arm counted the derived L1 quotes.
        // `ingest_file` appends quotes too but reports books and trades only, so the third field
        // was a literal `0` and every "0 L1 quotes" this bin logged was that placeholder rather
        // than a measurement. It is gone with the arm that needed it.
        let result = ingest_file(&store, &downloaded, hour, tokens.as_ref());
        // Best-effort cleanup regardless of ingest outcome — these are large temp files.
        if let Err(e) = std::fs::remove_file(&downloaded) {
            tracing::warn!("{hour}: failed to remove temp file {}: {e}", downloaded.display());
        }
        match result {
            Ok((books, trades)) => {
                tracing::info!("{hour}: {books} book events, {trades} trades");
                books_total += books;
                trades_total += trades;
                hours_ok += 1;
            }
            Err(e) => {
                tracing::error!("{hour}: ingest failed: {e} — skipping");
                hours_skipped += 1;
            }
        }
    }

    // ⚠ This was an `if clickhouse { … } else { … }`, and the ClickHouse arm's summary ended with
    // "NOT idempotent — do not re-run this range". There is one arm now and no such warning to
    // print, because the hist store's commit key makes a re-run a no-op.
    tracing::info!(
        "pmxt-backfill done: {hours_ok} hour(s) ok, {hours_skipped} skipped, \
         {books_total} book events, {trades_total} trades (into {})",
        root.display()
    );
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
