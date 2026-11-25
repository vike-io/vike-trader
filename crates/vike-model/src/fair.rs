//! The up/down window: its length ([`UPDOWN_WINDOW_SECS`]) and its law ([`updown_window_open_ms`]),
//! the venue facts the offline backtest, the live maker and the cockpit share. The probability that
//! such a window closes up is a formula, so it is `vike_math::distributions::up_probability`
//! (decision 0294), no longer this module's `p_up`.

/// The up/down window length in seconds — and the modulus a Polymarket slug's `sts` must be a
/// multiple of.
///
/// Hoisted here for the same reason the window's up-probability once was, and out of the same
/// crate: it was `vike_backtest::cheap_np`'s window constant (that module has since moved to vike-strategy too,
/// but it was the simulator's when this was hoisted). By the time anyone counted, the identical `300`
/// was spelled THREE times — once there, once in the cockpit tool view under a `POLY_`-prefixed
/// name, and a third time as a file-local `const` inside the `cheap_np_askgate` measurement bin.
/// Two crates, three spellings of one venue fact, none able to see the others. All three now read
/// this line.
pub const UPDOWN_WINDOW_SECS: i64 = 300;

/// The up/down window LAW: the open (epoch ms) of the `window_ms`-long window that holds `ts_ms`,
/// Polymarket's `open = now - now % window`. The floor is [`i64::div_euclid`], so an instant before
/// 1970 floors DOWN like every other instead of toward zero; a window closes `window_ms` later, at
/// the next one's open. `None` for a window that is not a positive length: a misconfigured window
/// selects nothing rather than dividing by zero, the polymarket catalog's rule, kept for every
/// caller (the cockpit's chain rail and its follow, the catalog's discovery).
#[must_use]
pub fn updown_window_open_ms(ts_ms: i64, window_ms: i64) -> Option<i64> {
    (window_ms > 0).then(|| ts_ms.div_euclid(window_ms) * window_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window law floors to the window, toward −∞ before 1970, and the boundary instant opens
    /// the new window; a window that is not a positive length has no open (the edge the cockpit
    /// once clamped to 1 s and the catalog answered with nothing).
    #[test]
    fn a_window_opens_on_its_floor_and_a_non_positive_window_has_none() {
        let w = UPDOWN_WINDOW_SECS * 1000;
        assert_eq!(updown_window_open_ms(w + 42_000, w), Some(w));
        assert_eq!(updown_window_open_ms(w, w), Some(w), "the boundary opens the new window");
        assert_eq!(updown_window_open_ms(w - 1, w), Some(0));
        assert_eq!(updown_window_open_ms(-1, w), Some(-w), "floors down before the epoch");
        assert_eq!(updown_window_open_ms(1_500, 1), Some(1_500), "the shortest window");
        for not_positive in [0, -1, -w] {
            assert_eq!(updown_window_open_ms(1_500, not_positive), None, "{not_positive}");
        }
    }
}
