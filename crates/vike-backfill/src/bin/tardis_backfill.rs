//! Tardis datasets historical backfill CLI. Downloads a data type for one symbol across a UTC-day
//! range and ingests it into the DataFusion hist store. Bars are derived by resampling ingested
//! TRADES (real OHLCV + trade volume) — Tardis has no native OHLCV series.
//!
//! Usage:
//!   tardis_backfill --exchange deribit --symbol BTC-PERPETUAL --data-type trades \
//!       --start 2024-03-01 --end 2024-03-03 [--store DIR] [--venue NAME]
//!   tardis_backfill --exchange deribit --symbol BTC-PERPETUAL --data-type bars \
//!       --start 2024-03-01 --end 2024-03-03 --interval 5m
//!
//! data-type: trades|quotes|incremental_book_L2|bars   (bars ⇒ ingest trades then resample to
//! `--interval`, default `1m`; the trades series is persisted too)
//! Key: TARDIS_API_KEY from the credential store (the settings database), read through
//! `vike_backfill::cli::scoped_credentials` —
//! optional, first-of-month is a free sample.

use std::process::ExitCode;

use vike_backfill::cli::{history_base, log_config, parse, scoped_credentials, store_root};
use vike_backfill::refusals::BackfillBinRefusal;
use vike_backfill::tardis::{TardisKind, backfill_range, days_in_range};
use vike_data::store::store_kind::TARDIS_LANE;
use vike_data::{HistoryStoreError, TimestampRange};
use vike_secrets::CredentialKeyScope;
use vike_store::HistoryStoreDataFusion;

/// **Every credential name this binary declares** — owner ruling 2026-09-16, a process
/// materialises only what it asked for.
///
/// One name, optional at that (Tardis' first-of-month sample is keyless). A `tardis_backfill` run
/// used to hold every venue secret the store carries in order to read it.
const SCOPE: [&str; 1] = ["TARDIS_API_KEY"];

/// The Tardis key out of the SCOPED credential answer.
///
/// # ⚠ `Ok(None)` and `Err` are different events
///
/// `Ok(None)` is the credential genuinely not being in the store — harmless here, because the
/// first-of-month sample is keyless. `Err` is the name having fallen out of [`SCOPE`], which is a
/// defect in this binary and not a fact about the box; an `Option` return would have made the two
/// indistinguishable, which is the failure `vike_secrets::CredentialLookup` exists to prevent.
///
/// # Errors
/// [`vike_secrets::CredentialUndeclaredKey`] when the name asked for is outside [`SCOPE`].
fn api_key(
    store: &vike_secrets::CredentialScopedRead,
) -> Result<Option<String>, vike_secrets::CredentialUndeclaredKey> {
    Ok(store.get("TARDIS_API_KEY").declared()?.map(str::to_string))
}

/// `YYYY-MM-DD` → validated `(year, month, day)`. Delegates to the shared
/// [`vike_model::parse_ymd`] (the one home for the split+range-check, dedup A11), casting the
/// model's `i64` year down to the `(i32, u32, u32)` day tuple this CLI threads through
/// `days_in_range`/`day_span_ms`. `None` on anything malformed.
fn ymd(s: &str) -> Option<(i32, u32, u32)> {
    vike_model::parse_ymd(s).ok().map(|(y, m, d)| (y as i32, m, d))
}

/// Inclusive `[start_of_day(start), end_of_day(end)]` epoch-ms range spanning a `(y, m, d)` pair —
/// used to bound the post-ingest trades→bars resample to exactly the days just backfilled.
fn day_span_ms(start: (i32, u32, u32), end: (i32, u32, u32)) -> TimestampRange {
    use vike_model::MS_PER_DAY;
    use vike_utility::calendar::days_from_civil;
    let (sy, sm, sd) = start;
    let (ey, em, ed) = end;
    let start_ms = days_from_civil(sy as i64, sm, sd) * MS_PER_DAY;
    let end_ms = (days_from_civil(ey as i64, em, ed) + 1) * MS_PER_DAY - 1;
    TimestampRange::of(start_ms, end_ms)
}

/// Resample the stored trades of `range` into `interval` bars in the `source=tardis` lane
/// ([`TARDIS_LANE`]), never the plain series, which holds the venue's own candles: bars built from
/// trades differ from them, in volume above all (a deribit inverse candle counts the coin, its trades
/// count dollars), and the store dedups by commit key, never by row, so a reader of the plain series
/// would read both (decision 0255's rule for binance's aggregated trades). Only a read that names the
/// lane meets these bars.
fn resample_into_lane(
    store: &HistoryStoreDataFusion,
    venue: &str,
    symbol: &str,
    interval: &str,
    range: TimestampRange,
) -> Result<usize, HistoryStoreError> {
    let key = format!(
        "tardis-resample:{symbol}:{interval}:{}-{}",
        range.start.unwrap_or(0),
        range.end.unwrap_or(0)
    );
    store.resample_trades_to_bars_sourced(venue, TARDIS_LANE, symbol, interval, range, Some(&key))
}

/// The bars-resample interval: `raw` (the `--interval` argv value) defaulting to `"1m"`, validated
/// against the shared interval vocabulary ([`vike_marketdata::interval::canonical_bar_step_ms`]): the
/// string becomes the series' interval, so `0m` and `01m` are refused here, before any download. Was
/// previously hardcoded `"1m"` at the resample call site, ignoring any request for a different bar
/// size — pulled out as a pure function (mirrors `vike_backfill::cli::resolve`'s split) so the default
/// + validation is unit-testable without a process argv.
fn resolve_interval(raw: Option<&str>) -> Result<String, BackfillBinRefusal> {
    let interval = raw.unwrap_or("1m").to_string();
    if vike_marketdata::interval::canonical_bar_step_ms(&interval).is_none() {
        return Err(BackfillBinRefusal::TardisIntervalInvalid { interval });
    }
    Ok(interval)
}

/// Ingest Tardis per-day `.csv.gz` datasets into the DataFusion hist store, one day at a time.
/// Idempotent per day by a `tardis:{kind}:{symbol}:{day}` commit key; a day the vendor has not
/// published (404) is skipped, not fatal.
#[derive(clap::Parser, Debug)]
#[command(
    after_help = "environment:\n  TARDIS_API_KEY    optional — the first-of-month sample is keyless"
)]
struct Args {
    /// Tardis exchange id, e.g. binance-futures
    #[arg(long)]
    exchange: String,
    /// The instrument symbol on that exchange
    #[arg(long)]
    symbol: String,
    /// trades | quotes | incremental_book_L2 | bars (`bars` ingests trades, then resamples them
    /// into --interval OHLCV)
    #[arg(long)]
    data_type: String,
    /// First day, YYYY-MM-DD (inclusive)
    #[arg(long)]
    start: String,
    /// Last day, YYYY-MM-DD (inclusive)
    #[arg(long)]
    end: String,
    /// Hist-store root (default: <repo>/market_data/hist, else the project's own)
    #[arg(long)]
    store: Option<String>,
    /// Venue label to store under
    #[arg(long, default_value = "tardis")]
    venue: String,
    /// Bar interval for --data-type bars (default 1m)
    #[arg(long)]
    interval: Option<String>,
}

fn main() -> ExitCode {
    // ⚠ FIRST — `--help`, `--version` and a usage error answer before any log file, store or call.
    let Args { exchange, symbol, data_type, start, end, store, venue, interval } =
        parse("tardis_backfill");
    // Also fixes a drift from its siblings: this bin previously passed `LogConfig::default()`
    // verbatim, so its file logs used the generic "vike" prefix instead of its own — now aligned
    // with every other batch bin's `{name}-backfill` prefix, on top of the file_level fix.
    // ONE process-environment sweep, at the root: the log level's settings directory, the
    // hist-store root and the credential chain's settings-directory override all come out of it.
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let _guards = vike_log::init(log_config("tardis-backfill", &env));

    let (Some(start), Some(end)) = (ymd(&start), ymd(&end)) else {
        eprintln!("{}", BackfillBinRefusal::TardisRangeNotADate.rendered());
        return ExitCode::FAILURE;
    };
    // "bars" ⇒ ingest trades, then resample trades→bars after (real OHLCV + trade volume).
    let resample_bars = data_type == "bars";
    let kind = match if resample_bars { "trades" } else { data_type.as_str() } {
        "trades" => TardisKind::Trades,
        "quotes" => TardisKind::Quotes,
        "incremental_book_L2" => TardisKind::BookL2,
        other => {
            let refusal =
                BackfillBinRefusal::TardisDataTypeUnknown { data_type: other.to_string() };
            eprintln!("{}", refusal.rendered());
            return ExitCode::FAILURE;
        }
    };
    let interval = match resolve_interval(interval.as_deref()) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("{}", e.rendered());
            return ExitCode::FAILURE;
        }
    };
    let root = store_root(store.as_deref(), &env);
    // The vendor's address: its `venue_url` row (decision 0265), the seed on a box with none.
    let base = match history_base(&env, "tardis", "") {
        Ok(base) => base,
        Err(e) => {
            eprintln!("tardis_backfill: {}", e.rendered());
            return ExitCode::FAILURE;
        }
    };

    // The credential store — the settings database — read for the ONE declared name and no
    // other. Absent key is not fatal here (Tardis' first-of-month sample is keyless), exactly as before — but a name
    // outside `SCOPE` is a DEFECT IN THIS BINARY rather than an unconfigured box, so it refuses
    // instead of degrading into that same silence.
    let creds = match scoped_credentials(&env, &CredentialKeyScope::of(SCOPE)) {
        Ok(creds) => creds,
        Err(e) => {
            eprintln!("tardis_backfill: {}", e.rendered());
            return ExitCode::FAILURE;
        }
    };
    let api_key = match api_key(&creds) {
        Ok(v) => v,
        Err(key) => {
            eprintln!("{}", BackfillBinRefusal::CredentialUndeclared { key }.rendered());
            return ExitCode::FAILURE;
        }
    };

    let store = match HistoryStoreDataFusion::open(&root) {
        Ok(s) => s,
        Err(e) => {
            let refusal = BackfillBinRefusal::StoreOpenFailed {
                root: root.display().to_string(),
                why: e.to_string(),
            };
            eprintln!("{}", refusal.rendered());
            return ExitCode::FAILURE;
        }
    };
    let days = days_in_range(start, end);
    let n = match backfill_range(
        &store,
        &base,
        api_key.as_deref(),
        &exchange,
        &venue,
        &symbol,
        &kind,
        &days,
    ) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("{}", BackfillBinRefusal::RunFailed { why: e.to_string() }.rendered());
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(rows = n, %symbol, %data_type, %venue, "tardis backfill complete");

    if resample_bars {
        // Resample the just-ingested trades into `interval` bars over exactly the ingested day span.
        match resample_into_lane(&store, &venue, &symbol, &interval, day_span_ms(start, end)) {
            Ok(bars) => {
                tracing::info!(bars, %symbol, %venue, %interval, "tardis trades→bars resample complete")
            }
            Err(e) => {
                let refusal = BackfillBinRefusal::TardisResampleFailed { why: e.to_string() };
                eprintln!("{}", refusal.rendered());
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_interval_defaults_to_1m() {
        assert_eq!(resolve_interval(None).unwrap(), "1m");
    }

    #[test]
    fn resolve_interval_threads_an_explicit_request() {
        // The regression this fix closes: bars were hardcoded "1m" regardless of the requested
        // interval. An explicit --interval must now be the one that reaches the resample call.
        assert_eq!(resolve_interval(Some("5m")).unwrap(), "5m");
        assert_eq!(resolve_interval(Some("1h")).unwrap(), "1h");
        assert_eq!(resolve_interval(Some("1d")).unwrap(), "1d");
    }

    #[test]
    fn resolve_interval_rejects_garbage() {
        assert!(resolve_interval(Some("not-an-interval")).is_err());
        assert!(resolve_interval(Some("")).is_err());
    }

    /// `0m` would reach the resample and panic after the whole ingest; `01m` would write an
    /// `interval=01m` series that a reader of `1m` never sees.
    #[test]
    fn resolve_interval_refuses_a_zero_width_and_a_second_spelling() {
        for refused in ["0m", "00d", "01m", "005m", "106751991168d", "213503982335d"] {
            let e = resolve_interval(Some(refused)).expect_err(refused);
            assert_eq!(
                e,
                BackfillBinRefusal::TardisIntervalInvalid { interval: refused.to_string() },
                "{refused}"
            );
        }
    }
}

/// **The declared credential SCOPE, exercised through this binary's own resolver**: both halves,
/// and why they exist, are `vike_backfill::cli::assert_scope_covers_its_read`'s.
#[cfg(test)]
mod scope_tests {
    use super::*;

    #[test]
    fn the_declared_scope_covers_the_name_this_binary_reads() {
        vike_backfill::cli::assert_scope_covers_its_read(&SCOPE, api_key);
    }
}

/// The resample's lane, against a temp store. The file's LAST test module, so the store-openers gate,
/// which reads only the last inline test module as test code, sees this `HistoryStoreDataFusion::open` as one.
#[cfg(test)]
mod lane_tests {
    use super::*;
    use vike_data::HistoryStore;
    use vike_marketdata::test_support::{flat_bar_unit_volume, trade};

    /// Bars built from trades land in the `source=tardis` lane: a plain read keeps the venue's candles
    /// (a deribit inverse candle counts the coin, these bars the dollars of its prints), and only a read
    /// naming the lane meets the derived bars.
    #[test]
    fn the_resampled_bars_land_in_their_own_lane_and_never_the_candles() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
        let (venue, symbol) = ("deribit", "BTC-PERPETUAL");
        let candle = flat_bar_unit_volume(0, 60_000.0); // the venue's candle: 1 coin
        store.append_bars(venue, symbol, "1m", std::slice::from_ref(&candle), None).unwrap();
        let prints =
            [trade(1_000, 60_000.0, 6_000.0, false), trade(2_000, 60_100.0, 4_000.0, true)];
        store.append_trades(venue, symbol, &prints, None).unwrap();

        let n =
            resample_into_lane(&store, venue, symbol, "1m", TimestampRange::of(0, 59_999)).unwrap();
        assert_eq!(n, 1);
        let plain = store.load_bars(venue, symbol, "1m", TimestampRange::all()).unwrap();
        assert_eq!(plain.len(), 1, "the plain series holds the candle alone");
        assert_eq!(plain[0].volume, 1.0, "the candle's coin");
        let lane = store
            .load_bars_sourced_capped(venue, TARDIS_LANE, symbol, "1m", TimestampRange::all(), None)
            .unwrap();
        assert_eq!(lane.len(), 1);
        assert_eq!(lane[0].volume, 10_000.0, "the prints' dollars");
    }
}
