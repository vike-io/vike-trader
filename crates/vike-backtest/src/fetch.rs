//! `backtest data fetch` — pull REAL bars from a venue's public history into the hist store.
//!
//! # Why this lives here and not in `vike-backfill`
//!
//! `vike-backfill` is the collector fleet and owns every serious ingest path — resume, pacing
//! across days, multi-symbol lanes, the archive stores. It is also not in the container image and
//! not in CI's roster, so a user who installed the product has no downloader at all: the tools they
//! DO have (`backtest`, `vike-study`, the chart) all read a store nobody shipped them a way to
//! fill. This module is the smallest thing that closes that — one symbol, one interval, one window,
//! straight into the store the next command reads — and it is deliberately not a second collector:
//! anything that wants resume, scheduling or many symbols should reach for `vike-backfill`.
//!
//! # Why it is optional, and must stay optional
//!
//! The venue bridge brings the whole blocking transport stack (ureq + tungstenite + rustls) and
//! `vike-bridge-core/full` with it. `vike-backtest` sits UNDER `vike-cli`, whose identity is that it
//! is light and DataFusion-free — the `light-consumers` CI lane asserts exactly that — so a
//! non-optional dependency here would land that stack in the CLI's graph by feature unification.
//! Hence `venue-fetch`: off by default, forwarded by the `vike` multicall's `backtest` feature and
//! enabled for the shipped release binary, absent everywhere else.
//!
//! # What this writes, and where it cannot be confused with the demo tape
//!
//! Rows land under the venue's REAL id (`binance`), never `vike_data::demo::DEMO_VENUE`. That
//! separation is the point of the demo tape having a venue of its own: one store can hold both, a
//! query names one, and no result can quietly mix invented prices with real ones.

use std::collections::HashMap;

use vike_data::hist::HistStore;

/// What to fetch: one instrument, one interval, from one venue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchSpec {
    /// Venue id, as the store spells it (`binance`).
    pub venue: String,
    /// Instrument id in the venue's own spelling (`BTCUSDT`).
    pub symbol: String,
    /// Bar interval in the venue's spelling (`1m`, `1h`, `1d`).
    pub interval: String,
}

/// Venues this build can fetch from. A ROSTER rather than a match arm's `_` so the error message
/// can name what IS available — a user who typed `bybit` needs the list, not a refusal.
pub const FETCHABLE_VENUES: &[&str] = &["binance"];

/// Parse `VENUE:SYMBOL:INTERVAL`.
///
/// # Errors
///
/// Returns a message naming the whole grammar, because the failure a user actually hits is having
/// typed two colons' worth of a three-part spec, and an error that only says "invalid" makes them
/// guess which part.
pub fn parse_spec(raw: &str) -> Result<FetchSpec, String> {
    let parts: Vec<&str> = raw.split(':').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
        return Err(format!(
            "data fetch takes VENUE:SYMBOL:INTERVAL (e.g. binance:BTCUSDT:1h), not {raw:?}"
        ));
    }
    let venue = parts[0].to_ascii_lowercase();
    if !FETCHABLE_VENUES.contains(&venue.as_str()) {
        return Err(format!(
            "this build cannot fetch from {venue:?}. Fetchable venues: {}. For anything else use \
             the vike-backfill collectors, which own the serious ingest paths.",
            FETCHABLE_VENUES.join(", ")
        ));
    }
    Ok(FetchSpec { venue, symbol: parts[1].to_string(), interval: parts[2].to_string() })
}

/// The append key for one fetched window.
///
/// Carries the window, so re-running the SAME fetch writes nothing (the store spends a commit key
/// once) while a fetch of a different window still lands. That is the honest idempotence for this
/// command: a user who re-runs after a network failure should not double-book rows, and a user
/// extending their history should not be told there is nothing to do.
#[must_use]
pub fn commit_key(spec: &FetchSpec, from_ms: i64, to_ms: i64) -> String {
    format!("fetch:{}:{}:{}:{from_ms}-{to_ms}", spec.venue, spec.symbol, spec.interval)
}

/// What one fetch did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    /// Bars the venue returned.
    pub fetched: usize,
    /// Rows the store accepted. Zero with a non-zero `fetched` means this exact window was already
    /// present — a success.
    pub written: usize,
    /// First and last bar timestamp actually returned, if any. Reported because a venue answering
    /// a narrower window than asked (listing date, retention) is normal and invisible otherwise.
    pub span_ms: Option<(i64, i64)>,
}

/// Fetch `[from_ms, to_ms)` and append it to `store`.
///
/// # Errors
///
/// Any venue transport or parse failure, verbatim from the bridge. Nothing is written on error:
/// the fetch completes into memory first, so a store is never left holding half a window under a
/// commit key that claims the whole one.
pub fn fetch_into(
    store: &dyn HistStore,
    spec: &FetchSpec,
    from_ms: i64,
    to_ms: i64,
) -> Result<Fetched, String> {
    if to_ms <= from_ms {
        return Err(format!("the window is empty: from {from_ms} is not before to {to_ms}"));
    }
    let bars = match spec.venue.as_str() {
        // The venue's own keyless public-history path, paging under its own rate-limit policy.
        // ⚠ `end_ms` is INCLUSIVE on the venue side while every window in this workspace is
        // half-open, so the last millisecond is dropped here rather than in the caller — putting
        // the adjustment beside the call that needs it is what keeps the convention one-sided.
        "binance" => vike_binance::data::fetch_klines_range(
            &spec.symbol,
            &spec.interval,
            from_ms,
            to_ms - 1,
        )?,
        other => unreachable!("parse_spec admitted {other:?}, which fetch_into cannot serve"),
    };
    let span_ms = bars.first().zip(bars.last()).map(|(a, b)| (a.ts, b.ts));
    let written = store
        .append_bars(
            &spec.venue,
            &spec.symbol,
            &spec.interval,
            &bars,
            Some(&commit_key(spec, from_ms, to_ms)),
        )
        .map_err(|e| e.to_string())?;
    Ok(Fetched { fetched: bars.len(), written, span_ms })
}

/// Resolve the window from the flags.
///
/// `--days N` is relative to `now_unix_secs` — a PARAMETER, never a clock read, because
/// `crates/vike-ops/tests/clock_pin.rs`'s ratchet keeps ambient clock reads out of the library
/// tree and because a relative window nobody can pin is a fetch nobody can reproduce.
///
/// # Errors
///
/// A message naming which combination was given, since "you may pass `--days` OR both `--from` and
/// `--to`" is the whole rule and the user who tripped it passed something adjacent.
pub fn window(
    days: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
    now_unix_secs: &dyn Fn() -> i64,
    parse_ts: &dyn Fn(&str) -> Result<i64, String>,
) -> Result<(i64, i64), String> {
    match (days, from, to) {
        (Some(d), None, None) => {
            let n: i64 =
                d.parse().map_err(|_| format!("--days takes a whole number, not {d:?}"))?;
            if n <= 0 {
                return Err(format!("--days must be positive, not {n}"));
            }
            let end = now_unix_secs() * 1000;
            Ok((end - n * 86_400_000, end))
        }
        (None, Some(f), Some(t)) => Ok((parse_ts(f)?, parse_ts(t)?)),
        (None, None, None) => {
            Err("data fetch needs a window: --days N, or --from LABEL --to LABEL".to_string())
        }
        _ => {
            Err("data fetch takes EITHER --days N OR both --from and --to, not a mixture"
                .to_string())
        }
    }
}

/// The store root a fetch writes into, as the caller's flags and environment resolve it. Present so
/// the CLI arm reads as one call rather than repeating the precedence the rest of the binary uses.
#[must_use]
pub fn store_root_for(
    explicit: Option<std::path::PathBuf>,
    vars: &HashMap<String, String>,
) -> std::path::PathBuf {
    crate::binutil::store_root(explicit, vars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spec_needs_all_three_parts() {
        assert_eq!(
            parse_spec("binance:BTCUSDT:1h").unwrap(),
            FetchSpec { venue: "binance".into(), symbol: "BTCUSDT".into(), interval: "1h".into() }
        );
        for bad in ["binance:BTCUSDT", "binance::1h", ":BTCUSDT:1h", "binance:BTCUSDT:1h:extra", ""]
        {
            assert!(parse_spec(bad).is_err(), "{bad:?} must not parse");
        }
    }

    /// The venue is matched case-insensitively but an UNKNOWN one must name the roster — a refusal
    /// that does not say what IS available leaves the user guessing at spellings.
    #[test]
    fn an_unknown_venue_is_refused_by_name_and_lists_what_works() {
        assert_eq!(parse_spec("BINANCE:BTCUSDT:1h").unwrap().venue, "binance");
        let err = parse_spec("bybit:BTCUSDT:1h").unwrap_err();
        assert!(err.contains("bybit"), "{err}");
        for venue in FETCHABLE_VENUES {
            assert!(err.contains(venue), "the refusal does not name {venue}: {err}");
        }
    }

    /// The key must separate windows, or a second fetch extending the history would be swallowed as
    /// a duplicate of the first.
    #[test]
    fn the_commit_key_separates_windows_and_instruments() {
        let a =
            FetchSpec { venue: "binance".into(), symbol: "BTCUSDT".into(), interval: "1h".into() };
        let b =
            FetchSpec { venue: "binance".into(), symbol: "ETHUSDT".into(), interval: "1h".into() };
        assert_ne!(commit_key(&a, 0, 10), commit_key(&a, 0, 20));
        assert_ne!(commit_key(&a, 0, 10), commit_key(&b, 0, 10));
        assert_eq!(commit_key(&a, 0, 10), commit_key(&a, 0, 10));
    }

    #[test]
    fn the_window_flags_are_either_days_or_a_pair() {
        let now = || 1_700_000_000_i64;
        let ts = |s: &str| s.parse::<i64>().map_err(|_| format!("bad {s}"));
        assert_eq!(
            window(Some("2"), None, None, &now, &ts).unwrap(),
            (1_700_000_000_000 - 2 * 86_400_000, 1_700_000_000_000)
        );
        assert_eq!(window(None, Some("5"), Some("9"), &now, &ts).unwrap(), (5, 9));
        for bad in [
            window(Some("2"), Some("5"), None, &now, &ts),
            window(None, Some("5"), None, &now, &ts),
            window(None, None, None, &now, &ts),
            window(Some("0"), None, None, &now, &ts),
            window(Some("-3"), None, None, &now, &ts),
            window(Some("many"), None, None, &now, &ts),
        ] {
            assert!(bad.is_err(), "expected a refusal, got {bad:?}");
        }
    }

    /// An empty or inverted window must be refused BEFORE the network call — a venue asked for a
    /// backwards range answers with something, and what it answers is not the caller's window.
    #[test]
    fn an_inverted_window_is_refused_without_touching_the_network() {
        let store = vike_data::test_support::MemHistStore::default();
        let spec =
            FetchSpec { venue: "binance".into(), symbol: "BTCUSDT".into(), interval: "1h".into() };
        assert!(fetch_into(&store, &spec, 10, 10).is_err());
        assert!(fetch_into(&store, &spec, 10, 5).is_err());
    }
}
