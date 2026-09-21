//! Bulk "Backfill" job planning (dm-bulk-backfill) — the pure fn that maps a Data-Manager grid
//! selection + the app's known gap ranges into a list of concrete kline-backfill jobs, factored
//! out of `vike-app`'s `App::maybe_spawn_stored_backfill` so it's unit-testable without an
//! `App`/`eframe::CreationContext` (same rationale as `feed_lifecycle`'s pure gates — see that
//! module's doc).
//!
//! **The one client-side gate is `kind == "bar"`**: the backfill verb is klines-only, so a
//! quote/trade/book series has nothing to request of anyone and is counted as skipped, never
//! silently dropped from the tally the GUI surfaces ("backfilled N series, skipped M unsupported").
//! WHICH VENUES have collectors is the SERVER's question, answered by the datahub's own roster
//! (`vike_backfill::kline_source::KLINE_SOURCES`, folded into
//! `crates/vike-datahub/src/backfill.rs`'s `real_backfill_table`) — so an off-roster venue is
//! still planned, sent, and answered by the server's refusal text naming ITS supported set, never
//! by a silent client-side drop that would misreport a capable server as unsupporting.
//!
//! ⚠ **This module used to carry a venue roster of its own — `SUPPORTED_BACKFILL_VENUES`, the
//! third of the four hand-copied kline rosters
//! `docs/decisions/0059-bars-and-ticks-for-every-venue-are-two-asks-not-one.md` counted — and
//! Phase 3 DELETED it rather than folding it into the one registry.** Two reasons, and the second
//! is the load-bearing one:
//!
//! * It was a DEAD GATE. Its only consumer was this planner's `BackfillRoute::Local` arm, and the
//!   Local executor (`crates/vike-desktop/src/app_methods.rs`'s `spawn_stored_backfill_local`) is a
//!   tombstone that sets a status string and runs nothing; the grid itself only ever populates in
//!   remote mode, so the arm is unreachable in practice. That is why 0059 Phase 2 deliberately
//!   added NO row here when it dispatched aster, deribit and hyperliquid: three names on a dead
//!   gate change nothing observable while reading, in review, as coverage.
//! * Folding it in was FORBIDDEN, not merely unnecessary. A shared registry lives in
//!   `vike-backfill` (layer 55) and this crate is layer 80 with NO `vike-backfill` edge —
//!   deliberately, since rulings 1 and 2 of the desktop split removed it along with the local core.
//!   Reaching the registry from here would drag `venue-backfill`'s seven bridge crates back into
//!   the GUI's dependency tree to feed a predicate nothing consults.
//!
//! ⚠ **The one behaviour that moved, stated because it is the only thing this deletion changed:**
//! on the Local route a selection of BAR series outside the old three now plans jobs instead of
//! being skipped, so the status slot shows the tombstone's *"Backfill unavailable: this build has
//! no local store engine — set config.datahub_addr…"* rather than *"0 series queued, N skipped
//! (unsupported)"*. Both are refusals and neither backfills; the surviving one names the fix and
//! is true, while the one it replaced blamed the venue for a missing engine. This module's own
//! `the_local_and_wire_routes_now_plan_identically` is where that is pinned, with `deribit` as the
//! discriminator — the exact venue the old Local gate skipped and the old Wire arm planned.

use std::collections::BTreeSet;

use vike_data_manager::{GapMap, SeriesKey};

/// One series' worth of backfill work: one or more `[start_ms, end_ms]` windows (inclusive
/// epoch-ms, same convention as `stored_gaps`/`DataFusionHist::series_gaps`) for the datahub's
/// backfill verb to fetch+ingest through its own kline registry. `ranges` holds the series' known
/// gap ranges (fill the holes) when any exist, else a single default-lookback window ending "now"
/// — see [`plan_backfill_jobs`].
#[derive(Debug, Clone, PartialEq)]
pub struct BackfillJob {
    pub venue: String,
    pub symbol: String,
    pub interval: String,
    pub ranges: Vec<(i64, i64)>,
}

/// Map a Data-Manager grid selection to concrete backfill jobs: one [`BackfillJob`] per selected
/// series whose `kind == "bar"`, targeting that series' gap ranges from `gaps` (fill the holes)
/// when it has any, else a single `[now_ms - default_lookback_ms, now_ms]` window (a
/// fresh/never-backfilled series has no gap history to target yet — a plain "since forever"
/// lookback is the sensible default). Every other selected series — a non-`"bar"` kind
/// (quote/trade/book) — is counted in the returned skip count, never silently dropped from the
/// tally.
///
/// **There is no client-side VENUE gate**, on any route. Which venues have collectors is the
/// server's roster, and the module doc carries why asking that question here was both dead and
/// forbidden. It took a [`crate::backfill_route::BackfillRoute`] argument until 0059 Phase 3 —
/// with the venue gate gone the two arms were the same fold, and a parameter that cannot change an
/// answer is a parameter that invites somebody to make it change one. The route still decides the
/// EXECUTOR, in `crates/vike-desktop/src/app_methods.rs`'s `maybe_spawn_stored_backfill`; it just
/// no longer decides the PLAN.
pub fn plan_backfill_jobs(
    selected: &BTreeSet<SeriesKey>,
    gaps: &GapMap,
    now_ms: i64,
    default_lookback_ms: i64,
) -> (Vec<BackfillJob>, usize) {
    let mut jobs = Vec::new();
    let mut skipped = 0usize;
    for key in selected {
        let supported = key.kind == "bar";
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

    /// ⚠ **This test used to be `an_unsupported_venue_is_skipped_not_planned` and asserted the
    /// opposite**, over `dukascopy`. It is the ONE behavioural pin of what 0059 Phase 3's deletion
    /// of `SUPPORTED_BACKFILL_VENUES` changed: a bar series on a venue the GUI never held a
    /// collector for is now PLANNED and sent, and the server answers for its own roster. The module
    /// doc carries why that is the better refusal.
    ///
    /// `dukascopy` is kept as the subject deliberately — it is not on the datahub's roster either,
    /// so this is the strongest form of the claim: even a venue nothing can serve is the SERVER's
    /// refusal to make.
    #[test]
    fn a_venue_the_client_cannot_judge_is_planned_and_left_to_the_server() {
        let mut selected = BTreeSet::new();
        selected.insert(key("dukascopy", "EURUSD", "bar", Some("1m")));
        let (jobs, skipped) = plan_backfill_jobs(&selected, &GapMap::new(), 10_000, 1_000);
        assert_eq!(skipped, 0, "venue support is not a question this build can answer");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].venue, "dukascopy");
    }

    #[test]
    fn a_mixed_selection_plans_every_bar_series_and_counts_the_rest_as_skipped() {
        let mut selected = BTreeSet::new();
        selected.insert(key("binance", "BTCUSDT", "bar", Some("1m"))); // planned
        selected.insert(key("binance", "BTCUSDT", "trade", None)); // skipped: non-bar
        // planned since 0059 Phase 3 — the SERVER has dispatched deribit since Phase 2, and this
        // build no longer keeps a roster of its own to disagree with it.
        selected.insert(key("deribit", "BTC-PERPETUAL", "bar", Some("1m")));
        selected.insert(key("okx", "ETH-USDT", "bar", Some("15m"))); // planned

        let (jobs, skipped) = plan_backfill_jobs(&selected, &GapMap::new(), 10_000, 1_000);
        assert_eq!(skipped, 1, "only the non-bar series is a client-side skip");
        let mut venues: Vec<&str> = jobs.iter().map(|j| j.venue.as_str()).collect();
        venues.sort_unstable();
        assert_eq!(venues, vec!["binance", "deribit", "okx"]);
    }

    #[test]
    fn empty_selection_plans_nothing_and_skips_nothing() {
        let (jobs, skipped) = plan_backfill_jobs(&BTreeSet::new(), &GapMap::new(), 10_000, 1_000);
        assert!(jobs.is_empty());
        assert_eq!(skipped, 0);
    }

    // ── what the route USED to change, and no longer does (0059 Phase 3) ────────────────────────

    /// **The deletion's own pin.** There were two entry points — `plan_backfill_jobs` (the Local
    /// gate, over `SUPPORTED_BACKFILL_VENUES`) and `plan_routed_backfill_jobs` (which passed
    /// `|_| true` on the Wire arm) — and this test is their successor: one planner, and a selection
    /// that would have split on the route is planned the same way whichever route is about to run
    /// it.
    ///
    /// ⚠ It is NOT vacuous now that the route argument is gone, because the SELECTION is the
    /// discriminator: `deribit` is precisely a venue the old Local gate skipped and the old Wire
    /// arm planned. If the venue gate were ever reintroduced here, this test is what fails.
    #[test]
    fn the_local_and_wire_routes_now_plan_identically() {
        let mut selected = BTreeSet::new();
        selected.insert(key("binance", "BTCUSDT", "bar", Some("1m")));
        // The discriminator: skipped by the old Local gate, planned by the old Wire arm.
        selected.insert(key("deribit", "BTC-PERPETUAL", "bar", Some("1m")));
        selected.insert(key("binance", "BTCUSDT", "trade", None)); // skipped on every route: non-bar
        let mut gaps = GapMap::new();
        gaps.insert(key("binance", "BTCUSDT", "bar", Some("1m")), vec![(100, 200)]);

        let (jobs, skipped) = plan_backfill_jobs(&selected, &gaps, 10_000, 1_000);
        assert_eq!(skipped, 1, "the non-bar series is the only client-side skip left");
        let mut venues: Vec<&str> = jobs.iter().map(|j| j.venue.as_str()).collect();
        venues.sort_unstable();
        assert_eq!(venues, vec!["binance", "deribit"]);
        assert_eq!(jobs.iter().find(|j| j.venue == "binance").unwrap().ranges, vec![(100, 200)]);
    }

    #[test]
    fn an_off_roster_venue_is_planned_so_the_server_answers_for_its_own_roster() {
        let mut selected = BTreeSet::new();
        selected.insert(key("deribit", "BTC-PERPETUAL", "bar", Some("1m")));

        let (jobs, skipped) = plan_backfill_jobs(&selected, &GapMap::new(), 10_000, 1_000);
        assert_eq!(skipped, 0, "venue support is the server's question");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].venue, "deribit");
    }

    #[test]
    fn a_non_bar_series_is_still_skipped() {
        let mut selected = BTreeSet::new();
        selected.insert(key("binance", "BTCUSDT", "trade", None));

        let (jobs, skipped) = plan_backfill_jobs(&selected, &GapMap::new(), 10_000, 1_000);
        assert!(
            jobs.is_empty(),
            "the wire verb is klines-only — nothing to request for a trade series"
        );
        assert_eq!(skipped, 1);
    }

    #[test]
    fn an_off_roster_venue_targets_its_gap_ranges_like_any_other() {
        let mut selected = BTreeSet::new();
        selected.insert(key("okx", "BTC-USDT", "bar", Some("1h")));
        let mut gaps = GapMap::new();
        gaps.insert(key("okx", "BTC-USDT", "bar", Some("1h")), vec![(100, 200), (400, 500)]);

        let (jobs, _) = plan_backfill_jobs(&selected, &gaps, 10_000, 1_000);
        assert_eq!(jobs[0].ranges, vec![(100, 200), (400, 500)]);
    }
}
