//! **Strict, then degraded: how a reconcile pass treats a NAMING read that keeps failing** (decision 0192). A pass that
//! names a fired stop's fills from the venue's stop history (binance's `allAlgoOrders`, okx's `orders-algo-history`)
//! fails its whole fill read while that history is unreadable, because a fill list handed over without the names would
//! let the restore planner read a stop that did its job as one the venue dropped. That is strict for the first
//! [`ALGO_NAMING_STRICT_FAILURES`] passes in a row; from the next one on the pass goes ahead with those fills unnamed and
//! says so once every [`ALGO_NAMING_NOTICE_EVERY`], so an endpoint that stays broken cannot blind the whole account
//! (orders, positions, balance) for good. A pass that named what it needed brings strictness back.
//!
//! The counter, the clock and the two numbers are this module's; which read it guards, the words and the log line stay
//! each venue's.

use std::time::{Duration, Instant};

/// How many passes in a row may fail the fill read over an unreadable stop history before the pass degrades.
pub const ALGO_NAMING_STRICT_FAILURES: u32 = 3;

/// How often a degraded pass repeats its one plain warning.
pub const ALGO_NAMING_NOTICE_EVERY: Duration = Duration::from_secs(3600);

/// How the naming reads have been going across passes (the reconcile client lives for the process): the failing passes
/// in a row since the last good read, and when the degraded warning was last due.
#[derive(Debug, Default)]
pub struct StrictThenDegrade {
    failures: u32,
    noticed: Option<Instant>,
}

impl StrictThenDegrade {
    /// A pass named what it needed (or had nothing to name): strictness comes back.
    pub fn succeeded(&mut self) {
        self.failures = 0;
    }

    /// A pass's naming read failed with `why`. The first [`ALGO_NAMING_STRICT_FAILURES`] failures in a row are `Err`
    /// (the fill read fails). After that the pass goes ahead unnamed: `Ok(true)` when its warning is due
    /// ([`Self::notice_due`]), `Ok(false)` inside the hour of the last one.
    ///
    /// # Errors
    /// `why`, while the pass is still strict.
    pub fn failed(&mut self, now: Instant, why: String) -> Result<bool, String> {
        self.failures = self.failures.saturating_add(1);
        if self.failures <= ALGO_NAMING_STRICT_FAILURES {
            return Err(why);
        }
        Ok(self.notice_due(now))
    }

    /// Is the degraded warning due at `now` (none within the last [`ALGO_NAMING_NOTICE_EVERY`])? Taking it marks it
    /// said, so a venue's other degraded case (a pass capped short of every name) shares the one clock.
    pub fn notice_due(&mut self, now: Instant) -> bool {
        let due = self.noticed.is_none_or(|at| now.duration_since(at) >= ALGO_NAMING_NOTICE_EVERY);
        if due {
            self.noticed = Some(now);
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Three strict failures, then degraded with one warning an hour, and a good read makes it strict again.**
    #[test]
    fn three_strict_failures_then_one_warning_an_hour_and_a_good_read_restores_strictness() {
        let mut naming = StrictThenDegrade::default();
        let t0 = Instant::now();
        let down = || "the stop history is unreadable".to_string();
        for pass in 1..=ALGO_NAMING_STRICT_FAILURES {
            assert_eq!(naming.failed(t0, down()), Err(down()), "pass {pass} is strict");
        }
        assert_eq!(naming.failed(t0, down()), Ok(true), "the first degraded pass says so");
        let inside = t0 + ALGO_NAMING_NOTICE_EVERY - Duration::from_secs(1);
        assert_eq!(naming.failed(inside, down()), Ok(false), "silent inside the hour");
        assert_eq!(
            naming.failed(t0 + ALGO_NAMING_NOTICE_EVERY, down()),
            Ok(true),
            "said again an hour on"
        );

        naming.succeeded();
        assert_eq!(
            naming.failed(t0 + ALGO_NAMING_NOTICE_EVERY * 2, down()),
            Err(down()),
            "a good read makes the next failure strict again"
        );
    }

    /// The capped case shares the warning's clock: a notice taken by it silences the degraded pass inside the hour.
    #[test]
    fn a_notice_taken_by_another_degraded_case_shares_the_clock() {
        let mut naming = StrictThenDegrade::default();
        let t0 = Instant::now();
        assert!(naming.notice_due(t0));
        assert!(!naming.notice_due(t0 + Duration::from_secs(60)));
        for _ in 0..ALGO_NAMING_STRICT_FAILURES {
            let _ = naming.failed(t0, String::new());
        }
        assert_eq!(naming.failed(t0 + Duration::from_secs(60), String::new()), Ok(false));
    }
}
