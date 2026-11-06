//! The per-venue token bucket the server-side fetch lanes ration a venue's API budget with: the
//! chart-gap seed lane (`crate::seed`) and the venue-catalog lane (`crate::catalog`). Each lane
//! keeps one [`Bucket`] per venue and hands in its own burst and refill constants; the refill
//! arithmetic lives here once, so a fix to it lands in both lanes.
//!
//! A plain value with the clock INJECTED (`now`), so the arithmetic is testable without sleeping.

use std::time::{Duration, Instant};

/// One venue's bucket: up to `burst` tokens, one more accrued per whole `refill` period.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Bucket {
    tokens: u32,
    last_refill: Instant,
    burst: u32,
    refill: Duration,
}

impl Bucket {
    /// A FULL bucket (`burst` tokens) whose refill clock starts at `now`. `refill` must be non-zero:
    /// [`take`](Self::take) divides by it, and each lane's constant carries a compile-time floor.
    pub(crate) fn fresh(now: Instant, burst: u32, refill: Duration) -> Self {
        Self { tokens: burst, last_refill: now, burst, refill }
    }

    /// Fold elapsed time into tokens, then spend one if there is one. `false` = refused.
    ///
    /// Integer division on purpose, with `last_refill` advanced by the WHOLE periods consumed
    /// rather than to `now`: advancing to `now` would discard the remainder every call, so a caller
    /// polling faster than the refill period would never accrue a token at all — the classic bucket
    /// bug, and the reason this is a method with a test rather than two lines at the call site.
    ///
    /// The period count is clamped to `u32::MAX` straight from the `u128` quotient. (The seed
    /// lane's former copy narrowed it to `u64` first, which differed only past 2^64 elapsed periods
    /// — about 2.9 trillion years at the seed lane's 5 s — unreachable by any `Instant`.)
    pub(crate) fn take(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last_refill);
        let periods = elapsed.as_millis() / self.refill.as_millis();
        if periods > 0 {
            let periods = u32::try_from(periods).unwrap_or(u32::MAX);
            self.tokens = self.tokens.saturating_add(periods).min(self.burst);
            self.last_refill += self.refill * periods;
        }
        if self.tokens == 0 {
            return false;
        }
        self.tokens -= 1;
        true
    }
}

#[cfg(test)]
#[path = "token_bucket_tests.rs"]
mod tests;
