//! The up/down window chain — Polymarket's short-horizon up/down markets as a deterministic CHAIN of
//! fixed-length `window_ts` buckets. RUST-NATIVE (no Python twin — the oracle app has no cockpit).
//!
//! Each window opens on a boundary (`open = now - now % window`) and closes `window` seconds later,
//! then the next one takes over. This is the pure, unit-tested i64-millisecond core
//! ([`window_open_ms`], [`window_close_ms`], [`chain_windows`], and the `M:SS` countdown every cockpit
//! clock prints) and the per-frame [`CockpitUpDownWindowCard`] a window's card is drawn from. The
//! rail ([`super::chain_rail`]) and the Price-to-Beat header ([`super::price_to_beat`]) paint over it.

/// The current window's countdown turns the status red inside this many ms of the close.
const URGENT_MS: i64 = 10_000;

// ---------------------------------------------------------------------------
// Pure window math (unit-tested) — no egui, so the chain law is verifiable.
// ---------------------------------------------------------------------------

/// One window in the chain: the open boundary and its close, both epoch-millisecond timestamps.
/// `close_ms` is `open_ms + window` and is the START of the next window (half-open `[open, close)`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CockpitUpDownWindow {
    pub open_ms: i64,
    pub(crate) close_ms: i64,
}

/// Floor `timestamp` (epoch ms) to its window-open boundary — the up/down window law,
/// `vike_model::fair::updown_window_open_ms` (Polymarket's `open = now - now % window`, toward −∞
/// for a negative timestamp). `None` for a `window_secs` that is not positive: there is no window,
/// as the catalog's discovery answers (this rail clamped such a window to 1 s until 2026-11-18).
pub fn window_open_ms(timestamp: i64, window_secs: i64) -> Option<i64> {
    vike_model::fair::updown_window_open_ms(timestamp, window_secs * 1000)
}

/// The close (= next open) of a window that opened at `open_ms`, epoch ms; `None` for a
/// `window_secs` that is not positive, as [`window_open_ms`].
pub fn window_close_ms(open_ms: i64, window_secs: i64) -> Option<i64> {
    (window_secs > 0).then(|| open_ms + window_secs * 1000)
}

/// The chain a scalper watches: the window CURRENT at `now` plus the next `n`, in order
/// (`n + 1` entries total), and none for a `window_secs` that is not positive. Boundaries are
/// deterministic, so this is pure arithmetic off `now`.
pub fn chain_windows(now: i64, window_secs: i64, n: usize) -> Vec<CockpitUpDownWindow> {
    let window_ms = window_secs * 1000;
    let Some(first) = window_open_ms(now, window_secs) else {
        return Vec::new();
    };
    (0..=n)
        .map(|i| {
            let open = first + i as i64 * window_ms;
            CockpitUpDownWindow { open_ms: open, close_ms: open + window_ms }
        })
        .collect()
}

/// Format a remaining duration as `M:SS`, flooring to whole seconds and clamping any negative
/// (already-closed) value to `0:00`. Minutes are unpadded, seconds zero-padded to two digits.
pub(crate) fn format_countdown(ms_remaining: i64) -> String {
    let secs = ms_remaining.max(0) / 1000;
    let m = secs / 60;
    let s = secs % 60;
    format!("{m}:{s:02}")
}

/// Is a window with `ms_remaining` to close in its final stretch (the countdown turns red)?
/// True for the closing window over the last [`URGENT_MS`] down to and including the boundary;
/// an already-closed window (negative remaining) is not urgent.
pub(super) fn is_urgent(ms_remaining: i64) -> bool {
    (0..=URGENT_MS).contains(&ms_remaining)
}

/// Compact volume label: `k`/`M` scaled, matching the terminal's dense style. KEPT local, not
/// `vike_ui_theme::fmt::format_compact`, which diverges: uppercase `K`, trims trailing zeros
/// ("2M" vs our "2.0M") and has a `B` tier ("7.1B" vs our "7100.0M") — pinned in the tests.
pub(super) fn format_volume(v: f64) -> String {
    let a = v.abs();
    if a >= 1_000_000.0 {
        format!("{:.1}M", v / 1_000_000.0)
    } else if a >= 1_000.0 {
        format!("{:.1}k", v / 1_000.0)
    } else {
        format!("{v:.0}")
    }
}

/// One window's live snapshot, handed to the rail per frame. Built by the app from its market
/// state (odds/volume are already resolved for the window opening at `open_ms`). Holds f64s, so
/// deliberately NOT `PartialEq`/`Eq`.
#[derive(Clone, Copy, Debug)]
pub struct CockpitUpDownWindowCard {
    /// window-open boundary, epoch ms (matches [`CockpitUpDownWindow::open_ms`])
    pub open_ms: i64,
    /// YES/Up price (0..1), if known
    pub up_price: Option<f64>,
    /// NO/Down price (0..1), if known
    pub down_price: Option<f64>,
    /// traded volume for this window, if known
    pub volume: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const W5M: i64 = 300; // a 5-minute up/down window, in seconds
    const W5M_MS: i64 = 300_000;

    #[test]
    fn window_open_floors_to_boundary() {
        // exactly on a boundary stays put
        assert_eq!(window_open_ms(W5M_MS, W5M), Some(W5M_MS));
        assert_eq!(window_open_ms(0, W5M), Some(0));
        // 5s past the boundary floors back to it
        assert_eq!(window_open_ms(W5M_MS + 5_000, W5M), Some(W5M_MS));
        // one ms short of the next boundary is still the previous window
        assert_eq!(window_open_ms(W5M_MS - 1, W5M), Some(0));
        // a large epoch-ms timestamp: the open is a multiple of the window and within one window
        let timestamp = 1_724_000_123_456;
        let open = window_open_ms(timestamp, W5M).unwrap();
        assert_eq!(open % W5M_MS, 0);
        assert!(open <= timestamp && timestamp - open < W5M_MS);
    }

    #[test]
    fn window_open_negative_timestamp_floors_toward_neg_infinity() {
        // the floor goes toward −∞: −1ms belongs to the window [−300000, 0)
        assert_eq!(window_open_ms(-1, W5M), Some(-W5M_MS));
        assert_eq!(window_open_ms(-W5M_MS, W5M), Some(-W5M_MS));
        let open = window_open_ms(-123_456, W5M).unwrap();
        assert!(open <= -123_456 && -123_456 < window_close_ms(open, W5M).unwrap());
    }

    #[test]
    fn window_close_is_open_plus_window() {
        assert_eq!(window_close_ms(0, W5M), Some(W5M_MS));
        assert_eq!(window_close_ms(W5M_MS, W5M), Some(2 * W5M_MS));
        // close of window k == open of window k+1
        let open = window_open_ms(1_724_000_123_456, W5M).unwrap();
        assert_eq!(window_close_ms(open, W5M), Some(open + W5M_MS));
    }

    /// The edge the catalog's discovery decides (`vike_model::fair::updown_window_open_ms`): a
    /// window that is not a positive length has no open, no close and no chain. The rail clamped it
    /// to 1 s until 2026-11-18 (`window_open_ms(1_500, 0)` was 1_000); 1 s is now the shortest
    /// window, asked for by name.
    #[test]
    fn a_window_that_is_not_a_positive_length_has_no_open_close_or_chain() {
        for secs in [0, -1, -W5M] {
            assert_eq!(window_open_ms(1_500, secs), None, "{secs}");
            assert_eq!(window_close_ms(1_000, secs), None, "{secs}");
            assert!(chain_windows(1_500, secs, 4).is_empty(), "{secs}");
        }
        assert_eq!(window_open_ms(1_500, 1), Some(1_000));
        assert_eq!(window_close_ms(1_000, 1), Some(2_000));
        assert_eq!(
            chain_windows(1_500, 1, 1),
            [
                CockpitUpDownWindow { open_ms: 1_000, close_ms: 2_000 },
                CockpitUpDownWindow { open_ms: 2_000, close_ms: 3_000 }
            ]
        );
    }

    #[test]
    fn chain_windows_produces_current_plus_n() {
        let now = W5M_MS + 42_000; // 42s into the second window
        let chain = chain_windows(now, W5M, 4);
        // current + 4 = 5 entries
        assert_eq!(chain.len(), 5);
        // first is the window current at `now`
        assert_eq!(chain[0].open_ms, W5M_MS);
        assert_eq!(chain[0].close_ms, 2 * W5M_MS);
        // strictly ascending, contiguous: each close is the next open, spaced one window apart
        for pair in chain.windows(2) {
            assert_eq!(pair[0].close_ms, pair[1].open_ms);
            assert_eq!(pair[1].open_ms - pair[0].open_ms, W5M_MS);
        }
        // last window opens 4 windows after the first
        assert_eq!(chain[4].open_ms, W5M_MS + 4 * W5M_MS);
    }

    #[test]
    fn chain_windows_n_zero_is_just_the_current_window() {
        let chain = chain_windows(W5M_MS + 1, W5M, 0);
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0], CockpitUpDownWindow { open_ms: W5M_MS, close_ms: 2 * W5M_MS });
    }

    #[test]
    fn format_countdown_minutes_seconds() {
        assert_eq!(format_countdown(300_000), "5:00");
        assert_eq!(format_countdown(125_400), "2:05"); // floors 125.4s → 2:05
        assert_eq!(format_countdown(65_000), "1:05");
        assert_eq!(format_countdown(60_000), "1:00");
        assert_eq!(format_countdown(59_000), "0:59");
        assert_eq!(format_countdown(5_000), "0:05");
        assert_eq!(format_countdown(900), "0:00"); // sub-second floors to zero
    }

    #[test]
    fn format_countdown_clamps_negatives_to_zero() {
        assert_eq!(format_countdown(0), "0:00");
        assert_eq!(format_countdown(-1), "0:00");
        assert_eq!(format_countdown(-50_000), "0:00");
    }

    #[test]
    fn urgent_only_in_final_stretch() {
        assert!(is_urgent(URGENT_MS)); // exactly 10s → urgent
        assert!(is_urgent(URGENT_MS - 1));
        assert!(is_urgent(0)); // at the boundary → still urgent (red)
        assert!(!is_urgent(URGENT_MS + 1)); // 10.001s → not yet
        assert!(!is_urgent(-1)); // already closed → not urgent
    }

    /// Pins the rendered strings, INCLUDING the divergences that keep this formatter local
    /// instead of swapping to `vike_ui_theme::fmt::format_compact` (GUI audit F7 — a swap would
    /// change rendered text).
    #[test]
    fn format_volume_scales_thousands_and_millions() {
        assert_eq!(format_volume(0.0), "0");
        assert_eq!(format_volume(950.0), "950");
        assert_eq!(format_volume(1_500.0), "1.5k"); // lowercase k — format_compact prints "1.5K"
        assert_eq!(format_volume(12_300.0), "12.3k");
        assert_eq!(format_volume(2_300_000.0), "2.3M");
        assert_eq!(format_volume(2_000_000.0), "2.0M"); // one decimal — format_compact trims to "2M"
        assert_eq!(format_volume(7_100_000_000.0), "7100.0M"); // no B — format_compact prints "7.1B"
    }
}
