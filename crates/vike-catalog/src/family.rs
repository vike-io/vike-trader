//! The polymarket recorder-FAMILY grammar, as a pure check.
//!
//! A rotating polymarket family (`btc-updown-5m`) is the slug template minus its `-{unix}`
//! suffix, and the trailing `-<N>m` carries the window length. The check lives here, below every
//! bridge, so a process that links no bridge (the data daemon, which holds the recorder's
//! `family` rows) refuses a malformed family at startup, offline, with the SAME words the bridge's
//! `crates/bridges/polymarket/src/rolling_family.rs`'s `window_spec_for` returns when it builds the
//! window planner from the family. One text, one home: the bridge calls this and does not spell it.

/// The window length, in minutes, a rolling family names, or the refusal text.
///
/// `btc-updown-5m` → `Ok(5)`. The interval is read off the trailing `-<N>m` token because that is
/// how Gamma labels these series (`crates/bridges/polymarket/src/discovery.rs`'s `interval_label`:
/// `300000 → "5m"`, and an HOURLY series is spelled `60m`, not `1h` — so only the minute form is
/// accepted, matching the venue rather than inventing a friendlier spelling that would resolve to
/// nothing).
pub fn polymarket_family_minutes(family: &str) -> Result<i64, String> {
    trailing_interval_minutes(family).ok_or_else(|| {
        format!(
            "polymarket family `{family}` does not end in an interval like `-5m` — a rolling family \
             is named after its window slugs (`btc-updown-5m` ⇒ `btc-updown-5m-<unix>`), and the \
             interval is what says how long a window lasts"
        )
    })
}

/// The `<N>` of a trailing `-<N>m`, when `N` is a positive integer.
fn trailing_interval_minutes(family: &str) -> Option<i64> {
    let tail = family.rsplit('-').next()?;
    let n: i64 = tail.strip_suffix('m')?.parse().ok()?;
    (n > 0).then_some(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An hourly series is spelled `60m` by Gamma, not `1h`. Accepting `1h` here would build slugs
    /// that resolve to nothing and record silence.
    #[test]
    fn only_the_minute_spelling_is_accepted() {
        assert_eq!(trailing_interval_minutes("eth-updown-60m"), Some(60));
        assert_eq!(trailing_interval_minutes("eth-updown-1h"), None);
        assert_eq!(
            trailing_interval_minutes("btc-updown-0m"),
            None,
            "a zero window is not a window"
        );
        assert_eq!(trailing_interval_minutes("some-event-slug"), None);
        assert_eq!(polymarket_family_minutes("btc-updown-5m"), Ok(5));
        assert_eq!(polymarket_family_minutes("eth-updown-60m"), Ok(60));
        for refused in ["eth-updown-1h", "btc-updown-0m", "some-event-slug", ""] {
            assert!(polymarket_family_minutes(refused).is_err(), "{refused:?} must be refused");
        }
    }

    /// The refusal is an operator-facing sentence read at startup (the bridge returns the same words
    /// from `window_spec_for`): pin it byte for byte so a reword is a decision, not drift.
    #[test]
    fn the_refusal_text_is_pinned() {
        assert_eq!(
            polymarket_family_minutes("eth-updown-1h").unwrap_err(),
            "polymarket family `eth-updown-1h` does not end in an interval like `-5m` — a rolling \
             family is named after its window slugs (`btc-updown-5m` ⇒ `btc-updown-5m-<unix>`), \
             and the interval is what says how long a window lasts"
        );
    }
}
