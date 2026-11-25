//! **The one token bucket** a rate-limited lane rations a budget with: up to `burst` calls back to
//! back, then one more per `refill` period. A plain value with the clock INJECTED (`now`), so the
//! arithmetic is testable without sleeping and this module reads no clock.
//!
//! **The refill choice: WHOLE periods, not continuous.** A token is added for each whole `refill`
//! period elapsed, by integer division, and the refill clock advances by exactly the periods
//! consumed, so the remainder carries into the next call. The continuous alternative (a fractional
//! `f64` balance topped up by `elapsed / refill` and moved to `now` on every call) was rejected: its
//! float sum can fall a hair short of a whole token on the boundary it should land on, and it drops
//! the partial period while the bucket sits full. With whole periods, a call refused at
//! 0.9 periods and repeated at 1.0 is admitted exactly at the boundary, on every platform.
//!
//! One consequence, stated rather than discovered: a FULL bucket keeps counting periods, so after a
//! long idle that ended mid-period, the first token after a drained burst arrives that partial
//! period early. Neither the burst nor the long-run rate (one token per `refill`) is exceeded.

use std::time::{Duration, Instant};

/// One budget's bucket: up to `burst` tokens, one more accrued per whole `refill` period.
#[derive(Debug, Clone, Copy)]
pub struct TokenBucket {
    tokens: u32,
    last_refill: Instant,
    burst: u32,
    refill: Duration,
}

impl TokenBucket {
    /// A FULL bucket (`burst` tokens) whose refill clock starts at `now`. A zero `refill` sets no
    /// rate at all: the bucket is full again at every call.
    pub fn fresh(now: Instant, burst: u32, refill: Duration) -> Self {
        Self { tokens: burst, last_refill: now, burst, refill }
    }

    /// Fold elapsed time into tokens, then spend one if there is one. `false` = refused, nothing
    /// spent.
    ///
    /// `last_refill` advances by the WHOLE periods consumed rather than to `now`: advancing to `now`
    /// would discard the remainder every call, so a caller polling faster than the refill period
    /// would never accrue a token at all, the classic bucket bug. A `now` before `last_refill` is
    /// zero elapsed (`saturating_duration_since`), never a panic or a refill. The period count is
    /// clamped to `u32::MAX`, past which only a burst of `u32::MAX` could tell.
    pub fn take(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last_refill);
        match elapsed.as_nanos().checked_div(self.refill.as_nanos()) {
            None => self.tokens = self.burst,
            Some(0) => {}
            Some(periods) => {
                let periods = u32::try_from(periods).unwrap_or(u32::MAX);
                self.tokens = self.tokens.saturating_add(periods).min(self.burst);
                self.last_refill += self.refill * periods;
            }
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
