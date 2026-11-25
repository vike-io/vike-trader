//! The stuck-order watchdog's two shipped durations and the last resort they make: how long an order
//! may wait for its venue's acknowledgement ([`DEFAULT_SUBMIT_ACK_TIMEOUT`]), the confirm grace after
//! it ([`DEFAULT_SUBMIT_ACK_CONFIRM_GRACE`]), and when the core's synthesized reject can fire at the
//! latest ([`submit_ack_last_resort`]).
//!
//! Here, in the risk configuration's home (decision 0114's precedent), because the sides that size a
//! budget against them cannot name each other: the core reads them as its config's defaults, a bridge
//! sums its own queue and hold waits against the last resort, and the settings loader checks its rows
//! against them. A bridge's `const` assert written against THIS value turns red when the default
//! moves; one written against its own copy stays green while the budget it guards breaks.
//!
//! Pure `const` data and one `const fn`: nothing here reads a clock or runs per message.

use std::time::Duration;

/// How long an order may wait for its venue's acknowledgement before the stuck-order watchdog's
/// ladder starts (its first stage soft-warns and asks the venue): the live daemon's default.
pub const DEFAULT_SUBMIT_ACK_TIMEOUT: Duration = Duration::from_secs(30);

/// The confirm grace after [`DEFAULT_SUBMIT_ACK_TIMEOUT`] before the watchdog's synthesized reject,
/// which a still-pending confirm defers by one more grace. 15 s clears a two-request venue confirm
/// (about 10 s) with the deferral's slack.
pub const DEFAULT_SUBMIT_ACK_CONFIRM_GRACE: Duration = Duration::from_secs(15);

/// **The latest an order with no answer is rejected by the core**: `timeout + 2 · confirm_grace`, the
/// reject deadline once a confirm in flight has deferred it by one grace. A venue answer later than
/// this finds the order already rejected. 60 s at the shipped defaults. Saturates rather than
/// overflows.
#[must_use]
pub const fn submit_ack_last_resort(timeout: Duration, confirm_grace: Duration) -> Duration {
    timeout.saturating_add(confirm_grace.saturating_mul(2))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped figures every budget is summed against: 30 s, 15 s, and the 60 s they make.
    #[test]
    fn the_shipped_defaults_make_a_sixty_second_last_resort() {
        assert_eq!(DEFAULT_SUBMIT_ACK_TIMEOUT, Duration::from_secs(30));
        assert_eq!(DEFAULT_SUBMIT_ACK_CONFIRM_GRACE, Duration::from_secs(15));
        const LAST: Duration =
            submit_ack_last_resort(DEFAULT_SUBMIT_ACK_TIMEOUT, DEFAULT_SUBMIT_ACK_CONFIRM_GRACE);
        assert_eq!(LAST, Duration::from_secs(60));
        assert_eq!(submit_ack_last_resort(Duration::MAX, Duration::MAX), Duration::MAX);
    }
}
