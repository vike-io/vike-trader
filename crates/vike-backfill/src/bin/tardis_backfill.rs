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
//! Key: TARDIS_API_KEY from the workspace .env (optional — first-of-month is a free sample).

use std::process::ExitCode;

use vike_backfill::cli::{CliSpec, arg, log_config, store_root};
use vike_backfill::tardis::{TardisKind, backfill_range, days_in_range};
use vike_bridge_core::credentials::{KeyScope, load_workspace_secrets_scoped_from_env};
use vike_data::{DataFusionHist, HistStore, TsRange};

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
/// indistinguishable, which is the failure `vike_secrets::Lookup` exists to prevent.
///
/// # Errors
/// [`vike_bridge_core::credentials::UndeclaredKey`] when the name asked for is outside [`SCOPE`].
fn api_key(
    store: &vike_bridge_core::credentials::ScopedSecrets,
) -> Result<Option<String>, vike_bridge_core::credentials::UndeclaredKey> {
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
/// used to bound the post-ingest quotes→bars resample to exactly the days just backfilled.
fn day_span_ms(start: (i32, u32, u32), end: (i32, u32, u32)) -> TsRange {
    use vike_model::time::days_from_civil;
    const MS_PER_DAY: i64 = 86_400_000;
    let (sy, sm, sd) = start;
    let (ey, em, ed) = end;
    let start_ms = days_from_civil(sy as i64, sm, sd) * MS_PER_DAY;
    let end_ms = (days_from_civil(ey as i64, em, ed) + 1) * MS_PER_DAY - 1;
    TsRange::of(start_ms, end_ms)
}

/// The bars-resample interval: `raw` (the `--interval` argv value) defaulting to `"1m"`, validated
/// against the shared interval vocabulary ([`vike_model::time::interval_ms`]). Was previously
/// hardcoded `"1m"` at the resample call site, ignoring any request for a different bar size —
/// pulled out as a pure function (mirrors `vike_backfill::cli::resolve`'s split) so the default +
/// validation is unit-testable without a process argv.
fn resolve_interval(raw: Option<&str>) -> Result<String, String> {
    let interval = raw.unwrap_or("1m").to_string();
    if vike_model::time::interval_ms(&interval).is_none() {
        return Err(format!(
            "--interval '{interval}' is not a valid interval (e.g. 1m, 5m, 1h, 1d)"
        ));
    }
    Ok(interval)
}

const USAGE: &str = "\
usage: tardis_backfill --exchange E --symbol S --data-type T --start YYYY-MM-DD --end YYYY-MM-DD
                       [--store DIR] [--venue NAME] [--interval 1m]

Ingest Tardis per-day `.csv.gz` datasets into the DataFusion hist store, one day at a time.
Idempotent per day by a `tardis:{kind}:{symbol}:{day}` commit key; a day the vendor has not
published (404) is skipped, not fatal.

  --exchange E    Tardis exchange id, e.g. binance-futures
  --symbol S      the instrument symbol on that exchange
  --data-type T   trades | quotes | incremental_book_L2 | bars
                  (`bars` ingests trades, then resamples them into --interval OHLCV)
  --start DATE    first day, YYYY-MM-DD (inclusive)
  --end DATE      last day, YYYY-MM-DD (inclusive)
  --store DIR     hist-store root (default: $VIKE_HIST_STORE, else <repo>/market_data/hist)
  --venue NAME    venue label to store under (default: tardis)
  --interval 1m   bar interval for --data-type bars (default 1m)
  -h, --help      print this and exit 0
  -V, --version   print the version and exit 0

environment:
  TARDIS_API_KEY    optional — the first-of-month sample is keyless
  VIKE_HIST_STORE   hist-store root when --store is absent";

const SPEC: CliSpec = CliSpec {
    bin: "tardis_backfill",
    usage: USAGE,
    valued: &[
        "--exchange",
        "--symbol",
        "--data-type",
        "--start",
        "--end",
        "--store",
        "--venue",
        "--interval",
    ],
    toggles: &[],
    positionals: 0,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = SPEC.short_circuit(&args) {
        return code;
    }
    // Also fixes a drift from its siblings: this bin previously passed `LogConfig::default()`
    // verbatim, so its file logs used the generic "vike" prefix instead of its own — now aligned
    // with every other batch bin's `{name}-backfill` prefix, on top of the file_level fix.
    let _guards = vike_log::init(log_config("tardis-backfill"));

    let (Some(exchange), Some(symbol), Some(data_type), Some(start), Some(end)) = (
        arg(&args, "--exchange"),
        arg(&args, "--symbol"),
        arg(&args, "--data-type"),
        arg(&args, "--start"),
        arg(&args, "--end"),
    ) else {
        eprintln!(
            "tardis_backfill: --exchange, --symbol, --data-type, --start and --end are all required\n\n{USAGE}"
        );
        return ExitCode::from(2);
    };
    let (Some(start), Some(end)) = (ymd(&start), ymd(&end)) else {
        eprintln!("--start/--end must be YYYY-MM-DD");
        return ExitCode::FAILURE;
    };
    // "bars" ⇒ ingest trades, then resample trades→bars after (real OHLCV + trade volume).
    let resample_bars = data_type == "bars";
    let kind = match if resample_bars { "trades" } else { data_type.as_str() } {
        "trades" => TardisKind::Trades,
        "quotes" => TardisKind::Quotes,
        "incremental_book_L2" => TardisKind::BookL2,
        other => {
            eprintln!("unknown data-type '{other}': trades|quotes|incremental_book_L2|bars");
            return ExitCode::FAILURE;
        }
    };
    let venue = arg(&args, "--venue").unwrap_or_else(|| "tardis".to_string());
    let interval = match resolve_interval(arg(&args, "--interval").as_deref()) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    // ONE process-environment sweep, at the root: the hist-store root and the credential chain's
    // settings-directory override comes out of it.
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let root = store_root(arg(&args, "--store").as_deref(), &env);

    // The credential store — `<project>/settings/secrets.env`, read for the ONE declared name and
    // no other. Absent key is not fatal here (Tardis' first-of-month sample is keyless), exactly
    // as before — but a name outside `SCOPE` is a DEFECT IN THIS BINARY rather than an
    // unconfigured box, so it refuses instead of degrading into that same silence.
    let creds = load_workspace_secrets_scoped_from_env(&env, &KeyScope::of(SCOPE));
    let api_key = match api_key(&creds) {
        Ok(v) => v,
        Err(undeclared) => {
            eprintln!("credential scope defect: {undeclared}");
            return ExitCode::FAILURE;
        }
    };

    let store = match DataFusionHist::open(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("open store {}: {e}", root.display());
            return ExitCode::FAILURE;
        }
    };
    let days = days_in_range(start, end);
    let n = match backfill_range(
        &store,
        api_key.as_deref(),
        &exchange,
        &venue,
        &symbol,
        &kind,
        &days,
    ) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("backfill failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(rows = n, %symbol, %data_type, %venue, "tardis backfill complete");

    if resample_bars {
        // Resample the just-ingested trades into `interval` bars over exactly the ingested day span.
        let range = day_span_ms(start, end);
        let key = format!(
            "tardis-resample:{symbol}:{interval}:{}-{}",
            range.start.unwrap_or(0),
            range.end.unwrap_or(0)
        );
        match store.resample_trades_to_bars(&venue, &symbol, &interval, range, Some(&key)) {
            Ok(bars) => {
                tracing::info!(bars, %symbol, %venue, %interval, "tardis trades→bars resample complete")
            }
            Err(e) => {
                eprintln!("bars resample failed: {e}");
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
    use vike_bridge_core::credentials::{KeyScope, ScopedSecrets, Source};

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
        // ⚠ A name with no `vike_ops::scan` prefix, deliberately: an env-SHAPED literal here would be
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
