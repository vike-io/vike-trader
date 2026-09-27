//! IBKR historical backfill — the pure planning half (window sizing + paging + commit keys),
//! fixture-tested with no network. The `ibkr_backfill` bin drives `vike_ibkr::HistoricalFetcher`
//! over these plans and ingests each window via `HistStore::append_bars`. Ports the historical
//! half of docs/superpowers/specs/2026-07-15-vike-ibkr-market-data-design.md §3. Mirrors the
//! eod/pmxt collectors (pure parser/planner fixture-tested, one file).

use vike_ibkr::Window;

/// Per-request paging window sized to the bar interval — conservative under IB's historical
/// per-request limits (daily allows ~1y/req; finer bars need shorter windows).
///
/// CRITICAL: the window unit MUST be **days** so its span exactly equals [`step_ms_for`]'s stride.
/// IB interprets calendar units (`months(1)`/`years(1)`) against the calendar — a "1 M" window is
/// 28-31 days and a "1 Y" is 365-366 — which would NOT tile with a fixed-ms stride, leaving gaps
/// (short months) or duplicate boundary bars (leap years; the store does not dedup by ts). Fixed
/// day-durations (`"365 D"`/`"30 D"`) make each window's IB span == `step_ms` to the millisecond.
pub fn window_for(interval: &str) -> Option<Window> {
    match interval {
        "1d" => Some(Window::days(365)),
        "1h" => Some(Window::days(30)),
        "5m" => Some(Window::days(7)),
        "1m" => Some(Window::days(1)),
        _ => None,
    }
}

/// The same paging window expressed in epoch-ms (for `plan_window_ends`). Kept EXACTLY in lockstep
/// with [`window_for`]'s day count × 86_400_000 so consecutive `.ending()` requests tile without
/// gaps or overlaps (see `window_and_step_agree_in_magnitude`).
pub fn step_ms_for(interval: &str) -> Option<i64> {
    match interval {
        "1d" => Some(365 * 86_400_000),
        "1h" => Some(30 * 86_400_000),
        "5m" => Some(7 * 86_400_000),
        "1m" => Some(86_400_000),
        _ => None,
    }
}

/// Descending per-request END timestamps covering `[head_ms, end_ms]` in `step_ms` strides. The
/// first entry is always `end_ms`; each subsequent end is `step_ms` earlier; the walk stops once a
/// stride would start at/below `head_ms` (the last window's fetch still covers down to head).
pub fn plan_window_ends(head_ms: i64, end_ms: i64, step_ms: i64) -> Vec<i64> {
    let mut ends = Vec::new();
    if end_ms <= head_ms || step_ms <= 0 {
        return ends;
    }
    let mut cur = end_ms;
    while cur > head_ms {
        ends.push(cur);
        cur -= step_ms;
    }
    ends
}

/// Idempotency key for one window's `append_bars` batch. Re-running the same (venue,symbol,
/// interval,what,window_end) is a store no-op. NEVER dedups by row value. `what` is part of the key
/// so a re-run with a different series type (e.g. `bid` after `trades`) actually writes rather than
/// silently colliding on the key of the earlier run. NOTE: the store series discriminator is only
/// `(venue,symbol,interval,kind=bar)` — so different `what` values write into the SAME series; a
/// backfill run should pick one `--what` per (symbol,interval) to avoid mixing trade/quote bars.
pub fn backfill_commit_key(
    venue: &str,
    symbol: &str,
    interval: &str,
    what: &str,
    window_end_ms: i64,
) -> String {
    format!("ibkr-backfill:{venue}:{symbol}:{interval}:{what}:{window_end_ms}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_and_step_agree_on_known_intervals() {
        assert!(window_for("1d").is_some() && step_ms_for("1d") == Some(365 * 86_400_000));
        assert!(window_for("1h").is_some() && step_ms_for("1h").is_some());
        assert!(window_for("30m").is_none() && step_ms_for("30m").is_none());
    }

    #[test]
    fn window_and_step_agree_in_magnitude() {
        // The IB window span MUST equal the ms stride exactly, else consecutive `.ending()`
        // requests gap or overlap (the calendar-vs-fixed-ms bug). Both must be day-units.
        for iv in ["1d", "1h", "5m", "1m"] {
            let w = window_for(iv).unwrap();
            let step = step_ms_for(iv).unwrap();
            assert_eq!(w.unit, 'D', "{iv}: window must be a day-unit to match the ms stride");
            assert_eq!(
                w.value as i64 * 86_400_000,
                step,
                "{iv}: window span (days) must equal step_ms exactly"
            );
        }
    }

    #[test]
    fn plan_walks_back_to_head_descending() {
        // 3 strides of 10 between head=0 and end=25 → ends [25,15,5]; the 5-window still covers <=0.
        let ends = plan_window_ends(0, 25, 10);
        assert_eq!(ends, vec![25, 15, 5]);
        // ascending sanity: each end is strictly greater than the next.
        assert!(ends.windows(2).all(|w| w[0] > w[1]));
    }

    #[test]
    fn plan_empty_when_end_at_or_below_head() {
        assert!(plan_window_ends(100, 100, 10).is_empty());
        assert!(plan_window_ends(100, 50, 10).is_empty());
        assert!(plan_window_ends(0, 25, 0).is_empty(), "non-positive step → empty");
    }

    #[test]
    fn commit_key_is_stable_window_and_what_specific() {
        let a = backfill_commit_key("ibkr", "AAPL.SMART.USD", "1d", "trades", 1_700_000_000_000);
        let b = backfill_commit_key("ibkr", "AAPL.SMART.USD", "1d", "trades", 1_700_000_000_000);
        let c = backfill_commit_key("ibkr", "AAPL.SMART.USD", "1d", "trades", 1_699_000_000_000);
        let d = backfill_commit_key("ibkr", "AAPL.SMART.USD", "1d", "bid", 1_700_000_000_000);
        assert_eq!(a, b);
        assert_ne!(a, c, "different window end → different key");
        assert_ne!(a, d, "different what → different key (no silent no-op on a re-run)");
    }
}
