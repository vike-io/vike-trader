//! Bulk "Backfill" job planning (dm-bulk-backfill) — the pure fn that maps a Data-Manager grid
//! selection + the app's known gap ranges into a list of concrete kline-backfill jobs, factored
//! out of `vike-app`'s `App::maybe_spawn_stored_backfill` so it's unit-testable without an
//! `App`/`eframe::CreationContext` (same rationale as `feed_lifecycle`'s pure gates — see that
//! module's doc). Only `kind == "bar"` series on a [`SUPPORTED_BACKFILL_VENUES`] venue are
//! backfillable today — the three crypto venues `vike-backfill` has a keyless kline backfiller
//! for (`backfill_binance_klines`/`backfill_bybit_klines`/`backfill_okx_klines`, all public REST,
//! no credentials). Everything else (quote/trade/book series, or a venue with no collector yet —
//! e.g. dukascopy/polymarket/deribit/oanda/…) is counted as skipped, never silently dropped from
//! the tally `vike-app` surfaces to the user ("backfilled N series, skipped M unsupported").

use std::collections::BTreeSet;

use vike_data_manager::{GapMap, SeriesKey};

use crate::backfill_route::BackfillRoute;

/// Venues `vike-backfill` can backfill klines for today (keyless public REST). Kept as a plain
/// slice (not an enum) so the caller's `match job.venue.as_str()` dispatch onto
/// `backfill_{binance,bybit,okx}_klines` and this filter stay the ONE place venue support is
/// named — adding a fourth venue's collector is then a two-line diff here plus one new match arm
/// at the call site, not a refactor.
pub const SUPPORTED_BACKFILL_VENUES: &[&str] = &["binance", "bybit", "okx"];

/// One series' worth of backfill work: one or more `[start_ms, end_ms]` windows (inclusive
/// epoch-ms, same convention as `stored_gaps`/`DataFusionHist::series_gaps`) to fetch+ingest via
/// `venue`'s `backfill_<venue>_klines`. `ranges` holds the series' known gap ranges (fill the
/// holes) when any exist, else a single default-lookback window ending "now" — see
/// [`plan_backfill_jobs`].
#[derive(Debug, Clone, PartialEq)]
pub struct BackfillJob {
    pub venue: String,
    pub symbol: String,
    pub interval: String,
    pub ranges: Vec<(i64, i64)>,
}

/// Map a Data-Manager grid selection to concrete backfill jobs: one [`BackfillJob`] per selected
/// series that is both `kind == "bar"` AND on a [`SUPPORTED_BACKFILL_VENUES`] venue, targeting
/// that series' gap ranges from `gaps` (fill the holes) when it has any, else a single
/// `[now_ms - default_lookback_ms, now_ms]` window (a fresh/never-backfilled series has no gap
/// history to target yet — a plain "since forever" lookback is the sensible default). Every
/// other selected series (a non-`"bar"` kind — quote/trade/book — or a venue outside
/// [`SUPPORTED_BACKFILL_VENUES`]) is counted in the returned skip count, never silently dropped
/// from the tally.
pub fn plan_backfill_jobs(
    selected: &BTreeSet<SeriesKey>,
    gaps: &GapMap,
    now_ms: i64,
    default_lookback_ms: i64,
) -> (Vec<BackfillJob>, usize) {
    plan_jobs_gated(selected, gaps, now_ms, default_lookback_ms, |venue| {
        SUPPORTED_BACKFILL_VENUES.contains(&venue)
    })
}

/// [`plan_backfill_jobs`] with the venue gate decided by the [`BackfillRoute`] (split-plane
/// REQ-9, GUI half):
///
/// - [`BackfillRoute::Local`] — identical to [`plan_backfill_jobs`] (the
///   [`SUPPORTED_BACKFILL_VENUES`] gate: only venues the GUI holds a local collector for), so the
///   local path's planning is byte-identical to before routing existed.
/// - [`BackfillRoute::Wire`] — NO client-side venue gate: which venues have collectors is the
///   SERVER's roster (`crates/vike-datahub/src/backfill.rs`'s `BackfillTable`), not this build's,
///   so an off-roster venue is still planned, sent, and answered by the server's own refusal text
///   naming ITS supported set — surfaced in the backfill status slot, never a silent client-side
///   drop that would misreport a capable server as unsupporting.
///
/// The `kind == "bar"` gate applies on EVERY route: the wire verb (like the local collectors) is
/// klines-only, so a quote/trade/book series has nothing to request of anyone — it stays a
/// counted skip.
pub fn plan_routed_backfill_jobs(
    selected: &BTreeSet<SeriesKey>,
    gaps: &GapMap,
    now_ms: i64,
    default_lookback_ms: i64,
    route: &BackfillRoute,
) -> (Vec<BackfillJob>, usize) {
    match route {
        BackfillRoute::Local => plan_backfill_jobs(selected, gaps, now_ms, default_lookback_ms),
        BackfillRoute::Wire { .. } => {
            plan_jobs_gated(selected, gaps, now_ms, default_lookback_ms, |_| true)
        }
    }
}

/// The shared planning fold: `venue_supported` is the ONE per-route difference (see
/// [`plan_routed_backfill_jobs`]); everything else — the `kind == "bar"` gate, the
/// gaps-else-default-lookback window choice, the skip tally — is route-independent.
fn plan_jobs_gated(
    selected: &BTreeSet<SeriesKey>,
    gaps: &GapMap,
    now_ms: i64,
    default_lookback_ms: i64,
    venue_supported: impl Fn(&str) -> bool,
) -> (Vec<BackfillJob>, usize) {
    let mut jobs = Vec::new();
    let mut skipped = 0usize;
    for key in selected {
        let supported = key.kind == "bar" && venue_supported(key.venue.as_str());
        let Some(interval) = (if supported { key.interval.clone() } else { None }) else {
            skipped += 1;
            continue;
        };
        let series_gaps = gaps.get(key).map(|v| v.as_slice()).unwrap_or(&[]);
        let ranges = if series_gaps.is_empty() {
            vec![(now_ms.saturating_sub(default_lookback_ms), now_ms)]
        } else {
            series_gaps.to_vec()
        };
        jobs.push(BackfillJob {
            venue: key.venue.clone(),
            symbol: key.symbol.clone(),
            interval,
            ranges,
        });
    }
    (jobs, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(venue: &str, symbol: &str, kind: &str, interval: Option<&str>) -> SeriesKey {
        SeriesKey {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            kind: kind.to_string(),
            interval: interval.map(str::to_string),
        }
    }

    #[test]
    fn a_supported_bar_series_with_a_gap_targets_the_gap_range() {
        let mut selected = BTreeSet::new();
        selected.insert(key("binance", "BTCUSDT", "bar", Some("1m")));
        let mut gaps = GapMap::new();
        gaps.insert(key("binance", "BTCUSDT", "bar", Some("1m")), vec![(100, 200), (400, 500)]);

        let (jobs, skipped) = plan_backfill_jobs(&selected, &gaps, 10_000, 1_000);
        assert_eq!(skipped, 0);
        assert_eq!(jobs.len(), 1);
        let job = &jobs[0];
        assert_eq!(job.venue, "binance");
        assert_eq!(job.symbol, "BTCUSDT");
        assert_eq!(job.interval, "1m");
        assert_eq!(job.ranges, vec![(100, 200), (400, 500)]);
    }

    #[test]
    fn a_supported_bar_series_with_no_gaps_falls_back_to_the_default_lookback() {
        let mut selected = BTreeSet::new();
        selected.insert(key("bybit", "ETHUSDT", "bar", Some("5m")));
        let gaps = GapMap::new(); // no entry at all — "never fetched"/"no gaps"

        let (jobs, skipped) = plan_backfill_jobs(&selected, &gaps, 10_000, 3_000);
        assert_eq!(skipped, 0);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].ranges, vec![(7_000, 10_000)]);
    }

    #[test]
    fn an_empty_gap_list_also_falls_back_to_the_default_lookback() {
        let mut selected = BTreeSet::new();
        selected.insert(key("okx", "BTC-USDT", "bar", Some("1h")));
        let mut gaps = GapMap::new();
        gaps.insert(key("okx", "BTC-USDT", "bar", Some("1h")), vec![]); // present but empty

        let (jobs, _skipped) = plan_backfill_jobs(&selected, &gaps, 10_000, 1_000);
        assert_eq!(jobs[0].ranges, vec![(9_000, 10_000)]);
    }

    #[test]
    fn a_non_bar_series_is_skipped_not_planned() {
        let mut selected = BTreeSet::new();
        selected.insert(key("binance", "BTCUSDT", "trade", None));
        let (jobs, skipped) = plan_backfill_jobs(&selected, &GapMap::new(), 10_000, 1_000);
        assert!(jobs.is_empty());
        assert_eq!(skipped, 1);
    }

    #[test]
    fn an_unsupported_venue_is_skipped_not_planned() {
        let mut selected = BTreeSet::new();
        selected.insert(key("dukascopy", "EURUSD", "bar", Some("1m")));
        let (jobs, skipped) = plan_backfill_jobs(&selected, &GapMap::new(), 10_000, 1_000);
        assert!(jobs.is_empty());
        assert_eq!(skipped, 1);
    }

    #[test]
    fn a_mixed_selection_plans_the_supported_ones_and_counts_the_rest_as_skipped() {
        let mut selected = BTreeSet::new();
        selected.insert(key("binance", "BTCUSDT", "bar", Some("1m"))); // planned
        selected.insert(key("binance", "BTCUSDT", "trade", None)); // skipped: non-bar
        selected.insert(key("deribit", "BTC-PERPETUAL", "bar", Some("1m"))); // skipped: unsupported venue
        selected.insert(key("okx", "ETH-USDT", "bar", Some("15m"))); // planned

        let (jobs, skipped) = plan_backfill_jobs(&selected, &GapMap::new(), 10_000, 1_000);
        assert_eq!(skipped, 2);
        let mut venues: Vec<&str> = jobs.iter().map(|j| j.venue.as_str()).collect();
        venues.sort_unstable();
        assert_eq!(venues, vec!["binance", "okx"]);
    }

    #[test]
    fn empty_selection_plans_nothing_and_skips_nothing() {
        let (jobs, skipped) = plan_backfill_jobs(&BTreeSet::new(), &GapMap::new(), 10_000, 1_000);
        assert!(jobs.is_empty());
        assert_eq!(skipped, 0);
    }

    // ── plan_routed_backfill_jobs (split-plane REQ-9, GUI half) ─────────────────────────────────

    fn wire() -> BackfillRoute {
        BackfillRoute::Wire { addr: "127.0.0.1:7878".to_string() }
    }

    #[test]
    fn the_local_route_plans_identically_to_the_unrouted_entry() {
        let mut selected = BTreeSet::new();
        selected.insert(key("binance", "BTCUSDT", "bar", Some("1m"))); // planned
        selected.insert(key("deribit", "BTC-PERPETUAL", "bar", Some("1m"))); // skipped: unsupported venue
        selected.insert(key("binance", "BTCUSDT", "trade", None)); // skipped: non-bar
        let mut gaps = GapMap::new();
        gaps.insert(key("binance", "BTCUSDT", "bar", Some("1m")), vec![(100, 200)]);

        let unrouted = plan_backfill_jobs(&selected, &gaps, 10_000, 1_000);
        let routed =
            plan_routed_backfill_jobs(&selected, &gaps, 10_000, 1_000, &BackfillRoute::Local);
        assert_eq!(routed, unrouted, "Local routing must not change the plan at all");
    }

    #[test]
    fn the_wire_route_keeps_an_off_roster_venue_so_the_server_answers_for_its_own_roster() {
        let mut selected = BTreeSet::new();
        selected.insert(key("deribit", "BTC-PERPETUAL", "bar", Some("1m")));

        let (jobs, skipped) =
            plan_routed_backfill_jobs(&selected, &GapMap::new(), 10_000, 1_000, &wire());
        assert_eq!(skipped, 0, "venue support is the server's question on the wire route");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].venue, "deribit");
    }

    #[test]
    fn the_wire_route_still_skips_a_non_bar_series() {
        let mut selected = BTreeSet::new();
        selected.insert(key("binance", "BTCUSDT", "trade", None));

        let (jobs, skipped) =
            plan_routed_backfill_jobs(&selected, &GapMap::new(), 10_000, 1_000, &wire());
        assert!(
            jobs.is_empty(),
            "the wire verb is klines-only — nothing to request for a trade series"
        );
        assert_eq!(skipped, 1);
    }

    #[test]
    fn the_wire_route_targets_the_same_gap_ranges_the_local_route_would() {
        let mut selected = BTreeSet::new();
        selected.insert(key("okx", "BTC-USDT", "bar", Some("1h")));
        let mut gaps = GapMap::new();
        gaps.insert(key("okx", "BTC-USDT", "bar", Some("1h")), vec![(100, 200), (400, 500)]);

        let (jobs, _) = plan_routed_backfill_jobs(&selected, &gaps, 10_000, 1_000, &wire());
        assert_eq!(jobs[0].ranges, vec![(100, 200), (400, 500)]);
    }
}
