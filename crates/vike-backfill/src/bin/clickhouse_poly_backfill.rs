//! Runnable ClickHouse → hist-store backfill for the recurring Polymarket 5-minute BTC/ETH
//! up/down markets (or any slug pattern). Reads the latency box's local `polymarket` ClickHouse DB
//! (SELECT-only) and ingests L1 quotes, the trade tape, and/or L2 book updates into the DataFusion
//! hist store.
//!
//! Usage:
//!   clickhouse_poly_backfill --from YYYY-MM-DD --to YYYY-MM-DD
//!       [--kind trade|quote|book|both|all] [--slug-like 'btc-updown-5m-%,eth-updown-5m-%']
//!       [--symbol-key token|slug] [--quote-source auto|l1_quotes|snapshots]
//!       [--store DIR] [--ch-bin clickhouse-client] [--all-quotes] [--keep-tmp] [--dry-run]
//!
//! Iterates every UTC day in `[--from, --to]` inclusive. For each day + kind it runs one
//! `clickhouse-client --query "... FORMAT Parquet"` export (streamed to a temp file), decodes it a
//! row group at a time (bounded memory), and appends per token under a
//! `clickhouse:{kind}:{token}:{day}` commit key — so re-running a day is idempotent. A failed day
//! is logged and skipped; one bad day never aborts the range.
//!
//! `--kind` selects the lane(s): `trade`/`quote`/`book` run just one; `both` (the default) keeps
//! the original trade+quote pair byte-identical; `all` adds book on top of `both`. The BOOK lane
//! reads `book_events` (the live L2 recorder's table — no `slug` column of its own), so it first
//! resolves the matching token universe ONCE per run via `--slug-like` against
//! `polymarket_markets` (same subquery the quotes/trades lanes fold into their own day query),
//! then scans + appends each token's day individually
//! (`vike_backfill::clickhouse_poly::ingest_book_day`, which reuses
//! `ClickHousePolyHistStore::scan_book_updates` verbatim rather than a second decoder).
//!
//! `--symbol-key` picks what the TRADE tape's series symbol is: `token` (default, the ERC-1155
//! `asset` id — matching the live feed and the pmxt archive) or `slug`, which keys the series
//! `"<slug>#<outcome_index>"`. `slug` is what `vike_backtest::CheapNp` can parse: the model layer
//! carries no instrument expiry, so a 5-minute window's open `sts` has to travel IN the symbol.
//! The two shapes are independent series and can coexist in one store. `--all-quotes` keeps the L1
//! zero rows (default drops `bid==0 && ask==0`, the expired-market heartbeat). `--dry-run` prints
//! the resolved days + the exact queries and exits without touching ClickHouse or the store (the
//! book lane's per-token queries depend on a live token-universe fetch, so `--dry-run` prints only
//! the token-universe query itself, not each token's `book_events` query).
//!
//! **Quotes have TWO possible sources** (see `vike_backfill::clickhouse_poly::QuoteSource`'s doc for
//! the full incident writeup): the retired Python `polymarket_snapshots` poller (frozen 2026-07-25
//! 03:52:20 UTC, the only source for pre-2026-07-23 history) and the live L2 recorder's
//! `l1_quotes` (current ever since). `--quote-source auto` (the default) probes both per day and
//! picks whichever has rows, preferring `l1_quotes`; `l1_quotes`/`snapshots` force one side. Either
//! way the bin logs which source + row count it used for every day, and `tracing::warn!`s loudly —
//! never silently — when the resolved source (or, under `auto`, both) has zero rows for a day.
//!
//! The tool ALSO logs the resolved `--slug-like` token universe (a distinct `clob_token_ids` count
//! from `polymarket_markets`) before the day loop and warns if it is zero — a bare pattern with no
//! trailing `%` (real slugs carry an epoch suffix, e.g. `btc-updown-5m-1785162900`) silently
//! matches no markets, so a zero-token run must never look like a quiet success.
//!
//! Example — export 7 days of book+quote+trade for the recurring 5m/15m up/down markets into a
//! local store, ready for `poly_mm_batch --store DIR`:
//!   clickhouse_poly_backfill --from 2026-07-01 --to 2026-07-07 --kind all \
//!       --slug-like 'btc-updown-5m-%,eth-updown-5m-%' --store /data/poly_hist

use std::path::PathBuf;
use std::process::ExitCode;

use vike_backfill::backtest_bridge::ClickHousePolyHistStore;
use vike_backfill::cli::{CliSpec, arg, has_flag, log_config, scratch_root, store_root};
use vike_backfill::clickhouse_poly::{
    DB, QuoteSource, ResolvedQuoteSource, TradeSymbolKey, book_tokens_query, fetch_book_tokens,
    ingest_book_day, ingest_quotes_file, ingest_trades_file, l1_quotes_query,
    quotes_count_query_l1, quotes_count_query_snapshots, quotes_query, resolve_quote_source,
    run_count_query, run_export, slug_filter, token_universe_count_query, trades_query,
};
use vike_data::{DataFusionHist, TsRange};
use vike_model::scratch::ScratchDir;
use vike_model::time::{civil_from_days, days_from_civil, days_in_range, parse_ymd};

fn ymd_str(y: i64, m: u32, d: u32) -> String {
    format!("{y:04}-{m:02}-{d:02}")
}

fn next_day(y: i64, m: u32, d: u32) -> String {
    let (ny, nm, nd) = civil_from_days(days_from_civil(y, m, d) + 1);
    ymd_str(ny, nm, nd)
}

/// `[day 00:00:00, next_day 00:00:00)` as an inclusive-bound `TsRange` — the book lane's day
/// window. Unlike the quotes/trades lanes (which fold their day bounds straight into `WHERE ts >=
/// ... AND ts < ...` SQL), `scan_book_updates` takes a `TsRange`, so the half-open day becomes
/// `[start_ms, start_ms + 1 day - 1]` inclusive.
fn day_ms_range(y: i64, m: u32, d: u32) -> TsRange {
    let start = days_from_civil(y, m, d) * 86_400_000;
    TsRange::of(start, start + 86_400_000 - 1)
}

/// Probe row counts for one day (skipping the count query for a source `requested` rules out
/// entirely, and skipping the `polymarket_snapshots` probe under `auto` once `l1_quotes` already
/// has rows — the common-case, count-query-per-day-not-per-source-pair path), resolve which source
/// to use via [`resolve_quote_source`], and log the decision — `tracing::info!` on a real source,
/// `tracing::warn!` (never silently) on [`ResolvedQuoteSource::Neither`]. A count-probe failure
/// degrades to `0` for that source with its own warning, rather than aborting the day.
fn resolve_day_quote_source(
    ch_bin: &str,
    requested: QuoteSource,
    slugf: &str,
    day: &str,
    nxt: &str,
    nonzero_only: bool,
) -> ResolvedQuoteSource {
    let l1_count = if requested != QuoteSource::Snapshots {
        match run_count_query(ch_bin, &quotes_count_query_l1(slugf, day, nxt, nonzero_only)) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("{day}: l1_quotes count probe failed: {e}");
                0
            }
        }
    } else {
        0
    };
    let need_snapshot_count =
        requested == QuoteSource::Snapshots || (requested == QuoteSource::Auto && l1_count == 0);
    let snapshot_count = if need_snapshot_count {
        match run_count_query(ch_bin, &quotes_count_query_snapshots(slugf, day, nxt, nonzero_only))
        {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("{day}: polymarket_snapshots count probe failed: {e}");
                0
            }
        }
    } else {
        0
    };

    let resolved = resolve_quote_source(requested, l1_count, snapshot_count);
    match resolved {
        ResolvedQuoteSource::L1Quotes(n) => {
            tracing::info!("{day}: quote source=l1_quotes ({n} row(s))")
        }
        ResolvedQuoteSource::Snapshots(n) => {
            tracing::info!("{day}: quote source=polymarket_snapshots ({n} row(s))")
        }
        ResolvedQuoteSource::Neither => tracing::warn!(
            "{day}: NO quote rows in {} for --quote-source {requested:?} (l1_quotes={l1_count}, \
             polymarket_snapshots={snapshot_count}) — writing NOTHING for this day rather than a \
             silent empty series",
            if requested == QuoteSource::Auto { "EITHER source" } else { "the requested source" },
        ),
    }
    resolved
}

const USAGE: &str = "\
usage: clickhouse_poly_backfill --from YYYY-MM-DD --to YYYY-MM-DD
                                [--kind trade|quote|book|both|all] [--slug-like P1,P2]
                                [--quote-source auto|l1_quotes|snapshots] [--symbol-key token|slug]
                                [--store DIR] [--ch-bin clickhouse-client]
                                [--all-quotes] [--keep-tmp] [--dry-run]

SELECT-only export of the live recorder's local `data_polymarket` ClickHouse tables into the
DataFusion hist store — the local-data twin of pmxt_backfill. One day per export, idempotent by a
`clickhouse:{kind}:{token}:{day}` commit key.

  --from DATE          first UTC day, YYYY-MM-DD (inclusive)
  --to DATE            last UTC day, YYYY-MM-DD (inclusive)
  --kind K             trade | quote | book | both | all (default both = trades + quotes)
  --slug-like P1,P2    market slug LIKE patterns (default btc-updown-5m-%,eth-updown-5m-%)
  --quote-source S     auto | l1_quotes | snapshots (default auto)
  --symbol-key K       token | slug — what the stored symbol is keyed on (default token)
  --store DIR          hist-store root (default: $VIKE_HIST_STORE, else <repo>/market_data/hist)
  --ch-bin BIN         the clickhouse client to spawn (default clickhouse-client)
  --all-quotes         keep zero-sized quote rows too (default: non-zero only)
  --keep-tmp           keep the per-day Parquet exports instead of deleting them
  --dry-run            print the resolved days and queries, then exit without touching ClickHouse
                       or the store
  -h, --help           print this and exit 0
  -V, --version        print the version and exit 0";

const SPEC: CliSpec = CliSpec {
    bin: "clickhouse_poly_backfill",
    usage: USAGE,
    valued: &[
        "--from",
        "--to",
        "--kind",
        "--slug-like",
        "--quote-source",
        "--symbol-key",
        "--store",
        "--ch-bin",
    ],
    toggles: &["--all-quotes", "--keep-tmp", "--dry-run"],
    positionals: 0,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = SPEC.short_circuit(&args) {
        return code;
    }
    let _log_guards = vike_log::init(log_config("clickhouse-poly-backfill"));

    let (Some(from), Some(to)) = (arg(&args, "--from"), arg(&args, "--to")) else {
        eprintln!("clickhouse_poly_backfill: --from and --to are required\n\n{USAGE}");
        return ExitCode::from(2);
    };

    let kind = arg(&args, "--kind").unwrap_or_else(|| "both".to_string());
    let (do_trades, do_quotes, do_book) = match kind.as_str() {
        "trade" | "trades" => (true, false, false),
        "quote" | "quotes" => (false, true, false),
        "book" | "books" => (false, false, true),
        "both" => (true, true, false),
        "all" => (true, true, true),
        other => {
            tracing::error!("bad --kind {other:?} (want trade|quote|book|both|all)");
            return ExitCode::FAILURE;
        }
    };

    let slug_patterns: Vec<String> = arg(&args, "--slug-like")
        .unwrap_or_else(|| "btc-updown-5m-%,eth-updown-5m-%".to_string())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let slugf = slug_filter(&slug_patterns);

    let ch_bin = arg(&args, "--ch-bin").unwrap_or_else(|| "clickhouse-client".to_string());
    let quote_source = match arg(&args, "--quote-source") {
        None => QuoteSource::Auto,
        Some(s) => match QuoteSource::parse(&s) {
            Some(qs) => qs,
            None => {
                tracing::error!("bad --quote-source {s:?} (want auto|l1_quotes|snapshots)");
                return ExitCode::FAILURE;
            }
        },
    };
    let symbol_key =
        match arg(&args, "--symbol-key").unwrap_or_else(|| "token".to_string()).as_str() {
            "token" | "token_id" => TradeSymbolKey::TokenId,
            "slug" | "slug-outcome" => TradeSymbolKey::SlugOutcome,
            other => {
                tracing::error!("bad --symbol-key {other:?} (want token|slug)");
                return ExitCode::FAILURE;
            }
        };
    let nonzero_only = !has_flag(&args, "--all-quotes");
    let keep_tmp = has_flag(&args, "--keep-tmp");
    let dry_run = has_flag(&args, "--dry-run");
    // ONE environment sweep for this bin, shared by the store root and the scratch root — so the
    // two cannot resolve against different `$VIKE_SETTINGS_DIR` answers.
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let root = store_root(arg(&args, "--store").as_deref(), &env);

    let (start, end) = match (parse_ymd(&from), parse_ymd(&to)) {
        (Ok(s), Ok(e)) => (s, e),
        (Err(e), _) | (_, Err(e)) => {
            tracing::error!("bad --from/--to: {e}");
            return ExitCode::FAILURE;
        }
    };
    let days = days_in_range((start.0, start.1, start.2), (end.0, end.1, end.2));
    if days.is_empty() {
        tracing::error!("empty day range (--from after --to?): {from}..{to}");
        return ExitCode::FAILURE;
    }

    tracing::info!(
        "clickhouse-poly-backfill: {} day(s) [{from}, {to}], kind={kind}, quote_source={quote_source:?}, \
         slugs=[{}] -> {}",
        days.len(),
        slug_patterns.join(","),
        root.display()
    );

    if dry_run {
        if do_book {
            // The book lane's per-token queries depend on a live token-universe fetch (there is no
            // slug column on `book_events` to fold a day bound into) — dry-run prints only the
            // token-universe query itself, once, rather than pretending to know the token set.
            tracing::info!("dry-run BOOK token-universe query:\n{}", book_tokens_query(&slugf));
        }
        for (y, m, d) in &days {
            let day = ymd_str(*y, *m, *d);
            let nxt = next_day(*y, *m, *d);
            if do_trades {
                tracing::info!(
                    "dry-run {day} TRADES query:\n{}",
                    trades_query(&slugf, &day, &nxt, symbol_key)
                );
            }
            if do_quotes {
                // `auto` can't know which source it would pick without a live count probe (which
                // dry-run must never do — see the module doc), so print BOTH candidates labeled;
                // an explicit override prints just the forced one.
                if quote_source != QuoteSource::Snapshots {
                    tracing::info!(
                        "dry-run {day} QUOTES query (l1_quotes{}):\n{}",
                        if quote_source == QuoteSource::Auto {
                            " candidate, preferred"
                        } else {
                            ""
                        },
                        l1_quotes_query(&slugf, &day, &nxt, nonzero_only)
                    );
                }
                if quote_source != QuoteSource::L1Quotes {
                    tracing::info!(
                        "dry-run {day} QUOTES query (polymarket_snapshots{}):\n{}",
                        if quote_source == QuoteSource::Auto { " candidate, fallback" } else { "" },
                        quotes_query(&slugf, &day, &nxt, nonzero_only)
                    );
                }
            }
            if do_book {
                tracing::info!(
                    "dry-run {day} BOOK: would scan book_events per resolved token over \
                     [{day} 00:00:00, {nxt} 00:00:00)"
                );
            }
        }
        tracing::info!("dry-run: nothing exported, nothing written");
        return ExitCode::SUCCESS;
    }

    let store = match DataFusionHist::open(&root) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("open hist store at {}: {e}", root.display());
            return ExitCode::FAILURE;
        }
    };

    if do_quotes {
        match run_count_query(&ch_bin, &token_universe_count_query(&slugf)) {
            Ok(n) => {
                tracing::info!(
                    "resolved token universe: {n} token_id(s) matching slug filter [{}]",
                    slug_patterns.join(",")
                );
                if n == 0 {
                    tracing::warn!(
                        "resolved token universe is EMPTY for slug filter [{}] — a --slug-like \
                         pattern usually needs a trailing '%' (real slugs carry an epoch suffix, \
                         e.g. btc-updown-5m-1785162900); this run will write NOTHING for --kind \
                         quote/both",
                        slug_patterns.join(",")
                    );
                }
            }
            Err(e) => {
                tracing::warn!("token universe count probe failed: {e} (continuing anyway)");
            }
        }
    }

    // An OWNED staging directory under `<project>/tmp`, removed when this guard drops at the end of
    // `main` — including on the panic path. It used to be a FIXED name under the system temp
    // directory, which leaked every export it ever staged and, on a box where CI and agents run as
    // different users, handed whoever created it first permanent ownership. See
    // `vike_backfill::cli::scratch_root` for both halves of that argument.
    let staged = match ScratchDir::create_in(&scratch_root(&env), "clickhouse_poly_backfill") {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("create scratch dir: {e}");
            return ExitCode::FAILURE;
        }
    };
    // ⚠ `--keep-tmp` gives up ownership HERE rather than at the end of `main`. Its whole promise is
    // that the staged exports outlive the run, and this function has several early returns below —
    // a guard released on only some of them would honour the flag or not depending on which error
    // path fired, which is the worst of both.
    let (tmp_dir, _staged): (PathBuf, Option<ScratchDir>) = if keep_tmp {
        let kept = staged.keep();
        tracing::info!(dir = %kept.display(), "--keep-tmp: staged exports are left in place");
        (kept, None)
    } else {
        (staged.path().to_path_buf(), Some(staged))
    };

    // The book lane's token universe is resolved ONCE up front (`polymarket_markets` isn't
    // day-scoped, so every day in the range shares the same answer) — see the module doc.
    let bridge = ClickHousePolyHistStore::new(ch_bin.clone(), DB, tmp_dir.clone());
    let book_tokens: Vec<String> = if do_book {
        match fetch_book_tokens(&ch_bin, &tmp_dir.join("book_tokens.parquet"), &slugf) {
            Ok(t) => {
                tracing::info!("book lane: {} token(s) matched the slug filter", t.len());
                t
            }
            Err(e) => {
                tracing::error!("failed to resolve book token universe: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        Vec::new()
    };

    let (mut q_total, mut t_total, mut bk_total, mut days_ok, mut days_err) =
        (0usize, 0usize, 0usize, 0usize, 0usize);

    for (y, m, d) in &days {
        let day = ymd_str(*y, *m, *d);
        let nxt = next_day(*y, *m, *d);
        let mut day_failed = false;

        if do_trades {
            let dest = tmp_dir.join(format!("trades_{day}.parquet"));
            match run_export(&ch_bin, &trades_query(&slugf, &day, &nxt, symbol_key), &dest)
                .and_then(|()| ingest_trades_file(&store, &dest, &day))
            {
                Ok(n) => {
                    tracing::info!("{day}: {n} trades");
                    t_total += n;
                }
                Err(e) => {
                    tracing::error!("{day}: trades failed: {e}");
                    day_failed = true;
                }
            }
            if !keep_tmp {
                let _ = std::fs::remove_file(&dest);
            }
        }

        if do_quotes {
            match resolve_day_quote_source(&ch_bin, quote_source, &slugf, &day, &nxt, nonzero_only)
            {
                ResolvedQuoteSource::Neither => {
                    // Already warned loudly inside resolve_day_quote_source — write nothing for
                    // this day rather than a silent empty series.
                }
                resolved @ (ResolvedQuoteSource::L1Quotes(_)
                | ResolvedQuoteSource::Snapshots(_)) => {
                    let sql = match resolved {
                        ResolvedQuoteSource::L1Quotes(_) => {
                            l1_quotes_query(&slugf, &day, &nxt, nonzero_only)
                        }
                        ResolvedQuoteSource::Snapshots(_) => {
                            quotes_query(&slugf, &day, &nxt, nonzero_only)
                        }
                        ResolvedQuoteSource::Neither => unreachable!("matched above"),
                    };
                    let dest = tmp_dir.join(format!("quotes_{day}.parquet"));
                    match run_export(&ch_bin, &sql, &dest)
                        .and_then(|()| ingest_quotes_file(&store, &dest, &day))
                    {
                        Ok(n) => {
                            tracing::info!("{day}: {n} quotes");
                            q_total += n;
                        }
                        Err(e) => {
                            tracing::error!("{day}: quotes failed: {e}");
                            day_failed = true;
                        }
                    }
                    if !keep_tmp {
                        let _ = std::fs::remove_file(&dest);
                    }
                }
            }
        }

        if do_book {
            match ingest_book_day(&store, &bridge, &book_tokens, &day, day_ms_range(*y, *m, *d)) {
                Ok(n) => {
                    tracing::info!("{day}: {n} book updates");
                    bk_total += n;
                }
                Err(e) => {
                    tracing::error!("{day}: book failed: {e}");
                    day_failed = true;
                }
            }
        }

        if day_failed {
            days_err += 1;
        } else {
            days_ok += 1;
        }
    }

    tracing::info!(
        "clickhouse-poly-backfill done: {days_ok} day(s) ok, {days_err} with errors, \
         {t_total} trades, {q_total} quotes, {bk_total} book updates -> {}",
        root.display()
    );
    ExitCode::SUCCESS
}
