//! ClickHouse → hist-store bridge: shell `clickhouse-client --query "... FORMAT Parquet"` to
//! export one UTC day of the target Polymarket series, decode the resulting Parquet through the
//! arrow reader (the same bounded row-group discipline as `pmxt/ingest.rs`), and append via the
//! `vike-data` store's public `append_quotes`/`append_trades` API.
//!
//! Why shell the CLI instead of the HTTP interface: on the latency box `clickhouse-client` is already
//! authenticated from `~/.clickhouse-client/config.xml`, while the 8123 HTTP endpoint rejects the
//! default-user-no-password path — so driving the CLI keeps auth entirely in its own config (no
//! credentials in this code or the workspace `.env`) and adds no HTTP/CH-client dependency.
//!
//! **Read-only on the source**: every query is a `SELECT`. Idempotency: each `(kind, token, day)`
//! append carries a `clickhouse:{kind}:{token}:{day}` commit key, so re-running a day is a no-op
//! (same batch-level contract as the pmxt/eod/tardis collectors).
//!
//! ## The two L1 quote sources (2026-07-28 incident)
//!
//! `polymarket_snapshots` — the Python L1 snapshot poller table — was **retired on 2026-07-25**
//! (its `max(ts)` froze at `2026-07-25 03:52:20`, verified live on the latency box). The live L2 recorder's
//! own `l1_quotes` table (see `crate::backtest_bridge`'s module doc for its schema) has been
//! current ever since. Querying `polymarket_snapshots` for any day after the cutover silently
//! returns zero (or near-zero) rows while the bin still reports `ExitCode::SUCCESS` — a
//! silent-wrong-data trap for any backtest reading the quote lane out of the store.
//!
//! [`QuoteSource`] + [`resolve_quote_source`] fix this: `auto` (the default) picks whichever
//! source actually has rows for a given day, preferring `l1_quotes` (today's live source) and
//! falling back to `polymarket_snapshots` (the only source for pre-cutover history) — and the bin
//! logs the choice + row count for every day, `tracing::warn!`ing loudly if BOTH are empty rather
//! than writing nothing and calling it success. `--quote-source l1_quotes|snapshots` forces one
//! side, bypassing the probe.
//!
//! Both sources are projected into the SAME export column set (`ts_ms`, `token_id`, `bid`, `ask`,
//! `bid_size`, `ask_size`) so [`ingest_quotes_file`] / [`super::map::quotes_from_batch`] need no
//! source-specific branch — only the SQL differs ([`quotes_query`] vs [`l1_quotes_query`]).
//!
//! **Commit-key decision**: the append commit key stays exactly `clickhouse:quote:{token}:{day}`
//! for BOTH sources — no source tag. Rationale: (1) every day already committed under this key
//! predates the recorder and was necessarily sourced from `polymarket_snapshots` (the only source
//! that existed then), so nothing about this fix touches or re-ingests that history; (2) every
//! day silently dropped by the 2026-07-25 bug decoded ZERO rows (the frozen table returns nothing
//! at all for a post-cutover day, not just an empty-per-token slice), so `ingest_quotes_file`'s
//! `per_token` map never had an entry to append for those days — no commit was ever recorded, so
//! this fix's first real run against them commits cleanly with no key collision; (3) the one
//! narrow residual is the ~2-day window (2026-07-23 to 2026-07-25) where both sources briefly
//! overlapped — a day already committed from `polymarket_snapshots` there stays committed as-is
//! under `auto` (same key ⇒ treated as done), rather than being re-ingested from `l1_quotes`. That
//! is the SAFEST failure mode (no duplicate rows across sources for the same key), at the cost of
//! not automatically upgrading that 2-day window to the recorder's source; an operator who wants
//! it re-sourced can force `--quote-source l1_quotes` against a fresh store. A source-qualified
//! key (`clickhouse:quote:{source}:{token}:{day}`) was considered and rejected: it would either
//! leave pre-cutover history's un-tagged key format alone (defeating the point) or require a
//! one-time migration, and for the ONLY case where it would matter (that same 2-day overlap) it
//! would instead risk double-ingesting the same wall-clock ticks from both sources.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

use vike_data::{DataFusionHist, HistStore, TsRange};
use vike_model::time::{days_from_civil, parse_ymd};
use vike_model::{QuoteTick, TradeTick};

use crate::arrowutil::for_each_batch;
use crate::backtest_bridge::ClickHousePolyHistStore;
use crate::clickhouse_poly::map::{quotes_from_batch, tokens_from_batch, trades_from_batch};
use crate::error::CollectError;

/// The source database on the latency box's ClickHouse.
pub const DB: &str = "polymarket";
/// The hist-store venue every Polymarket series is written under (matches the live feed + pmxt).
pub const VENUE: &str = "polymarket";
/// The live L2 recorder's L1 top-of-book table (see the module doc + `crate::backtest_bridge`).
pub const L1_QUOTES_TABLE: &str = "l1_quotes";
/// The retired Python snapshot poller's L1 table (frozen 2026-07-25 03:52:20 UTC).
pub const SNAPSHOTS_TABLE: &str = "polymarket_snapshots";

/// Build the `slug LIKE ...` disjunction that selects the target markets in the catalog subquery.
/// Patterns containing a single quote are dropped (defensive — these come from operator argv, not
/// untrusted input, but a stray quote would break the SQL rather than inject since the whole thing
/// is one `--query` arg). Empty input yields `1` (match every market) — callers guard against that.
pub fn slug_filter(patterns: &[String]) -> String {
    let clauses: Vec<String> = patterns
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty() && !p.contains('\''))
        .map(|p| format!("slug LIKE '{p}'"))
        .collect();
    if clauses.is_empty() {
        "1".to_string()
    } else {
        clauses.join(" OR ")
    }
}

/// The catalog subquery: the set of `clob_token_ids` whose market slug matches `slug_filter`.
fn token_subquery(slug_filter: &str) -> String {
    format!("SELECT arrayJoin(clob_token_ids) FROM {DB}.polymarket_markets WHERE {slug_filter}")
}

/// How the trade tape's hist-store series key is derived from a `polymarket_trades` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TradeSymbolKey {
    /// The ERC-1155 outcome token id (`asset`) — the original shape, matching the live Polymarket
    /// feed and the pmxt archive backfill.
    #[default]
    TokenId,
    /// `"<slug>#<outcome_index>"`, e.g. `btc-updown-5m-1781092500#0` — the symbol convention
    /// [`vike_backtest::CheapNp`](../../../vike_backtest/cheap_np/struct.CheapNp.html) parses to
    /// recover a window's open `sts` and its outcome side. The model layer carries no instrument
    /// expiry (port backlog G9), so the window identity has to live IN the symbol; a 78-char token
    /// id cannot express it. `polymarket_trades` carries `slug` and `outcome_index` as columns of
    /// their own, so this needs no join against the market catalog.
    SlugOutcome,
}

impl TradeSymbolKey {
    /// The projected `token_id` column expression for this key shape.
    fn expr(self) -> &'static str {
        match self {
            TradeSymbolKey::TokenId => "asset",
            // `toString` on the UInt8 `outcome_index` gives the bare digit — `CheapNp::parse`
            // reads a single `0`/`1` after the `#`.
            TradeSymbolKey::SlugOutcome => "concat(slug, '#', toString(outcome_index))",
        }
    }

    /// The `WHERE` clause that restricts the tape to the target markets, for this key shape.
    ///
    /// **The two shapes cannot share one predicate, and getting this wrong silently loses most of
    /// the tape.** `TokenId` has no choice but to go through the market catalog: the series symbol
    /// IS the `asset` id, so the run must be restricted to the assets `polymarket_markets` lists
    /// for the matching slugs. `SlugOutcome` reads `slug` straight off `polymarket_trades`, and
    /// filtering it through the catalog would additionally require the market to have been
    /// CATALOGUED — which, for the recurring 5-minute windows, it usually has not been:
    /// `polymarket_markets` holds ~19.8k `btc-updown-5m-%` slugs against ~46k in the tape, so for
    /// 2026-04 the catalog covers **1,827 of 8,687** windows (21 %). Measured on the latency box 2026-07-22.
    fn market_filter(self, slug_filter: &str) -> String {
        match self {
            TradeSymbolKey::TokenId => {
                format!("asset IN ({})", token_subquery(slug_filter))
            }
            // `slug` is a column of `polymarket_trades` itself — no join, no catalog dependency.
            TradeSymbolKey::SlugOutcome => format!("({slug_filter})"),
        }
    }
}

/// SQL for one UTC day `[day, next_day)` of the trade tape, projected to the fixed export schema
/// (see `map.rs`). `day`/`next_day` are `YYYY-MM-DD`; the half-open `ts` range uses the table's
/// `ts` sort key rather than `toDate(ts)`.
///
/// `key` selects what the exported `token_id` column — and therefore the hist-store series symbol
/// — actually is; see [`TradeSymbolKey`]. It ALSO selects how the markets are filtered (see
/// [`TradeSymbolKey::market_filter`]): the catalog subquery for `TokenId`, the tape's own `slug`
/// column for `SlugOutcome`.
pub fn trades_query(slug_filter: &str, day: &str, next_day: &str, key: TradeSymbolKey) -> String {
    format!(
        "SELECT toUnixTimestamp64Milli(ts) AS ts_ms, {sym} AS token_id, price, size, \
         CAST(side AS String) AS side \
         FROM {DB}.polymarket_trades \
         WHERE ts >= '{day} 00:00:00' AND ts < '{next_day} 00:00:00' \
         AND {mkt} \
         ORDER BY token_id, ts \
         FORMAT Parquet",
        sym = key.expr(),
        mkt = key.market_filter(slug_filter),
    )
}

/// SQL for one UTC day of L1 top-of-book from the (now-retired) Python snapshot poller table.
/// `nonzero_only` adds `(bid > 0 OR ask > 0)` — the snapshot table carries a heartbeat of all-zero
/// rows for expired markets that carry no book. **Frozen since 2026-07-25 03:52:20 UTC** — see the
/// module doc; a caller wanting current quotes should go through [`resolve_quote_source`] rather
/// than calling this directly.
pub fn quotes_query(slug_filter: &str, day: &str, next_day: &str, nonzero_only: bool) -> String {
    let nonzero = if nonzero_only { "AND (bid > 0 OR ask > 0) " } else { "" };
    format!(
        "SELECT toUnixTimestamp64Milli(ts) AS ts_ms, token_id, bid, ask, bid_size, ask_size \
         FROM {DB}.{SNAPSHOTS_TABLE} \
         WHERE ts >= '{day} 00:00:00' AND ts < '{next_day} 00:00:00' \
         {nonzero}AND token_id IN ({sub}) \
         ORDER BY token_id, ts \
         FORMAT Parquet",
        sub = token_subquery(slug_filter),
    )
}

/// Epoch-ms for `day 00:00:00 UTC`, parsed from a `YYYY-MM-DD` string. `day`/`next_day` are always
/// generator-produced (never raw operator input — see the bin's `ymd_str`/`next_day`), so a parse
/// failure can only mean a caller bug; degrading to `0` rather than panicking keeps this a pure,
/// total function (a `0` bound would simply widen the exported range, never silently narrow it out
/// of existence, so a caller bug here fails loud via an over-wide export, not a silently-missed one).
fn day_epoch_ms(day: &str) -> i64 {
    parse_ymd(day).map(|(y, m, d)| days_from_civil(y, m, d) * 86_400_000).unwrap_or(0)
}

/// SQL for one UTC day of L1 top-of-book from the LIVE L2 recorder's `l1_quotes` table — the
/// current source (see the module doc). Projected into the identical `ts_ms`/`token_id`/`bid`/
/// `ask`/`bid_size`/`ask_size` column set [`quotes_query`] uses, so [`ingest_quotes_file`] needs no
/// source-specific decode branch. `l1_quotes.ts` is already `Int64` epoch-ms (unlike the snapshot
/// table's `DateTime64`), so the day bounds are computed directly rather than via
/// `toUnixTimestamp64Milli`.
pub fn l1_quotes_query(slug_filter: &str, day: &str, next_day: &str, nonzero_only: bool) -> String {
    let (start, end) = (day_epoch_ms(day), day_epoch_ms(next_day));
    let nonzero = if nonzero_only { "AND (bid > 0 OR ask > 0) " } else { "" };
    format!(
        "SELECT ts AS ts_ms, token_id, CAST(bid AS Float64) AS bid, CAST(ask AS Float64) AS ask, \
         CAST(bid_size AS Float64) AS bid_size, CAST(ask_size AS Float64) AS ask_size \
         FROM {DB}.{L1_QUOTES_TABLE} \
         WHERE ts >= {start} AND ts < {end} \
         {nonzero}AND token_id IN ({sub}) \
         ORDER BY token_id, ts \
         FORMAT Parquet",
        sub = token_subquery(slug_filter),
    )
}

/// Cheap `SELECT count()` probe (no Parquet export) over the snapshot table for one day + slug
/// filter, using the identical predicate [`quotes_query`] exports with — the availability check
/// `auto` mode runs before committing to a source.
pub fn quotes_count_query_snapshots(
    slug_filter: &str,
    day: &str,
    next_day: &str,
    nonzero_only: bool,
) -> String {
    let nonzero = if nonzero_only { "AND (bid > 0 OR ask > 0) " } else { "" };
    format!(
        "SELECT count() FROM {DB}.{SNAPSHOTS_TABLE} \
         WHERE ts >= '{day} 00:00:00' AND ts < '{next_day} 00:00:00' \
         {nonzero}AND token_id IN ({sub}) \
         FORMAT TSV",
        sub = token_subquery(slug_filter),
    )
}

/// Cheap `SELECT count()` probe over `l1_quotes` for one day + slug filter, the `l1_quotes` twin of
/// [`quotes_count_query_snapshots`].
pub fn quotes_count_query_l1(
    slug_filter: &str,
    day: &str,
    next_day: &str,
    nonzero_only: bool,
) -> String {
    let (start, end) = (day_epoch_ms(day), day_epoch_ms(next_day));
    let nonzero = if nonzero_only { "AND (bid > 0 OR ask > 0) " } else { "" };
    format!(
        "SELECT count() FROM {DB}.{L1_QUOTES_TABLE} \
         WHERE ts >= {start} AND ts < {end} \
         {nonzero}AND token_id IN ({sub}) \
         FORMAT TSV",
        sub = token_subquery(slug_filter),
    )
}

/// `SELECT count()` of the DISTINCT `clob_token_ids` the slug filter resolves to in
/// `polymarket_markets` — the token-universe sanity probe: a `--slug-like` pattern with no `%`
/// wildcard (a real slug always carries an epoch suffix, e.g. `btc-updown-5m-1785162900`) silently
/// matches zero markets, so the bin runs this once up front and warns loudly on `0` rather than
/// quietly backfilling nothing.
pub fn token_universe_count_query(slug_filter: &str) -> String {
    format!(
        "SELECT count() FROM (SELECT DISTINCT arrayJoin(clob_token_ids) FROM {DB}.polymarket_markets \
         WHERE {slug_filter}) FORMAT TSV"
    )
}

/// Which L1 quote source to read. `Auto` (the default) picks whichever source has rows for a given
/// day — see [`resolve_quote_source`] and the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QuoteSource {
    /// The live L2 recorder's table — current since ~2026-07-23.
    L1Quotes,
    /// The retired Python snapshot poller's table — the only source for pre-2026-07-23 history.
    Snapshots,
    /// Probe both per day and pick whichever has rows, preferring `l1_quotes` (today's live
    /// source); warn loudly if neither does. The default.
    #[default]
    Auto,
}

impl QuoteSource {
    /// Parse the bin's `--quote-source` value. `None` on an unrecognized string.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "l1_quotes" | "l1" => Some(Self::L1Quotes),
            "snapshots" | "snapshot" => Some(Self::Snapshots),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }
}

/// What [`resolve_quote_source`] decided for one day, plus the row count that decision was based
/// on (for logging) — never a silent empty write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedQuoteSource {
    /// Use `l1_quotes`; carries its row count for the day.
    L1Quotes(u64),
    /// Use `polymarket_snapshots`; carries its row count for the day.
    Snapshots(u64),
    /// The requested source (or, under `Auto`, BOTH sources) has zero rows for this day — the
    /// caller must warn loudly and skip the export rather than write an empty series.
    Neither,
}

/// Pure decision function (no I/O — the counts are probed separately via
/// [`quotes_count_query_l1`]/[`quotes_count_query_snapshots`] and [`run_count_query`], so this is
/// unit-tested directly): given what the operator requested and each source's row count for one
/// day, decide which source to read from, or `Neither` if the requested source(s) have nothing.
///
/// An explicit override still degrades to `Neither` on a zero count for the forced source — an
/// operator asking for `--quote-source snapshots` on a day past the 2026-07-25 cutover must be
/// warned, not handed a silent empty write, same as `auto`.
pub fn resolve_quote_source(
    requested: QuoteSource,
    l1_count: u64,
    snapshot_count: u64,
) -> ResolvedQuoteSource {
    match requested {
        QuoteSource::L1Quotes => {
            if l1_count > 0 {
                ResolvedQuoteSource::L1Quotes(l1_count)
            } else {
                ResolvedQuoteSource::Neither
            }
        }
        QuoteSource::Snapshots => {
            if snapshot_count > 0 {
                ResolvedQuoteSource::Snapshots(snapshot_count)
            } else {
                ResolvedQuoteSource::Neither
            }
        }
        QuoteSource::Auto => {
            if l1_count > 0 {
                ResolvedQuoteSource::L1Quotes(l1_count)
            } else if snapshot_count > 0 {
                ResolvedQuoteSource::Snapshots(snapshot_count)
            } else {
                ResolvedQuoteSource::Neither
            }
        }
    }
}

/// Run `ch_bin --query <sql>`, streaming stdout straight to `dest` (never buffered — a day of
/// trades is many MB of Parquet) and capturing stderr for the error message. `SELECT`-only.
pub fn run_export(ch_bin: &str, sql: &str, dest: &Path) -> Result<(), CollectError> {
    let file = File::create(dest)
        .map_err(|e| CollectError::Fetch(format!("create {}: {e}", dest.display())))?;
    let mut child = Command::new(ch_bin)
        .arg("--query")
        .arg(sql)
        .stdout(Stdio::from(file))
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| CollectError::Fetch(format!("spawn {ch_bin}: {e}")))?;
    let mut err = String::new();
    if let Some(mut se) = child.stderr.take() {
        let _ = se.read_to_string(&mut err);
    }
    let status = child.wait().map_err(|e| CollectError::Fetch(format!("wait {ch_bin}: {e}")))?;
    if !status.success() {
        return Err(CollectError::Fetch(format!("{ch_bin} exited {status}: {}", err.trim())));
    }
    Ok(())
}

/// Run a `SELECT count() ... FORMAT TSV` query and parse the single-line result — the cheap
/// availability probe [`resolve_quote_source`] is driven from, and the `--slug-like` token-universe
/// sanity check. Captures stdout directly (no temp file — one number, never worth streaming).
pub fn run_count_query(ch_bin: &str, sql: &str) -> Result<u64, CollectError> {
    let output = Command::new(ch_bin)
        .arg("--query")
        .arg(sql)
        .output()
        .map_err(|e| CollectError::Fetch(format!("spawn {ch_bin}: {e}")))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(CollectError::Fetch(format!(
            "{ch_bin} exited {}: {}",
            output.status,
            err.trim()
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.trim()
        .parse::<u64>()
        .map_err(|e| CollectError::Fetch(format!("parse count query result {text:?}: {e}")))
}

/// Open a downloaded Parquet part and iterate its `RecordBatch`es one row group at a time (bounded
/// memory), applying `decode` to each and collecting per-token vectors.
fn decode_by_token<T>(
    path: &Path,
    decode: impl Fn(
        &datafusion::arrow::record_batch::RecordBatch,
    ) -> Result<Vec<(String, T)>, CollectError>,
) -> Result<HashMap<String, Vec<T>>, CollectError> {
    // Shared bounded-memory row-group reader ([`crate::arrowutil::for_each_batch`]); this collector
    // just fans each decoded row into its per-token vector.
    let mut per_token: HashMap<String, Vec<T>> = HashMap::new();
    for_each_batch(path, |batch| {
        for (token, row) in decode(batch)? {
            per_token.entry(token).or_default().push(row);
        }
        Ok(())
    })?;
    Ok(per_token)
}

/// Decode a trades Parquet export and append per token under `clickhouse:trade:{token}:{day}`.
/// Returns trades written (0 for a token whose day was already ingested).
pub fn ingest_trades_file(
    store: &DataFusionHist,
    path: &Path,
    day: &str,
) -> Result<usize, CollectError> {
    let per_token = decode_by_token::<TradeTick>(path, trades_from_batch)?;
    let mut total = 0usize;
    for (token, ticks) in per_token {
        if ticks.is_empty() {
            continue;
        }
        total += store.append_trades(
            VENUE,
            &token,
            &ticks,
            Some(&format!("clickhouse:trade:{token}:{day}")),
        )?;
    }
    Ok(total)
}

/// Decode a quotes (L1) Parquet export and append per token under `clickhouse:quote:{token}:{day}`.
pub fn ingest_quotes_file(
    store: &DataFusionHist,
    path: &Path,
    day: &str,
) -> Result<usize, CollectError> {
    let per_token = decode_by_token::<QuoteTick>(path, quotes_from_batch)?;
    let mut total = 0usize;
    for (token, ticks) in per_token {
        if ticks.is_empty() {
            continue;
        }
        total += store.append_quotes(
            VENUE,
            &token,
            &ticks,
            Some(&format!("clickhouse:quote:{token}:{day}")),
        )?;
    }
    Ok(total)
}

// ---- the BOOK lane -------------------------------------------------------------------------
//
// `book_events` (the live L2 recorder's table, read by `ClickHousePolyHistStore::scan_book_updates`
// in `crate::backtest_bridge`) carries no `slug` column of its own — unlike `quotes_query`/
// `trades_query` above, which fold `token_subquery` straight into their per-day `WHERE` clause, the
// book lane has to resolve its token universe UP FRONT (once per run, not once per day: the
// catalog isn't day-scoped, so every day in a range shares the same token set), then scan each
// token's `book_events` for the day individually. Rather than write a second book decoder, this
// reuses `ClickHousePolyHistStore::scan_book_updates` verbatim — the SAME decode path
// `poly_ch_backtest`/`poly_mm_batch` already read historical Polymarket L2 through.

/// SQL to list the DISTINCT `clob_token_ids` of markets matching `slug_filter` — the book lane's
/// token-universe resolution query (see the module note above).
pub fn book_tokens_query(slug_filter: &str) -> String {
    format!(
        "SELECT DISTINCT arrayJoin(clob_token_ids) AS token_id FROM {DB}.polymarket_markets \
         WHERE {slug_filter} FORMAT Parquet"
    )
}

/// Run [`book_tokens_query`] and decode it into a flat token-id list (order not significant — the
/// caller, [`ingest_book_day`], scans each one independently).
pub fn fetch_book_tokens(
    ch_bin: &str,
    dest: &Path,
    slug_filter: &str,
) -> Result<Vec<String>, CollectError> {
    run_export(ch_bin, &book_tokens_query(slug_filter), dest)?;
    let mut out = Vec::new();
    for_each_batch(dest, |b| {
        out.extend(tokens_from_batch(b)?);
        Ok(())
    })?;
    Ok(out)
}

/// Ingest one day's `book_events` for `tokens`, reusing
/// [`crate::backtest_bridge::ClickHousePolyHistStore::scan_book_updates`] — the SAME decoder the
/// `poly_ch_backtest`/`poly_mm_batch` read path already exercises against live `book_events` —
/// rather than a second book decoder. Appends under `clickhouse:book:{token}:{day}`, the same
/// per-`(kind, token, day)` idempotency shape as [`ingest_quotes_file`]/[`ingest_trades_file`].
/// `range` is the day's `[start_ms, end_ms]` inclusive bound. Returns book updates written (0 for a
/// token whose day was already ingested). NOTE: unlike the quotes/trades lanes, each per-token
/// export here is via `ClickHousePolyHistStore`'s own temp-file plumbing (always removed
/// internally, regardless of the bin's `--keep-tmp`) — there is no day-level Parquet file for the
/// book lane to keep.
pub fn ingest_book_day(
    store: &DataFusionHist,
    bridge: &ClickHousePolyHistStore,
    tokens: &[String],
    day: &str,
    range: TsRange,
) -> Result<usize, CollectError> {
    let mut total = 0usize;
    for token in tokens {
        let updates = bridge.scan_book_updates(VENUE, token, range)?;
        if updates.is_empty() {
            continue;
        }
        total += store.append_book_updates(
            VENUE,
            token,
            &updates,
            Some(&format!("clickhouse:book:{token}:{day}")),
        )?;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_filter_builds_disjunction_and_drops_quotes() {
        assert_eq!(
            slug_filter(&["btc-updown-5m-%".into(), "eth-updown-5m-%".into()]),
            "slug LIKE 'btc-updown-5m-%' OR slug LIKE 'eth-updown-5m-%'"
        );
        // a pattern with a stray quote is dropped, not passed through
        assert_eq!(slug_filter(&["bad'quote".into()]), "1");
        assert_eq!(slug_filter(&[]), "1");
    }

    #[test]
    fn trades_query_is_readonly_range_scoped_and_token_filtered() {
        let q = trades_query(
            "slug LIKE 'btc-updown-5m-%'",
            "2026-06-04",
            "2026-06-05",
            TradeSymbolKey::TokenId,
        );
        assert!(q.starts_with("SELECT "), "SELECT-only");
        assert!(!q.to_uppercase().contains("INSERT") && !q.to_uppercase().contains("DROP"));
        assert!(q.contains("FROM polymarket.polymarket_trades"));
        assert!(q.contains("ts >= '2026-06-04 00:00:00' AND ts < '2026-06-05 00:00:00'"));
        assert!(q.contains("asset IN (SELECT arrayJoin(clob_token_ids)"));
        assert!(q.trim_end().ends_with("FORMAT Parquet"));
        // the default (and previously the ONLY) key shape: the raw ERC-1155 token id
        assert!(q.contains("asset AS token_id"));
        assert_eq!(TradeSymbolKey::default(), TradeSymbolKey::TokenId);
    }

    #[test]
    fn slug_outcome_key_projects_the_cheap_np_symbol_convention() {
        let q = trades_query(
            "slug LIKE 'btc-updown-5m-%'",
            "2026-06-04",
            "2026-06-05",
            TradeSymbolKey::SlugOutcome,
        );
        // `CheapNp::parse` wants "<slug>#<0|1>" — anything else is silently ignored by the
        // strategy, so this projection is what makes the ingested tape tradeable at all.
        assert!(q.contains("concat(slug, '#', toString(outcome_index)) AS token_id"), "{q}");
        // everything else about the query is unchanged
        assert!(q.contains("FROM polymarket.polymarket_trades"));
        assert!(q.trim_end().ends_with("FORMAT Parquet"));
    }

    #[test]
    fn slug_outcome_key_filters_the_tape_directly_not_through_the_market_catalog() {
        // The load-bearing asymmetry (see `TradeSymbolKey::market_filter`): the catalog covers a
        // MINORITY of the recurring 5-minute windows (1,827 of 8,687 for 2026-04), so routing the
        // slug-keyed export through it would silently drop ~79 % of the tape.
        let q = trades_query(
            "slug LIKE 'btc-updown-5m-%'",
            "2026-06-04",
            "2026-06-05",
            TradeSymbolKey::SlugOutcome,
        );
        assert!(!q.contains("polymarket_markets"), "no catalog join: {q}");
        assert!(!q.contains("asset IN ("), "no asset subquery: {q}");
        assert!(q.contains("AND (slug LIKE 'btc-updown-5m-%')"), "{q}");

        // ...while the token-keyed export still MUST go through it — the series symbol is the
        // asset id, which the tape's `slug` alone cannot select.
        let t = trades_query(
            "slug LIKE 'btc-updown-5m-%'",
            "2026-06-04",
            "2026-06-05",
            TradeSymbolKey::TokenId,
        );
        assert!(t.contains("asset IN (SELECT arrayJoin(clob_token_ids)"), "{t}");
    }

    #[test]
    fn quotes_query_nonzero_toggle() {
        let on = quotes_query("slug LIKE 'x'", "2026-06-04", "2026-06-05", true);
        assert!(on.contains("(bid > 0 OR ask > 0)"));
        assert!(on.contains("FROM polymarket.polymarket_snapshots"));
        let off = quotes_query("slug LIKE 'x'", "2026-06-04", "2026-06-05", false);
        assert!(!off.contains("bid > 0"));
    }

    // ---- book lane ----------------------------------------------------------------------------

    #[test]
    fn book_tokens_query_is_readonly_distinct_and_slug_scoped() {
        let q = book_tokens_query("slug LIKE 'btc-updown-5m-%'");
        assert!(q.starts_with("SELECT "), "SELECT-only");
        assert!(!q.to_uppercase().contains("INSERT") && !q.to_uppercase().contains("DROP"));
        assert!(q.contains("SELECT DISTINCT arrayJoin(clob_token_ids) AS token_id"));
        assert!(q.contains("FROM polymarket.polymarket_markets"));
        assert!(q.contains("WHERE slug LIKE 'btc-updown-5m-%'"));
        assert!(q.trim_end().ends_with("FORMAT Parquet"));
    }

    /// CI-safe (no real ClickHouse needed): an empty token list is a no-op — `ingest_book_day`
    /// never even reaches `run_export`/the store for a token universe of zero.
    #[test]
    fn ingest_book_day_with_no_tokens_is_a_clean_noop() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        // The scratch root is spelled rather than defaulted — `ClickHousePolyHistStore` has no
        // `Default` any more, because guessing where a library stages gigabytes is the defect the
        // third parameter exists to remove. Nothing is written here: a zero-token universe never
        // reaches `run_export`.
        let bridge =
            ClickHousePolyHistStore::new("clickhouse-client", DB, dir.path().join("scratch"));
        let n = ingest_book_day(&store, &bridge, &[], "2026-06-04", TsRange::of(0, 1)).unwrap();
        assert_eq!(n, 0);
    }

    // ---- the l1_quotes vs polymarket_snapshots source split (2026-07-28 incident) --------------

    #[test]
    fn l1_quotes_query_reads_the_live_recorder_table_with_int64_epoch_bounds() {
        let start = days_from_civil(2026, 6, 4) * 86_400_000;
        let end = days_from_civil(2026, 6, 5) * 86_400_000;
        let q = l1_quotes_query("slug LIKE 'x'", "2026-06-04", "2026-06-05", true);
        assert!(q.contains("FROM polymarket.l1_quotes"), "{q}");
        assert!(!q.contains("polymarket_snapshots"), "{q}");
        assert!(q.contains(&format!("ts >= {start} AND ts < {end}")), "{q}");
        // no DateTime64 string bound — l1_quotes.ts is already Int64 epoch-ms.
        assert!(!q.contains("00:00:00"), "{q}");
        assert!(q.contains("CAST(bid AS Float64) AS bid"), "{q}");
        assert!(q.contains("(bid > 0 OR ask > 0)"), "{q}");
        // same projected column set as the snapshots query, so the decode side needs no branch.
        assert!(q.contains("ts AS ts_ms"));
        assert!(q.contains("token_id"));
    }

    #[test]
    fn l1_quotes_query_and_quotes_query_project_the_identical_column_set() {
        // Both queries must decode through the SAME `quotes_from_batch` (see map.rs) — so their
        // *aliases* must match exactly even though the underlying tables/types differ.
        let snap = quotes_query("slug LIKE 'x'", "2026-06-04", "2026-06-05", true);
        let l1 = l1_quotes_query("slug LIKE 'x'", "2026-06-04", "2026-06-05", true);
        for alias in ["ts_ms", "token_id", "bid", "ask", "bid_size", "ask_size"] {
            assert!(snap.contains(alias), "snapshots query missing {alias}: {snap}");
            assert!(l1.contains(alias), "l1_quotes query missing {alias}: {l1}");
        }
    }

    #[test]
    fn count_queries_mirror_their_export_queries_predicate() {
        let snap_count =
            quotes_count_query_snapshots("slug LIKE 'x'", "2026-06-04", "2026-06-05", true);
        assert!(snap_count.starts_with("SELECT count()"));
        assert!(snap_count.contains("FROM polymarket.polymarket_snapshots"));
        assert!(snap_count.contains("(bid > 0 OR ask > 0)"));
        assert!(snap_count.trim_end().ends_with("FORMAT TSV"));

        let l1_count = quotes_count_query_l1("slug LIKE 'x'", "2026-06-04", "2026-06-05", true);
        assert!(l1_count.starts_with("SELECT count()"));
        assert!(l1_count.contains("FROM polymarket.l1_quotes"));
        assert!(l1_count.contains("(bid > 0 OR ask > 0)"));
        assert!(l1_count.trim_end().ends_with("FORMAT TSV"));
    }

    #[test]
    fn token_universe_count_query_is_readonly_and_distinct() {
        let q = token_universe_count_query("slug LIKE 'btc-updown-5m'");
        assert!(q.starts_with("SELECT count()"));
        assert!(q.contains("DISTINCT arrayJoin(clob_token_ids)"));
        assert!(q.contains("FROM polymarket.polymarket_markets"));
        assert!(q.contains("WHERE slug LIKE 'btc-updown-5m'"));
        assert!(q.trim_end().ends_with("FORMAT TSV"));
    }

    #[test]
    fn quote_source_parses_the_bin_flag_values() {
        assert_eq!(QuoteSource::parse("auto"), Some(QuoteSource::Auto));
        assert_eq!(QuoteSource::parse("l1_quotes"), Some(QuoteSource::L1Quotes));
        assert_eq!(QuoteSource::parse("l1"), Some(QuoteSource::L1Quotes));
        assert_eq!(QuoteSource::parse("snapshots"), Some(QuoteSource::Snapshots));
        assert_eq!(QuoteSource::parse("snapshot"), Some(QuoteSource::Snapshots));
        assert_eq!(QuoteSource::parse("bogus"), None);
        assert_eq!(QuoteSource::default(), QuoteSource::Auto);
    }

    #[test]
    fn resolve_quote_source_auto_prefers_l1_then_snapshots_then_neither() {
        // l1 has rows -> l1, regardless of what snapshots has (it's the frozen/stale source).
        assert_eq!(
            resolve_quote_source(QuoteSource::Auto, 42, 999),
            ResolvedQuoteSource::L1Quotes(42)
        );
        // l1 empty, snapshots has rows (pre-cutover history) -> snapshots.
        assert_eq!(
            resolve_quote_source(QuoteSource::Auto, 0, 7),
            ResolvedQuoteSource::Snapshots(7)
        );
        // both empty -> Neither, the "warn loudly, write nothing" signal.
        assert_eq!(resolve_quote_source(QuoteSource::Auto, 0, 0), ResolvedQuoteSource::Neither);
    }

    #[test]
    fn resolve_quote_source_explicit_override_still_reports_neither_on_zero() {
        // Forcing l1_quotes on a day it has no rows must NOT silently fall back to snapshots —
        // the whole point of an explicit override is the operator gets exactly that source, or a
        // loud warning, never a quiet substitution.
        assert_eq!(
            resolve_quote_source(QuoteSource::L1Quotes, 0, 500),
            ResolvedQuoteSource::Neither
        );
        assert_eq!(
            resolve_quote_source(QuoteSource::L1Quotes, 10, 0),
            ResolvedQuoteSource::L1Quotes(10)
        );
        // Forcing snapshots on a post-cutover day (real scenario: the bug this fix addresses)
        // must warn rather than silently write the empty result.
        assert_eq!(
            resolve_quote_source(QuoteSource::Snapshots, 0, 0),
            ResolvedQuoteSource::Neither
        );
        assert_eq!(
            resolve_quote_source(QuoteSource::Snapshots, 0, 3),
            ResolvedQuoteSource::Snapshots(3)
        );
    }
}
