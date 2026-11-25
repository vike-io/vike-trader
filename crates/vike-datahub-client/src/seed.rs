//! **The bounds on [`Request::SeedSeries`](crate::proto::Request::SeedSeries) that BOTH ends need**
//! — the window the server fetches, the intervals it will fetch, and the two validators that stand
//! between a client-named string and a venue URL. Declared ungated: a default `vike-datahub` build
//! must DECODE the verb in order to answer or refuse it cleanly.
//!
//! # Why the constants live HERE and not in `vike-datahub`
//!
//! A client cannot guard a rule the server does not know, nor the reverse (as with
//! [`crate::market`]'s `MARKET_DATA_MAX_SYMBOL_BYTES` / `validate_market_data_symbol`).
//! `crates/vike-datahub/src/server/seed_series.rs`'s `seed_series_verb` and
//! [`crate::DatahubClient::seed_series`] both call the functions below, so a refusal seen locally is
//! the one the server would give: the local check is for the MESSAGE, the server's re-check at its
//! own door, before it dispatches to a venue, is the enforcement. The SERVER's own constants (the
//! token bucket, the per-process series cap) stay in `crates/vike-datahub/src/seed.rs`: a client
//! that knew them could only mis-predict them.
//!
//! # ⚠ The validators are a SECURITY boundary, not hygiene
//!
//! `crates/bridges/binance/src/family/history/klines.rs`'s `klines_url` interpolates `symbol` and `interval`
//! into its query string with **no allowlist and no percent-encoding** (bybit and okx validate in
//! their own code tables: `crates/bridges/bybit/src/history.rs`'s `interval_code`,
//! `crates/bridges/okx/src/history.rs`'s `bar_code`). Those strings now arrive from an OBSERVE client,
//! so [`validate_seed_interval`] and `vike_hub_protocol::wire_ident::validate_seed_symbol` run at the server's door before a venue
//! is dispatched to. `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md` records them as a
//! STOPGAP that the deferred per-venue interval table replaces.

use std::fmt::Write as _;

/// How many bars ONE seed fetches — the whole of the "how much history" question; the client names
/// no part of it (the cost rule
/// `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md`'s decision 1 rests on).
///
/// **600 is ONE page on binance and bybit at ANY interval**, so a `1m` chart and a `1d` chart cost
/// the same: `crates/bridges/binance/src/family/history/klines.rs`'s `MAX_LIMIT` and
/// `crates/bridges/bybit/src/history.rs`'s `MAX_LIMIT` are both 1000 (`/klines?limit=1000` is weight 2
/// on binance spot and 5 on `fapi`, per `crates/vike-model/src/rate_limits.rs`).
///
/// ⚠ **SIX paced pages on OKX** (`crates/bridges/okx/src/history.rs`'s `MAX_LIMIT` is 100), accepted:
/// OKX publishes no weight counter, its pager paces itself, and a per-venue window is the deferred
/// per-venue table rather than this constant. Aster pages through the binance family pager (one
/// page, very likely); deribit and hyperliquid are UNMEASURED here.
///
/// Comfortably above a chart's visible width, and far below the chart read's default depth
/// (`vike_config::preferences::DEFAULT_CHART_HISTORY_BARS`, the `preferences.chart_history_bars`
/// row that `vike_app_core::data::stored::read_range` takes as a parameter), so with no row the
/// read after a seed trims nothing. An operator who sets that row below this number has asked for
/// a shallower chart than the seed fetched.
pub const SEED_BARS: u32 = 600;

/// The intervals a seed will fetch. **The client's string is checked against THIS and nothing else
/// before a venue is dispatched to** (see the module doc).
///
/// The rule: (1) what every venue in `crates/vike-datahub/src/backfill.rs`'s `real_backfill_table`
/// serves (the code tables are bybit's `interval_code` and okx's `bar_code`), intersected with
/// (2) what `vike_marketdata::interval::interval_ms` can parse — an interval with no bar width has no window
/// and no partition.
///
/// ⚠ **`4h` stays although deribit refuses it** (`crates/bridges/deribit/src/history.rs`'s
/// `resolution_code` has no 4h): removing it would take a step from the five venues that serve it
/// to spare deribit an error it answers cleanly from its OWN table before a request leaves the box
/// (a [`crate::proto::Response::Error`] with nothing written), not the silent wrong bucket rule 2
/// prevents.
///
/// ⚠ **`1s` is EXCLUDED though binance serves it:** bybit and okx refuse it from their own tables,
/// so admitting it would make this set a per-venue answer (and 600 bars of `1s` is ten minutes).
/// `1w`/`1M` have no width in `interval_ms`'s grammar (rule 2);
/// `crates/vike-datahub/src/server/backfill.rs`'s `backfill_verb` refuses the same family on the
/// Control-scoped sibling, and this set stays the narrower allowlist because an OBSERVE client names
/// the string.
///
/// A STOPGAP: the per-venue interval table
/// `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md` defers replaces it.
pub const SEED_INTERVALS: [&str; 11] =
    ["1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "6h", "12h", "1d"];

/// The interval rule: membership in [`SEED_INTERVALS`], exact and case-SENSITIVE — `1M` and `1m`
/// are a calendar month and a minute at every venue, so a case-insensitive match would silently turn
/// one into the other. The message names the whole permitted set, so an operator who sent `1s` sees
/// that the SET refused it rather than the venue.
pub fn validate_seed_interval(interval: &str) -> Result<(), String> {
    if SEED_INTERVALS.contains(&interval) {
        return Ok(());
    }
    let mut msg = format!(
        "interval {interval:?} is not one this server will seed. It is refused HERE, before any \
         venue is dispatched to. Permitted: ["
    );
    for (i, iv) in SEED_INTERVALS.iter().enumerate() {
        if i > 0 {
            msg.push_str(", ");
        }
        let _ = write!(msg, "{iv}");
    }
    msg.push_str(
        "]. The set is the intersection of what every venue in this server's collector table \
         serves and what the store's own interval vocabulary can parse, so it is narrower than any \
         one venue — `1s` is served by binance alone and is excluded for that reason.",
    );
    Err(msg)
}

/// The inclusive epoch-ms window one seed fetches: the last [`SEED_BARS`] bars ending `now_ms`.
///
/// **The SERVER computes this** from its own clock and constant — the request carries no range (the
/// term 0058 turns on). Exposed so a client can report what it is about to be given, and so the two
/// ends cannot disagree about what "600 bars" meant.
///
/// `None` for an interval outside [`SEED_INTERVALS`] or one `vike_marketdata::interval::interval_ms` cannot
/// parse — the same set by construction; the redundant check stays because this is the function
/// that would otherwise multiply by a garbage width.
pub fn seed_range(interval: &str, now_ms: i64) -> Option<(i64, i64)> {
    if !SEED_INTERVALS.contains(&interval) {
        return None;
    }
    let step = vike_marketdata::interval::bar_step_ms(interval)?;
    Some((now_ms.saturating_sub(step.saturating_mul(i64::from(SEED_BARS))), now_ms))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;

    #[test]
    fn every_permitted_interval_has_a_bar_width_the_store_can_parse() {
        // Rule 2 of `SEED_INTERVALS`' derivation, asserted: otherwise `seed_range` would answer
        // `None` for an interval `validate_seed_interval` had just accepted.
        for iv in SEED_INTERVALS {
            let ms = vike_marketdata::interval::interval_ms(iv);
            assert_matches!(ms, Some(n) if n > 0, "{iv} has no positive bar width: {ms:?}");
            assert!(seed_range(iv, 1_700_000_000_000).is_some(), "{iv} has no window");
        }
    }

    /// Row 2's table on the set an observe client can name: the hand-written guard every caller
    /// carried, the width home and the writers' check answer one number per interval.
    #[test]
    fn every_permitted_interval_has_one_width_through_every_guard() {
        use vike_marketdata::interval::{bar_step_ms, canonical_bar_step_ms, interval_ms};
        for iv in SEED_INTERVALS {
            let by_hand = interval_ms(iv).filter(|&ms| ms > 0);
            assert!(by_hand.is_some(), "{iv}");
            assert_eq!(bar_step_ms(iv), by_hand, "{iv}");
            assert_eq!(canonical_bar_step_ms(iv), by_hand, "{iv}");
        }
    }

    #[test]
    fn the_permitted_set_is_sorted_by_bar_width_and_free_of_duplicates() {
        // Not cosmetic: the set is rendered into a refusal an operator reads.
        let widths: Vec<i64> = SEED_INTERVALS
            .iter()
            .map(|iv| vike_marketdata::interval::interval_ms(iv).unwrap())
            .collect();
        assert!(widths.windows(2).all(|w| w[0] < w[1]), "not strictly increasing: {widths:?}");
    }

    #[test]
    fn the_intervals_no_venue_or_no_store_can_serve_are_refused_by_name() {
        // `1s`: binance serves it, bybit and okx refuse it in their own code tables (MEASURED).
        // `1w`/`1M`: no bar width in `interval_ms`'s grammar. `1M` also proves case sensitivity.
        for bad in ["1s", "1w", "1M", "1mo", "60", "", "1h ", " 1h", "1H"] {
            let err = validate_seed_interval(bad).expect_err("{bad} must be refused");
            assert!(err.contains("before any venue"), "{bad}: {err}");
            assert!(err.contains("Permitted"), "{bad}: {err}");
        }
    }

    #[test]
    fn the_window_is_exactly_seed_bars_wide_whatever_the_interval() {
        // The property that makes opening a 1m chart and a 1d chart cost the same request count on
        // binance and bybit — the reasoning `SEED_BARS`' doc carries.
        const NOW: i64 = 1_700_000_000_000;
        for iv in SEED_INTERVALS {
            let (start, end) = seed_range(iv, NOW).expect("permitted");
            let step = vike_marketdata::interval::interval_ms(iv).unwrap();
            assert_eq!(end, NOW, "{iv}");
            assert_eq!(end - start, step * i64::from(SEED_BARS), "{iv}");
        }
    }

    #[test]
    fn a_refused_interval_has_no_window() {
        assert_eq!(seed_range("1s", 1_700_000_000_000), None);
        assert_eq!(seed_range("not-an-interval", 1_700_000_000_000), None);
    }
}
