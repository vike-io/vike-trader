//! The lease's two heartbeat deadlines, as pure arithmetic.
//!
//! BOTH ends beat (`proto::GW_TRADE_HEARTBEAT`): the client pings after one period of its own
//! silence, and calls the link dead after `max(floor, 3 x the period the server stated)` of the
//! server's. The period is read from `TradeOpened.heartbeat_ms` (clamped into
//! `GW_TRADE_HEARTBEAT_MIN..=GW_TRADE_HEARTBEAT_MAX` by `crate::client::stated_trade_heartbeat`), so it needs
//! no compile-time agreement between independently deployed binaries. The floor is
//! `TradeTimings::read_floor`: raise-only in production, narrowed by a test.

use std::time::{Duration, Instant};

/// The read deadline: `max(floor, 3 x heartbeat)`. A frame of any kind inside it proves the link.
pub fn read_deadline(floor: Duration, heartbeat: Duration) -> Duration {
    floor.max(heartbeat.saturating_mul(3))
}

/// How long until the client owes the server a ping: one `heartbeat` after its last write, never
/// negative.
pub fn ping_due_in(last_write: Instant, heartbeat: Duration, now: Instant) -> Duration {
    (last_write + heartbeat).saturating_duration_since(now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deadline_is_the_floor_or_three_stated_periods() {
        let floor = Duration::from_secs(6);
        assert_eq!(read_deadline(floor, Duration::from_secs(2)), Duration::from_secs(6));
        assert_eq!(
            read_deadline(floor, Duration::from_millis(500)),
            floor,
            "a short period never lowers it"
        );
        assert_eq!(read_deadline(floor, Duration::from_secs(10)), Duration::from_secs(30));
        assert_eq!(read_deadline(floor, Duration::MAX), Duration::MAX, "no overflow");
    }

    #[test]
    fn a_ping_is_due_one_period_after_the_last_write_and_never_negative() {
        let t0 = Instant::now();
        let hb = Duration::from_secs(2);
        assert_eq!(ping_due_in(t0, hb, t0), hb);
        assert_eq!(ping_due_in(t0, hb, t0 + Duration::from_secs(1)), Duration::from_secs(1));
        assert_eq!(ping_due_in(t0, hb, t0 + Duration::from_secs(9)), Duration::ZERO);
    }
}
